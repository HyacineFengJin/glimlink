// SPDX-License-Identifier: MIT
//! Optional human-operated X window. Cookies stay in Rust and the private pipe.
use crate::bridge::{ApiError, ApiResult, CookieTicket, Sessions};
use runtime_control::Secret;
use serde_json::json;
use std::{
    sync::{
        Arc, Mutex,
        atomic::{AtomicBool, AtomicU64, Ordering},
    },
    thread,
    time::{Duration, Instant},
};
use tauri::{
    AppHandle, Emitter, Manager, State, Url, WebviewUrl, WebviewWindow, WebviewWindowBuilder,
    webview::{Cookie, NewWindowResponse, PermissionResponse},
};

const X_ROOT: &str = "https://x.com/";
const LOGIN_URL: &str = "https://x.com/i/flow/login";
const WINDOW_TIMEOUT: Duration = Duration::from_secs(300);

pub(crate) fn x_navigation(url: &Url) -> bool {
    url.scheme() == "https"
        && url.host_str() == Some("x.com")
        && url.username().is_empty()
        && url.password().is_none()
        && url.port().is_none()
}
fn handoff_ready(url: &Url) -> bool {
    // Cookie creation alone does not prove that the human has passed a login
    // challenge. Wait for a normal landing page; Runtime still verifies them.
    x_navigation(url) && matches!(url.path(), "/" | "/home" | "/home/")
}

/// A scoped URL read includes HttpOnly/Secure cookies. Still check the domain,
/// root path and Secure attribute, reject ambiguous names, and discard all else.
fn cookie_secret(cookies: Vec<Cookie<'static>>) -> ApiResult<Option<Secret>> {
    let mut auth = None;
    let mut csrf = None;
    for cookie in cookies {
        let slot = match cookie.name() {
            "auth_token" => &mut auth,
            "ct0" => &mut csrf,
            _ => continue,
        };
        if !matches!(cookie.domain(), Some("x.com" | ".x.com"))
            || cookie.path() != Some("/")
            || cookie.secure() != Some(true)
        {
            continue;
        }
        if slot.is_some() {
            return Err(ApiError::new(
                "x_cookie_ambiguous",
                "网页会话存在重复 Cookie，请关闭窗口后重试或手工导入。",
            ));
        }
        *slot = Some(cookie.value().to_owned());
    }
    let (Some(auth_token), Some(ct0)) = (auth, csrf) else {
        return Ok(None);
    };
    let bytes = serde_json::to_vec(&json!({"auth_token":auth_token,"ct0":ct0}))
        .map_err(|_| ApiError::new("x_cookie_unavailable", "无法准备网页会话。"))?;
    runtime_control::human_ui::cookie_secret(bytes)
        .map(Some)
        .map_err(|_| {
            ApiError::new(
                "x_cookie_invalid",
                "网页登录信息尚不完整，请继续登录或手工导入。",
            )
        })
}

struct Run {
    serial: u64,
    label: String,
    stop: Arc<AtomicBool>,
}
#[derive(Default)]
struct Shared {
    active: Mutex<Option<Arc<Run>>>,
    closing: AtomicBool,
    next: AtomicU64,
}
#[derive(Default)]
pub(crate) struct Windows {
    shared: Arc<Shared>,
}

#[derive(Clone)]
struct Policy {
    url: Url,
    timeout: Duration,
    #[cfg(feature = "x-login-fixture")]
    fixture: bool,
}
impl Policy {
    fn production() -> Self {
        Self {
            url: LOGIN_URL.parse().expect("fixed X URL"),
            timeout: WINDOW_TIMEOUT,
            #[cfg(feature = "x-login-fixture")]
            fixture: false,
        }
    }
    fn permits(&self, url: &Url) -> bool {
        #[cfg(feature = "x-login-fixture")]
        if self.fixture {
            return url.origin() == self.url.origin()
                && (url.path() == self.url.path()
                    || matches!(url.path(), "/home" | "/account/access"));
        }
        x_navigation(url)
    }
    fn handoff_ready(&self, url: &Url) -> bool {
        #[cfg(feature = "x-login-fixture")]
        if self.fixture {
            return self.permits(url) && url.path() == "/home";
        }
        handoff_ready(url)
    }
}
fn progress(app: &AppHandle, id: &str, message: &'static str, submitted: bool) {
    let _ = app.emit_to(tauri::EventTarget::webview_window("main"), "operator-login",
        json!({"login_id":id,"platform":"x","kind":"progress","data":{"status":if submitted { "pending" } else { "verification_required" },"message":message,"private_input_submitted":submitted}}));
}
fn destroy(app: &AppHandle, label: &str) {
    if let Some(window) = app.get_webview_window(label) {
        // The incognito context is private to this window; never clear main UI data.
        let _ = window.clear_all_browsing_data();
        let _ = window.destroy();
    }
}
impl Windows {
    pub(crate) fn close_all(&self, app: &AppHandle) {
        self.shared.closing.store(true, Ordering::Relaxed);
        if let Ok(active) = self.shared.active.lock() {
            if let Some(run) = active.as_ref() {
                run.stop.store(true, Ordering::Relaxed);
                destroy(app, &run.label);
            }
        }
    }
    fn release(shared: &Shared, serial: u64) {
        if let Ok(mut active) = shared.active.lock() {
            if active.as_ref().is_some_and(|run| run.serial == serial) {
                *active = None;
            }
        }
    }
    fn open_policy(
        &self,
        app: AppHandle,
        ticket: CookieTicket,
        policy: Policy,
    ) -> ApiResult<String> {
        if !ticket.waiting() || self.shared.closing.load(Ordering::Relaxed) {
            return Err(ApiError::new(
                "input_not_requested",
                "本次 X 登录已经结束或不再等待 Cookie。",
            ));
        }
        let run = {
            let mut active = self
                .shared
                .active
                .lock()
                .map_err(|_| ApiError::new("client_state", "窗口状态不可用。"))?;
            if active.is_some() {
                return Err(ApiError::new(
                    "x_window_open",
                    "已有 X 登录窗口，请使用该窗口或先将其关闭。",
                ));
            }
            let serial = self.shared.next.fetch_add(1, Ordering::Relaxed);
            let run = Arc::new(Run {
                serial,
                label: format!("x-login-{serial}"),
                stop: Arc::new(AtomicBool::new(false)),
            });
            *active = Some(run.clone());
            run
        };
        let navigation = policy.clone();
        let handle = app.clone();
        let id = ticket.id().to_owned();
        let builder =
            WebviewWindowBuilder::new(&app, &run.label, WebviewUrl::External(policy.url.clone()))
                .title("X · 人工登录 · 完成后自动核验")
                .inner_size(520.0, 740.0)
                .min_inner_size(400.0, 560.0)
                .center()
                .incognito(true)
                .devtools(false)
                .on_navigation(move |url| {
                    let allowed = navigation.permits(url);
                    if !allowed {
                        progress(
                            &handle,
                            &id,
                            "登录窗口仅允许 x.com。Google 等外部登录请在系统浏览器完成后手工导入。",
                            false,
                        );
                    }
                    allowed
                })
                .on_new_window(|_, _| NewWindowResponse::Deny)
                .on_download(|_, _| false)
                .on_permission_request(|_, _| PermissionResponse::Deny);
        let window = match builder.build() {
            Ok(window) => window,
            Err(_) => {
                Self::release(&self.shared, run.serial);
                return Err(ApiError::new(
                    "x_window_unavailable",
                    "无法打开 X 登录窗口；可继续使用私密导入。",
                ));
            }
        };
        let stop = run.stop.clone();
        window.on_window_event(move |event| {
            if matches!(
                event,
                tauri::WindowEvent::CloseRequested { .. } | tauri::WindowEvent::Destroyed
            ) {
                stop.store(true, Ordering::Relaxed);
            }
        });
        if !ticket.waiting() || self.shared.closing.load(Ordering::Relaxed) {
            destroy(&app, &run.label);
            Self::release(&self.shared, run.serial);
            return Err(ApiError::new(
                "login_session_mismatch",
                "登录会话已经改变，窗口已关闭。",
            ));
        }
        let shared = self.shared.clone();
        let worker_run = run.clone();
        let worker_app = app.clone();
        if thread::Builder::new()
            .name("operator-x-cookies".into())
            .spawn(move || {
                poll(&worker_app, &window, &ticket, &worker_run, &policy);
                destroy(&worker_app, &worker_run.label);
                Self::release(&shared, worker_run.serial);
            })
            .is_err()
        {
            destroy(&app, &run.label);
            Self::release(&self.shared, run.serial);
            return Err(ApiError::new(
                "x_window_unavailable",
                "无法启动网页会话核验；可继续私密导入。",
            ));
        }
        Ok(run.label.clone())
    }
    #[cfg(feature = "x-login-fixture")]
    pub(crate) fn fixture_idle(&self) -> bool {
        self.shared.active.lock().unwrap().is_none()
    }
    #[cfg(feature = "x-login-fixture")]
    pub(crate) fn open_fixture(
        &self,
        app: AppHandle,
        ticket: CookieTicket,
        url: Url,
        timeout: Duration,
    ) -> ApiResult<String> {
        assert_eq!(url.scheme(), "http");
        assert_eq!(url.host_str(), Some("127.0.0.1"));
        self.open_policy(
            app,
            ticket,
            Policy {
                url,
                timeout,
                fixture: true,
            },
        )
    }
}
fn poll(
    app: &AppHandle,
    window: &WebviewWindow,
    ticket: &CookieTicket,
    run: &Run,
    policy: &Policy,
) {
    let deadline = Instant::now() + policy.timeout;
    while ticket.waiting() && !run.stop.load(Ordering::Relaxed) && Instant::now() < deadline {
        if !window.url().is_ok_and(|url| policy.handoff_ready(&url)) {
            thread::sleep(Duration::from_millis(600));
            continue;
        }
        // This thread is never the UI/event thread (WebView2 cookie reads can deadlock there).
        match window
            .cookies_for_url(X_ROOT.parse().expect("fixed X URL"))
            .map(cookie_secret)
        {
            Ok(Ok(Some(secret))) => {
                if run.stop.load(Ordering::Relaxed) {
                    return;
                }
                if window.url().is_ok_and(|url| policy.handoff_ready(&url)) {
                    if ticket.submit(secret).is_ok() {
                        progress(
                            app,
                            ticket.id(),
                            "网页登录信息已通过私有通道提交，等待 Runtime 核验。",
                            true,
                        );
                    }
                    return;
                }
            }
            Ok(Ok(None)) => {}
            _ => {
                if ticket.waiting() {
                    progress(
                        app,
                        ticket.id(),
                        "无法读取完整网页会话；可关闭窗口后重试，或使用私密导入。",
                        false,
                    );
                }
                return;
            }
        }
        thread::sleep(Duration::from_millis(600));
    }
    if ticket.waiting() {
        progress(
            app,
            ticket.id(),
            if Instant::now() >= deadline {
                "X 登录窗口已超时关闭，可重试或使用私密导入。"
            } else {
                "X 登录窗口已关闭，仍可使用私密导入。"
            },
            false,
        );
    }
}

#[tauri::command]
pub(crate) async fn open_x_login_window(
    app: AppHandle,
    login_id: String,
    sessions: State<'_, Sessions>,
    windows: State<'_, Windows>,
) -> ApiResult<()> {
    let ticket = sessions.x_cookie_ticket(&login_id)?;
    let state = Windows {
        shared: windows.shared.clone(),
    };
    tauri::async_runtime::spawn_blocking(move || {
        state
            .open_policy(app, ticket, Policy::production())
            .map(|_| ())
    })
    .await
    .map_err(|_| ApiError::new("x_window_unavailable", "X 登录窗口任务未完成。"))?
}

#[tauri::command]
pub(crate) async fn open_x_in_browser(
    login_id: String,
    sessions: State<'_, Sessions>,
) -> ApiResult<()> {
    // The URL is fixed in Rust. The external browser keeps its own profile;
    // users return to the private input rather than granting us profile access.
    let ticket = sessions.x_cookie_ticket(&login_id)?;
    tauri::async_runtime::spawn_blocking(move || {
        if !ticket.waiting() {
            return Err(ApiError::new(
                "input_not_requested",
                "本次 X 登录已经结束。",
            ));
        }
        crate::bridge::open_browser(LOGIN_URL)
    })
    .await
    .map_err(|_| ApiError::new("browser_unavailable", "系统浏览器任务未完成。"))?
}

#[cfg(test)]
mod tests {
    use super::*;
    fn cookie(name: &str, value: &str) -> Cookie<'static> {
        Cookie::build((name.to_owned(), value.to_owned()))
            .domain(".x.com")
            .path("/")
            .secure(true)
            .http_only(true)
            .build()
    }
    #[test]
    fn exact_https_x_navigation_has_no_external_or_local_escape() {
        assert!(x_navigation(&LOGIN_URL.parse().unwrap()));
        for url in [
            "http://x.com/",
            "https://x.com.evil/",
            "https://x.com:444/",
            "https://a@x.com/",
            "https://www.x.com/",
            "https://accounts.google.com/",
            "tauri://localhost/",
            "file:///tmp/login.html",
            "javascript:alert(1)",
        ] {
            assert!(!x_navigation(&url.parse().unwrap()), "{url}");
        }
    }
    #[test]
    fn cookies_cannot_end_a_login_challenge_before_a_normal_landing() {
        for url in [
            "https://x.com/",
            "https://x.com/home",
            "https://x.com/home/",
        ] {
            assert!(handoff_ready(&url.parse().unwrap()));
        }
        for url in [
            LOGIN_URL,
            "https://x.com/i/flow/challenge",
            "https://x.com/account/access",
            "https://x.com/account/login_challenge",
            "https://accounts.google.com/",
            "http://x.com/home",
        ] {
            assert!(!handoff_ready(&url.parse().unwrap()));
        }
    }
    #[test]
    fn only_exact_required_secure_root_cookies_enter_the_private_input() {
        let secret = cookie_secret(vec![
            cookie("auth_token", "synthetic-auth"),
            cookie("ct0", "synthetic-csrf"),
            cookie("twid", "must-disappear"),
        ])
        .unwrap()
        .unwrap();
        let value: serde_json::Value = serde_json::from_slice(secret.bytes()).unwrap();
        assert_eq!(value.as_object().unwrap().len(), 2);
        assert_eq!(value["auth_token"], "synthetic-auth");
        assert!(
            cookie_secret(vec![cookie("auth_token", "auth")])
                .unwrap()
                .is_none()
        );
        let mut wrong = cookie("ct0", "csrf");
        wrong.set_domain("evil.x.com");
        assert!(
            cookie_secret(vec![cookie("auth_token", "auth"), wrong])
                .unwrap()
                .is_none()
        );
        let mut insecure = cookie("ct0", "csrf");
        insecure.set_secure(false);
        assert!(
            cookie_secret(vec![cookie("auth_token", "auth"), insecure])
                .unwrap()
                .is_none()
        );
        assert!(
            cookie_secret(vec![
                cookie("auth_token", "a"),
                cookie("auth_token", "b"),
                cookie("ct0", "c")
            ])
            .is_err()
        );
    }
}
