#![cfg(target_os = "linux")]
//! Synthetic native processes only: no model, shell, account or network access.
use runtime_engine::{CodexClient, CodexConfig};
use serde::{Deserialize, Serialize};
use std::{
    fs,
    os::unix::{fs::PermissionsExt, process::ExitStatusExt},
    path::{Path, PathBuf},
    process::{Child, Command, Stdio},
    thread,
    time::{Duration, Instant, SystemTime, UNIX_EPOCH},
};

const FIXTURE_ENV: &str = "AGENT_ENGINE_PARENT_DEATH_FIXTURE";
const FIXTURE_PREFIX: &str = "runtime-engine-parent-death-fixture-";
const READY: &str = "native-ready.json";

fn fixture_root(path: &Path) -> bool {
    path.is_absolute()
        && path
            .file_name()
            .and_then(|name| name.to_str())
            .is_some_and(|name| name.starts_with(FIXTURE_PREFIX))
}

#[derive(Deserialize, Serialize)]
struct NativeInfo {
    pid: libc::pid_t,
    parent: libc::pid_t,
    process_group: libc::pid_t,
    parent_death_signal: libc::c_int,
}

/// Only selected by the outer test using this same native ELF. Running ignored
/// tests manually does nothing unless a synthetic fixture directory is supplied.
#[test]
#[ignore]
fn synthetic_owner_helper() {
    let Some(root) = std::env::var_os(FIXTURE_ENV).map(PathBuf::from) else {
        return;
    };
    if !fixture_root(&root) {
        return;
    }
    let mut config = CodexConfig::new(
        std::env::current_exe().unwrap(),
        root.join("home"),
        root.join("workspace"),
    );
    config.args = vec![
        "--ignored".into(),
        "--exact".into(),
        "synthetic_native_helper".into(),
        "--nocapture".into(),
        "--test-threads=1".into(),
    ];
    let _client = CodexClient::spawn(config).unwrap();
    // No initialize/account/model call. Keep the owner thread alive so this
    // verifies process death rather than Linux's creator-thread exit behavior.
    thread::sleep(Duration::from_secs(30));
}

/// The probe deliberately ignores stdin EOF, so death cannot be mistaken for
/// ordinary pipe cleanup. CODEX_HOME is a new, validated synthetic directory.
#[test]
#[ignore]
fn synthetic_native_helper() {
    let Some(home) = std::env::var_os("CODEX_HOME").map(PathBuf::from) else {
        return;
    };
    if home.file_name().and_then(|name| name.to_str()) != Some("home")
        || !home.parent().is_some_and(fixture_root)
    {
        return;
    }
    // libtest runs this function in a newly cloned test thread. PR_GET reads
    // that thread's value (normally zero), not the exec leader configured by
    // pre_exec. Keep it diagnostic; verify actual parent-death behavior below.
    let mut signal = 0;
    assert_eq!(
        unsafe {
            libc::prctl(
                libc::PR_GET_PDEATHSIG,
                &mut signal as *mut libc::c_int,
                0 as libc::c_ulong,
                0 as libc::c_ulong,
                0 as libc::c_ulong,
            )
        },
        0
    );
    let info = NativeInfo {
        pid: unsafe { libc::getpid() },
        parent: unsafe { libc::getppid() },
        process_group: unsafe { libc::getpgrp() },
        parent_death_signal: signal,
    };
    let ready = home.join(READY);
    let temporary = home.join("native-ready.tmp");
    fs::write(&temporary, serde_json::to_vec(&info).unwrap()).unwrap();
    fs::set_permissions(&temporary, fs::Permissions::from_mode(0o600)).unwrap();
    fs::rename(temporary, ready).unwrap();
    // Even when the implementation is broken, the fixture has a finite fallback
    // lifetime. The outer guard normally kills and reaps it well before this.
    thread::sleep(Duration::from_secs(30));
}

struct Subreaper {
    previous: libc::c_int,
}
impl Subreaper {
    fn new() -> Self {
        let mut previous = 0;
        assert_eq!(
            unsafe {
                libc::prctl(
                    libc::PR_GET_CHILD_SUBREAPER,
                    &mut previous as *mut libc::c_int,
                    0 as libc::c_ulong,
                    0 as libc::c_ulong,
                    0 as libc::c_ulong,
                )
            },
            0
        );
        assert_eq!(
            unsafe {
                libc::prctl(
                    libc::PR_SET_CHILD_SUBREAPER,
                    1 as libc::c_ulong,
                    0 as libc::c_ulong,
                    0 as libc::c_ulong,
                    0 as libc::c_ulong,
                )
            },
            0
        );
        Self { previous }
    }
}
impl Drop for Subreaper {
    fn drop(&mut self) {
        unsafe {
            libc::prctl(
                libc::PR_SET_CHILD_SUBREAPER,
                self.previous as libc::c_ulong,
                0 as libc::c_ulong,
                0 as libc::c_ulong,
                0 as libc::c_ulong,
            );
        }
    }
}

struct Processes {
    owner: Child,
    native: Option<libc::pid_t>,
    root: PathBuf,
}
impl Drop for Processes {
    fn drop(&mut self) {
        if self.owner.try_wait().ok().flatten().is_none() {
            let _ = self.owner.kill();
        }
        let _ = self.owner.wait();
        if let Some(pid) = self.native {
            // The subreaper owns this known child until it is reaped; its PID
            // cannot be reused for an unrelated process before waitpid succeeds.
            let mut status = 0;
            if unsafe { libc::waitpid(pid, &mut status, libc::WNOHANG) } == 0 {
                unsafe {
                    libc::kill(pid, libc::SIGKILL);
                    libc::waitpid(pid, &mut status, 0);
                }
            }
        }
        let _ = fs::remove_dir_all(&self.root);
    }
}

#[test]
fn owner_sigkill_kills_and_reaps_the_native_engine_without_drop() {
    // This integration binary has one ordinary test. Helpers run only in their
    // own subprocesses, so the temporary subreaper setting cannot affect other
    // test binaries or account processes.
    let _subreaper = Subreaper::new();
    let nonce = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap()
        .as_nanos();
    let root = std::env::temp_dir().join(format!("{FIXTURE_PREFIX}{}-{nonce}", std::process::id()));
    fs::create_dir(&root).unwrap();
    fs::set_permissions(&root, fs::Permissions::from_mode(0o700)).unwrap();
    fs::create_dir(root.join("workspace")).unwrap();
    let owner = Command::new(std::env::current_exe().unwrap())
        .args([
            "--ignored",
            "--exact",
            "synthetic_owner_helper",
            "--nocapture",
            "--test-threads=1",
        ])
        .env_clear()
        .env(FIXTURE_ENV, &root)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .unwrap();
    let mut processes = Processes {
        owner,
        native: None,
        root,
    };
    let deadline = Instant::now() + Duration::from_secs(5);
    let info = loop {
        let path = processes.root.join("home").join(READY);
        if let Ok(bytes) = fs::read(path) {
            break serde_json::from_slice::<NativeInfo>(&bytes).unwrap();
        }
        assert!(
            processes.owner.try_wait().unwrap().is_none(),
            "synthetic owner exited before probe readiness"
        );
        assert!(
            Instant::now() < deadline,
            "native probe did not become ready"
        );
        thread::sleep(Duration::from_millis(10));
    };
    assert!(info.pid > 1);
    assert_eq!(info.parent, processes.owner.id() as libc::pid_t);
    processes.native = Some(info.pid);
    assert_eq!(info.process_group, info.pid);
    processes.owner.kill().unwrap();
    assert_eq!(
        processes.owner.wait().unwrap().signal(),
        Some(libc::SIGKILL)
    );
    let deadline = Instant::now() + Duration::from_secs(3);
    let status = loop {
        let mut status = 0;
        let reaped = unsafe { libc::waitpid(info.pid, &mut status, libc::WNOHANG) };
        if reaped == info.pid {
            break status;
        }
        assert_eq!(
            reaped, 0,
            "native fixture was not adopted by this test subreaper"
        );
        assert!(
            Instant::now() < deadline,
            "native engine survived owner SIGKILL"
        );
        thread::sleep(Duration::from_millis(10));
    };
    processes.native = None;
    assert!(libc::WIFSIGNALED(status));
    assert_eq!(libc::WTERMSIG(status), libc::SIGKILL);
}
