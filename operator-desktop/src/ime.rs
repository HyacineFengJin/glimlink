// SPDX-License-Identifier: MIT
//! Use compositor-positioned IME popups on native Niri/Wayland.
//! Re-exec before GTK starts, rather than mutating another thread's environment.
#[cfg(target_os = "linux")]
pub fn prepare() -> std::io::Result<()> {
    use std::{env, os::unix::process::CommandExt, process::Command};
    let current = env::var("GTK_IM_MODULE").ok();
    let requested = env::var("GLIMLINK_OPERATOR_IM_MODULE").ok();
    let niri = env::var_os("NIRI_SOCKET").is_some()
        || env::var("XDG_CURRENT_DESKTOP")
            .is_ok_and(|v| v.split(':').any(|s| s.eq_ignore_ascii_case("niri")));
    let native =
        env::var_os("WAYLAND_DISPLAY").is_some() && env::var("GDK_BACKEND").as_deref() != Ok("x11");
    let chosen = match requested.as_deref() {
        Some("inherit") => None,
        Some(v @ ("wayland" | "fcitx" | "ibus" | "xim" | "simple")) => Some(v),
        Some(_) => {
            return Err(std::io::Error::new(
                std::io::ErrorKind::InvalidInput,
                "Invalid GLIMLINK_OPERATOR_IM_MODULE",
            ));
        }
        None if niri && native && current.as_deref() == Some("fcitx") => Some("wayland"),
        None => None,
    };
    if let Some(value) = chosen.filter(|v| Some(*v) != current.as_deref()) {
        return Err(Command::new(env::current_exe()?)
            .args(env::args_os().skip(1))
            .env("GTK_IM_MODULE", value)
            .exec());
    }
    Ok(())
}
#[cfg(not(target_os = "linux"))]
pub fn prepare() -> std::io::Result<()> {
    Ok(())
}
