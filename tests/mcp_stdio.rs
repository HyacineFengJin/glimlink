// SPDX-License-Identifier: MIT
//! Offline wire tests through the real binary. Every state/account is synthetic.
use agent_runtime::{
    app::Runtime,
    outbox::{Operation, Outbox, Status},
    private_state::PrivateLocators,
};
use base64::{Engine, engine::general_purpose::STANDARD};
use runtime_core::{Platform, Scope};
use serde_json::{Value, json};
use std::{
    fs,
    io::{BufRead, BufReader, Write},
    path::{Path, PathBuf},
    process::{Child, ChildStdin, ChildStdout, Command, Stdio},
    sync::atomic::{AtomicU64, Ordering},
};

static NEXT: AtomicU64 = AtomicU64::new(0);
struct Lab(PathBuf);
impl Lab {
    fn new() -> Self {
        let path = std::env::temp_dir().join(format!(
            "mcp-wire-{}-{}",
            std::process::id(),
            NEXT.fetch_add(1, Ordering::Relaxed)
        ));
        fs::create_dir(&path).unwrap();
        Self(path)
    }
    fn state(&self) -> PathBuf {
        self.0.join("state")
    }
}
impl Drop for Lab {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}
struct Peer {
    child: Child,
    input: Option<ChildStdin>,
    output: BufReader<ChildStdout>,
}
impl Peer {
    fn new(state: &Path) -> Self {
        let mut child = Command::new(env!("CARGO_BIN_EXE_agent-runtime"))
            .arg("--state")
            .arg(state)
            .arg("mcp")
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .unwrap();
        let input = child.stdin.take();
        let output = BufReader::new(child.stdout.take().unwrap());
        Self {
            child,
            input,
            output,
        }
    }
    fn write(&mut self, value: &Value) {
        serde_json::to_writer(self.input.as_mut().unwrap(), value).unwrap();
        self.input.as_mut().unwrap().write_all(b"\n").unwrap();
        self.input.as_mut().unwrap().flush().unwrap();
    }
    fn response(&mut self) -> Value {
        let mut line = String::new();
        assert!(
            self.output.read_line(&mut line).unwrap() > 0,
            "MCP server exited without a response"
        );
        let value: Value =
            serde_json::from_str(&line).expect("stdout must contain only MCP messages");
        assert_eq!(value["jsonrpc"], "2.0");
        value
    }
    fn request(&mut self, value: Value) -> Value {
        self.write(&value);
        self.response()
    }
    fn raw(&mut self, bytes: &[u8]) -> Value {
        self.input.as_mut().unwrap().write_all(bytes).unwrap();
        self.input.as_mut().unwrap().flush().unwrap();
        self.response()
    }
    fn legacy(&mut self, version: &str) -> Value {
        let response = self.request(json!({"jsonrpc":"2.0","id":"initialize","method":"initialize",
            "params":{"protocolVersion":version,"capabilities":{},"clientInfo":{"name":"wire-test","version":"1"}}}));
        self.write(&json!({"jsonrpc":"2.0","method":"notifications/initialized"}));
        response
    }
    fn close(mut self) {
        drop(self.input.take());
        assert!(self.child.wait().unwrap().success());
        let mut tail = String::new();
        assert_eq!(
            self.output.read_line(&mut tail).unwrap(),
            0,
            "notifications/shutdown must not add protocol responses"
        );
    }
}
impl Drop for Peer {
    fn drop(&mut self) {
        drop(self.input.take());
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}
fn meta(version: &str) -> Value {
    json!({"io.modelcontextprotocol/protocolVersion":version,
        "io.modelcontextprotocol/clientCapabilities":{},"io.modelcontextprotocol/clientInfo":{"name":"wire-test","version":"1"}})
}
fn modern(id: Value, method: &str, mut params: Value) -> Value {
    params["_meta"] = meta("2026-07-28");
    json!({"jsonrpc":"2.0","id":id,"method":method,"params":params})
}
fn structured(response: &Value) -> &Value {
    &response["result"]["structuredContent"]
}

#[test]
fn modern_requests_are_stateless_versioned_and_list_only_the_default_task_catalog() {
    let lab = Lab::new();
    let mut peer = Peer::new(&lab.state());
    let discovery = peer.request(modern(json!(1), "server/discover", json!({})));
    assert_eq!(discovery["result"]["resultType"], "complete");
    assert!(
        discovery["result"]["supportedVersions"]
            .as_array()
            .unwrap()
            .contains(&json!("2026-07-28"))
    );
    assert_eq!(discovery["result"]["capabilities"]["tools"], json!({}));
    assert_eq!(
        discovery["result"]["_meta"]["io.modelcontextprotocol/serverInfo"]["name"],
        "agent-runtime"
    );
    let bare = peer.request(json!({"jsonrpc":"2.0","id":2,"method":"tools/list"}));
    assert_eq!(bare["error"]["code"], -32602); // discovery established no hidden session
    let mut request = modern(json!(3), "tools/list", json!({}));
    request["params"]["_meta"] = meta("2999-01-01");
    let unknown = peer.request(request);
    assert_eq!(unknown["error"]["code"], -32022);
    assert_eq!(unknown["error"]["data"]["requested"], "2999-01-01");
    assert_eq!(unknown["error"]["data"]["supported"], json!(["2026-07-28"]));
    let mut missing = modern(json!(4), "tools/list", json!({}));
    missing["params"]["_meta"]
        .as_object_mut()
        .unwrap()
        .remove("io.modelcontextprotocol/clientCapabilities");
    assert_eq!(peer.request(missing)["error"]["code"], -32602);
    let list = peer.request(modern(json!(5), "tools/list", json!({})));
    let listed = list["result"]["tools"].as_array().unwrap();
    let expected = agent_runtime::tools::specs();
    assert_eq!(listed.len(), expected.len());
    for (entry, spec) in listed.iter().zip(expected) {
        assert_eq!(entry["name"], spec.name);
        assert_eq!(entry["inputSchema"], spec.input_schema);
        assert!(!spec.name.starts_with("bilibili_") && !spec.name.starts_with("tieba_"));
    }
    assert_eq!(
        peer.request(modern(json!(6), "ping", json!({})))["error"]["code"],
        -32601
    );
    peer.close();
}

#[test]
fn legacy_negotiation_requires_initialized_and_does_not_respond_to_notifications() {
    let lab = Lab::new();
    let mut peer = Peer::new(&lab.state());
    let ping = peer.request(json!({"jsonrpc":"2.0","id":0,"method":"ping"}));
    assert_eq!(ping["result"], json!({}));
    let initialize = peer.request(json!({"jsonrpc":"2.0","id":1,"method":"initialize",
        "params":{"protocolVersion":"2026-07-28","capabilities":{},"clientInfo":{"name":"legacy","version":"1"}}}));
    assert_eq!(initialize["result"]["protocolVersion"], "2025-11-25");
    assert!(initialize["result"].get("resultType").is_none());
    assert!(
        peer.request(json!({"jsonrpc":"2.0","id":2,"method":"tools/list"}))
            .get("error")
            .is_some()
    );
    peer.write(&json!({"jsonrpc":"2.0","method":"notifications/initialized"}));
    peer.write(&json!({"jsonrpc":"2.0","method":"notifications/not-implemented"}));
    // A business method sent as a notification must not execute at all.
    peer.write(&json!({"jsonrpc":"2.0","method":"tools/call","params":{"name":"social_send","arguments":{"latest_own":"bilibili","message":"must never dispatch"}}}));
    let list = peer.request(json!({"jsonrpc":"2.0","id":3,"method":"tools/list"}));
    assert_eq!(list["id"], 3);
    assert!(list["result"]["tools"].is_array());
    assert!(!lab.state().join("outbox").exists());
    peer.close();
    let mut peer = Peer::new(&lab.state());
    assert_eq!(
        peer.legacy("2025-06-18")["result"]["protocolVersion"],
        "2025-06-18"
    );
    let call = peer.request(
        json!({"jsonrpc":"2.0","id":4,"method":"tools/call","params":{"name":"runtime_modules"}}),
    );
    assert_eq!(call["result"]["isError"], false);
    assert!(structured(&call)["modules"].is_array());
    peer.close();
}

#[test]
fn protocol_errors_tool_errors_and_paused_capabilities_are_distinct() {
    let lab = Lab::new();
    let mut peer = Peer::new(&lab.state());
    let unknown = peer.request(modern(
        json!(1),
        "tools/call",
        json!({"name":"bilibili_fetch","arguments":{}}),
    ));
    assert_eq!(unknown["error"]["code"], -32602);
    let malformed = peer.request(modern(
        json!(2),
        "tools/call",
        json!({"name":"runtime_modules","arguments":[]}),
    ));
    assert_eq!(malformed["error"]["code"], -32602);
    let invalid = peer.request(modern(
        json!(3),
        "tools/call",
        json!({"name":"social_discussion","arguments":{"target_ref":"synthetic","limit":0}}),
    ));
    assert!(invalid.get("error").is_none());
    assert_eq!(invalid["result"]["isError"], true);
    assert!(
        structured(&invalid)["error"]
            .as_str()
            .unwrap()
            .contains("supported range")
    );
    let injected = peer.request(modern(
        json!(4),
        "tools/call",
        json!({"name":"runtime_modules","arguments":{"cookie":"must not be accepted"}}),
    ));
    assert_eq!(injected["result"]["isError"], true);
    let absent = peer.request(modern(
        json!(5),
        "tools/call",
        json!({"name":"social_discussion","arguments":{"target_ref":"a".repeat(32)}}),
    ));
    assert_eq!(absent["result"]["isError"], true);
    #[cfg(not(feature = "weixin-live"))]
    {
        let paused = peer.request(modern(
            json!(6),
            "tools/call",
            json!({"name":"weixin_status"}),
        ));
        assert_eq!(structured(&paused)["status"], "testing_paused");
        assert_eq!(structured(&paused)["account_state_read"], false);
    }
    peer.close();
}

#[test]
fn real_cached_image_becomes_mcp_image_content_with_actual_bytes() {
    let lab = Lab::new();
    let file = lab.0.join("fixture.png");
    image::DynamicImage::new_rgb8(8, 8).save(&file).unwrap();
    let runtime = Runtime::open(&lab.state(), "synthetic-image-owner", None).unwrap();
    let resource = runtime.import_file(&file, "image/png").unwrap();
    drop(runtime);
    let mut peer = Peer::new(&lab.state());
    let response = peer.request(modern(
        json!(1),
        "tools/call",
        json!({"name":"media_read","arguments":{"reference":resource.id}}),
    ));
    assert_eq!(response["result"]["isError"], false);
    let picture = response["result"]["content"]
        .as_array()
        .unwrap()
        .iter()
        .find(|item| item["type"] == "image")
        .unwrap();
    assert_eq!(picture["mimeType"], "image/jpeg");
    let bytes = STANDARD.decode(picture["data"].as_str().unwrap()).unwrap();
    assert!(bytes.starts_with(b"\xff\xd8\xff"));
    let image = image::load_from_memory(&bytes).unwrap();
    assert_eq!((image.width(), image.height()), (8, 8));
    assert_eq!(
        structured(&response)["engine_input"]["image_resources_attached"]
            .as_array()
            .unwrap()
            .len(),
        1
    );
    assert_eq!(
        structured(&response)["resolved_source"]["scope"]["account"],
        "synthetic-image-owner"
    );
    peer.close();
}

#[test]
fn malformed_oversized_and_batch_frames_do_not_execute_or_desynchronize_stdio() {
    let lab = Lab::new();
    let mut peer = Peer::new(&lab.state());
    assert_eq!(peer.raw(b"not-json\n")["error"]["code"], -32700);
    let batch = json!([modern(
        json!(1),
        "tools/call",
        json!({"name":"social_send","arguments":{"latest_own":"bilibili","message":"never dispatch"}})
    )]);
    assert_eq!(peer.request(batch)["error"]["code"], -32600);
    let mut oversized = vec![b'x'; 1024 * 1024 + 10];
    oversized.push(b'\n');
    assert_eq!(peer.raw(&oversized)["error"]["code"], -32600);
    let response = peer.request(modern(
        json!(2),
        "tools/call",
        json!({"name":"runtime_modules"}),
    ));
    assert_eq!(response["id"], 2);
    assert_eq!(response["result"]["isError"], false);
    assert!(!lab.state().join("outbox").exists());
    peer.close();
}

#[test]
fn auto_ids_identify_new_calls_and_explicit_retry_ids_read_synthetic_cached_outcomes() {
    let lab = Lab::new();
    let runtime = Runtime::open(&lab.state(), "synthetic-owner", None).unwrap();
    let scope = Scope {
        platform: Platform::Bilibili,
        account: "synthetic-owner".into(),
    };
    let locators = PrivateLocators::open(&lab.state()).unwrap();
    let creator = locators
        .put_reusable(&scope, "task-target", &json!({"kind":"creator","mid":42}))
        .unwrap();
    let video = locators
        .put_reusable(
            &scope,
            "task-target",
            &json!({"kind":"video","video":"BV17x411w7KC"}),
        )
        .unwrap();
    let outbox = Outbox::open(&lab.state()).unwrap();
    let mut operation = Operation::new(
        "synthetic-cached-write",
        scope.clone(),
        "42".into(),
        json!({"aid":170001,"bvid":"BV17x411w7KC","title":"synthetic fixture","owner_mid":42}),
        "cached fixture only",
    )
    .unwrap();
    outbox.insert(&operation).unwrap();
    outbox.begin_attempt(&mut operation).unwrap();
    operation.status = Status::Sent;
    outbox.finish(&operation).unwrap();
    operation.status = Status::Verified;
    operation.verification = Some(json!({"synthetic":true}));
    outbox.finish(&operation).unwrap();
    let timestamp = operation.attempt_started_at;
    drop(runtime);
    let mut peer = Peer::new(&lab.state());
    let automatic = |reference: &str| {
        modern(
            json!(7),
            "tools/call",
            json!({"name":"social_send","arguments":{"target_ref":reference,"message":"unsupported creator target"}}),
        )
    };
    let one = peer.request(automatic(&creator));
    let two = peer.request(automatic(&creator)); // completed modern IDs may be reused
    assert_eq!(structured(&one)["status"], "unsupported");
    let first_id = structured(&one)["operation_id"]
        .as_str()
        .unwrap()
        .to_owned();
    assert_ne!(structured(&two)["operation_id"], first_id);
    let retry = || {
        modern(
            json!(8),
            "tools/call",
            json!({"name":"social_send","arguments":{"target_ref":video,"message":"cached fixture only","operation_id":"synthetic-cached-write"}}),
        )
    };
    assert_eq!(structured(&peer.request(retry()))["status"], "verified");
    peer.close();
    let mut peer = Peer::new(&lab.state());
    assert_ne!(
        structured(&peer.request(automatic(&creator)))["operation_id"],
        first_id
    );
    assert_eq!(structured(&peer.request(retry()))["status"], "verified");
    peer.close();
    let stored = outbox.load("synthetic-cached-write", &scope).unwrap();
    assert_eq!(stored.attempt_started_at, timestamp);
    assert_eq!(fs::read_dir(lab.state().join("outbox")).unwrap().count(), 1);
}
