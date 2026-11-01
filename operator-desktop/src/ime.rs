// SPDX-License-Identifier: MIT
//! Synchronize viewport caret geometry with the system WebKit input context.
//! No desktop detection, environment changes, text transport, or background task.
use crate::bridge::{ApiError, ApiResult};
use serde::Deserialize;

pub fn initialize(window: &tauri::WebviewWindow) -> tauri::Result<()> {
    #[cfg(target_os = "linux")]
    window.with_webview(|view| {
        use webkit2gtk::{InputMethodContextExt, WebViewExt};
        if let Some(context) = view.inner().input_method_context() {
            // Wry disables inline preedit by default. Restore normal WebKit
            // composition so the DOM receives composition events and tracks
            // the preedit caret instead of an external IME editing window.
            context.set_enable_preedit(true);
        }
    })?;
    #[cfg(not(target_os = "linux"))]
    let _ = window;
    Ok(())
}

#[derive(Clone, Copy, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CursorArea {
    x: f64,
    y: f64,
    width: f64,
    height: f64,
    viewport_width: f64,
    viewport_height: f64,
}
impl CursorArea {
    fn valid(self) -> bool {
        [
            self.x,
            self.y,
            self.width,
            self.height,
            self.viewport_width,
            self.viewport_height,
        ]
        .iter()
        .all(|v| v.is_finite())
            && (1.0..=32768.0).contains(&self.viewport_width)
            && (1.0..=32768.0).contains(&self.viewport_height)
            && self.x >= 0.0
            && self.x < self.viewport_width
            && self.y >= 0.0
            && self.y < self.viewport_height
            && (0.0..=256.0).contains(&self.width)
            && (1.0..=512.0).contains(&self.height)
    }
    #[cfg(any(target_os = "linux", test))]
    fn native(self, allocated_width: i32) -> Option<(i32, i32, i32, i32)> {
        if !self.valid() {
            return None;
        }
        // CSS viewport pixels -> GTK widget coordinates. Do not multiply by
        // devicePixelRatio: GTK already handles monitor/output scaling. The
        // allocation ratio also covers WebKit page zoom and X11 font scaling.
        let scale = f64::from(allocated_width) / self.viewport_width;
        if !(0.125..=8.0).contains(&scale) {
            return None;
        }
        Some((
            (self.x * scale).round() as i32,
            (self.y * scale).round() as i32,
            (self.width * scale).ceil().max(1.0) as i32,
            (self.height * scale).ceil().max(1.0) as i32,
        ))
    }
}

#[tauri::command]
pub fn ime_capabilities() -> bool {
    cfg!(target_os = "linux")
}

#[tauri::command]
pub fn ime_cursor_area(window: tauri::WebviewWindow, area: CursorArea) -> ApiResult<()> {
    if window.label() != "main" {
        return Err(ApiError::new("untrusted_caller", "输入位置不可用。"));
    }
    if !area.valid() {
        return Err(ApiError::new("invalid_input_area", "输入位置无效。"));
    }
    #[cfg(target_os = "linux")]
    window
        .with_webview(move |view| {
            use gtk::prelude::WidgetExt;
            use webkit2gtk::{InputMethodContextExt, WebViewExt};
            let webview = view.inner();
            if let (Some(context), Some((x, y, width, height))) = (
                webview.input_method_context(),
                area.native(webview.allocated_width()),
            ) {
                context.notify_cursor_area(x, y, width, height);
            }
        })
        .map_err(|_| ApiError::new("input_context_unavailable", "输入位置暂不可用。"))?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    fn sample() -> CursorArea {
        CursorArea {
            x: 140.0,
            y: 300.0,
            width: 1.0,
            height: 20.0,
            viewport_width: 800.0,
            viewport_height: 600.0,
        }
    }
    #[test]
    fn uses_widget_ratio_not_output_dpi() {
        assert_eq!(sample().native(800), Some((140, 300, 1, 20)));
        assert_eq!(sample().native(1200), Some((210, 450, 2, 30)));
        assert_eq!(sample().native(1000), Some((175, 375, 2, 25)));
    }
    #[test]
    fn rejects_nonfinite_offscreen_and_unbounded_geometry() {
        for bad in [
            CursorArea {
                x: f64::NAN,
                ..sample()
            },
            CursorArea {
                y: f64::INFINITY,
                ..sample()
            },
            CursorArea {
                y: -1.0,
                ..sample()
            },
            CursorArea {
                x: 800.0,
                ..sample()
            },
            CursorArea {
                height: 10000.0,
                ..sample()
            },
            CursorArea {
                viewport_width: 1e20,
                ..sample()
            },
        ] {
            assert!(!bad.valid());
            assert!(bad.native(800).is_none());
        }
        assert!(sample().native(0).is_none());
    }
    #[test]
    fn command_accepts_geometry_only() {
        let value = serde_json::json!({"x":1,"y":1,"width":1,"height":20,"viewport_width":800,"viewport_height":600,"text":"not allowed"});
        assert!(serde_json::from_value::<CursorArea>(value).is_err());
    }
}
