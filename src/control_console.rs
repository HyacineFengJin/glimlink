// SPDX-License-Identifier: MIT
//! Short management transactions; no Runtime/kernel lock or resident server.
use crate::{
    agent_host,
    agent_profiles::{ProfileInput, ProfileStore},
    feature_state::Store,
};
use runtime_control::console::{Command, MAX_REQUEST, MAX_RESPONSE, Mode, Request, Response};
use runtime_core::Result;
use runtime_jobs::{AgentContext, Job, Queue};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use std::{
    collections::{BTreeMap, BTreeSet},
    io::{BufRead, Read, Write},
    path::Path,
};

#[derive(Clone, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct Preferences {
    pub revision: u64,
    pub selected_agent: String,
    pub modes: BTreeMap<String, Mode>,
}
impl Default for Preferences {
    fn default() -> Self {
        Self {
            revision: 0,
            selected_agent: "main".into(),
            modes: BTreeMap::new(),
        }
    }
}
impl Preferences {
    pub fn permits(&self, job: &Job) -> bool {
        match self
            .modes
            .get(&job.context.agent)
            .copied()
            .unwrap_or_default()
        {
            Mode::Active => true,
            Mode::Quiet => job.activity.is_none() && job.event.is_none(),
            Mode::Paused => false,
        }
    }
    pub fn autonomous(&self, agent: &str) -> bool {
        self.modes.get(agent).copied().unwrap_or_default() == Mode::Active
    }
}
fn store(state: &Path) -> Result<Store> {
    Store::open(state, "operator-console", 32768)
}
pub fn preferences(state: &Path) -> Result<Preferences> {
    let p: Preferences = store(state)?.load()?;
    if p.modes.len() > 32 {
        return Err("control_quota".into());
    }
    context(&p.selected_agent, "main", "operator")?;
    Ok(p)
}
pub fn mode(state: &Path, agent: &str) -> Result<Mode> {
    Ok(preferences(state)?
        .modes
        .get(agent)
        .copied()
        .unwrap_or_default())
}
pub fn eligible(state: &Path, job: &Job) -> Result<bool> {
    Ok(match mode(state, &job.context.agent)? {
        Mode::Active => true,
        Mode::Quiet => job.activity.is_none() && job.event.is_none(),
        Mode::Paused => false,
    })
}
fn context(agent: &str, persona: &str, conversation: &str) -> Result<AgentContext> {
    let c = AgentContext {
        agent: agent.into(),
        persona: persona.into(),
        conversation: Some(conversation.into()),
    };
    c.validate()?;
    Ok(c)
}
fn info(job: &Job) -> Value {
    json!({"id":job.id,"status":job.status,"context":job.context,"engine":job.engine,"model":job.execution_profile.as_ref().and_then(|p|p.model.as_ref()),"updated_at":job.updated_at,"created_at":job.created_at,"activity":job.activity.is_some(),"tool_calls":job.progress.tool_calls,"error":job.error.as_ref().map(|s|crate::agent_mind::clip(s,256))})
}
fn host_snapshot(state: &Path) -> Result<Value> {
    use std::os::unix::fs::PermissionsExt;
    let path = state.join("agent-status.json");
    if !path.try_exists().map_err(|_| "host_status_unavailable")? {
        return Ok(Value::Null);
    }
    let mut file = runtime_core::resources::open_regular(&path, 65536)?;
    if file
        .metadata()
        .map_err(|_| "host_status_unavailable")?
        .permissions()
        .mode()
        & 0o077
        != 0
    {
        return Err("host_status_not_private".into());
    }
    let mut data = Vec::new();
    file.read_to_end(&mut data)
        .map_err(|_| "host_status_unavailable")?;
    serde_json::from_slice(&data).map_err(|_| "invalid_host_status".into())
}
pub fn execute(state: &Path, cmd: Command) -> Result<Value> {
    let now = agent_host::now()?;
    match cmd {
        Command::Connections => crate::operator_identities::list(state),
        Command::Snapshot => {
            let mut jobs = Queue::open(state)?.list(runtime_jobs::MAX_JOBS)?;
            jobs.sort_by_key(|j| j.created_at);
            let profiles = ProfileStore::open(state)?.list();
            let conversations = crate::agent_dialogue::Store::open(state)?.conversations()?;
            let mut names: BTreeSet<String> = profiles.keys().cloned().collect();
            for j in &jobs {
                names.insert(j.context.agent.clone());
            }
            names.insert("main".into());
            let prefs = preferences(state)?;
            let host = host_snapshot(state)?;
            let fresh = host["updated_at"].as_u64().is_some_and(|t| {
                now.saturating_sub(t)
                    <= if host["phase"] == "running_job" {
                        660
                    } else {
                        45
                    }
            }) && host["status"] != "stopped"
                && host["status"] != "needs_auth";
            Ok(
                json!({"preferences":prefs,"profiles":profiles,"agents":names,"conversations":conversations,"jobs":jobs.iter().rev().take(32).map(info).collect::<Vec<_>>(),"host":host,"worker_online":fresh,"activities":crate::agent_activity::ActivityStore::open(state)?.list()?,"changes_apply":"next_turn","commands":crate::chat_commands::configuration(state)?}),
            )
        }
        Command::Conversation {
            persona,
            conversation,
        } => {
            let ctx = context("main", &persona, &conversation)?;
            let mut jobs = Queue::open(state)?
                .list(runtime_jobs::MAX_JOBS)?
                .into_iter()
                .filter(|j| {
                    j.context.persona == persona
                        && j.context.conversation.as_deref() == Some(conversation.as_str())
                })
                .collect::<Vec<_>>();
            jobs.sort_by_key(|j| j.created_at);
            let items = jobs
                .iter()
                .rev()
                .take(8)
                .map(|j| {
                    let mut v = info(j);
                    v["user"] = json!(crate::agent_mind::clip(&j.prompt, 2048));
                    v["assistant"] =
                        json!(j.text.as_ref().map(|s| crate::agent_mind::clip(s, 4096)));
                    v["truncated"] = json!(
                        j.prompt.len() > 2048 || j.text.as_ref().is_some_and(|s| s.len() > 4096)
                    );
                    v
                })
                .collect::<Vec<_>>();
            Ok(
                json!({"history":crate::agent_dialogue::Store::open(state)?.history(&ctx)?,"jobs":items,"bounded":true}),
            )
        }
        Command::Job { id } => {
            let j = Queue::open(state)?.get(&id)?;
            let mut v = info(&j);
            v["user"] = json!(j.prompt);
            v["assistant"] = json!(j.text);
            Ok(v)
        }
        Command::Send {
            id,
            agent,
            persona,
            conversation,
            text,
        } => {
            let ctx = context(&agent, &persona, &conversation)?;
            if text.trim().is_empty()
                || text.len() > runtime_jobs::MAX_PROMPT_BYTES
                || text.contains('\0')
            {
                return Err("invalid_message".into());
            }
            let profile = ProfileStore::open(state)?
                .get(&agent)?
                .ok_or("profile_required")?;
            let q = Queue::open(state)?;
            if let Some(j) = q
                .list(runtime_jobs::MAX_JOBS)?
                .into_iter()
                .find(|j| j.id == id)
            {
                if j.prompt == text && j.context == ctx {
                    return Ok(json!({"job":info(&j),"reused":true}));
                }
                return Err("message_id_conflict".into());
            }
            let j = q.enqueue_configured(&id, &text, None, ctx, Some(profile), now)?;
            Ok(json!({"job":info(&j),"queued":true,"mode":mode(state,&agent)?}))
        }
        Command::Select { agent } => {
            if ProfileStore::open(state)?.get(&agent)?.is_none() {
                return Err("profile_required".into());
            }
            store(state)?.update(|p: &mut Preferences| {
                p.selected_agent = agent;
                p.revision = p.revision.checked_add(1).ok_or("revision_exhausted")?;
                Ok(json!(p))
            })
        }
        Command::Mode { agent, mode } => {
            context(&agent, "main", "operator")?;
            store(state)?.update(|p: &mut Preferences| {
                if p.modes.len() >= 32 && !p.modes.contains_key(&agent) {
                    return Err("control_quota".into());
                }
                p.modes.insert(agent, mode);
                p.revision = p.revision.checked_add(1).ok_or("revision_exhausted")?;
                Ok(json!({"preferences":p,"applies_after_current_turn":true}))
            })
        }
        Command::Profile {
            agent,
            engine,
            model,
            expected_revision,
        } => {
            if !matches!(engine.as_str(), "codex" | "antigravity") {
                return Err("invalid_engine".into());
            }
            let mut s = ProfileStore::open(state)?;
            let old = s.get(&agent)?;
            let base = old.clone().or(s.get("main")?);
            let input = ProfileInput {
                engine,
                model: if model.trim().is_empty() {
                    None
                } else {
                    Some(model)
                },
                allowed_tools: base
                    .as_ref()
                    .map_or(Some(vec!["runtime_memory".into()]), |p| {
                        p.allowed_tools.clone()
                    }),
                default_accounts: base.map(|p| p.default_accounts).unwrap_or_default(),
            };
            Ok(
                json!({"agent":agent,"profile":s.set(&agent,input,Some(expected_revision))?,"queued_jobs_unchanged":true}),
            )
        }
        Command::Activity { id, enabled, wake } => {
            crate::agent_activity::ActivityStore::open(state)?.toggle(&id, enabled, wake, now)
        }
        Command::Cancel { id } => Ok(
            json!({"canceled":Queue::open(state)?.cancel_pending(&[id],now)?,"running_turn_unchanged":true}),
        ),
        Command::CommandsGet => crate::chat_commands::configuration(state),
        Command::CommandsSet {
            config,
            expected_revision,
        } => crate::chat_commands::configure(state, config, expected_revision),
    }
}
pub fn serve(state: &Path, mut input: impl BufRead, mut output: impl Write) -> Result<()> {
    loop {
        let mut bytes = Vec::new();
        let n = input
            .by_ref()
            .take((MAX_REQUEST + 1) as u64)
            .read_until(b'\n', &mut bytes)
            .map_err(|_| "control_read_failed")?;
        if n == 0 {
            return Ok(());
        }
        if n > MAX_REQUEST || !bytes.ends_with(b"\n") {
            return Err("control_frame_limit".into());
        }
        let req: Request = serde_json::from_slice(&bytes).map_err(|_| "invalid_control_frame")?;
        if req.version != 1 || req.id == 0 {
            return Err("invalid_control_version".into());
        }
        let result = execute(state, req.command);
        let response = match result {
            Ok(v) => Response {
                version: 1,
                id: req.id,
                ok: true,
                result: Some(v),
                error: None,
            },
            Err(e) => Response {
                version: 1,
                id: req.id,
                ok: false,
                result: None,
                error: Some(public_error(&e)),
            },
        };
        let encoded = serde_json::to_vec(&response).map_err(|_| "control_encode_failed")?;
        if encoded.len() > MAX_RESPONSE {
            return Err("control_response_limit".into());
        }
        output
            .write_all(&encoded)
            .and_then(|_| output.write_all(b"\n"))
            .and_then(|_| output.flush())
            .map_err(|_| "control_write_failed")?;
    }
}
pub fn public_error(e: &str) -> String {
    if [
        "profile_required",
        "invalid_engine",
        "invalid_message",
        "message_id_conflict",
        "control_quota",
        "activity_not_found",
        "invalid_command_scope",
        "command_binding_limit",
    ]
    .contains(&e)
    {
        e.into()
    } else if e.contains("revision") {
        "revision_conflict".into()
    } else {
        "control_request_failed".into()
    }
}
