#![cfg(feature = "test-helper")]
use runtime_engine::{
    CodexClient, CodexConfig, DynamicToolSpec, EngineError, EngineEvent, ErrorKind, Input,
    ToolContent, ToolResult, TurnStatus,
};
#[cfg(unix)]
use std::os::unix::fs::PermissionsExt;
use std::{
    fs,
    path::PathBuf,
    sync::atomic::{AtomicU64, Ordering},
    time::{Duration, Instant},
};
static COUNTER: AtomicU64 = AtomicU64::new(0);
struct Lab {
    root: PathBuf,
}
impl Lab {
    fn new() -> Self {
        let root = std::env::temp_dir().join(format!(
            "engine-fixture-{}-{}",
            std::process::id(),
            COUNTER.fetch_add(1, Ordering::Relaxed)
        ));
        fs::create_dir(&root).unwrap();
        fs::set_permissions(&root, fs::Permissions::from_mode(0o700)).unwrap();
        fs::create_dir(root.join("workspace")).unwrap();
        Self { root }
    }
    fn config(&self, scenario: &str) -> CodexConfig {
        let mut config = CodexConfig::new(
            env!("CARGO_BIN_EXE_engine-fake-server"),
            self.root.join("home"),
            self.root.join("workspace"),
        );
        config.args = vec!["app-server".into(), scenario.into()];
        config.timeout = Duration::from_secs(2);
        config
    }
    fn client(&self, scenario: &str) -> CodexClient {
        CodexClient::spawn(self.config(scenario)).unwrap()
    }
}
impl Drop for Lab {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.root);
    }
}

#[test]
fn handshake_account_projection_resume_and_authoritative_final() {
    let lab = Lab::new();
    let mut client = lab.client("normal");
    assert_eq!(client.account_read().unwrap_err().kind, ErrorKind::State);
    assert_eq!(
        client.initialize().unwrap().user_agent.as_deref(),
        Some("native-fixture")
    );
    let account = client.account_read().unwrap();
    assert!(account.authenticated);
    assert_eq!(account.auth_kind.as_deref(), Some("chatgpt"));
    let serialized = serde_json::to_string(&account).unwrap();
    assert!(!serialized.contains("private@example") && !serialized.contains("token-should"));
    let thread = client.thread_start(None).unwrap();
    let mut events = Vec::new();
    let output = client
        .turn_start(
            &thread.id,
            &[Input::Text {
                text: "synthetic prompt".into(),
            }],
            |e| events.push(e),
        )
        .unwrap();
    assert_eq!(output.status, TurnStatus::Completed);
    assert_eq!(output.text, "authoritative final");
    assert_eq!(output.messages.len(), 1);
    assert_eq!(
        events
            .iter()
            .filter(|e| matches!(e, EngineEvent::ApprovalDenied { .. }))
            .count(),
        3
    );
    assert!(
        events
            .iter()
            .any(|e| matches!(e, EngineEvent::Delta { .. }))
    );
    client.shutdown();
    let mut restored = lab.client("normal");
    restored.initialize().unwrap();
    assert_eq!(restored.thread_resume(&thread.id).unwrap().id, thread.id);
}

#[test]
fn local_image_is_a_real_local_image_block_and_audio_video_are_rejected() {
    let lab = Lab::new();
    let image = lab.root.join("workspace/image.png");
    fs::write(
        &image,
        b"synthetic fixture: media decoder validates actual formats upstream",
    )
    .unwrap();
    let mut client = lab.client("image");
    client.initialize().unwrap();
    let thread = client.thread_start(None).unwrap();
    for input in [
        Input::Audio {
            path: image.clone(),
        },
        Input::Video {
            path: image.clone(),
        },
    ] {
        assert_eq!(
            client
                .turn_start(&thread.id, &[input], |_| {})
                .unwrap_err()
                .kind,
            ErrorKind::Unsupported
        );
    }
    let output = client
        .turn_start(
            &thread.id,
            &[
                Input::Text {
                    text: "inspect image".into(),
                },
                Input::LocalImage { path: image },
            ],
            |_| {},
        )
        .unwrap();
    assert_eq!(output.status, TurnStatus::Completed);
    let outside = lab.root.join("private.png");
    fs::write(&outside, b"outside").unwrap();
    assert_eq!(
        client
            .turn_start(&thread.id, &[Input::LocalImage { path: outside }], |_| {})
            .unwrap_err()
            .kind,
        ErrorKind::InvalidInput
    );
}

#[test]
fn failures_and_interrupted_streams_never_become_success() {
    let lab = Lab::new();
    let mut client = lab.client("failed");
    client.initialize().unwrap();
    let thread = client.thread_start(None).unwrap();
    let output = client
        .turn_start(
            &thread.id,
            &[Input::Text {
                text: "fixture".into(),
            }],
            |_| {},
        )
        .unwrap();
    assert_eq!(output.status, TurnStatus::Failed);
    assert!(
        !serde_json::to_string(&output)
            .unwrap()
            .contains("token-should")
    );
    // The next native client uses the same credential home. Release the first
    // process explicitly; login/model clients must never mutate it concurrently.
    client.shutdown();
    let mut config = lab.config("hang-turn");
    config.timeout = Duration::from_millis(300);
    let mut client = CodexClient::spawn(config).unwrap();
    client.initialize().unwrap();
    let thread = client.thread_start(None).unwrap();
    let start = Instant::now();
    assert_eq!(
        client
            .turn_start(
                &thread.id,
                &[Input::Text {
                    text: "fixture".into()
                }],
                |_| {}
            )
            .unwrap_err()
            .kind,
        ErrorKind::Timeout
    );
    assert!(start.elapsed() < Duration::from_secs(3));
    assert_eq!(client.account_read().unwrap_err().kind, ErrorKind::State);
}

#[test]
fn malformed_rpc_eof_timeouts_and_remote_errors_are_bounded_and_redacted() {
    for (scenario, kind) in [
        ("invalid", ErrorKind::Protocol),
        ("truncated", ErrorKind::Protocol),
        ("exit", ErrorKind::Closed),
        ("rpc-error", ErrorKind::Remote),
        ("mis-id", ErrorKind::Protocol),
        ("hang", ErrorKind::Timeout),
    ] {
        let lab = Lab::new();
        let mut config = lab.config(scenario);
        config.timeout = Duration::from_millis(300);
        let mut client = CodexClient::spawn(config).unwrap();
        let error = client.initialize().unwrap_err();
        assert_eq!(error.kind, kind, "{scenario}");
        assert!(!error.detail.contains("token-should"));
    }
}

#[test]
fn stdout_stderr_and_answer_limits_are_enforced() {
    for scenario in ["huge", "stderr"] {
        let lab = Lab::new();
        let mut config = lab.config(scenario);
        config.max_jsonl_bytes = 1024;
        config.max_input_bytes = 256;
        config.max_wire_bytes = 4096;
        let mut client = CodexClient::spawn(config).unwrap();
        let error = client.initialize().unwrap_err();
        assert!(matches!(error.kind, ErrorKind::Limit | ErrorKind::Closed));
        assert!(!error.detail.contains("token-should"));
    }
    let lab = Lab::new();
    let mut config = lab.config("large-output");
    config.max_output_bytes = 64;
    let mut client = CodexClient::spawn(config).unwrap();
    client.initialize().unwrap();
    let thread = client.thread_start(None).unwrap();
    assert_eq!(
        client
            .turn_start(
                &thread.id,
                &[Input::Text {
                    text: "fixture".into()
                }],
                |_| {}
            )
            .unwrap_err()
            .kind,
        ErrorKind::Limit
    );
}

#[test]
fn blocked_stdin_is_also_timed_out_and_reaped() {
    let lab = Lab::new();
    let mut config = lab.config("blocked-stdin");
    config.timeout = Duration::from_millis(300);
    let mut client = CodexClient::spawn(config).unwrap();
    client.initialize().unwrap();
    let thread = client.thread_start(None).unwrap();
    let start = Instant::now();
    assert_eq!(
        client
            .turn_start(
                &thread.id,
                &[Input::Text {
                    text: "a".repeat(100000)
                }],
                |_| {}
            )
            .unwrap_err()
            .kind,
        ErrorKind::Timeout
    );
    assert!(start.elapsed() < Duration::from_secs(3));
}

#[test]
fn credential_home_cannot_be_part_of_model_workspace() {
    let lab = Lab::new();
    let mut config = lab.config("normal");
    config.codex_home = lab.root.join("workspace/credentials");
    assert!(matches!(CodexClient::spawn(config),Err(error) if error.kind==ErrorKind::InvalidInput));
}

#[test]
fn model_turn_requires_chatgpt_plan_login_and_cannot_silently_use_api_key() {
    for scenario in ["unauthenticated", "api-key"] {
        let lab = Lab::new();
        let mut client = lab.client(scenario);
        client.initialize().unwrap();
        let thread = client.thread_start(None).unwrap();
        assert_eq!(
            client
                .turn_start(
                    &thread.id,
                    &[Input::Text {
                        text: "fixture".into()
                    }],
                    |_| {}
                )
                .unwrap_err()
                .kind,
            ErrorKind::State
        );
    }
}

fn tools() -> Vec<DynamicToolSpec> {
    vec![DynamicToolSpec {
        name: "runtime_read".into(),
        description: "Read a bounded runtime resource".into(),
        input_schema: serde_json::json!({"type":"object","properties":{"resource_id":{"type":"string"}},"required":["resource_id"],"additionalProperties":false}),
    }]
}

#[test]
fn dynamic_tool_reads_and_actual_image_payloads_use_callback_protocol() {
    for scenario in [
        "tool-text",
        "tool-image",
        "tool-error",
        "tool-repeat",
        "tool-unknown",
        "tool-wrong-thread",
    ] {
        let lab = Lab::new();
        let mut client = lab.client(scenario);
        client.initialize().unwrap();
        let thread = client.thread_start_with_tools(None, &tools()).unwrap();
        let mut calls = 0;
        let output = client
            .turn_start_with_tools(
                &thread.id,
                &[Input::Text {
                    text: "read the resource".into(),
                }],
                |_| {},
                |call| {
                    calls += 1;
                    assert_eq!(call.tool, "runtime_read");
                    assert_eq!(call.call_id, "call-fixture");
                    assert_eq!(call.arguments["resource_id"], "synthetic");
                    if scenario == "tool-error" {
                        return Err(EngineError {
                            kind: ErrorKind::Io,
                            detail: "token-should-never-leak".into(),
                        });
                    }
                    if scenario == "tool-image" {
                        return Ok(ToolResult {
                            content_items: vec![ToolContent::InputImage {
                                image_url: "data:image/jpeg;base64,/9j/2Q==".into(),
                            }],
                            success: true,
                        });
                    }
                    Ok(ToolResult::text("actual synthetic resource bytes", true))
                },
            )
            .unwrap();
        assert_eq!(output.status, TurnStatus::Completed, "{scenario}");
        assert_eq!(
            calls,
            if scenario == "tool-unknown" || scenario == "tool-wrong-thread" {
                0
            } else {
                1
            },
            "{scenario}"
        );
    }
}

#[test]
fn dynamic_tool_oversize_and_non_image_data_fail_before_result_transmission() {
    for image_url in [
        None,
        Some("https://example.invalid/image.jpg"),
        Some("data:audio/wav;base64,AAAA"),
        Some("data:image/jpeg;base64,bm90YW5pbWFnZQ=="),
    ] {
        let lab = Lab::new();
        let mut config = lab.config("tool-text");
        config.max_tool_result_bytes = 1024;
        config.max_tool_image_bytes = 512;
        let mut client = CodexClient::spawn(config).unwrap();
        client.initialize().unwrap();
        let thread = client.thread_start_with_tools(None, &tools()).unwrap();
        let error = client
            .turn_start_with_tools(
                &thread.id,
                &[Input::Text {
                    text: "fixture".into(),
                }],
                |_| {},
                |_| {
                    Ok(match image_url {
                        None => ToolResult::text("a".repeat(1025), true),
                        Some(url) => ToolResult {
                            content_items: vec![ToolContent::InputImage {
                                image_url: url.into(),
                            }],
                            success: true,
                        },
                    })
                },
            )
            .unwrap_err();
        assert!(matches!(
            error.kind,
            ErrorKind::Limit | ErrorKind::InvalidInput | ErrorKind::Unsupported
        ));
    }
}
