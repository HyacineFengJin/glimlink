// SPDX-License-Identifier: MIT
#![cfg(feature = "agent-host-tests")]
//! Account-free, real queue/Runtime/native-wire checks for autonomous continuity.
use agent_runtime::{
    agent_activity::{ActivityStore, Config},
    agent_antigravity, agent_host, agent_mind,
    agent_profiles::{ProfileInput, ProfileStore},
    app::Runtime,
    mind_store::Store,
};
use runtime_engine::{AntigravityConfig, CodexConfig};
use runtime_jobs::{AgentContext, Outcome, Queue, Status};
use serde_json::{Value, json};
use std::{
    fs,
    path::PathBuf,
    sync::atomic::{AtomicBool, AtomicU64, Ordering},
    time::Duration,
};
static NEXT: AtomicU64 = AtomicU64::new(0);
struct Lab {
    state: PathBuf,
    root: PathBuf,
}
impl Lab {
    fn new() -> Self {
        let root = std::env::temp_dir().join(format!(
            "activity-test-{}-{}",
            std::process::id(),
            NEXT.fetch_add(1, Ordering::Relaxed)
        ));
        fs::create_dir(&root).unwrap();
        fs::create_dir(root.join("workspace")).unwrap();
        let l = Self {
            state: root.join("state"),
            root,
        };
        let mind = Store::open(&l.state).unwrap();
        mind.set_persona("shared", "MIND_PERSONA_SHARED_MARKER", 0)
            .unwrap();
        mind.set_role("reader", "MIND_ROLE_READER_MARKER", 0)
            .unwrap();
        l.profile("codex");
        l
    }
    fn profile(&self, engine: &str) {
        ProfileStore::open(&self.state)
            .unwrap()
            .set(
                "reader",
                ProfileInput {
                    engine: engine.into(),
                    model: None,
                    allowed_tools: Some(vec![
                        "runtime_memory".into(),
                        "runtime_modules".into(),
                        "social_send".into(),
                    ]),
                    default_accounts: Default::default(),
                },
                None,
            )
            .unwrap();
    }
    fn activity(&self, now: u64, max: u32) -> ActivityStore {
        let s = ActivityStore::open(&self.state).unwrap();
        s.configure(
            Config {
                id: "browsing".into(),
                context: AgentContext {
                    persona: "shared".into(),
                    agent: "reader".into(),
                    conversation: Some("chat-a".into()),
                },
                enabled: true,
                interval_seconds: 60,
                max_turns_per_day: max,
                read_only: true,
                interaction_limits: None,
            },
            now,
        )
        .unwrap();
        s
    }
}
impl Drop for Lab {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.root);
    }
}

#[test]
fn interaction_allowance_survives_engine_switch_and_retries_without_network() {
    use agent_runtime::{agent_activity::InteractionLimits, private_state::PrivateLocators};
    use runtime_core::{Platform, Scope};
    let l = Lab::new();
    let now = agent_host::now().unwrap();
    let a = l.activity(now, 8);
    let mut config: Config =
        serde_json::from_value(a.list().unwrap()["activities"][0]["config"].clone()).unwrap();
    config.read_only = false;
    config.interaction_limits = Some(InteractionLimits {
        likes: 1,
        comments: 1,
    });
    a.configure(config.clone(), now).unwrap();
    let refs = PrivateLocators::open(&l.state).unwrap();
    let scope = Scope {
        platform: Platform::Bilibili,
        account: "synthetic-bili".into(),
    };
    let target = refs
        .put(
            &scope,
            "task-target",
            &json!({"kind":"video","video":"BV17x411w7KC"}),
        )
        .unwrap();
    let foreign = refs
        .put(
            &Scope {
                platform: Platform::Tieba,
                account: "synthetic-bili".into(),
            },
            "task-target",
            &json!({"kind":"thread","thread":"123"}),
        )
        .unwrap();
    let q = Queue::open(&l.state).unwrap();
    let first = a.schedule(&l.state, &q, now).unwrap().unwrap();
    let claim = q.claim_job(&first, now).unwrap().unwrap();
    let args = json!({"target_ref":target,"operation_id":"one-like"});
    assert!(
        a.reserve_interaction(&l.state, &claim.job, "social_follow", &args)
            .is_err()
    );
    assert!(
        a.reserve_interaction(
            &l.state,
            &claim.job,
            "social_like",
            &json!({"target_ref":foreign,"operation_id":"foreign"})
        )
        .is_err()
    );
    assert!(
        a.reserve_interaction(
            &l.state,
            &claim.job,
            "social_like",
            &json!({"target_ref":target,"operation_id":"mismatch","account":"other"})
        )
        .is_err()
    );
    a.reserve_interaction(&l.state, &claim.job, "social_like", &args)
        .unwrap();
    let restarted = ActivityStore::open(&l.state).unwrap();
    restarted
        .reserve_interaction(&l.state, &claim.job, "social_like", &args)
        .unwrap();
    assert_eq!(
        restarted.inline(&claim.job.context, "browsing").unwrap()["interaction_allowance"]["reserved_likes"],
        1
    );
    let done = q
        .finish(
            &claim.lease,
            Outcome::completed("first native engine"),
            now + 1,
        )
        .unwrap();
    restarted.finish(&done, now + 1).unwrap();
    l.profile("antigravity");
    restarted.wake("browsing", now + 2).unwrap();
    let second = restarted.schedule(&l.state, &q, now + 2).unwrap().unwrap();
    let second = q.claim_job(&second, now + 2).unwrap().unwrap();
    assert_eq!(
        second.job.execution_profile.as_ref().unwrap().engine,
        "antigravity"
    );
    assert!(
        restarted
            .reserve_interaction(
                &l.state,
                &second.job,
                "social_like",
                &json!({"target_ref":target,"operation_id":"another-like"})
            )
            .is_err()
    );
    assert!(
        restarted
            .reserve_interaction(
                &l.state,
                &second.job,
                "social_send",
                &json!({"target_ref":target,"operation_id":"one-like","message":"改成评论"})
            )
            .is_err()
    );
    assert!(restarted.reserve_interaction(&l.state,&second.job,"social_send",&json!({"target_ref":target,"operation_id":"image","message":"图片","image_ref":"bad"})).is_err());
    restarted
        .reserve_interaction(
            &l.state,
            &second.job,
            "social_send",
            &json!({"target_ref":target,"operation_id":"one-comment","message":"这个转场接得好顺"}),
        )
        .unwrap();
    assert!(
        restarted
            .reserve_interaction(
                &l.state,
                &second.job,
                "social_send",
                &json!({"target_ref":target,"operation_id":"extra-comment","message":"第二条"})
            )
            .is_err()
    );
    assert!(
        !l.state.join("outbox").exists(),
        "allowance reservation never performs HTTP or fabricates receipts"
    );
    let done = q
        .finish(&second.lease, Outcome::completed("second engine"), now + 3)
        .unwrap();
    restarted.finish(&done, now + 3).unwrap();
    assert!(
        restarted
            .reserve_interaction(&l.state, &done, "social_like", &args)
            .is_err(),
        "closed wakes cannot dispatch even an old identity"
    );
    restarted.wake("browsing", now + 86401).unwrap();
    let third = restarted
        .schedule(&l.state, &q, now + 86401)
        .unwrap()
        .unwrap();
    let third = q.get(&third).unwrap();
    assert_eq!(
        restarted.inline(&third.context, "browsing").unwrap()["interaction_allowance"]["likes_remaining"],
        1
    );
    // A private reference from yesterday can expire: quota reset does not renew it.
    assert_eq!(
        restarted.list().unwrap()["activities"][0]["interaction_allowance"]["reserved_comments"],
        0
    );
}

#[test]
fn codex_checkpoint_and_linked_episode_reach_antigravity_after_queue_gc() {
    let l = Lab::new();
    let now = agent_host::now().unwrap();
    let a = l.activity(now, 4);
    let q = Queue::open(&l.state).unwrap();
    let first = a.schedule(&l.state, &q, now).unwrap().unwrap();
    assert_eq!(a.schedule(&l.state, &q, now).unwrap(), Some(first.clone()));
    let j = q.get(&first).unwrap();
    assert!(j.prompt.is_empty());
    assert!(j.event.is_none());
    assert!(j.activity.is_some());
    assert!(
        !j.execution_profile
            .as_ref()
            .unwrap()
            .allowed_tools
            .as_ref()
            .unwrap()
            .contains(&"social_send".into())
    );
    let mut rt = Runtime::open(&l.state, "public", None).unwrap();
    let mut codex = CodexConfig::new(
        env!("CARGO_BIN_EXE_agent-host-fixture"),
        l.root.join("codex-home"),
        l.root.join("workspace"),
    );
    codex.args = vec!["app-server".into(), "agent_job_activity".into()];
    codex.timeout = Duration::from_secs(5);
    let result = agent_host::run_job_controlled(
        &mut rt,
        &q,
        codex,
        None,
        &AtomicBool::new(false),
        None,
        &first,
    )
    .unwrap();
    assert_eq!(result.status, "completed");
    assert_eq!(result.history_persisted, Some(true));
    assert!(result.operation_ids.is_empty());
    assert_eq!(a.list().unwrap()["activities"][0]["checkpoint_revision"], 1);
    q.gc_finished(&[first.clone()]).unwrap();
    assert!(q.get(&first).is_err());
    l.profile("antigravity");
    a.wake("browsing", now).unwrap();
    let second = a.schedule(&l.state, &q, now).unwrap().unwrap();
    assert_ne!(first, second);
    let mut agy = AntigravityConfig::new(
        env!("CARGO_BIN_EXE_agent-antigravity-fixture"),
        l.root.join("agy-home"),
        l.root.join("workspace"),
    );
    agy.args = vec!["activity".into()];
    agy.bridge_binary = env!("CARGO_BIN_EXE_agent-runtime").into();
    agy.timeout = Duration::from_secs(5);
    let result = agent_antigravity::run_job_controlled(
        &mut rt,
        &q,
        agy,
        &AtomicBool::new(false),
        None,
        &second,
    )
    .unwrap();
    assert_eq!(result.status, "completed", "{:?}", result.context_error);
    assert_eq!(result.history_persisted, Some(true));
    let ctx = q.get(&second).unwrap().context;
    let inline = a.inline(&ctx, "browsing").unwrap();
    assert_eq!(inline["checkpoint"]["summary"], "AGY_CONTINUED_OBSERVATION");
    assert_eq!(inline["recent_episodes"][0]["previous_episode"], first);
    let detail = a.read(&ctx, "browsing", &first, 0, 4096).unwrap();
    assert_eq!(
        detail["checkpoint_snapshot"]["summary"],
        "CODEX_ACTIVITY_OBSERVATION"
    );
    assert!(
        detail["text"]
            .as_str()
            .unwrap()
            .contains("fixture completed")
    );
    assert!(result.operation_ids.is_empty());
    assert!(!l.state.join("outbox").exists());
    let prepared = agent_mind::assemble_job(&l.state, &l.state, &q.get(&second).unwrap()).unwrap();
    assert!(
        !prepared
            .developer_instructions
            .contains("AGY_CONTINUED_OBSERVATION")
    );
    let inputs = serde_json::to_string(&prepared.inputs).unwrap();
    assert!(inputs.contains("AGY_CONTINUED_OBSERVATION"));
    assert!(!inputs.contains("Current operator task"));
}

#[test]
fn budgets_rest_restart_and_failed_turns_do_not_create_busy_loops_or_replay() {
    let l = Lab::new();
    let a = l.activity(100, 1);
    let q = Queue::open(&l.state).unwrap();
    let first = a.schedule(&l.state, &q, 100).unwrap().unwrap();
    let claim = q.claim_job(&first, 100).unwrap().unwrap();
    let j = q
        .finish(&claim.lease, Outcome::completed("rest"), 101)
        .unwrap();
    a.finish(&j, 101).unwrap();
    let before = fs::read(l.state.join("agent-activities/state.json")).unwrap();
    assert!(a.schedule(&l.state, &q, 130).unwrap().is_none());
    assert_eq!(
        fs::read(l.state.join("agent-activities/state.json")).unwrap(),
        before,
        "sleep does not rewrite the store"
    );
    a.wake("browsing", 150).unwrap();
    assert!(
        a.schedule(&l.state, &q, 150).unwrap().is_none(),
        "explicit wakes respect the daily budget"
    );
    let second = a.schedule(&l.state, &q, 86501).unwrap().unwrap();
    q.claim_job(&second, 86501).unwrap().unwrap();
    q.recover_interrupted(86502).unwrap();
    let restarted = ActivityStore::open(&l.state).unwrap();
    assert!(restarted.schedule(&l.state, &q, 90000).unwrap().is_none());
    assert_eq!(q.get(&second).unwrap().status, Status::Interrupted);
    assert_eq!(restarted.list().unwrap()["activities"][0]["halted"], true);
    assert_eq!(q.list(128).unwrap().len(), 2);
}

#[test]
fn spill_is_bounded_linked_utf8_paged_and_available_after_job_gc() {
    let l = Lab::new();
    let a = l.activity(10, 20);
    let q = Queue::open(&l.state).unwrap();
    let mut oldest = String::new();
    let mut latest = String::new();
    let mut context = None;
    let content = format!("{}TAIL_MARKER", "画面与观点。".repeat(1000));
    for n in 0..10 {
        let now = 10 + n * 100;
        a.wake("browsing", now).unwrap();
        let key = a.schedule(&l.state, &q, now).unwrap().unwrap();
        if n == 0 {
            oldest = key.clone();
        }
        latest = key.clone();
        let claim = q.claim_job(&key, now).unwrap().unwrap();
        context = Some(claim.job.context.clone());
        let j = q
            .finish(&claim.lease, Outcome::completed(content.clone()), now + 1)
            .unwrap();
        a.finish(&j, now + 1).unwrap();
        q.gc_finished(&[key]).unwrap();
    }
    let ctx = context.unwrap();
    let inline = a.inline(&ctx, "browsing").unwrap();
    assert_eq!(inline["retained_episodes"], 8);
    assert_eq!(inline["evicted_episodes"], 2);
    assert!(serde_json::to_vec(&inline).unwrap().len() < 3000);
    assert!(!inline.to_string().contains("TAIL_MARKER"));
    assert!(a.read(&ctx, "browsing", &oldest, 0, 4096).is_err());
    let mut offset = 0;
    let mut assembled = String::new();
    loop {
        let v = a.read(&ctx, "browsing", &latest, offset, 101).unwrap();
        assembled.push_str(v["text"].as_str().unwrap());
        assert_eq!(v["archive_truncated"], true);
        match v["next_offset"].as_u64() {
            Some(n) => offset = n as usize,
            None => break,
        }
    }
    assert!(content.starts_with(&assembled));
    assert!(assembled.len() <= 8192);
    assert!(assembled.len() > 8000);
    assert!(a.read(&ctx, "browsing", &latest, 1, 100).is_err());
    let mut other = ctx.clone();
    other.persona = "other".into();
    assert!(a.inline(&other, "browsing").is_err());
}

#[test]
fn checkpoint_updates_require_current_wake_revision_and_scope() {
    let l = Lab::new();
    let a = l.activity(10, 4);
    let q = Queue::open(&l.state).unwrap();
    let key = a.schedule(&l.state, &q, 10).unwrap().unwrap();
    let claim = q.claim_job(&key, 10).unwrap().unwrap();
    let ctx = &claim.job.context;
    let call = |params: Value| agent_mind::call(&l.state, ctx, Some(&key), &params);
    assert!(call(json!({"action":"activity_save","expected_revision":0,"checkpoint":{"summary":"private note","interests":["animation"],"questions":["uncertain claim"],"rest_seconds":120}})).is_ok());
    assert!(call(json!({"action":"activity_save","expected_revision":0,"checkpoint":{}})).is_err());
    assert!(call(json!({"action":"activity_get","persona":"forged"})).is_err());
    assert!(call(json!({"action":"activity_save","expected_revision":1,"checkpoint":{"summary":"x".repeat(7000)}})).is_err());
    let job = q
        .finish(&claim.lease, Outcome::completed("private note"), 11)
        .unwrap();
    a.finish(&job, 11).unwrap();
    assert!(call(json!({"action":"activity_save","expected_revision":1,"checkpoint":{}})).is_err());
    assert!(a.schedule(&l.state, &q, 100).unwrap().is_none());
    assert_eq!(a.list().unwrap()["activities"][0]["next_wake_at"], 131);
    assert!(q.enqueue("empty-task", "", None, 20).is_err());
}

#[test]
fn reservation_pins_its_receipt_until_episode_is_saved() {
    let l = Lab::new();
    let a = l.activity(10, 4);
    let q = Queue::open(&l.state).unwrap();
    let key = a.schedule(&l.state, &q, 10).unwrap().unwrap();
    let claim = q.claim_job(&key, 10).unwrap().unwrap();
    q.finish(
        &claim.lease,
        Outcome::completed("stored but not archived yet"),
        11,
    )
    .unwrap();
    let args = vec!["gc".into(), key.clone()];
    assert!(agent_runtime::agent_cli::command(&l.state, "public", &args).is_err());
    a.schedule(&l.state, &q, 12).unwrap();
    assert!(a.pending_job_ids().unwrap().is_empty());
    assert!(agent_runtime::agent_cli::command(&l.state, "public", &args).is_ok());
}

#[test]
fn daemon_wakes_without_a_task_and_prioritizes_an_explicit_request() {
    let l = Lab::new();
    let now = agent_host::now().unwrap();
    let a = l.activity(now, 4);
    let q = Queue::open(&l.state).unwrap();
    q.enqueue("explicit-first", "Synthetic explicit request", None, now)
        .unwrap();
    let run = || {
        let output = std::process::Command::new(env!("CARGO_BIN_EXE_agent-runtime"))
            .args([
                "--state",
                l.state.to_str().unwrap(),
                "agent",
                "run",
                "--once",
                "--codex-binary",
                env!("CARGO_BIN_EXE_agent-host-fixture"),
            ])
            .output()
            .unwrap();
        assert!(
            output.status.success(),
            "{}",
            String::from_utf8_lossy(&output.stderr)
        );
    };
    run();
    assert_eq!(q.get("explicit-first").unwrap().status, Status::Completed);
    assert_eq!(a.list().unwrap()["activities"][0]["cycle"], 0);
    run();
    let jobs = q.list(128).unwrap();
    let autonomous = jobs.iter().find(|j| j.activity.is_some()).unwrap();
    assert_eq!(autonomous.status, Status::Completed);
    assert!(autonomous.prompt.is_empty());
    assert_eq!(a.list().unwrap()["activities"][0]["retained_episodes"], 1);
    assert!(jobs.iter().all(|j| j.progress.operation_ids.is_empty()));
}
