// SPDX-License-Identifier: MIT
use agent_runtime::{app::Runtime, private_state::PrivateLocators};
use runtime_core::{Platform, Scope};
use serde_json::json;
#[test]
fn watch_tool_resumes_without_old_content_reference_and_preserves_account_and_cas() {
    let root = std::env::temp_dir().join(format!("watch-task-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&root);
    let target = PrivateLocators::open(&root)
        .unwrap()
        .put(
            &Scope {
                platform: Platform::Bilibili,
                account: "owner".into(),
            },
            "task-target",
            &json!({"kind":"video","video":"BV17x411w7KC"}),
        )
        .unwrap();
    let watch = {
        let mut rt = Runtime::open(&root, "public", None).unwrap();
        rt.call(
            "media.watch",
            &json!({"action":"start","target_ref":target,"read_now":false,"start_ms":1234}),
        )
        .unwrap()["session"]
            .clone()
    };
    let mut rt = Runtime::open(&root, "public", None).unwrap();
    let id = watch["watch_ref"].as_str().unwrap();
    let resumed = rt
        .call(
            "media.watch",
            &json!({"action":"next","watch_ref":id,"read_now":false}),
        )
        .unwrap();
    assert_eq!(resumed["session"]["next_start_ms"], 1234);
    assert!(
        rt.call(
            "media.watch",
            &json!({"action":"status","watch_ref":id,"account":"other"})
        )
        .is_err()
    );
    let sought=rt.call("media.watch",&json!({"action":"seek","watch_ref":id,"expected_revision":watch["revision"],"start_ms":5000,"read_now":false})).unwrap();
    assert_eq!(sought["session"]["next_start_ms"], 5000);
    assert_eq!(sought["session"]["full_video_watched"], false);
    assert!(rt.call("media.watch",&json!({"action":"seek","watch_ref":id,"expected_revision":watch["revision"],"start_ms":0,"read_now":false})).is_err());
    rt.call("media.watch", &json!({"action":"drop","watch_ref":id}))
        .unwrap();
    assert!(
        rt.call("media.watch", &json!({"action":"status","watch_ref":id}))
            .is_err()
    );
    drop(rt);
    std::fs::remove_dir_all(root).unwrap();
}

#[test]
fn returned_watch_reference_retains_media_preferences_across_restart_and_next() {
    let root = std::env::temp_dir().join(format!("watch-prefs-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&root);
    let target = PrivateLocators::open(&root)
        .unwrap()
        .put(
            &Scope {
                platform: Platform::Bilibili,
                account: "owner".into(),
            },
            "task-target",
            &json!({"kind":"video","video":"BV17x411w7KC"}),
        )
        .unwrap();
    let session = {
        let mut rt = Runtime::open(&root, "public", None).unwrap();
        rt.call("media.watch",&json!({"action":"start","target_ref":target,"read_now":false,"transcribe":false,"include_subtitles":false,"subtitle_language":"12345","speech_language":"zh-CN"})).unwrap()["session"].clone()
    };
    let mut rt = Runtime::open(&root, "public", None).unwrap();
    let next = rt
        .call(
            "media.watch",
            &json!({"action":"next","watch_ref":session["watch_ref"],"read_now":false}),
        )
        .unwrap();
    assert_eq!(next["session"]["preferences"]["transcribe"], false);
    assert_eq!(next["session"]["preferences"]["include_subtitles"], false);
    assert_eq!(next["session"]["preferences"]["subtitle_language"], "12345");
    assert_eq!(next["session"]["preferences"]["speech_language"], "zh");
    let changed=rt.call("media.watch",&json!({"action":"next","watch_ref":session["watch_ref"],"read_now":false,"transcribe":true,"language":"ai-zh"})).unwrap();
    assert_eq!(changed["session"]["preferences"]["transcribe"], true);
    assert_eq!(
        changed["session"]["preferences"]["subtitle_language"],
        "ai-zh"
    );
    assert_eq!(changed["session"]["preferences"]["speech_language"], "zh");
    let inherited = rt
        .call(
            "media.watch",
            &json!({"action":"status","watch_ref":session["watch_ref"]}),
        )
        .unwrap();
    assert_eq!(
        inherited["session"]["preferences"],
        changed["session"]["preferences"]
    );
    drop(rt);
    std::fs::remove_dir_all(root).unwrap();
}
