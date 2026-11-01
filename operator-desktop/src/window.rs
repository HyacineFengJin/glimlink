// SPDX-License-Identifier: MIT
//! Fixed actions for the packaged main window; no arbitrary window identifiers.
use crate::bridge::{ApiError, ApiResult};
use serde::Deserialize;
use serde_json::{Value, json};

#[derive(Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum WindowAction {
    State,
    Drag,
    Minimize,
    ToggleMaximize,
    Close,
}

#[tauri::command]
pub fn window_action(window: tauri::WebviewWindow, action: WindowAction) -> ApiResult<Value> {
    if window.label() != "main" {
        return Err(ApiError::new("untrusted_caller", "窗口操作不可用。"));
    }
    let failed = |_| ApiError::new("window_unavailable", "窗口操作未完成。");
    match action {
        WindowAction::Drag => window.start_dragging().map_err(failed)?,
        WindowAction::Minimize => window.minimize().map_err(failed)?,
        WindowAction::ToggleMaximize => {
            if window.is_maximized().map_err(failed)? {
                window.unmaximize().map_err(failed)?;
            } else {
                window.maximize().map_err(failed)?;
            }
        }
        WindowAction::Close => {
            window.close().map_err(failed)?;
            return Ok(json!({}));
        }
        WindowAction::State => {}
    }
    // Niri has no minimize workflow; its compositor/workspaces manage visibility.
    let niri = std::env::var_os("NIRI_SOCKET").is_some()
        || std::env::var("XDG_CURRENT_DESKTOP")
            .is_ok_and(|v| v.split(':').any(|d| d.eq_ignore_ascii_case("niri")));
    Ok(json!({"maximized":window.is_maximized().map_err(failed)?,"can_minimize":!niri}))
}
