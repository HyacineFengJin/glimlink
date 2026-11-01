// SPDX-License-Identifier: MIT
use agent_runtime::{chat_commands as commands, control_console as control};
use runtime_control::console::{Binding, Command, CommandConfig, Mode};
use runtime_jobs::{AgentContext, Queue, Status};
use serde_json::{Value, json};
use std::{
    path::{Path, PathBuf},
    sync::atomic::{AtomicU64, Ordering},
};
static N: AtomicU64 = AtomicU64::new(0);
struct Lab(PathBuf);
impl Lab {
    fn new() -> Self {
        let p = std::env::temp_dir().join(format!(
            "control-console-{}-{}",
            std::process::id(),
            N.fetch_add(1, Ordering::Relaxed)
        ));
        runtime_core::resources::private_dir(&p).unwrap();
        Self(p)
    }
}
impl Drop for Lab {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}
fn profile(p: &Path) {
    control::execute(
        p,
        Command::Profile {
            agent: "main".into(),
            engine: "codex".into(),
            model: "gpt-6.1-terra".into(),
            expected_revision: 0,
        },
    )
    .unwrap();
}
fn send(id: &str) -> Command {
    Command::Send {
        id: id.into(),
        agent: "main".into(),
        persona: "main".into(),
        conversation: "chat-1".into(),
        text: "Hello / 你好".into(),
    }
}
#[test]
fn managed_chat_works_while_runtime_holds_its_kernel_lock() {
    let l = Lab::new();
    profile(&l.0);
    let _runtime = agent_runtime::app::Runtime::open(&l.0, "public", None).unwrap();
    assert_eq!(
        control::execute(&l.0, send("message-1")).unwrap()["queued"],
        true
    );
    assert_eq!(
        control::execute(&l.0, send("message-1")).unwrap()["reused"],
        true
    );
    let q = Queue::open(&l.0).unwrap();
    let now = agent_runtime::agent_host::now().unwrap();
    let claim = q.claim_job("message-1", now).unwrap().unwrap();
    q.finish(
        &claim.lease,
        runtime_jobs::Outcome::completed("Reply with a real saved result"),
        now + 1,
    )
    .unwrap();
    let view = control::execute(
        &l.0,
        Command::Conversation {
            persona: "main".into(),
            conversation: "chat-1".into(),
        },
    )
    .unwrap();
    assert_eq!(
        view["jobs"][0]["assistant"],
        "Reply with a real saved result"
    );
    let other = control::execute(
        &l.0,
        Command::Conversation {
            persona: "other".into(),
            conversation: "chat-1".into(),
        },
    )
    .unwrap();
    assert_eq!(other["jobs"].as_array().unwrap().len(), 0);
}
#[test]
fn profile_changes_keep_queued_identity_and_permissions() {
    let l = Lab::new();
    profile(&l.0);
    control::execute(&l.0, send("one")).unwrap();
    control::execute(
        &l.0,
        Command::Profile {
            agent: "main".into(),
            engine: "antigravity".into(),
            model: "gemini-3.8-flash-medium".into(),
            expected_revision: 1,
        },
    )
    .unwrap();
    assert!(
        control::execute(
            &l.0,
            Command::Profile {
                agent: "main".into(),
                engine: "codex".into(),
                model: "x".into(),
                expected_revision: 1
            }
        )
        .is_err()
    );
    let j = Queue::open(&l.0).unwrap().get("one").unwrap();
    let p = j.execution_profile.unwrap();
    assert_eq!(p.engine, "codex");
    assert_eq!(p.allowed_tools, Some(vec!["runtime_memory".into()]));
}
#[test]
fn paused_and_quiet_agent_gates_do_not_cancel_running_turns() {
    let l = Lab::new();
    profile(&l.0);
    control::execute(&l.0, send("one")).unwrap();
    let j = Queue::open(&l.0).unwrap().get("one").unwrap();
    control::execute(
        &l.0,
        Command::Mode {
            agent: "main".into(),
            mode: Mode::Paused,
        },
    )
    .unwrap();
    assert!(!control::eligible(&l.0, &j).unwrap());
    control::execute(
        &l.0,
        Command::Mode {
            agent: "main".into(),
            mode: Mode::Quiet,
        },
    )
    .unwrap();
    assert!(control::eligible(&l.0, &j).unwrap());
    let mut event = j.clone();
    event.event = Some(json!({"text":"platform"}));
    assert!(!control::eligible(&l.0, &event).unwrap());
    assert_eq!(
        Queue::open(&l.0).unwrap().get("one").unwrap().status,
        Status::Pending
    );
}
fn binding() -> Binding {
    Binding {
        id: "owner".into(),
        enabled: true,
        platform: "qq".into(),
        account: "qq-fixture".into(),
        chat_type: "private".into(),
        chat_id: "42".into(),
        sender_id: "42".into(),
        prefix: "/gl".into(),
        agent: "main".into(),
        allowed: vec![
            "disable".into(),
            "enable".into(),
            "agent".into(),
            "status".into(),
        ],
        reply: false,
        language: "en".into(),
    }
}
fn event(text: &str, at: u64) -> agent_runtime::agent_sources::SourceEvent {
    agent_runtime::agent_sources::SourceEvent {
        job_id: "source-one".into(),
        watch_id: "command-test".into(),
        scope: runtime_core::Scope {
            platform: runtime_core::Platform::Qq,
            account: "qq-fixture".into(),
        },
        source: agent_runtime::agent_sources::Source::QqMessages,
        source_key: "sequence:1".into(),
        identity_quality: "local_sequence".into(),
        observed_at: at,
        context: AgentContext::default(),
        execution_profile: None,
        payload: json!({"truncated":false,"item":{"conversation":{"actor":99,"destination":{"kind":"private","uid":"u_fixture"}},"to_uin":99,"from_uid":"u_fixture","direction":"incoming","truncated":false,"unexpanded":false,"server_time":at,"from_uin":42,"group_code":null,"segments":[{"type":"text","text":text}],"text":text,"target_ref":"synthetic-target"}}),
    }
}
fn setup() -> (Lab, u64) {
    let l = Lab::new();
    profile(&l.0);
    commands::configure(
        &l.0,
        CommandConfig {
            enabled: true,
            bindings: vec![binding()],
        },
        0,
    )
    .unwrap();
    let now = agent_runtime::agent_host::now().unwrap();
    (l, now)
}
#[test]
fn scoped_commands_never_enqueue_model_work_and_are_idempotent() {
    let (l, now) = setup();
    let e = event("/gl disable", now);
    assert!(commands::dispatch_with(&l.0, &e, now, |_, _, _, _| panic!("reply disabled")).unwrap());
    assert_eq!(control::mode(&l.0, "main").unwrap(), Mode::Paused);
    let revision = control::preferences(&l.0).unwrap().revision;
    assert!(commands::dispatch_with(&l.0, &e, now, |_, _, _, _| panic!()).unwrap());
    assert_eq!(control::preferences(&l.0).unwrap().revision, revision);
    assert!(Queue::open(&l.0).unwrap().list(128).unwrap().is_empty());
}
#[test]
fn commands_reject_wrong_senders_accounts_groups_forwarded_or_old_content() {
    let (l, now) = setup();
    let e = event("/gl disable", now);
    let mut bad = Vec::new();
    let mut x = e.clone();
    x.scope.account = "other".into();
    bad.push(x);
    for (key, v) in [
        ("from_uin", json!(99)),
        ("group_code", json!(123)),
        ("direction", json!("outgoing")),
        ("truncated", json!(true)),
        ("unexpanded", json!(true)),
        ("segments", json!([{"type":"reply"}])),
    ] {
        let mut x = e.clone();
        x.payload["item"][key] = v;
        bad.push(x);
    }
    let mut x = e.clone();
    x.payload["truncated"] = json!(true);
    bad.push(x);
    let mut x = e.clone();
    x.payload["item"]["server_time"] = json!(now.saturating_sub(1000));
    bad.push(x);
    for x in bad {
        assert!(!commands::dispatch_with(&l.0, &x, now, |_, _, _, _| panic!()).unwrap());
    }
    assert_eq!(control::mode(&l.0, "main").unwrap(), Mode::Active);
}
#[test]
fn natural_language_and_ai_task_commands_do_not_have_control_authority() {
    let (l, now) = setup();
    for s in [
        "please disable main",
        "quoted /gl disable",
        "/glimpse disable",
        "/gl ask do something",
    ] {
        let e = event(s, now);
        let handled = commands::dispatch_with(&l.0, &e, now, |_, _, _, _| panic!()).unwrap();
        if s == "/gl ask do something" {
            assert!(handled)
        } else {
            assert!(!handled)
        }
    }
    assert!(Queue::open(&l.0).unwrap().list(128).unwrap().is_empty());
    assert_eq!(control::mode(&l.0, "main").unwrap(), Mode::Active);
}
#[test]
fn command_config_is_default_denied_revisioned_and_no_wildcards() {
    let l = Lab::new();
    assert_eq!(
        commands::configuration(&l.0).unwrap()["config"]["enabled"],
        false
    );
    let mut b = binding();
    b.sender_id = "*".into();
    assert!(
        commands::configure(
            &l.0,
            CommandConfig {
                enabled: true,
                bindings: vec![b]
            },
            0
        )
        .is_err()
    );
    commands::configure(
        &l.0,
        CommandConfig {
            enabled: true,
            bindings: vec![binding()],
        },
        0,
    )
    .unwrap();
    assert!(commands::configure(&l.0, CommandConfig::default(), 0).is_err());
}
#[test]
fn unknown_or_oversized_management_frames_are_rejected() {
    let l = Lab::new();
    for input in [
        "{\"version\":1,\"id\":1,\"command\":{\"action\":\"shell\",\"command\":\"whoami\"}}\n"
            .as_bytes()
            .to_vec(),
        vec![b'x'; 65538],
    ] {
        assert!(control::serve(&l.0, std::io::Cursor::new(input), Vec::new()).is_err())
    }
    let mut out = Vec::new();
    control::serve(
        &l.0,
        std::io::Cursor::new(
            b"{\"version\":1,\"id\":7,\"command\":{\"action\":\"commands_get\"}}\n",
        ),
        &mut out,
    )
    .unwrap();
    let v: Value = serde_json::from_slice(&out).unwrap();
    assert_eq!(v["id"], 7);
    assert_eq!(v["ok"], true);
}

#[test]
fn group_commands_require_both_exact_group_and_sender_and_resolved_route() {
    let (l, now) = setup();
    let mut b = binding();
    b.chat_type = "group".into();
    b.chat_id = "123".into();
    commands::configure(
        &l.0,
        CommandConfig {
            enabled: true,
            bindings: vec![b],
        },
        1,
    )
    .unwrap();
    let mut e = event("/gl disable", now);
    e.payload["item"]["group_code"] = json!(123);
    e.payload["item"]["conversation"]["destination"] = json!({"kind":"group","code":123});
    assert!(commands::dispatch_with(&l.0, &e, now, |_, _, _, _| panic!()).unwrap());
    let mut wrong = e.clone();
    wrong.payload["item"]["group_code"] = json!(124);
    assert!(!commands::dispatch_with(&l.0, &wrong, now, |_, _, _, _| panic!()).unwrap());
    let mut unresolved = e.clone();
    unresolved.payload["item"]["conversation"]["destination"] = Value::Null;
    assert!(!commands::dispatch_with(&l.0, &unresolved, now, |_, _, _, _| panic!()).unwrap());
}
#[test]
fn reply_failure_does_not_repeat_management_or_send_to_another_target() {
    let (l, now) = setup();
    let mut b = binding();
    b.reply = true;
    commands::configure(
        &l.0,
        CommandConfig {
            enabled: true,
            bindings: vec![b],
        },
        1,
    )
    .unwrap();
    let e = event("/gl disable", now);
    let mut calls = 0;
    commands::dispatch_with(&l.0, &e, now, |account, target, op, _| {
        calls += 1;
        assert_eq!(account, "qq-fixture");
        assert_eq!(target, "synthetic-target");
        assert!(op.starts_with("command-reply-"));
        Err("synthetic unknown send".into())
    })
    .unwrap();
    commands::dispatch_with(&l.0, &e, now, |_, _, _, _| panic!("must not repeat")).unwrap();
    assert_eq!(calls, 1);
    assert_eq!(
        commands::configuration(&l.0).unwrap()["receipts"][0]["status"],
        "applied_reply_unconfirmed"
    );
}
#[test]
fn cached_account_labels_are_human_only_bounded_data_not_credentials() {
    let l = Lab::new();
    agent_runtime::operator_identities::remember(&l.0,"bilibili","main",&json!({"status":"online","user":{"name":"连接","id":7,"access_token":"never-copy"},"cookie":"never-copy"})).unwrap();
    let value = control::execute(&l.0, Command::Connections).unwrap();
    assert_eq!(value["identities"][0]["name"], "连接");
    assert!(!value.to_string().contains("never-copy"));
    agent_runtime::operator_identities::remember(
        &l.0,
        "bilibili",
        "main",
        &json!({"status":"signed_out"}),
    )
    .unwrap();
    assert!(
        control::execute(&l.0, Command::Connections).unwrap()["identities"][0]["name"].is_null()
    );
}
