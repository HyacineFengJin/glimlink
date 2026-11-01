// SPDX-License-Identifier: MIT
use crate::{
    AccountStatus, AgentMessage, DynamicToolSpec, EngineError, EngineEvent, ErrorKind, Input,
    Result, ServerInfo, ThreadHandle, ToolCall, ToolContent, ToolResult, TurnOutput, TurnStatus,
    transport::Transport,
};
use base64::{Engine, engine::general_purpose::STANDARD};
use serde_json::{Value, json};
#[cfg(unix)]
use std::os::unix::fs::{DirBuilderExt, PermissionsExt};
use std::{
    collections::{BTreeMap, BTreeSet, VecDeque},
    fs,
    io::Read,
    path::PathBuf,
    process::Command,
    time::{Duration, Instant},
};

#[derive(Debug, Clone)]
pub struct CodexConfig {
    pub binary: PathBuf,
    pub codex_home: PathBuf,
    pub workspace: PathBuf,
    /// Operator persona/role, supplied by the embedding Runtime. Recollections
    /// and platform content belong in turn input, never this field.
    pub developer_instructions: Option<String>,
    pub timeout: Duration,
    pub max_jsonl_bytes: usize,
    pub max_wire_bytes: usize,
    pub max_output_bytes: usize,
    pub max_input_bytes: usize,
    pub max_image_bytes: u64,
    pub max_tool_result_bytes: usize,
    pub max_tool_image_bytes: usize,
    pub max_tool_calls: usize,
    /// Native app-server arguments. The CLI need not expose this override.
    pub args: Vec<String>,
}
impl CodexConfig {
    pub fn new(
        binary: impl Into<PathBuf>,
        codex_home: impl Into<PathBuf>,
        workspace: impl Into<PathBuf>,
    ) -> Self {
        Self {
            binary: binary.into(),
            codex_home: codex_home.into(),
            workspace: workspace.into(),
            developer_instructions: None,
            timeout: Duration::from_secs(120),
            max_jsonl_bytes: 1024 * 1024,
            max_wire_bytes: 8 * 1024 * 1024,
            max_output_bytes: 256 * 1024,
            max_input_bytes: 128 * 1024,
            max_image_bytes: 16 * 1024 * 1024,
            max_tool_result_bytes: 896 * 1024,
            max_tool_image_bytes: 512 * 1024,
            max_tool_calls: 32,
            args: [
                "shell_tool",
                "unified_exec",
                "code_mode",
                "code_mode_host",
                "code_mode_prewarm",
                "apps",
                "plugins",
                "hooks",
                "browser_use",
                "browser_use_external",
                "in_app_browser",
                "computer_use",
                "multi_agent",
            ]
            .into_iter()
            .flat_map(|feature| ["--disable".to_owned(), feature.to_owned()])
            .chain(["-c".to_owned(), "web_search=\"disabled\"".to_owned()])
            .chain([
                "-c".to_owned(),
                "cli_auth_credentials_store=\"file\"".to_owned(),
            ])
            .chain(std::iter::once("app-server".to_owned()))
            .collect(),
        }
    }
}

pub struct CodexClient {
    config: CodexConfig,
    transport: Transport,
    initialized: bool,
    next_id: u64,
    threads: BTreeSet<String>,
    tool_names: BTreeMap<String, BTreeSet<String>>,
    pending: VecDeque<Value>,
    pending_bytes: usize,
    denied: VecDeque<String>,
    home_lock: Option<crate::credential_home::CredentialHomeLock>,
}

/// Private operator-only device authorization. Never pass this to a model.
pub struct DeviceLogin {
    pub login_id: String,
    pub verification_url: String,
    pub user_code: String,
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum LoginProgress {
    Pending,
    Completed,
    Failed,
}

fn error(kind: ErrorKind, detail: &str) -> EngineError {
    EngineError::new(kind, detail)
}
fn valid_id(id: &str) -> bool {
    !id.is_empty()
        && id.len() <= 256
        && id
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b"-_:".contains(&b))
}
fn metadata_string(value: &Value, key: &str) -> Option<String> {
    value
        .get(key)
        .and_then(Value::as_str)
        .filter(|s| s.len() <= 1024 && !s.chars().any(char::is_control))
        .map(str::to_owned)
}

impl CodexClient {
    pub fn spawn(mut config: CodexConfig) -> Result<Self> {
        if config.timeout.is_zero()
            || config
                .developer_instructions
                .as_ref()
                .is_some_and(|text| text.len() > 16 * 1024 || text.contains('\0'))
            || config.timeout > Duration::from_secs(600)
            || !(1024..=4 * 1024 * 1024).contains(&config.max_jsonl_bytes)
            || config.max_wire_bytes < config.max_jsonl_bytes
            || config.max_wire_bytes > 64 * 1024 * 1024
            || config.max_output_bytes == 0
            || config.max_output_bytes > 4 * 1024 * 1024
            || config.max_input_bytes == 0
            || config.max_input_bytes > config.max_jsonl_bytes / 2
            || config.max_tool_result_bytes == 0
            || config.max_tool_result_bytes > 4 * 1024 * 1024
            || config.max_tool_image_bytes == 0
            || config.max_tool_image_bytes > config.max_tool_result_bytes
            || config.max_tool_calls == 0
            || config.max_tool_calls > 128
        {
            return Err(error(
                ErrorKind::InvalidInput,
                "invalid engine resource limits",
            ));
        }
        if !config.binary.is_absolute()
            || !config.codex_home.is_absolute()
            || !config.workspace.is_absolute()
        {
            return Err(error(
                ErrorKind::InvalidInput,
                "engine paths must be absolute",
            ));
        }
        config.binary = fs::canonicalize(&config.binary).map_err(|_| {
            error(
                ErrorKind::InvalidInput,
                "native app-server binary is unavailable",
            )
        })?;
        // Reject npm/shell launchers instead of quietly introducing a JS runtime.
        let mut magic = [0; 4];
        fs::File::open(&config.binary)
            .and_then(|mut f| f.read_exact(&mut magic))
            .map_err(|_| {
                error(
                    ErrorKind::InvalidInput,
                    "cannot inspect app-server executable",
                )
            })?;
        if magic != *b"\x7fELF" {
            return Err(error(
                ErrorKind::Unsupported,
                "Linux engine requires a native ELF executable, not a script or Node launcher",
            ));
        }
        #[cfg(unix)]
        fs::DirBuilder::new()
            .recursive(true)
            .mode(0o700)
            .create(&config.codex_home)
            .map_err(|_| error(ErrorKind::Io, "cannot create dedicated CODEX_HOME"))?;
        #[cfg(not(unix))]
        return Err(error(
            ErrorKind::Unsupported,
            "native engine process isolation currently requires Unix",
        ));
        config.codex_home = fs::canonicalize(&config.codex_home)
            .map_err(|_| error(ErrorKind::InvalidInput, "invalid CODEX_HOME"))?;
        config.workspace = fs::canonicalize(&config.workspace)
            .map_err(|_| error(ErrorKind::InvalidInput, "invalid engine workspace"))?;
        if !config.workspace.is_dir()
            || config.codex_home.starts_with(&config.workspace)
            || config.workspace.starts_with(&config.codex_home)
        {
            return Err(error(
                ErrorKind::InvalidInput,
                "workspace and credential home must be separate directory trees",
            ));
        }
        #[cfg(unix)]
        if fs::metadata(&config.codex_home)
            .map_err(|_| error(ErrorKind::Io, "cannot inspect CODEX_HOME"))?
            .permissions()
            .mode()
            & 0o077
            != 0
        {
            return Err(error(
                ErrorKind::InvalidInput,
                "CODEX_HOME must be private (0700)",
            ));
        }
        let home_lock = crate::credential_home::lock(&config.codex_home)?;
        let mut command = Command::new(&config.binary);
        command
            .args(&config.args)
            .current_dir(&config.workspace)
            .env_clear()
            .env("CODEX_HOME", &config.codex_home)
            .env("HOME", &config.codex_home)
            .env("PATH", "/usr/local/bin:/usr/bin:/bin")
            .env("LANG", "C.UTF-8");
        let transport = Transport::spawn(command, config.max_jsonl_bytes, config.max_wire_bytes)?;
        Ok(Self {
            config,
            transport,
            initialized: false,
            next_id: 1,
            threads: BTreeSet::new(),
            tool_names: BTreeMap::new(),
            pending: VecDeque::new(),
            pending_bytes: 0,
            denied: VecDeque::new(),
            home_lock: Some(home_lock),
        })
    }

    pub fn initialize(&mut self) -> Result<ServerInfo> {
        if self.initialized {
            return Err(error(ErrorKind::State, "engine was already initialized"));
        }
        let result = (|| {
            let result = self.rpc("initialize", json!({"capabilities":{"experimentalApi":true},"clientInfo":{
                "name":"agent_runtime", "title":"Agent Runtime", "version":env!("CARGO_PKG_VERSION")
            }}))?;
            self.transport.write(
                &json!({"method":"initialized","params":{}}),
                Instant::now() + self.config.timeout,
            )?;
            self.initialized = true;
            Ok(ServerInfo {
                user_agent: metadata_string(&result, "userAgent"),
                platform_family: metadata_string(&result, "platformFamily"),
                platform_os: metadata_string(&result, "platformOs"),
            })
        })();
        self.close_if_error(result)
    }
    /// Start the native ChatGPT plan's device flow. No callback listener or
    /// browser is started by the Runtime; only a human operator gets the URL.
    pub fn login_device_start(&mut self) -> Result<DeviceLogin> {
        self.require_initialized()?;
        let result = (|| {
            let value = self.rpc("account/login/start", json!({"type":"chatgptDeviceCode"}))?;
            if value["type"] != "chatgptDeviceCode" {
                return Err(error(
                    ErrorKind::Protocol,
                    "native login returned an unexpected method",
                ));
            }
            let login_id = value["loginId"]
                .as_str()
                .filter(|v| valid_id(v))
                .ok_or_else(|| error(ErrorKind::Protocol, "native login returned no identity"))?;
            let verification_url = value["verificationUrl"]
                .as_str()
                .filter(|v| {
                    v.len() <= 2048
                        && !v.chars().any(char::is_control)
                        && (v.starts_with("https://auth.openai.com/")
                            || v.starts_with("https://chatgpt.com/"))
                })
                .ok_or_else(|| {
                    error(
                        ErrorKind::Protocol,
                        "native login returned an invalid verification URL",
                    )
                })?;
            let user_code = value["userCode"]
                .as_str()
                .filter(|v| {
                    !v.is_empty()
                        && v.len() <= 64
                        && v.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'-')
                })
                .ok_or_else(|| {
                    error(
                        ErrorKind::Protocol,
                        "native login returned an invalid device code",
                    )
                })?;
            self.transport.set_lifetime(Duration::from_secs(600))?;
            Ok(DeviceLogin {
                login_id: login_id.into(),
                verification_url: verification_url.into(),
                user_code: user_code.into(),
            })
        })();
        self.close_if_error(result)
    }
    /// Consume bounded native authentication notifications without blocking the
    /// control channel for the full device-code lifetime.
    pub fn login_device_poll(&mut self, login_id: &str) -> Result<LoginProgress> {
        self.require_initialized()?;
        if !valid_id(login_id) {
            return Err(error(ErrorKind::InvalidInput, "invalid login identity"));
        }
        let result = (|| {
            let mut wire_bytes = 0usize;
            for _ in 0..32 {
                let message = if let Some(value) = self.pending.pop_front() {
                    self.pending_bytes = self
                        .pending_bytes
                        .saturating_sub(serde_json::to_vec(&value).map_or(0, |v| v.len()));
                    value
                } else {
                    let Some((value, size)) = self
                        .transport
                        .poll(Instant::now() + Duration::from_millis(10))?
                    else {
                        return Ok(LoginProgress::Pending);
                    };
                    wire_bytes = wire_bytes.saturating_add(size);
                    if wire_bytes > self.config.max_wire_bytes {
                        return Err(error(ErrorKind::Limit, "native login event byte limit"));
                    }
                    value
                };
                if message.get("id").is_some() && message.get("method").is_some() {
                    self.deny_request(&message, Instant::now() + Duration::from_secs(1))?;
                    continue;
                }
                if message["method"] == "account/login/completed" {
                    let params = &message["params"];
                    if params["loginId"].as_str() != Some(login_id) {
                        continue;
                    }
                    return match params["success"].as_bool() {
                        Some(true) => Ok(LoginProgress::Completed),
                        Some(false) => Ok(LoginProgress::Failed),
                        None => Err(error(
                            ErrorKind::Protocol,
                            "invalid native login completion",
                        )),
                    };
                }
                // Raw native metadata/errors may contain tokens or account data.
                // Only the completion boolean for our exact login is projected.
                if message.get("id").is_some() {
                    return Err(error(
                        ErrorKind::Protocol,
                        "unexpected native login response",
                    ));
                }
            }
            Ok(LoginProgress::Pending)
        })();
        self.close_if_error(result)
    }
    pub fn login_device_cancel(&mut self, login_id: &str) -> Result<()> {
        self.require_initialized()?;
        if !valid_id(login_id) {
            return Err(error(ErrorKind::InvalidInput, "invalid login identity"));
        }
        let result = self
            .rpc("account/login/cancel", json!({"loginId":login_id}))
            .map(|_| ());
        self.shutdown();
        result
    }
    pub fn account_read(&mut self) -> Result<AccountStatus> {
        self.require_initialized()?;
        let result = (|| {
            let result = self.rpc("account/read", json!({"refreshToken":false}))?;
            let account = result.get("account").ok_or_else(|| {
                error(
                    ErrorKind::Protocol,
                    "account/read result has no account field",
                )
            })?;
            if !account.is_null() && !account.is_object() {
                return Err(error(
                    ErrorKind::Protocol,
                    "invalid account/read account shape",
                ));
            }
            let authenticated = account.is_object();
            let auth_kind = if authenticated {
                Some(
                    match account.get("type").and_then(Value::as_str) {
                        Some("chatgpt") => "chatgpt",
                        Some("apiKey") => "api_key",
                        Some("chatgptAuthTokens") => "chatgpt_auth_tokens",
                        Some("amazonBedrock") => "amazon_bedrock",
                        _ => "unknown",
                    }
                    .into(),
                )
            } else {
                None
            };
            let plan_type = account
                .get("planType")
                .and_then(Value::as_str)
                .map(|p| match p {
                    "free" | "plus" | "pro" | "team" | "business" | "enterprise" | "edu" | "go" => {
                        p.to_owned()
                    }
                    _ => "unknown".into(),
                });
            let requires_openai_auth = result
                .get("requiresOpenaiAuth")
                .and_then(Value::as_bool)
                .ok_or_else(|| error(ErrorKind::Protocol, "invalid account/read auth status"))?;
            Ok(AccountStatus {
                authenticated,
                auth_kind,
                plan_type,
                requires_openai_auth,
            })
        })();
        self.close_if_error(result)
    }
    pub fn thread_start(&mut self, model: Option<&str>) -> Result<ThreadHandle> {
        self.thread_start_with_tools(model, &[])
    }
    pub fn thread_start_with_tools(
        &mut self,
        model: Option<&str>,
        tools: &[DynamicToolSpec],
    ) -> Result<ThreadHandle> {
        self.require_initialized()?;
        let (specs, names) = self.prepare_tools(tools)?;
        let mut params = self.thread_params();
        if !tools.is_empty() {
            params["dynamicTools"] = json!(specs);
        }
        if let Some(model) = model {
            if model.is_empty() || model.len() > 128 {
                return Err(error(ErrorKind::InvalidInput, "invalid model name"));
            }
            params["model"] = json!(model);
        }
        let result = (|| {
            let reply = self.rpc("thread/start", params)?;
            let thread = self.remember_thread(&reply)?;
            self.tool_names.insert(thread.id.clone(), names);
            Ok(thread)
        })();
        self.close_if_error(result)
    }
    pub fn thread_resume(&mut self, id: &str) -> Result<ThreadHandle> {
        self.thread_resume_with_tools(id, &[])
    }
    /// 0.165.0 restores persisted tool schemas itself; this list only rebinds
    /// the local allowlist and must match that thread's original definitions.
    pub fn thread_resume_with_tools(
        &mut self,
        id: &str,
        tools: &[DynamicToolSpec],
    ) -> Result<ThreadHandle> {
        self.require_initialized()?;
        let (_, names) = self.prepare_tools(tools)?;
        if !valid_id(id) {
            return Err(error(ErrorKind::InvalidInput, "invalid thread ID"));
        }
        let mut params = self.thread_params();
        params["threadId"] = json!(id);
        let result = (|| {
            let result = self.rpc("thread/resume", params)?;
            let thread = self.remember_thread(&result)?;
            if thread.id != id {
                return Err(error(
                    ErrorKind::Protocol,
                    "resumed thread identity mismatch",
                ));
            }
            self.tool_names.insert(thread.id.clone(), names);
            Ok(thread)
        })();
        self.close_if_error(result)
    }

    fn thread_params(&self) -> Value {
        // SandboxMode/AskForApproval are kebab-case in 0.165.0's generated
        // ThreadStartParams, unlike SandboxPolicy's tagged readOnly variant.
        let mut params = json!({"cwd":self.config.workspace,"approvalPolicy":"untrusted","sandbox":"read-only","modelProvider":"openai"});
        if let Some(instructions) = &self.config.developer_instructions {
            params["developerInstructions"] = json!(instructions);
        }
        params
    }
    fn prepare_tools(&self, tools: &[DynamicToolSpec]) -> Result<(Vec<Value>, BTreeSet<String>)> {
        if tools.len() > 32 {
            return Err(error(ErrorKind::Limit, "dynamic tool definition limit"));
        }
        let mut names = BTreeSet::new();
        let mut specs = Vec::new();
        for tool in tools {
            if tool.name.is_empty()
                || tool.name.len() > 64
                || !tool
                    .name
                    .bytes()
                    .all(|b| b.is_ascii_alphanumeric() || b"_-".contains(&b))
                || tool.description.is_empty()
                || tool.description.len() > 4096
                || tool.input_schema.get("type").and_then(Value::as_str) != Some("object")
                || !names.insert(tool.name.clone())
            {
                return Err(error(
                    ErrorKind::InvalidInput,
                    "invalid or duplicate dynamic tool specification",
                ));
            }
            specs.push(json!({"type":"function","name":tool.name,"description":tool.description,"inputSchema":tool.input_schema}));
        }
        if serde_json::to_vec(&specs).map_or(usize::MAX, |v| v.len()) > self.config.max_input_bytes
        {
            return Err(error(ErrorKind::Limit, "dynamic tool schema byte limit"));
        }
        Ok((specs, names))
    }
    fn remember_thread(&mut self, result: &Value) -> Result<ThreadHandle> {
        let id = result
            .pointer("/thread/id")
            .and_then(Value::as_str)
            .filter(|s| valid_id(s))
            .ok_or_else(|| error(ErrorKind::Protocol, "missing valid thread ID"))?
            .to_owned();
        if self.threads.len() >= 32 && !self.threads.contains(&id) {
            return Err(error(ErrorKind::Limit, "engine thread handle limit"));
        }
        self.threads.insert(id.clone());
        Ok(ThreadHandle { id })
    }
    fn require_initialized(&self) -> Result<()> {
        if self.initialized {
            Ok(())
        } else {
            Err(error(ErrorKind::State, "initialize the engine first"))
        }
    }
    fn close_if_error<T>(&mut self, result: Result<T>) -> Result<T> {
        if result.is_err() {
            self.shutdown();
        }
        result
    }

    /// The callback runs synchronously; callers must keep it bounded and prompt.
    pub fn turn_start(
        &mut self,
        thread_id: &str,
        input: &[Input],
        mut on_event: impl FnMut(EngineEvent),
    ) -> Result<TurnOutput> {
        self.turn_start_with_tools(thread_id, input, &mut on_event, |_| {
            Err(error(
                ErrorKind::Unsupported,
                "no runtime tool callback was supplied",
            ))
        })
    }
    /// Runtime tools are synchronous callbacks and must enforce their own I/O
    /// deadlines. No shell or arbitrary RPC is used to execute a platform tool.
    pub fn turn_start_with_tools(
        &mut self,
        thread_id: &str,
        input: &[Input],
        mut on_event: impl FnMut(EngineEvent),
        mut on_tool: impl FnMut(ToolCall) -> Result<ToolResult>,
    ) -> Result<TurnOutput> {
        self.require_initialized()?;
        if !self.threads.contains(thread_id) {
            return Err(error(
                ErrorKind::State,
                "start or resume the thread on this connection first",
            ));
        }
        let input = self.prepare_input(input)?;
        let account = self.account_read()?;
        if !account.authenticated
            || !matches!(
                account.auth_kind.as_deref(),
                Some("chatgpt" | "chatgpt_auth_tokens")
            )
        {
            return Err(error(
                ErrorKind::State,
                "a ChatGPT plan login in the dedicated CODEX_HOME is required before a model turn",
            ));
        }
        let result = (|| {
            let deadline = Instant::now() + self.config.timeout;
            let mut wire_bytes = 0_usize;
            let start = self.rpc_bounded(
                "turn/start",
                json!({"threadId":thread_id,"input":input,
                    "cwd":self.config.workspace,"approvalPolicy":"untrusted",
                    "sandboxPolicy":{"type":"readOnly","networkAccess":false}
                }),
                deadline,
                &mut wire_bytes,
            )?;
            let turn_id = start
                .pointer("/turn/id")
                .and_then(Value::as_str)
                .filter(|s| valid_id(s))
                .ok_or_else(|| error(ErrorKind::Protocol, "turn/start returned no valid turn ID"))?
                .to_owned();
            self.read_turn(
                thread_id,
                &turn_id,
                &mut on_event,
                &mut on_tool,
                deadline,
                &mut wire_bytes,
            )
        })();
        self.close_if_error(result)
    }
    fn prepare_input(&self, input: &[Input]) -> Result<Vec<Value>> {
        if input.is_empty() || input.len() > 64 {
            return Err(error(
                ErrorKind::InvalidInput,
                "input requires 1..64 blocks",
            ));
        }
        let mut bytes = 0_usize;
        let mut result = Vec::new();
        for block in input {
            let value = match block {
                Input::Audio { .. } | Input::Video { .. } => {
                    return Err(error(
                        ErrorKind::Unsupported,
                        "Codex subscription adapter does not support audio/video; captions are not an equivalent input",
                    ));
                }
                Input::Text { text } => {
                    bytes = bytes.saturating_add(text.len());
                    if text.is_empty() {
                        return Err(error(ErrorKind::InvalidInput, "empty text input"));
                    }
                    json!({"type":"text","text":text})
                }
                Input::LocalImage { path } => {
                    let path = fs::canonicalize(path).map_err(|_| {
                        error(ErrorKind::InvalidInput, "local image is unavailable")
                    })?;
                    let meta = fs::metadata(&path).map_err(|_| {
                        error(ErrorKind::InvalidInput, "cannot inspect local image")
                    })?;
                    if !path.starts_with(&self.config.workspace)
                        || !meta.is_file()
                        || meta.len() == 0
                        || meta.len() > self.config.max_image_bytes
                    {
                        return Err(error(
                            ErrorKind::InvalidInput,
                            "image must be a bounded file inside the read-only workspace",
                        ));
                    }
                    bytes = bytes.saturating_add(path.as_os_str().len());
                    json!({"type":"localImage","path":path})
                }
            };
            if bytes > self.config.max_input_bytes {
                return Err(error(ErrorKind::Limit, "engine input exceeds limit"));
            }
            result.push(value);
        }
        Ok(result)
    }

    fn read_turn(
        &mut self,
        thread_id: &str,
        turn_id: &str,
        on_event: &mut impl FnMut(EngineEvent),
        on_tool: &mut impl FnMut(ToolCall) -> Result<ToolResult>,
        deadline: Instant,
        wire_bytes: &mut usize,
    ) -> Result<TurnOutput> {
        let mut output_bytes = 0_usize;
        let mut messages = BTreeMap::<String, AgentMessage>::new();
        let mut order = Vec::<String>::new();
        let mut calls = BTreeSet::new();
        let mut call_count = 0_usize;
        loop {
            while let Some(method) = self.denied.pop_front() {
                on_event(EngineEvent::ApprovalDenied { method });
            }
            let message = if let Some(value) = self.pending.pop_front() {
                self.pending_bytes = self
                    .pending_bytes
                    .saturating_sub(serde_json::to_vec(&value).map_or(0, |v| v.len()));
                value
            } else {
                self.next(deadline, wire_bytes)?
            };
            if message.get("id").is_some() {
                if message.get("method").and_then(Value::as_str) == Some("item/tool/call") {
                    call_count += 1;
                    if call_count > self.config.max_tool_calls {
                        return Err(error(ErrorKind::Limit, "runtime tool request count limit"));
                    }
                    self.run_tool(&message, thread_id, turn_id, &mut calls, deadline, on_tool)?;
                } else {
                    self.deny_request(&message, deadline)?;
                }
                continue;
            }
            let Some(method) = message.get("method").and_then(Value::as_str) else {
                return Err(error(ErrorKind::Protocol, "notification has no method"));
            };
            let params = &message["params"];
            if params
                .get("threadId")
                .and_then(Value::as_str)
                .is_some_and(|id| id != thread_id)
                || params
                    .get("turnId")
                    .and_then(Value::as_str)
                    .is_some_and(|id| id != turn_id)
            {
                continue;
            }
            match method {
                "item/agentMessage/delta" => {
                    let id = params
                        .get("itemId")
                        .and_then(Value::as_str)
                        .filter(|s| valid_id(s))
                        .ok_or_else(|| error(ErrorKind::Protocol, "delta has no item ID"))?;
                    let text = params
                        .get("delta")
                        .and_then(Value::as_str)
                        .ok_or_else(|| error(ErrorKind::Protocol, "delta has no text"))?;
                    output_bytes = output_bytes.saturating_add(text.len());
                    if output_bytes > self.config.max_output_bytes {
                        return Err(error(ErrorKind::Limit, "engine text output exceeds limit"));
                    }
                    if !messages.contains_key(id) {
                        if messages.len() >= 128 {
                            return Err(error(ErrorKind::Limit, "engine message count limit"));
                        }
                        order.push(id.to_owned());
                    }
                    messages
                        .entry(id.into())
                        .or_insert_with(|| AgentMessage {
                            id: id.into(),
                            text: String::new(),
                            phase: None,
                        })
                        .text
                        .push_str(text);
                    on_event(EngineEvent::Delta {
                        item_id: id.into(),
                        text: text.into(),
                    });
                }
                "item/completed" => {
                    let item = &params["item"];
                    if item.get("type").and_then(Value::as_str) == Some("agentMessage") {
                        let id = item
                            .get("id")
                            .and_then(Value::as_str)
                            .filter(|s| valid_id(s))
                            .ok_or_else(|| error(ErrorKind::Protocol, "agent message has no ID"))?;
                        let text = item.get("text").and_then(Value::as_str).ok_or_else(|| {
                            error(ErrorKind::Protocol, "agent message has no text")
                        })?;
                        output_bytes = output_bytes.saturating_add(text.len());
                        if output_bytes > self.config.max_output_bytes {
                            return Err(error(
                                ErrorKind::Limit,
                                "engine text output exceeds limit",
                            ));
                        }
                        if !messages.contains_key(id) {
                            if messages.len() >= 128 {
                                return Err(error(ErrorKind::Limit, "engine message count limit"));
                            }
                            order.push(id.to_owned());
                        }
                        let message = AgentMessage {
                            id: id.into(),
                            text: text.into(),
                            phase: metadata_string(item, "phase"),
                        };
                        messages.insert(id.into(), message.clone());
                        on_event(EngineEvent::MessageCompleted { message });
                    }
                }
                "turn/completed" => {
                    if params.pointer("/turn/id").and_then(Value::as_str) != Some(turn_id) {
                        continue;
                    }
                    let status = match params.pointer("/turn/status").and_then(Value::as_str) {
                        Some("completed") => TurnStatus::Completed,
                        Some("interrupted") => TurnStatus::Interrupted,
                        Some("failed") => TurnStatus::Failed,
                        _ => {
                            return Err(error(ErrorKind::Protocol, "unknown terminal turn status"));
                        }
                    };
                    let messages: Vec<_> = order
                        .into_iter()
                        .filter_map(|id| messages.remove(&id))
                        .collect();
                    let text = messages
                        .iter()
                        .filter(|m| m.phase.as_deref() != Some("commentary"))
                        .map(|m| m.text.as_str())
                        .collect::<Vec<_>>()
                        .join("\n");
                    on_event(EngineEvent::TurnCompleted {
                        turn_id: turn_id.into(),
                        status,
                    });
                    return Ok(TurnOutput {
                        thread_id: thread_id.into(),
                        turn_id: turn_id.into(),
                        status,
                        text,
                        messages,
                    });
                }
                _ => (), // Reasoning, commands, errors and other unknown data are never logged.
            }
        }
    }
    fn next(&mut self, deadline: Instant, wire_bytes: &mut usize) -> Result<Value> {
        let (message, size) = self.transport.read(deadline)?;
        *wire_bytes = wire_bytes.saturating_add(size);
        if *wire_bytes > self.config.max_wire_bytes {
            return Err(error(
                ErrorKind::Limit,
                "app-server total output exceeds limit",
            ));
        }
        Ok(message)
    }
    fn run_tool(
        &mut self,
        message: &Value,
        thread_id: &str,
        turn_id: &str,
        calls: &mut BTreeSet<String>,
        deadline: Instant,
        on_tool: &mut impl FnMut(ToolCall) -> Result<ToolResult>,
    ) -> Result<()> {
        let id = message
            .get("id")
            .filter(|id| id.is_string() || id.is_number())
            .ok_or_else(|| error(ErrorKind::Protocol, "invalid dynamic request ID"))?;
        let params = &message["params"];
        let name = params.get("tool").and_then(Value::as_str).unwrap_or("");
        let call_id = params
            .get("callId")
            .and_then(Value::as_str)
            .filter(|s| valid_id(s));
        let allowed = params.get("threadId").and_then(Value::as_str) == Some(thread_id)
            && params.get("turnId").and_then(Value::as_str) == Some(turn_id)
            && params.get("namespace").is_none_or(Value::is_null)
            && self
                .tool_names
                .get(thread_id)
                .is_some_and(|names| names.contains(name))
            && params.get("arguments").is_some_and(Value::is_object)
            && call_id.is_some();
        if calls.len() >= self.config.max_tool_calls {
            return Err(error(ErrorKind::Limit, "runtime tool call limit"));
        }
        let result = if allowed && calls.insert(call_id.unwrap().to_owned()) {
            let call = ToolCall {
                thread_id: thread_id.into(),
                turn_id: turn_id.into(),
                call_id: call_id.unwrap().into(),
                tool: name.into(),
                arguments: params["arguments"].clone(),
            };
            match on_tool(call) {
                Ok(result) => result,
                Err(_) => ToolResult::text(
                    "Runtime tool failed; private error details are withheld.",
                    false,
                ),
            }
        } else {
            ToolResult::text(
                "Unregistered, mismatched or repeated runtime tool call was refused.",
                false,
            )
        };
        if Instant::now() >= deadline {
            return Err(error(
                ErrorKind::Timeout,
                "runtime tool callback exceeded turn deadline",
            ));
        }
        self.validate_tool_result(&result)?;
        self.transport
            .write(&json!({"id":id,"result":result}), deadline)
    }
    fn validate_tool_result(&self, result: &ToolResult) -> Result<()> {
        if result.content_items.is_empty() || result.content_items.len() > 16 {
            return Err(error(ErrorKind::Limit, "runtime tool content count limit"));
        }
        let mut total = 0_usize;
        for item in &result.content_items {
            match item {
                ToolContent::InputText { text } => total = total.saturating_add(text.len()),
                ToolContent::InputImage { image_url } => {
                    if image_url.len() > self.config.max_tool_result_bytes {
                        return Err(error(ErrorKind::Limit, "runtime tool image byte limit"));
                    }
                    let (prefix, encoded) = image_url.split_once(',').ok_or_else(|| {
                        error(
                            ErrorKind::InvalidInput,
                            "tool images must be base64 image data URLs",
                        )
                    })?;
                    if !matches!(
                        prefix,
                        "data:image/jpeg;base64"
                            | "data:image/png;base64"
                            | "data:image/webp;base64"
                    ) {
                        return Err(error(
                            ErrorKind::Unsupported,
                            "tool image requires JPEG, PNG or WebP data URL; audio is unsupported",
                        ));
                    }
                    let decoded = STANDARD
                        .decode(encoded)
                        .map_err(|_| error(ErrorKind::InvalidInput, "invalid tool image base64"))?;
                    if decoded.len() > self.config.max_tool_image_bytes {
                        return Err(error(ErrorKind::Limit, "decoded tool image byte limit"));
                    }
                    let signature_ok = match prefix {
                        "data:image/jpeg;base64" => decoded.starts_with(&[0xff, 0xd8, 0xff]),
                        "data:image/png;base64" => decoded.starts_with(b"\x89PNG\r\n\x1a\n"),
                        _ => {
                            decoded.starts_with(b"RIFF") && decoded.get(8..12) == Some(&b"WEBP"[..])
                        }
                    };
                    if !signature_ok {
                        return Err(error(
                            ErrorKind::InvalidInput,
                            "tool image format does not match data URL",
                        ));
                    }
                    total = total.saturating_add(image_url.len());
                }
            }
            if total > self.config.max_tool_result_bytes {
                return Err(error(ErrorKind::Limit, "runtime tool result byte limit"));
            }
        }
        if serde_json::to_vec(result).map_or(usize::MAX, |v| v.len())
            > self.config.max_tool_result_bytes
        {
            return Err(error(
                ErrorKind::Limit,
                "serialized runtime tool result byte limit",
            ));
        }
        Ok(())
    }
    fn rpc(&mut self, method: &str, params: Value) -> Result<Value> {
        self.rpc_bounded(method, params, Instant::now() + self.config.timeout, &mut 0)
    }
    fn rpc_bounded(
        &mut self,
        method: &str,
        params: Value,
        deadline: Instant,
        bytes: &mut usize,
    ) -> Result<Value> {
        let id = self.next_id;
        self.next_id = self
            .next_id
            .checked_add(1)
            .ok_or_else(|| error(ErrorKind::Limit, "RPC ID exhausted"))?;
        self.transport
            .write(&json!({"id":id,"method":method,"params":params}), deadline)?;
        loop {
            let message = self.next(deadline, bytes)?;
            if message.get("method").is_some() && message.get("id").is_some() {
                if method == "turn/start"
                    && message.get("method").and_then(Value::as_str) == Some("item/tool/call")
                {
                    self.queue_event(message)?;
                } else {
                    self.deny_request(&message, deadline)?;
                }
                continue;
            }
            if message.get("id").is_some() {
                if message.get("id").and_then(Value::as_u64) != Some(id) {
                    return Err(error(ErrorKind::Protocol, "RPC response ID mismatch"));
                }
                if message.get("error").is_some() {
                    // Error strings and additionalDetails can contain authentication data.
                    let code = message["error"]["code"].as_i64();
                    return Err(EngineError::new(
                        ErrorKind::Remote,
                        format!(
                            "app-server rejected {method} (code {code:?}); raw server details withheld"
                        ),
                    ));
                }
                return message
                    .get("result")
                    .cloned()
                    .ok_or_else(|| error(ErrorKind::Protocol, "RPC response has no result"));
            }
            if matches!(
                message.get("method").and_then(Value::as_str),
                Some(
                    "item/agentMessage/delta"
                        | "item/completed"
                        | "turn/completed"
                        | "account/login/completed"
                )
            ) {
                self.queue_event(message)?;
            }
        }
    }
    fn queue_event(&mut self, message: Value) -> Result<()> {
        let size = serde_json::to_vec(&message).map_or(self.config.max_wire_bytes, |v| v.len());
        self.pending_bytes = self.pending_bytes.saturating_add(size);
        if self.pending.len() >= 64 || self.pending_bytes > self.config.max_wire_bytes {
            return Err(error(ErrorKind::Limit, "pending event limit"));
        }
        self.pending.push_back(message);
        Ok(())
    }
    fn deny_request(&mut self, message: &Value, deadline: Instant) -> Result<()> {
        let id = message
            .get("id")
            .filter(|id| id.is_string() || id.is_number())
            .ok_or_else(|| error(ErrorKind::Protocol, "invalid server request ID"))?;
        let method = message
            .get("method")
            .and_then(Value::as_str)
            .ok_or_else(|| error(ErrorKind::Protocol, "unexpected RPC response during turn"))?;
        let result = match method {
            "item/commandExecution/requestApproval" | "item/fileChange/requestApproval" => {
                Some(json!({"decision":"decline"}))
            }
            "item/permissions/requestApproval" => Some(json!({"permissions":{},"scope":"turn"})),
            "mcpServer/elicitation/request" => Some(json!({"action":"decline","content":null})),
            "tool/requestUserInput" | "item/tool/requestUserInput" => Some(json!({"answers":{}})),
            _ => None,
        };
        let response = if let Some(result) = result {
            if self.denied.len() >= 64 {
                return Err(error(ErrorKind::Limit, "server approval request limit"));
            }
            self.denied.push_back(method.into());
            json!({"id":id,"result":result})
        } else {
            json!({"id":id,"error":{"code":-32601,"message":"Client capability unsupported"}})
        };
        self.transport.write(&response, deadline)
    }
    pub fn shutdown(&mut self) {
        self.transport.shutdown();
        self.home_lock.take();
        self.initialized = false;
        self.pending.clear();
        self.pending_bytes = 0;
        self.denied.clear();
    }
}
