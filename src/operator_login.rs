// SPDX-License-Identifier: MIT
//! Stateful human authentication. A control pipe owns its challenges and native
//! connections; the remote core never serves HTML or launches a browser.
use crate::{app::Runtime, operator_control::PrivateInput, operator_engines::EngineLoginManager};
use runtime_control::{
    AccountIdentity, Action, InputRequired, LoginCapabilities, LoginError, LoginMethod,
    LoginResult, LoginStatus, Platform, QrPresentation, Request, SecretInputKind,
};
use runtime_core::Result;
use serde_json::{Value, json};
use std::{
    fs::File,
    io::Read,
    path::{Path, PathBuf},
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
        mpsc,
    },
    thread::{self, JoinHandle},
    time::{Duration, Instant, SystemTime, UNIX_EPOCH},
};

fn now() -> Result<u64> {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|v| v.as_secs())
        .map_err(|_| "invalid clock".into())
}
fn login_id() -> Result<String> {
    let mut bytes = [0u8; 24];
    File::open("/dev/urandom")
        .and_then(|mut file| file.read_exact(&mut bytes))
        .map_err(|_| "cannot create login ID")?;
    Ok(bytes.iter().map(|b| format!("{b:02x}")).collect())
}
fn encode(value: impl serde::Serialize) -> Result<Value> {
    serde_json::to_value(value).map_err(|_| "cannot encode login result".into())
}
fn failed(code: &str, message: &str) -> LoginResult {
    let mut result = LoginResult::new(LoginStatus::Failed);
    result.error = Some(LoginError {
        code: code.into(),
        message: message.into(),
    });
    result
}
fn is_engine(platform: Platform) -> bool {
    matches!(platform, Platform::Codex | Platform::Antigravity)
}

enum WorkerMessage {
    Event(String, Value),
    Done(Result<Value>),
}
struct QrWorker {
    stop: Arc<AtomicBool>,
    receiver: mpsc::Receiver<WorkerMessage>,
    handle: Option<JoinHandle<()>>,
}
impl QrWorker {
    fn stop_and_result(&mut self) -> Option<Result<Value>> {
        self.stop.store(true, Ordering::Relaxed);
        if let Some(handle) = self.handle.take() {
            let _ = handle.join();
        }
        self.receiver
            .try_iter()
            .filter_map(|message| match message {
                WorkerMessage::Done(result) => Some(result),
                _ => None,
            })
            .last()
    }
    fn start(state: PathBuf, account: String, profile: Option<String>) -> Result<Self> {
        let (sender, receiver) = mpsc::sync_channel(16);
        let stop = Arc::new(AtomicBool::new(false));
        let worker_stop = stop.clone();
        let handle = thread::Builder::new()
            .name("qq-operator-qr".into())
            .spawn(move || {
                let result = (|| {
                    let mut runtime = Runtime::open(&state, &account, None)?;
                    runtime.operator_qq_qr_with_stop(
                        profile.as_deref(),
                        &worker_stop,
                        &mut |kind, data| {
                            sender
                                .try_send(WorkerMessage::Event(kind.into(), data))
                                .map_err(|_| "operator event queue closed or full".into())
                        },
                    )
                })();
                let _ = sender.try_send(WorkerMessage::Done(result));
            })
            .map_err(|_| "cannot start QR login worker")?;
        Ok(Self {
            stop,
            receiver,
            handle: Some(handle),
        })
    }
}
impl Drop for QrWorker {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Relaxed);
        if let Some(handle) = self.handle.take() {
            let _ = handle.join();
        }
    }
}

enum Stage {
    Input(SecretInputKind),
    Qr,
    QqQr(QrWorker),
    Password,
    Engine,
    GithubDevice {
        client: platform_github::auth::Client,
        device: platform_github::auth::DeviceFlow,
    },
    GithubVerify {
        tokens: platform_github::auth::Tokens,
        client_id: String,
        received_at: u64,
    },
}
struct Flow {
    id: String,
    platform: Platform,
    account: String,
    stage: Stage,
    result: LoginResult,
    deadline: Instant,
    next_poll: Instant,
    profile: Option<String>,
}
impl Flow {
    fn new(platform: Platform, account: &str, method: LoginMethod, stage: Stage) -> Result<Self> {
        let id = login_id()?;
        let mut result = LoginResult::new(LoginStatus::Pending);
        result.login_id = Some(id.clone());
        result.method = Some(method);
        result.expires_at = Some(now()? + 900);
        result.poll_after_ms = Some(1000);
        Ok(Self {
            id,
            platform,
            account: account.into(),
            stage,
            result,
            deadline: Instant::now() + Duration::from_secs(900),
            next_poll: Instant::now(),
            profile: None,
        })
    }
    fn matches(&self, request: &Request) -> bool {
        self.platform == request.platform
            && self.account == request.account
            && request.params.get("login_id").and_then(Value::as_str) == Some(&self.id)
    }
    fn update(&mut self, mut result: LoginResult) -> Result<()> {
        result.login_id = Some(self.id.clone());
        result.method = result.method.or(self.result.method);
        let expiry = result
            .expires_at
            .or(self.result.expires_at)
            .unwrap_or(now()? + 900)
            .min(now()? + 900);
        result.expires_at = Some(expiry);
        if !result.status.is_terminal() {
            result.poll_after_ms = Some(result.poll_after_ms.unwrap_or(1000).clamp(250, 60_000));
        }
        self.deadline = self
            .deadline
            .min(Instant::now() + Duration::from_secs(expiry.saturating_sub(now()?)));
        self.next_poll =
            Instant::now() + Duration::from_millis(result.poll_after_ms.unwrap_or(1000));
        self.result = result;
        Ok(())
    }
}

/// Only one login can be active per control connection. Exact login IDs are
/// bound to platform and account alias, including cancellation and terminal replay.
pub(crate) struct LoginController {
    state: PathBuf,
    runtime: Option<(String, Runtime)>,
    engines: EngineLoginManager,
    active: Option<Flow>,
    finished: Option<Flow>,
}
impl LoginController {
    pub(crate) fn new(state: &Path) -> Result<Self> {
        Ok(Self {
            state: state.into(),
            runtime: None,
            engines: EngineLoginManager::new(state)?,
            active: None,
            finished: None,
        })
    }
    fn runtime(&mut self, account: &str) -> Result<&mut Runtime> {
        if self
            .runtime
            .as_ref()
            .is_some_and(|(alias, _)| alias != account)
        {
            if self.active.is_some() {
                return Err("another login is active; cancel it before switching accounts".into());
            }
            self.runtime.take();
        }
        if self.runtime.is_none() {
            if self
                .active
                .as_ref()
                .is_some_and(|flow| matches!(flow.stage, Stage::QqQr(_)))
            {
                return Err("another login owns this state".into());
            }
            self.runtime = Some((account.into(), Runtime::open(&self.state, account, None)?));
        }
        Ok(&mut self.runtime.as_mut().unwrap().1)
    }
    pub(crate) fn execute(
        &mut self,
        request: &Request,
        emit: &mut impl FnMut(&str, Value) -> Result<()>,
    ) -> Result<Value> {
        self.expire()?;
        match request.action {
            Action::Capabilities => {
                let mut caps = capabilities(request.platform);
                if is_engine(request.platform) {
                    let native = EngineLoginManager::capabilities(request.platform.name())?;
                    caps.methods = serde_json::from_value(native["methods"].clone())
                        .map_err(|_| "invalid engine login capabilities")?;
                }
                encode(caps)
            }
            Action::Login => self.start(request, emit),
            Action::LoginPoll => self.poll(request, emit),
            Action::CancelLogin => self.cancel(request),
            Action::Status | Action::ReuseLogin if is_engine(request.platform) => {
                self.engines.status(request.platform.name())
            }
            Action::Status if request.platform == Platform::Weixin => {
                Ok(json!({"status":"paused","login_started":false}))
            }
            Action::ReuseLogin => self
                .runtime(&request.account)?
                .operator_reuse_login(request.platform.name()),
            Action::Qr | Action::PasswordLogin => {
                let mut replacement = Request::new(
                    request.id,
                    request.platform,
                    request.account.clone(),
                    Action::Login,
                );
                replacement.params =
                    json!({"method": if request.action == Action::Qr { "qr" } else { "password" }});
                if let Some(profile) = request.params.get("app_profile") {
                    replacement.params["app_profile"] = profile.clone();
                }
                replacement.secret = request
                    .secret
                    .as_ref()
                    .map(|secret| runtime_control::Secret::new(secret.bytes().to_vec()))
                    .transpose()?;
                self.start(&replacement, emit)
            }
            Action::PasswordResume | Action::QrPoll => {
                let Some(flow) = &self.active else {
                    return Err("same_connection_unavailable: use one agentctl login flow".into());
                };
                let mut replacement = Request::new(
                    request.id,
                    request.platform,
                    request.account.clone(),
                    Action::LoginPoll,
                );
                replacement.params = json!({"login_id":flow.id});
                if let Some(challenge_id) = request.params.get("challenge_id") {
                    replacement.params["challenge_id"] = challenge_id.clone();
                }
                replacement.secret = request
                    .secret
                    .as_ref()
                    .map(|secret| runtime_control::Secret::new(secret.bytes().to_vec()))
                    .transpose()?;
                self.poll(&replacement, emit)
            }
            _ => {
                if self.active.as_ref().is_some_and(|flow| {
                    flow.account != request.account || matches!(flow.stage, Stage::QqQr(_))
                }) || self.active.is_some()
                    && !matches!(request.action, Action::Status | Action::Challenge)
                {
                    return Err("another login is active".into());
                }
                crate::operator_control::execute_legacy(
                    self.runtime(&request.account)?,
                    request,
                    emit,
                )
            }
        }
    }
    fn start(
        &mut self,
        request: &Request,
        emit: &mut impl FnMut(&str, Value) -> Result<()>,
    ) -> Result<Value> {
        let caps = capabilities(request.platform);
        if caps.paused {
            return encode(failed(
                "paused",
                "Personal WeChat login and account testing remain paused.",
            ));
        }
        if let Some(active) = &self.active {
            if active.platform == request.platform && active.account == request.account {
                return encode(&active.result);
            }
            return Err("another login is active; cancel it first".into());
        }
        let requested: LoginMethod = request
            .params
            .get("method")
            .and_then(Value::as_str)
            .unwrap_or("auto")
            .parse()?;
        if let Some(profile) = request.params.get("app_profile").and_then(Value::as_str) {
            let _: platform_qq::AppProfile = profile.parse()?;
        }
        if requested != LoginMethod::Auto && !caps.methods.contains(&requested) {
            return encode(failed(
                "unsupported_method",
                "This platform does not support that login method.",
            ));
        }
        if is_engine(request.platform) {
            let value = self.engines.start(
                request.platform.name(),
                request
                    .params
                    .get("method")
                    .and_then(Value::as_str)
                    .unwrap_or("auto"),
                &mut |value| emit("authorization", value),
            )?;
            let result: LoginResult =
                serde_json::from_value(value).map_err(|_| "invalid engine login result")?;
            let mut flow = Flow::new(
                request.platform,
                &request.account,
                result.method.unwrap_or(LoginMethod::Device),
                Stage::Engine,
            )?;
            flow.update(result)?;
            return self.retain(flow);
        }
        // A failed transport, risk check or rate limit is not authentication
        // expiry. The adapter only returns needs_auth for explicit expiry.
        // A person may explicitly refresh the same alias's X web session.
        // Auto never turns a denied/rate-limited request into a fresh login.
        let explicit_refresh = matches!(request.platform, Platform::X | Platform::Bilibili | Platform::Tieba | Platform::Pixiv) && requested == LoginMethod::Browser
            || request.platform == Platform::Github
                && matches!(requested, LoginMethod::Device | LoginMethod::Token);
        if !explicit_refresh {
            let reused = self
                .runtime(&request.account)?
                .operator_reuse_login(request.platform.name())?;
            if reused["status"] != "needs_auth" {
                return encode(normalize(&reused)?);
            }
        }
        let method = if requested == LoginMethod::Auto {
            if request.platform == Platform::Qq {
                LoginMethod::Password
            } else {
                caps.preferred_method.unwrap_or(LoginMethod::Auto)
            }
        } else {
            requested
        };
        if request.platform == Platform::Github && method == LoginMethod::Device {
            if request.account == "public" {
                return encode(failed(
                    "account_required",
                    "Choose a personal account alias before GitHub authorization.",
                ));
            }
            // Acquire the same private state lock before asking for consent,
            // including an explicitly requested reauthorization.
            let _ = self.runtime(&request.account)?;
            let client_id = std::env::var("GLIMLINK_GITHUB_CLIENT_ID")
                .unwrap_or_else(|_| platform_github::auth::DEFAULT_CLIENT_ID.into());
            let client =
                platform_github::auth::Client::new(&client_id).map_err(|e| e.to_string())?;
            let device = client.start().map_err(|e| e.to_string())?;
            let mut result = LoginResult::new(LoginStatus::Pending);
            result.method = Some(LoginMethod::Device);
            result.expires_at = Some(now()?.saturating_add(device.expires_in));
            result.poll_after_ms = Some(device.interval * 1000);
            result.authorization=Some(runtime_control::Authorization{url:Some(platform_github::auth::VERIFICATION_URL.into()),user_code:Some(device.user_code.clone()),expires_at:result.expires_at,message:Some("Authorize Glimlink in your browser. Enter the short code shown here; no personal token or app registration is required.".into())});
            let mut flow = Flow::new(
                request.platform,
                &request.account,
                method,
                Stage::GithubDevice { client, device },
            )?;
            flow.update(result)?;
            return self.retain(flow);
        }
        let input_kind = if matches!(request.platform, Platform::X | Platform::Bilibili | Platform::Tieba | Platform::Pixiv) && method == LoginMethod::Browser {
            if request.account == "public" { return encode(failed("account_required", "Choose a personal account alias before browser sign-in.")); }
            Some(SecretInputKind::BrowserSession)
        } else if request.platform == Platform::Github {
            Some(SecretInputKind::Token)
        } else if request.platform == Platform::X {
            Some(SecretInputKind::Cookies)
        } else if request.platform == Platform::Qq && method == LoginMethod::Password {
            Some(SecretInputKind::Password)
        } else {
            None
        };
        let stage = input_kind.map(Stage::Input).unwrap_or(Stage::Qr);
        let mut flow = Flow::new(request.platform, &request.account, method, stage)?;
        flow.profile = request
            .params
            .get("app_profile")
            .and_then(Value::as_str)
            .map(str::to_owned);
        if let Some(kind) = input_kind {
            if let Some(secret) = &request.secret {
                self.submit_input(&mut flow, secret.bytes())?;
            } else {
                flow.result.status = LoginStatus::VerificationRequired;
                flow.result.input_required = Some(InputRequired { kind, message: Some("Enter credentials on the operator client; they travel only through the private control pipe.".into()) });
            }
        } else if request.platform == Platform::Qq {
            self.runtime.take();
            flow.stage = Stage::QqQr(QrWorker::start(
                self.state.clone(),
                request.account.clone(),
                request
                    .params
                    .get("app_profile")
                    .and_then(Value::as_str)
                    .map(str::to_owned),
            )?);
            flow.result.expires_at = Some(now()? + 180);
            flow.deadline = Instant::now() + Duration::from_secs(180);
        } else {
            let runtime = self.runtime(&request.account)?;
            let raw = match request.platform {
                Platform::Bilibili => runtime.operator_bilibili_qr_mode(false)?,
                Platform::Tieba => runtime.operator_tieba_qr()?,
                _ => {
                    return encode(failed(
                        "unsupported_method",
                        "No interactive login method is available; use explicit login import.",
                    ));
                }
            };
            flow.update(normalize(&raw)?)?;
        }
        self.retain(flow)
    }
    fn submit_input(&mut self, flow: &mut Flow, bytes: &[u8]) -> Result<()> {
        let runtime = self.runtime(&flow.account)?;
        let raw = if flow.platform == Platform::Github {
            let input = PrivateInput::create(runtime.state_dir(), bytes)?;
            runtime.github_login(&input.path)?
        } else if flow.platform == Platform::X {
            let input = PrivateInput::create(runtime.state_dir(), bytes)?;
            runtime.x_import_login(&input.path)?
        } else if flow.platform == Platform::Bilibili {
            let input = PrivateInput::create(runtime.state_dir(), bytes)?;
            runtime.bilibili_import_login(&input.path)?
        } else if flow.platform == Platform::Tieba {
            let input = PrivateInput::create(runtime.state_dir(), bytes)?;
            runtime.tieba_import_login(&input.path)?
        } else if flow.platform == Platform::Pixiv {
            let input = PrivateInput::create(runtime.state_dir(), bytes)?;
            runtime.pixiv_import_login(&input.path)?
        } else {
            let bytes = password_input(bytes, flow.profile.as_deref())?;
            let input = PrivateInput::create(runtime.state_dir(), &bytes)?;
            runtime.operator_check_password_input(false, &json!({}), &bytes)?;
            runtime.qq_password_login(&input.path)?
        };
        flow.stage = Stage::Password;
        flow.update(normalize(&raw)?)?;
        if flow.result.status == LoginStatus::VerificationRequired {
            flow.result.challenge = Some(runtime.operator_challenge()?);
        }
        Ok(())
    }
    fn poll(
        &mut self,
        request: &Request,
        emit: &mut impl FnMut(&str, Value) -> Result<()>,
    ) -> Result<Value> {
        if self
            .active
            .as_ref()
            .is_none_or(|flow| !flow.matches(request))
        {
            if let Some(flow) = self.finished.as_ref().filter(|flow| flow.matches(request)) {
                return encode(&flow.result);
            }
            return Err(
                "login_id does not belong to this platform/account/control connection".into(),
            );
        }
        if request.secret.is_none()
            && self
                .active
                .as_ref()
                .is_some_and(|flow| flow.next_poll > Instant::now())
        {
            return encode(&self.active.as_ref().unwrap().result);
        }
        let mut flow = self.active.take().unwrap();
        let result = self.advance(&mut flow, request, emit);
        if let Err(error) = result {
            // Keep a live CAPTCHA socket for an input correction. Never restart
            // native login or silently switch methods after a rejected frame.
            if matches!(flow.stage, Stage::Password)
                && flow.platform == Platform::Qq
                && !self.runtime(&flow.account)?.qq_password_session_active()
            {
                flow.update(failed("login_connection_closed", "The original native login connection closed. Start a new explicit login; the previous challenge cannot be replayed."))?;
                return self.retain(flow);
            }
            if matches!(flow.stage, Stage::Engine) && !self.engines.is_pending(flow.platform.name())
            {
                flow.update(failed("login_connection_closed", "The native subscription login process ended. Start a new explicit login; the previous authorization cannot be replayed."))?;
                return self.retain(flow);
            }
            self.active = Some(flow);
            return Err(error);
        }
        self.retain(flow)
    }
    fn advance(
        &mut self,
        flow: &mut Flow,
        request: &Request,
        emit: &mut impl FnMut(&str, Value) -> Result<()>,
    ) -> Result<()> {
        match &mut flow.stage {
            Stage::Input(kind) => {
                if !matches!(
                    (flow.platform, *kind),
                    (Platform::Github, SecretInputKind::Token)
                        | (Platform::X, SecretInputKind::Cookies)
                        | (Platform::X | Platform::Bilibili | Platform::Tieba | Platform::Pixiv, SecretInputKind::BrowserSession)
                        | (Platform::Qq, SecretInputKind::Password)
                ) {
                    return Err("private input does not match the active platform login".into());
                }
                if let Some(secret) = &request.secret {
                    self.submit_input(flow, secret.bytes())?;
                }
            }
            Stage::Engine => {
                let raw = self.engines.poll(
                    flow.platform.name(),
                    request.secret.as_ref().map(|v| v.bytes()),
                    &mut |value| emit("authorization", value),
                )?;
                flow.update(
                    serde_json::from_value(raw).map_err(|_| "invalid engine login result")?,
                )?;
            }
            Stage::GithubDevice { client, device } => {
                let polled = match client.poll(device) {
                    Ok(value) => value,
                    Err(error) => {
                        if let Some(delay) = platform_github::auth::retry_after(&error) {
                            flow.result.poll_after_ms = Some(delay * 1000);
                            flow.next_poll = Instant::now() + Duration::from_secs(delay);
                            emit(
                                "progress",
                                json!({"status":"pending","message":"Network interrupted; authorization is still waiting. You do not need to sign in again."}),
                            )?;
                            return Ok(());
                        }
                        return flow
                            .update(failed("github_authorization_failed", &error.to_string()));
                    }
                };
                let result = match polled {
                    platform_github::auth::Poll::Pending => {
                        flow.result.poll_after_ms = Some(device.interval * 1000);
                        flow.next_poll = Instant::now() + Duration::from_secs(device.interval);
                        return Ok(());
                    }
                    platform_github::auth::Poll::Denied => failed(
                        "authorization_denied",
                        "GitHub authorization was cancelled. Existing account state was retained.",
                    ),
                    platform_github::auth::Poll::Expired => failed(
                        "login_expired",
                        "GitHub authorization expired. Start login again.",
                    ),
                    platform_github::auth::Poll::Authorized(tokens) => {
                        let client_id = client.client_id().to_owned();
                        flow.stage = Stage::GithubVerify {
                            tokens,
                            client_id,
                            received_at: now()?,
                        };
                        flow.result.authorization = None;
                        return self.finish_github(flow, emit);
                    }
                };
                flow.update(result)?;
            }
            Stage::GithubVerify { .. } => self.finish_github(flow, emit)?,
            Stage::Password => {
                let runtime = self.runtime(&flow.account)?;
                let challenge = runtime.operator_challenge()?;
                if challenge.kind == "captcha" && request.secret.is_none() {
                    return Ok(());
                }
                let bytes = request.secret.as_ref().map(|v| v.bytes()).unwrap_or(b"{}");
                let mut params = request.params.clone();
                if params.get("challenge_id").is_none() {
                    params["challenge_id"] = json!(challenge.challenge_id);
                }
                runtime.operator_check_password_input(true, &params, bytes)?;
                let input = PrivateInput::create(runtime.state_dir(), bytes)?;
                let raw = runtime.qq_password_resume(&input.path)?;
                flow.update(normalize(&raw)?)?;
                if flow.result.status == LoginStatus::VerificationRequired {
                    flow.result.challenge = Some(runtime.operator_challenge()?);
                }
            }
            Stage::Qr => {
                let runtime = self.runtime(&flow.account)?;
                let raw = match flow.platform {
                    Platform::Bilibili => runtime.bilibili_poll_login()?,
                    Platform::Tieba => runtime.tieba_poll_login()?,
                    _ => return Err("invalid QR platform".into()),
                };
                // Keep the client QR presentation and fixed original expiry.
                let qr = flow.result.qr.clone();
                let expires = flow.result.expires_at;
                flow.update(normalize(&raw)?)?;
                flow.result.qr = qr;
                flow.result.expires_at = expires;
            }
            Stage::QqQr(worker) => {
                let mut final_result = None;
                loop {
                    match worker.receiver.try_recv() {
                        Ok(WorkerMessage::Event(kind, mut data)) => {
                            if kind == "qr_ready" {
                                data["expires_at"] = json!(flow.result.expires_at);
                            }
                            emit(&kind, data)?;
                        }
                        Ok(WorkerMessage::Done(result)) => {
                            final_result = Some(result);
                            break;
                        }
                        Err(mpsc::TryRecvError::Empty) => break,
                        Err(mpsc::TryRecvError::Disconnected) => {
                            final_result =
                                Some(Err("QR login worker ended before a result".into()));
                            break;
                        }
                    }
                }
                if let Some(result) = final_result {
                    flow.update(match result {
                        Ok(raw) => normalize(&raw)?,
                        Err(error) => qr_failure(&error),
                    })?;
                }
            }
        }
        Ok(())
    }
    fn finish_github(
        &mut self,
        flow: &mut Flow,
        emit: &mut impl FnMut(&str, Value) -> Result<()>,
    ) -> Result<()> {
        let Stage::GithubVerify {
            tokens,
            client_id,
            received_at,
        } = &flow.stage
        else {
            return Err("invalid GitHub verification stage".into());
        };
        let raw =
            self.runtime(&flow.account)?
                .github_oauth_login(tokens, client_id, *received_at)?;
        if raw["status"] == "pending" {
            let delay = raw["poll_after_ms"]
                .as_u64()
                .unwrap_or(10000)
                .clamp(1000, 3_600_000);
            flow.result.status = LoginStatus::Pending;
            flow.result.poll_after_ms = Some(delay);
            flow.next_poll = Instant::now() + Duration::from_millis(delay);
            emit(
                "progress",
                json!({"status":"pending","message":"GitHub authorization received; waiting to verify the account. Do not authorize again."}),
            )?;
            return Ok(());
        }
        if raw["status"] == "failed" {
            return flow.update(failed("github_identity_verification_failed",raw["oauth_error"].as_str().unwrap_or("The authorized account could not be verified or saved. Existing credentials were retained.")));
        }
        flow.update(normalize(&raw)?)
    }
    fn retain(&mut self, flow: Flow) -> Result<Value> {
        let value = encode(&flow.result)?;
        if flow.result.status.is_terminal() {
            self.finished = Some(flow);
            // Never retain a completed QR worker's Runtime lock in a replay.
            if let Some(finished) = &mut self.finished {
                if matches!(finished.stage, Stage::QqQr(_)) {
                    finished.stage = Stage::Qr;
                }
            }
        } else {
            self.active = Some(flow);
        }
        Ok(value)
    }
    fn cleanup(&mut self, flow: &mut Flow) -> Result<()> {
        match &mut flow.stage {
            Stage::Engine => {
                self.engines.cancel(flow.platform.name())?;
            }
            Stage::QqQr(worker) => {
                worker.stop.store(true, Ordering::Relaxed);
            }
            Stage::Password if flow.platform == Platform::Qq => {
                self.runtime(&flow.account)?.qq_password_cancel()?;
            }
            Stage::Qr if matches!(flow.platform, Platform::Bilibili | Platform::Tieba) => {
                self.runtime(&flow.account)?
                    .operator_cancel_qr(flow.platform.name())?;
            }
            _ => {}
        }
        if matches!(flow.stage, Stage::QqQr(_)) {
            flow.stage = Stage::Qr;
        }
        Ok(())
    }
    fn cancel(&mut self, request: &Request) -> Result<Value> {
        if self
            .active
            .as_ref()
            .is_none_or(|flow| !flow.matches(request))
        {
            if let Some(flow) = self.finished.as_ref().filter(|flow| flow.matches(request)) {
                return encode(&flow.result);
            }
            return Err(
                "login_id does not belong to this platform/account/control connection".into(),
            );
        }
        let mut flow = self.active.take().unwrap();
        if self.completed_qr(&mut flow)? {
            return self.retain(flow);
        }
        self.cleanup(&mut flow)?;
        let mut result = LoginResult::new(LoginStatus::Cancelled);
        result.method = flow.result.method;
        flow.update(result)?;
        self.retain(flow)
    }
    fn expire(&mut self) -> Result<()> {
        if self
            .active
            .as_ref()
            .is_some_and(|flow| flow.deadline <= Instant::now())
        {
            let mut flow = self.active.take().unwrap();
            if self.completed_qr(&mut flow)? {
                self.finished = Some(flow);
                return Ok(());
            }
            self.cleanup(&mut flow)?;
            flow.update(failed(
                "login_expired",
                "The login session expired. Start a new explicit login.",
            ))?;
            self.finished = Some(flow);
        }
        Ok(())
    }
    fn completed_qr(&mut self, flow: &mut Flow) -> Result<bool> {
        if let Stage::QqQr(worker) = &mut flow.stage {
            if let Some(Ok(raw)) = worker.stop_and_result() {
                let result = normalize(&raw)?;
                if result.status == LoginStatus::Authenticated {
                    flow.stage = Stage::Qr;
                    flow.update(result)?;
                    return Ok(true);
                }
            }
        }
        Ok(false)
    }
}
impl Drop for LoginController {
    fn drop(&mut self) {
        if let Some(mut flow) = self.active.take() {
            let _ = self.cleanup(&mut flow);
        }
    }
}

fn capabilities(platform: Platform) -> LoginCapabilities {
    let (methods, reuse, import_login, preferred_method, paused, message) = match platform {
        Platform::Bilibili | Platform::Tieba => (vec![LoginMethod::Browser, LoginMethod::Qr], true, true, Some(LoginMethod::Browser), false, Some("Reuse saved sessions first. A real browser on the operator can hand off this platform's session over the private control connection; Runtime verifies identity before saving. QR remains available explicitly.".into())),
        Platform::Qq => (vec![LoginMethod::Password, LoginMethod::Qr], true, true, Some(LoginMethod::Password), false, None),
        Platform::Github => (vec![LoginMethod::Device, LoginMethod::Token], true, true, Some(LoginMethod::Device), false, Some("Authorize the project-provided Glimlink application in your system browser. Existing accounts are reused; private token import remains optional.".into())),
        Platform::X => (vec![LoginMethod::Auto, LoginMethod::Browser], true, true, Some(LoginMethod::Browser), false, Some("Reuse the saved X web session first. Otherwise, sign in using an installed browser on the operator. Only the platform session crosses the private control channel; Runtime independently verifies it before saving.".into())),
        Platform::Pixiv => (vec![LoginMethod::Browser], true, true, Some(LoginMethod::Browser), false, Some("Sign in through a real browser on the operator. Runtime verifies the authenticated Pixiv user before saving the web session. Read-only website adapter; no mobile OAuth app impersonation.".into())),
        Platform::Codex => (vec![LoginMethod::Device], true, false, Some(LoginMethod::Device), false, Some("Native subscription login; one dedicated credential home per engine in this Runtime.".into())),
        Platform::Antigravity => (vec![LoginMethod::Browser], true, false, Some(LoginMethod::Browser), false, Some("Native Google subscription login; the browser and one-time code input run on the operator client.".into())),
        Platform::Weixin => (vec![], false, false, None, true, Some("Personal WeChat login/account testing is paused.".into())),
        Platform::Runtime => (vec![], false, false, None, false, Some("Select a platform or model engine for login.".into())),
    };
    LoginCapabilities {
        platform,
        methods,
        reuse,
        import_login,
        preferred_method,
        paused,
        message,
    }
}

fn normalize(raw: &Value) -> Result<LoginResult> {
    let state = raw["status"].as_str().unwrap_or("");
    let status = match state {
        "online" | "authenticated" => LoginStatus::Authenticated,
        "verification_required" => LoginStatus::VerificationRequired,
        "waiting"
        | "scanned"
        | "waiting_for_scan"
        | "waiting_for_confirm"
        | "confirmed"
        | "pending" => LoginStatus::Pending,
        "canceled" | "cancelled" => LoginStatus::Cancelled,
        _ => LoginStatus::Failed,
    };
    let mut result = LoginResult::new(status);
    if status == LoginStatus::Pending {
        result.poll_after_ms = Some(2000);
    }
    result.reused_session = raw.get("reused_session").and_then(Value::as_bool);
    result.expires_at = raw.get("expires_at").and_then(Value::as_u64).or_else(|| {
        raw.get("expires_in_seconds")
            .and_then(Value::as_u64)
            .and_then(|n| now().ok().map(|t| t + n))
    });
    if raw.get("qr_url").is_some() || raw.get("image_base64").is_some() {
        result.qr = Some(QrPresentation {
            qr_url: raw["qr_url"].as_str().map(str::to_owned),
            image_base64: raw["image_base64"].as_str().map(str::to_owned),
            mime: raw["mime"].as_str().map(str::to_owned),
            expires_at: result.expires_at,
            expires_in_seconds: raw["expires_in_seconds"].as_u64(),
        });
    }
    let user = raw.get("user");
    let identity = user
        .and_then(|u| {
            ["mid", "id", "user_id", "uin", "uid"]
                .iter()
                .find_map(|key| u.get(key))
        })
        .or_else(|| raw.get("identity"));
    if let Some(id) = identity {
        let id = id
            .as_str()
            .map(str::to_owned)
            .or_else(|| id.as_u64().map(|n| n.to_string()));
        if let Some(id) = id {
            result.identity = Some(AccountIdentity {
                id,
                label: user
                    .and_then(|u| {
                        ["login", "name", "username", "display_name"]
                            .iter()
                            .find_map(|key| u.get(key).and_then(Value::as_str))
                    })
                    .map(|s| s.chars().take(256).collect()),
            });
        }
    }
    if status == LoginStatus::Failed {
        result.error = Some(LoginError {
            code: if state == "expired" {
                "login_expired".into()
            } else {
                raw.get("code")
                    .and_then(Value::as_u64)
                    .map(|n| format!("provider_{n}"))
                    .unwrap_or_else(|| "login_rejected".into())
            },
            message: if raw["reason"] == "SecurityRestricted" {
                "QQ requires an account/device security check. Existing saved credentials were retained; no automatic alternate login was started.".into()
            } else if raw["reason"] == "SecBeatRejected" || raw["code"] == 140022017u64 {
                "QQ native login was rejected with SEC_BEAT (140022017). The precise cause is unconfirmed; no automatic retry was started.".into()
            } else {
                "The provider did not accept this login. Existing saved credentials were retained; no automatic alternate login was started.".into()
            },
        });
    }
    Ok(result)
}

fn qr_failure(error: &str) -> LoginResult {
    if error == "QR login timed out; existing saved session was kept"
        || error == "QR code expired or canceled; existing saved session was kept"
    {
        return failed(
            "login_expired",
            "The QQ QR challenge expired or was cancelled by the provider; start a new explicit login.",
        );
    }
    if let Some(rest) = error.strip_prefix("QR login rejected (0x") {
        if rest.len() >= 3
            && rest.as_bytes()[..2].iter().all(u8::is_ascii_hexdigit)
            && rest.as_bytes()[2] == b')'
        {
            // This exact error is generated by the SDK after URL/token/account
            // redaction, never by forwarding arbitrary transport logs.
            return failed(
                &format!("provider_0x{}", &rest[..2]),
                &error.chars().take(440).collect::<String>(),
            );
        }
    }
    failed(
        "qr_login_failed",
        "The native QQ QR login did not complete; no message was sent.",
    )
}

fn password_input(bytes: &[u8], profile: Option<&str>) -> Result<Vec<u8>> {
    let mut value: Value =
        serde_json::from_slice(bytes).map_err(|_| "invalid private password input")?;
    let object = value
        .as_object_mut()
        .ok_or("private password input must be an object")?;
    if let Some(profile) = profile {
        let selected: platform_qq::AppProfile = profile.parse()?;
        if let Some(input) = object.get("app_profile") {
            let supplied: platform_qq::AppProfile = input
                .as_str()
                .ok_or("invalid private password profile")?
                .parse()?;
            if selected != supplied {
                return Err(
                    "private password profile differs from selected application profile".into(),
                );
            }
        }
        object.insert("app_profile".into(), json!(profile));
    }
    serde_json::to_vec(&value).map_err(|_| "cannot encode private password input".into())
}

#[cfg(test)]
mod tests {
    use super::*;
    fn state(name: &str) -> PathBuf {
        std::env::temp_dir().join(format!("login-{name}-{}", std::process::id()))
    }
    #[test]
    fn project_github_authorization_needs_no_configuration_and_rejects_public_alias_before_network()
    {
        let caps = capabilities(Platform::Github);
        assert_eq!(caps.preferred_method, Some(LoginMethod::Device));
        assert!(caps.methods.contains(&LoginMethod::Token));
        assert!(!platform_github::auth::DEFAULT_CLIENT_ID.is_empty());
        let path = state("github-public-device");
        let mut control = LoginController::new(&path).unwrap();
        let mut request = Request::new(1, Platform::Github, "public".into(), Action::Login);
        request.params = json!({"method":"device"});
        let value = control.execute(&request, &mut |_, _| Ok(())).unwrap();
        assert_eq!(value["error"]["code"], "account_required");
        assert!(control.active.is_none() && control.runtime.is_none());
    }
    #[test]
    fn received_github_credentials_stay_private_and_cancel_without_reauthorizing() {
        let path = state("github-received-cancel");
        let mut control = LoginController::new(&path).unwrap();
        let tokens = platform_github::auth::Tokens {
            access_token: "synthetic-private-access".into(),
            refresh_token: Some("synthetic-private-refresh".into()),
            expires_in: Some(28800),
            refresh_expires_in: Some(15_552_000),
            scope: "read:user".into(),
        };
        let flow = Flow::new(
            Platform::Github,
            "owner",
            LoginMethod::Device,
            Stage::GithubVerify {
                tokens,
                client_id: platform_github::auth::DEFAULT_CLIENT_ID.into(),
                received_at: 1,
            },
        )
        .unwrap();
        let id = flow.id.clone();
        let serialized = encode(&flow.result).unwrap().to_string();
        assert!(!serialized.contains("synthetic-private"));
        assert!(flow.result.authorization.is_none());
        control.active = Some(flow);
        let mut cancel = Request::new(2, Platform::Github, "owner".into(), Action::CancelLogin);
        cancel.params = json!({"login_id":id});
        let value = control.execute(&cancel, &mut |_, _| Ok(())).unwrap();
        assert_eq!(value["status"], "cancelled");
        assert!(!value.to_string().contains("synthetic-private"));
        assert!(control.runtime.is_none());
    }
    #[test]
    fn explicit_x_refresh_requests_private_input_without_touching_saved_state() {
        let path = state("x-explicit-refresh");
        let dir = crate::private_state::account_directory(&path, "x", "owner").unwrap();
        let saved = dir.join("session.json");
        let retained = b"synthetic-saved-state-that-must-not-be-read-or-replaced";
        std::fs::write(&saved, retained).unwrap();
        let mut control = LoginController::new(&path).unwrap();
        let mut start = Request::new(1, Platform::X, "owner".into(), Action::Login);
        start.params = json!({"method":"browser"});
        let first = control.execute(&start, &mut |_, _| Ok(())).unwrap();
        assert_eq!(first["status"], "verification_required");
        assert_eq!(first["method"], "browser");
        assert_eq!(first["input_required"]["kind"], "browser_session");
        assert!(control.runtime.is_none());
        start.action = Action::CancelLogin;
        start.params = json!({"login_id":first["login_id"]});
        assert_eq!(
            control.execute(&start, &mut |_, _| Ok(())).unwrap()["status"],
            "cancelled"
        );
        drop(control);
        assert_eq!(std::fs::read(saved).unwrap(), retained);
        std::fs::remove_dir_all(path).unwrap();
    }
    #[test]
    fn token_input_flow_is_bound_to_account_platform_pipe_and_replays_cancel() {
        let path = state("binding");
        let mut control = LoginController::new(&path).unwrap();
        let mut start = Request::new(1, Platform::Github, "owner".into(), Action::Login);
        start.params = json!({"method":"token"});
        let first = control.execute(&start, &mut |_, _| Ok(())).unwrap();
        assert_eq!(first["status"], "verification_required");
        assert_eq!(first["input_required"]["kind"], "token");
        let second = control.execute(&start, &mut |_, _| Ok(())).unwrap();
        assert_eq!(first["login_id"], second["login_id"]);
        start.action = Action::CancelLogin;
        start.params = json!({"login_id":first["login_id"]});
        start.account = "other".into();
        assert!(control.execute(&start, &mut |_, _| Ok(())).is_err());
        start.account = "owner".into();
        start.platform = Platform::Qq;
        assert!(control.execute(&start, &mut |_, _| Ok(())).is_err());
        start.platform = Platform::Github;
        let cancelled = control.execute(&start, &mut |_, _| Ok(())).unwrap();
        assert_eq!(cancelled["status"], "cancelled");
        assert_eq!(
            control.execute(&start, &mut |_, _| Ok(())).unwrap(),
            cancelled
        );
        let mut other_pipe = LoginController::new(&path).unwrap();
        assert!(other_pipe.execute(&start, &mut |_, _| Ok(())).is_err());
        drop(other_pipe);
        drop(control);
        std::fs::remove_dir_all(path).unwrap();
    }
    #[test]
    fn capabilities_and_paused_wechat_never_open_account_state() {
        let path = state("paused");
        let mut control = LoginController::new(&path).unwrap();
        let mut request = Request::new(1, Platform::Weixin, "owner".into(), Action::Capabilities);
        let caps = control.execute(&request, &mut |_, _| Ok(())).unwrap();
        assert_eq!(caps["paused"], true);
        assert_eq!(caps["methods"], json!([]));
        request.action = Action::Login;
        assert_eq!(
            control.execute(&request, &mut |_, _| Ok(())).unwrap()["error"]["code"],
            "paused"
        );
        assert!(control.runtime.is_none());
    }
    #[test]
    fn normalization_projects_identity_not_credentials_or_management_paths() {
        let raw = json!({"status":"online","user":{"mid":42,"name":"fixture","cookie":"secret"},"management_file":"/private/account","session":{"token":"secret"}});
        let result = encode(normalize(&raw).unwrap()).unwrap();
        assert_eq!(result["identity"]["id"], "42");
        let text = result.to_string();
        assert!(!text.contains("secret") && !text.contains("/private/account"));
        assert_eq!(
            normalize(&json!({"status":"confirmed"})).unwrap().status,
            LoginStatus::Pending
        );
        assert_eq!(
            normalize(&json!({"status":"unknown"})).unwrap().status,
            LoginStatus::Failed
        );
    }
    #[test]
    fn expiry_and_disconnect_keep_saved_credentials_and_stop_the_active_flow() {
        let path = state("expiry");
        let mut control = LoginController::new(&path).unwrap();
        let mut start = Request::new(1, Platform::Github, "owner".into(), Action::Login);
        start.params = json!({"method":"token"});
        let first = control.execute(&start, &mut |_, _| Ok(())).unwrap();
        let dir = crate::private_state::account_directory(&path, "github", "owner").unwrap();
        let saved = dir.join("session.json");
        let retained = b"synthetic-existing-credential-not-read-by-cancel";
        std::fs::write(&saved, retained).unwrap();
        control.active.as_mut().unwrap().deadline = Instant::now() - Duration::from_secs(1);
        let mut poll = Request::new(2, Platform::Github, "owner".into(), Action::LoginPoll);
        poll.params = json!({"login_id":first["login_id"]});
        let expired = control.execute(&poll, &mut |_, _| Ok(())).unwrap();
        assert_eq!(expired["error"]["code"], "login_expired");
        assert!(control.active.is_none());
        drop(control);
        assert_eq!(std::fs::read(saved).unwrap(), retained);
        std::fs::remove_dir_all(path).unwrap();
    }
    #[test]
    fn password_profile_applies_to_initial_and_deferred_input_and_rejects_conflicts() {
        let input = br#"{"uin":42,"password":"synthetic"}"#;
        let selected = "3.2.26-46494";
        let merged: Value =
            serde_json::from_slice(&password_input(input, Some(selected)).unwrap()).unwrap();
        assert_eq!(merged["app_profile"], selected);
        assert!(
            password_input(
                br#"{"uin":42,"password":"synthetic","app_profile":"3.2.19-39038"}"#,
                Some(selected)
            )
            .is_err()
        );
        assert!(password_input(input, Some("unsupported-profile")).is_err());
    }
    #[test]
    fn queued_qr_success_wins_a_late_cancel_without_repeating_authentication() {
        let path = state("qr-commit");
        let mut control = LoginController::new(&path).unwrap();
        let (sender, receiver) = mpsc::sync_channel(2);
        sender
            .send(WorkerMessage::Done(Ok(
                json!({"status":"online","identity":"42","session_saved":true}),
            )))
            .unwrap();
        drop(sender);
        let worker = QrWorker {
            stop: Arc::new(AtomicBool::new(false)),
            receiver,
            handle: None,
        };
        let flow = Flow::new(Platform::Qq, "owner", LoginMethod::Qr, Stage::QqQr(worker)).unwrap();
        let mut cancel = Request::new(1, Platform::Qq, "owner".into(), Action::CancelLogin);
        cancel.params = json!({"login_id":flow.id});
        control.active = Some(flow);
        let result = control.execute(&cancel, &mut |_, _| Ok(())).unwrap();
        assert_eq!(result["status"], "authenticated");
        assert_eq!(result["identity"]["id"], "42");
        assert!(control.active.is_none());
        drop(control);
        std::fs::remove_dir_all(path).unwrap();
        assert_eq!(
            qr_failure("QR login timed out; existing saved session was kept")
                .error
                .unwrap()
                .code,
            "login_expired"
        );
        assert_eq!(
            qr_failure("QR login rejected (0x2d): 登录限制")
                .error
                .unwrap()
                .code,
            "provider_0x2d"
        );
        assert!(
            !qr_failure("network error /private/token")
                .error
                .unwrap()
                .message
                .contains("/private/token")
        );
    }
}
