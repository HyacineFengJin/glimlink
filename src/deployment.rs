// SPDX-License-Identifier: MIT
//! Host paths work in an unpacked release, user installation or test container.
use crate::agent_host::EngineKind;
use std::{
    os::unix::fs::PermissionsExt,
    path::{Path, PathBuf},
};

fn executable(path: &Path) -> bool {
    path.metadata()
        .is_ok_and(|m| m.is_file() && m.permissions().mode() & 0o111 != 0)
}
pub fn native_binary(engine: EngineKind) -> PathBuf {
    let (variable, bundled, command) = match engine {
        EngineKind::Codex => ("GLIMLINK_CODEX_BINARY", "codex-native", "codex"),
        EngineKind::Antigravity => ("GLIMLINK_ANTIGRAVITY_BINARY", "antigravity-native", "agy"),
    };
    if let Some(path) = std::env::var_os(variable) {
        return path.into();
    }
    let adjacent = std::env::current_exe()
        .ok()
        .and_then(|p| p.parent().map(|d| d.join(bundled)));
    if let Some(path) = adjacent.as_ref().filter(|p| executable(p)) {
        return path.clone();
    }
    if let Some(path) = std::env::var_os("PATH").and_then(|paths| {
        std::env::split_paths(&paths)
            .filter(|d| d.is_absolute())
            .map(|d| d.join(command))
            .find(|p| executable(p))
    }) {
        return path;
    }
    adjacent.unwrap_or_else(|| PathBuf::from(bundled))
}

pub fn doctor(state: &Path) -> serde_json::Value {
    let engines=[("codex",EngineKind::Codex),("antigravity",EngineKind::Antigravity)].map(|(name,kind)|{
        let path=native_binary(kind);
        serde_json::json!({"engine":name,"binary":path,"available":path.is_absolute() && executable(&path)})
    });
    serde_json::json!({"version":env!("CARGO_PKG_VERSION"),"os":std::env::consts::OS,"arch":std::env::consts::ARCH,
        "state":state,"state_exists":state.is_dir(),"engines":engines,
        "network_requests":0,"login_started":false,
        "engine_installation":"Place pinned codex-native/antigravity-native beside agent-runtime, or set GLIMLINK_CODEX_BINARY/GLIMLINK_ANTIGRAVITY_BINARY to absolute paths.",
        "operator_connection":"Stop the agent service before login; operator serve owns the same private state lock."})
}
