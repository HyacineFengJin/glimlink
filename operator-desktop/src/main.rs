// SPDX-License-Identifier: MIT
mod bridge;
mod ime;
mod launch;
mod memory;
mod security;
mod window;
mod x_login;

fn commands(invoke: tauri::ipc::Invoke<tauri::Wry>) -> bool {
    let handler: fn(tauri::ipc::Invoke<tauri::Wry>) -> bool = tauri::generate_handler![
        window::window_action,
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
    ime::prepare().unwrap_or_else(|error| {
        eprintln!("Cannot prepare operator input method: {error}");
        std::process::exit(2)
    });
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
            let main = builder.decorations(false).build()?;
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
