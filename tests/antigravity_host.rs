// SPDX-License-Identifier: MIT
#![cfg(feature = "agent-host-tests")]
//! Cross-engine jobs and real Rust MCP forwarding with synthetic state only.
use agent_runtime::{
    agent_antigravity, agent_dialogue, agent_host,
    app::Runtime,
    mind_store::{Origin, Remember, Store},
};
use runtime_engine::{AntigravityConfig, CodexConfig};
use runtime_jobs::{AgentContext, Queue, Status};
use std::{
    fs,
    os::unix::fs::PermissionsExt,
    path::PathBuf,
    sync::atomic::{AtomicBool, AtomicU64, Ordering},
    time::Duration,
};

static NEXT: AtomicU64 = AtomicU64::new(0);
struct Lab {
    root: PathBuf,
    state: PathBuf,
}
impl Lab {
    fn new() -> Self {
        let root = std::env::temp_dir().join(format!(
            "ar-acp-{}-{}",
            std::process::id(),
            NEXT.fetch_add(1, Ordering::Relaxed)
        ));
        fs::create_dir(&root).unwrap();
        fs::set_permissions(&root, fs::Permissions::from_mode(0o700)).unwrap();
        fs::create_dir(root.join("workspace")).unwrap();
        Self {
            state: root.join("state"),
            root,
        }
    }
    fn runtime(&self) -> Runtime {
        Runtime::open(&self.state, "public", None).unwrap()
    }
    fn config(&self, scenario: &str) -> AntigravityConfig {
        let mut config = AntigravityConfig::new(
            env!("CARGO_BIN_EXE_agent-antigravity-fixture"),
            self.root.join("agy-home"),
            self.root.join("workspace"),
        );
        config.bridge_binary = env!("CARGO_BIN_EXE_agent-runtime").into();
        config.timeout = Duration::from_secs(5);
        config.args = vec![scenario.into()];
        config
    }
    fn codex(&self, scenario: &str) -> CodexConfig {
        let mut config = CodexConfig::new(
            env!("CARGO_BIN_EXE_agent-host-fixture"),
            self.root.join("codex-home"),
            self.root.join("workspace"),
        );
        config.timeout = Duration::from_secs(5);
        config.args = vec!["app-server".into(), scenario.into()];
        config
    }
    fn failure(&self) -> String {
        fs::read_to_string(self.root.join("workspace/fixture.failure")).unwrap_or_default()
    }
    fn seed(&self) -> Store {
        let store = Store::open(&self.state).unwrap();
        store
            .set_persona("shared", "MIND_PERSONA_SHARED_MARKER", 0)
            .unwrap();
        store
            .set_role("reader", "MIND_ROLE_READER_MARKER", 0)
            .unwrap();
        store
            .set_role("writer", "MIND_ROLE_WRITER_MARKER", 0)
            .unwrap();
        for (context, key, text, visibility) in [
            (
                ctx("reader", "chat-a"),
                "shared-fact",
                "MIND_SHARED_FACT_MARKER",
                "shared",
            ),
            (
                ctx("reader", "chat-a"),
                "private-a",
                "MIND_PRIVATE_A_MARKER",
                "conversation",
            ),
            (
                ctx("writer", "chat-b"),
                "private-b",
                "MIND_PRIVATE_B_MARKER",
                "conversation",
            ),
        ] {
            store
                .remember(
                    &context,
                    Remember {
                        key: key.into(),
                        text: text.into(),
                        tags: vec!["mind".into()],
                        visibility: visibility.into(),
                        expected_revision: 0,
                        source: Origin::operator(),
                    },
                )
                .unwrap();
        }
        store
    }
    fn enqueue(&self, id: &str, context: AgentContext, prompt: &str) -> Queue {
        let queue = Queue::open(&self.state).unwrap();
        queue
            .enqueue_with_context(id, prompt, None, context, agent_host::now().unwrap())
            .unwrap();
        queue
    }
    fn run(
        &self,
        runtime: &mut Runtime,
        queue: &Queue,
        scenario: &str,
        tools: &[&str],
    ) -> agent_host::RunReport {
        let tools = tools.iter().map(|s| s.to_string()).collect::<Vec<_>>();
        agent_antigravity::run_once_controlled(
            runtime,
            queue,
            self.config(scenario),
            &AtomicBool::new(false),
            Some(&tools),
        )
        .unwrap()
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

#[test]
fn codex_to_antigravity_shared_memory_and_antigravity_to_codex_conversation_continuity() {
    let lab = Lab::new();
    let mut runtime = lab.runtime();
    let store = lab.seed();
    let queue = lab.enqueue(
        "mind-writer-b",
        ctx("writer", "chat-b"),
        "MIND_WRITER_TASK_MARKER",
    );
    let first = agent_host::run_once(
        &mut runtime,
        &queue,
        lab.codex("agent_job_mind_writer"),
        None,
    )
    .unwrap();
    assert_eq!(first.status, "completed");
    let previous = AgentContext {
        agent: "operator".into(),
        ..Default::default()
    };
    runtime.set_agent_context(previous.clone()).unwrap();
    lab.enqueue(
        "mind-reader-a",
        ctx("reader", "chat-a"),
        "MIND_FIRST_TASK_MARKER",
    );
    let second = lab.run(&mut runtime, &queue, "read-codex", &["runtime_memory"]);
    assert_eq!(second.status, "completed", "{second:?} {}", lab.failure());
    assert_eq!(second.engine, "antigravity");
    assert_eq!(second.tool_calls, 2);
    assert_eq!(second.history_persisted, Some(true));
    assert_eq!(runtime.agent_context(), &previous);
    let saved = queue.get("mind-reader-a").unwrap();
    assert!(
        saved
            .progress
            .thread_id
            .as_deref()
            .unwrap()
            .starts_with("agy-fixture-")
    );
    assert!(
        saved
            .progress
            .turn_id
            .as_deref()
            .unwrap()
            .starts_with("acp-")
    );
    assert_eq!(saved.context_version, second.context_version);
    assert!(saved.context_version.as_ref().unwrap().persona_revision > 0);
    let memory = store
        .get(&ctx("writer", "chat-b"), "from-agy", "shared")
        .unwrap();
    assert_eq!(memory.text, "AGY_SHARED_WRITTEN");
    assert_eq!(
        memory.source,
        Origin::agent("reader", Some("mind-reader-a"))
    );
    // Codex's existing native-wire peer requires precisely this previous Agent's
    // retained task/answer and no chat-b transcript in its portable history.
    lab.enqueue(
        "mind-continuation-a",
        ctx("writer", "chat-a"),
        "MIND_CONTINUATION_TASK_MARKER",
    );
    let third = agent_host::run_once(
        &mut runtime,
        &queue,
        lab.codex("agent_job_mind_continuation"),
        None,
    )
    .unwrap();
    assert_eq!(third.status, "completed");
    assert_eq!(third.history_persisted, Some(true));
    assert_eq!(runtime.agent_context(), &previous);
    assert_eq!(
        agent_dialogue::Store::open(&lab.state)
            .unwrap()
            .history(&ctx("reader", "chat-a"))
            .unwrap()
            .turns
            .len(),
        2
    );
    assert_eq!(
        agent_dialogue::Store::open(&lab.state)
            .unwrap()
            .history(&ctx("reader", "chat-b"))
            .unwrap()
            .turns
            .len(),
        1
    );
    assert!(
        fs::read_dir(lab.state.join("engine-bridge"))
            .unwrap()
            .next()
            .is_none()
    );
}

#[test]
fn absent_subscription_never_calls_tools_and_can_resume_only_before_execution() {
    let lab = Lab::new();
    let mut runtime = lab.runtime();
    lab.seed();
    let queue = lab.enqueue(
        "auth-blocked",
        ctx("reader", "chat-a"),
        "synthetic auth test",
    );
    let before = fs::read(lab.state.join("mind/store.json")).unwrap();
    let report = lab.run(&mut runtime, &queue, "needs-auth", &["runtime_memory"]);
    assert_eq!(report.status, "needs_auth", "{}", lab.failure());
    assert_eq!(report.tool_calls, 0);
    assert!(report.thread_id.is_none() && report.context_version.is_none());
    assert_eq!(fs::read(lab.state.join("mind/store.json")).unwrap(), before);
    queue
        .resume_needs_auth(&["auth-blocked".into()], agent_host::now().unwrap())
        .unwrap();
    let resumed = lab.run(&mut runtime, &queue, "write", &["runtime_memory"]);
    assert_eq!(resumed.status, "completed", "{resumed:?} {}", lab.failure());
    assert_eq!(resumed.tool_calls, 1);
    assert!(
        queue
            .resume_needs_auth(&["auth-blocked".into()], agent_host::now().unwrap())
            .is_err()
    );
}

#[test]
fn native_quota_end_turn_preserves_tool_progress_without_automatic_replay() {
    for (scenario, expected) in [
        ("quota", "failed"),
        ("quota-after-tool", "failed"),
        ("quota-quote", "completed"),
    ] {
        let lab = Lab::new();
        let mut runtime = lab.runtime();
        lab.seed();
        let queue = lab.enqueue(
            "quota-check",
            ctx("reader", "chat-a"),
            "Synthetic native quota reply",
        );
        let report = lab.run(&mut runtime, &queue, scenario, &["runtime_memory"]);
        assert_eq!(report.status, expected, "{report:?} {}", lab.failure());
        assert_eq!(report.tool_calls, u32::from(scenario == "quota-after-tool"));
        let job = queue.get("quota-check").unwrap();
        if scenario != "quota-quote" {
            assert_eq!(job.status, Status::Failed);
            assert!(job.text.is_none());
            assert!(job.error.unwrap().contains("subscription quota exhausted"));
            if scenario == "quota-after-tool" {
                assert_eq!(job.progress.tool_calls, 1);
                assert_eq!(
                    Store::open(&lab.state)
                        .unwrap()
                        .get(&ctx("reader", "chat-a"), "before-quota", "shared")
                        .unwrap()
                        .text,
                    "synthetic completed tool"
                );
            }
        } else {
            assert!(job.text.unwrap().starts_with("Quoted provider message:"));
        }
        assert!(
            queue
                .claim_next(agent_host::now().unwrap())
                .unwrap()
                .is_none()
        );
    }
}

#[test]
fn business_call_before_native_session_binding_is_rejected_without_memory_write() {
    let lab = Lab::new();
    let mut runtime = lab.runtime();
    lab.seed();
    let queue = lab.enqueue(
        "premature",
        ctx("reader", "chat-a"),
        "synthetic premature call",
    );
    let before = fs::read(lab.state.join("mind/store.json")).unwrap();
    let report = lab.run(&mut runtime, &queue, "premature", &["runtime_memory"]);
    assert_eq!(report.status, "failed");
    assert_eq!(report.tool_calls, 0);
    assert!(report.thread_id.is_none());
    assert_eq!(fs::read(lab.state.join("mind/store.json")).unwrap(), before);
}

#[test]
fn filtered_mcp_catalog_never_dispatches_an_unselected_tool() {
    let lab = Lab::new();
    let mut runtime = lab.runtime();
    lab.seed();
    let queue = lab.enqueue(
        "filtered",
        ctx("reader", "chat-a"),
        "synthetic tool allowlist",
    );
    let before = fs::read(lab.state.join("mind/store.json")).unwrap();
    let report = lab.run(&mut runtime, &queue, "filtered", &["runtime_modules"]);
    assert_eq!(report.status, "completed", "{report:?} {}", lab.failure());
    assert_eq!(report.tool_calls, 0);
    assert_eq!(fs::read(lab.state.join("mind/store.json")).unwrap(), before);
}

#[test]
fn interrupted_reply_preserves_executed_memory_and_never_replays_the_job() {
    let lab = Lab::new();
    let mut runtime = lab.runtime();
    let store = lab.seed();
    let queue = lab.enqueue(
        "bad-reply",
        ctx("reader", "chat-a"),
        "synthetic interrupted reply",
    );
    let report = lab.run(&mut runtime, &queue, "wrong-session", &["runtime_memory"]);
    assert_eq!(report.status, "interrupted", "{report:?} {}", lab.failure());
    assert_eq!(report.tool_calls, 1);
    assert_eq!(queue.get("bad-reply").unwrap().status, Status::Interrupted);
    assert_eq!(
        store
            .get(&ctx("reader", "chat-a"), "from-agy", "shared")
            .unwrap()
            .source,
        Origin::agent("reader", Some("bad-reply"))
    );
    let before = fs::read(lab.state.join("mind/store.json")).unwrap();
    let mut missing = lab.config("write");
    missing.binary = "/missing/engine".into();
    let idle = agent_antigravity::run_once_controlled(
        &mut runtime,
        &queue,
        missing,
        &AtomicBool::new(false),
        None,
    )
    .unwrap();
    assert_eq!(idle.status, "idle");
    assert_eq!(fs::read(lab.state.join("mind/store.json")).unwrap(), before);
    assert!(
        queue
            .resume_needs_auth(&["bad-reply".into()], agent_host::now().unwrap())
            .is_err()
    );
}

#[test]
fn social_write_identity_is_durable_even_when_invalid_target_prevents_execution() {
    let lab = Lab::new();
    let mut runtime = lab.runtime();
    lab.seed();
    let queue = lab.enqueue(
        "invalid-write",
        ctx("reader", "chat-a"),
        "synthetic invalid write",
    );
    let report = lab.run(&mut runtime, &queue, "invalid-write", &["social_send"]);
    assert_eq!(report.status, "completed", "{report:?} {}", lab.failure());
    assert_eq!(report.tool_calls, 1);
    assert_eq!(report.operation_ids.len(), 1);
    let job = queue.get("invalid-write").unwrap();
    assert_eq!(job.progress.operation_ids, report.operation_ids);
    let evidence = serde_json::to_value(report.action_evidence.unwrap()).unwrap();
    assert_eq!(evidence["counts"]["not_recorded"], 1);
    assert_eq!(evidence["counts"]["verified"], 0);
    assert!(!lab.state.join("outbox").exists());
}

#[test]
fn stop_before_claim_leaves_pending_job_and_skips_missing_native_binary() {
    let lab = Lab::new();
    let mut runtime = lab.runtime();
    lab.seed();
    let queue = lab.enqueue(
        "not-started",
        ctx("reader", "chat-a"),
        "synthetic stopped task",
    );
    let mut config = lab.config("write");
    config.binary = "/missing/engine".into();
    let report = agent_antigravity::run_once_controlled(
        &mut runtime,
        &queue,
        config,
        &AtomicBool::new(true),
        None,
    )
    .unwrap();
    assert_eq!(report.status, "stopped");
    assert_eq!(queue.get("not-started").unwrap().status, Status::Pending);
    assert!(!lab.state.join("engine-bridge").exists());
}
