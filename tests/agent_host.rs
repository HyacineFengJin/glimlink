// SPDX-License-Identifier: MIT
#![cfg(feature = "agent-host-tests")]
//! Real Runtime + native app-server fixture, with only synthetic private state.
//! No model service, platform connection, saved account or subscription is used.
use agent_runtime::{
    agent_host::{self, run_once, run_once_controlled, run_once_with_stop},
    agent_sources::{PollStatus, Source, SourceStore, WatchConfig},
    app::Runtime,
    outbox::{Operation, Outbox, Status as ActionStatus},
};
use runtime_core::{Platform, Scope};
use runtime_engine::{CodexClient, CodexConfig, Input, ToolCall, ToolContent, TurnStatus};
use runtime_jobs::{Outcome, Progress, Queue, Status};
use serde_json::{Value, json};
use std::{
    fs,
    os::unix::fs::PermissionsExt,
    path::PathBuf,
    sync::atomic::{AtomicBool, AtomicU64, Ordering},
    time::Duration,
};

static NEXT: AtomicU64 = AtomicU64::new(0);
const PRIVATE_PROMPT: &str = "synthetic-operator-prompt-do-not-expose-in-agent-status";
const PRIVATE_EVENT: &str = "synthetic-platform-text-do-not-expose-in-agent-status";

struct Lab {
    root: PathBuf,
    state: PathBuf,
}
impl Lab {
    fn new() -> Self {
        let root = std::env::temp_dir().join(format!(
            "runtime-agent-host-fixture-{}-{}",
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
    fn queue(&self) -> Queue {
        Queue::open(&self.state).unwrap()
    }
    fn config(&self, mode: &str) -> CodexConfig {
        let mut config = CodexConfig::new(
            env!("CARGO_BIN_EXE_agent-host-fixture"),
            self.root.join("subscription-home"),
            self.root.join("workspace"),
        );
        config.args = vec!["app-server".into(), mode.into()];
        config.timeout = Duration::from_secs(3);
        config
    }
    fn missing_engine(&self) -> CodexConfig {
        let mut config = self.config("agent_job");
        config.binary = self.root.join("native-binary-must-not-be-spawned");
        config
    }
}
impl Drop for Lab {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.root);
    }
}

fn status_tool(runtime: &mut Runtime) -> Value {
    let result = runtime.dispatch_engine_tool(&ToolCall {
        thread_id: "host-status-fixture".into(),
        turn_id: "status-turn".into(),
        call_id: format!("status-{}", NEXT.fetch_add(1, Ordering::Relaxed)),
        tool: "runtime_agent_status".into(),
        arguments: json!({}),
    });
    assert!(result.success);
    let text = result
        .content_items
        .iter()
        .find_map(|item| match item {
            ToolContent::InputText { text } => Some(text.as_str()),
            _ => None,
        })
        .unwrap();
    assert!(!text.contains(PRIVATE_PROMPT));
    assert!(!text.contains(PRIVATE_EVENT));
    assert!(!text.contains("fixture@example.invalid"));
    serde_json::from_str(text).unwrap()
}

#[test]
fn one_pending_job_runs_a_real_runtime_tool_and_persists_completed_progress() {
    let lab = Lab::new();
    let mut runtime = lab.runtime();
    let queue = lab.queue();
    let id = "complete-one";
    queue
        .enqueue(
            id,
            PRIVATE_PROMPT,
            Some(json!({"text":PRIVATE_EVENT})),
            agent_host::now().unwrap(),
        )
        .unwrap();
    let report = run_once(&mut runtime, &queue, lab.config("agent_job"), None).unwrap();
    assert_eq!(report.status, "completed");
    assert_eq!(report.job_id.as_deref(), Some(id));
    assert_eq!(report.tool_calls, 1);
    assert!(report.operation_ids.is_empty());
    let evidence = report
        .action_evidence
        .as_ref()
        .expect("run_once must aggregate action evidence even for a read-only job");
    assert_eq!(evidence.counts.total, 0);
    assert!(evidence.actions.is_empty());
    assert!(evidence.readonly && !evidence.needs_attention);
    assert!(
        !lab.state.join("outbox").exists(),
        "empty evidence must not create a ledger"
    );
    assert!(report.thread_id.is_some() && report.turn_id.is_some());
    assert!(!report.output_truncated);
    let saved = lab.queue().get(id).unwrap();
    assert_eq!(saved.status, Status::Completed);
    assert_eq!(saved.progress.tool_calls, 1);
    assert_eq!(saved.progress.thread_id, report.thread_id);
    assert_eq!(saved.progress.turn_id, report.turn_id);
    assert!(saved.progress.operation_ids.is_empty());
    assert_eq!(
        saved.text.as_deref(),
        Some("Read-only Runtime job fixture completed.")
    );
    assert!(saved.started_at.is_some() && saved.finished_at.is_some());
    let status = status_tool(&mut runtime);
    assert_eq!(status["jobs"]["counts"]["completed"], 1);
    assert_eq!(status["network_requests"], 0);
    let idle = run_once(&mut runtime, &queue, lab.missing_engine(), None).unwrap();
    assert_eq!(idle.status, "idle");
    assert!(idle.job_id.is_none());
    assert_eq!(idle.tool_calls, 0);
    assert_eq!(queue.get(id).unwrap(), saved);
}

#[test]
fn tool_then_engine_eof_stays_interrupted_after_runtime_and_queue_restart() {
    let lab = Lab::new();
    let id = "interrupt-after-tool";
    let before = {
        let mut runtime = lab.runtime();
        let queue = lab.queue();
        queue
            .enqueue(id, PRIVATE_PROMPT, None, agent_host::now().unwrap())
            .unwrap();
        let report = run_once(
            &mut runtime,
            &queue,
            lab.config("agent_job_interrupt"),
            None,
        )
        .unwrap();
        assert_eq!(report.status, "interrupted");
        assert_eq!(report.tool_calls, 1);
        let saved = queue.get(id).unwrap();
        assert_eq!(saved.status, Status::Interrupted);
        assert_eq!(saved.progress.tool_calls, 1);
        assert!(saved.progress.thread_id.is_some() && saved.progress.turn_id.is_some());
        assert!(saved.progress.operation_ids.is_empty());
        assert!(saved.text.is_none() && saved.error.is_some());
        saved
    };
    let mut runtime = lab.runtime();
    let queue = lab.queue();
    assert!(
        queue
            .recover_interrupted(agent_host::now().unwrap())
            .unwrap()
            .is_empty()
    );
    let report = run_once(&mut runtime, &queue, lab.missing_engine(), None).unwrap();
    assert_eq!(report.status, "idle");
    assert_eq!(report.tool_calls, 0);
    assert_eq!(queue.get(id).unwrap(), before);
    assert!(
        queue
            .resume_needs_auth(&[id.into()], agent_host::now().unwrap())
            .is_err()
    );
    let status = status_tool(&mut runtime);
    assert_eq!(status["jobs"]["counts"]["interrupted"], 1);
    assert_eq!(status["interrupted_jobs_auto_restarted"], false);
}

#[test]
fn missing_subscription_is_needs_auth_until_explicit_resume_then_completes() {
    let lab = Lab::new();
    let mut runtime = lab.runtime();
    let queue = lab.queue();
    let id = "subscription-required";
    queue
        .enqueue(id, PRIVATE_PROMPT, None, agent_host::now().unwrap())
        .unwrap();
    let report = run_once(
        &mut runtime,
        &queue,
        lab.config("agent_job_unauthenticated"),
        None,
    )
    .unwrap();
    assert_eq!(report.status, "needs_auth");
    assert_eq!(report.tool_calls, 0);
    assert!(report.thread_id.is_none() && report.turn_id.is_none());
    assert!(report.operation_ids.is_empty());
    let saved = queue.get(id).unwrap();
    assert_eq!(saved.status, Status::NeedsAuth);
    assert_eq!(saved.progress, Progress::default());
    assert!(saved.text.is_none());
    let idle = run_once(&mut runtime, &queue, lab.missing_engine(), None).unwrap();
    assert_eq!(idle.status, "idle");
    assert_eq!(queue.get(id).unwrap(), saved);
    assert_eq!(
        queue
            .resume_needs_auth(&[id.into()], agent_host::now().unwrap())
            .unwrap(),
        1
    );
    assert_eq!(queue.get(id).unwrap().status, Status::Pending);
    let finished = run_once(&mut runtime, &queue, lab.config("agent_job"), None).unwrap();
    assert_eq!(finished.status, "completed");
    assert_eq!(finished.tool_calls, 1);
    assert!(finished.thread_id.is_some());
    assert_eq!(queue.get(id).unwrap().status, Status::Completed);
}

#[test]
fn a_preexisting_stop_flag_leaves_pending_job_unclaimed_without_spawning() {
    let lab = Lab::new();
    let mut runtime = lab.runtime();
    let queue = lab.queue();
    let id = "stop-before-claim";
    let saved = queue
        .enqueue(id, PRIVATE_PROMPT, None, agent_host::now().unwrap())
        .unwrap();
    let report = run_once_with_stop(
        &mut runtime,
        &queue,
        lab.missing_engine(),
        None,
        &AtomicBool::new(true),
    )
    .unwrap();
    assert_eq!(report.status, "stopped");
    assert!(report.job_id.is_none());
    assert_eq!(report.tool_calls, 0);
    assert_eq!(queue.get(id).unwrap(), saved);
    assert_eq!(queue.get(id).unwrap().status, Status::Pending);
    assert!(queue.get(id).unwrap().started_at.is_none());
    assert!(!lab.root.join("subscription-home").exists());
}

#[test]
fn public_agent_status_does_not_expose_pending_prompt_or_platform_event_text() {
    let lab = Lab::new();
    let mut runtime = lab.runtime();
    let queue = lab.queue();
    queue
        .enqueue(
            "private-pending",
            PRIVATE_PROMPT,
            Some(json!({"text":PRIVATE_EVENT})),
            agent_host::now().unwrap(),
        )
        .unwrap();
    let status = status_tool(&mut runtime);
    assert_eq!(status["status"], "ok");
    assert_eq!(status["jobs"]["counts"]["pending"], 1);
    assert_eq!(status["jobs"]["total"], 1);
    assert_eq!(
        queue.get("private-pending").unwrap().status,
        Status::Pending
    );
}

#[test]
fn staged_qq_watch_replays_idempotently_then_runs_through_native_host_to_completion() {
    let lab = Lab::new();
    let mut runtime = lab.runtime();
    let queue = lab.queue();
    let config = WatchConfig {
        context: runtime_jobs::AgentContext::default(),
        id: "synthetic-qq-watch".into(),
        source: Source::QqMessages,
        account: "fixture-account".into(),
        operator_prompt: PRIVATE_PROMPT.into(),
        interval_seconds: 5,
        enabled: true,
        include_existing: true,
    };
    let mut sources = SourceStore::open(&lab.state).unwrap();
    sources.configure(config.clone()).unwrap();
    let batch = sources
        .collect_with(&config.id, 100, |method, params| {
            assert_eq!(method, "social.messages");
            assert_eq!(params["platform"], "qq");
            assert_eq!(params["account"], "fixture-account");
            assert_eq!(params["refresh"], true);
            assert_eq!(params["wait_ms"], 0);
            assert!(params.get("start").is_none());
            // Same task response shape as agent_sources' QQ fixtures. These opaque
            // refs are synthetic and are never dereferenced by the native fixture.
            Ok(json!({"status":"ok","scope":config.scope(),"messages":[{
            "local_sequence":1,"direction":"incoming","text":PRIVATE_EVENT,
            "target_ref":format!("{:032x}",901),"source_ref":format!("{:032x}",1901),
            "server_time":1700000001
        }],"continuation":format!("{:032x}",1),"has_more":false,"marked_read":false,
        "retention":{"newest_sequence":1,"history":"runtime_observed_only"},"gap":null}))
        })
        .unwrap();
    assert_eq!(batch.status, PollStatus::Ready);
    assert_eq!(batch.events.len(), 1);
    assert_eq!(batch.events[0].source_key, "local_sequence:1");
    let event = &batch.events[0];
    let first = queue
        .enqueue(
            &event.job_id,
            &batch.operator_prompt,
            Some(serde_json::to_value(event).unwrap()),
            event.observed_at,
        )
        .unwrap();
    assert_eq!(first.status, Status::Pending);
    assert!(sources.status(&config.id).unwrap().pending);
    assert!(!sources.status(&config.id).unwrap().initialized);
    drop(sources); // Simulate interruption after enqueue, before cursor commit.

    let mut sources = SourceStore::open(&lab.state).unwrap();
    let replay = sources
        .collect_with(&config.id, 999, |_, _| {
            panic!("an uncommitted staged batch must replay without any provider call")
        })
        .unwrap();
    assert_eq!(replay, batch);
    for event in &replay.events {
        let same = queue
            .enqueue(
                &event.job_id,
                &replay.operator_prompt,
                Some(serde_json::to_value(event).unwrap()),
                event.observed_at,
            )
            .unwrap();
        assert_eq!(same, first);
    }
    assert_eq!(queue.list(128).unwrap().len(), 1);
    let committed = sources.commit(&replay).unwrap();
    assert!(committed.initialized);
    assert!(!committed.pending);
    assert_eq!(committed.generation, 1);
    assert!(sources.pending_job_ids().unwrap().is_empty());
    drop(sources);

    let report = run_once(&mut runtime, &queue, lab.config("agent_job"), None).unwrap();
    assert_eq!(report.job_id.as_deref(), Some(event.job_id.as_str()));
    assert_eq!(report.status, "completed");
    assert_eq!(report.tool_calls, 1);
    assert!(report.operation_ids.is_empty());
    let saved = lab.queue().get(&event.job_id).unwrap();
    assert_eq!(saved.status, Status::Completed);
    assert_eq!(saved.prompt, config.operator_prompt);
    assert_eq!(
        saved.event.as_ref().unwrap()["payload"]["item"]["text"],
        PRIVATE_EVENT
    );
    assert_eq!(saved.progress.tool_calls, 1);
    assert!(saved.progress.thread_id.is_some() && saved.progress.turn_id.is_some());
    assert_eq!(status_tool(&mut runtime)["jobs"]["counts"]["completed"], 1);
    let idle = run_once(&mut runtime, &queue, lab.missing_engine(), None).unwrap();
    assert_eq!(idle.status, "idle");
    assert_eq!(queue.get(&event.job_id).unwrap(), saved);
}

#[test]
fn invalid_tool_allowlists_fail_before_claim_and_leave_the_pending_job_unchanged() {
    let lab = Lab::new();
    let mut runtime = lab.runtime();
    let queue = lab.queue();
    let saved = queue
        .enqueue(
            "allowlist-gate",
            PRIVATE_PROMPT,
            None,
            agent_host::now().unwrap(),
        )
        .unwrap();
    for allowed in [
        vec![
            "runtime_modules".to_owned(),
            "unregistered_agent_tool".to_owned(),
        ],
        vec!["runtime_modules".to_owned(), "runtime_modules".to_owned()],
    ] {
        let error = run_once_controlled(
            &mut runtime,
            &queue,
            lab.missing_engine(),
            None,
            &AtomicBool::new(false),
            Some(&allowed),
        )
        .unwrap_err();
        assert!(
            error.contains("allowed-tools"),
            "allowlist validation must precede engine spawn: {error}"
        );
        assert_eq!(queue.get(&saved.id).unwrap(), saved);
        assert_eq!(saved.status, Status::Pending);
        assert!(saved.started_at.is_none());
        assert!(!lab.root.join("subscription-home").exists());
        assert!(!lab.state.join("outbox").exists());
    }
}

#[test]
fn empty_allowlist_never_defaults_to_the_registered_runtime_tools() {
    let lab = Lab::new();
    let mut runtime = lab.runtime();
    let queue = lab.queue();
    queue
        .enqueue(
            "deny-all-tools",
            PRIVATE_PROMPT,
            None,
            agent_host::now().unwrap(),
        )
        .unwrap();
    // Existing agent_job deliberately requires runtime_modules in thread/start.
    // With a genuine empty catalog it exits before a tool can be dispatched;
    // silently falling back to the default catalog would complete one tool.
    let allowed = Vec::<String>::new();
    let report = run_once_controlled(
        &mut runtime,
        &queue,
        lab.config("agent_job"),
        None,
        &AtomicBool::new(false),
        Some(&allowed),
    )
    .unwrap();
    assert_eq!(report.status, "failed");
    assert_eq!(report.tool_calls, 0);
    assert!(report.operation_ids.is_empty());
    assert_eq!(
        queue.get("deny-all-tools").unwrap().progress,
        Progress::default()
    );
    assert_eq!(report.action_evidence.as_ref().unwrap().counts.total, 0);
    assert!(!lab.state.join("outbox").exists());

    // The existing protocol fixture can also emit an actually unknown tool.
    // Resume skips its unrelated thread/start schema assertions, allowing us
    // to verify the empty local allowlist refuses the request before callback.
    let mut client = CodexClient::spawn(lab.config("tool-unknown")).unwrap();
    client.initialize().unwrap();
    let thread = client
        .thread_resume_with_tools("thread-fixture", &[])
        .unwrap();
    let mut callbacks = 0;
    let output = client
        .turn_start_with_tools(
            &thread.id,
            &[Input::Text {
                text: "Synthetic no-tools protocol turn".into(),
            }],
            |_| {},
            |_| {
                callbacks += 1;
                Ok(runtime_engine::ToolResult::text(
                    "unexpected callback",
                    false,
                ))
            },
        )
        .unwrap();
    assert_eq!(output.status, TurnStatus::Completed);
    assert_eq!(
        callbacks, 0,
        "unregistered tools must not enter Runtime dispatch"
    );
    assert_eq!(queue.get("deny-all-tools").unwrap().status, Status::Failed);
    assert!(!lab.state.join("outbox").exists());
}

const PRIVATE_ACTION_MESSAGE: &str = "synthetic-ledger-message-must-not-appear-in-evidence";
const PRIVATE_ACTION_TARGET: &str = "synthetic-ledger-target-must-not-appear-in-evidence";
const PRIVATE_ACTION_REQUEST: &str = "synthetic-ledger-frozen-request-must-not-appear-in-evidence";
const PRIVATE_ACTION_ACTOR: &str = "synthetic-ledger-actor-must-not-appear-in-evidence";
const PRIVATE_MODEL_RESPONSE: &str =
    "synthetic-model-response-must-not-appear-in-model-facing-job-status";

fn action_fixture(outbox: &Outbox, id: &str, status: ActionStatus) -> Operation {
    let mut record = Operation::new(
        id,
        Scope {
            platform: Platform::Github,
            account: "synthetic-evidence-owner".into(),
        },
        PRIVATE_ACTION_ACTOR.into(),
        json!({"private_target":PRIVATE_ACTION_TARGET}),
        PRIVATE_ACTION_MESSAGE,
    )
    .unwrap();
    record.request = Some(json!({"private_request":PRIVATE_ACTION_REQUEST}));
    outbox.insert(&record).unwrap();
    if status == ActionStatus::Prepared {
        return record;
    }
    if status == ActionStatus::AlreadySatisfied {
        record.status = status;
        record.verification = Some(json!({"verified":true}));
        outbox.finish(&record).unwrap();
        return record;
    }
    outbox.begin_attempt(&mut record).unwrap();
    if status == ActionStatus::Attempting {
        return record;
    }
    if status == ActionStatus::Verified {
        record.status = ActionStatus::Sent;
        record.receipt = Some(json!({"id":"synthetic-receipt"}));
        outbox.finish(&record).unwrap();
        record.status = status;
        record.verification = Some(json!({"verified":true}));
        outbox.finish(&record).unwrap();
        return record;
    }
    record.status = status;
    outbox.finish(&record).unwrap();
    record
}

#[test]
fn completed_job_query_exposes_all_action_evidence_while_status_is_bounded_and_private() {
    let lab = Lab::new();
    let mut runtime = lab.runtime();
    let queue = lab.queue();
    let outbox = Outbox::open(&lab.state).unwrap();
    let fixture = [
        ("verified-op", ActionStatus::Verified),
        ("sent-op", ActionStatus::Sent),
        ("attempting-op", ActionStatus::Attempting),
        ("unknown-op", ActionStatus::Unknown),
        ("rejected-op", ActionStatus::Rejected),
        ("prepared-op", ActionStatus::Prepared),
        ("already-op", ActionStatus::AlreadySatisfied),
        ("verified-extra", ActionStatus::Verified),
        ("sent-extra", ActionStatus::Sent),
        ("already-extra", ActionStatus::AlreadySatisfied),
    ];
    let mut ids = Vec::new();
    for (id, status) in fixture {
        action_fixture(&outbox, id, status);
        ids.push(id.to_owned());
    }
    ids.extend(["missing-op".into(), "invalid-op".into()]);
    let invalid = lab.state.join("outbox/invalid-op.json");
    fs::write(&invalid, b"{\"schema\":999}").unwrap();
    fs::set_permissions(&invalid, fs::Permissions::from_mode(0o600)).unwrap();
    let attempting_before = fs::read(lab.state.join("outbox/attempting-op.json")).unwrap();
    let time = agent_host::now().unwrap();
    let id = "completed-with-unresolved-actions";
    queue
        .enqueue(
            id,
            PRIVATE_PROMPT,
            Some(json!({"private_event":PRIVATE_EVENT})),
            time,
        )
        .unwrap();
    let claim = queue.claim_next(time).unwrap().unwrap();
    queue
        .update_progress(
            &claim.lease,
            Progress {
                thread_id: Some("synthetic-evidence-thread".into()),
                turn_id: Some("synthetic-evidence-turn".into()),
                operation_ids: ids.clone(),
                tool_calls: 12,
            },
            time,
        )
        .unwrap();
    queue
        .finish(
            &claim.lease,
            Outcome::completed(PRIVATE_MODEL_RESPONSE),
            time,
        )
        .unwrap();

    let full = agent_runtime::agent_cli::command(&lab.state, "public", &["job".into(), id.into()])
        .unwrap();
    assert_eq!(full["job"]["status"], "completed");
    assert_eq!(
        full["job"]["progress"]["operation_ids"]
            .as_array()
            .unwrap()
            .len(),
        12
    );
    let evidence = &full["action_evidence"];
    assert_eq!(
        evidence["counts"],
        json!({"total":12,"verified":2,"acknowledged_only":2,"unknown":2,"rejected":1,"not_recorded":1,"prepared":1,"already_satisfied":2,"invalid_record":1})
    );
    assert_eq!(evidence["needs_attention"], true);
    assert_eq!(evidence["readonly"], true);
    let actions = evidence["actions"].as_array().unwrap();
    assert_eq!(actions.len(), 12);
    for expected in &ids {
        assert!(
            actions
                .iter()
                .any(|action| action["operation_id"].as_str() == Some(expected.as_str()))
        );
    }
    let attempting = actions
        .iter()
        .find(|action| action["operation_id"] == "attempting-op")
        .unwrap();
    assert_eq!(attempting["status"], "unknown");
    assert_eq!(attempting["execution_may_be_in_flight"], true);
    assert_eq!(
        actions
            .iter()
            .find(|a| a["operation_id"] == "sent-op")
            .unwrap()["status"],
        "acknowledged_only"
    );

    let public = status_tool(&mut runtime);
    assert_eq!(
        public["jobs"]["counts"]["completed"], 1,
        "engine completion must remain distinct from unverified action outcomes"
    );
    let item = public["jobs"]["items"]
        .as_array()
        .unwrap()
        .iter()
        .find(|item| item["id"] == id)
        .unwrap();
    assert_eq!(item["status"], "completed");
    assert_eq!(item["action_evidence"]["counts"], evidence["counts"]);
    assert_eq!(item["action_evidence"]["needs_attention"], true);
    assert_eq!(
        item["progress"]["operation_ids"].as_array().unwrap().len(),
        8
    );
    assert_eq!(item["progress"]["operation_count"], 12);
    assert_eq!(item["progress"]["operation_ids_truncated"], true);
    assert!(
        item["action_evidence"].get("actions").is_none(),
        "status returns counts, not every private action record"
    );
    let detail_result = runtime.dispatch_engine_tool(&ToolCall {
        thread_id: "host-job-detail-fixture".into(),
        turn_id: "detail-turn".into(),
        call_id: format!("job-detail-{}", NEXT.fetch_add(1, Ordering::Relaxed)),
        tool: "runtime_agent_status".into(),
        arguments: json!({"job_id":id}),
    });
    assert!(detail_result.success);
    let detail_text = detail_result
        .content_items
        .iter()
        .find_map(|item| match item {
            ToolContent::InputText { text } => Some(text.as_str()),
            _ => None,
        })
        .unwrap();
    let detail: Value = serde_json::from_str(detail_text).unwrap();
    assert_eq!(detail["job"]["id"], id);
    assert_eq!(detail["job"]["status"], "completed");
    assert_eq!(detail["action_evidence"]["counts"], evidence["counts"]);
    assert_eq!(detail["action_evidence"]["needs_attention"], true);
    assert_eq!(
        detail["action_evidence"]["actions"]
            .as_array()
            .unwrap()
            .len(),
        12
    );
    for expected in &ids {
        assert!(
            detail["action_evidence"]["actions"]
                .as_array()
                .unwrap()
                .iter()
                .any(|action| action["operation_id"].as_str() == Some(expected.as_str())),
            "every action must be returned by the single model tool query"
        );
    }
    assert_eq!(detail["network_requests"], 0);
    assert_eq!(detail["automatic_replay"], false);
    for field in ["prompt", "event", "text"] {
        assert!(
            detail["job"].get(field).is_none(),
            "model-facing job status must omit {field}"
        );
    }
    for private in [
        PRIVATE_PROMPT,
        PRIVATE_EVENT,
        PRIVATE_MODEL_RESPONSE,
        PRIVATE_ACTION_MESSAGE,
        PRIVATE_ACTION_TARGET,
        PRIVATE_ACTION_REQUEST,
        PRIVATE_ACTION_ACTOR,
    ] {
        assert!(!detail_text.contains(private), "{private}");
    }
    let unknown = runtime.dispatch_engine_tool(&ToolCall {
        thread_id: "host-job-detail-fixture".into(),
        turn_id: "detail-turn".into(),
        call_id: format!(
            "unknown-job-detail-{}",
            NEXT.fetch_add(1, Ordering::Relaxed)
        ),
        tool: "runtime_agent_status".into(),
        arguments: json!({"job_id":"unknown-job-fixture"}),
    });
    assert!(
        !unknown.success,
        "an unknown job must fail instead of returning another job or the global summary"
    );
    let unknown_text = unknown
        .content_items
        .iter()
        .filter_map(|item| match item {
            ToolContent::InputText { text } => Some(text.as_str()),
            _ => None,
        })
        .collect::<Vec<_>>()
        .join("\n");
    assert!(!unknown_text.contains(id));
    for private in [
        PRIVATE_PROMPT,
        PRIVATE_EVENT,
        PRIVATE_MODEL_RESPONSE,
        PRIVATE_ACTION_MESSAGE,
        PRIVATE_ACTION_TARGET,
        PRIVATE_ACTION_REQUEST,
        PRIVATE_ACTION_ACTOR,
    ] {
        assert!(!unknown_text.contains(private), "{private}");
    }
    for projection in [
        serde_json::to_string(&full).unwrap(),
        serde_json::to_string(&public).unwrap(),
    ] {
        for secret in [
            PRIVATE_ACTION_MESSAGE,
            PRIVATE_ACTION_TARGET,
            PRIVATE_ACTION_REQUEST,
            PRIVATE_ACTION_ACTOR,
        ] {
            assert!(!projection.contains(secret), "{secret}");
        }
    }
    assert_eq!(
        fs::read(lab.state.join("outbox/attempting-op.json")).unwrap(),
        attempting_before
    );
    assert_eq!(
        outbox.inspect("attempting-op").unwrap().status,
        ActionStatus::Attempting
    );
    assert_eq!(queue.get(id).unwrap().status, Status::Completed);
}

#[test]
fn corrupt_source_journal_does_not_block_pending_work_or_allow_unverified_history_gc() {
    let lab = Lab::new();
    let queue = lab.queue();
    let time = agent_host::now().unwrap();
    queue
        .enqueue(
            "existing-history",
            "synthetic completed history",
            None,
            time,
        )
        .unwrap();
    let historical = queue.claim_next(time).unwrap().unwrap();
    queue
        .finish(
            &historical.lease,
            Outcome::completed("keep until source pins can be inspected"),
            time,
        )
        .unwrap();
    queue
        .enqueue("queued-independent-work", PRIVATE_PROMPT, None, time)
        .unwrap();
    let sources = lab.state.join("agent-sources");
    fs::create_dir(&sources).unwrap();
    fs::set_permissions(&sources, fs::Permissions::from_mode(0o700)).unwrap();
    let broken = sources.join("broken.json");
    let invalid = b"synthetic malformed private source record";
    fs::write(&broken, invalid).unwrap();
    fs::set_permissions(&broken, fs::Permissions::from_mode(0o600)).unwrap();

    // CLI settings retain native default arguments and the provider's default
    // model. The native daemon peer performs a read-only Runtime module request;
    // it must not require the model selected only by profile-specific tests.
    let result = agent_runtime::agent_cli::command(
        &lab.state,
        "public",
        &[
            "run".into(),
            "--once".into(),
            "--binary".into(),
            env!("CARGO_BIN_EXE_agent-host-fixture").into(),
            "--timeout-seconds".into(),
            "3".into(),
            "--retain-finished".into(),
            "0".into(),
        ],
    )
    .unwrap();
    assert_eq!(result["completed_jobs"], 1);
    assert_eq!(result["last_job"]["id"], "queued-independent-work");
    assert_eq!(result["last_job"]["status"], "completed");
    assert_eq!(
        queue.get("queued-independent-work").unwrap().status,
        Status::Completed
    );
    assert_eq!(
        queue.get("existing-history").unwrap().status,
        Status::Completed,
        "even retain=0 must not GC history while uncommitted source pins cannot be validated"
    );
    assert_eq!(
        fs::read(&broken).unwrap(),
        invalid,
        "host must not erase or repair malformed source evidence implicitly"
    );
    assert!(SourceStore::open(&lab.state).is_err());
    assert!(!lab.state.join("outbox").exists());
}
