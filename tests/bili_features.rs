// SPDX-License-Identifier: MIT
//! Synthetic task contract tests. No credentials, HTTP or platform writes.
use agent_runtime::{
    app::Runtime,
    outbox::{Operation, Outbox, Status},
    private_state::PrivateLocators,
};
use runtime_core::{Platform, Scope};
use serde_json::json;
use std::{
    fs,
    path::PathBuf,
    sync::atomic::{AtomicU64, Ordering},
};
static N: AtomicU64 = AtomicU64::new(0);
struct Lab(PathBuf);
impl Lab {
    fn new() -> Self {
        Self(std::env::temp_dir().join(format!(
            "bili-features-{}-{}",
            std::process::id(),
            N.fetch_add(1, Ordering::Relaxed)
        )))
    }
}
impl Drop for Lab {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}
fn scope() -> Scope {
    Scope {
        platform: Platform::Bilibili,
        account: "synthetic-only".into(),
    }
}
#[test]
fn unknown_and_interrupted_feature_writes_are_never_replayed() {
    for interrupted in [false, true] {
        let lab = Lab::new();
        let locators = PrivateLocators::open(&lab.0).unwrap();
        let reference = locators
            .put(
                &scope(),
                "task-target",
                &json!({"kind":"video","video":"BV17x411w7KC"}),
            )
            .unwrap();
        let input = json!({"action":"coin","target_ref":reference,"count":1});
        let outbox = Outbox::open(&lab.0).unwrap();
        let mut r =
            Operation::new("once", scope(), "42".into(), json!({"aid":170001}), "coin").unwrap();
        r.request = Some(json!({"kind":"bilibili_feature","input":input}));
        outbox.insert(&r).unwrap();
        outbox.begin_attempt(&mut r).unwrap();
        if !interrupted {
            r.status = Status::Unknown;
            outbox.finish(&r).unwrap();
        }
        let mut rt = Runtime::open(&lab.0, "public", None).unwrap();
        let mut p = input.clone();
        p["operation_id"] = json!("once");
        let result = rt.call("bilibili.features", &p).unwrap();
        assert_eq!(result["status"], "unknown");
        assert_eq!(result["automatic_resubmission"], false);
        assert_eq!(result["reused"], true);
        assert_eq!(
            outbox.load("once", &scope()).unwrap().status,
            Status::Unknown
        );
        p["count"] = json!(2);
        assert!(
            rt.call("bilibili.features", &p)
                .unwrap_err()
                .contains("another Bilibili action")
        );
    }
}
#[test]
fn exact_account_targets_and_native_semantics_cannot_be_silently_rebound() {
    let lab = Lab::new();
    let locators = PrivateLocators::open(&lab.0).unwrap();
    let reference = locators
        .put(
            &scope(),
            "task-bilibili-feature",
            &json!({"kind":"dynamic","id":"123"}),
        )
        .unwrap();
    let mut rt = Runtime::open(&lab.0, "public", None).unwrap();
    assert!(rt.call("bilibili.features",&json!({"action":"dynamic_like","target_ref":reference,"operation_id":"a","account":"other"})).unwrap_err().contains("differs"));
    assert!(
        rt.call(
            "bilibili.features",
            &json!({"action":"coin","target_ref":reference,"operation_id":"a"})
        )
        .unwrap_err()
        .contains("exact target")
    );
    assert!(
        rt.call(
            "bilibili.features",
            &json!({"action":"dynamic_create","operation_id":"a","url":"https://example.invalid"})
        )
        .is_err()
    );
}
#[test]
fn dynamic_discussion_continuation_checks_action_and_scope_before_fetching() {
    let lab = Lab::new();
    let l = PrivateLocators::open(&lab.0).unwrap();
    let reference = l
        .put(
            &scope(),
            "task-bilibili-feature-page",
            &json!({"action":"dynamic_discussion","target":{"kind":"dynamic","id":"123"},"page":2}),
        )
        .unwrap();
    let mut rt = Runtime::open(&lab.0, "public", None).unwrap();
    assert!(
        rt.call(
            "bilibili.features",
            &json!({"action":"favorite_contents","continuation":reference})
        )
        .unwrap_err()
        .contains("another Bilibili action")
    );
    assert!(
        rt.call(
            "bilibili.features",
            &json!({"action":"dynamic_discussion","continuation":reference,"account":"other"})
        )
        .is_err()
    );
}
