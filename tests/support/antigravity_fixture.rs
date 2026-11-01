// SPDX-License-Identifier: MIT
//! Offline ACP peer which exercises the real Runtime MCP proxy. Synthetic data only.
use serde_json::{Value, json};
use std::{
    io::{self, BufRead, BufReader, Write},
    process::{Child, Command, Stdio},
};

fn emit(value: Value) {
    let mut out = io::stdout().lock();
    serde_json::to_writer(&mut out, &value).unwrap();
    out.write_all(b"\n").unwrap();
    out.flush().unwrap();
}

struct Mcp {
    child: Child,
    output: BufReader<std::process::ChildStdout>,
}
impl Drop for Mcp {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}
impl Mcp {
    fn open(server: &Value) -> Self {
        assert_eq!(server["name"], "runtime");
        assert_eq!(server["args"][0], "engine");
        assert_eq!(server["args"][1], "mcp-bridge");
        assert_eq!(server["env"], json!([]));
        let mut child = Command::new(server["command"].as_str().unwrap())
            .args(
                server["args"]
                    .as_array()
                    .unwrap()
                    .iter()
                    .map(|v| v.as_str().unwrap()),
            )
            .env_clear()
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .spawn()
            .unwrap();
        let output = BufReader::new(child.stdout.take().unwrap());
        let mut client = Self { child, output };
        let init = client.rpc("initialize", json!({"protocolVersion":"2025-11-25","clientInfo":{"name":"synthetic-acp-peer","version":"1"},"capabilities":{}}));
        assert!(init.get("result").is_some());
        client.send(json!({"jsonrpc":"2.0","method":"notifications/initialized"}));
        client
    }
    fn send(&mut self, value: Value) {
        let stdin = self.child.stdin.as_mut().unwrap();
        serde_json::to_writer(&mut *stdin, &value).unwrap();
        stdin.write_all(b"\n").unwrap();
        stdin.flush().unwrap();
    }
    fn rpc(&mut self, method: &str, params: Value) -> Value {
        // Completed RPC IDs can be reused without reusing a business operation.
        self.send(json!({"jsonrpc":"2.0","id":7,"method":method,"params":params}));
        let mut line = String::new();
        assert!(
            self.output.read_line(&mut line).unwrap() > 0,
            "MCP proxy closed"
        );
        let result: Value = serde_json::from_str(&line).unwrap();
        assert_eq!(result["id"], 7);
        result
    }
    fn tool(&mut self, name: &str, arguments: Value) -> Value {
        self.rpc("tools/call", json!({"name":name,"arguments":arguments}))
    }
}

fn context(request: &Value) -> Value {
    let input = request["params"]["prompt"][0]["text"].as_str().unwrap();
    let (_, json) = input.split_once('\n').unwrap();
    serde_json::from_str(json).unwrap()
}

fn main() {
    std::panic::set_hook(Box::new(|info| {
        let _ = std::fs::write("fixture.failure", info.to_string());
    }));
    for credential in [
        "GEMINI_API_KEY",
        "GOOGLE_API_KEY",
        "GOOGLE_APPLICATION_CREDENTIALS",
        "AGY_ADC_AUTH",
    ] {
        assert!(std::env::var_os(credential).is_none());
    }
    assert_eq!(
        std::env::var("AGY_CLI_DISABLE_AUTO_UPDATE").unwrap(),
        "true"
    );
    let requested = std::env::args().nth(1).unwrap();
    // Real daemon startup, still using this native ELF test peer.
    let scenario = if matches!(requested.as_str(), "acp" | "--experimental-acp") {
        "filtered".to_owned()
    } else {
        requested
    };
    let session = format!("agy-fixture-{}", std::process::id());
    let mut mcp = None;
    for line in io::stdin().lock().lines() {
        let request: Value = serde_json::from_str(&line.unwrap()).unwrap();
        let id = request["id"].clone();
        match request["method"].as_str().unwrap() {
            "initialize" => {
                assert_eq!(request["params"]["protocolVersion"], 1);
                assert_eq!(request["params"]["clientCapabilities"]["terminal"], false);
                emit(
                    json!({"jsonrpc":"2.0","id":id,"result":{"protocolVersion":1,"agentCapabilities":{"_meta":{"antigravity":{"systemPrompt":{"append":true}}}},"authMethods":[{"id":"oauth-personal","name":"Synthetic Google subscription"}]}}),
                );
            }
            "session/new" => {
                assert_eq!(request["params"]["_meta"]["disableBuiltInTools"], true);
                assert!(request["params"].get("disableBuiltInTools").is_none());
                assert!(
                    request["params"]["_meta"]["antigravity"]
                        .get("disableBuiltInTools")
                        .is_none()
                );
                let developer = request["params"]["_meta"]["antigravity"]["systemPrompt"]["append"]
                    .as_str()
                    .unwrap();
                assert!(developer.contains("MIND_PERSONA_SHARED_MARKER"));
                assert!(developer.contains("MIND_ROLE_READER_MARKER"));
                for marker in [
                    "MIND_SHARED_FACT_MARKER",
                    "MIND_PRIVATE_A_MARKER",
                    "MIND_WRITER_SHARED_FACT_MARKER",
                    "AGY_SHARED_WRITTEN",
                ] {
                    assert!(!developer.contains(marker), "memory must remain data");
                }
                if scenario == "needs-auth" {
                    emit(
                        json!({"jsonrpc":"2.0","id":id,"error":{"code":-32000,"message":"synthetic login required"}}),
                    );
                    continue;
                }
                let mut connection = Mcp::open(&request["params"]["mcpServers"][0]);
                let catalog = connection.rpc("tools/list", json!({}));
                let names = catalog["result"]["tools"]
                    .as_array()
                    .unwrap()
                    .iter()
                    .map(|v| v["name"].as_str().unwrap())
                    .collect::<Vec<_>>();
                if scenario == "activity" {
                    assert!(names.contains(&"runtime_memory"));
                    assert!(!names.contains(&"social_send"));
                } else if scenario == "filtered" {
                    assert_eq!(names, vec!["runtime_modules"]);
                } else if scenario == "invalid-write" {
                    assert_eq!(names, vec!["social_send"]);
                } else {
                    assert_eq!(names, vec!["runtime_memory"]);
                }
                if scenario == "premature" {
                    let _ = connection.tool("runtime_memory", json!({"action":"remember","key":"premature","text":"must-not-be-saved","expected_revision":0}));
                    return;
                }
                mcp = Some(connection);
                emit(json!({"jsonrpc":"2.0","id":id,"result":{"sessionId":session}}));
            }
            "session/set_model" => {
                assert_eq!(scenario, "filtered");
                assert_eq!(request["params"]["sessionId"], session);
                assert_eq!(request["params"]["modelId"], "fixture-agy");
                emit(json!({"jsonrpc":"2.0","id":id,"result":{}}));
            }
            "session/prompt" => {
                assert_eq!(request["params"]["sessionId"], session);
                let data = context(&request);
                if scenario == "activity" {
                    assert!(data["recent_dialogue"].is_null());
                    assert_eq!(
                        data["autonomous_activity"]["checkpoint"]["summary"],
                        "CODEX_ACTIVITY_OBSERVATION"
                    );
                    let episode = data["autonomous_activity"]["recent_episodes"][0]["episode_id"]
                        .as_str()
                        .unwrap();
                    let connection = mcp.as_mut().unwrap();
                    let old = connection.tool(
                        "runtime_memory",
                        json!({"action":"activity_read","episode_id":episode,"limit":4096}),
                    );
                    assert_eq!(old["result"]["isError"], false);
                    assert!(
                        old["result"]["structuredContent"]["result"]["text"]
                            .as_str()
                            .unwrap()
                            .contains("Read-only Runtime job fixture")
                    );
                    let save=connection.tool("runtime_memory",json!({"action":"activity_save","expected_revision":1,"checkpoint":{
                        "summary":"AGY_CONTINUED_OBSERVATION","interests":["visual storytelling"],"questions":[],"anchors":[],"rest_seconds":120}}));
                    assert_eq!(save["result"]["isError"], false);
                    emit(
                        json!({"jsonrpc":"2.0","method":"session/update","params":{"sessionId":session,"update":{"sessionUpdate":"agent_message_chunk","content":{"type":"text","text":"Private autonomous continuation; no task and no public message."}}}}),
                    );
                    emit(json!({"jsonrpc":"2.0","id":id,"result":{"stopReason":"end_turn"}}));
                    continue;
                }
                assert_eq!(
                    data["identity"],
                    json!({"persona":"shared","agent":"reader","conversation":"chat-a"})
                );
                assert!(
                    data["recent_dialogue"]["turns"]
                        .as_array()
                        .unwrap()
                        .is_empty()
                );
                if matches!(
                    scenario.as_str(),
                    "quota" | "quota-quote" | "quota-after-tool"
                ) {
                    if scenario == "quota-after-tool" {
                        let result = mcp.as_mut().unwrap().tool("runtime_memory", json!({"action":"remember","key":"before-quota","text":"synthetic completed tool","expected_revision":0}));
                        assert_eq!(result["result"]["isError"], false);
                    }
                    let notice = "⚠ Individual quota reached. Please upgrade your subscription to increase your limits. Resets in 96h46m39s.";
                    let text = if scenario == "quota-quote" {
                        format!("Quoted provider message: {notice}")
                    } else {
                        notice.to_owned()
                    };
                    emit(
                        json!({"jsonrpc":"2.0","method":"session/update","params":{"sessionId":session,"update":{"sessionUpdate":"agent_message_chunk","content":{"type":"text","text":text}}}}),
                    );
                    emit(json!({"jsonrpc":"2.0","id":id,"result":{"stopReason":"end_turn"}}));
                    continue;
                }
                let connection = mcp.as_mut().unwrap();
                if scenario == "filtered" {
                    let result = connection.tool("runtime_memory", json!({"action":"remember","key":"forbidden","text":"must-not-be-saved","expected_revision":0}));
                    assert!(result.get("error").is_some());
                } else if scenario == "invalid-write" {
                    let result = connection.tool("social_send", json!({"target_ref":"00000000000000000000000000000000","message":"synthetic, never sent"}));
                    assert_eq!(result["result"]["isError"], true);
                } else {
                    if scenario == "read-codex" {
                        let recall = data["recollections"].to_string();
                        assert!(recall.contains("MIND_WRITER_SHARED_FACT_MARKER"));
                        assert!(recall.contains("MIND_PRIVATE_A_MARKER"));
                        assert!(!recall.contains("MIND_PRIVATE_B_MARKER"));
                        let result = connection.tool(
                            "runtime_memory",
                            json!({"action":"get","key":"from-writer-job"}),
                        );
                        assert_eq!(result["result"]["isError"], false);
                        assert_eq!(
                            result["result"]["structuredContent"]["result"]["memory"]["text"],
                            "MIND_WRITER_SHARED_FACT_MARKER"
                        );
                    }
                    let result = connection.tool("runtime_memory", json!({"action":"remember","key":"from-agy","text":"AGY_SHARED_WRITTEN","tags":["mind"],"expected_revision":0}));
                    assert_eq!(result["result"]["isError"], false);
                    assert_eq!(
                        result["result"]["structuredContent"]["result"]["memory"]["source"]["agent"],
                        "reader"
                    );
                }
                let update_session = if scenario == "wrong-session" {
                    "another-session"
                } else {
                    &session
                };
                emit(
                    json!({"jsonrpc":"2.0","method":"session/update","params":{"sessionId":update_session,"update":{"sessionUpdate":"agent_message_chunk","content":{"type":"text","text":"MIND_FIRST_ANSWER_MARKER"}}}}),
                );
                emit(json!({"jsonrpc":"2.0","id":id,"result":{"stopReason":"end_turn"}}));
            }
            unexpected => panic!("Unexpected ACP method {unexpected}"),
        }
    }
}
