// SPDX-License-Identifier: MIT
//! Synthetic ACP peer for offline native-wire acceptance. No account or network.
use serde_json::{Value, json};
use std::io::{self, BufRead, Write};
fn emit(value: Value) {
    let mut out = io::stdout().lock();
    serde_json::to_writer(&mut out, &value).unwrap();
    out.write_all(b"\n").unwrap();
    out.flush().unwrap();
}
fn receive(lines: &mut impl Iterator<Item = io::Result<String>>) -> Value {
    serde_json::from_str(&lines.next().unwrap().unwrap()).unwrap()
}
fn update(session: &str, kind: &str, text: &str) {
    emit(
        json!({"jsonrpc":"2.0","method":"session/update","params":{"sessionId":session,"update":{"sessionUpdate":kind,"content":{"type":"text","text":text}}}}),
    );
}
fn main() {
    assert!(std::env::var_os("GEMINI_API_KEY").is_none());
    assert!(std::env::var_os("GOOGLE_API_KEY").is_none());
    assert!(std::env::var_os("AGY_ADC_AUTH").is_none());
    assert_eq!(
        std::env::var("AGY_CLI_DISABLE_AUTO_UPDATE").unwrap(),
        "true"
    );
    let scenario = std::env::args().nth(1).unwrap();
    let args = std::env::args().collect::<Vec<_>>();
    let app_dir = args
        .windows(2)
        .find(|pair| pair[0] == "--app-data-dir")
        .unwrap();
    assert_eq!(app_dir[1], "antigravity-cli");
    let home = std::env::var_os("HOME").unwrap();
    let settings: Value = serde_json::from_slice(
        &std::fs::read(std::path::Path::new(&home).join(".gemini/antigravity-cli/settings.json"))
            .unwrap(),
    )
    .unwrap();
    assert_eq!(settings["toolPermission"], "request-review");
    assert_eq!(settings["permissions"]["allow"], json!(["mcp(runtime/*)"]));
    assert!(
        settings["permissions"]["deny"]
            .as_array()
            .unwrap()
            .contains(&json!("command(*)"))
    );
    let mut lines = io::stdin().lock().lines();
    while let Some(line) = lines.next() {
        let request: Value = serde_json::from_str(&line.unwrap()).unwrap();
        let id = request["id"].clone();
        assert_eq!(request["jsonrpc"], "2.0");
        match request["method"].as_str().unwrap() {
            "initialize" => {
                assert_eq!(request["params"]["protocolVersion"], 1);
                assert_eq!(request["params"]["clientCapabilities"]["terminal"], false);
                assert_eq!(
                    request["params"]["clientCapabilities"]["fs"]["readTextFile"],
                    false
                );
                let auth = if scenario == "missing-personal" {
                    json!([])
                } else {
                    json!([{"id":"oauth-personal","name":"PRIVATE_EMAIL_SHOULD_NOT_LEAK"}])
                };
                emit(
                    json!({"jsonrpc":"2.0","id":id,"result":{"protocolVersion":1,"agentCapabilities":{"_meta":{"antigravity":{"systemPrompt":{"append":scenario!="missing-persona"}}},"promptCapabilities":{"image":true,"audio":true},"auth":{"logout":{}}},"authMethods":auth,"private_account":"PRIVATE_ACCOUNT_SHOULD_NOT_LEAK"}}),
                );
            }
            "session/new" => {
                let params = &request["params"];
                assert_eq!(params["_meta"]["disableBuiltInTools"], true);
                assert!(params.get("disableBuiltInTools").is_none());
                assert!(
                    params["_meta"]["antigravity"]
                        .get("disableBuiltInTools")
                        .is_none()
                );
                assert_eq!(
                    params["_meta"]["antigravity"]["systemPrompt"]["append"],
                    "OPERATOR_PERSONA_MARKER"
                );
                assert_eq!(params["mcpServers"][0]["name"], "runtime");
                assert_eq!(params["mcpServers"][0]["args"][0], "engine");
                assert_eq!(params["mcpServers"][0]["args"][1], "mcp-bridge");
                assert_eq!(params["mcpServers"][0]["env"], json!([]));
                if scenario == "needs-auth" {
                    emit(
                        json!({"jsonrpc":"2.0","id":id,"error":{"code":-32000,"message":"PRIVATE_AUTH_SHOULD_NOT_LEAK"}}),
                    );
                } else {
                    emit(json!({"jsonrpc":"2.0","id":id,"result":{"sessionId":"fixture-session"}}));
                }
            }
            "session/set_model" => {
                assert_eq!(request["params"]["sessionId"], "fixture-session");
                assert_eq!(request["params"]["modelId"], "fixture-model");
                emit(json!({"jsonrpc":"2.0","id":id,"result":{}}));
            }
            "session/prompt" => {
                assert_eq!(request["params"]["sessionId"], "fixture-session");
                assert_eq!(
                    request["params"]["prompt"][0]["text"],
                    "synthetic explicit input"
                );
                if scenario == "hang" {
                    std::thread::sleep(std::time::Duration::from_secs(60));
                }
                if scenario == "client-requests" {
                    for (index, method) in [
                        "session/request_permission",
                        "fs/read_text_file",
                        "terminal/create",
                        "client/unsafe",
                    ]
                    .into_iter()
                    .enumerate()
                    {
                        emit(
                            json!({"jsonrpc":"2.0","id":format!("deny-{index}"),"method":method,"params":{"sessionId":"fixture-session","toolCall":{"toolCallId":"unsafe"},"options":[{"optionId":"allow","kind":"allow_once"}]}}),
                        );
                        let reply = receive(&mut lines);
                        if index == 0 {
                            assert_eq!(reply["result"]["outcome"]["outcome"], "cancelled");
                        } else {
                            assert_eq!(reply["error"]["code"], -32601);
                        }
                    }
                }
                if scenario == "wrong-session" {
                    update("another-session", "agent_message_chunk", "wrong");
                } else if scenario == "oversized-output" {
                    update("fixture-session", "agent_message_chunk", &"X".repeat(8192));
                } else if scenario == "wire-limit" {
                    for _ in 0..12 {
                        update("fixture-session", "agent_thought_chunk", &"P".repeat(8192));
                    }
                } else {
                    update(
                        "fixture-session",
                        "agent_thought_chunk",
                        "PRIVATE_THOUGHT_SHOULD_NOT_LEAK",
                    );
                    update(
                        "fixture-session",
                        "user_message_chunk",
                        "PRIVATE_USER_ECHO_SHOULD_NOT_LEAK",
                    );
                    update("fixture-session", "agent_message_chunk", "actual ");
                    update("fixture-session", "agent_message_chunk", "answer");
                }
                let status = match scenario.as_str() {
                    "cancelled" => "cancelled",
                    "max-tokens" => "max_tokens",
                    "unknown-stop" => "new-unknown-stop",
                    _ => "end_turn",
                };
                let response_id = if scenario == "wrong-response" {
                    json!(99999)
                } else {
                    id
                };
                if scenario == "remote-error" {
                    emit(
                        json!({"jsonrpc":"2.0","id":response_id,"error":{"code":-32603,"message":"PRIVATE_ERROR_SHOULD_NOT_LEAK"}}),
                    );
                } else {
                    emit(json!({"jsonrpc":"2.0","id":response_id,"result":{"stopReason":status}}));
                }
            }
            unexpected => panic!("unexpected ACP method {unexpected}"),
        }
    }
}
