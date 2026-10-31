// SPDX-License-Identifier: MIT
#![cfg(all(feature = "test-helper", target_os = "linux"))]
use runtime_engine::{
    CodexClient, CodexConfig, ErrorKind,
    codex::LoginProgress,
    login_pty::{AntigravityLogin, LoginPtyProgress},
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
            "engine-login-{}-{}",
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
    fn codex(&self, scenario: &str) -> CodexConfig {
        let mut c = CodexConfig::new(
            env!("CARGO_BIN_EXE_engine-login-fixture"),
            self.0.join("home"),
            self.0.join("workspace"),
        );
        c.args = vec![scenario.into()];
        c.timeout = Duration::from_secs(2);
        c
    }
    fn pty(&self) -> AntigravityLogin {
        AntigravityLogin::spawn(
            std::path::Path::new(env!("CARGO_BIN_EXE_engine-login-fixture")),
            &self.0.join("home"),
            &self.0.join("workspace"),
            Duration::from_secs(3),
        )
        .unwrap()
    }
}
impl Drop for Lab {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}
#[test]
fn codex_native_device_flow_and_early_completion_are_correlated() {
    for scenario in ["auth-success", "auth-early", "auth-failed"] {
        let lab = Lab::new();
        let mut client = CodexClient::spawn(lab.codex(scenario)).unwrap();
        client.initialize().unwrap();
        let login = client.login_device_start().unwrap();
        assert_eq!(login.user_code, "ABCD-EFGH");
        assert_eq!(
            login.verification_url,
            "https://auth.openai.com/codex/device"
        );
        let done = client.login_device_poll(&login.login_id).unwrap();
        assert_eq!(
            done,
            if scenario == "auth-failed" {
                LoginProgress::Failed
            } else {
                LoginProgress::Completed
            }
        );
        assert!(!format!("{done:?}").contains("PRIVATE"));
    }
}
#[test]
fn codex_device_url_rejects_lookalike_host_and_cancel_releases_home_lock() {
    let lab = Lab::new();
    let mut client = CodexClient::spawn(lab.codex("auth-bad-url")).unwrap();
    client.initialize().unwrap();
    assert_eq!(
        client.login_device_start().err().unwrap().kind,
        ErrorKind::Protocol
    );
    let mut client = CodexClient::spawn(lab.codex("auth-wait")).unwrap();
    client.initialize().unwrap();
    let login = client.login_device_start().unwrap();
    assert_eq!(
        client.login_device_poll(&login.login_id).unwrap(),
        LoginProgress::Pending
    );
    assert!(
        matches!(CodexClient::spawn(lab.codex("auth-success")),Err(e) if e.kind==ErrorKind::State)
    );
    client.login_device_cancel(&login.login_id).unwrap();
    let _other = CodexClient::spawn(lab.codex("auth-success")).unwrap();
}
#[test]
fn antigravity_native_pty_code_is_single_use_and_cancel_reaps_process() {
    let lab = Lab::new();
    let mut process = lab.pty();
    let deadline = Instant::now() + Duration::from_secs(2);
    loop {
        match process.poll().unwrap() {
            LoginPtyProgress::Authorization { url } => {
                assert!(url.starts_with("https://accounts.google.com/"));
                break;
            }
            LoginPtyProgress::Pending => (),
            _ => panic!("unexpected login state"),
        };
        assert!(Instant::now() < deadline);
        std::thread::sleep(Duration::from_millis(5));
    }
    assert!(process.submit_code("bad\ncode").is_err());
    process.submit_code("synthetic-once-code").unwrap();
    assert!(process.submit_code("synthetic-once-code").is_err());
    loop {
        match process.poll().unwrap() {
            LoginPtyProgress::Completed => break,
            LoginPtyProgress::Pending => (),
            _ => panic!("unexpected login state"),
        };
        assert!(Instant::now() < deadline);
        std::thread::sleep(Duration::from_millis(5));
    }
    assert!(lab.0.join("home/synthetic-authenticated").is_file());
    drop(process);
    let pid: i32 = fs::read_to_string(lab.0.join("home/synthetic-login-pid"))
        .unwrap()
        .parse()
        .unwrap();
    assert_eq!(unsafe { libc::kill(pid, 0) }, -1);
    let process = lab.pty();
    drop(process);
}

#[test]
#[ignore = "Explicit native CLI, empty home, no-network Docker inspection only"]
fn native_antigravity_generates_operator_authorization_without_browser_or_account() {
    let binary = std::env::var_os("AGENT_RUNTIME_AUTH_INSPECT_BINARY")
        .expect("explicit native binary required");
    let lab = Lab::new();
    let mut process = AntigravityLogin::spawn(
        std::path::Path::new(&binary),
        &lab.0.join("home"),
        &lab.0.join("workspace"),
        Duration::from_secs(5),
    )
    .unwrap();
    let deadline = Instant::now() + Duration::from_secs(4);
    loop {
        match process.poll().unwrap() {
            LoginPtyProgress::Authorization { url } => {
                assert!(url.starts_with("https://accounts.google.com/o/oauth2/auth?"));
                break;
            }
            LoginPtyProgress::Pending => (),
            _ => panic!("native CLI did not provide a remote human authorization step"),
        }
        assert!(Instant::now() < deadline);
        std::thread::sleep(Duration::from_millis(10));
    }
    // No code is submitted and no account is available. Drop cancels the native
    // process; this test never treats URL generation as real authentication.
}

#[test]
fn native_login_deadline_kills_process_even_when_operator_stops_polling() {
    let lab = Lab::new();
    let mut process = AntigravityLogin::spawn(
        std::path::Path::new(env!("CARGO_BIN_EXE_engine-login-fixture")),
        &lab.0.join("home"),
        &lab.0.join("workspace"),
        Duration::from_millis(300),
    )
    .unwrap();
    let pidfile = lab.0.join("home/synthetic-login-pid");
    let deadline = Instant::now() + Duration::from_millis(250);
    while !pidfile.exists() {
        assert!(Instant::now() < deadline);
        std::thread::sleep(Duration::from_millis(5));
    }
    let pid: i32 = fs::read_to_string(pidfile).unwrap().parse().unwrap();
    // There is deliberately no poll call during this pause.
    std::thread::sleep(Duration::from_millis(400));
    let stat = fs::read_to_string(format!("/proc/{pid}/stat")).unwrap();
    assert_eq!(
        stat.rsplit_once(") ").unwrap().1.split_whitespace().next(),
        Some("Z")
    );
    assert_eq!(process.poll().err().unwrap().kind, ErrorKind::Timeout);
    assert_eq!(unsafe { libc::kill(pid, 0) }, -1);
}
