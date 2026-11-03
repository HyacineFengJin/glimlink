// SPDX-License-Identifier: MIT
//! One login driver for local and SSH transports. The core selects and owns the
//! authentication flow; only this client presents human UI or reads secrets.
use crate::{
    Connection, Options, interrupt,
    presentation::{LocalAction, Presenter},
    private_input,
};
use runtime_control::{
    Action, Frame, LoginCapabilities, LoginError, Platform, Request, Result, human_ui,
};
pub use runtime_control::{
    Authorization, CaptchaProof, Challenge, InputRequired, LoginResult, LoginStatus, Secret,
    SecretInputKind,
    human_ui::{UiOutcome, UiStatus},
};
use serde_json::{Value, json};
use std::{
    cell::RefCell,
    fs::{File, OpenOptions},
    io::{Read, Write},
    os::fd::AsRawFd,
    sync::atomic::{AtomicBool, Ordering},
    time::{Duration, Instant, SystemTime, UNIX_EPOCH},
};

trait Rpc {
    fn call(
        &mut self,
        request: &Request,
        deadline: Instant,
        interruptible: bool,
        event: &mut dyn FnMut(&str, &Value) -> Result<()>,
        check: &mut dyn FnMut() -> Result<()>,
    ) -> Result<Frame>;
}
impl Rpc for Connection {
    fn call(
        &mut self,
        request: &Request,
        deadline: Instant,
        interruptible: bool,
        event: &mut dyn FnMut(&str, &Value) -> Result<()>,
        check: &mut dyn FnMut() -> Result<()>,
    ) -> Result<Frame> {
        self.request_until_checked(request, deadline, interruptible, event, check)
    }
}
/// Human presentation and private input for one login flow. Implementations may
/// wait on a bounded GUI channel; check their own cancellation and the supplied
/// deadline during that wait. status() is polled during RPC reads and writes.
pub trait Human {
    fn event(&mut self, event: &str, data: &Value) -> Result<()>;
    fn authorization(&mut self, value: &Authorization) -> Result<()>;
    fn qr(&mut self, value: &Value) -> Result<()>;
    fn captcha(
        &mut self,
        challenge: &Challenge,
        cancel: &AtomicBool,
        deadline: Instant,
    ) -> Result<UiOutcome<CaptchaProof>>;
    fn input(&mut self, required: &InputRequired, deadline: Instant) -> Result<Option<Secret>>;
    fn status(&self) -> UiStatus;
    fn close(&mut self);
}
pub struct ClientUi {
    options: Options,
    portal: Option<human_ui::QrPortal>,
    qr_seen: Option<String>,
    authorization_seen: Option<String>,
    file_used: bool,
    presentation: Presenter,
    browser_lease: Option<crate::browser_login::BrowserLease>,
}
fn now() -> Result<u64> {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|time| time.as_secs())
        .map_err(|_| "invalid operator clock".into())
}
fn text(value: &str) -> String {
    value
        .chars()
        .filter(|c| !c.is_control() || matches!(c, '\n' | '\t'))
        .take(4096)
        .collect()
}
fn authorization_url(url: &str) -> Result<()> {
    if url.len() > 16384 || url.chars().any(char::is_control) || !url.starts_with("https://") {
        return Err("authorization URL must be bounded HTTPS".into());
    }
    let host = url[8..].split(['/', '?', '#']).next().unwrap_or("");
    if host.is_empty() || host.contains('@') {
        return Err("authorization URL authority rejected".into());
    }
    Ok(())
}
impl ClientUi {
    pub fn browser_context(&self) -> crate::browser_login::CaptureContext {
        crate::browser_login::CaptureContext::from_options(&self.options)
    }
    /// Reusable local QR/CAPTCHA presenter. GUI wrappers should override input
    /// and authorization as needed; constructing this adapter performs no input.
    pub fn new(options: &Options) -> Self {
        Self {
            options: options.clone(),
            portal: None,
            qr_seen: None,
            authorization_seen: None,
            file_used: false,
            presentation: Presenter::new(options.ui),
            browser_lease: None,
        }
    }
    fn cookies(&mut self, deadline: Instant) -> Result<Option<Secret>> {
        self.close();
        if !self.options.interactive {
            // --no-ui disables browser pages, while an explicit X cookie flow
            // can still use the controlling terminal without echoing secrets.
            let input = tty(
                "X Cookie header or minimal JSON (hidden; Enter cancels): ",
                true,
                32768,
                deadline,
            )?;
            if input.is_empty() {
                return Ok(None);
            }
            return normalize_input(SecretInputKind::Cookies, input.into_bytes()).map(Some);
        }
        let timeout = deadline.saturating_duration_since(Instant::now()).as_secs();
        let result = {
            let presentation = RefCell::new(&mut self.presentation);
            let open = self.options.open_browser;
            human_ui::cookies_with_cancel_check(
                deadline,
                &interrupt::CANCELLED,
                |url| {
                    let action = presentation.borrow_mut().local(url, open, timeout)?;
                    if let LocalAction::Browser { open } = action {
                        crate::browser(url, open);
                    }
                    Ok(())
                },
                || presentation.borrow().status(),
            )
        };
        self.close();
        match result? {
            UiOutcome::Completed(secret) => Ok(Some(secret)),
            UiOutcome::Cancelled => Ok(None),
            UiOutcome::TimedOut => Err("operator_timeout".into()),
        }
    }
}
impl Human for ClientUi {
    fn event(&mut self, event: &str, data: &Value) -> Result<()> {
        match event {
            "authorization" => self.authorization(
                &serde_json::from_value(data.clone()).map_err(|_| "invalid authorization event")?,
            ),
            "qr_ready" => self.qr(data),
            "message" => {
                if let Some(message) = data["message"].as_str() {
                    eprintln!("{}", text(message));
                }
                Ok(())
            }
            "qr_status" => {
                if let Some(status) = data["status"].as_str() {
                    eprintln!("QR: {}", text(status));
                }
                Ok(())
            }
            _ => Ok(()),
        }
    }
    fn authorization(&mut self, value: &Authorization) -> Result<()> {
        let key = format!(
            "{}\n{}",
            value.url.as_deref().unwrap_or(""),
            value.user_code.as_deref().unwrap_or("")
        );
        if self.authorization_seen.as_ref() == Some(&key) {
            return Ok(());
        }
        if let Some(message) = &value.message {
            eprintln!("{}", text(message));
        }
        if let Some(code) = &value.user_code {
            if code.len() > 256 || code.chars().any(char::is_control) {
                return Err("invalid authorization user code".into());
            }
            eprintln!("Authorization code: {code}");
        }
        if let Some(url) = &value.url {
            authorization_url(url)?;
            // Provider authorization, including Google OAuth, is always external.
            self.close();
            crate::browser(url, self.options.open_browser && self.options.interactive);
        }
        self.authorization_seen = Some(key);
        Ok(())
    }
    fn qr(&mut self, value: &Value) -> Result<()> {
        if !self.options.interactive {
            return Err(
                "QR login requires client human UI; use --no-open for manual opening".into(),
            );
        }
        let key = serde_json::to_string(&json!({"qr_url":value.get("qr_url"),"image_base64":value.get("image_base64"),"mime":value.get("mime")})).map_err(|_| "invalid QR presentation")?;
        if self.qr_seen.as_ref() == Some(&key) {
            return Ok(());
        }
        self.close();
        let timeout = value
            .get("expires_at")
            .and_then(Value::as_u64)
            .map(|expiry| expiry.saturating_sub(now().unwrap_or(expiry)))
            .or_else(|| value.get("expires_in_seconds").and_then(Value::as_u64))
            .unwrap_or(self.options.timeout_secs)
            .min(self.options.timeout_secs);
        let presentation = &mut self.presentation;
        let open = self.options.open_browser;
        let mut opened = Ok(());
        let portal = human_ui::qr(value, |url| {
            opened = presentation.local(url, open, timeout).map(|action| {
                if let LocalAction::Browser { open } = action {
                    crate::browser(url, open);
                }
            });
        })?;
        opened?;
        self.portal = Some(portal);
        self.qr_seen = Some(key);
        Ok(())
    }
    fn captcha(
        &mut self,
        challenge: &Challenge,
        cancel: &AtomicBool,
        deadline: Instant,
    ) -> Result<UiOutcome<CaptchaProof>> {
        if !self.options.interactive {
            return Ok(UiOutcome::Cancelled);
        }
        self.close();
        let mut bounded = challenge.clone();
        bounded.expires_at = bounded.expires_at.min(
            now()?
                .saturating_add(deadline.saturating_duration_since(Instant::now()).as_secs())
                .min(u64::from(u32::MAX)) as u32,
        );
        let timeout = u64::from(bounded.expires_at)
            .saturating_sub(now()?)
            .min(self.options.timeout_secs);
        let presentation = RefCell::new(&mut self.presentation);
        let open = self.options.open_browser;
        human_ui::captcha_with_cancel_check(
            &bounded,
            cancel,
            |url| {
                let action = presentation.borrow_mut().local(url, open, timeout)?;
                if let LocalAction::Browser { open } = action {
                    crate::browser(url, open);
                }
                Ok(())
            },
            || presentation.borrow().status(),
        )
    }
    fn input(&mut self, required: &InputRequired, deadline: Instant) -> Result<Option<Secret>> {
        if let Some(message) = &required.message {
            eprintln!("{}", text(message));
        }
        let bytes = if !self.file_used {
            if let Some(file) = &self.options.secret_file {
                self.file_used = true;
                Some(private_input(file)?)
            } else {
                None
            }
        } else {
            None
        };
        if let Some(bytes) = bytes {
            return normalize_input(required.kind, bytes).map(Some);
        }
        if required.kind == SecretInputKind::BrowserSession {
            if !self.options.open_browser {
                return Err("browser_session_requires_browser_or_secret_file".into());
            }
            let handoff=crate::browser_login::capture(self.options.platform, self.browser_context(), deadline, &interrupt::CANCELLED, |status| {
                eprintln!("Browser login: {status}");
            })?;
            return Ok(handoff.map(|handoff|{let (secret,lease)=handoff.into_parts();self.browser_lease=Some(lease);secret}));
        }
        if required.kind == SecretInputKind::Cookies {
            return self.cookies(deadline);
        }
        if !self.options.interactive {
            return Ok(None);
        }
        match required.kind {
            SecretInputKind::Password => {
                let uin = tty("QQ account UIN: ", false, 16, deadline)?
                    .trim()
                    .parse::<u32>()
                    .map_err(|_| "QQ account UIN must be a positive integer")?;
                if uin == 0 {
                    return Err("QQ account UIN must be positive".into());
                }
                let password = tty("QQ password (hidden): ", true, 4096, deadline)?;
                if password.is_empty() {
                    return Ok(None);
                }
                Secret::new(
                    serde_json::to_vec(&json!({"uin":uin,"password":password}))
                        .map_err(|_| "cannot encode private password input")?,
                )
                .map(Some)
            }
            SecretInputKind::AuthorizationCode | SecretInputKind::Token => {
                let prompt = if required.kind == SecretInputKind::AuthorizationCode {
                    "Authorization code (hidden; Enter cancels): "
                } else {
                    "Token (hidden; Enter cancels): "
                };
                let value = tty(prompt, true, 16384, deadline)?;
                if value.trim().is_empty() {
                    Ok(None)
                } else {
                    normalize_input(required.kind, value.into_bytes()).map(Some)
                }
            }
            SecretInputKind::Session => {
                Err("session import requires a private --secret-file".into())
            }
            SecretInputKind::Cookies => unreachable!("cookie input handled before page policy"),
            SecretInputKind::BrowserSession => unreachable!("browser input handled before page policy"),
        }
    }
    fn status(&self) -> UiStatus {
        let status = self.presentation.status();
        if status != UiStatus::Active {
            return status;
        }
        self.portal
            .as_ref()
            .map_or(UiStatus::Active, |portal| portal.status())
    }
    fn close(&mut self) {
        self.browser_lease=None;
        self.presentation.close();
        self.portal = None;
    }
}
fn normalize_input(kind: SecretInputKind, bytes: Vec<u8>) -> Result<Secret> {
    if kind == SecretInputKind::BrowserSession {
        return Secret::new(bytes);
    }
    if kind == SecretInputKind::Cookies {
        return human_ui::cookie_secret(bytes);
    }
    if kind == SecretInputKind::AuthorizationCode {
        if let Ok(value) = serde_json::from_slice::<Value>(&bytes) {
            if value.as_object().is_some_and(|object| {
                object.len() == 1
                    && object
                        .get("authorization_code")
                        .is_some_and(Value::is_string)
            }) {
                return Secret::new(bytes);
            }
            return Err("authorization input JSON must contain only authorization_code".into());
        }
        let code = String::from_utf8(bytes).map_err(|_| "authorization code must be UTF-8")?;
        let code = code.trim();
        if code.is_empty() || code.len() > 16384 || code.chars().any(char::is_control) {
            return Err("authorization code is empty or invalid".into());
        }
        return Secret::new(
            serde_json::to_vec(&json!({"authorization_code":code}))
                .map_err(|_| "cannot encode authorization input")?,
        );
    }
    Secret::new(bytes)
}
struct EchoGuard {
    fd: libc::c_int,
    old: libc::termios,
}
impl Drop for EchoGuard {
    fn drop(&mut self) {
        unsafe {
            libc::tcsetattr(self.fd, libc::TCSANOW, &self.old);
        }
    }
}
fn tty(prompt: &str, hidden: bool, max: usize, deadline: Instant) -> Result<String> {
    // Never consume stdin: it may belong to an embedding operator or an RPC
    // transport. The controlling terminal is the only interactive secret input.
    let mut terminal: File = OpenOptions::new()
        .read(true)
        .write(true)
        .open("/dev/tty")
        .map_err(|_| "no controlling terminal; provide a private --secret-file")?;
    let guard = if hidden {
        let fd = terminal.as_raw_fd();
        let mut old: libc::termios = unsafe { std::mem::zeroed() };
        if unsafe { libc::tcgetattr(fd, &mut old) } != 0 {
            return Err("cannot hide terminal input".into());
        }
        let mut changed = old;
        changed.c_lflag &= !(libc::ECHO | libc::ECHONL);
        if unsafe { libc::tcsetattr(fd, libc::TCSANOW, &changed) } != 0 {
            return Err("cannot hide terminal input".into());
        }
        Some(EchoGuard { fd, old })
    } else {
        None
    };
    terminal
        .write_all(prompt.as_bytes())
        .and_then(|_| terminal.flush())
        .map_err(|_| "cannot prompt on controlling terminal")?;
    let mut bytes = Vec::new();
    let mut byte = [0];
    loop {
        if interrupt::is_interrupted() {
            drop(guard);
            let _ = terminal.write_all(b"\n");
            return Err("operator_cancelled".into());
        }
        if Instant::now() >= deadline {
            return Err("operator_timeout".into());
        }
        let mut ready = libc::pollfd {
            fd: terminal.as_raw_fd(),
            events: libc::POLLIN,
            revents: 0,
        };
        if unsafe { libc::poll(&mut ready, 1, 100) } <= 0 {
            continue;
        }
        match terminal.read(&mut byte) {
            Ok(0) => break,
            Ok(_) => {
                if matches!(byte[0], b'\n' | b'\r') {
                    break;
                }
                if bytes.len() >= max {
                    return Err("private terminal input exceeds limit".into());
                }
                bytes.push(byte[0]);
            }
            Err(error) if error.kind() == std::io::ErrorKind::Interrupted => continue,
            Err(_) => return Err("private terminal input read failed".into()),
        }
    }
    drop(guard);
    if hidden {
        let _ = terminal.write_all(b"\n");
    }
    String::from_utf8(bytes).map_err(|_| "private terminal input must be UTF-8".into())
}

fn response(frame: Frame) -> Result<LoginResult> {
    match frame {
        Frame::Response {
            ok: true, result, ..
        } => serde_json::from_value(result).map_err(|_| "invalid common login result".into()),
        Frame::Response { result, .. } => {
            let mut failed = LoginResult::new(LoginStatus::Failed);
            failed.error = Some(LoginError {
                code: result["code"].as_str().unwrap_or("operation_failed").into(),
                message: result["message"]
                    .as_str()
                    .unwrap_or("Core rejected the login operation")
                    .into(),
            });
            Ok(failed)
        }
        _ => Err("expected operator response".into()),
    }
}
fn fail(status: LoginStatus, code: &str, message: &str) -> LoginResult {
    let mut result = LoginResult::new(status);
    result.error = Some(LoginError {
        code: code.into(),
        message: message.into(),
    });
    result
}
fn cancel(
    rpc: &mut impl Rpc,
    request: &mut Request,
    login_id: Option<&str>,
    code: &str,
    message: &str,
) -> LoginResult {
    if let Some(login_id) = login_id {
        request.id += 1;
        request.action = Action::CancelLogin;
        request.params = json!({"login_id":login_id});
        request.secret = None;
        if let Ok(Frame::Response {
            ok: true, result, ..
        }) = rpc.call(
            request,
            Instant::now() + Duration::from_secs(3),
            false,
            &mut |_, _| Ok(()),
            &mut || Ok(()),
        ) {
            // Cancellation may race a native completion. The core's completed
            // authenticated/failed result is authoritative; never overwrite it
            // with a local cancellation after credentials were already saved.
            if let Ok(completed) = serde_json::from_value::<LoginResult>(result) {
                if matches!(
                    completed.status,
                    LoginStatus::Authenticated | LoginStatus::Failed
                ) {
                    return completed;
                }
            }
        }
    }
    let status = if matches!(code, "operator_cancelled" | "operator_timeout") {
        LoginStatus::Cancelled
    } else {
        LoginStatus::Failed
    };
    let mut result = fail(status, code, message);
    result.login_id = login_id.map(str::to_owned);
    result
}
fn call_ui(
    rpc: &mut impl Rpc,
    request: &Request,
    deadline: Instant,
    ui: &mut (impl Human + ?Sized),
    cancelled: &AtomicBool,
) -> Result<Frame> {
    let ui = RefCell::new(ui);
    rpc.call(
        request,
        deadline,
        false,
        &mut |kind, data| ui.borrow_mut().event(kind, data),
        &mut || {
            if cancelled.load(Ordering::Relaxed) {
                return Err("operator_cancelled".into());
            }
            match ui.borrow().status() {
                UiStatus::Cancelled => Err("operator_cancelled".into()),
                UiStatus::TimedOut => Err("operator_timeout".into()),
                UiStatus::Failed => Err("client_ui_failed".into()),
                UiStatus::Active => Ok(()),
            }
        },
    )
}
fn drive(
    options: &Options,
    rpc: &mut impl Rpc,
    ui: &mut (impl Human + ?Sized),
    initial_secret: Option<Secret>,
) -> Result<LoginResult> {
    drive_with_cancel(options, rpc, ui, initial_secret, &AtomicBool::new(false))
}
fn drive_with_cancel(
    options: &Options,
    rpc: &mut impl Rpc,
    ui: &mut (impl Human + ?Sized),
    initial_secret: Option<Secret>,
    cancelled: &AtomicBool,
) -> Result<LoginResult> {
    let mut request = Request::new(
        1,
        options.platform,
        options.account.clone(),
        Action::Capabilities,
    );
    let mut deadline = Instant::now() + Duration::from_secs(options.timeout_secs);
    let frame = call_ui(rpc, &request, deadline, ui, cancelled)?;
    let capabilities: LoginCapabilities = match frame {
        Frame::Response {
            ok: true, result, ..
        } => serde_json::from_value(result).map_err(|_| "invalid login capabilities")?,
        frame => return response(frame),
    };
    if capabilities.platform != options.platform {
        return Err("login capabilities platform differs".into());
    }
    if capabilities.paused {
        return Ok(fail(
            LoginStatus::Failed,
            "paused",
            capabilities
                .message
                .as_deref()
                .unwrap_or("This platform's login is paused"),
        ));
    }
    if options.method != runtime_control::LoginMethod::Auto
        && !capabilities.methods.contains(&options.method)
    {
        return Ok(fail(
            LoginStatus::Failed,
            "unsupported_method",
            "This backend does not support the explicitly selected login method",
        ));
    }
    if let Some(message) = capabilities.message {
        ui.event("message", &json!({"message":message}))?;
    }
    request.id += 1;
    request.action = Action::Login;
    request.params = json!({"method":options.method});
    request.secret = initial_secret;
    if let Some(profile) = &options.profile {
        request.params["app_profile"] = json!(profile);
    }
    let mut active: Option<String> = None;
    let mut seen_captcha: Option<String> = None;
    loop {
        let frame = match call_ui(
            rpc,
            &request,
            deadline.min(Instant::now() + Duration::from_secs(90)),
            ui,
            cancelled,
        ) {
            Ok(frame) => frame,
            Err(error) => {
                return Ok(cancel(
                    rpc,
                    &mut request,
                    active.as_deref(),
                    if error == "operator_timeout" {
                        "operator_timeout"
                    } else if error == "operator_cancelled" {
                        "operator_cancelled"
                    } else {
                        "client_transport_failed"
                    },
                    if error == "operator_timeout" {
                        "The login deadline elapsed"
                    } else if error == "operator_cancelled" {
                        "Login was cancelled"
                    } else {
                        "The operator transport or human presentation failed"
                    },
                ));
            }
        };
        request.secret = None;
        let value = response(frame)?;
        if value.status.is_terminal() {
            ui.close();
            return Ok(value);
        }
        let login_id = value
            .login_id
            .as_deref()
            .filter(|id| !id.is_empty() && id.len() <= 128)
            .ok_or("pending login is missing its opaque login_id")?;
        if active.as_deref().is_some_and(|saved| saved != login_id) {
            return Err("login flow identity changed during polling".into());
        }
        active = Some(login_id.into());
        for expires in [
            value.expires_at,
            value
                .authorization
                .as_ref()
                .and_then(|auth| auth.expires_at),
            value
                .challenge
                .as_ref()
                .map(|challenge| u64::from(challenge.expires_at)),
            value.qr.as_ref().and_then(|qr| qr.expires_at),
        ]
        .into_iter()
        .flatten()
        {
            let remaining = expires.saturating_sub(now()?);
            deadline = deadline.min(Instant::now() + Duration::from_secs(remaining.min(3600)));
        }
        if ui.status() == UiStatus::Failed {
            return Ok(cancel(
                rpc,
                &mut request,
                active.as_deref(),
                "client_ui_failed",
                "The client verification window failed",
            ));
        }
        if cancelled.load(Ordering::Relaxed) || ui.status() == UiStatus::Cancelled {
            return Ok(cancel(
                rpc,
                &mut request,
                active.as_deref(),
                "operator_cancelled",
                "Login was cancelled",
            ));
        }
        if Instant::now() >= deadline || ui.status() == UiStatus::TimedOut {
            return Ok(cancel(
                rpc,
                &mut request,
                active.as_deref(),
                "operator_timeout",
                "The login deadline elapsed",
            ));
        }
        let present = (|| -> Result<()> {
            if let Some(authorization) = &value.authorization {
                ui.authorization(authorization)?;
            }
            if let Some(qr) = &value.qr {
                ui.qr(&serde_json::to_value(qr).map_err(|_| "invalid login QR presentation")?)?;
            }
            Ok(())
        })();
        if let Err(error) = present {
            return Ok(cancel(
                rpc,
                &mut request,
                active.as_deref(),
                if error == "operator_cancelled" || cancelled.load(Ordering::Relaxed) {
                    "operator_cancelled"
                } else {
                    "client_ui_failed"
                },
                "The client could not present the requested human verification",
            ));
        }
        request.id += 1;
        request.action = Action::LoginPoll;
        request.params = json!({"login_id":login_id});
        if ui.status() == UiStatus::Failed {
            return Ok(cancel(
                rpc,
                &mut request,
                active.as_deref(),
                "client_ui_failed",
                "The client verification window failed",
            ));
        }
        if cancelled.load(Ordering::Relaxed) || ui.status() == UiStatus::Cancelled {
            return Ok(cancel(
                rpc,
                &mut request,
                active.as_deref(),
                "operator_cancelled",
                "Login was cancelled",
            ));
        }
        if Instant::now() >= deadline || ui.status() == UiStatus::TimedOut {
            return Ok(cancel(
                rpc,
                &mut request,
                active.as_deref(),
                "operator_timeout",
                "The login deadline elapsed",
            ));
        }
        if let Some(challenge) = &value.challenge {
            if challenge.kind == "captcha" {
                if seen_captcha.as_deref() == Some(&challenge.challenge_id) {
                    return Ok(cancel(
                        rpc,
                        &mut request,
                        active.as_deref(),
                        "repeated_challenge",
                        "The submitted verification did not complete; no automatic fresh login was started",
                    ));
                }
                seen_captcha = Some(challenge.challenge_id.clone());
                match ui.captcha(challenge, cancelled, deadline) {
                    Ok(UiOutcome::Completed(proof)) => {
                        request.params["challenge_id"] = json!(challenge.challenge_id);
                        request.secret=Some(Secret::new(serde_json::to_vec(&json!({"captcha":{"ticket":proof.ticket,"randstr":proof.randstr,"session_id":proof.session_id}})).map_err(|_|"cannot encode private challenge proof")?)?);
                        continue;
                    }
                    Ok(UiOutcome::TimedOut) => {
                        return Ok(cancel(
                            rpc,
                            &mut request,
                            active.as_deref(),
                            "operator_timeout",
                            "The human verification deadline elapsed",
                        ));
                    }
                    Ok(UiOutcome::Cancelled) => {
                        return Ok(cancel(
                            rpc,
                            &mut request,
                            active.as_deref(),
                            "operator_cancelled",
                            "Human verification was cancelled",
                        ));
                    }
                    Err(error) => {
                        return Ok(cancel(
                            rpc,
                            &mut request,
                            active.as_deref(),
                            if error == "operator_cancelled" || cancelled.load(Ordering::Relaxed) {
                                "operator_cancelled"
                            } else {
                                "client_ui_failed"
                            },
                            "Human verification could not be completed in the client",
                        ));
                    }
                }
            } else if !challenge.verify_url.is_empty() {
                if ui.authorization(&Authorization{url:Some(challenge.verify_url.clone()),message:Some("Complete the official verification; the core will poll this same login flow".into()),..Default::default()}).is_err(){return Ok(cancel(rpc,&mut request,active.as_deref(),"client_ui_failed","Official verification could not be presented"));}
            }
        }
        if let Some(required) = &value.input_required {
            match ui.input(required, deadline) {
                Ok(Some(secret)) => {
                    request.secret = Some(secret);
                    continue;
                }
                Ok(None) => {
                    return Ok(cancel(
                        rpc,
                        &mut request,
                        active.as_deref(),
                        "operator_cancelled",
                        "No private input was supplied",
                    ));
                }
                Err(error) => {
                    return Ok(cancel(
                        rpc,
                        &mut request,
                        active.as_deref(),
                        if error == "operator_cancelled" || cancelled.load(Ordering::Relaxed) {
                            "operator_cancelled"
                        } else if error == "operator_timeout" {
                            "operator_timeout"
                        } else {
                            "private_input_unavailable"
                        },
                        "Private input was unavailable; use a private file or the client's controlling terminal",
                    ));
                }
            }
        }
        let delay = Duration::from_millis(value.poll_after_ms.unwrap_or(1000).max(100));
        let wake = Instant::now()
            .checked_add(delay)
            .unwrap_or(deadline)
            .min(deadline);
        while Instant::now() < wake {
            if ui.status() == UiStatus::Failed {
                return Ok(cancel(
                    rpc,
                    &mut request,
                    active.as_deref(),
                    "client_ui_failed",
                    "The client verification window failed",
                ));
            }
            if cancelled.load(Ordering::Relaxed) || ui.status() == UiStatus::Cancelled {
                return Ok(cancel(
                    rpc,
                    &mut request,
                    active.as_deref(),
                    "operator_cancelled",
                    "Login was cancelled",
                ));
            }
            if ui.status() == UiStatus::TimedOut {
                return Ok(cancel(
                    rpc,
                    &mut request,
                    active.as_deref(),
                    "operator_timeout",
                    "The QR deadline elapsed",
                ));
            }
            std::thread::sleep(
                wake.saturating_duration_since(Instant::now())
                    .min(Duration::from_millis(40)),
            );
        }
        if Instant::now() >= deadline {
            return Ok(cancel(
                rpc,
                &mut request,
                active.as_deref(),
                "operator_timeout",
                "The login deadline elapsed",
            ));
        }
    }
}

fn typed_result(result: Result<LoginResult>) -> Result<LoginResult> {
    match result {
        Ok(result) => Ok(result),
        Err(error) if error == "operator_cancelled" || error == "operator_timeout" => Ok(fail(
            LoginStatus::Cancelled,
            &error,
            if error == "operator_timeout" {
                "The login deadline elapsed"
            } else {
                "Login was cancelled"
            },
        )),
        Err(error) => Err(error),
    }
}
/// Drive one persistent login using caller-owned GUI/events/private input. This
/// installs no signal handler, reads no file/TTY and prints no output. SSH is
/// noninteractive and preserves strict host verification. Human.status() is the
/// per-operation cancellation source, also checked during transport waits.
pub fn run_with_ui(mut options: Options, ui: &mut (impl Human + ?Sized)) -> Result<LoginResult> {
    options.ssh_batch = true;
    let mut connection = Connection::open(&options)?;
    let result = drive(&options, &mut connection, ui, None);
    ui.close();
    let result = typed_result(result)?;
    let finished = connection.finish();
    if result.status == LoginStatus::Authenticated {
        finished?;
    }
    Ok(result)
}

pub fn run(options: Options) -> Result<i32> {
    let deferred_input = matches!(
        options.platform,
        Platform::Codex | Platform::Antigravity | Platform::X
    );
    let initial = if deferred_input {
        None
    } else {
        options
            .secret_file
            .as_deref()
            .map(private_input)
            .transpose()?
            .map(Secret::new)
            .transpose()?
    };
    let mut connection = Connection::open(&options)?;
    let mut ui = ClientUi::new(&options);
    ui.file_used = !deferred_input && options.secret_file.is_some();
    let result = drive_with_cancel(
        &options,
        &mut connection,
        &mut ui,
        initial,
        &interrupt::CANCELLED,
    );
    ui.close();
    let result = match result {
        Ok(result) => result,
        Err(error) if error == "operator_cancelled" || error == "operator_timeout" => fail(
            LoginStatus::Cancelled,
            &error,
            if error == "operator_timeout" {
                "The login deadline elapsed"
            } else {
                "Login was cancelled"
            },
        ),
        Err(error) => return Err(error),
    };
    let code = match result.status {
        LoginStatus::Authenticated => 0,
        LoginStatus::Failed
            if result
                .error
                .as_ref()
                .is_some_and(|error| error.code == "login_expired") =>
        {
            124
        }
        LoginStatus::Cancelled
            if result
                .error
                .as_ref()
                .is_some_and(|error| error.code == "operator_timeout") =>
        {
            124
        }
        LoginStatus::Cancelled => 130,
        _ => 1,
    };
    let mut safe =
        serde_json::to_value(&result).map_err(|_| "cannot encode common login result")?;
    crate::display_safe(&mut safe);
    println!(
        "{}",
        serde_json::to_string(&Frame::success(connection.last_request_id, safe))
            .map_err(|_| "cannot encode login output")?
    );
    // EOF is also a cancellation boundary for server-owned workers. A process
    // still finishing cancellation is killed by Connection's bounded drop.
    let finished = connection.finish();
    if code == 0 {
        finished?;
    }
    Ok(code)
}

#[cfg(test)]
mod tests {
    use super::*;
    use runtime_control::LoginMethod;
    use std::collections::VecDeque;
    struct Recorded {
        action: Action,
        params: Value,
        secret: Option<Value>,
    }
    struct Fixture {
        replies: VecDeque<Value>,
        requests: Vec<Recorded>,
    }
    impl Fixture {
        fn new(replies: Vec<Value>) -> Self {
            Self {
                replies: replies.into(),
                requests: Vec::new(),
            }
        }
    }
    impl Rpc for Fixture {
        fn call(
            &mut self,
            request: &Request,
            _: Instant,
            _: bool,
            _: &mut dyn FnMut(&str, &Value) -> Result<()>,
            check: &mut dyn FnMut() -> Result<()>,
        ) -> Result<Frame> {
            check()?;
            request.validate()?;
            self.requests.push(Recorded {
                action: request.action,
                params: request.params.clone(),
                secret: request
                    .secret
                    .as_ref()
                    .map(|secret| serde_json::from_slice(secret.bytes()).unwrap()),
            });
            Ok(Frame::success(
                request.id,
                self.replies
                    .pop_front()
                    .expect("unexpected extra authentication request"),
            ))
        }
    }
    struct HumanFixture {
        inputs: VecDeque<Secret>,
        proof: Option<CaptchaProof>,
        cancel: bool,
        pages: usize,
        authorizations: usize,
    }
    impl HumanFixture {
        fn new() -> Self {
            Self {
                inputs: VecDeque::new(),
                proof: None,
                cancel: false,
                pages: 0,
                authorizations: 0,
            }
        }
    }
    impl Human for HumanFixture {
        fn event(&mut self, _: &str, _: &Value) -> Result<()> {
            Ok(())
        }
        fn authorization(&mut self, _: &Authorization) -> Result<()> {
            self.authorizations += 1;
            Ok(())
        }
        fn qr(&mut self, _: &Value) -> Result<()> {
            self.pages += 1;
            Ok(())
        }
        fn captcha(
            &mut self,
            _: &Challenge,
            _: &AtomicBool,
            _: Instant,
        ) -> Result<UiOutcome<CaptchaProof>> {
            if self.cancel {
                Ok(UiOutcome::Cancelled)
            } else {
                Ok(UiOutcome::Completed(
                    self.proof.take().expect("unexpected second CAPTCHA"),
                ))
            }
        }
        fn input(&mut self, _: &InputRequired, _: Instant) -> Result<Option<Secret>> {
            Ok(self.inputs.pop_front())
        }
        fn status(&self) -> UiStatus {
            UiStatus::Active
        }
        fn close(&mut self) {}
    }
    fn options(platform: &str) -> Options {
        Options::parse(
            ["--no-open", "--account", "synthetic", platform, "login"].map(str::to_owned),
        )
        .unwrap()
        .unwrap()
    }
    fn caps(platform: &str, methods: Value) -> Value {
        json!({"platform":platform,"methods":methods,"reuse":true})
    }
    fn pending(status: &str) -> Value {
        json!({"status":status,"login_id":"synthetic-flow","poll_after_ms":1,"expires_at":now().unwrap()+120})
    }
    fn authenticated() -> Value {
        json!({"status":"authenticated","identity":{"id":"42","label":"synthetic"}})
    }
    #[test]
    fn closed_optional_window_interrupts_captcha_without_waiting_for_http() {
        use crate::presentation::UiMode;
        use std::{fs, os::unix::fs::PermissionsExt};
        let helper = std::env::current_exe()
            .unwrap()
            .parent()
            .unwrap()
            .join(format!("captcha-window-fixture-{}", std::process::id()));
        fs::write(&helper,b"#!/bin/sh\nIFS= read -r request\nprintf '%s\\n' '{\"version\":1,\"event\":\"ready\"}' '{\"version\":1,\"event\":\"closed\"}'\n").unwrap();
        fs::set_permissions(&helper, fs::Permissions::from_mode(0o700)).unwrap();
        let mut options = options("qq");
        options.ui = UiMode::Webview;
        options.open_browser = true;
        let mut human = ClientUi {
            options: options.clone(),
            portal: None,
            qr_seen: None,
            authorization_seen: None,
            file_used: false,
            presentation: Presenter::fixture(UiMode::Webview, helper.clone()),
            browser_lease:None,
        };
        let challenge=Challenge{kind:"captcha".into(),challenge_id:"synthetic-fingerprint".into(),expires_at:(now().unwrap()+120) as u32,verify_url:"https://ti.qq.com/safe/tools/captcha/sms-verify-login?aid=123&login_appid=456&sid=raw%2Bsid&uin=42".into()};
        let started = Instant::now();
        assert!(matches!(
            human
                .captcha(
                    &challenge,
                    &AtomicBool::new(false),
                    started + Duration::from_secs(120)
                )
                .unwrap(),
            UiOutcome::Cancelled
        ));
        assert!(started.elapsed() < Duration::from_secs(2));
        human.close();
        fs::remove_file(helper).unwrap();
    }
    #[test]
    fn oauth_uses_external_browser_path_even_when_webview_is_explicit() {
        let mut options = options("agy");
        options.ui = crate::presentation::UiMode::Webview;
        let mut human = ClientUi {
            options: options.clone(),
            portal: None,
            qr_seen: None,
            authorization_seen: None,
            file_used: false,
            presentation: Presenter::fixture(options.ui, "/not/a/helper".into()),
            browser_lease:None,
        };
        // --no-open is set by options(), so this only exercises routing and
        // printing a synthetic URL; no browser or native helper is launched.
        human
            .authorization(&Authorization {
                url: Some("https://accounts.google.com/o/oauth2/auth?synthetic=1".into()),
                ..Default::default()
            })
            .unwrap();
        assert_eq!(human.status(), UiStatus::Active);
    }
    #[test]
    fn public_gui_driver_uses_caller_input_and_never_reads_configured_secret_file() {
        use std::{fs, os::unix::fs::PermissionsExt};
        let fixture = std::env::current_exe()
            .unwrap()
            .parent()
            .unwrap()
            .join(format!("gui-core-fixture-{}", std::process::id()));
        fs::write(&fixture,b"#!/bin/sh\nIFS= read -r request\nprintf '%s\\n' '{\"type\":\"response\",\"version\":1,\"id\":1,\"ok\":true,\"result\":{\"platform\":\"github\",\"methods\":[],\"reuse\":true}}'\nIFS= read -r request\ncase \"$request\" in *'\"secret\"'*) exit 91 ;; esac\nprintf '%s\\n' '{\"type\":\"response\",\"version\":1,\"id\":2,\"ok\":true,\"result\":{\"status\":\"verification_required\",\"login_id\":\"gui-flow\",\"input_required\":{\"kind\":\"token\"}}}'\nIFS= read -r request\nprintf '%s\\n' '{\"type\":\"response\",\"version\":1,\"id\":3,\"ok\":true,\"result\":{\"status\":\"authenticated\",\"identity\":{\"id\":\"42\"}}}'\nwhile IFS= read -r request; do :; done\n").unwrap();
        fs::set_permissions(&fixture, fs::Permissions::from_mode(0o700)).unwrap();
        let mut options = options("github");
        options.runtime = Some(fixture.clone());
        options.secret_file = Some("/this/gui-hook/must/not/read/input".into());
        options.interactive = false;
        let mut ui = HumanFixture::new();
        ui.inputs
            .push_back(Secret::new(b"synthetic-gui-token".to_vec()).unwrap());
        let result = run_with_ui(options, &mut ui).unwrap();
        assert_eq!(result.status, LoginStatus::Authenticated);
        assert_eq!(result.identity.unwrap().id, "42");
        assert!(ui.inputs.is_empty());
        fs::remove_file(fixture).unwrap();
    }
    #[test]
    fn saved_login_finishes_without_reuse_probe_or_human_input() {
        let mut backend = Fixture::new(vec![
            caps("bilibili", json!(["qr"])),
            json!({"status":"authenticated","reused_session":true,"identity":{"id":"42"}}),
        ]);
        let mut human = HumanFixture::new();
        let result = drive(&options("bili"), &mut backend, &mut human, None).unwrap();
        assert_eq!(result.status, LoginStatus::Authenticated);
        assert_eq!(result.reused_session, Some(true));
        assert_eq!(
            backend
                .requests
                .iter()
                .map(|r| r.action)
                .collect::<Vec<_>>(),
            [Action::Capabilities, Action::Login]
        );
        assert!(
            backend
                .requests
                .iter()
                .all(|request| request.secret.is_none())
        );
        assert_eq!(human.pages + human.authorizations, 0);
    }
    #[test]
    fn x_web_session_reuses_first_and_submits_cookies_only_on_the_same_private_flow() {
        let mut saved = Fixture::new(vec![
            caps("x", json!(["auto"])),
            json!({"status":"authenticated","reused_session":true}),
        ]);
        let mut human = HumanFixture::new();
        assert_eq!(
            drive(&options("twitter"), &mut saved, &mut human, None)
                .unwrap()
                .reused_session,
            Some(true)
        );
        assert_eq!(saved.requests.len(), 2);
        assert!(
            saved
                .requests
                .iter()
                .all(|request| request.secret.is_none())
        );
        let mut input = pending("verification_required");
        input["input_required"] = json!({"kind":"cookies"});
        let mut backend = Fixture::new(vec![caps("x", json!(["auto"])), input, authenticated()]);
        human.inputs.push_back(
            normalize_input(
                SecretInputKind::Cookies,
                br#"{"auth_token":"synthetic-auth","ct0":"synthetic-csrf"}"#.to_vec(),
            )
            .unwrap(),
        );
        assert_eq!(
            drive(&options("x"), &mut backend, &mut human, None)
                .unwrap()
                .status,
            LoginStatus::Authenticated
        );
        assert_eq!(
            backend
                .requests
                .iter()
                .map(|request| request.action)
                .collect::<Vec<_>>(),
            [Action::Capabilities, Action::Login, Action::LoginPoll]
        );
        assert!(backend.requests[1].secret.is_none());
        assert_eq!(
            backend.requests[2].params,
            json!({"login_id":"synthetic-flow"})
        );
        assert_eq!(
            backend.requests[2].secret.as_ref().unwrap()["auth_token"],
            "synthetic-auth"
        );
        assert_eq!(human.pages + human.authorizations, 0);
    }
    #[test]
    fn x_cli_reuse_does_not_read_a_configured_private_file() {
        use std::{fs, os::unix::fs::PermissionsExt};
        let fixture = std::env::current_exe()
            .unwrap()
            .parent()
            .unwrap()
            .join(format!("x-reuse-fixture-{}", std::process::id()));
        fs::write(&fixture,b"#!/bin/sh\nIFS= read -r request\nprintf '%s\\n' '{\"type\":\"response\",\"version\":1,\"id\":1,\"ok\":true,\"result\":{\"platform\":\"x\",\"methods\":[\"auto\"],\"reuse\":true}}'\nIFS= read -r request\ncase \"$request\" in *'\"secret\"'*) exit 91 ;; esac\nprintf '%s\\n' '{\"type\":\"response\",\"version\":1,\"id\":2,\"ok\":true,\"result\":{\"status\":\"authenticated\",\"reused_session\":true}}'\nwhile IFS= read -r request; do :; done\n").unwrap();
        fs::set_permissions(&fixture, fs::Permissions::from_mode(0o700)).unwrap();
        let mut options = options("x");
        options.runtime = Some(fixture.clone());
        options.secret_file = Some("/this/x/reuse/must/not/read/input".into());
        assert_eq!(run(options).unwrap(), 0);
        fs::remove_file(fixture).unwrap();
    }
    #[test]
    fn x_private_cookie_file_precedes_any_browser_or_tty_input() {
        use std::{fs, os::unix::fs::PermissionsExt};
        let file = std::env::current_exe()
            .unwrap()
            .parent()
            .unwrap()
            .join(format!("x-cookies-fixture-{}", std::process::id()));
        fs::write(
            &file,
            br#"{"auth_token":"synthetic-auth","ct0":"synthetic-csrf"}"#,
        )
        .unwrap();
        fs::set_permissions(&file, fs::Permissions::from_mode(0o600)).unwrap();
        let mut config = options("x");
        config.secret_file = Some(file.clone());
        config.interactive = false;
        let mut ui = ClientUi::new(&config);
        let secret = ui
            .input(
                &InputRequired {
                    kind: SecretInputKind::Cookies,
                    message: None,
                },
                Instant::now() + Duration::from_secs(2),
            )
            .unwrap()
            .unwrap();
        assert!(ui.file_used);
        assert_eq!(
            serde_json::from_slice::<Value>(secret.bytes()).unwrap()["ct0"],
            "synthetic-csrf"
        );
        fs::remove_file(file).unwrap();
    }
    #[test]
    fn qq_same_flow_accepts_initial_password_then_only_captcha_proof() {
        let mut input = pending("verification_required");
        input["input_required"] = json!({"kind":"password"});
        let mut verification = pending("verification_required");
        verification["challenge"] = json!({"kind":"captcha","challenge_id":"synthetic-fingerprint","expires_at":now().unwrap()+120,"verify_url":"https://ti.qq.com/safe/tools/captcha/sms-verify-login?aid=123&login_appid=456&sid=raw%2Bsid&uin=42"});
        let mut backend = Fixture::new(vec![
            caps("qq", json!(["password", "qr"])),
            input,
            verification,
            authenticated(),
        ]);
        let mut human = HumanFixture::new();
        human.inputs.push_back(
            Secret::new(br#"{"uin":42,"password":"synthetic-password"}"#.to_vec()).unwrap(),
        );
        human.proof = Some(CaptchaProof {
            ticket: "synthetic-ticket".into(),
            randstr: "synthetic-rand".into(),
            widget_app_id: "123".into(),
            session_id: "raw%2Bsid".into(),
        });
        let mut config = options("qq");
        config.method = LoginMethod::Password;
        assert_eq!(
            drive(&config, &mut backend, &mut human, None)
                .unwrap()
                .status,
            LoginStatus::Authenticated
        );
        assert_eq!(
            backend
                .requests
                .iter()
                .map(|r| r.action)
                .collect::<Vec<_>>(),
            [
                Action::Capabilities,
                Action::Login,
                Action::LoginPoll,
                Action::LoginPoll
            ]
        );
        assert_eq!(
            backend.requests[2].secret.as_ref().unwrap()["password"],
            "synthetic-password"
        );
        let proof = backend.requests[3].secret.as_ref().unwrap();
        assert_eq!(
            proof,
            &json!({"captcha":{"ticket":"synthetic-ticket","randstr":"synthetic-rand","session_id":"raw%2Bsid"}})
        );
        assert_eq!(
            backend.requests[2].params["login_id"],
            backend.requests[3].params["login_id"]
        );
        assert_eq!(
            backend.requests[3].params["challenge_id"],
            "synthetic-fingerprint"
        );
        assert!(backend.requests[3].params.get("captcha").is_none());
    }
    #[test]
    fn engine_browser_authorization_then_private_code_and_async_poll() {
        let mut start = pending("pending");
        start["authorization"] = json!({"url":"https://accounts.google.com/o/oauth2/auth?synthetic=1","expires_at":now().unwrap()+120,"message":"Synthetic human authorization"});
        let mut input = pending("verification_required");
        input["input_required"] = json!({"kind":"authorization_code"});
        let mut backend = Fixture::new(vec![
            caps("antigravity", json!(["browser"])),
            start,
            input,
            authenticated(),
        ]);
        let mut human = HumanFixture::new();
        human.inputs.push_back(
            normalize_input(
                SecretInputKind::AuthorizationCode,
                b"synthetic-one-use-code".to_vec(),
            )
            .unwrap(),
        );
        assert_eq!(
            drive(&options("agy"), &mut backend, &mut human, None)
                .unwrap()
                .status,
            LoginStatus::Authenticated
        );
        assert_eq!(human.authorizations, 1);
        assert_eq!(human.pages, 0);
        assert!(backend.requests[2].secret.is_none());
        assert_eq!(
            backend.requests[3].secret.as_ref().unwrap(),
            &json!({"authorization_code":"synthetic-one-use-code"})
        );
    }
    #[test]
    fn backend_network_or_risk_failure_never_triggers_a_new_method() {
        for code in ["network_error", "risk_control", "rate_limited"] {
            let mut backend = Fixture::new(vec![
                caps("qq", json!(["password", "qr"])),
                json!({"status":"failed","error":{"code":code,"message":"Synthetic terminal failure"}}),
            ]);
            let result =
                drive(&options("qq"), &mut backend, &mut HumanFixture::new(), None).unwrap();
            assert_eq!(result.status, LoginStatus::Failed);
            assert_eq!(backend.requests.len(), 2);
            assert_eq!(backend.requests[1].action, Action::Login);
        }
    }
    #[test]
    fn cancelled_human_or_expired_qr_cancels_exact_flow_without_secret() {
        let mut verification = pending("verification_required");
        verification["challenge"] = json!({"kind":"captcha","challenge_id":"synthetic","expires_at":now().unwrap()+120,"verify_url":"https://example.invalid/synthetic"});
        let mut backend = Fixture::new(vec![
            caps("qq", json!(["password"])),
            verification,
            json!({"status":"cancelled"}),
        ]);
        let mut human = HumanFixture::new();
        human.cancel = true;
        let result = drive(&options("qq"), &mut backend, &mut human, None).unwrap();
        assert_eq!(result.status, LoginStatus::Cancelled);
        assert_eq!(result.error.unwrap().code, "operator_cancelled");
        assert_eq!(backend.requests.last().unwrap().action, Action::CancelLogin);
        assert_eq!(
            backend.requests.last().unwrap().params,
            json!({"login_id":"synthetic-flow"})
        );
        assert!(backend.requests.last().unwrap().secret.is_none());
        let mut expired = pending("pending");
        expired["expires_at"] = json!(now().unwrap().saturating_sub(1));
        expired["qr"] = json!({"qr_url":"https://example.invalid/synthetic"});
        let mut backend = Fixture::new(vec![
            caps("bilibili", json!(["qr"])),
            expired,
            json!({"status":"cancelled"}),
        ]);
        let mut human = HumanFixture::new();
        let result = drive(&options("bili"), &mut backend, &mut human, None).unwrap();
        assert_eq!(result.error.unwrap().code, "operator_timeout");
        assert_eq!(human.pages, 0);
        assert_eq!(backend.requests.last().unwrap().action, Action::CancelLogin);
    }
    #[test]
    fn paused_or_unsupported_method_has_no_login_side_effect() {
        let mut paused = caps("weixin", json!([]));
        paused["paused"] = json!(true);
        let mut backend = Fixture::new(vec![paused]);
        assert_eq!(
            drive(
                &options("weixin"),
                &mut backend,
                &mut HumanFixture::new(),
                None
            )
            .unwrap()
            .error
            .unwrap()
            .code,
            "paused"
        );
        assert_eq!(backend.requests.len(), 1);
        let mut config = options("bili");
        config.method = LoginMethod::Password;
        let mut backend = Fixture::new(vec![caps("bilibili", json!(["qr"]))]);
        assert_eq!(
            drive(&config, &mut backend, &mut HumanFixture::new(), None)
                .unwrap()
                .error
                .unwrap()
                .code,
            "unsupported_method"
        );
        assert_eq!(backend.requests.len(), 1);
    }
    #[test]
    fn native_terminal_completion_wins_a_racing_page_cancellation() {
        let mut verification = pending("verification_required");
        verification["challenge"] = json!({"kind":"captcha","challenge_id":"synthetic","expires_at":now().unwrap()+120,"verify_url":"https://example.invalid/synthetic"});
        let mut backend = Fixture::new(vec![
            caps("qq", json!(["password"])),
            verification,
            authenticated(),
        ]);
        let mut human = HumanFixture::new();
        human.cancel = true;
        let result = drive(&options("qq"), &mut backend, &mut human, None).unwrap();
        assert_eq!(result.status, LoginStatus::Authenticated);
        assert_eq!(result.identity.unwrap().id, "42");
        assert_eq!(backend.requests.last().unwrap().action, Action::CancelLogin);
    }
}
