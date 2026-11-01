// SPDX-License-Identifier: MIT
//! Human-only Tauri adapter over the shared operator login driver. No platform
//! API, credentials store or browser callback server is implemented here.
use runtime_control::{
    Action, Authorization, CaptchaProof, Challenge, Frame, InputRequired, LoginResult, Platform,
    Request, Secret, SecretInputKind,
    human_ui::{UiOutcome, UiStatus},
};
use runtime_operator::{
    Options,
    login::{ClientUi, Human},
};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use std::{
    fs::File,
    io::Read,
    path::Path,
    process::{Command, Stdio},
    sync::{
        Arc, Mutex,
        atomic::{AtomicBool, Ordering},
        mpsc::{self, Receiver, SyncSender},
    },
    thread::{self, JoinHandle},
    time::{Duration, Instant},
};
use tauri::{AppHandle, Emitter, State};

pub(crate) type ApiResult<T> = std::result::Result<T, ApiError>;
#[derive(Clone, Debug, Serialize)]
pub struct ApiError {
    pub code: String,
    pub message: String,
}
impl ApiError {
    pub(crate) fn new(code: &str, message: &str) -> Self {
        Self {
            code: code.into(),
            message: message.into(),
        }
    }
    fn transport() -> Self {
        Self::new(
            "control_unavailable",
            "无法连接 Runtime。请检查程序路径、状态目录；SSH 需预先配置密钥/agent，并在终端确认主机指纹。",
        )
    }
    fn busy() -> Self {
        Self::new("login_busy", "已有登录正在进行，请完成或取消后再操作。")
    }
}
#[derive(Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Connection {
    mode: String,
    destination: Option<String>,
    runtime_binary: Option<String>,
    state: String,
    account: String,
}
impl Connection {
    fn options(&self, platform: Platform, action: Action, method: &str) -> ApiResult<Options> {
        if !matches!(self.mode.as_str(), "local" | "ssh")
            || self.state.is_empty()
            || self.state.len() > 4096
            || self.state.chars().any(char::is_control)
            || self.account.is_empty()
            || self.account.len() > 128
            || self.account.chars().any(char::is_control)
        {
            return Err(ApiError::new(
                "invalid_connection",
                "连接方式、状态目录或账号别名无效。",
            ));
        }
        let mut args = vec![
            "--state".into(),
            self.state.clone(),
            "--account".into(),
            self.account.clone(),
        ];
        if self.mode == "ssh" {
            let destination = self
                .destination
                .as_ref()
                .filter(|v| !v.is_empty())
                .ok_or_else(|| {
                    ApiError::new("invalid_connection", "请填写已配置的 SSH 主机名。")
                })?;
            args.extend(["--ssh".into(), destination.clone()]);
        }
        if let Some(binary) = self.runtime_binary.as_ref().filter(|v| !v.is_empty()) {
            if binary.len() > 4096 || binary.chars().any(char::is_control) {
                return Err(ApiError::new(
                    "invalid_connection",
                    "Runtime 程序路径无效。",
                ));
            }
            args.extend(["--runtime".into(), binary.clone()]);
        }
        args.extend([
            platform.name().into(),
            match action {
                Action::Capabilities => "capabilities",
                Action::Status => "status",
                Action::Login => "login",
                _ => {
                    return Err(ApiError::new(
                        "unsupported_action",
                        "桌面端未开放此管理动作。",
                    ));
                }
            }
            .into(),
        ]);
        if action == Action::Login {
            args.extend(["--method".into(), method.into()]);
        }
        let mut options = Options::parse(args)
            .map_err(|_| ApiError::new("invalid_connection", "连接参数或登录方式无效。"))?
            .ok_or_else(|| ApiError::new("invalid_connection", "缺少连接参数。"))?;
        // The GUI has no terminal for a manual nonce URL fallback. An explicit
        // helper failure is preferable to leaking a private portal link to logs.
        options.ui = runtime_operator::presentation::UiMode::Webview;
        options.ssh_batch = true;
        Ok(options)
    }
}
fn platform(value: &str) -> ApiResult<Platform> {
    let platform: Platform = value
        .parse()
        .map_err(|_| ApiError::new("unsupported_platform", "平台不受支持。"))?;
    if platform == Platform::Runtime {
        return Err(ApiError::new("unsupported_platform", "请选择具体平台。"));
    }
    Ok(platform)
}
fn random_id() -> ApiResult<String> {
    let mut bytes = [0u8; 16];
    File::open("/dev/urandom")
        .and_then(|mut f| f.read_exact(&mut bytes))
        .map_err(|_| ApiError::new("client_state", "无法创建本次登录标识。"))?;
    Ok(bytes.iter().map(|v| format!("{v:02x}")).collect())
}
struct Input {
    kind: SecretInputKind,
    secret: Secret,
}
struct Active {
    id: String,
    platform: Platform,
    cancel: Arc<AtomicBool>,
    expected: Arc<Mutex<Option<SecretInputKind>>>,
    input: SyncSender<Input>,
    worker: JoinHandle<()>,
}
#[derive(Default)]
struct Shared {
    active: Mutex<Option<Active>>,
    inspection: Mutex<()>,
    shutting_down: AtomicBool,
}
#[derive(Default)]
pub struct Sessions {
    shared: Arc<Shared>,
}
#[tauri::command]
pub async fn control_request(
    connection: Connection,
    command: runtime_control::console::Command,
    sessions: State<'_, Sessions>,
) -> ApiResult<Value> {
    let shared = sessions.shared.clone();
    let options = connection.options(Platform::Runtime, Action::Status, "auto")?;
    tauri::async_runtime::spawn_blocking(move || {
        let _gate = shared
            .inspection
            .lock()
            .map_err(|_| ApiError::transport())?;
        idle(&shared)?;
        let snapshot = matches!(&command, runtime_control::console::Command::Snapshot);
        let mut value = runtime_operator::console::request(&options, command)
            .map_err(|code| ApiError::new(&code, "管理请求未完成，请检查状态或重新读取。"))?;
        if snapshot && options.ssh.is_some() {
            if let Ok(host) = runtime_operator::console::worker_status(&options) {
                value["worker_online"] = host["running"].clone();
                value["worker_source"] = json!("docker");
            }
        }
        Ok(value)
    })
    .await
    .map_err(|_| ApiError::transport())?
}
#[tauri::command]
pub async fn memory_snapshot(
    connection: Connection,
    sessions: State<'_, Sessions>,
) -> ApiResult<Value> {
    let shared = sessions.shared.clone();
    let options = connection.options(Platform::Runtime, Action::Status, "auto")?;
    tauri::async_runtime::spawn_blocking(move || {
        let _gate = shared
            .inspection
            .lock()
            .map_err(|_| ApiError::transport())?;
        idle(&shared)?;
        let result = if options.ssh.is_some() {
            runtime_operator::console::worker_memory(&options)
        } else {
            crate::memory::sample_runtime(&options)
        };
        result.map_err(|code| ApiError::new(&code, "无法读取当前连接的 Runtime 内存。"))
    })
    .await
    .map_err(|_| ApiError::transport())?
}
#[tauri::command]
pub async fn worker_control(
    connection: Connection,
    start: bool,
    sessions: State<'_, Sessions>,
) -> ApiResult<Value> {
    let shared = sessions.shared.clone();
    let options = connection.options(Platform::Runtime, Action::Status, "auto")?;
    tauri::async_runtime::spawn_blocking(move || {
        let _gate = shared
            .inspection
            .lock()
            .map_err(|_| ApiError::transport())?;
        idle(&shared)?;
        runtime_operator::console::worker(&options, start)
            .map_err(|code| ApiError::new(&code, "此部署暂不支持工作进程管理，或服务操作未完成。"))
    })
    .await
    .map_err(|_| ApiError::transport())?
}
impl Sessions {
    pub(crate) fn x_cookie_ticket(&self, id: &str) -> ApiResult<CookieTicket> {
        let ticket = CookieTicket {
            id: id.into(),
            shared: self.shared.clone(),
        };
        if !ticket.waiting() {
            return Err(ApiError::new(
                "input_not_requested",
                "本次 X 登录没有等待 Cookie，或登录已经结束。",
            ));
        }
        Ok(ticket)
    }
    pub fn cancel_all(&self) {
        if self.shared.shutting_down.swap(true, Ordering::Relaxed) {
            return;
        }
        if let Ok(active) = self.shared.active.lock() {
            if let Some(active) = active.as_ref() {
                active.cancel.store(true, Ordering::Relaxed);
            }
        }
        // Cooperatively close the same control session and its helper before
        // exiting. The bounded grace period never waits indefinitely on SSH.
        let deadline = Instant::now() + Duration::from_secs(6);
        loop {
            if let Ok(mut active) = self.shared.active.lock() {
                reap(&mut active);
                if active.is_none() {
                    break;
                }
            }
            if Instant::now() >= deadline {
                break;
            }
            thread::sleep(Duration::from_millis(25));
        }
    }
}
impl Drop for Sessions {
    fn drop(&mut self) {
        self.cancel_all();
    }
}
fn reap(active: &mut Option<Active>) {
    if active.as_ref().is_some_and(|v| v.worker.is_finished()) {
        if let Some(done) = active.take() {
            let _ = done.worker.join();
        }
    }
}
fn idle(shared: &Shared) -> ApiResult<()> {
    if shared.shutting_down.load(Ordering::Relaxed) {
        return Err(ApiError::new("client_closing", "操控端正在退出。"));
    }
    let mut active = shared
        .active
        .lock()
        .map_err(|_| ApiError::new("client_state", "操控端状态暂不可用。"))?;
    reap(&mut active);
    if active.is_some() {
        Err(ApiError::busy())
    } else {
        Ok(())
    }
}
fn public_response(frame: Frame) -> ApiResult<Value> {
    match frame {
        Frame::Response {
            ok: true, result, ..
        } => Ok(result),
        Frame::Response { result, .. } => {
            let code = result["code"]
                .as_str()
                .filter(|v| {
                    v.len() <= 64 && v.chars().all(|c| c.is_ascii_alphanumeric() || c == '_')
                })
                .unwrap_or("operation_failed");
            let message = result["message"]
                .as_str()
                .filter(|v| v.len() <= 2048)
                .unwrap_or("Runtime 拒绝了此操作。");
            Err(ApiError::new(code, message))
        }
        _ => Err(ApiError::transport()),
    }
}
async fn inspect(
    connection: Connection,
    selected: String,
    action: Action,
    shared: Arc<Shared>,
) -> ApiResult<Value> {
    let selected = platform(&selected)?;
    let options = connection.options(selected, action, "auto")?;
    tauri::async_runtime::spawn_blocking(move || {
        // A single Runtime state is exclusive; dashboard refreshes must not
        // launch seven competing core processes against the same directory.
        let _gate = shared
            .inspection
            .lock()
            .map_err(|_| ApiError::transport())?;
        idle(&shared)?;
        let request = Request::new(1, selected, options.account.clone(), action);
        let frame = runtime_operator::request_once(
            &options,
            &request,
            Instant::now() + Duration::from_secs(20),
        )
        .map_err(|_| ApiError::transport())?;
        public_response(frame)
    })
    .await
    .map_err(|_| ApiError::transport())?
}
#[tauri::command]
pub async fn platform_capabilities(
    connection: Connection,
    platform: String,
    sessions: State<'_, Sessions>,
) -> ApiResult<Value> {
    inspect(
        connection,
        platform,
        Action::Capabilities,
        sessions.shared.clone(),
    )
    .await
}
#[tauri::command]
pub async fn account_status(
    connection: Connection,
    platform: String,
    sessions: State<'_, Sessions>,
) -> ApiResult<Value> {
    inspect(
        connection,
        platform,
        Action::Status,
        sessions.shared.clone(),
    )
    .await
}
#[derive(Serialize)]
pub struct Started {
    login_id: String,
}
#[tauri::command]
pub async fn begin_login(
    app: AppHandle,
    connection: Connection,
    platform: String,
    method: String,
    secret: Option<String>,
    sessions: State<'_, Sessions>,
) -> ApiResult<Started> {
    let shared = sessions.shared.clone();
    tauri::async_runtime::spawn_blocking(move || {
        start_login(app, connection, platform, method, secret, shared)
    })
    .await
    .map_err(|_| ApiError::transport())?
}
fn start_login(
    app: AppHandle,
    connection: Connection,
    platform: String,
    method: String,
    secret: Option<String>,
    shared: Arc<Shared>,
) -> ApiResult<Started> {
    let platform = self::platform(&platform)?;
    let options = connection.options(platform, Action::Login, &method)?;
    if secret
        .as_ref()
        .is_some_and(|s| s.len() > runtime_control::MAX_SECRET)
    {
        return Err(ApiError::new(
            "invalid_private_input",
            "私密输入超出大小限制。",
        ));
    }
    let _gate = shared
        .inspection
        .lock()
        .map_err(|_| ApiError::transport())?;
    let mut active = shared.active.lock().map_err(|_| ApiError::transport())?;
    if shared.shutting_down.load(Ordering::Relaxed) {
        return Err(ApiError::new("client_closing", "操控端正在退出。"));
    }
    reap(&mut active);
    if active.is_some() {
        return Err(ApiError::busy());
    }
    let id = random_id()?;
    let cancel = Arc::new(AtomicBool::new(false));
    let expected = Arc::new(Mutex::new(None));
    let (sender, receiver) = mpsc::sync_channel(1);
    let worker_id = id.clone();
    let worker_cancel = cancel.clone();
    let worker_expected = expected.clone();
    let worker = thread::Builder::new().name("operator-desktop-login".into()).spawn(move || {
        let native = ClientUi::new(&options);
        let mut ui = DesktopUi { app, id: worker_id, platform, native, cancel: worker_cancel, expected: worker_expected, receiver, initial: secret, authorization_seen: None };
        ui.event_data("progress", json!({"status":"checking","message":"正在核实已有登录态。"}));
        let outcome = runtime_operator::login::run_with_ui(options, &mut ui);
        ui.close();
        let result = match outcome {
            Ok(result) => safe_result(result),
            Err(_) => json!({"status":"failed","error":{"code":"control_unavailable","message":"操控连接或验证界面不可用。SSH 首版需已配置密钥/agent 与主机指纹；未自动切换登录方式。"}}),
        };
        ui.event_data("finished", result);
    }).map_err(|_| ApiError::new("client_state", "无法启动登录任务。"))?;
    *active = Some(Active {
        id: id.clone(),
        platform,
        cancel,
        expected,
        input: sender,
        worker,
    });
    Ok(Started { login_id: id })
}
/// Session-bound private capability. Never serialized or sent to JavaScript.
#[derive(Clone)]
pub(crate) struct CookieTicket {
    id: String,
    shared: Arc<Shared>,
}
impl CookieTicket {
    pub(crate) fn waiting(&self) -> bool {
        if self.shared.shutting_down.load(Ordering::Relaxed) {
            return false;
        }
        let Ok(active) = self.shared.active.lock() else {
            return false;
        };
        active.as_ref().is_some_and(|active| {
            active.id == self.id
                && active.platform == Platform::X
                && !active.cancel.load(Ordering::Relaxed)
                && !active.worker.is_finished()
                && active
                    .expected
                    .lock()
                    .is_ok_and(|expected| *expected == Some(SecretInputKind::Cookies))
        })
    }
    pub(crate) fn submit(&self, secret: Secret) -> ApiResult<()> {
        let active = self
            .shared
            .active
            .lock()
            .map_err(|_| ApiError::transport())?;
        let active = active
            .as_ref()
            .filter(|active| {
                active.id == self.id
                    && active.platform == Platform::X
                    && !active.cancel.load(Ordering::Relaxed)
                    && !active.worker.is_finished()
            })
            .ok_or_else(|| ApiError::new("login_session_mismatch", "本次登录已经结束。"))?;
        let mut expected = active.expected.lock().map_err(|_| ApiError::transport())?;
        if self.shared.shutting_down.load(Ordering::Relaxed)
            || *expected != Some(SecretInputKind::Cookies)
        {
            return Err(ApiError::new(
                "input_not_requested",
                "本次登录已不再等待 Cookie。",
            ));
        }
        active
            .input
            .try_send(Input {
                kind: SecretInputKind::Cookies,
                secret,
            })
            .map_err(|_| ApiError::new("input_already_supplied", "登录信息已提交，请等待核验。"))?;
        *expected = None;
        Ok(())
    }
    pub(crate) fn id(&self) -> &str {
        &self.id
    }
}
#[cfg(feature = "x-login-fixture")]
pub(crate) struct FixtureInput {
    receiver: Receiver<Input>,
}
#[cfg(feature = "x-login-fixture")]
impl FixtureInput {
    pub(crate) fn receive(&self, timeout: Duration) -> Option<Secret> {
        self.receiver
            .recv_timeout(timeout)
            .ok()
            .map(|input| input.secret)
    }
}
#[cfg(feature = "x-login-fixture")]
impl Sessions {
    // Only the separately feature-gated synthetic executable calls these hooks.
    // It never starts the Runtime or reads account state.
    pub(crate) fn fixture_request(&self, id: &str) -> (CookieTicket, FixtureInput) {
        let cancel = Arc::new(AtomicBool::new(false));
        let stop = cancel.clone();
        let (input, receiver) = mpsc::sync_channel(1);
        let worker = thread::spawn(move || {
            while !stop.load(Ordering::Relaxed) {
                thread::sleep(Duration::from_millis(10));
            }
        });
        let mut active = self.shared.active.lock().unwrap();
        assert!(active.is_none());
        *active = Some(Active {
            id: id.into(),
            platform: Platform::X,
            cancel,
            expected: Arc::new(Mutex::new(Some(SecretInputKind::Cookies))),
            input,
            worker,
        });
        (
            CookieTicket {
                id: id.into(),
                shared: self.shared.clone(),
            },
            FixtureInput { receiver },
        )
    }
    pub(crate) fn fixture_end(&self) {
        let active = self.shared.active.lock().unwrap().take();
        if let Some(active) = active {
            active.cancel.store(true, Ordering::Relaxed);
            let _ = active.worker.join();
        }
    }
}
fn input_kind(value: &str) -> ApiResult<SecretInputKind> {
    serde_json::from_value(Value::String(value.into()))
        .map_err(|_| ApiError::new("invalid_private_input", "私密输入类型无效。"))
}
fn secret_input(kind: SecretInputKind, value: String) -> ApiResult<Secret> {
    if value.len() > runtime_control::MAX_SECRET {
        return Err(ApiError::new(
            "invalid_private_input",
            "私密输入超出大小限制。",
        ));
    }
    let bytes = match kind {
        SecretInputKind::Password => {
            let input: Value = serde_json::from_str(&value)
                .map_err(|_| ApiError::new("invalid_private_input", "请填写有效 QQ 号和密码。"))?;
            if input["uin"]
                .as_u64()
                .is_none_or(|n| n == 0 || n > u32::MAX as u64)
                || input["password"]
                    .as_str()
                    .is_none_or(|s| s.is_empty() || s.len() > 4096)
            {
                return Err(ApiError::new(
                    "invalid_private_input",
                    "请填写有效 QQ 号和密码。",
                ));
            }
            serde_json::to_vec(&input)
                .map_err(|_| ApiError::new("invalid_private_input", "无法准备私密输入。"))?
        }
        SecretInputKind::AuthorizationCode => {
            let code = value.trim();
            if code.is_empty() || code.len() > 8192 || !code.bytes().all(|b| b.is_ascii_graphic()) {
                return Err(ApiError::new(
                    "invalid_private_input",
                    "授权码为空或格式无效。",
                ));
            }
            serde_json::to_vec(&json!({"authorization_code":code}))
                .map_err(|_| ApiError::new("invalid_private_input", "无法准备授权码。"))?
        }
        SecretInputKind::Token => {
            let token = value.trim();
            if token.is_empty()
                || token.len() > 4096
                || !token.bytes().all(|b| b.is_ascii_graphic())
            {
                return Err(ApiError::new(
                    "invalid_private_input",
                    "Token 为空或格式无效。",
                ));
            }
            token.as_bytes().to_vec()
        }
        SecretInputKind::Cookies => {
            return runtime_control::human_ui::cookie_secret(value.into_bytes()).map_err(|_| {
                ApiError::new(
                    "invalid_private_input",
                    "请粘贴含 auth_token、ct0 的有效 Cookie 请求头，或填写这两项。",
                )
            });
        }
        SecretInputKind::Session => {
            runtime_operator::private_input(Path::new(&value)).map_err(|_| {
                ApiError::new(
                    "invalid_private_input",
                    "登录态文件必须是操控端本机的0600私有普通文件。",
                )
            })?
        }
    };
    Secret::new(bytes)
        .map_err(|_| ApiError::new("invalid_private_input", "私密输入为空或超出限制。"))
}
#[tauri::command]
pub fn submit_login_input(
    login_id: String,
    kind: String,
    value: String,
    sessions: State<'_, Sessions>,
) -> ApiResult<()> {
    let active = sessions
        .shared
        .active
        .lock()
        .map_err(|_| ApiError::transport())?;
    let active = active
        .as_ref()
        .filter(|v| v.id == login_id)
        .ok_or_else(|| ApiError::new("login_session_mismatch", "本次登录已结束或标识不匹配。"))?;
    let kind = input_kind(&kind)?;
    let mut expected = active.expected.lock().map_err(|_| ApiError::transport())?;
    if *expected != Some(kind) {
        return Err(ApiError::new(
            "input_not_requested",
            "当前登录没有请求这种私密输入。",
        ));
    }
    let secret = secret_input(kind, value)?;
    active
        .input
        .try_send(Input { kind, secret })
        .map_err(|_| ApiError::new("input_already_supplied", "输入已提交，请等待登录结果。"))?;
    *expected = None;
    Ok(())
}
#[tauri::command]
pub fn cancel_login(login_id: String, sessions: State<'_, Sessions>) -> ApiResult<()> {
    let active = sessions
        .shared
        .active
        .lock()
        .map_err(|_| ApiError::transport())?;
    let active = active
        .as_ref()
        .filter(|v| v.id == login_id)
        .ok_or_else(|| ApiError::new("login_session_mismatch", "本次登录已结束或标识不匹配。"))?;
    active.cancel.store(true, Ordering::Relaxed);
    Ok(())
}
fn safe_result(result: LoginResult) -> Value {
    let mut value = serde_json::to_value(result).unwrap_or_else(|_| json!({"status":"failed"}));
    if let Some(object) = value.as_object_mut() {
        for key in ["authorization", "qr", "challenge", "input_required"] {
            object.remove(key);
        }
    }
    value
}
struct DesktopUi {
    app: AppHandle,
    id: String,
    platform: Platform,
    native: ClientUi,
    cancel: Arc<AtomicBool>,
    expected: Arc<Mutex<Option<SecretInputKind>>>,
    receiver: Receiver<Input>,
    initial: Option<String>,
    authorization_seen: Option<String>,
}
impl DesktopUi {
    fn event_data(&self, kind: &str, data: Value) {
        let _ = self.app.emit_to(
            tauri::EventTarget::webview_window("main"),
            "operator-login",
            json!({"login_id":self.id,"platform":self.platform.name(),"kind":kind,"data":data}),
        );
    }
}
fn authorization_url(url: &str) -> bool {
    if url.len() > 16384 || url.chars().any(char::is_control) {
        return false;
    }
    let Some(tail) = url.strip_prefix("https://") else {
        return false;
    };
    let host = tail.split(['/', '?', '#']).next().unwrap_or("");
    !host.contains('@')
        && !host.contains(':')
        && !host.contains('\\')
        && (matches!(
            host,
            "auth.openai.com" | "chatgpt.com" | "accounts.google.com" | "qq.com"
        ) || host.ends_with(".qq.com"))
}
pub(crate) fn open_browser(url: &str) -> ApiResult<()> {
    let mut child = Command::new(if cfg!(target_os = "macos") {
        "open"
    } else {
        "xdg-open"
    })
    .arg(url)
    .stdin(Stdio::null())
    .stdout(Stdio::null())
    .stderr(Stdio::null())
    .spawn()
    .map_err(|_| {
        ApiError::new(
            "browser_unavailable",
            "无法打开系统浏览器，请检查默认浏览器设置。",
        )
    })?;
    thread::spawn(move || {
        let _ = child.wait();
    });
    Ok(())
}
impl Human for DesktopUi {
    fn event(&mut self, event: &str, data: &Value) -> runtime_control::Result<()> {
        if event == "authorization" {
            return self.authorization(
                &serde_json::from_value(data.clone()).map_err(|_| "invalid authorization event")?,
            );
        }
        if event == "qr_ready" {
            self.qr(data)?;
        }
        if matches!(event, "qr_ready" | "qr_status") {
            self.event_data("progress", json!({"status":"verification_required","message":"请在独立验证窗口完成扫码或确认。"}));
        }
        Ok(())
    }
    fn authorization(&mut self, value: &Authorization) -> runtime_control::Result<()> {
        let key = format!(
            "{}\n{}",
            value.url.as_deref().unwrap_or(""),
            value.user_code.as_deref().unwrap_or("")
        );
        if self.authorization_seen.as_ref() == Some(&key) {
            return Ok(());
        }
        self.event_data("authorization", json!({"message":value.message.as_deref().unwrap_or("请在系统浏览器中完成授权。"),"user_code":value.user_code}));
        if let Some(url) = &value.url {
            if !authorization_url(url) {
                return Err("authorization origin rejected".into());
            }
            open_browser(url).map_err(|_| "system browser unavailable")?;
        }
        self.authorization_seen = Some(key);
        Ok(())
    }
    fn qr(&mut self, value: &Value) -> runtime_control::Result<()> {
        self.native.qr(value)
    }
    fn captcha(
        &mut self,
        challenge: &Challenge,
        _: &AtomicBool,
        deadline: Instant,
    ) -> runtime_control::Result<UiOutcome<CaptchaProof>> {
        self.event_data(
            "progress",
            json!({"status":"verification_required","message":"请在独立验证窗口完成人工验证。"}),
        );
        self.native.captcha(challenge, &self.cancel, deadline)
    }
    fn input(
        &mut self,
        required: &InputRequired,
        deadline: Instant,
    ) -> runtime_control::Result<Option<Secret>> {
        if let Some(initial) = self.initial.take() {
            return secret_input(required.kind, initial)
                .map(Some)
                .map_err(|_| "invalid private GUI input".into());
        }
        *self
            .expected
            .lock()
            .map_err(|_| "input state unavailable")? = Some(required.kind);
        self.event_data(
            "input_required",
            json!({"kind":required.kind,"message":required.message}),
        );
        let result = loop {
            if self.cancel.load(Ordering::Relaxed) {
                break Ok(None);
            }
            if Instant::now() >= deadline {
                break Err("operator_timeout".into());
            }
            match self.receiver.recv_timeout(Duration::from_millis(100)) {
                Ok(input) if input.kind == required.kind => break Ok(Some(input.secret)),
                Ok(_) => break Err("private input kind changed".into()),
                Err(mpsc::RecvTimeoutError::Timeout) => {}
                Err(_) => break Ok(None),
            }
        };
        if let Ok(mut expected) = self.expected.lock() {
            *expected = None;
        }
        result
    }
    fn status(&self) -> UiStatus {
        if self.cancel.load(Ordering::Relaxed) {
            UiStatus::Cancelled
        } else {
            self.native.status()
        }
    }
    fn close(&mut self) {
        self.native.close();
        if let Ok(mut expected) = self.expected.lock() {
            *expected = None;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn gui_input_is_typed_and_private_results_omit_authentication_capabilities() {
        assert!(
            secret_input(
                SecretInputKind::Password,
                r#"{"uin":42,"password":"synthetic"}"#.into()
            )
            .is_ok()
        );
        assert!(
            secret_input(
                SecretInputKind::Password,
                r#"{"uin":0,"password":""}"#.into()
            )
            .is_err()
        );
        assert_eq!(
            serde_json::from_slice::<Value>(
                secret_input(SecretInputKind::AuthorizationCode, "4/synthetic".into())
                    .unwrap()
                    .bytes()
            )
            .unwrap()["authorization_code"],
            "4/synthetic"
        );
        let result: LoginResult = serde_json::from_value(json!({"status":"pending","authorization":{"url":"https://accounts.google.com/private","user_code":"1234"},"challenge":{"kind":"captcha","challenge_id":"synthetic","expires_at":1,"verify_url":"private"},"qr":{"image_base64":"private"}})).unwrap();
        let safe = safe_result(result).to_string();
        assert!(!safe.contains("private") && !safe.contains("1234"));
    }
    #[test]
    fn provider_links_cannot_become_arbitrary_shell_or_file_actions() {
        assert!(authorization_url(
            "https://accounts.google.com/o/oauth2/auth?state=synthetic"
        ));
        for url in [
            "file:///etc/passwd",
            "javascript:alert(1)",
            "https://accounts.google.com.evil/",
            "https://accounts.google.com@evil/",
            "https://evil/",
            "https://auth.openai.com/\nsecret",
        ] {
            assert!(!authorization_url(url));
        }
    }
}
