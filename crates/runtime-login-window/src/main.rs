// SPDX-License-Identifier: MIT
#[cfg(target_os = "linux")]
fn main() {
    std::process::exit(runtime_login_window::native::run());
}
#[cfg(not(target_os = "linux"))]
fn main() {
    use runtime_control::{
        login_window::{WindowErrorCode, WindowEvent},
        write_json,
    };
    let _ = write_json(
        &mut std::io::stdout().lock(),
        &WindowEvent::error(WindowErrorCode::MissingWebkit),
    );
    std::process::exit(1);
}
