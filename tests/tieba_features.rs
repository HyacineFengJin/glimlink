// SPDX-License-Identifier: MIT
//! Network-free contract tests. Cached outcomes must not require a live session.
use agent_runtime::{
    app::Runtime,
    outbox::{Operation, Outbox, Status},
    private_state::PrivateLocators,
};
use runtime_core::{Platform, Scope};
use serde_json::{Value, json};
use std::{
    fs,
    path::PathBuf,
    sync::atomic::{AtomicU64, Ordering},
};
static NEXT: AtomicU64 = AtomicU64::new(0);
struct Lab(PathBuf);
impl Lab {
    fn new() -> Self {
        let path = std::env::temp_dir().join(format!(
            "tieba-features-{}-{}",
            std::process::id(),
            NEXT.fetch_add(1, Ordering::Relaxed)
        ));
        fs::create_dir(&path).unwrap();
        Self(path)
    }
}
impl Drop for Lab {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}
fn scope() -> Scope {
    Scope {
        platform: Platform::Tieba,
        account: "alice".into(),
    }
}
fn record(
    state: &std::path::Path,
    id: &str,
    params: &Value,
    status: Status,
    receipt: Option<Value>,
) {
    let mut input = params.clone();
    input.as_object_mut().unwrap().remove("operation_id");
    input.as_object_mut().unwrap().remove("account");
    let mut op =
        Operation::new(id, scope(), "77".into(), json!({"thread":123}), "message").unwrap();
    op.request = Some(json!({"action":params["action"],"input":input}));
    let store = Outbox::open(state).unwrap();
    store.insert(&op).unwrap();
    store.begin_attempt(&mut op).unwrap();
    op.status = status;
    op.receipt = receipt;
    store.finish(&op).unwrap();
}
#[test]
fn unknown_write_replays_without_session_or_reference_resolution() {
    let lab = Lab::new();
    let mut rt = Runtime::open(&lab.0, "public", None).unwrap();
    let params = json!({"action":"reply_floor","operation_id":"unknown-floor","reply_ref":"expired-reference","message":"message"});
    record(&lab.0, "unknown-floor", &params, Status::Unknown, None);
    let response = rt.call("social.tieba", &params).unwrap();
    assert_eq!(response["status"], "unknown");
    assert_eq!(response["reused_operation"], true);
    assert_eq!(response["automatic_resend"], false);
    assert_eq!(rt.scope(Platform::Tieba).account, "public");
    let mut changed = params.clone();
    changed["message"] = json!("different");
    assert!(rt.call("social.tieba", &changed).is_err());
    let mut changed = params;
    changed["account"] = json!("bob");
    assert!(rt.call("social.tieba", &changed).is_err());
}
#[test]
fn upload_receipt_returns_bound_reference_without_url_or_second_upload() {
    let lab = Lab::new();
    let mut rt = Runtime::open(&lab.0, "public", None).unwrap();
    let params = json!({"action":"upload_image","operation_id":"image-upload","forum":"Rust","image_ref":"00000000000000000000000000000000"});
    record(
        &lab.0,
        "image-upload",
        &params,
        Status::Sent,
        Some(
            json!({"forum_id":42,"pic_id":"abc123","width":64,"height":32,"url":"https://imgsrc.baidu.com/forum/pic/item/abc123.jpg"}),
        ),
    );
    let result = rt.call("social.tieba", &params).unwrap();
    let reference = result["uploaded_image_ref"].as_str().unwrap();
    let locators = PrivateLocators::open(&lab.0).unwrap();
    assert_eq!(
        locators
            .scope_for("task-tieba-uploaded", reference)
            .unwrap(),
        scope()
    );
    assert_eq!(result["width"], 64);
    assert_eq!(result["reused_operation"], true);
    let serialized = serde_json::to_string(&result).unwrap();
    assert!(!serialized.contains("imgsrc"));
    assert!(!serialized.contains("pic_id"));
}
#[test]
fn pending_search_results_paginate_without_fetching_or_retargeting() {
    let lab = Lab::new();
    let mut rt = Runtime::open(&lab.0, "public", None).unwrap();
    let locators = PrivateLocators::open(&lab.0).unwrap();
    let continuation=locators.put(&scope(),"task-tieba-discovery-page",&json!({"action":"search","params":{"query":"fixed"},"page":2,"seen":[],"pending":[{"title":"first"},{"title":"second"}],"has_more":false})).unwrap();
    let params = json!({"action":"search","continuation":continuation,"limit":1});
    let first = rt.call("social.tieba", &params).unwrap();
    assert_eq!(first["items"][0]["title"], "first");
    assert_eq!(first["has_more"], true);
    let second = rt
        .call(
            "social.tieba",
            &json!({"action":"search","continuation":first["continuation"],"limit":1}),
        )
        .unwrap();
    assert_eq!(second["items"][0]["title"], "second");
    assert_eq!(second["has_more"], false);
    assert!(
        rt.call(
            "social.tieba",
            &json!({"action":"forum","continuation":continuation})
        )
        .is_err()
    );
    assert!(
        rt.call(
            "social.tieba",
            &json!({"action":"search","continuation":continuation,"account":"bob"})
        )
        .is_err()
    );
    assert_eq!(rt.scope(Platform::Tieba).account, "public");
}
