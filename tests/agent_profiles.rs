// SPDX-License-Identifier: MIT
use agent_runtime::{
    agent_cli,
    agent_host::{EngineKind, Settings},
    agent_profiles::{ProfileInput, ProfileStore, apply_default_account},
    agent_sources::{Source, SourceStore, WatchConfig},
};
use runtime_jobs::{AgentContext, Queue};
use serde_json::json;
use std::{
    collections::BTreeMap,
    fs,
    os::unix::fs::PermissionsExt,
    path::PathBuf,
    sync::atomic::{AtomicU64, Ordering},
};
static NEXT: AtomicU64 = AtomicU64::new(0);
struct Lab(PathBuf);
impl Lab {
    fn new() -> Self {
        let path = std::env::temp_dir().join(format!(
            "agent-profiles-{}-{}",
            std::process::id(),
            NEXT.fetch_add(1, Ordering::Relaxed)
        ));
        fs::create_dir(&path).unwrap();
        fs::set_permissions(&path, fs::Permissions::from_mode(0o700)).unwrap();
        Self(path)
    }
}
impl Drop for Lab {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}
fn input(engine: &str, model: &str) -> ProfileInput {
    ProfileInput {
        engine: engine.into(),
        model: Some(model.into()),
        allowed_tools: Some(vec!["runtime_modules".into()]),
        default_accounts: BTreeMap::from([("tieba".into(), "personal".into())]),
    }
}
fn context() -> AgentContext {
    AgentContext {
        persona: "shared".into(),
        agent: "reader".into(),
        conversation: None,
    }
}

#[test]
fn operator_profile_persists_and_enqueue_freezes_policy_across_edits() {
    let lab = Lab::new();
    let config = lab.0.join("profile-input.json");
    fs::write(
        &config,
        serde_json::to_vec(&input("codex", "first-model")).unwrap(),
    )
    .unwrap();
    fs::set_permissions(&config, fs::Permissions::from_mode(0o600)).unwrap();
    let prompt = lab.0.join("prompt.txt");
    fs::write(&prompt, "synthetic operator task").unwrap();
    fs::set_permissions(&prompt, fs::Permissions::from_mode(0o600)).unwrap();
    let result = agent_cli::command(
        &lab.0,
        "public",
        &[
            "profile-set".into(),
            "reader".into(),
            config.to_str().unwrap().into(),
            "0".into(),
        ],
    )
    .unwrap();
    assert_eq!(result["profile"]["revision"], 1);
    agent_cli::command_with_context(
        &lab.0,
        "public",
        &[
            "enqueue".into(),
            "first".into(),
            prompt.to_str().unwrap().into(),
        ],
        &context(),
    )
    .unwrap();
    let first = Queue::open(&lab.0).unwrap().get("first").unwrap();
    let mut profiles = ProfileStore::open(&lab.0).unwrap();
    assert!(
        profiles
            .set("reader", input("antigravity", "second-model"), Some(0))
            .is_err()
    );
    let latest = profiles
        .set("reader", input("antigravity", "second-model"), Some(1))
        .unwrap();
    drop(profiles);
    agent_cli::command_with_context(
        &lab.0,
        "public",
        &[
            "enqueue".into(),
            "second".into(),
            prompt.to_str().unwrap().into(),
        ],
        &context(),
    )
    .unwrap();
    let queue = Queue::open(&lab.0).unwrap();
    let second = queue.get("second").unwrap();
    assert_eq!(queue.get("first").unwrap(), first);
    assert_eq!(first.execution_profile.as_ref().unwrap().engine, "codex");
    assert_eq!(second.execution_profile, Some(latest));
    assert_eq!(second.context.persona, "shared");
    assert!(
        queue
            .enqueue_configured(
                "first",
                "synthetic operator task",
                None,
                context(),
                second.execution_profile.clone(),
                999
            )
            .is_err()
    );
    let selected = Settings::default().for_job(&second).unwrap();
    assert_eq!(selected.engine, EngineKind::Antigravity);
    assert_eq!(selected.model.as_deref(), Some("second-model"));
    assert_eq!(
        selected.binary,
        PathBuf::from("/cache/bin/antigravity-native")
    );
    let original = Settings::default().for_job(&first).unwrap();
    assert_eq!(original.engine, EngineKind::Codex);
    assert_eq!(original.model.as_deref(), Some("first-model"));
}

#[test]
fn default_accounts_never_override_explicit_or_reference_owned_accounts() {
    let lab = Lab::new();
    let profile = ProfileStore::open(&lab.0)
        .unwrap()
        .set("reader", input("codex", "fixture"), Some(0))
        .unwrap();
    let mut fresh = json!({"action":"search","query":"Rust"});
    apply_default_account(&profile, "social_tieba", &mut fresh);
    assert_eq!(fresh["account"], "personal");
    for mut params in [
        json!({"account":"other","action":"forum"}),
        json!({"target_ref":"00000000000000000000000000000000"}),
        json!({"forum_ref":"00000000000000000000000000000000"}),
        json!({"continuation":"cursor"}),
        json!({"reference":"00000000000000000000000000000000"}),
    ] {
        let original = params.clone();
        apply_default_account(&profile, "social_tieba", &mut params);
        assert_eq!(params, original);
    }
    let mut direct = json!({"reference":"https://tieba.baidu.com/p/123"});
    apply_default_account(&profile, "social_open", &mut direct);
    assert_eq!(direct["account"], "personal");
    let mut unrelated = json!({"action":"search","query":"Rust"});
    apply_default_account(&profile, "social_bilibili", &mut unrelated);
    assert!(unrelated.get("account").is_none());
}

#[test]
fn staged_watch_event_keeps_profile_and_next_observation_uses_updated_profile() {
    let lab = Lab::new();
    let old = ProfileStore::open(&lab.0)
        .unwrap()
        .set("reader", input("codex", "old"), Some(0))
        .unwrap();
    let config = WatchConfig {
        id: "incoming".into(),
        source: Source::QqMessages,
        account: "personal".into(),
        operator_prompt: "synthetic task".into(),
        context: context(),
        interval_seconds: 5,
        enabled: true,
        include_existing: true,
    };
    let mut sources = SourceStore::open(&lab.0).unwrap();
    sources.configure(config.clone()).unwrap();
    let response = |n: u64| json!({"status":"ok","scope":config.scope(),"messages":[{"local_sequence":n,"direction":"incoming","text":"message","target_ref":format!("{n:032x}"),"source_ref":format!("{:032x}",n+10),"server_time":100+n}],"continuation":format!("{n:032x}"),"has_more":false,"marked_read":false,"retention":{"newest_sequence":n,"history":"runtime_observed_only"},"gap":null});
    let first = sources
        .collect_with("incoming", 100, |_, _| Ok(response(1)))
        .unwrap();
    assert_eq!(first.events.len(), 1);
    assert_eq!(first.events[0].execution_profile, Some(old));
    let updated = ProfileStore::open(&lab.0)
        .unwrap()
        .set("reader", input("antigravity", "new"), Some(1))
        .unwrap();
    let replay = sources
        .collect_with("incoming", 105, |_, _| {
            panic!("staged batch must not read again")
        })
        .unwrap();
    assert_eq!(first, replay);
    sources.commit(&first).unwrap();
    let second = sources
        .collect_with("incoming", 106, |_, _| Ok(response(2)))
        .unwrap();
    assert_eq!(second.events.len(), 1);
    assert_eq!(second.events[0].execution_profile, Some(updated));
}

#[test]
fn profile_rejects_unknown_tools_and_private_file_tampering() {
    let lab = Lab::new();
    let mut store = ProfileStore::open(&lab.0).unwrap();
    assert!(
        !agent_runtime::tools::specs()
            .iter()
            .any(|tool| tool.name.starts_with("agent_profile"))
    );
    let mut invalid = input("codex", "model");
    invalid.allowed_tools = Some(vec!["agent_profile_set".into()]);
    assert!(store.set("reader", invalid, Some(0)).is_err());
    let mut invalid = input("codex", "model");
    invalid.engine = "browser".into();
    assert!(store.set("reader", invalid, Some(0)).is_err());
    store
        .set("reader", input("codex", "model"), Some(0))
        .unwrap();
    drop(store);
    fs::set_permissions(
        lab.0.join("agent-profiles/profiles.json"),
        fs::Permissions::from_mode(0o644),
    )
    .unwrap();
    assert!(ProfileStore::open(&lab.0).is_err());
}

#[cfg(feature = "agent-host-tests")]
#[test]
fn frozen_profiles_run_both_native_engines_and_exact_claim_preserves_other_jobs() {
    use agent_runtime::{agent_antigravity, agent_host, app::Runtime, mind_store::Store};
    use runtime_engine::{AntigravityConfig, CodexConfig};
    use runtime_jobs::Status;
    use std::{sync::atomic::AtomicBool, time::Duration};
    let lab = Lab::new();
    let workspace = lab.0.join("workspace");
    fs::create_dir(&workspace).unwrap();
    let store = Store::open(&lab.0).unwrap();
    store
        .set_persona("shared", "MIND_PERSONA_SHARED_MARKER", 0)
        .unwrap();
    store
        .set_role("reader", "MIND_ROLE_READER_MARKER", 0)
        .unwrap();
    drop(store);
    let mut profiles = ProfileStore::open(&lab.0).unwrap();
    let codex = profiles
        .set("reader", input("codex", "fixture-codex"), Some(0))
        .unwrap();
    let agy = profiles
        .set("reader", input("antigravity", "fixture-agy"), Some(1))
        .unwrap();
    drop(profiles);
    let queue = Queue::open(&lab.0).unwrap();
    let now = agent_host::now().unwrap();
    queue
        .enqueue("older", "unrelated pending task", None, now)
        .unwrap();
    queue
        .enqueue_configured("codex", "synthetic task", None, context(), Some(codex), now)
        .unwrap();
    let mut agy_context = context();
    agy_context.conversation = Some("chat-a".into());
    queue
        .enqueue_configured("agy", "synthetic task", None, agy_context, Some(agy), now)
        .unwrap();
    let mut runtime = Runtime::open(&lab.0, "public", None).unwrap();
    let stop = AtomicBool::new(false);
    let mut config = CodexConfig::new(
        env!("CARGO_BIN_EXE_agent-host-fixture"),
        lab.0.join("codex-home"),
        &workspace,
    );
    config.args = vec!["app-server".into(), "agent_job".into()];
    config.timeout = Duration::from_secs(5);
    let first = agent_host::run_job_controlled(
        &mut runtime,
        &queue,
        config,
        Some("operator-model-does-not-replace-frozen-model"),
        &stop,
        None,
        "codex",
    )
    .unwrap();
    assert_eq!(first.status, "completed");
    assert_eq!(first.tool_calls, 1);
    assert_eq!(queue.get("older").unwrap().status, Status::Pending);
    let mut config = AntigravityConfig::new(
        env!("CARGO_BIN_EXE_agent-antigravity-fixture"),
        lab.0.join("agy-home"),
        &workspace,
    );
    config.args = vec!["filtered".into()];
    config.bridge_binary = env!("CARGO_BIN_EXE_agent-runtime").into();
    config.timeout = Duration::from_secs(5);
    let second =
        agent_antigravity::run_job_controlled(&mut runtime, &queue, config, &stop, None, "agy")
            .unwrap();
    assert_eq!(second.status, "completed");
    assert_eq!(second.engine, "antigravity");
    assert_eq!(queue.get("older").unwrap().status, Status::Pending);
    assert_eq!(runtime.agent_context(), &AgentContext::default());
}

#[cfg(feature = "agent-host-tests")]
#[test]
fn daemon_routes_mixed_profile_queue_through_both_native_engines() {
    use agent_runtime::{agent_host, mind_store::Store};
    use runtime_jobs::Status;
    use std::process::Command;
    let lab = Lab::new();
    let store = Store::open(&lab.0).unwrap();
    store
        .set_persona("shared", "MIND_PERSONA_SHARED_MARKER", 0)
        .unwrap();
    store
        .set_role("reader", "MIND_ROLE_READER_MARKER", 0)
        .unwrap();
    drop(store);
    let mut profiles = ProfileStore::open(&lab.0).unwrap();
    let codex = profiles
        .set("reader", input("codex", "fixture-codex"), Some(0))
        .unwrap();
    let agy = profiles
        .set("reader", input("antigravity", "fixture-agy"), Some(1))
        .unwrap();
    drop(profiles);
    let queue = Queue::open(&lab.0).unwrap();
    let now = agent_host::now().unwrap();
    queue
        .enqueue_configured(
            "first-codex",
            "synthetic task",
            None,
            context(),
            Some(codex),
            now,
        )
        .unwrap();
    let mut agy_context = context();
    agy_context.conversation = Some("chat-a".into());
    queue
        .enqueue_configured(
            "second-agy",
            "synthetic task",
            None,
            agy_context,
            Some(agy),
            now,
        )
        .unwrap();
    let output = Command::new(env!("CARGO_BIN_EXE_agent-runtime"))
        .args([
            "--state",
            lab.0.to_str().unwrap(),
            "agent",
            "run",
            "--cycles",
            "2",
            "--poll-ms",
            "250",
            "--codex-binary",
            env!("CARGO_BIN_EXE_agent-host-fixture"),
            "--antigravity-binary",
            env!("CARGO_BIN_EXE_agent-antigravity-fixture"),
        ])
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    for (id, engine) in [("first-codex", "codex"), ("second-agy", "antigravity")] {
        let job = queue.get(id).unwrap();
        assert_eq!(
            job.status,
            Status::Completed,
            "{}",
            job.error.unwrap_or_default()
        );
        assert_eq!(job.engine.as_deref(), Some(engine));
        if engine == "codex" {
            assert_eq!(
                job.text.as_deref(),
                Some("Read-only Runtime job fixture completed. Selected model: fixture-codex")
            );
        }
    }
}

#[test]
fn semantic_tools_choose_profile_accounts_without_requiring_provider_boilerplate() {
    let profile = runtime_jobs::ExecutionProfile {
        revision: 1,
        engine: "codex".into(),
        model: None,
        allowed_tools: None,
        default_accounts: BTreeMap::from([
            ("bilibili".into(), "bili-reader".into()),
            ("qq".into(), "qq-reader".into()),
            ("tieba".into(), "tieba-reader".into()),
        ]),
    };
    for (tool, mut input, expected) in [
        ("social_chats", json!({"kind":"friends"}), "qq-reader"),
        ("social_search", json!({"keyword":"Rust"}), "bili-reader"),
        (
            "social_open",
            json!({"reference":"https://t.bilibili.com/1234"}),
            "bili-reader",
        ),
        (
            "social_open",
            json!({"reference":"av170001"}),
            "bili-reader",
        ),
        (
            "social_send",
            json!({"latest_own":"tieba","message":"test"}),
            "tieba-reader",
        ),
    ] {
        apply_default_account(&profile, tool, &mut input);
        assert_eq!(input["account"], expected);
    }
}
