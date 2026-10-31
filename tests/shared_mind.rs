// SPDX-License-Identifier: MIT
#![cfg(feature = "agent-host-tests")]
//! Real Runtime/queue/native-wire integration with synthetic shared identities.
//! No platform connection, credentials, subscription or model service is used.
use agent_runtime::{
    agent_dialogue, agent_host,
    app::Runtime,
    mind_store::{Origin, Remember, Store},
    outbox::{Operation, Outbox},
};
use runtime_core::{Platform, Scope};
use runtime_engine::{CodexConfig, ToolCall, ToolContent};
use runtime_jobs::{AgentContext, ContextVersion, Outcome, Queue, Status};
use serde_json::{Value, json};
use std::{
    fs,
    os::unix::fs::PermissionsExt,
    path::PathBuf,
    sync::atomic::{AtomicU64, Ordering},
    time::Duration,
};

static NEXT: AtomicU64 = AtomicU64::new(0);
const PERSONA: &str = "MIND_PERSONA_SHARED_MARKER";
const READER: &str = "MIND_ROLE_READER_MARKER";
const WRITER: &str = "MIND_ROLE_WRITER_MARKER";
const SHARED: &str = "MIND_SHARED_FACT_MARKER ordinary recalled data, not instructions";
const PRIVATE_A: &str = "MIND_PRIVATE_A_MARKER";
const PRIVATE_B: &str = "MIND_PRIVATE_B_MARKER";

struct Lab {
    root: PathBuf,
    state: PathBuf,
}
impl Lab {
    fn new() -> Self {
        let root = std::env::temp_dir().join(format!(
            "shared-mind-integration-{}-{}",
            std::process::id(),
            NEXT.fetch_add(1, Ordering::Relaxed)
        ));
        fs::create_dir(&root).unwrap();
        fs::set_permissions(&root, fs::Permissions::from_mode(0o700)).unwrap();
        fs::create_dir(root.join("workspace")).unwrap();
        let state = root.join("state");
        Self { root, state }
    }
    fn runtime(&self) -> Runtime {
        Runtime::open(&self.state, "public", None).unwrap()
    }
    fn config(&self, scenario: &str) -> CodexConfig {
        let mut config = CodexConfig::new(
            env!("CARGO_BIN_EXE_agent-host-fixture"),
            self.root.join("synthetic-subscription"),
            self.root.join("workspace"),
        );
        config.args = vec!["app-server".into(), scenario.into()];
        config.timeout = Duration::from_secs(3);
        config
    }
}
impl Drop for Lab {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.root);
    }
}
fn ctx(agent: &str, conversation: &str) -> AgentContext {
    AgentContext {
        persona: "shared".into(),
        agent: agent.into(),
        conversation: Some(conversation.into()),
    }
}
fn seed(lab: &Lab) -> Store {
    let store = Store::open(&lab.state).unwrap();
    store.set_persona("shared", PERSONA, 0).unwrap();
    store.set_role("reader", READER, 0).unwrap();
    store.set_role("writer", WRITER, 0).unwrap();
    store
}
fn remember(
    store: &Store,
    context: &AgentContext,
    key: &str,
    text: &str,
    visibility: &str,
    operator: bool,
) -> agent_runtime::mind_store::Memory {
    store
        .remember(
            context,
            Remember {
                key: key.into(),
                text: text.into(),
                tags: vec!["mind".into()],
                visibility: visibility.into(),
                expected_revision: 0,
                source: if operator {
                    Origin::operator()
                } else {
                    Origin::agent(&context.agent, None)
                },
            },
        )
        .unwrap()
}
fn memory(runtime: &mut Runtime, args: Value) -> (bool, Value) {
    let result = runtime.dispatch_engine_tool(&ToolCall {
        thread_id: "untrusted-client-name".into(),
        turn_id: "untrusted-client-turn".into(),
        call_id: format!("memory-call-{}", NEXT.fetch_add(1, Ordering::Relaxed)),
        tool: "runtime_memory".into(),
        arguments: args,
    });
    let text = result
        .content_items
        .iter()
        .find_map(|item| match item {
            ToolContent::InputText { text } => Some(text),
            _ => None,
        })
        .expect("tool text missing");
    (
        result.success,
        serde_json::from_str(text).expect("memory tool result must be structured"),
    )
}

#[test]
fn completed_pre_reset_job_does_not_restore_cleared_dialogue_or_change_execution_evidence() {
    let lab = Lab::new();
    let _runtime = lab.runtime();
    seed(&lab);
    let context = ctx("reader", "chat-a");
    let queue = Queue::open(&lab.state).unwrap();
    queue
        .enqueue_with_context("pre-reset", "old task", None, context.clone(), 10)
        .unwrap();
    let claim = queue.claim_next(11).unwrap().unwrap();
    let prepared = agent_runtime::agent_mind::assemble(
        &lab.state,
        &context,
        "old task",
        None,
        Some("pre-reset"),
    )
    .unwrap();
    queue
        .record_context(&claim.lease, prepared.version, 12)
        .unwrap();
    queue
        .update_progress(
            &claim.lease,
            runtime_jobs::Progress {
                thread_id: Some("active-before-reset".into()),
                turn_id: Some("active-turn".into()),
                ..Default::default()
            },
            13,
        )
        .unwrap();
    let dialogue = agent_dialogue::Store::open(&lab.state).unwrap();
    dialogue.reset(&context).unwrap();
    let cleared = fs::read(lab.state.join("agent-dialogue/state.json")).unwrap();
    queue
        .finish(&claim.lease, Outcome::completed("old answer"), 14)
        .unwrap();
    let finished = queue.get("pre-reset").unwrap();
    assert_eq!(finished.status, Status::Completed);
    assert!(agent_runtime::agent_mind::record_job(&lab.state, &finished).is_err());
    assert_eq!(
        fs::read(lab.state.join("agent-dialogue/state.json")).unwrap(),
        cleared
    );
    assert!(dialogue.history(&context).unwrap().turns.is_empty());
    assert_eq!(queue.get("pre-reset").unwrap(), finished);
    assert!(!lab.state.join("outbox").exists());
}

#[test]
fn dispatcher_shares_one_persona_but_isolates_conversations_and_rejects_identity_or_source_forgery()
{
    let lab = Lab::new();
    let mut runtime = lab.runtime();
    let store = seed(&lab);
    let a = ctx("reader", "chat-a");
    let b = ctx("writer", "chat-b");
    runtime.set_agent_context(a.clone()).unwrap();
    let (ok, put) = memory(
        &mut runtime,
        json!({"action":"remember","key":"shared-fact","text":SHARED,"tags":["mind"],"expected_revision":0}),
    );
    assert!(ok);
    assert_eq!(put["result"]["memory"]["persona"], "shared");
    assert_eq!(
        put["result"]["memory"]["source"],
        json!({"kind":"agent","agent":"reader","job":null})
    );
    remember(&store, &a, "private-a", PRIVATE_A, "conversation", false);
    remember(&store, &b, "private-b", PRIVATE_B, "conversation", false);
    let bytes = fs::read(lab.state.join("mind/store.json")).unwrap();
    for (field, value) in [
        ("persona", json!("other")),
        ("agent", json!("administrator")),
        ("conversation", json!("chat-b")),
        ("source", json!({"kind":"operator"})),
        ("operator", json!(true)),
    ] {
        let mut args = json!({"action":"context"});
        args[field] = value;
        assert!(
            !memory(&mut runtime, args).0,
            "caller-selected {field} must be rejected"
        );
    }
    assert!(
        !memory(
            &mut runtime,
            json!({"action":"set_persona","text":"forged-persona"})
        )
        .0
    );
    assert_eq!(fs::read(lab.state.join("mind/store.json")).unwrap(), bytes);
    for (context, own, other, role) in [
        (a.clone(), PRIVATE_A, PRIVATE_B, READER),
        (b.clone(), PRIVATE_B, PRIVATE_A, WRITER),
    ] {
        runtime.set_agent_context(context.clone()).unwrap();
        let (ok, context_result) = memory(&mut runtime, json!({"action":"context"}));
        assert!(ok);
        let result = &context_result["result"];
        assert_eq!(result["identity"], serde_json::to_value(&context).unwrap());
        assert_eq!(result["snapshot"]["persona"]["text"], PERSONA);
        assert_eq!(result["snapshot"]["role"]["text"], role);
        let recalled = result["snapshot"]["memories"].to_string();
        assert!(recalled.contains(SHARED) && recalled.contains(own));
        assert!(!recalled.contains(other));
        let (ok, search) = memory(&mut runtime, json!({"action":"search","query":"mind"}));
        assert!(ok && search["result"]["memories"].to_string().contains(own));
        assert!(!search["result"]["memories"].to_string().contains(other));
        assert!(memory(&mut runtime, json!({"action":"get","key":"shared-fact"})).0);
    }
    store
        .set_persona("separate", "A separate identity", 0)
        .unwrap();
    runtime
        .set_agent_context(AgentContext {
            persona: "separate".into(),
            ..a
        })
        .unwrap();
    let (ok, separate) = memory(&mut runtime, json!({"action":"context"}));
    assert!(
        ok && separate["result"]["snapshot"]["memories"]
            .as_array()
            .unwrap()
            .is_empty()
    );
    assert!(!memory(&mut runtime, json!({"action":"get","key":"shared-fact"})).0);
}

#[test]
fn dispatcher_cannot_modify_operator_facts_and_stale_revisions_never_change_active_memory() {
    let lab = Lab::new();
    let mut runtime = lab.runtime();
    let store = seed(&lab);
    let a = ctx("reader", "chat-a");
    let b = ctx("writer", "chat-b");
    let protected = remember(
        &store,
        &a,
        "operator-fact",
        "MIND_OPERATOR_FACT_MARKER",
        "shared",
        true,
    );
    let ordinary = remember(&store, &a, "ordinary", SHARED, "shared", false);
    runtime.set_agent_context(b.clone()).unwrap();
    let before = fs::read(lab.state.join("mind/store.json")).unwrap();
    for action in ["remember", "forget"] {
        assert!(!memory(&mut runtime,json!({"action":action,"key":"operator-fact","text":"forged replacement","expected_revision":protected.revision})).0);
    }
    assert_eq!(store.get(&a, "operator-fact", "shared").unwrap(), protected);
    assert!(
        !memory(
            &mut runtime,
            json!({"action":"remember","key":"ordinary","text":"stale edit","expected_revision":0})
        )
        .0
    );
    assert_eq!(fs::read(lab.state.join("mind/store.json")).unwrap(), before);
    let (ok, updated) = memory(
        &mut runtime,
        json!({"action":"remember","key":"ordinary","text":"MIND_UPDATED_SHARED_FACT_MARKER","tags":["mind"],"expected_revision":ordinary.revision}),
    );
    assert!(ok);
    let current = store.get(&a, "ordinary", "shared").unwrap();
    assert!(current.revision > ordinary.revision);
    assert_eq!(current.source, Origin::agent("writer", None));
    assert_eq!(updated["result"]["memory"]["revision"], current.revision);
    assert!(
        !memory(
            &mut runtime,
            json!({"action":"forget","key":"ordinary","expected_revision":ordinary.revision})
        )
        .0
    );
    assert_eq!(store.get(&a, "ordinary", "shared").unwrap(), current);
    let (ok, forgotten) = memory(
        &mut runtime,
        json!({"action":"forget","key":"ordinary","expected_revision":current.revision}),
    );
    assert!(ok && forgotten["result"]["removed"] == true);
    assert_eq!(forgotten["result"]["old_model_transcripts_erased"], false);
    runtime.set_agent_context(a).unwrap();
    assert!(!memory(&mut runtime, json!({"action":"get","key":"ordinary"})).0);
    assert_eq!(store.get(&b, "operator-fact", "shared").unwrap(), protected);
}

#[test]
fn host_native_wire_shares_persona_keeps_roles_separate_and_carries_only_same_conversation_history()
{
    let lab = Lab::new();
    let mut runtime = lab.runtime();
    let store = seed(&lab);
    let a = ctx("reader", "chat-a");
    let b = ctx("writer", "chat-b");
    remember(&store, &a, "shared-fact", SHARED, "shared", false);
    remember(&store, &a, "private-a", PRIVATE_A, "conversation", false);
    remember(&store, &b, "private-b", PRIVATE_B, "conversation", false);
    let queue = Queue::open(&lab.state).unwrap();
    let previous = AgentContext {
        persona: "main".into(),
        agent: "outside".into(),
        conversation: Some("management".into()),
    };
    runtime.set_agent_context(previous.clone()).unwrap();
    let time = agent_host::now().unwrap();
    queue
        .enqueue_with_context(
            "mind-reader-a",
            "MIND_FIRST_TASK_MARKER",
            None,
            a.clone(),
            time,
        )
        .unwrap();
    let first = agent_host::run_once(
        &mut runtime,
        &queue,
        lab.config("agent_job_mind_reader"),
        None,
    )
    .unwrap();
    assert_eq!(first.status, "completed");
    assert_eq!(first.tool_calls, 1);
    assert_eq!(first.history_persisted, Some(true));
    assert_eq!(runtime.agent_context(), &previous);
    let first_job = queue.get("mind-reader-a").unwrap();
    let first_version = first_job
        .context_version
        .clone()
        .expect("context version must precede the native thread");
    assert_eq!(first.context_version, Some(first_version.clone()));
    assert_eq!(
        first_version.persona_revision,
        store.persona("shared").unwrap().revision
    );
    assert_eq!(
        first_version.role_revision,
        store.role("reader").unwrap().revision
    );
    assert_eq!(
        first_version.memory_revision,
        store.status().unwrap()["store_revision"].as_u64().unwrap()
    );
    assert_eq!(first_job.text.as_deref(), Some("MIND_FIRST_ANSWER_MARKER"));
    let (ok, restored) = memory(
        &mut runtime,
        json!({"action":"remember","key":"after-host","text":"identity restoration fixture","expected_revision":0}),
    );
    assert!(ok);
    assert_eq!(restored["result"]["memory"]["persona"], "main");
    assert_eq!(
        restored["result"]["memory"]["source"],
        json!({"kind":"agent","agent":"outside","job":null})
    );
    let old_persona = store.persona("shared").unwrap();
    store
        .set_persona(
            "shared",
            &format!("{PERSONA} UPDATED"),
            old_persona.revision,
        )
        .unwrap();
    queue
        .enqueue_with_context(
            "mind-writer-b",
            "MIND_WRITER_TASK_MARKER",
            None,
            b.clone(),
            agent_host::now().unwrap(),
        )
        .unwrap();
    let second = agent_host::run_once(
        &mut runtime,
        &queue,
        lab.config("agent_job_mind_writer"),
        None,
    )
    .unwrap();
    assert_eq!(second.status, "completed");
    assert_eq!(second.tool_calls, 2);
    assert_eq!(runtime.agent_context(), &previous);
    assert!(
        second.context_version.as_ref().unwrap().persona_revision > first_version.persona_revision
    );
    let written = store.get(&a, "from-writer-job", "shared").unwrap();
    assert_eq!(
        written.source,
        Origin::agent("writer", Some("mind-writer-b"))
    );
    assert!(
        second.context_version.as_ref().unwrap().memory_revision < written.revision,
        "a job's first context version remains fixed while it writes ordinary memories"
    );
    let continuation = ctx("writer", "chat-a");
    queue
        .enqueue_with_context(
            "mind-continuation-a",
            "MIND_CONTINUATION_TASK_MARKER",
            None,
            continuation.clone(),
            agent_host::now().unwrap(),
        )
        .unwrap();
    let third = agent_host::run_once(
        &mut runtime,
        &queue,
        lab.config("agent_job_mind_continuation"),
        None,
    )
    .unwrap();
    assert_eq!(third.status, "completed");
    assert_eq!(third.tool_calls, 1);
    assert_eq!(third.history_persisted, Some(true));
    assert_eq!(runtime.agent_context(), &previous);
    assert_eq!(
        queue.get("mind-reader-a").unwrap(),
        first_job,
        "future profile updates do not rewrite a job's recorded snapshot version"
    );
    let history_a = agent_dialogue::Store::open(&lab.state)
        .unwrap()
        .history(&continuation)
        .unwrap();
    let history_b = agent_dialogue::Store::open(&lab.state)
        .unwrap()
        .history(&b)
        .unwrap();
    assert_eq!(
        history_a
            .turns
            .iter()
            .map(|t| t.job_id.as_str())
            .collect::<Vec<_>>(),
        vec!["mind-reader-a", "mind-continuation-a"]
    );
    assert_eq!(
        history_b
            .turns
            .iter()
            .map(|t| t.job_id.as_str())
            .collect::<Vec<_>>(),
        vec!["mind-writer-b"]
    );
    assert!(!lab.state.join("outbox").exists());
}

#[test]
fn queue_identity_and_first_context_version_are_immutable_and_legacy_jobs_still_read() {
    let lab = Lab::new();
    let queue = Queue::open(&lab.state).unwrap();
    let identity = AgentContext::default();
    let original = queue
        .enqueue_with_context(
            "fixed-context",
            "synthetic task",
            None,
            identity.clone(),
            100,
        )
        .unwrap();
    assert_eq!(
        queue
            .enqueue_with_context(
                "fixed-context",
                "synthetic task",
                None,
                identity.clone(),
                101
            )
            .unwrap(),
        original
    );
    for changed in [
        AgentContext {
            agent: "different".into(),
            ..identity.clone()
        },
        AgentContext {
            conversation: Some("another-session".into()),
            ..identity.clone()
        },
    ] {
        assert!(
            queue
                .enqueue_with_context("fixed-context", "synthetic task", None, changed, 101)
                .is_err()
        );
        assert_eq!(queue.get("fixed-context").unwrap(), original);
    }
    let claimed = queue.claim_next(101).unwrap().unwrap();
    let version = ContextVersion {
        persona_revision: 1,
        role_revision: 2,
        memory_revision: 3,
        history_epoch: 4,
    };
    let stamped = queue
        .record_context(&claimed.lease, version.clone(), 102)
        .unwrap();
    assert_eq!(
        queue
            .record_context(&claimed.lease, version.clone(), 102)
            .unwrap(),
        stamped
    );
    assert!(
        queue
            .record_context(
                &claimed.lease,
                ContextVersion {
                    memory_revision: 99,
                    ..version.clone()
                },
                102
            )
            .is_err()
    );
    assert_eq!(
        queue.get("fixed-context").unwrap().context_version,
        Some(version)
    );
    queue
        .finish(&claimed.lease, Outcome::completed("synthetic answer"), 103)
        .unwrap();
    let path = lab.state.join("jobs/fixed-context.json");
    let mut legacy: Value = serde_json::from_slice(&fs::read(&path).unwrap()).unwrap();
    legacy["job"].as_object_mut().unwrap().remove("context");
    legacy["job"]
        .as_object_mut()
        .unwrap()
        .remove("context_version");
    fs::write(&path, serde_json::to_vec(&legacy).unwrap()).unwrap();
    let old = Queue::open(&lab.state)
        .unwrap()
        .get("fixed-context")
        .unwrap();
    assert_eq!(old.context, AgentContext::default());
    assert_eq!(old.context_version, None);
    assert_eq!(old.status, Status::Completed);
}

#[test]
fn damaged_persona_fails_before_engine_start_and_restores_the_runtime_identity() {
    let lab = Lab::new();
    let mut runtime = lab.runtime();
    seed(&lab);
    let path = lab.state.join("mind/store.json");
    let damaged = b"{broken-profile-MUST-NOT-BECOME-INSTRUCTIONS";
    fs::write(&path, damaged).unwrap();
    let queue = Queue::open(&lab.state).unwrap();
    queue
        .enqueue_with_context(
            "bad-profile",
            "synthetic task",
            None,
            ctx("reader", "chat-a"),
            agent_host::now().unwrap(),
        )
        .unwrap();
    let previous = AgentContext {
        agent: "outside".into(),
        ..AgentContext::default()
    };
    runtime.set_agent_context(previous.clone()).unwrap();
    let mut config = lab.config("agent_job_mind_reader");
    config.binary = lab.root.join("engine-must-not-be-spawned");
    let report = agent_host::run_once(&mut runtime, &queue, config, None).unwrap();
    assert_eq!(report.status, "failed");
    assert_eq!(report.tool_calls, 0);
    assert_eq!(report.thread_id, None);
    assert_eq!(report.turn_id, None);
    assert_eq!(report.context_version, None);
    assert_eq!(runtime.agent_context(), &previous);
    let saved = queue.get("bad-profile").unwrap();
    assert_eq!(saved.status, Status::Failed);
    assert!(
        saved
            .error
            .as_deref()
            .unwrap()
            .contains("Shared identity/context")
    );
    assert!(!saved.error.as_deref().unwrap().contains("broken-profile"));
    assert_eq!(fs::read(&path).unwrap(), damaged);
    assert!(!lab.state.join("agent-dialogue").exists());
    assert!(!lab.state.join("outbox").exists());
}

#[test]
fn independent_runtime_kernels_share_only_the_explicit_mind_root() {
    let lab = Lab::new();
    let other_state = lab.root.join("second-runtime");
    let shared_root = lab.root.join("shared-identity");
    let mut first = lab.runtime();
    let mut second = Runtime::open(&other_state, "public", None).unwrap();
    first.set_mind_state(&shared_root).unwrap();
    second.set_mind_state(&shared_root).unwrap();
    let shared = Store::open(&shared_root).unwrap();
    shared.set_persona("shared", PERSONA, 0).unwrap();
    shared.set_role("reader", READER, 0).unwrap();
    shared.set_role("writer", WRITER, 0).unwrap();
    let a = ctx("reader", "local-a");
    let b = ctx("writer", "local-b");
    first.set_agent_context(a.clone()).unwrap();
    second.set_agent_context(b.clone()).unwrap();
    let mut local_files = Vec::new();
    for (state, suffix) in [(&lab.state, "a"), (&other_state, "b")] {
        let outbox = Outbox::open(state).unwrap();
        let operation = Operation::new(
            &format!("local-operation-{suffix}"),
            Scope {
                platform: Platform::Qq,
                account: format!("synthetic-account-{suffix}"),
            },
            format!("synthetic-actor-{suffix}"),
            json!({"fixture":suffix}),
            &format!("PRIVATE_LOCAL_OUTBOX_{suffix}"),
        )
        .unwrap();
        outbox.insert(&operation).unwrap();
        let accounts = state.join("accounts");
        fs::create_dir_all(&accounts).unwrap();
        fs::set_permissions(&accounts, fs::Permissions::from_mode(0o700)).unwrap();
        let marker = accounts.join("synthetic-owner.marker");
        fs::write(&marker, format!("synthetic local account {suffix}")).unwrap();
        fs::set_permissions(&marker, fs::Permissions::from_mode(0o600)).unwrap();
        for path in [
            marker,
            state.join(format!("outbox/local-operation-{suffix}.json")),
        ] {
            local_files.push((path.clone(), fs::read(path).unwrap()));
        }
    }
    agent_dialogue::Store::open(&lab.state)
        .unwrap()
        .append(
            &a,
            agent_dialogue::Turn {
                job_id: "local-history-a".into(),
                execution_id: None,
                at: 100,
                status: "completed".into(),
                user: "PRIVATE_LOCAL_DIALOGUE_A".into(),
                assistant: Some("Only Runtime A should carry this history".into()),
                operation_ids: vec![],
                operation_count: 0,
            },
        )
        .unwrap();
    let (ok, written) = memory(
        &mut first,
        json!({"action":"remember","key":"across-processes","text":SHARED,"expected_revision":0}),
    );
    assert!(ok);
    assert_eq!(written["result"]["memory"]["source"]["agent"], "reader");
    let (ok, read) = memory(
        &mut second,
        json!({"action":"get","key":"across-processes"}),
    );
    assert!(ok);
    assert_eq!(read["result"]["memory"], written["result"]["memory"]);
    for (runtime, role, history_count) in [(&mut first, READER, 1), (&mut second, WRITER, 0)] {
        let (ok, context) = memory(runtime, json!({"action":"context"}));
        assert!(ok);
        assert_eq!(context["result"]["snapshot"]["persona"]["text"], PERSONA);
        assert_eq!(context["result"]["snapshot"]["role"]["text"], role);
        assert!(
            context["result"]["snapshot"]["memories"]
                .to_string()
                .contains(SHARED)
        );
        assert_eq!(
            context["result"]["recent_dialogue"]["turns"]
                .as_array()
                .unwrap()
                .len(),
            history_count
        );
        assert!(!context.to_string().contains("PRIVATE_LOCAL_OUTBOX"));
    }
    assert!(
        agent_dialogue::Store::open(&other_state)
            .unwrap()
            .history(&a)
            .unwrap()
            .turns
            .is_empty(),
        "even an identical persona/conversation lookup uses the second kernel's own local history"
    );
    assert!(!shared_root.join("agent-dialogue").exists());
    assert!(!shared_root.join("accounts").exists());
    assert!(!shared_root.join("outbox").exists());
    assert!(!lab.state.join("mind").exists());
    assert!(!other_state.join("mind").exists());
    for (path, original) in local_files {
        assert_eq!(fs::read(path).unwrap(), original);
    }
}
