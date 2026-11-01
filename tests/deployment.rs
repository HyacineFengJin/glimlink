// SPDX-License-Identifier: MIT
//! Clean-install discovery does not execute an engine or read its account home.
use std::{
    fs,
    os::unix::fs::PermissionsExt,
    path::PathBuf,
    process::Command,
    sync::atomic::{AtomicU64, Ordering},
};
static NEXT: AtomicU64 = AtomicU64::new(0);
struct Lab(PathBuf);
impl Lab {
    fn new() -> Self {
        let path = std::env::temp_dir().join(format!(
            "glimlink-deploy-{}-{}",
            std::process::id(),
            NEXT.fetch_add(1, Ordering::Relaxed)
        ));
        fs::create_dir(&path).unwrap();
        fs::copy(
            env!("CARGO_BIN_EXE_agent-runtime"),
            path.join("agent-runtime"),
        )
        .unwrap();
        Self(path)
    }
    fn doctor(&self) -> Command {
        let mut c = Command::new(self.0.join("agent-runtime"));
        c.env_clear()
            .env("PATH", "")
            .arg("--state")
            .arg(self.0.join("absent-state"))
            .arg("doctor");
        c
    }
    fn engine(&self, name: &str) -> PathBuf {
        let p = self.0.join(name);
        fs::write(&p, b"#!/bin/sh\nexit 99\n").unwrap();
        fs::set_permissions(&p, fs::Permissions::from_mode(0o700)).unwrap();
        p
    }
}
impl Drop for Lab {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}
#[test]
fn adjacent_engines_and_version_work_without_state_or_cache_mount() {
    let l = Lab::new();
    l.engine("codex-native");
    l.engine("antigravity-native");
    let out = l.doctor().output().unwrap();
    assert!(out.status.success());
    let d: serde_json::Value = serde_json::from_slice(&out.stdout).unwrap();
    assert!(
        d["engines"]
            .as_array()
            .unwrap()
            .iter()
            .all(|e| e["available"] == true)
    );
    assert_eq!(d["network_requests"], 0);
    assert_eq!(d["login_started"], false);
    assert!(!l.0.join("absent-state").exists());
    let out = Command::new(l.0.join("agent-runtime"))
        .arg("--version")
        .output()
        .unwrap();
    assert!(out.status.success());
    assert!(
        String::from_utf8(out.stdout)
            .unwrap()
            .contains(env!("CARGO_PKG_VERSION"))
    );
}
#[test]
fn trusted_overrides_take_precedence_and_relative_paths_fail_closed() {
    let l = Lab::new();
    l.engine("codex-native");
    let override_path = l.engine("chosen-codex");
    let out = l
        .doctor()
        .env("GLIMLINK_CODEX_BINARY", &override_path)
        .env("GLIMLINK_ANTIGRAVITY_BINARY", "relative-agy")
        .output()
        .unwrap();
    assert!(out.status.success());
    let d: serde_json::Value = serde_json::from_slice(&out.stdout).unwrap();
    assert_eq!(d["engines"][0]["binary"], override_path.to_str().unwrap());
    assert_eq!(d["engines"][0]["available"], true);
    assert_eq!(d["engines"][1]["available"], false);
    assert!(!l.0.join("absent-state").exists());
}
