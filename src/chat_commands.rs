// SPDX-License-Identifier: MIT
//! Exact, operator-configured chat authorization before any model sees a command.
use crate::{
    agent_sources::{Source, SourceEvent, SourceStore, WatchConfig},
    control_console,
    feature_state::Store,
};
use runtime_control::console::{Binding, Command, CommandConfig, Mode};
use runtime_core::Result;
use runtime_jobs::AgentContext;
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use std::{
    collections::{BTreeMap, BTreeSet},
    path::Path,
};
pub const VERBS: &[&str] = &[
    "help", "status", "agent", "model", "enable", "disable", "quiet", "wake",
];
#[derive(Clone, Default, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
struct State {
    revision: u64,
    config: CommandConfig,
    activated_at: u64,
    selected: BTreeMap<String, String>,
    receipts: Vec<Receipt>,
}
#[derive(Clone, Serialize, Deserialize)]
struct Receipt {
    id: String,
    binding: String,
    at: u64,
    status: String,
    verb: String,
}
fn store(state: &Path) -> Result<Store> {
    Store::open(state, "chat-commands", 128 * 1024)
}
fn load(state: &Path) -> Result<State> {
    let s: State = store(state)?.load()?;
    validate(&s.config)?;
    if s.receipts.len() > 256 || s.selected.len() > 8 {
        return Err("command_state_limit".into());
    }
    Ok(s)
}
fn numeric(s: &str) -> bool {
    !s.is_empty()
        && s.len() <= 10
        && s.bytes().all(|b| b.is_ascii_digit())
        && s.parse::<u32>().is_ok_and(|n| n > 0)
}
fn validate(c: &CommandConfig) -> Result<()> {
    if c.bindings.len() > 8 {
        return Err("command_binding_limit".into());
    }
    let mut ids = BTreeSet::new();
    for b in &c.bindings {
        AgentContext {
            agent: b.agent.clone(),
            persona: "main".into(),
            conversation: None,
        }
        .validate()?;
        AgentContext {
            agent: b.id.clone(),
            ..Default::default()
        }
        .validate()?;
        runtime_core::Scope {
            platform: runtime_core::Platform::Qq,
            account: b.account.clone(),
        }
        .validate()?;
        if !ids.insert(&b.id)
            || b.platform != "qq"
            || b.account == "public"
            || !matches!(b.chat_type.as_str(), "private" | "group")
            || !numeric(&b.chat_id)
            || !numeric(&b.sender_id)
            || (b.chat_type == "private" && b.chat_id != b.sender_id)
            || b.prefix.is_empty()
            || b.prefix.len() > 24
            || !b.prefix.starts_with('/')
            || !b
                .prefix
                .bytes()
                .all(|x| x.is_ascii_alphanumeric() || b"/-_".contains(&x))
            || b.allowed.is_empty()
            || b.allowed.len() > VERBS.len()
            || b.allowed.iter().any(|v| !VERBS.contains(&v.as_str()))
            || !matches!(b.language.as_str(), "en" | "zh-CN")
        {
            return Err("invalid_command_scope".into());
        }
    }
    Ok(())
}
pub fn configuration(state: &Path) -> Result<Value> {
    let s = load(state)?;
    Ok(
        json!({"revision":s.revision,"config":s.config,"selected_agents":s.selected,"receipts":s.receipts.iter().rev().take(20).collect::<Vec<_>>(),"verbs":VERBS,"platforms":["qq"],"default_denied":true,"live_qq_acceptance":false}),
    )
}
pub fn configure(state: &Path, config: CommandConfig, expected: u64) -> Result<Value> {
    validate(&config)?;
    store(state)?.update(|s: &mut State| {
        if s.revision != expected {
            return Err("command revision changed".into());
        }
        s.selected = config
            .bindings
            .iter()
            .map(|b| (b.id.clone(), b.agent.clone()))
            .collect();
        s.config = config;
        s.revision = s.revision.checked_add(1).ok_or("revision exhausted")?;
        s.activated_at = crate::agent_host::now()?;
        Ok(())
    })?;
    configuration(state)
}
const WATCH_PROMPT: &str = "Runtime Command channel (management only).";
pub fn sync_watches(state: &Path) -> Result<()> {
    let s = load(state)?;
    let mut accounts = BTreeSet::new();
    if s.config.enabled {
        for b in &s.config.bindings {
            if b.enabled {
                accounts.insert(b.account.clone());
            }
        }
    }
    let mut watches = SourceStore::open(state)?;
    let current = watches.list()?;
    for w in &current {
        if w.config.id.starts_with("command-")
            && w.config.operator_prompt == WATCH_PROMPT
            && !accounts.contains(&w.config.account)
            && w.config.enabled
        {
            let mut c = w.config.clone();
            c.enabled = false;
            watches.configure(c)?;
        }
    }
    for account in accounts {
        let id = format!(
            "command-{}",
            crate::agent_mind::stable_key(&[account.as_bytes()])
        );
        if let Some(w) = current.iter().find(|w| w.config.id == id) {
            if w.config.operator_prompt != WATCH_PROMPT
                || w.config.account != account
                || w.config.source != Source::QqMessages
            {
                return Err("command_watch_conflict".into());
            }
            if w.config.enabled {
                continue;
            }
        }
        watches.configure(WatchConfig {
            id,
            source: Source::QqMessages,
            account,
            operator_prompt: WATCH_PROMPT.into(),
            context: Default::default(),
            interval_seconds: 5,
            enabled: true,
            include_existing: false,
        })?;
    }
    Ok(())
}
pub fn command_watch(prompt: &str) -> bool {
    prompt == WATCH_PROMPT
}

fn authorized<'a>(
    s: &'a State,
    e: &SourceEvent,
    now: u64,
) -> Option<(&'a Binding, String, String)> {
    if !s.config.enabled
        || e.source != Source::QqMessages
        || e.scope.platform != runtime_core::Platform::Qq
        || e.payload["truncated"] != false
    {
        return None;
    }
    let m = &e.payload["item"];
    if m["direction"] != "incoming" || m["truncated"] != false || m["unexpanded"] != false {
        return None;
    }
    let at = m["server_time"].as_u64()?;
    if at < s.activated_at || at > now + 30 || now.saturating_sub(at) > 600 {
        return None;
    }
    if m["segments"]
        .as_array()
        .is_none_or(|v| v.iter().any(|x| x["type"] != "text"))
    {
        return None;
    }
    let sender = m["from_uin"].as_u64()?.to_string();
    let destination = &m["conversation"]["destination"];
    let actor = m["conversation"]["actor"].as_u64().filter(|n| *n > 0)?;
    let text = m["text"].as_str()?;
    if text.len() > 4096 || text.contains('\0') {
        return None;
    }
    for b in &s.config.bindings {
        if !b.enabled || b.account != e.scope.account || b.sender_id != sender {
            continue;
        }
        let chat = if b.chat_type == "private" {
            let Some(uid) = m["from_uid"].as_str().filter(|v| !v.is_empty()) else {
                continue;
            };
            if destination["kind"] != "private"
                || m["to_uin"].as_u64() != Some(actor)
                || destination["uid"].as_str() != Some(uid)
            {
                continue;
            }
            if !m["group_code"].is_null() && m["group_code"] != 0 {
                continue;
            }
            sender.clone()
        } else {
            if destination["kind"] != "group" || destination["code"] != m["group_code"] {
                continue;
            }
            m["group_code"].as_u64()?.to_string()
        };
        if b.chat_id != chat {
            continue;
        }
        let Some(rest) = text.strip_prefix(&b.prefix) else {
            continue;
        };
        let rest = if rest.is_empty() {
            "help"
        } else {
            let Some(v) = rest.strip_prefix(' ') else {
                continue;
            };
            v
        };
        let (verb, args) = rest.split_once(' ').unwrap_or((rest, ""));
        if verb.is_empty() {
            return None;
        }
        return Some((b, verb.into(), args.trim().into()));
    }
    None
}
fn perform(
    state: &Path,
    b: &Binding,
    target: &str,
    verb: &str,
    args: &str,
    _id: &str,
) -> Result<String> {
    if !b.allowed.iter().any(|v| v == verb) {
        return Err("command_denied".into());
    }
    let no_args = || {
        if args.is_empty() {
            Ok(())
        } else {
            Err("unexpected_arguments".to_string())
        }
    };
    let command = match verb {
        "help" => {
            no_args()?;
            return Ok(b
                .allowed
                .iter()
                .map(|v| format!("{} {}", b.prefix, v))
                .collect::<Vec<_>>()
                .join(" · "));
        }
        "status" => {
            no_args()?;
            let mode = control_console::mode(state, target)?;
            return Ok(format!(
                "{target} · {}",
                serde_json::to_value(mode)
                    .unwrap()
                    .as_str()
                    .unwrap_or("unknown")
            ));
        }
        "disable" | "quiet" | "enable" => {
            no_args()?;
            Command::Mode {
                agent: target.into(),
                mode: match verb {
                    "disable" => Mode::Paused,
                    "quiet" => Mode::Quiet,
                    _ => Mode::Active,
                },
            }
        }
        "agent" => Command::Select { agent: args.into() },
        "model" => {
            let (engine, model) = args.split_once(' ').ok_or("expected_engine_and_model")?;
            let revision = crate::agent_profiles::ProfileStore::open(state)?
                .get(target)?
                .map_or(0, |p| p.revision);
            Command::Profile {
                agent: target.into(),
                engine: engine.into(),
                model: model.into(),
                expected_revision: revision,
            }
        }
        "wake" => Command::Activity {
            id: args.into(),
            enabled: true,
            wake: true,
        },
        _ => return Err("command_denied".into()),
    };
    control_console::execute(state, command)?;
    if verb == "agent" {
        store(state)?.update(|s: &mut State| {
            s.selected.insert(b.id.clone(), args.into());
            Ok(())
        })?;
    }
    Ok(if b.language == "zh-CN" {
        if verb == "disable" {
            "已暂停接新一轮；当前轮结束后停止".into()
        } else {
            "已应用".into()
        }
    } else {
        if verb == "disable" {
            "Paused after the current turn".into()
        } else {
            "Applied".into()
        }
    })
}
/// At-most-once reservation survives crashes. Unknown attempts are never replayed.
/// Returns true only for a scoped command; the source's cursor commit stays separate.
pub fn dispatch_with(
    state: &Path,
    e: &SourceEvent,
    now: u64,
    mut reply: impl FnMut(&str, &str, &str, &str) -> Result<()>,
) -> Result<bool> {
    let s = load(state)?;
    let Some((binding, verb, args)) = authorized(&s, e, now) else {
        return Ok(false);
    };
    let b = binding.clone();
    let id = crate::agent_mind::stable_key(&[
        b"qq-command",
        e.scope.account.as_bytes(),
        e.source_key.as_bytes(),
    ]);
    let reserved = store(state)?.update(|s: &mut State| {
        if s.receipts.iter().any(|r| r.id == id) {
            return Ok(false);
        }
        s.receipts.push(Receipt {
            id: id.clone(),
            binding: b.id.clone(),
            at: now,
            status: "attempting".into(),
            verb: verb.clone(),
        });
        if s.receipts.len() > 256 {
            s.receipts.remove(0);
        }
        Ok(true)
    })?;
    if !reserved {
        return Ok(true);
    }
    let target = s.selected.get(&b.id).unwrap_or(&b.agent);
    let result = perform(state, &b, target, &verb, &args, &id);
    let message = match &result {
        Ok(v) => format!("Glimlink · {v}"),
        Err(_) => {
            if b.language == "zh-CN" {
                "Glimlink · 指令未应用，请检查权限、参数或状态".into()
            } else {
                "Glimlink · Command not applied; check permissions, arguments or state".into()
            }
        }
    };
    let ack = if b.reply {
        match e.payload["item"]["target_ref"].as_str() {
            Some(target) => reply(&b.account, target, &format!("command-reply-{id}"), &message),
            None => Err("command_reply_target_missing".into()),
        }
    } else {
        Ok(())
    };
    store(state)?.update(|s: &mut State| {
        if let Some(r) = s.receipts.iter_mut().find(|r| r.id == id) {
            r.status = if result.is_err() {
                "rejected"
            } else if ack.is_err() {
                "applied_reply_unconfirmed"
            } else {
                "applied"
            }
            .into();
        }
        Ok(())
    })?;
    Ok(true)
}
