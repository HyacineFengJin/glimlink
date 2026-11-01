// SPDX-License-Identifier: MIT
//! ACP v1 on the pinned native Antigravity CLI. Subscription login remains a
//! separate management action; ordinary jobs never launch authentication.
use crate::{
    AgentMessage, EngineError, EngineEvent, ErrorKind, Input, Result, ThreadHandle, TurnOutput,
    TurnStatus, transport::Transport,
};
use serde_json::{Value, json};
use std::{
    collections::BTreeSet,
    fs,
    io::{Read, Write},
    os::unix::fs::{DirBuilderExt, OpenOptionsExt, PermissionsExt},
    path::{Path, PathBuf},
    process::Command,
    time::{Duration, Instant},
};

#[derive(Clone, Debug)]
pub struct AntigravityConfig {
    pub binary: PathBuf,
    pub home: PathBuf,
    pub workspace: PathBuf,
    pub bridge_binary: PathBuf,
    pub bridge_socket: PathBuf,
    /// Operator persona/role only. Recollections belong in the prompt as data.
    pub developer_instructions: Option<String>,
    pub model: Option<String>,
    /// Go soft runtime budget; mapped/native allocations still count in cgroup.
    pub go_memory_limit_mib: u16,
    pub timeout: Duration,
    pub max_jsonl_bytes: usize,
    pub max_wire_bytes: usize,
    pub max_input_bytes: usize,
    pub max_output_bytes: usize,
    /// Trusted host/fixture override, never model input.
    pub args: Vec<String>,
}
impl AntigravityConfig {
    pub fn new(
        binary: impl Into<PathBuf>,
        home: impl Into<PathBuf>,
        workspace: impl Into<PathBuf>,
    ) -> Self {
        let home = home.into();
        Self {
            binary: binary.into(),
            workspace: workspace.into(),
            bridge_binary: PathBuf::from("/cache/bin/agent-runtime"),
            bridge_socket: home.join("runtime-mcp.sock"),
            home,
            developer_instructions: None,
            model: None,
            go_memory_limit_mib: 128,
            timeout: Duration::from_secs(120),
            max_jsonl_bytes: 1024 * 1024,
            max_wire_bytes: 8 * 1024 * 1024,
            max_input_bytes: 128 * 1024,
            max_output_bytes: 256 * 1024,
            args: vec![
                "acp".into(),
                "--include-default-customizations=false".into(),
                "--disable-telemetry".into(),
            ],
        }
    }
}

pub struct AntigravityClient {
    config: AntigravityConfig,
    transport: Transport,
    initialized: bool,
    next_id: u64,
    sessions: BTreeSet<String>,
    home_lock: Option<crate::credential_home::CredentialHomeLock>,
}
fn error(kind: ErrorKind, text: &str) -> EngineError {
    EngineError::new(kind, text)
}
fn valid_id(text: &str) -> bool {
    !text.is_empty()
        && text.len() <= 256
        && text
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b"-_:.".contains(&b))
}
pub(crate) fn native(path: &Path) -> Result<PathBuf> {
    if !path.is_absolute() {
        return Err(error(
            ErrorKind::InvalidInput,
            "native executable path must be absolute",
        ));
    }
    let path = fs::canonicalize(path)
        .map_err(|_| error(ErrorKind::InvalidInput, "native executable is unavailable"))?;
    let mut magic = [0; 4];
    fs::File::open(&path)
        .and_then(|mut file| file.read_exact(&mut magic))
        .map_err(|_| error(ErrorKind::InvalidInput, "cannot inspect native executable"))?;
    if magic != *b"\x7fELF" {
        return Err(error(
            ErrorKind::Unsupported,
            "engine and MCP bridge require native ELF executables",
        ));
    }
    Ok(path)
}
pub(crate) fn private_directory(path: &Path) -> Result<PathBuf> {
    if !path.is_absolute() {
        return Err(error(
            ErrorKind::InvalidInput,
            "engine directories must be absolute",
        ));
    }
    fs::DirBuilder::new()
        .recursive(true)
        .mode(0o700)
        .create(path)
        .map_err(|_| error(ErrorKind::Io, "cannot create private engine directory"))?;
    if !fs::symlink_metadata(path).is_ok_and(|meta| {
        meta.is_dir() && !meta.file_type().is_symlink() && meta.permissions().mode() & 0o077 == 0
    }) {
        return Err(error(
            ErrorKind::InvalidInput,
            "engine credential directories must be private (0700)",
        ));
    }
    fs::canonicalize(path).map_err(|_| error(ErrorKind::InvalidInput, "invalid engine directory"))
}

/// The dedicated Runtime profile owns this policy file. Native subscription
/// credentials live elsewhere and are never opened or copied here.
fn install_managed_settings(gemini: &Path) -> Result<()> {
    let directory = private_directory(&gemini.join("antigravity-cli"))?;
    let path = directory.join("settings.json");
    let settings = json!({"toolPermission":"request-review","permissions":{
        "allow":["mcp(runtime/*)"],
        "deny":["read_file(*)","write_file(*)","read_url(*)","execute_url(*)","command(*)","unsandboxed(*)"]
    }});
    match fs::symlink_metadata(&path) {
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
        Err(_) => {
            return Err(error(
                ErrorKind::Io,
                "cannot inspect managed Antigravity settings",
            ));
        }
        Ok(meta) => {
            if !meta.is_file()
                || meta.file_type().is_symlink()
                || meta.permissions().mode() & 0o077 != 0
                || meta.len() > 64 * 1024
            {
                return Err(error(
                    ErrorKind::InvalidInput,
                    "existing Antigravity settings must be a regular private bounded file",
                ));
            }
            let mut file = fs::OpenOptions::new()
                .read(true)
                .custom_flags(libc::O_NOFOLLOW)
                .open(&path)
                .map_err(|_| error(ErrorKind::Io, "cannot read managed Antigravity settings"))?;
            let mut bytes = Vec::new();
            Read::by_ref(&mut file)
                .take(64 * 1024 + 1)
                .read_to_end(&mut bytes)
                .map_err(|_| error(ErrorKind::Io, "cannot read managed Antigravity settings"))?;
            if bytes.len() > 64 * 1024
                || !serde_json::from_slice::<Value>(&bytes).is_ok_and(|v| v.is_object())
            {
                return Err(error(
                    ErrorKind::InvalidInput,
                    "existing Antigravity settings are not bounded JSON",
                ));
            }
            if serde_json::from_slice::<Value>(&bytes).ok().as_ref() == Some(&settings) {
                return Ok(());
            }
        }
    }
    let temporary = directory.join("settings.runtime.next");
    match fs::symlink_metadata(&temporary) {
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
        _ => {
            return Err(error(
                ErrorKind::State,
                "managed Antigravity settings replacement is busy or requires cleanup",
            ));
        }
    }
    let result = (|| {
        let mut file = fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .mode(0o600)
            .open(&temporary)
            .map_err(|_| error(ErrorKind::Io, "cannot prepare managed Antigravity settings"))?;
        let bytes = serde_json::to_vec(&settings)
            .map_err(|_| error(ErrorKind::State, "invalid managed settings"))?;
        file.write_all(&bytes)
            .and_then(|_| file.sync_all())
            .map_err(|_| error(ErrorKind::Io, "cannot persist managed Antigravity settings"))?;
        fs::rename(&temporary, &path)
            .map_err(|_| error(ErrorKind::Io, "cannot replace managed Antigravity settings"))?;
        fs::File::open(&directory)
            .and_then(|f| f.sync_all())
            .map_err(|_| error(ErrorKind::Io, "cannot persist managed settings directory"))
    })();
    if result.is_err() {
        let _ = fs::remove_file(&temporary);
    }
    result
}

impl AntigravityClient {
    pub fn spawn(mut config: AntigravityConfig) -> Result<Self> {
        if !(32..=256).contains(&config.go_memory_limit_mib)
            || config.timeout.is_zero()
            || config.timeout > Duration::from_secs(600)
            || !(1024..=4 * 1024 * 1024).contains(&config.max_jsonl_bytes)
            || config.max_wire_bytes < config.max_jsonl_bytes
            || config.max_wire_bytes > 64 * 1024 * 1024
            || config.max_input_bytes == 0
            || config.max_input_bytes > config.max_jsonl_bytes / 2
            || config.max_output_bytes == 0
            || config.max_output_bytes > 4 * 1024 * 1024
            || config
                .developer_instructions
                .as_ref()
                .is_some_and(|v| v.len() > 16 * 1024 || v.contains('\0'))
            || config
                .model
                .as_ref()
                .is_some_and(|v| v.is_empty() || v.len() > 128 || v.chars().any(char::is_control))
            || !config.bridge_socket.is_absolute()
            || config.bridge_socket.as_os_str().as_encoded_bytes().len() > 104
        {
            return Err(error(
                ErrorKind::InvalidInput,
                "invalid Antigravity settings or resource limits",
            ));
        }
        config.binary = native(&config.binary)?;
        config.bridge_binary = native(&config.bridge_binary)?;
        config.home = private_directory(&config.home)?;
        if !config.workspace.is_absolute() {
            return Err(error(ErrorKind::InvalidInput, "workspace must be absolute"));
        }
        config.workspace = fs::canonicalize(&config.workspace)
            .map_err(|_| error(ErrorKind::InvalidInput, "engine workspace is unavailable"))?;
        if !config.workspace.is_dir()
            || config.workspace.starts_with(&config.home)
            || config.home.starts_with(&config.workspace)
        {
            return Err(error(
                ErrorKind::InvalidInput,
                "workspace and credential home must be separate directory trees",
            ));
        }
        let home_lock = crate::credential_home::lock(&config.home)?;
        let gemini = private_directory(&config.home.join(".gemini"))?;
        install_managed_settings(&gemini)?;
        let app_data = private_directory(&config.home.join("app-data"))?;
        let cache = private_directory(&config.home.join("cache"))?;
        let mut command = Command::new(&config.binary);
        command
            .args(&config.args)
            .args([
                "--gemini-dir".as_ref(),
                gemini.as_os_str(),
                "--app-data-dir".as_ref(),
                // Native 1.4.11 requires this relative to --gemini-dir.
                // Match normal CLI login's profile rather than creating one.
                "antigravity-cli".as_ref(),
            ])
            .current_dir(&config.workspace)
            .env_clear()
            .env("HOME", &config.home)
            .env("XDG_CONFIG_HOME", &gemini)
            .env("XDG_DATA_HOME", &app_data)
            .env("XDG_CACHE_HOME", &cache)
            .env("PATH", "/usr/local/bin:/usr/bin:/bin")
            .env("LANG", "C.UTF-8")
            // Native Go keeps its own soft GC budget inside the outer cgroup.
            // This is not an RSS limit: mapped code and native allocations remain.
            .env("GOMEMLIMIT", format!("{}MiB", config.go_memory_limit_mib))
            .env("GOGC", "60")
            .env("AGY_CLI_DISABLE_AUTO_UPDATE", "true");
        let transport = Transport::spawn(command, config.max_jsonl_bytes, config.max_wire_bytes)?;
        Ok(Self {
            config,
            transport,
            initialized: false,
            next_id: 1,
            sessions: BTreeSet::new(),
            home_lock: Some(home_lock),
        })
    }

    /// Only capability and implementation metadata is returned, never account
    /// identities, raw authentication data, or arbitrary server metadata.
    pub fn initialize(&mut self) -> Result<Value> {
        if self.initialized {
            return Err(error(ErrorKind::State, "engine was already initialized"));
        }
        let result = (|| {
            let (_,value)=self.request("initialize",json!({"protocolVersion":1,"clientInfo":{"name":"agent-runtime","version":env!("CARGO_PKG_VERSION")},"clientCapabilities":{"fs":{"readTextFile":false,"writeTextFile":false},"terminal":false,"auth":{"terminal":false}}}), |_|Ok(()), &mut ||Ok(()))?;
            if value["protocolVersion"] != 1
                || value["agentCapabilities"]["_meta"]["antigravity"]["systemPrompt"]["append"]
                    != true
                || !value["authMethods"]
                    .as_array()
                    .is_some_and(|methods| methods.iter().any(|m| m["id"] == "oauth-personal"))
            {
                return Err(error(
                    ErrorKind::Unsupported,
                    "native Antigravity lacks ACP v1 personal subscription/system-prompt support",
                ));
            }
            self.initialized = true;
            Ok(
                json!({"protocolVersion":1,"agentCapabilities":{"_meta":{"antigravity":{"systemPrompt":{"append":true}}},"promptCapabilities":{"image":value["agentCapabilities"]["promptCapabilities"]["image"]==true,"audio":value["agentCapabilities"]["promptCapabilities"]["audio"]==true},"auth":{"logout":value["agentCapabilities"]["auth"].get("logout").is_some()}},"authMethods":[{"id":"oauth-personal"}]}),
            )
        })();
        if result.is_err() {
            self.shutdown();
        }
        result
    }

    pub fn new_session(&mut self, pump: &mut impl FnMut() -> Result<()>) -> Result<ThreadHandle> {
        if !self.initialized {
            return Err(error(ErrorKind::State, "initialize the engine first"));
        }
        if self.sessions.len() >= 16 {
            return Err(error(ErrorKind::Limit, "engine session limit"));
        }
        let result = (|| {
            let params = json!({"cwd":self.config.workspace,"mcpServers":[{"name":"runtime","command":self.config.bridge_binary,"args":["engine","mcp-bridge",self.config.bridge_socket],"env":[]}],"_meta":{"disableBuiltInTools":true,"antigravity":{"systemPrompt":{"append":self.config.developer_instructions.as_deref().unwrap_or("")}}}});
            let (_, value) = self.request("session/new", params, |_| Ok(()), pump)?;
            let id = value["sessionId"]
                .as_str()
                .filter(|v| valid_id(v))
                .ok_or_else(|| error(ErrorKind::Protocol, "ACP session has no valid identity"))?
                .to_owned();
            if !self.sessions.insert(id.clone()) {
                return Err(error(ErrorKind::Protocol, "ACP reused a session identity"));
            }
            if let Some(model) = self.config.model.clone() {
                self.request(
                    "session/set_model",
                    json!({"sessionId":id,"modelId":model}),
                    |_| Ok(()),
                    pump,
                )?;
            }
            Ok(ThreadHandle { id })
        })();
        if result.is_err() {
            self.shutdown();
        }
        result
    }

    /// Persist this synthetic per-process request identity before invoking prompt.
    pub fn next_turn_id(&self) -> String {
        format!("acp-{}", self.next_id)
    }

    pub fn prompt(
        &mut self,
        session: &str,
        inputs: &[Input],
        mut event: impl FnMut(EngineEvent),
        pump: &mut impl FnMut() -> Result<()>,
    ) -> Result<TurnOutput> {
        if !self.initialized || !self.sessions.contains(session) {
            return Err(error(ErrorKind::State, "unknown ACP session"));
        }
        if inputs.is_empty() || inputs.len() > 32 {
            return Err(error(ErrorKind::InvalidInput, "invalid ACP input count"));
        }
        let mut prompt = Vec::new();
        let mut bytes = 0usize;
        for input in inputs {
            match input {
                Input::Text { text } if !text.is_empty() && !text.contains('\0') => {
                    bytes = bytes.saturating_add(text.len());
                    prompt.push(json!({"type":"text","text":text}));
                }
                Input::Text { .. } => {
                    return Err(error(
                        ErrorKind::InvalidInput,
                        "invalid empty/NUL text input",
                    ));
                }
                _ => {
                    return Err(error(
                        ErrorKind::Unsupported,
                        "Antigravity media inputs have not been verified; only text is currently accepted",
                    ));
                }
            }
        }
        if bytes > self.config.max_input_bytes
            || serde_json::to_vec(&prompt).map_or(true, |v| v.len() > self.config.max_input_bytes)
        {
            return Err(error(ErrorKind::Limit, "ACP prompt exceeds input budget"));
        }
        let turn_id = self.next_turn_id();
        let mut text = String::new();
        let output_limit = self.config.max_output_bytes;
        let result = (|| {
            let (_, value) = self.request(
                "session/prompt",
                json!({"sessionId":session,"prompt":prompt}),
                |message| {
                    if message["method"] == "session/update" {
                        let params = &message["params"];
                        if params["sessionId"] != session {
                            return Err(error(
                                ErrorKind::Protocol,
                                "ACP update belongs to another session",
                            ));
                        }
                        let update = &params["update"];
                        if update["sessionUpdate"] == "agent_message_chunk" {
                            if update["content"]["type"] != "text" {
                                return Err(error(
                                    ErrorKind::Unsupported,
                                    "non-text ACP reply is unsupported",
                                ));
                            }
                            let delta = update["content"]["text"].as_str().ok_or_else(|| {
                                error(ErrorKind::Protocol, "ACP text chunk is malformed")
                            })?;
                            if text.len().saturating_add(delta.len()) > output_limit {
                                return Err(error(
                                    ErrorKind::Limit,
                                    "ACP answer exceeds output budget",
                                ));
                            }
                            text.push_str(delta);
                            event(EngineEvent::Delta {
                                item_id: turn_id.clone(),
                                text: delta.into(),
                            });
                        }
                    } else if message["method"] == "runtime/permission_denied" {
                        event(EngineEvent::ApprovalDenied {
                            method: "session/request_permission".into(),
                        });
                    }
                    Ok(())
                },
                pump,
            )?;
            let status = match value["stopReason"].as_str() {
                Some("end_turn") => TurnStatus::Completed,
                Some("cancelled") => TurnStatus::Interrupted,
                Some("max_tokens" | "max_turn_requests" | "refusal") => TurnStatus::Failed,
                _ => return Err(error(ErrorKind::Protocol, "unknown ACP stop reason")),
            };
            let message = AgentMessage {
                id: turn_id.clone(),
                text: text.clone(),
                phase: None,
            };
            event(EngineEvent::MessageCompleted {
                message: message.clone(),
            });
            event(EngineEvent::TurnCompleted {
                turn_id: turn_id.clone(),
                status,
            });
            Ok(TurnOutput {
                thread_id: session.into(),
                turn_id: turn_id.clone(),
                status,
                text: std::mem::take(&mut text),
                messages: vec![message],
            })
        })();
        if result.is_err() {
            self.shutdown();
        }
        result
    }

    fn request(
        &mut self,
        method: &str,
        params: Value,
        mut notification: impl FnMut(&Value) -> Result<()>,
        pump: &mut impl FnMut() -> Result<()>,
    ) -> Result<(u64, Value)> {
        let id = self.next_id;
        self.next_id = self
            .next_id
            .checked_add(1)
            .ok_or_else(|| error(ErrorKind::Limit, "ACP request identity exhausted"))?;
        let deadline = Instant::now() + self.config.timeout;
        self.transport.write(
            &json!({"jsonrpc":"2.0","id":id,"method":method,"params":params}),
            deadline,
        )?;
        let mut bytes = 0usize;
        let mut events = 0usize;
        let mut requests = 0usize;
        loop {
            if Instant::now() >= deadline {
                return Err(error(ErrorKind::Timeout, "ACP request timed out"));
            }
            pump()?;
            if Instant::now() >= deadline {
                return Err(error(ErrorKind::Timeout, "ACP request timed out"));
            }
            let Some((message, size)) = self
                .transport
                .poll(deadline.min(Instant::now() + Duration::from_millis(25)))?
            else {
                continue;
            };
            bytes = bytes.saturating_add(size);
            events += 1;
            if bytes > self.config.max_wire_bytes || events > 8192 {
                return Err(error(ErrorKind::Limit, "ACP event/wire budget exceeded"));
            }
            if message["jsonrpc"] != "2.0" {
                return Err(error(ErrorKind::Protocol, "invalid ACP JSON-RPC version"));
            }
            if message.get("method").is_some() {
                if message.get("id").is_some() {
                    requests += 1;
                    if requests > 32 {
                        return Err(error(ErrorKind::Limit, "ACP client request limit"));
                    }
                    let request_id = &message["id"];
                    if !(request_id.is_i64()
                        || request_id.is_u64()
                        || request_id.as_str().is_some_and(|v| v.len() <= 256))
                    {
                        return Err(error(
                            ErrorKind::Protocol,
                            "invalid ACP client request identity",
                        ));
                    }
                    if message["method"] == "session/request_permission" {
                        self.transport.write(&json!({"jsonrpc":"2.0","id":request_id,"result":{"outcome":{"outcome":"cancelled"}}}),deadline)?;
                        notification(&json!({"method":"runtime/permission_denied"}))?;
                    } else {
                        self.transport.write(&json!({"jsonrpc":"2.0","id":request_id,"error":{"code":-32601,"message":"Client capability unsupported"}}),deadline)?;
                    }
                } else {
                    notification(&message)?;
                }
                continue;
            }
            if message["id"].as_u64() != Some(id) {
                return Err(error(ErrorKind::Protocol, "ACP response identity mismatch"));
            }
            if let Some(remote) = message.get("error") {
                let code = remote["code"].as_i64();
                return Err(EngineError::new(
                    if code == Some(-32000) {
                        ErrorKind::Authentication
                    } else {
                        ErrorKind::Remote
                    },
                    format!(
                        "Antigravity rejected {method} (code {code:?}); raw server details withheld"
                    ),
                ));
            }
            return message
                .get("result")
                .filter(|v| v.is_object())
                .cloned()
                .map(|v| (id, v))
                .ok_or_else(|| error(ErrorKind::Protocol, "ACP response has no object result"));
        }
    }
    pub fn shutdown(&mut self) {
        self.transport.shutdown();
        self.home_lock.take();
        self.initialized = false;
        self.sessions.clear();
    }
}
