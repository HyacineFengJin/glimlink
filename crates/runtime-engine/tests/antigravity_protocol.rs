#![cfg(feature = "test-helper")]
use runtime_engine::{
    AntigravityClient, AntigravityConfig, EngineEvent, ErrorKind, Input, TurnStatus,
};
use std::{
    fs,
    os::unix::fs::{DirBuilderExt, PermissionsExt},
    path::PathBuf,
    sync::atomic::{AtomicU64, Ordering},
    time::{Duration, Instant},
};
static NEXT: AtomicU64 = AtomicU64::new(0);
struct Lab(PathBuf);
impl Lab {
    fn new() -> Self {
        let p = std::env::temp_dir().join(format!(
            "agy-acp-{}-{}",
            std::process::id(),
            NEXT.fetch_add(1, Ordering::Relaxed)
        ));
        fs::create_dir(&p).unwrap();
        fs::set_permissions(&p, fs::Permissions::from_mode(0o700)).unwrap();
        fs::DirBuilder::new()
            .mode(0o700)
            .create(p.join("workspace"))
            .unwrap();
        Self(p)
    }
    fn config(&self, scenario: &str) -> AntigravityConfig {
        let binary = env!("CARGO_BIN_EXE_antigravity-fake-server");
        let mut config =
            AntigravityConfig::new(binary, self.0.join("home"), self.0.join("workspace"));
        config.bridge_binary = PathBuf::from(binary);
        config.bridge_socket = self.0.join("bridge.sock");
        config.developer_instructions = Some("OPERATOR_PERSONA_MARKER".into());
        config.args = vec![scenario.into()];
        config.timeout = Duration::from_secs(2);
        config
    }
    fn client(&self, scenario: &str) -> AntigravityClient {
        AntigravityClient::spawn(self.config(scenario)).unwrap()
    }
}
impl Drop for Lab {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}
fn input() -> Vec<Input> {
    vec![Input::Text {
        text: "synthetic explicit input".into(),
    }]
}

#[test]
fn subscription_session_persona_mcp_model_and_answer_cross_real_native_wire() {
    let lab = Lab::new();
    let mut config = lab.config("normal");
    config.model = Some("fixture-model".into());
    let mut client = AntigravityClient::spawn(config).unwrap();
    let info = client.initialize().unwrap();
    assert_eq!(info["protocolVersion"], 1);
    assert!(!info.to_string().contains("PRIVATE_"));
    let mut pumps = 0;
    let mut pump = || {
        pumps += 1;
        Ok(())
    };
    let session = client.new_session(&mut pump).unwrap();
    let expected = client.next_turn_id();
    let mut events = Vec::new();
    let output = client
        .prompt(&session.id, &input(), |event| events.push(event), &mut pump)
        .unwrap();
    assert_eq!(output.status, TurnStatus::Completed);
    assert_eq!(output.text, "actual answer");
    assert_eq!(output.turn_id, expected);
    assert_eq!(output.messages.len(), 1);
    assert!(!serde_json::to_string(&output).unwrap().contains("PRIVATE_"));
    assert_eq!(
        events
            .iter()
            .filter(|e| matches!(e, EngineEvent::Delta { .. }))
            .count(),
        2
    );
    assert!(pumps > 2);
    assert_ne!(client.next_turn_id(), expected);
}

#[test]
fn no_auth_never_calls_authenticate_or_starts_a_prompt_and_raw_error_stays_private() {
    let lab = Lab::new();
    let mut client = lab.client("needs-auth");
    client.initialize().unwrap();
    let error = client.new_session(&mut || Ok(())).unwrap_err();
    assert_eq!(error.kind, ErrorKind::Authentication);
    assert!(!error.to_string().contains("PRIVATE_"));
    assert_eq!(
        client.new_session(&mut || Ok(())).unwrap_err().kind,
        ErrorKind::State
    );
}

#[test]
fn missing_required_subscription_or_persona_capability_is_not_silently_accepted() {
    for scenario in ["missing-personal", "missing-persona"] {
        let lab = Lab::new();
        let mut client = lab.client(scenario);
        assert_eq!(
            client.initialize().unwrap_err().kind,
            ErrorKind::Unsupported
        );
    }
}

#[test]
fn client_permissions_filesystem_and_terminal_requests_are_all_denied() {
    let lab = Lab::new();
    let mut client = lab.client("client-requests");
    client.initialize().unwrap();
    let session = client.new_session(&mut || Ok(())).unwrap();
    let mut events = Vec::new();
    let result = client
        .prompt(&session.id, &input(), |e| events.push(e), &mut || Ok(()))
        .unwrap();
    assert_eq!(result.text, "actual answer");
    assert_eq!(
        events
            .iter()
            .filter(|e| matches!(e, EngineEvent::ApprovalDenied { .. }))
            .count(),
        1
    );
}

#[test]
fn unsupported_media_and_unknown_sessions_are_rejected_before_the_request() {
    let lab = Lab::new();
    let mut client = lab.client("normal");
    client.initialize().unwrap();
    let session = client.new_session(&mut || Ok(())).unwrap();
    for item in [
        Input::LocalImage {
            path: lab.0.join("image"),
        },
        Input::Audio {
            path: lab.0.join("audio"),
        },
        Input::Video {
            path: lab.0.join("video"),
        },
    ] {
        assert_eq!(
            client
                .prompt(&session.id, &[item], |_| {}, &mut || Ok(()))
                .unwrap_err()
                .kind,
            ErrorKind::Unsupported
        );
    }
    assert_eq!(
        client
            .prompt("unknown", &input(), |_| {}, &mut || Ok(()))
            .unwrap_err()
            .kind,
        ErrorKind::State
    );
    assert_eq!(
        client
            .prompt(&session.id, &input(), |_| {}, &mut || Ok(()))
            .unwrap()
            .text,
        "actual answer"
    );
}

#[test]
fn cancellation_limits_and_cross_session_or_response_corruption_never_complete_successfully() {
    for (scenario, kind) in [
        ("wrong-session", ErrorKind::Protocol),
        ("wrong-response", ErrorKind::Protocol),
        ("unknown-stop", ErrorKind::Protocol),
        ("remote-error", ErrorKind::Remote),
    ] {
        let lab = Lab::new();
        let mut client = lab.client(scenario);
        client.initialize().unwrap();
        let session = client.new_session(&mut || Ok(())).unwrap();
        let error = client
            .prompt(&session.id, &input(), |_| {}, &mut || Ok(()))
            .unwrap_err();
        assert_eq!(error.kind, kind);
        assert!(!error.to_string().contains("PRIVATE_"));
    }
    for (scenario, status) in [
        ("cancelled", TurnStatus::Interrupted),
        ("max-tokens", TurnStatus::Failed),
    ] {
        let lab = Lab::new();
        let mut client = lab.client(scenario);
        client.initialize().unwrap();
        let session = client.new_session(&mut || Ok(())).unwrap();
        assert_eq!(
            client
                .prompt(&session.id, &input(), |_| {}, &mut || Ok(()))
                .unwrap()
                .status,
            status
        );
    }
}

#[test]
fn output_and_wire_budgets_are_distinct_and_timeout_keeps_pumping() {
    for (scenario, output, wire) in [
        ("oversized-output", 128, 128 * 1024),
        ("wire-limit", 128 * 1024, 64 * 1024),
    ] {
        let lab = Lab::new();
        let mut config = lab.config(scenario);
        config.max_output_bytes = output;
        config.max_wire_bytes = wire;
        config.max_jsonl_bytes = 32 * 1024;
        config.max_input_bytes = 8192;
        let mut client = AntigravityClient::spawn(config).unwrap();
        client.initialize().unwrap();
        let session = client.new_session(&mut || Ok(())).unwrap();
        assert_eq!(
            client
                .prompt(&session.id, &input(), |_| {}, &mut || Ok(()))
                .unwrap_err()
                .kind,
            ErrorKind::Limit
        );
    }
    let lab = Lab::new();
    let mut config = lab.config("hang");
    config.timeout = Duration::from_millis(180);
    let mut client = AntigravityClient::spawn(config).unwrap();
    client.initialize().unwrap();
    let session = client.new_session(&mut || Ok(())).unwrap();
    let mut pumps = 0;
    let begin = Instant::now();
    let error = client
        .prompt(&session.id, &input(), |_| {}, &mut || {
            pumps += 1;
            Ok(())
        })
        .unwrap_err();
    assert_eq!(error.kind, ErrorKind::Timeout);
    assert!(pumps >= 3);
    assert!(begin.elapsed() < Duration::from_secs(2));
}

#[test]
fn reject_scripts_broad_credential_directories_and_overlapping_workspace() {
    let lab = Lab::new();
    let script = lab.0.join("launcher");
    fs::write(&script, b"#!/bin/sh\nexit 0\n").unwrap();
    let mut config = lab.config("normal");
    config.binary = script;
    assert!(matches!(AntigravityClient::spawn(config),Err(e)if e.kind==ErrorKind::Unsupported));
    let mut config = lab.config("normal");
    config.home = lab.0.join("workspace");
    fs::set_permissions(&config.home, fs::Permissions::from_mode(0o700)).unwrap();
    assert!(matches!(AntigravityClient::spawn(config),Err(e)if e.kind==ErrorKind::InvalidInput));
    let config = lab.config("normal");
    fs::create_dir(&config.home).unwrap();
    fs::set_permissions(&config.home, fs::Permissions::from_mode(0o755)).unwrap();
    assert!(matches!(AntigravityClient::spawn(config),Err(e)if e.kind==ErrorKind::InvalidInput));
}

#[test]
fn managed_settings_are_private_and_do_not_follow_symlinks_or_invalid_existing_files() {
    for case in ["symlink", "public", "invalid"] {
        let lab = Lab::new();
        let config = lab.config("normal");
        let dir = config.home.join(".gemini/antigravity-cli");
        fs::DirBuilder::new()
            .recursive(true)
            .mode(0o700)
            .create(&dir)
            .unwrap();
        let path = dir.join("settings.json");
        let other = lab.0.join("must-not-touch");
        fs::write(&other, b"preserve synthetic data").unwrap();
        if case == "symlink" {
            std::os::unix::fs::symlink(&other, &path).unwrap();
        } else {
            fs::write(
                &path,
                if case == "invalid" {
                    b"invalid JSON".as_slice()
                } else {
                    b"{}".as_slice()
                },
            )
            .unwrap();
            fs::set_permissions(
                &path,
                fs::Permissions::from_mode(if case == "public" { 0o644 } else { 0o600 }),
            )
            .unwrap();
        }
        assert!(
            matches!(AntigravityClient::spawn(config),Err(e)if e.kind==ErrorKind::InvalidInput)
        );
        assert_eq!(fs::read(&other).unwrap(), b"preserve synthetic data");
    }
    let lab = Lab::new();
    let config = lab.config("normal");
    let settings = config.home.join(".gemini/antigravity-cli/settings.json");
    let client = AntigravityClient::spawn(config.clone()).unwrap();
    drop(client);
    assert_eq!(
        fs::metadata(&settings).unwrap().permissions().mode() & 0o077,
        0
    );
    let bytes = fs::read(&settings).unwrap();
    let metadata = fs::metadata(&settings).unwrap();
    let client = AntigravityClient::spawn(config).unwrap();
    drop(client);
    assert_eq!(fs::read(&settings).unwrap(), bytes);
    assert_eq!(
        fs::metadata(&settings).unwrap().modified().unwrap(),
        metadata.modified().unwrap()
    );
}
