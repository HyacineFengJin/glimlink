// SPDX-License-Identifier: MIT
//! Human management messages. Never exposed in the model tool catalog.
use serde::{Deserialize, Serialize};
use serde_json::Value;

pub const MAX_REQUEST: usize = 64 * 1024;
pub const MAX_RESPONSE: usize = 512 * 1024;

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(tag = "action", rename_all = "snake_case", deny_unknown_fields)]
pub enum Command {
    Connections,
    Snapshot,
    Conversation {
        persona: String,
        conversation: String,
    },
    Job {
        id: String,
    },
    Send {
        id: String,
        agent: String,
        persona: String,
        conversation: String,
        text: String,
    },
    Select {
        agent: String,
    },
    Mode {
        agent: String,
        mode: Mode,
    },
    Profile {
        agent: String,
        engine: String,
        model: String,
        expected_revision: u64,
    },
    Activity {
        id: String,
        enabled: bool,
        wake: bool,
    },
    Cancel {
        id: String,
    },
    CommandsGet,
    CommandsSet {
        config: CommandConfig,
        expected_revision: u64,
    },
}
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Mode {
    #[default]
    Active,
    Quiet,
    Paused,
}

#[derive(Clone, Debug, Default, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct CommandConfig {
    pub enabled: bool,
    pub bindings: Vec<Binding>,
}
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Binding {
    pub id: String,
    pub enabled: bool,
    pub platform: String,
    pub account: String,
    pub chat_type: String,
    pub chat_id: String,
    pub sender_id: String,
    pub prefix: String,
    pub agent: String,
    pub allowed: Vec<String>,
    pub reply: bool,
    pub language: String,
}
#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Request {
    pub version: u32,
    pub id: u64,
    pub command: Command,
}
#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Response {
    pub version: u32,
    pub id: u64,
    pub ok: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub result: Option<Value>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
}
