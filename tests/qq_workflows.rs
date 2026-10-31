// SPDX-License-Identifier: MIT
//! Synthetic account fixtures only. None of these tests connects or sends.
use agent_runtime::{
    app::{Runtime, save_private_new},
    outbox::{Operation, Outbox, Status},
    private_state::{PrivateLocators, account_directory},
    qq_inbox::Inbox,
};
use runtime_core::{Platform, Scope};
use runtime_engine::{ToolCall, ToolContent};
use serde_json::{Value, json};
use std::{
    fs,
    path::PathBuf,
    sync::atomic::{AtomicU64, Ordering},
};
static NEXT: AtomicU64 = AtomicU64::new(0);
struct Lab {
    state: PathBuf,
    scope: Scope,
}
impl Lab {
    fn new() -> Self {
        let state = std::env::temp_dir().join(format!(
            "qq-workflow-{}-{}",
            std::process::id(),
            NEXT.fetch_add(1, Ordering::Relaxed)
        ));
        let scope = Scope {
            platform: Platform::Qq,
            account: "fixture-account".into(),
        };
        let dir = account_directory(&state, "qq", &scope.account).unwrap();
        let mut session = platform_qq::Session::empty();
        session.uin = 12345;
        session.uid = "u_own_fixture".into();
        session
            .set_credentials(&[1, 2], &[3, 4], &[0; 16], &[], None)
            .unwrap();
        session.save(&dir.join("session.json")).unwrap();
        platform_qq::session::write_private_json(&dir.join("device.json"),&json!({"guid":"00000000000000000000000000000001","device_name":"fixture","system_kernel":"Linux","kernel_version":"6.1.0"})).unwrap();
        Self { state, scope }
    }
    fn append(&self, key: &str, destination: Value, text: &str) {
        let conversation = target(destination);
        Inbox::open(&self.state,&self.scope).unwrap().append(key,&json!({"conversation":conversation,"text":text,"html":text,"direction":"incoming","from_uin":67890,"to_uin":12345,"server_time":1700000000,"unexpanded":false,"source_status":"fixture_only"})).unwrap();
    }
    fn runtime(&self) -> Runtime {
        Runtime::open(&self.state, "public", None).unwrap()
    }
}
impl Drop for Lab {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.state);
    }
}
fn target(destination: Value) -> Value {
    json!({"actor":12345,"key":destination.to_string(),"destination":destination})
}
fn call(rt: &mut Runtime, name: &str, args: Value) -> (bool, Value) {
    let result = rt.dispatch_engine_tool(&ToolCall {
        thread_id: "qq-fixture-thread".into(),
        turn_id: "qq-fixture-turn".into(),
        call_id: format!("fixture-{}", NEXT.fetch_add(1, Ordering::Relaxed)),
        tool: name.into(),
        arguments: args,
    });
    let text = result
        .content_items
        .iter()
        .find_map(|c| {
            if let ToolContent::InputText { text } = c {
                Some(text)
            } else {
                None
            }
        })
        .unwrap();
    (
        result.success,
        serde_json::from_str(text).unwrap_or_else(|_| json!({"error":text})),
    )
}
fn chats(rt: &mut Runtime) -> Value {
    let (ok, data) = call(rt, "social_chats", json!({"platform":"qq","refresh":false}));
    assert!(ok, "{data}");
    data
}

#[test]
fn saved_accounts_and_observed_chats_provide_direct_account_bound_reply_targets() {
    let lab = Lab::new();
    lab.append(
        "private-1",
        json!({"kind":"private","uid":"u_peer_fixture"}),
        "hello",
    );
    lab.append("group-1", json!({"kind":"group","code":777}), "group text");
    lab.append("unknown-1", Value::Null, "no UID");
    let mut rt = lab.runtime();
    let accounts = rt.call("runtime.accounts", &json!({})).unwrap();
    assert_eq!(accounts["qq"], json!([lab.scope.account]));
    let list = chats(&mut rt);
    assert_eq!(list["chats"].as_array().unwrap().len(), 4);
    let peer = list["chats"]
        .as_array()
        .unwrap()
        .iter()
        .find(|v| v["kind"] == "private")
        .unwrap();
    assert_eq!(peer["can_send_text"], true);
    assert!(!list.to_string().contains("u_peer_fixture"));
    let (ok, data) = call(
        &mut rt,
        "social_messages",
        json!({"target_ref":peer["target_ref"],"refresh":false}),
    );
    assert!(ok, "{data}");
    assert_eq!(data["messages"].as_array().unwrap().len(), 1);
    assert_eq!(data["messages"][0]["text"], "hello");
    assert_eq!(data["messages"][0]["target_ref"], peer["target_ref"]);
    let (ok, _) = call(
        &mut rt,
        "social_messages",
        json!({"target_ref":peer["target_ref"],"refresh":false,"account":"somebody-else"}),
    );
    assert!(!ok);
}

#[test]
fn incremental_message_cursor_survives_runtime_restart_without_duplicates() {
    let lab = Lab::new();
    let peer = json!({"kind":"private","uid":"u_peer_fixture"});
    lab.append("first", peer.clone(), "first");
    lab.append("second", peer.clone(), "second");
    let mut rt = lab.runtime();
    let (ok, first) = call(
        &mut rt,
        "social_messages",
        json!({"platform":"qq","refresh":false,"limit":1}),
    );
    assert!(ok, "{first}");
    assert_eq!(first["has_more"], true);
    let continuation = first["continuation"].clone();
    drop(rt);
    lab.append("third", peer, "third");
    let mut rt = lab.runtime();
    let (ok, next) = call(
        &mut rt,
        "social_messages",
        json!({"continuation":continuation,"refresh":false,"limit":20}),
    );
    assert!(ok, "{next}");
    let text = next["messages"]
        .as_array()
        .unwrap()
        .iter()
        .map(|v| v["text"].as_str().unwrap())
        .collect::<Vec<_>>();
    assert_eq!(text, vec!["second", "third"]);
    let (ok, empty) = call(
        &mut rt,
        "social_messages",
        json!({"continuation":next["continuation"],"refresh":false}),
    );
    assert!(ok);
    assert!(empty["messages"].as_array().unwrap().is_empty());
}

#[test]
fn latest_baseline_skips_the_complete_retained_history_then_reads_only_future_events() {
    let lab = Lab::new();
    let peer = json!({"kind":"private","uid":"u_peer_fixture"});
    for i in 0..25 {
        lab.append(&format!("old-{i}"), peer.clone(), "old history");
    }
    let mut rt = lab.runtime();
    let (ok, seed) = call(
        &mut rt,
        "social_messages",
        json!({"platform":"qq","start":"latest","refresh":false}),
    );
    assert!(ok, "{seed}");
    assert_eq!(seed["seeded_latest"], true);
    assert_eq!(seed["baseline_sequence"], 25);
    assert!(seed["messages"].as_array().unwrap().is_empty());
    let continuation = seed["continuation"].clone();
    drop(rt);
    lab.append("new-event", peer, "only new message");
    let mut rt = lab.runtime();
    let (ok, page) = call(
        &mut rt,
        "social_messages",
        json!({"continuation":continuation,"refresh":false}),
    );
    assert!(ok, "{page}");
    assert_eq!(page["messages"].as_array().unwrap().len(), 1);
    assert_eq!(page["messages"][0]["text"], "only new message");
    assert!(
        !call(
            &mut rt,
            "social_messages",
            json!({"continuation":page["continuation"],"start":"latest","refresh":false})
        )
        .0
    );
    assert!(
        !call(
            &mut rt,
            "social_messages",
            json!({"start":17,"refresh":false})
        )
        .0
    );
}

#[test]
fn conversation_pagination_exposes_the_retained_tail_without_reconnecting() {
    let lab = Lab::new();
    for i in 0..4 {
        lab.append(
            &format!("conversation-{i}"),
            json!({"kind":"group","code":700+i}),
            "fixture",
        );
    }
    let mut rt = lab.runtime();
    let (ok, first) = call(
        &mut rt,
        "social_chats",
        json!({"platform":"qq","refresh":false,"limit":1}),
    );
    assert!(ok, "{first}");
    assert_eq!(first["has_more"], true);
    let mut reference = first["continuation"].clone();
    let mut seen = vec![first["chats"][0]["target_ref"].as_str().unwrap().to_owned()];
    drop(rt);
    let mut rt = lab.runtime();
    while !reference.is_null() {
        let (ok, page) = call(
            &mut rt,
            "social_chats",
            json!({"continuation":reference,"refresh":false,"limit":2}),
        );
        assert!(ok, "{page}");
        for chat in page["chats"].as_array().unwrap() {
            let id = chat["target_ref"].as_str().unwrap().to_owned();
            assert!(!seen.contains(&id));
            seen.push(id);
        }
        reference = page["continuation"].clone();
    }
    assert_eq!(seen.len(), 5);
}

fn recorded_send(lab: &Lab, status: Status) -> (String, String) {
    let destination = json!({"kind":"private","uid":"u_peer_fixture"});
    let target = target(destination);
    let refs = PrivateLocators::open(&lab.state).unwrap();
    let reference = refs
        .put_reusable(&lab.scope, "task-qq-chat", &target)
        .unwrap();
    let outbox = Outbox::open(&lab.state).unwrap();
    let id = "fixture-delivery";
    let mut record =
        Operation::new(id, lab.scope.clone(), "12345".into(), target, "cached text").unwrap();
    record.request = Some(json!({"action":"qq_text","random":17,"session_sequence":42}));
    outbox.insert(&record).unwrap();
    outbox.begin_attempt(&mut record).unwrap();
    if status != Status::Attempting {
        record.status = status;
        record.receipt = Some(json!({"result":0,"private_sequence":5}));
        outbox.finish(&record).unwrap();
    }
    (reference, id.into())
}
#[test]
fn acknowledged_qq_send_replays_before_connecting_or_loading_any_signer() {
    let lab = Lab::new();
    let (reference, id) = recorded_send(&lab, Status::Sent);
    let session_path = account_directory(&lab.state, "qq", &lab.scope.account)
        .unwrap()
        .join("session.json");
    let before = fs::read(&session_path).unwrap();
    let mut rt = lab.runtime();
    let args = json!({"target_ref":reference,"operation_id":id,"message":"cached text"});
    let (ok, data) = call(&mut rt, "social_send", args.clone());
    assert!(ok, "{data}");
    assert_eq!(data["status"], "sent");
    assert_eq!(data["reused_operation"], true);
    assert_eq!(data["readback_verified"], false);
    let mut wrong = args;
    wrong["message"] = json!("changed body");
    assert!(!call(&mut rt, "social_send", wrong).0);
    assert_eq!(fs::read(session_path).unwrap(), before);
}
#[test]
fn interrupted_qq_attempt_becomes_unknown_without_resubmitting() {
    let lab = Lab::new();
    let (reference, id) = recorded_send(&lab, Status::Attempting);
    let mut rt = lab.runtime();
    let args = json!({"target_ref":reference,"operation_id":id,"message":"cached text"});
    let (ok, data) = call(&mut rt, "social_send", args.clone());
    assert!(ok, "{data}");
    assert_eq!(data["status"], "unknown");
    let (ok, again) = call(&mut rt, "social_send", args);
    assert!(ok);
    assert_eq!(again["status"], "unknown");
    assert_eq!(
        Outbox::open(&lab.state)
            .unwrap()
            .load(&id, &lab.scope)
            .unwrap()
            .status,
        Status::Unknown
    );
}

#[test]
fn durable_operation_query_works_after_its_target_reference_is_gone() {
    let lab = Lab::new();
    let (reference, id) = recorded_send(&lab, Status::Sent);
    PrivateLocators::open(&lab.state)
        .unwrap()
        .remove(&lab.scope, "task-qq-chat", &reference)
        .unwrap();
    let mut rt = lab.runtime();
    let (ok, data) = call(&mut rt, "runtime_operation", json!({"operation_id":id}));
    assert!(ok, "{data}");
    assert_eq!(data["operation"]["status"], "sent");
    assert_eq!(data["network_requests"], 0);
    assert_eq!(data["resubmitted"], false);
    assert!(data["operation"].get("request").is_none());
    assert!(data["operation"].get("target").is_none());
    let (ok, _) = call(
        &mut rt,
        "runtime_operation",
        json!({"operation_id":id,"account":"another-account"}),
    );
    assert!(!ok);
}
#[test]
fn legacy_consumed_operation_and_unresolved_destination_never_connect() {
    let lab = Lab::new();
    let mut rt = lab.runtime();
    let list = chats(&mut rt);
    let own = list["chats"][0]["target_ref"].clone();
    let dir = account_directory(&lab.state, "qq", &lab.scope.account).unwrap();
    save_private_new(
        &dir.join("legacy-consumed.json"),
        &json!({"schema":1,"operation_ids":["already-consumed"],"go_self_send_consumed":true}),
    )
    .unwrap();
    let (ok, data) = call(
        &mut rt,
        "social_send",
        json!({"target_ref":own,"operation_id":"already-consumed","message":"do not send"}),
    );
    assert!(!ok);
    assert!(data.to_string().contains("already consumed"));
    let unresolved = PrivateLocators::open(&lab.state)
        .unwrap()
        .put(&lab.scope, "task-qq-chat", &target(Value::Null))
        .unwrap();
    let (ok, data) = call(
        &mut rt,
        "social_send",
        json!({"target_ref":unresolved,"operation_id":"unresolved","message":"do not send"}),
    );
    assert!(!ok, "{data}");
    assert_eq!(data["status"], "failed");
    assert!(data.to_string().contains("verified UID/group code"));
    assert!(
        !Outbox::open(&lab.state)
            .unwrap()
            .exists("unresolved")
            .unwrap()
    );
}
