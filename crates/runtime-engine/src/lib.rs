// SPDX-License-Identifier: MIT
//! Bounded stdio integration with the native Codex app-server.
//! No credential file is read by this crate, and no model is called implicitly.
pub mod antigravity;
pub mod codex;
mod credential_home;
#[cfg(target_os = "linux")]
pub mod login_pty;
mod process_deadline;
mod transport;
pub use antigravity::{AntigravityClient, AntigravityConfig};
pub use codex::{CodexClient, CodexConfig};
/// Public upstream metadata with only tool_mode changed, paired with 0.165.0.
/// This contains no login state. See data/README.md for provenance and limits.
pub const CODEX_DIRECT_MODEL_CATALOG: &[u8] = include_bytes!("../data/codex-0.165.0-direct.json");
use serde::{Deserialize, Serialize};
use std::{fmt, path::PathBuf};

pub type Result<T> = std::result::Result<T, EngineError>;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ErrorKind {
    InvalidInput,
    Unsupported,
    Authentication,
    State,
    Io,
    Protocol,
    Remote,
    Timeout,
    Limit,
    Closed,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct EngineError {
    pub kind: ErrorKind,
    pub detail: String,
}
impl EngineError {
    pub(crate) fn new(kind: ErrorKind, detail: impl Into<String>) -> Self {
        Self {
            kind,
            detail: detail.into(),
        }
    }
}
impl fmt::Display for EngineError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}", self.detail)
    }
}
impl std::error::Error for EngineError {}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum Input {
    Text { text: String },
    LocalImage { path: PathBuf },
    Audio { path: PathBuf },
    Video { path: PathBuf },
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ServerInfo {
    pub user_agent: Option<String>,
    pub platform_family: Option<String>,
    pub platform_os: Option<String>,
}

/// Deliberately excludes email, account IDs, raw responses, and credentials.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AccountStatus {
    pub authenticated: bool,
    pub auth_kind: Option<String>,
    pub plan_type: Option<String>,
    pub requires_openai_auth: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ThreadHandle {
    pub id: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TurnStatus {
    Completed,
    Failed,
    Interrupted,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AgentMessage {
    pub id: String,
    pub text: String,
    pub phase: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "event", rename_all = "snake_case")]
pub enum EngineEvent {
    Delta { item_id: String, text: String },
    MessageCompleted { message: AgentMessage },
    ApprovalDenied { method: String },
    TurnCompleted { turn_id: String, status: TurnStatus },
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TurnOutput {
    pub thread_id: String,
    pub turn_id: String,
    pub status: TurnStatus,
    pub text: String,
    pub messages: Vec<AgentMessage>,
}

/// Native app-server 0.165.0 function tool specification. Namespaces are not
/// exposed by this first adapter; use distinct names for platform functions.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DynamicToolSpec {
    pub name: String,
    pub description: String,
    pub input_schema: serde_json::Value,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ToolCall {
    pub thread_id: String,
    pub turn_id: String,
    pub call_id: String,
    pub tool: String,
    pub arguments: serde_json::Value,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type")]
pub enum ToolContent {
    #[serde(rename = "inputText")]
    InputText { text: String },
    #[serde(rename = "inputImage")]
    InputImage {
        #[serde(rename = "imageUrl")]
        image_url: String,
    },
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ToolResult {
    pub content_items: Vec<ToolContent>,
    pub success: bool,
}
impl ToolResult {
    pub fn text(text: impl Into<String>, success: bool) -> Self {
        Self {
            content_items: vec![ToolContent::InputText { text: text.into() }],
            success,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Capabilities {
    pub engine: String,
    pub protocol_available: bool,
    pub text: bool,
    pub local_image: bool,
    pub audio: bool,
    pub video: bool,
    pub dynamic_tools: bool,
    pub model_input_verified: bool,
    pub detail: String,
}
pub fn codex_capabilities() -> Capabilities {
    Capabilities {
        engine: "codex".into(), protocol_available: true, text: true, local_image: true,
        audio: false, video: false, dynamic_tools: true, model_input_verified: true,
        detail: "Native 0.165.0 + gpt-6.1-sol subscription text, direct Runtime tools and actual tool-result image input passed controlled live acceptance. This is evidence for that tested route, not every model/account. Native audio/video remain unsupported.".into(),
    }
}

pub struct Antigravity;
impl Antigravity {
    pub fn capabilities() -> Capabilities {
        Capabilities {
            engine: "antigravity".into(), protocol_available: true, text: true,
            local_image: false, audio: false, video: false, dynamic_tools: true,
            model_input_verified: true,
            detail: "Native agy 1.4.11 ACP v1, dedicated Google subscription cache, shared operator persona and actual Runtime memory tools passed controlled live acceptance. Tools use a native Rust MCP bridge to the same Runtime. Direct image/audio/video inputs remain unverified and are rejected.".into(),
        }
    }
}
