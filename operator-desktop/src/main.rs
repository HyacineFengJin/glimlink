// SPDX-License-Identifier: MIT
mod bridge;
mod launch;
mod memory;
mod security;
mod x_login;

fn commands(invoke: tauri::ipc::Invoke<tauri::Wry>) -> bool {
    let handler: fn(tauri::ipc::Invoke<tauri::Wry>) -> bool = tauri::generate_handler![
        bridge::platform_capabilities,
        bridge::account_status,
        bridge::begin_login,
        bridge::submit_login_input,
        bridge::cancel_login,
        bridge::control_request,
        bridge::worker_control,
        launch::launch_connection,
        bridge::memory_snapshot,
        x_login::open_x_login_window,
        x_login::open_x_in_browser,
    ];
    handler(invoke)
}
fn main() {
    let defaults =
        launch::Defaults::parse(std::env::args().skip(1).collect()).unwrap_or_else(|message| {
            eprintln!("{message}");
            std::process::exit(2)
        });
    let application = tauri::Builder::default()
        .manage(defaults)
        .manage(bridge::Sessions::default())
        .manage(x_login::Windows::default())
        .setup(|app| {
            let config = app
                .config()
                .app
                .windows
                .iter()
                .find(|window| window.label == "main")
                .ok_or("main window configuration missing")?;
            let builder = tauri::WebviewWindowBuilder::from_config(app, config)?
                .on_navigation(security::local_document);
            // Niri already provides window management; avoid GTK's extra title bar.
            #[cfg(target_os = "linux")]
            let builder = if std::env::var_os("NIRI_SOCKET").is_some()
                || std::env::var("XDG_CURRENT_DESKTOP").is_ok_and(|v| v.split(':').any(|d| d.eq_ignore_ascii_case("niri")))
            { builder.decorations(false) } else { builder };
            let main = builder.build()?;
            let handle = app.handle().clone();
            main.on_window_event(move |event| {
                if matches!(event, tauri::WindowEvent::CloseRequested { .. }) {
                    use tauri::Manager;
                    handle.state::<x_login::Windows>().close_all(&handle);
                }
            });
            Ok(())
        })
        .invoke_handler(move |invoke| {
            let caller = invoke.message.webview_ref();
            if !caller.url().is_ok_and(|url| security::management_caller(caller.label(), caller.window().label(), &url)) {
                invoke.resolver.reject(serde_json::json!({"code":"untrusted_caller","message":"管理操作仅允许打包的本地主界面调用。"}));
                return true;
            }
            commands(invoke)
        })
        .build(tauri::generate_context!())
        .expect("cannot initialize operator desktop");
    application.run(|handle, event| {
        if matches!(
            event,
            tauri::RunEvent::ExitRequested { .. } | tauri::RunEvent::Exit
        ) {
            use tauri::Manager;
            handle.state::<x_login::Windows>().close_all(handle);
            handle.state::<bridge::Sessions>().cancel_all();
        }
    });
}
