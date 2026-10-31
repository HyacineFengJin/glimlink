// SPDX-License-Identifier: MIT
//! Explicit feature-gated synthetic acceptance. Never navigates to real X.
#[path = "../bridge.rs"]
#[allow(dead_code)]
mod bridge;
#[path = "../memory.rs"]
mod memory;
#[path = "../security.rs"]
mod security;
#[path = "../x_login.rs"]
mod x_login;
use serde_json::json;
use std::{
    io::{Read, Write},
    net::TcpListener,
    sync::{
        Arc,
        atomic::{AtomicBool, AtomicUsize, Ordering},
    },
    thread,
    time::{Duration, Instant},
};
use tauri::{Manager, Url, webview::Cookie};

#[track_caller]
fn wait(mut predicate: impl FnMut() -> bool) {
    let deadline = Instant::now() + Duration::from_secs(5);
    while !predicate() {
        assert!(Instant::now() < deadline, "synthetic operation timed out");
        thread::sleep(Duration::from_millis(30));
    }
}
fn seed(window: &tauri::WebviewWindow, name: &str, value: &str) {
    window
        .set_cookie(
            Cookie::build((name.to_owned(), value.to_owned()))
                .domain("x.com")
                .path("/")
                .secure(true)
                .http_only(true)
                .build(),
        )
        .unwrap();
}
fn commands(invoke: tauri::ipc::Invoke<tauri::Wry>) -> bool {
    let handler: fn(tauri::ipc::Invoke<tauri::Wry>) -> bool = tauri::generate_handler![
        memory::memory_snapshot,
        x_login::open_x_login_window,
        x_login::open_x_in_browser
    ];
    handler(invoke)
}
fn main() {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let url: Url = format!(
        "http://127.0.0.1:{}/fixture",
        listener.local_addr().unwrap().port()
    )
    .parse()
    .unwrap();
    let denied = Arc::new(AtomicBool::new(false));
    let observed = denied.clone();
    thread::spawn(move || {
        for socket in listener.incoming() {
            let Ok(mut socket) = socket else {
                break;
            };
            let _ = socket.set_read_timeout(Some(Duration::from_secs(1)));
            let mut request = [0u8; 8192];
            let Ok(length) = socket.read(&mut request) else {
                continue;
            };
            let first = String::from_utf8_lossy(&request[..length]);
            let path = first.split_whitespace().nth(1).unwrap_or("");
            let page = if path == "/fixture" {
                r#"<!doctype html><title>Synthetic login</title><p>Synthetic isolated login page. No account.</p><script>addEventListener('load',async()=>{try{await window.__TAURI__.core.invoke('fixture_probe');await fetch('/unexpected-access')}catch(_){await fetch('/denied')}window.open('https://evil.invalid/');location.href='https://accounts.google.com/';});</script>"#
            } else {
                if path == "/denied" {
                    observed.store(true, Ordering::Relaxed);
                }
                "ok"
            };
            let response = format!(
                "HTTP/1.1 200 OK\r\nContent-Type: text/html\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
                page.len(),
                page
            );
            let _ = socket.write_all(response.as_bytes());
        }
    });
    let probes = Arc::new(AtomicUsize::new(0));
    let command_probes = probes.clone();
    let browser_result = Arc::new(AtomicUsize::new(0));
    let browser_command_result = browser_result.clone();
    tauri::Builder::default().manage(bridge::Sessions::default()).manage(x_login::Windows::default())
        .invoke_handler(move |invoke| {
            let caller = invoke.message.webview_ref();
            if !caller.url().is_ok_and(|url| security::management_caller(caller.label(), caller.window().label(), &url)) {
                invoke.resolver.reject(json!({"code":"untrusted_caller"})); return true;
            }
            if invoke.message.command() == "fixture_probe" { command_probes.fetch_add(1, Ordering::Relaxed); invoke.resolver.resolve(true); true }
            else if invoke.message.command() == "fixture_browser_result" {
                if let tauri::ipc::InvokeBody::Json(value) = invoke.message.payload() {
                    let result = match value["code"].as_str() {
                        Some("ok") => 1,
                        Some("input_not_requested") => 2,
                        Some("browser_unavailable") => 3,
                        Some("untrusted_caller") => 4,
                        _ => 5,
                    };
                    browser_command_result.store(result, Ordering::Relaxed);
                }
                invoke.resolver.resolve(true); true
            } else { commands(invoke) }
        })
        .setup(move |app| {
            let main = tauri::WebviewWindowBuilder::new(app, "main", tauri::WebviewUrl::App("index.html".into()))
                .title("Synthetic X window acceptance").on_navigation(security::local_document).build()?;
            let handle = app.handle().clone();
            let fixture_denied = denied.clone();
            let fixture_probes = probes.clone();
            thread::spawn(move || {
                let outcome = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                    wait(|| {
                        main.eval("if(!window.__syntheticProbe && window.__TAURI__?.core){window.__syntheticProbe=true;window.__TAURI__.core.invoke('fixture_probe')}").unwrap();
                        fixture_probes.load(Ordering::Relaxed) == 1
                    });
                    let sessions = handle.state::<bridge::Sessions>();
                    let windows = handle.state::<x_login::Windows>();
                    let (ticket, input) = sessions.fixture_request("synthetic-first");
                    assert!(sessions.x_cookie_ticket("wrong-login").is_err());
                    let opener_result = std::path::PathBuf::from(std::env::var_os("AGENT_OPERATOR_SYNTHETIC_OPENER_RESULT").expect("use the isolated fixture runner"));
                    main.eval("window.__TAURI__.core.invoke('open_x_in_browser',{loginId:'synthetic-first'}).then(()=>window.__TAURI__.core.invoke('fixture_browser_result',{code:'ok'})).catch(e=>window.__TAURI__.core.invoke('fixture_browser_result',{code:e?.code||'other'}))").unwrap();
                    wait(|| browser_result.load(Ordering::Relaxed) != 0);
                    assert_eq!(browser_result.load(Ordering::Relaxed), 1, "system browser command failed: 2=input_not_requested, 3=browser_unavailable, 4=untrusted_caller, 5=other");
                    wait(|| opener_result.exists());
                    assert!(ticket.waiting() && input.receive(Duration::from_millis(50)).is_none(), "external browser must not fabricate a callback");
                    main.eval("window.__TAURI__.core.invoke('open_x_in_browser',{loginId:'wrong-login'}).catch(()=>{})").unwrap();
                    thread::sleep(Duration::from_millis(100));
                    assert_eq!(std::fs::read_to_string(&opener_result).unwrap(), "fixed-x-url\n");
                    let label = windows.open_fixture(handle.clone(), ticket.clone(), url.clone(), Duration::from_secs(8)).unwrap();
                    assert!(windows.open_fixture(handle.clone(), ticket.clone(), url.clone(), Duration::from_secs(8)).is_err());
                    let view = handle.get_webview_window(&label).unwrap();
                    wait(|| fixture_denied.load(Ordering::Relaxed));
                    assert_eq!(fixture_probes.load(Ordering::Relaxed), 1, "remote management IPC ran");
                    assert_eq!(handle.webview_windows().len(), 2, "popup escaped the window policy");
                    seed(&view, "auth_token", "synthetic-native-auth");
                    seed(&view, "ct0", "synthetic-native-csrf");
                    seed(&view, "twid", "synthetic-extra-must-disappear");
                    assert!(input.receive(Duration::from_millis(650)).is_none(), "login route submitted cookies before completion");
                    let mut challenge = url.clone(); challenge.set_path("/account/access");
                    view.navigate(challenge).unwrap();
                    wait(|| view.url().is_ok_and(|url| url.path() == "/account/access"));
                    assert!(input.receive(Duration::from_millis(650)).is_none(), "challenge route submitted cookies before completion");
                    let mut home = url.clone(); home.set_path("/home");
                    view.navigate(home).unwrap();
                    let secret = input.receive(Duration::from_secs(5)).expect("native cookie capture missing");
                    let values: serde_json::Value = serde_json::from_slice(secret.bytes()).unwrap();
                    assert_eq!(values, json!({"auth_token":"synthetic-native-auth","ct0":"synthetic-native-csrf"}));
                    wait(|| handle.get_webview_window(&label).is_none() && windows.fixture_idle());
                    sessions.fixture_end();
                    // A fresh ephemeral context must not inherit the first login.
                    let (next, pending) = sessions.fixture_request("synthetic-second");
                    let second = windows.open_fixture(handle.clone(), next.clone(), url.clone(), Duration::from_secs(8)).unwrap();
                    let view = handle.get_webview_window(&second).unwrap();
                    assert!(view.cookies_for_url("https://x.com/".parse().unwrap()).unwrap().is_empty());
                    seed(&view, "auth_token", "synthetic-only-one-cookie");
                    let mut home = url.clone(); home.set_path("/home");
                    view.navigate(home).unwrap();
                    assert!(pending.receive(Duration::from_millis(250)).is_none());
                    sessions.fixture_end();
                    wait(|| handle.get_webview_window(&second).is_none() && windows.fixture_idle());
                    let (third, absent) = sessions.fixture_request("synthetic-third");
                    assert!(ticket.submit(runtime_control::Secret::new(b"synthetic-stale".to_vec()).unwrap()).is_err());
                    let timeout = windows.open_fixture(handle.clone(), third.clone(), url.clone(), Duration::from_millis(250)).unwrap();
                    wait(|| handle.get_webview_window(&timeout).is_none() && windows.fixture_idle());
                    assert!(third.waiting() && absent.receive(Duration::from_millis(50)).is_none());
                    sessions.fixture_end();
                    let (fourth, manual) = sessions.fixture_request("synthetic-fourth");
                    let final_window = windows.open_fixture(handle.clone(), fourth.clone(), url.clone(), Duration::from_secs(8)).unwrap();
                    fourth.submit(runtime_control::Secret::new(b"synthetic-manual-input".to_vec()).unwrap()).unwrap();
                    assert!(manual.receive(Duration::from_secs(1)).is_some());
                    wait(|| handle.get_webview_window(&final_window).is_none() && windows.fixture_idle());
                    assert!(manual.receive(Duration::from_millis(50)).is_none());
                    sessions.fixture_end();
                    windows.close_all(&handle);
                    println!("{}", json!({"native_system_webview":true,"http_only_secure_capture":true,"private_input_only_required_cookies":true,"remote_management_ipc_denied":true,"popups_denied":true,"fresh_incognito_isolation":true,"incomplete_cookies_not_submitted":true,"login_and_challenge_routes_do_not_submit":true,"duplicate_window_rejected":true,"session_mismatch_rejected":true,"cancel_closes_window":true,"timeout_closes_window":true,"manual_input_closes_window":true,"external_browser_fixed_url":true,"external_browser_no_automatic_callback":true,"external_browser_wrong_session_rejected":true,"no_live_x_request":true,"real_account_used":false}));
                }));
                if outcome.is_err() { eprintln!("synthetic X login window acceptance failed"); }
                handle.exit(if outcome.is_ok() {0} else {1});
            });
            Ok(())
        })
        .run(tauri::generate_context!()).expect("synthetic native fixture failed");
}
