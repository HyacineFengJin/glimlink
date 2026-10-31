// SPDX-License-Identifier: MIT
//! Account-free native fixture, built only with the test-helper feature.
use serde_json::{Value, json};
use std::io::{self, BufRead, Write};
fn send(value: Value) {
    println!("{value}");
    io::stdout().flush().unwrap();
}

/// Main Runtime job fixtures, kept separate from the engine's existing
/// adversarial scenarios. No environment credential, account file or network
/// access is performed; even authenticated status is entirely synthetic.
fn agent_job(scenario: &str) {
    let mut lines = io::stdin().lock().lines();
    let mut thread_id = None::<String>;
    let mut selected_model = None::<String>;
    let mut turns = 0_u32;
    while let Some(Ok(line)) = lines.next() {
        let request: Value = serde_json::from_str(&line).unwrap();
        let id = request.get("id").cloned().unwrap_or(Value::Null);
        match request["method"].as_str().unwrap_or("") {
            "initialize" => {
                assert!(request.pointer("/params/clientInfo/name").is_some());
                assert_eq!(
                    request.pointer("/params/capabilities/experimentalApi"),
                    Some(&json!(true))
                );
                send(
                    json!({"id":id,"result":{"userAgent":"native-agent-job-fixture","platformFamily":"unix","platformOs":"linux"}}),
                );
            }
            "initialized" => (),
            "account/read" => {
                assert_eq!(request.pointer("/params/refreshToken"), Some(&json!(false)));
                let account = if scenario == "agent_job_unauthenticated" {
                    Value::Null
                } else {
                    // Valid 0.165 ChatGPT account projection, with no API-key or
                    // token fields and no real subscription/account identity.
                    json!({"type":"chatgpt","planType":"pro","email":"fixture@example.invalid"})
                };
                send(json!({"id":id,"result":{"account":account,"requiresOpenaiAuth":true}}));
            }
            "thread/start" | "thread/resume" => {
                if scenario == "agent_job_daemon" {
                    selected_model = request["params"]["model"].as_str().map(str::to_owned);
                }
                assert_eq!(
                    request.pointer("/params/sandbox").and_then(Value::as_str),
                    Some("read-only")
                );
                assert_eq!(
                    request
                        .pointer("/params/approvalPolicy")
                        .and_then(Value::as_str),
                    Some("untrusted")
                );
                assert_eq!(
                    request
                        .pointer("/params/modelProvider")
                        .and_then(Value::as_str),
                    Some("openai")
                );
                if scenario.starts_with("agent_job_mind") {
                    let developer = request["params"]["developerInstructions"]
                        .as_str()
                        .expect("shared persona must be sent on the native wire");
                    assert!(developer.contains("MIND_PERSONA_SHARED_MARKER"));
                    let (role, other) = if scenario == "agent_job_mind_reader" {
                        ("MIND_ROLE_READER_MARKER", "MIND_ROLE_WRITER_MARKER")
                    } else {
                        ("MIND_ROLE_WRITER_MARKER", "MIND_ROLE_READER_MARKER")
                    };
                    assert!(developer.contains(role));
                    assert!(!developer.contains(other));
                    for memory in [
                        "MIND_SHARED_FACT_MARKER",
                        "MIND_PRIVATE_A_MARKER",
                        "MIND_PRIVATE_B_MARKER",
                        "MIND_WRITER_SHARED_FACT_MARKER",
                    ] {
                        assert!(
                            !developer.contains(memory),
                            "remembered data must not become developer instructions"
                        );
                    }
                }
                let current = if request["method"] == "thread/resume" {
                    // App-server 0.165 restores persisted schemas itself;
                    // thread/resume does not resend dynamicTools on the wire.
                    request
                        .pointer("/params/threadId")
                        .and_then(Value::as_str)
                        .filter(|value| !value.is_empty())
                        .expect("resume thread ID missing")
                        .to_owned()
                } else {
                    let tools = request
                        .pointer("/params/dynamicTools")
                        .and_then(Value::as_array)
                        .expect("job dynamic tools missing");
                    assert!(tools.iter().any(
                        |tool| tool["type"] == "function" && tool["name"] == "runtime_modules"
                    ));
                    format!("agent-job-thread-{}", std::process::id())
                };
                send(json!({"method":"thread/started","params":{"thread":{"id":current}}}));
                send(json!({"id":id,"result":{"thread":{"id":current}}}));
                thread_id = Some(current);
            }
            "turn/start" => {
                if scenario == "agent_job_unauthenticated" {
                    send(
                        json!({"id":id,"error":{"code":-32000,"message":"Synthetic ChatGPT login required; no model turn was started"}}),
                    );
                    continue;
                }
                let current = thread_id
                    .as_deref()
                    .expect("job thread was not started or resumed");
                assert_eq!(
                    request.pointer("/params/threadId").and_then(Value::as_str),
                    Some(current)
                );
                assert_eq!(
                    request
                        .pointer("/params/sandboxPolicy/type")
                        .and_then(Value::as_str),
                    Some("readOnly")
                );
                assert_eq!(
                    request.pointer("/params/sandboxPolicy/networkAccess"),
                    Some(&json!(false))
                );
                if scenario.starts_with("agent_job_mind") {
                    let inputs = request["params"]["input"]
                        .as_array()
                        .expect("mind inputs missing");
                    let first = inputs[0]["text"]
                        .as_str()
                        .expect("context data must be text");
                    let (_, data) = first
                        .split_once('\n')
                        .expect("attributed context JSON missing");
                    let data: Value = serde_json::from_str(data).expect("invalid context JSON");
                    assert_eq!(data["identity"]["persona"], "shared");
                    let recollections = data["recollections"].to_string();
                    assert!(recollections.contains("MIND_SHARED_FACT_MARKER"));
                    let conversation_a = scenario != "agent_job_mind_writer";
                    assert_eq!(
                        data["identity"]["agent"],
                        if scenario == "agent_job_mind_reader" {
                            "reader"
                        } else {
                            "writer"
                        }
                    );
                    assert_eq!(
                        data["identity"]["conversation"],
                        if conversation_a { "chat-a" } else { "chat-b" }
                    );
                    assert_eq!(
                        recollections.contains("MIND_PRIVATE_A_MARKER"),
                        conversation_a
                    );
                    assert_eq!(
                        recollections.contains("MIND_PRIVATE_B_MARKER"),
                        !conversation_a
                    );
                    let history = data["recent_dialogue"]["turns"]
                        .as_array()
                        .expect("dialogue envelope missing");
                    if scenario == "agent_job_mind_continuation" {
                        assert!(
                            recollections.contains("MIND_WRITER_SHARED_FACT_MARKER"),
                            "ordinary memory from another job/conversation must become shared recall"
                        );
                        assert_eq!(history.len(), 1);
                        assert_eq!(history[0]["job_id"], "mind-reader-a");
                        assert!(
                            history[0]["user"]
                                .as_str()
                                .unwrap()
                                .contains("MIND_FIRST_TASK_MARKER")
                        );
                        assert!(
                            history[0]["assistant"]
                                .as_str()
                                .unwrap()
                                .contains("MIND_FIRST_ANSWER_MARKER")
                        );
                        assert_eq!(data["identity"]["agent"], "writer");
                    } else {
                        assert!(
                            history.is_empty(),
                            "another conversation's history must not be injected"
                        );
                    }
                }
                turns += 1;
                let turn = format!("agent-job-turn-{turns}");
                let answer = if scenario == "agent_job_mind_reader" {
                    "MIND_FIRST_ANSWER_MARKER".to_owned()
                } else if scenario == "agent_job_daemon" {
                    format!(
                        "Read-only Runtime job fixture completed. Selected model: {}",
                        selected_model.as_deref().unwrap_or("provider-default")
                    )
                } else {
                    "Read-only Runtime job fixture completed.".to_owned()
                };
                send(
                    json!({"id":id,"result":{"turn":{"id":turn,"status":"inProgress","items":[]}}}),
                );
                let call_id = format!("agent-job-call-{turns}");
                let request_id = 1500 + turns;
                // Exercise the real Runtime dynamic callback with one strictly
                // read-only static capability query. No social/account request.
                send(json!({"id":request_id,"method":"item/tool/call","params":{
                    "threadId":current,"turnId":turn,"callId":call_id,
                    "tool":"runtime_modules","namespace":null,"arguments":{}
                }}));
                let reply: Value = serde_json::from_str(
                    &lines.next().expect("job tool response missing").unwrap(),
                )
                .unwrap();
                assert_eq!(reply["id"], request_id);
                assert_eq!(
                    reply.pointer("/result/success").and_then(Value::as_bool),
                    Some(true)
                );
                assert_eq!(
                    reply
                        .pointer("/result/contentItems/0/type")
                        .and_then(Value::as_str),
                    Some("inputText")
                );
                let text = reply
                    .pointer("/result/contentItems/0/text")
                    .and_then(Value::as_str)
                    .expect("job tool result text missing");
                let modules: Value = serde_json::from_str(text)
                    .expect("real runtime_modules result must be structured JSON");
                assert!(
                    modules["modules"]
                        .as_array()
                        .is_some_and(|modules| !modules.is_empty())
                );
                if scenario == "agent_job_mind_writer" {
                    let memory_id = 2500 + turns;
                    send(json!({"id":memory_id,"method":"item/tool/call","params":{
                        "threadId":current,"turnId":turn,"callId":format!("agent-job-memory-call-{turns}"),
                        "tool":"runtime_memory","namespace":null,"arguments":{
                            "action":"remember","key":"from-writer-job","text":"MIND_WRITER_SHARED_FACT_MARKER",
                            "tags":["mind"],"expected_revision":0
                        }
                    }}));
                    let reply: Value = serde_json::from_str(
                        &lines.next().expect("memory result missing").unwrap(),
                    )
                    .unwrap();
                    assert_eq!(reply["id"], memory_id);
                    assert_eq!(reply["result"]["success"], true);
                    let value: Value = serde_json::from_str(
                        reply["result"]["contentItems"][0]["text"].as_str().unwrap(),
                    )
                    .unwrap();
                    assert_eq!(value["result"]["memory"]["persona"], "shared");
                    assert_eq!(
                        value["result"]["memory"]["source"],
                        json!({"kind":"agent","agent":"writer","job":"mind-writer-b"})
                    );
                }
                if scenario == "agent_job_interrupt" {
                    // EOF occurs only after Runtime actually handled the tool.
                    // No turn/completed was emitted; the durable host must mark
                    // this job Interrupted and must not replay it automatically.
                    return;
                }
                send(
                    json!({"method":"item/completed","params":{"threadId":current,"turnId":turn,"item":{
                        "id":format!("agent-job-answer-{turns}"),"type":"agentMessage",
                        "text":answer,"phase":"final_answer"
                    }}}),
                );
                send(
                    json!({"method":"turn/completed","params":{"threadId":current,"turn":{
                        "id":turn,"status":"completed","items":[]
                    }}}),
                );
            }
            _ => send(
                json!({"id":id,"error":{"code":-32601,"message":"unsupported agent job fixture method"}}),
            ),
        }
    }
}
fn main() {
    let args = std::env::args().skip(1).collect::<Vec<_>>();
    // The mixed-profile daemon uses the same startup arguments as native Codex.
    // Keep this a compiled ELF peer; production's native executable guard stays on.
    let scenario = if args.last().is_some_and(|v| v == "app-server")
        && args.iter().any(|v| v.starts_with("model_catalog_json="))
    {
        "agent_job_daemon".into()
    } else {
        args.get(1).cloned().unwrap_or_else(|| "normal".into())
    };
    if matches!(
        scenario.as_str(),
        "agent_job" | "agent_job_daemon" | "agent_job_interrupt" | "agent_job_unauthenticated"
    ) || scenario.starts_with("agent_job_mind")
    {
        agent_job(&scenario);
        return;
    }
    let mut lines = io::stdin().lock().lines();
    let mut thread_started = false;
    while let Some(Ok(line)) = lines.next() {
        let request: Value = serde_json::from_str(&line).unwrap();
        let id = request.get("id").cloned().unwrap_or(Value::Null);
        let method = request.get("method").and_then(Value::as_str).unwrap_or("");
        match method {
            "initialize" => {
                match scenario.as_str() {
                    "hang" => {
                        std::thread::sleep(std::time::Duration::from_secs(30));
                        return;
                    }
                    "huge" => {
                        print!("{}", "x".repeat(100_000));
                        io::stdout().flush().unwrap();
                        return;
                    }
                    "invalid" => {
                        println!("not-json token-should-never-leak");
                        return;
                    }
                    "truncated" => {
                        print!("{{\"secret\":\"token-should-never-leak");
                        return;
                    }
                    "stderr" => {
                        eprint!("{}", "token-should-never-leak".repeat(10000));
                        return;
                    }
                    "exit" => return,
                    "rpc-error" => {
                        send(
                            json!({"id":id,"error":{"code":-1,"message":"token-should-never-leak"}}),
                        );
                        continue;
                    }
                    "mis-id" => {
                        send(json!({"id":99999,"result":{}}));
                        continue;
                    }
                    _ => (),
                }
                assert!(request.pointer("/params/clientInfo/name").is_some());
                send(
                    json!({"id":id,"result":{"userAgent":"native-fixture","platformFamily":"unix","platformOs":"linux"}}),
                );
            }
            "initialized" => (),
            "account/read" => {
                assert_eq!(
                    request
                        .pointer("/params/refreshToken")
                        .and_then(Value::as_bool),
                    Some(false)
                );
                let account = if scenario == "unauthenticated" {
                    Value::Null
                } else {
                    json!({"type":if scenario=="api-key" {"apiKey"} else {"chatgpt"},"planType":"pro","email":"private@example.invalid","access_token":"token-should-never-leak"})
                };
                send(json!({"id":id,"result":{"account":account,"requiresOpenaiAuth":true}}));
                if scenario == "blocked-stdin" && thread_started {
                    std::thread::sleep(std::time::Duration::from_secs(30));
                    return;
                }
            }
            "thread/start" | "thread/resume" => {
                assert_eq!(
                    request.pointer("/params/sandbox").and_then(Value::as_str),
                    Some("read-only")
                );
                assert_eq!(
                    request
                        .pointer("/params/approvalPolicy")
                        .and_then(Value::as_str),
                    Some("untrusted")
                );
                assert_eq!(
                    request
                        .pointer("/params/modelProvider")
                        .and_then(Value::as_str),
                    Some("openai")
                );
                if scenario.starts_with("tool-") && method == "thread/start" {
                    assert_eq!(
                        request
                            .pointer("/params/dynamicTools/0/type")
                            .and_then(Value::as_str),
                        Some("function")
                    );
                    assert_eq!(
                        request
                            .pointer("/params/dynamicTools/0/name")
                            .and_then(Value::as_str),
                        Some("runtime_read")
                    );
                }
                let thread = request
                    .pointer("/params/threadId")
                    .and_then(Value::as_str)
                    .unwrap_or("thread-fixture");
                send(json!({"method":"thread/started","params":{"thread":{"id":thread}}}));
                send(json!({"id":id,"result":{"thread":{"id":thread}}}));
                thread_started = true;
            }
            "turn/start" => {
                assert_eq!(
                    request
                        .pointer("/params/sandboxPolicy/type")
                        .and_then(Value::as_str),
                    Some("readOnly")
                );
                assert_eq!(
                    request.pointer("/params/sandboxPolicy/networkAccess"),
                    Some(&json!(false))
                );
                if scenario == "image" {
                    assert_eq!(
                        request
                            .pointer("/params/input/1/type")
                            .and_then(Value::as_str),
                        Some("localImage")
                    );
                }
                for (request_id, method, expected) in [
                    (
                        901,
                        "item/commandExecution/requestApproval",
                        json!({"decision":"decline"}),
                    ),
                    (
                        902,
                        "item/fileChange/requestApproval",
                        json!({"decision":"decline"}),
                    ),
                    (
                        903,
                        "item/permissions/requestApproval",
                        json!({"permissions":{},"scope":"turn"}),
                    ),
                ] {
                    send(
                        json!({"id":request_id,"method":method,"params":{"threadId":"thread-fixture","turnId":"turn-fixture"}}),
                    );
                    let reply: Value =
                        serde_json::from_str(&lines.next().unwrap().unwrap()).unwrap();
                    assert_eq!(reply["id"], request_id);
                    assert_eq!(reply["result"], expected);
                }
                // Before the response on purpose: the client must preserve early events.
                send(
                    json!({"method":"item/agentMessage/delta","params":{"threadId":"thread-fixture","turnId":"turn-fixture","itemId":"answer-fixture","delta":"streamed draft"}}),
                );
                send(
                    json!({"id":id,"result":{"turn":{"id":"turn-fixture","status":"inProgress","items":[]}}}),
                );
                if scenario.starts_with("tool-") {
                    for attempt in 0..if scenario == "tool-repeat" { 2 } else { 1 } {
                        send(json!({"id":950+attempt,"method":"item/tool/call","params":{
                            "threadId":if scenario=="tool-wrong-thread" {"foreign-thread"} else {"thread-fixture"},
                            "turnId":"turn-fixture","callId":"call-fixture","tool":if scenario=="tool-unknown" {"not_registered"} else {"runtime_read"},
                            "namespace":null,"arguments":{"resource_id":"synthetic"}
                        }}));
                        let reply: Value =
                            serde_json::from_str(&lines.next().unwrap().unwrap()).unwrap();
                        assert_eq!(reply["id"], 950 + attempt);
                        if scenario == "tool-unknown"
                            || scenario == "tool-wrong-thread"
                            || (scenario == "tool-repeat" && attempt == 1)
                            || scenario == "tool-error"
                        {
                            assert_eq!(
                                reply.pointer("/result/success").and_then(Value::as_bool),
                                Some(false)
                            );
                            assert!(!reply.to_string().contains("token-should"));
                        } else if scenario == "tool-image" {
                            assert_eq!(
                                reply
                                    .pointer("/result/contentItems/0/type")
                                    .and_then(Value::as_str),
                                Some("inputImage")
                            );
                            assert_eq!(
                                reply
                                    .pointer("/result/contentItems/0/imageUrl")
                                    .and_then(Value::as_str),
                                Some("data:image/jpeg;base64,/9j/2Q==")
                            );
                        } else {
                            assert_eq!(
                                reply.pointer("/result/success").and_then(Value::as_bool),
                                Some(true)
                            );
                            assert_eq!(
                                reply
                                    .pointer("/result/contentItems/0/text")
                                    .and_then(Value::as_str),
                                Some("actual synthetic resource bytes")
                            );
                        }
                    }
                }
                if scenario == "hang-turn" {
                    std::thread::sleep(std::time::Duration::from_secs(30));
                    return;
                }
                // A notification for another thread must not enter the active result.
                send(
                    json!({"method":"item/completed","params":{"threadId":"wrong-thread","turnId":"turn-fixture","item":{"id":"wrong-item","type":"agentMessage","text":"unrelated"}}}),
                );
                if scenario == "failed" {
                    send(
                        json!({"method":"error","params":{"error":{"message":"token-should-never-leak"}}}),
                    );
                }
                let text = if scenario == "large-output" {
                    "a".repeat(10000)
                } else {
                    "authoritative final".into()
                };
                send(
                    json!({"method":"item/completed","params":{"threadId":"thread-fixture","turnId":"turn-fixture","item":{"id":"answer-fixture","type":"agentMessage","text":text,"phase":"final_answer"}}}),
                );
                send(
                    json!({"method":"turn/completed","params":{"threadId":"thread-fixture","turn":{"id":"turn-fixture","status":if scenario=="failed" {"failed"} else {"completed"},"items":[]}}}),
                );
            }
            _ => send(
                json!({"id":id,"error":{"code":-32601,"message":"unsupported fixture method"}}),
            ),
        }
    }
}
