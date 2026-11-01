// SPDX-License-Identifier: MIT
//! Native subscription login for the private operator channel. URLs and one-time
//! codes are sent only to the human client; model-facing tools never use this.
use crate::agent_host::{EngineKind, Settings};
use runtime_core::{Result, resources::private_dir};
use runtime_engine::{
    CodexClient,
    codex::LoginProgress,
    login_pty::{AntigravityLogin, LoginPtyProgress},
};
use serde::Deserialize;
use serde_json::{Value, json};
use std::{
    path::{Path, PathBuf},
    time::{Duration, Instant, SystemTime, UNIX_EPOCH},
};

const LOGIN_LIFETIME: Duration = Duration::from_secs(600);
struct Pending {
    engine: &'static str,
    expires_at: u64,
    deadline: Instant,
    process: Process,
    awaiting_code: bool,
}
enum Process {
    Codex { client: CodexClient, id: String },
    Antigravity(AntigravityLogin),
}
pub struct EngineLoginManager {
    state: PathBuf,
    codex_binary: PathBuf,
    antigravity_binary: PathBuf,
    pending: Option<Pending>,
}
fn now() -> Result<u64> {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs())
        .map_err(|_| "engine login clock unavailable".into())
}
fn engine_name(engine: &str) -> Result<&'static str> {
    match engine {
        "codex" => Ok("codex"),
        "antigravity" => Ok("antigravity"),
        _ => Err("unsupported subscription engine".into()),
    }
}
fn failure(code: &str, message: &str) -> Value {
    json!({"status":"failed","error":{"code":code,"message":message}})
}
impl EngineLoginManager {
    pub fn is_pending(&self, engine: &str) -> bool {
        self.pending.as_ref().is_some_and(|p| p.engine == engine)
    }
    pub fn new(state: &Path) -> Result<Self> {
        Self::with_binaries(
            state,
            crate::deployment::native_binary(EngineKind::Codex),
            crate::deployment::native_binary(EngineKind::Antigravity),
        )
    }
    /// Trusted host configuration, never an operator-provided remote path.
    pub fn with_binaries(
        state: &Path,
        codex_binary: PathBuf,
        antigravity_binary: PathBuf,
    ) -> Result<Self> {
        private_dir(state)?;
        let state = state
            .canonicalize()
            .map_err(|_| "cannot resolve engine login state")?;
        if !codex_binary.is_absolute() || !antigravity_binary.is_absolute() {
            return Err("engine binaries must be absolute paths".into());
        }
        Ok(Self {
            state,
            codex_binary,
            antigravity_binary,
            pending: None,
        })
    }
    pub fn capabilities(engine: &str) -> Result<Value> {
        let engine = engine_name(engine)?;
        Ok(
            json!({"engine":engine,"methods":if engine=="codex"{vec!["device"]}else{vec!["browser"]},"subscription_login":true,
            "browser_on":"operator_client","callback_listener_on_core":false,"native_cli_required":true,"reuses_saved_session":true,
            "manual_authorization_code":engine=="antigravity","cancel":true,"model_turn_started":false}),
        )
    }
    fn settings(&self, engine: &str) -> Settings {
        Settings {
            engine: if engine == "codex" {
                EngineKind::Codex
            } else {
                EngineKind::Antigravity
            },
            binary: if engine == "codex" {
                self.codex_binary.clone()
            } else {
                self.antigravity_binary.clone()
            },
            turn_timeout_seconds: 15,
            ..Settings::default()
        }
    }
    fn codex_client(&self) -> Result<CodexClient> {
        let config = self.settings("codex").engine_config(&self.state)?;
        let mut client = CodexClient::spawn(config).map_err(|e| e.to_string())?;
        client.initialize().map_err(|e| e.to_string())?;
        Ok(client)
    }
    pub fn status(&mut self, engine: &str) -> Result<Value> {
        let engine = engine_name(engine)?;
        if let Some(pending) = &self.pending {
            if pending.engine == engine {
                return Ok(pending_result(pending));
            }
        }
        if engine == "codex" {
            let mut client = self.codex_client()?;
            let account = client.account_read().map_err(|e| e.to_string())?;
            let subscription = account.authenticated
                && matches!(
                    account.auth_kind.as_deref(),
                    Some("chatgpt" | "chatgpt_auth_tokens")
                );
            Ok(
                json!({"engine":engine,"status":if subscription{"authenticated"}else{"signed_out"},"authenticated":subscription,
                "subscription_login":true,"login_started":false,"model_turn_started":false}),
            )
        } else {
            let config = self.settings(engine).antigravity_config(&self.state)?;
            let probe = crate::agent_antigravity::probe(config)?;
            let authenticated = probe["account"]["authenticated"] == true;
            Ok(
                json!({"engine":engine,"status":if authenticated{"authenticated"}else{"signed_out"},"authenticated":authenticated,
                "subscription_login":true,"login_started":false,"model_turn_started":false}),
            )
        }
    }
    pub fn start(
        &mut self,
        engine: &str,
        method: &str,
        _emit: &mut impl FnMut(Value) -> Result<()>,
    ) -> Result<Value> {
        let engine = engine_name(engine)?;
        if self.pending.is_some() {
            return Err("another subscription login is pending".into());
        }
        let selected = match (engine, method) {
            ("codex", "auto" | "device") => "device",
            ("antigravity", "auto" | "browser") => "browser",
            _ => {
                return Ok(failure(
                    "unsupported_login_method",
                    "This native engine does not provide that headless login method.",
                ));
            }
        };
        let status = self.status(engine)?;
        if status["authenticated"] == true {
            return Ok(
                json!({"engine":engine,"status":"authenticated","method":selected,"reused_session":true,"model_turn_started":false}),
            );
        }
        let expires_at = now()?.saturating_add(LOGIN_LIFETIME.as_secs());
        let mut result = json!({"engine":engine,"status":"pending","method":selected,"expires_at":expires_at,"poll_after_ms":500,"model_turn_started":false});
        let process = if engine == "codex" {
            let mut client = self.codex_client()?;
            let authorization = client.login_device_start().map_err(|e| e.to_string())?;
            result["authorization"] = json!({"url":authorization.verification_url,"user_code":authorization.user_code,
                "expires_at":expires_at,"message":"Open the official page on your own device and enter this one-time device code."});
            Process::Codex {
                client,
                id: authorization.login_id,
            }
        } else {
            let process = AntigravityLogin::spawn(
                &self.antigravity_binary,
                &self.state.join("antigravity"),
                &self.state.join("antigravity-login-workspace"),
                LOGIN_LIFETIME,
            )
            .map_err(|e| e.to_string())?;
            Process::Antigravity(process)
        };
        self.pending = Some(Pending {
            engine,
            expires_at,
            deadline: Instant::now() + LOGIN_LIFETIME,
            process,
            awaiting_code: false,
        });
        Ok(result)
    }
    pub fn poll(
        &mut self,
        engine: &str,
        secret: Option<&[u8]>,
        _emit: &mut impl FnMut(Value) -> Result<()>,
    ) -> Result<Value> {
        let engine = engine_name(engine)?;
        if self.pending.as_ref().is_none_or(|p| p.engine != engine) {
            return Err("no matching subscription login is pending".into());
        }
        let mut pending = self.pending.take().unwrap();
        if Instant::now() >= pending.deadline {
            return Ok(failure(
                "login_expired",
                "The native login session expired. Start a new explicit login.",
            ));
        }
        let progress = match &mut pending.process {
            Process::Codex { client, id } => {
                if secret.is_some() {
                    self.pending = Some(pending);
                    return Err("device login does not accept a private authorization code".into());
                }
                match client.login_device_poll(id).map_err(|e| e.to_string())? {
                    LoginProgress::Pending => 0,
                    LoginProgress::Completed => 1,
                    LoginProgress::Failed => 2,
                }
            }
            Process::Antigravity(process) => {
                if let Some(bytes) = secret {
                    #[derive(Deserialize)]
                    #[serde(deny_unknown_fields)]
                    struct Code {
                        authorization_code: String,
                    }
                    if bytes.len() > 16384 {
                        self.pending = Some(pending);
                        return Err("private authorization input exceeds limit".into());
                    }
                    let input: Code = match serde_json::from_slice(bytes) {
                        Ok(input) => input,
                        Err(_) => {
                            self.pending = Some(pending);
                            return Err("invalid private authorization input".into());
                        }
                    };
                    if let Err(error) = process.submit_code(&input.authorization_code) {
                        self.pending = Some(pending);
                        return Err(error.to_string());
                    }
                    pending.awaiting_code = false;
                }
                match process.poll().map_err(|e| e.to_string())? {
                    LoginPtyProgress::Pending => 0,
                    LoginPtyProgress::Completed => 1,
                    LoginPtyProgress::Failed => 2,
                    LoginPtyProgress::Authorization { url } => {
                        pending.awaiting_code = true;
                        let mut result = pending_result(&pending);
                        result["authorization"] = json!({"url":url,"expires_at":pending.expires_at,
                            "message":"Sign in on your own device. Paste the one-time code from the official Antigravity callback into the operator client."});
                        self.pending = Some(pending);
                        return Ok(result);
                    }
                }
            }
        };
        if progress == 0 {
            let result = pending_result(&pending);
            self.pending = Some(pending);
            return Ok(result);
        }
        drop(pending);
        if progress == 2 {
            return Ok(failure(
                "native_login_failed",
                "The native login did not complete. Existing native account storage was not removed.",
            ));
        }
        // Native success is confirmed through the native cached-account path.
        // No model prompt, token extraction or copied credential JSON is used.
        let status = self.status(engine)?;
        if status["authenticated"] != true {
            return Ok(failure(
                "login_unverified",
                "Native completion was not confirmed by the cached-account probe.",
            ));
        }
        Ok(
            json!({"engine":engine,"status":"authenticated","reused_session":false,"model_turn_started":false}),
        )
    }
    pub fn cancel(&mut self, engine: &str) -> Result<Value> {
        let engine = engine_name(engine)?;
        if self.pending.as_ref().is_some_and(|p| p.engine != engine) {
            return Err("another engine owns the pending login".into());
        }
        if let Some(mut pending) = self.pending.take() {
            if let Process::Codex { client, id } = &mut pending.process {
                let _ = client.login_device_cancel(id);
            }
        }
        Ok(
            json!({"engine":engine,"status":"cancelled","saved_login_removed":false,"model_turn_started":false}),
        )
    }
}
fn pending_result(pending: &Pending) -> Value {
    let mut result = json!({"engine":pending.engine,"status":"pending","method":if pending.engine=="codex"{"device"}else{"browser"},
        "expires_at":pending.expires_at,"poll_after_ms":500,"model_turn_started":false});
    if pending.awaiting_code {
        result["input_required"] = json!({"kind":"authorization_code","message":"Enter the one-time code from the official callback on the operator client."});
    }
    result
}
