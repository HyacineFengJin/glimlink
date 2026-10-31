// SPDX-License-Identifier: MIT
use agent_runtime::app::Runtime;
use serde_json::json;
use std::{
    fs,
    io::Write,
    path::PathBuf,
    process::{Command, Stdio},
    sync::atomic::{AtomicU64, Ordering},
};
static COUNT: AtomicU64 = AtomicU64::new(0);
struct Lab(PathBuf);
impl Lab {
    fn new() -> Self {
        let path = std::env::temp_dir().join(format!(
            "runtime-flow-{}-{}",
            std::process::id(),
            COUNT.fetch_add(1, Ordering::Relaxed)
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

#[test]
fn normalize_expand_restart_and_account_isolation() {
    let lab = Lab::new();
    let state = lab.0.join("state");
    let input = lab.0.join("document.html");
    fs::write(
        &input,
        "<p>正文</p><script>const state={video:42}</script><style>p{color:red}</style>",
    )
    .unwrap();
    let mut runtime = Runtime::open(&state, "alice", None).unwrap();
    assert!(Runtime::open(&state, "alice", None).is_err());
    let source = runtime.import_file(&input, "text/html").unwrap();
    let normalized = runtime
        .call("content.html", &json!({"resource":source.id}))
        .unwrap();
    let blocks = normalized
        .pointer("/document/blocks")
        .unwrap()
        .as_array()
        .unwrap();
    let script = blocks
        .iter()
        .find(|v| v["label"] == "inline script")
        .unwrap()["resource"]["id"]
        .as_str()
        .unwrap()
        .to_owned();
    let html = blocks.iter().find_map(|v| v["html"].as_str()).unwrap();
    assert!(html.contains("正文"));
    assert!(!html.contains("const state"));
    assert_eq!(
        runtime
            .call("resource.read", &json!({"resource":script}))
            .unwrap()["text"],
        "const state={video:42}"
    );
    drop(runtime);
    let mut other = Runtime::open(&state, "bob", None).unwrap();
    assert!(
        other
            .call("resource.read", &json!({"resource":source.id}))
            .is_err()
    );
    drop(other);
    let mut restored = Runtime::open(&state, "alice", None).unwrap();
    assert_eq!(
        restored
            .call("resource.read", &json!({"resource":script}))
            .unwrap()["text"],
        "const state={video:42}"
    );
    assert!(
        restored
            .call("qq.send", &json!({"text":"must not send"}))
            .is_err()
    );
}

#[test]
fn startup_reclaims_abandoned_scratch_and_orphan_bytes() {
    let lab = Lab::new();
    let state = lab.0.join("state");
    fs::create_dir_all(state.join("work/crashed")).unwrap();
    fs::write(state.join("work/crashed/partial.wav"), b"partial").unwrap();
    fs::create_dir_all(state.join("resources")).unwrap();
    let orphan = state.join("resources/0123456789abcdef0123456789abcdef.blob");
    fs::write(&orphan, b"interrupted download").unwrap();
    let runtime = Runtime::open(&state, "public", None).unwrap();
    assert!(!orphan.exists());
    assert_eq!(fs::read_dir(state.join("work")).unwrap().count(), 0);
    drop(runtime);
}

#[test]
fn fifo_input_is_rejected_and_jsonl_recovers_from_a_bad_request() {
    let lab = Lab::new();
    let pipe = lab.0.join("named-pipe");
    assert!(
        Command::new("mkfifo")
            .arg(&pipe)
            .status()
            .unwrap()
            .success()
    );
    let runtime = Runtime::open(&lab.0.join("fifo-state"), "public", None).unwrap();
    assert!(runtime.import_file(&pipe, "text/plain").is_err());
    drop(runtime);
    let mut child = Command::new(env!("CARGO_BIN_EXE_agent-runtime"))
        .arg("--state")
        .arg(lab.0.join("server-state"))
        .arg("serve")
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .spawn()
        .unwrap();
    let mut input = child.stdin.take().unwrap();
    input.write_all(b"not json\n{\"id\":1,\"method\":\"unknown\",\"params\":{}}\n{\"id\":2,\"method\":\"runtime.modules\",\"params\":{}}\n").unwrap();
    drop(input);
    let output = child.wait_with_output().unwrap();
    assert!(output.status.success());
    let lines: Vec<serde_json::Value> = String::from_utf8(output.stdout)
        .unwrap()
        .lines()
        .map(|v| serde_json::from_str(v).unwrap())
        .collect();
    assert_eq!(lines.len(), 3);
    assert!(lines[0].get("error").is_some());
    assert!(lines[1].get("error").is_some());
    assert!(lines[2].pointer("/result/modules").unwrap().is_array());
}

#[test]
fn binary_sources_are_explicit_and_large_tool_results_remain_readable() {
    use runtime_engine::ToolContent;
    let lab = Lab::new();
    let mut runtime = Runtime::open(&lab.0.join("bounded"), "public", None).unwrap();
    let file = lab.0.join("source.pb");
    fs::write(&file, [0x0a, 0x03, 0xff, 0x00, 0x01]).unwrap();
    let resource = runtime
        .import_file(&file, "application/x-protobuf")
        .unwrap();
    assert!(
        runtime
            .call("resource.read", &json!({"resource":resource.id}))
            .is_err()
    );
    let result = runtime
        .call(
            "resource.read",
            &json!({"resource":resource.id,"encoding":"base64"}),
        )
        .unwrap();
    assert_eq!(result["data"], "CgP/AAE=");
    let file = lab.0.join("large.txt");
    fs::write(&file, "x".repeat(100000)).unwrap();
    let resource = runtime.import_file(&file, "text/plain").unwrap();
    let tool = runtime.call_for_engine(
        "resource.read",
        &json!({"resource":resource.id,"limit":100000}),
    );
    assert!(tool.success);
    let ToolContent::InputText { text } = &tool.content_items[0] else {
        panic!("expected bounded text")
    };
    assert!(text.len() < 2048);
    let saved: serde_json::Value = serde_json::from_str(text).unwrap();
    let id = saved["content_resource"]["id"].as_str().unwrap();
    let expanded = runtime
        .call(
            "content.json",
            &json!({"resource":id,"pointer":"/next_offset"}),
        )
        .unwrap();
    assert_eq!(expanded["document"]["blocks"][1]["value"]["value"], 100000);
}

#[test]
fn social_login_controls_stay_out_of_model_tools_and_need_an_account_alias() {
    let lab = Lab::new();
    let state = lab.0.join("no-login-state");
    let mut runtime = Runtime::open(&state, "public", None).unwrap();
    for method in [
        "bilibili.login",
        "bilibili.login-poll",
        "tieba.login",
        "tieba.login-poll",
    ] {
        assert!(runtime.call(method, &json!({})).is_err());
    }
    assert!(
        runtime
            .bilibili_login()
            .unwrap_err()
            .contains("explicit --account")
    );
    assert!(
        runtime
            .tieba_login()
            .unwrap_err()
            .contains("explicit --account")
    );
    for method in ["bilibili.status", "tieba.status"] {
        assert_eq!(
            runtime.call(method, &json!({})).unwrap()["status"],
            "signed_out"
        );
    }
    assert!(!state.join("accounts").exists());
    assert!(
        !agent_runtime::tools::specs()
            .iter()
            .any(|tool| tool.name.contains("login"))
    );
}

#[test]
fn notification_reads_require_an_account_and_do_not_route_read_receipt_writes() {
    let lab = Lab::new();
    let state = lab.0.join("notification-gates");
    let mut runtime = Runtime::open(&state, "public", None).unwrap();
    for method in [
        "bilibili.unread",
        "bilibili.notifications",
        "tieba.notifications",
    ] {
        assert!(
            runtime
                .call(method, &json!({"kind":"reply"}))
                .unwrap_err()
                .contains("account")
        );
    }
    for method in [
        "bilibili.mark_read",
        "bilibili.clear_unread",
        "tieba.mark_read",
        "tieba.clear_unread",
    ] {
        assert!(runtime.call(method, &json!({})).is_err());
    }
    assert!(!state.join("accounts").exists());
    drop(runtime);
    let mut runtime = Runtime::open(&state, "missing-session", None).unwrap();
    for params in [
        json!({"kind":"unknown"}),
        json!({"kind":"reply","limit":21}),
        json!({"kind":"reply","cursor_id":1}),
    ] {
        assert!(runtime.call("bilibili.notifications", &params).is_err());
    }
    assert!(
        runtime
            .call("tieba.notifications", &json!({"kind":"reply","page":0}))
            .is_err()
    );
    assert!(!state.join("accounts").exists());
}

#[test]
fn own_publication_reads_require_an_account_and_validate_before_network() {
    let lab = Lab::new();
    let state = lab.0.join("own-publication-gates");
    let mut runtime = Runtime::open(&state, "public", None).unwrap();
    for method in ["bilibili.publications", "tieba.publications"] {
        assert!(
            runtime
                .call(method, &json!({}))
                .unwrap_err()
                .contains("account")
        );
    }
    drop(runtime);
    let mut runtime = Runtime::open(&state, "missing-session", None).unwrap();
    for method in ["bilibili.publications", "tieba.publications"] {
        assert!(runtime.call(method, &json!({"page":0})).is_err());
        assert!(runtime.call(method, &json!({"limit":21})).is_err());
    }
    assert!(
        runtime
            .call("tieba.publications", &json!({"kind":123}))
            .is_err()
    );
    assert!(!state.join("accounts").exists());
}

#[test]
fn gif_samples_reach_engine_as_real_images_with_original_timing() {
    use image::{Delay, Frame, Rgba, RgbaImage, codecs::gif::GifEncoder};
    use runtime_engine::ToolContent;
    let lab = Lab::new();
    let input = lab.0.join("animation.gif");
    {
        let file = std::fs::File::create(&input).unwrap();
        let mut encoder = GifEncoder::new(file);
        for colour in [[255, 0, 0, 255], [0, 255, 0, 255], [0, 0, 255, 255]] {
            encoder
                .encode_frame(Frame::from_parts(
                    RgbaImage::from_pixel(8, 8, Rgba(colour)),
                    0,
                    0,
                    Delay::from_numer_denom_ms(100, 1),
                ))
                .unwrap();
        }
    }
    let mut runtime = Runtime::open(&lab.0.join("gif-state"), "gif-account", None).unwrap();
    let source = runtime.import_file(&input, "image/gif").unwrap();
    let result = runtime.call_for_engine(
        "media.gif",
        &json!({"resource":source.id,"duration_ms":300,"interval_ms":100,"max_edge":16}),
    );
    assert!(result.success);
    assert_eq!(
        result
            .content_items
            .iter()
            .filter(|item| matches!(item, ToolContent::InputImage { .. }))
            .count(),
        3
    );
    let text = result
        .content_items
        .iter()
        .find_map(|item| match item {
            ToolContent::InputText { text } => Some(text),
            _ => None,
        })
        .unwrap();
    let metadata: serde_json::Value = serde_json::from_str(text).unwrap();
    assert_eq!(metadata["window"]["probe"]["frame_count"], 3);
    assert_eq!(metadata["window"]["frames"][0]["width"], 8);
    assert_eq!(metadata["window"]["frames"][0]["actual_pts_ms"], 0);
    assert_eq!(metadata["window"]["frames"][2]["actual_pts_ms"], 200);
    assert!(!metadata.to_string().contains("/tmp/"));
}

#[test]
fn single_call_send_reuses_a_verified_operation_and_restores_active_account() {
    use agent_runtime::{
        outbox::{Operation, Outbox, Status},
        private_state::account_directory,
    };
    use runtime_core::{Platform, Scope};
    use std::{fs::OpenOptions, os::unix::fs::OpenOptionsExt};
    let lab = Lab::new();
    let state = lab.0.join("single-call-send");
    let account = account_directory(&state, "bilibili", "test-bili").unwrap();
    let file = OpenOptions::new()
        .write(true)
        .create_new(true)
        .mode(0o600)
        .open(account.join("session.json"))
        .unwrap();
    serde_json::to_writer(file,&json!({"schema":1,"mid":42,"cookies":{"DedeUserID":"42","DedeUserID__ckMd5":"synthetic-hash","SESSDATA":"synthetic-private-session","bili_jct":"0123456789abcdef0123456789abcdef"},"refresh_token":"","authenticated_at":0})).unwrap();
    let outbox = Outbox::open(&state).unwrap();
    let mut record = Operation::new(
        "verified-once",
        Scope {
            platform: Platform::Bilibili,
            account: "test-bili".into(),
        },
        "42".into(),
        json!({"aid":7,"bvid":"BV17x411w7KC","title":"owned","owner_mid":42}),
        "晚安喵（test）",
    )
    .unwrap();
    outbox.insert(&record).unwrap();
    outbox.begin_attempt(&mut record).unwrap();
    record.status = Status::Sent;
    record.receipt = Some(json!({"rpid":8,"oid":7,"bvid":"BV17x411w7KC"}));
    outbox.finish(&record).unwrap();
    record.status = Status::Verified;
    record.verification = Some(json!({"rpid":8,"author_mid":42,"message":"晚安喵（test）"}));
    outbox.finish(&record).unwrap();
    let mut runtime = Runtime::open(&state, "public", None).unwrap();
    let params = json!({"operation_id":"verified-once","message":"晚安喵（test）"});
    for _ in 0..2 {
        let result = runtime.call("bilibili.comment_latest", &params).unwrap();
        assert_eq!(result["status"], "verified");
        assert_eq!(result["scope"]["account"], "test-bili");
        assert!(!result.to_string().contains("synthetic-private"));
        assert_eq!(runtime.scope(Platform::Bilibili).account, "public");
    }
    let task=runtime.call("social.send",&json!({"latest_own":"bilibili","operation_id":"verified-once","message":"晚安喵（test）"})).unwrap();
    assert_eq!(task["status"], "verified");
    assert!(task["target_ref"].as_str().is_some());
    assert_eq!(
        task["target_resolution"],
        "own_latest_frozen_on_first_preparation"
    );
    // Both a lost response and a process crash must return a durable unknown
    // outcome without contacting the platform or trying the write again.
    for interrupted in [false, true] {
        let id = if interrupted {
            "interrupted-once"
        } else {
            "unknown-once"
        };
        let mut attempt = Operation::new(
            id,
            record.scope.clone(),
            record.actor_id.clone(),
            record.target.clone(),
            &record.message,
        )
        .unwrap();
        outbox.insert(&attempt).unwrap();
        outbox.begin_attempt(&mut attempt).unwrap();
        if !interrupted {
            attempt.status = Status::Unknown;
            outbox.finish(&attempt).unwrap();
        }
        for _ in 0..2 {
            let result = runtime
                .call(
                    "bilibili.comment_latest",
                    &json!({
                        "operation_id":id,"message":record.message,
                    }),
                )
                .unwrap();
            assert_eq!(result["status"], "unknown");
            assert!(result["receipt"].is_null());
            assert_eq!(runtime.scope(Platform::Bilibili).account, "public");
        }
        let mut saved = outbox.load(id, &record.scope).unwrap();
        assert!(outbox.begin_attempt(&mut saved).is_err());
        saved.attempt_context = Some(json!({"before_post_ids":[123]}));
        assert!(outbox.finish(&saved).is_err());
        saved.attempt_context = None;
        saved.request = Some(json!({"root":8,"parent":8,"image_resource":null}));
        assert!(outbox.finish(&saved).is_err());
    }
    let mut rich = Operation::new(
        "rich-verified-once",
        record.scope.clone(),
        record.actor_id.clone(),
        record.target.clone(),
        "rich reply",
    )
    .unwrap();
    rich.request = Some(json!({"root":8,"parent":8,"image_resource":"a".repeat(32)}));
    outbox.insert(&rich).unwrap();
    outbox.begin_attempt(&mut rich).unwrap();
    rich.status = Status::Sent;
    outbox.finish(&rich).unwrap();
    rich.status = Status::Verified;
    rich.verification = Some(json!({"root":8,"parent":8,"message":"rich reply"}));
    outbox.finish(&rich).unwrap();
    let rich_params = json!({"operation_id":"rich-verified-once","root":8,"image_resource":"a".repeat(32),"message":"rich reply"});
    assert_eq!(
        runtime.call("bilibili.reply_latest", &rich_params).unwrap()["status"],
        "verified"
    );
    for change in [json!({"root":9}), json!({"image_resource":"b".repeat(32)})] {
        let mut changed = rich_params.clone();
        for (key, value) in change.as_object().unwrap() {
            changed[key] = value.clone();
        }
        assert!(runtime.call("bilibili.reply_latest", &changed).is_err());
        assert_eq!(runtime.scope(Platform::Bilibili).account, "public");
    }
    assert!(
        runtime
            .call(
                "bilibili.comment_latest",
                &json!({"operation_id":"verified-once","message":"different"})
            )
            .is_err()
    );
    assert_eq!(runtime.scope(Platform::Bilibili).account, "public");
    let accounts = runtime.call("runtime.accounts", &json!({})).unwrap();
    assert_eq!(accounts["bilibili"], json!(["test-bili"]));
}

#[test]
#[cfg(feature = "weixin-live")]
fn weixin_control_plane_and_identity_are_separate_from_ai_resources() {
    use agent_runtime::private_state::{PrivateLocators, account_directory};
    use runtime_core::{Platform, Scope};
    use std::{fs::OpenOptions, os::unix::fs::OpenOptionsExt};
    let lab = Lab::new();
    let state = lab.0.join("wx-state");
    let mut runtime = Runtime::open(&state, "test-alias", None).unwrap();
    assert!(runtime.call("weixin.login", &json!({})).is_err());
    assert!(
        runtime
            .call("weixin.send", &json!({"text":"must not send"}))
            .is_err()
    );
    assert_eq!(
        runtime.call("weixin.status", &json!({})).unwrap()["status"],
        "signed_out"
    );
    let account = account_directory(&state, "weixin", "test-alias").unwrap();
    let path = account.join("session.json");
    let mut saved = json!({"schema":1,"host":"wx.qq.com","skey":"synthetic-private-skey","sid":"synthetic-private-sid","uin":12345,"pass_ticket":"synthetic-private-ticket","device_id":"e123456789012345","cookies":[],"sync_key":[[1,2]],"user":{"username":"@synthetic","nickname":"test","remark":"","group":false}});
    {
        let file = OpenOptions::new()
            .write(true)
            .create_new(true)
            .mode(0o600)
            .open(&path)
            .unwrap();
        serde_json::to_writer(file, &saved).unwrap();
    }
    let status = runtime.call("weixin.status", &json!({})).unwrap();
    assert_eq!(status["status"], "saved_session");
    assert!(!status.to_string().contains("synthetic-private"));
    let locators = PrivateLocators::open(&state).unwrap();
    let scope = Scope {
        platform: Platform::Weixin,
        account: "test-alias".into(),
    };
    let asset = locators
        .put(
            &scope,
            "weixin-payload",
            &json!({"key":"must-stay-private"}),
        )
        .unwrap();
    assert!(
        runtime
            .call("resource.read", &json!({"resource":asset}))
            .is_err()
    );
    let listed = runtime.call("asset.list", &json!({})).unwrap();
    assert!(!listed.to_string().contains("must-stay-private"));
    saved["uin"] = json!(98765);
    {
        let file = OpenOptions::new()
            .write(true)
            .truncate(true)
            .open(&path)
            .unwrap();
        serde_json::to_writer(file, &saved).unwrap();
    }
    assert!(runtime.call("weixin.status", &json!({})).is_err());
}

#[test]
#[cfg(not(feature = "weixin-live"))]
fn paused_weixin_entrypoints_do_not_read_account_state_or_offer_live_tools() {
    let lab = Lab::new();
    let state = lab.0.join("paused-state");
    let mut runtime = Runtime::open(&state, "test-alias", None).unwrap();
    assert_eq!(
        runtime.call("weixin.status", &json!({})).unwrap(),
        json!({"status":"testing_paused","live_enabled":false,"account_state_read":false,"automatic_retry":false})
    );
    assert!(runtime.weixin_login().unwrap_err().contains("paused"));
    for profile in ["web", "desktop"] {
        assert!(
            runtime
                .weixin_login_profile(profile)
                .unwrap_err()
                .contains("paused")
        );
    }
    assert!(runtime.weixin_poll_login().unwrap_err().contains("paused"));
    for method in [
        "weixin.resume",
        "weixin.contacts",
        "weixin.receive",
        "weixin.inbox",
        "weixin.media",
    ] {
        assert!(
            runtime
                .call(method, &json!({}))
                .unwrap_err()
                .contains("paused")
        );
    }
    assert!(!state.join("accounts").exists());
    let specs = agent_runtime::tools::specs();
    assert!(specs.iter().any(|spec| spec.name == "social_open"));
    assert!(!specs.iter().any(|spec| spec.name == "tieba_thread"));
    assert!(specs.iter().any(|spec| spec.name == "weixin_status"));
    assert!(
        !specs
            .iter()
            .any(|spec| spec.name.starts_with("weixin_") && spec.name != "weixin_status")
    );
}
