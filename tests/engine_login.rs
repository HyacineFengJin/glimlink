// SPDX-License-Identifier: MIT
#![cfg(all(feature = "agent-host-tests", target_os = "linux"))]
use agent_runtime::operator_engines::EngineLoginManager;
use serde_json::{Value, json};
use std::{
    fs,
    os::unix::fs::PermissionsExt,
    path::PathBuf,
    sync::atomic::{AtomicU64, Ordering},
    time::{Duration, Instant},
};
static NEXT: AtomicU64 = AtomicU64::new(0);
struct Lab(PathBuf);
impl Lab {
    fn new() -> Self {
        let p = std::env::temp_dir().join(format!(
            "operator-engines-{}-{}",
            std::process::id(),
            NEXT.fetch_add(1, Ordering::Relaxed)
        ));
        fs::create_dir(&p).unwrap();
        fs::set_permissions(&p, fs::Permissions::from_mode(0o700)).unwrap();
        Self(p)
    }
    fn manager(&self) -> EngineLoginManager {
        EngineLoginManager::with_binaries(
            &self.0,
            env!("CARGO_BIN_EXE_agent-login-fixture").into(),
            env!("CARGO_BIN_EXE_agent-login-fixture").into(),
        )
        .unwrap()
    }
}
impl Drop for Lab {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}
fn ignore(_: Value) -> runtime_core::Result<()> {
    Ok(())
}
#[test]
fn subscription_device_login_is_native_and_reuses_verified_saved_session() {
    let lab = Lab::new();
    let mut manager = lab.manager();
    assert_eq!(manager.status("codex").unwrap()["authenticated"], false);
    let started = manager.start("codex", "auto", &mut ignore).unwrap();
    assert_eq!(started["status"], "pending");
    assert_eq!(started["method"], "device");
    assert_eq!(started["authorization"]["user_code"], "ABCD-EFGH");
    let completed = manager.poll("codex", None, &mut ignore).unwrap();
    assert_eq!(completed["status"], "authenticated");
    assert!(!completed.to_string().contains("SYNTHETIC_PRIVATE"));
    let reused = manager.start("codex", "auto", &mut ignore).unwrap();
    assert_eq!(reused["reused_session"], true);
    assert!(reused.get("authorization").is_none());
    assert_eq!(
        manager.start("codex", "browser", &mut ignore).unwrap()["error"]["code"],
        "unsupported_login_method"
    );
}
#[test]
fn google_code_stays_private_and_native_completion_is_independently_verified() {
    let lab = Lab::new();
    let mut manager = lab.manager();
    assert_eq!(
        manager.status("antigravity").unwrap()["authenticated"],
        false
    );
    assert_eq!(
        manager
            .start("antigravity", "browser", &mut ignore)
            .unwrap()["status"],
        "pending"
    );
    assert!(manager.start("codex", "device", &mut ignore).is_err());
    let deadline = Instant::now() + Duration::from_secs(3);
    loop {
        let result = manager.poll("antigravity", None, &mut ignore).unwrap();
        if result.get("authorization").is_some() {
            assert_eq!(result["input_required"]["kind"], "authorization_code");
            break;
        }
        assert!(Instant::now() < deadline);
        std::thread::sleep(Duration::from_millis(5));
    }
    assert!(
        manager
            .poll(
                "antigravity",
                Some(br#"{"wrong":"synthetic"}"#),
                &mut ignore
            )
            .is_err()
    );
    let input = serde_json::to_vec(&json!({"authorization_code":"synthetic-once-code"})).unwrap();
    let initial = manager
        .poll("antigravity", Some(&input), &mut ignore)
        .unwrap();
    assert!(!initial.to_string().contains("synthetic-once-code"));
    let mut result = initial;
    while result["status"] == "pending" {
        assert!(Instant::now() < deadline);
        std::thread::sleep(Duration::from_millis(5));
        result = manager.poll("antigravity", None, &mut ignore).unwrap();
    }
    assert_eq!(result["status"], "authenticated");
    assert!(!result.to_string().contains("SYNTHETIC_PRIVATE"));
    assert_eq!(
        manager.start("antigravity", "auto", &mut ignore).unwrap()["reused_session"],
        true
    );
}
#[test]
fn operator_disconnect_and_cancel_do_not_remove_saved_login() {
    let lab = Lab::new();
    let mut manager = lab.manager();
    manager.start("antigravity", "auto", &mut ignore).unwrap();
    let deadline = Instant::now() + Duration::from_secs(3);
    while !lab.0.join("antigravity/synthetic-login-pid").exists() {
        manager.poll("antigravity", None, &mut ignore).unwrap();
        assert!(Instant::now() < deadline);
        std::thread::sleep(Duration::from_millis(5));
    }
    let pid: i32 = fs::read_to_string(lab.0.join("antigravity/synthetic-login-pid"))
        .unwrap()
        .parse()
        .unwrap();
    assert_eq!(
        manager.cancel("antigravity").unwrap()["status"],
        "cancelled"
    );
    assert_eq!(unsafe { libc::kill(pid, 0) }, -1);
    assert_eq!(
        manager.status("antigravity").unwrap()["authenticated"],
        false
    );
    manager.start("antigravity", "auto", &mut ignore).unwrap();
    drop(manager);
    let mut manager = lab.manager();
    assert_eq!(
        manager.status("antigravity").unwrap()["authenticated"],
        false
    );
}

#[test]
fn native_token_exchange_failure_is_terminal_without_private_details() {
    let lab = Lab::new();
    let mut manager = lab.manager();
    manager
        .start("antigravity", "browser", &mut ignore)
        .unwrap();
    let deadline = Instant::now() + Duration::from_secs(3);
    loop {
        let result = manager.poll("antigravity", None, &mut ignore).unwrap();
        if result.get("authorization").is_some() {
            break;
        }
        assert!(Instant::now() < deadline);
        std::thread::sleep(Duration::from_millis(5));
    }
    let input =
        serde_json::to_vec(&json!({"authorization_code":"synthetic-rejected-code"})).unwrap();
    let mut result = manager
        .poll("antigravity", Some(&input), &mut ignore)
        .unwrap();
    while result["status"] == "pending" {
        assert!(Instant::now() < deadline);
        std::thread::sleep(Duration::from_millis(5));
        result = manager.poll("antigravity", None, &mut ignore).unwrap();
    }
    assert_eq!(result["status"], "failed");
    assert_eq!(result["error"]["code"], "native_login_failed");
    assert!(!result.to_string().contains("SYNTHETIC_PRIVATE"));
    assert!(!manager.is_pending("antigravity"));
}
