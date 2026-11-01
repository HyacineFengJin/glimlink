// SPDX-License-Identifier: MIT
//! Serial, on-demand engine execution. A claimed job is never silently rerun.
use crate::{app::Runtime, tools};
use runtime_core::Result;
use runtime_engine::{CodexClient, CodexConfig, EngineError, ErrorKind, ToolCall, TurnStatus};
use runtime_jobs::{ClaimedJob, Outcome, Progress, Queue, Status};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use std::{
    fs,
    path::{Path, PathBuf},
    sync::atomic::{AtomicBool, Ordering},
    time::{SystemTime, UNIX_EPOCH},
};

const MAX_RESULT_BYTES: usize = 64 * 1024;
pub fn now() -> Result<u64> {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs())
        .map_err(|_| "invalid agent clock".into())
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Settings {
    #[serde(default)]
    pub engine: EngineKind,
    pub binary: PathBuf,
    #[serde(default)]
    pub model: Option<String>,
    #[serde(default = "turn_timeout")]
    pub turn_timeout_seconds: u64,
    #[serde(default)]
    pub allowed_tools: Option<Vec<String>>,
    /// Explicit startup catalog override. None uses bundled 0.165.0 metadata
    /// with direct function tools, never the separate V8 code-mode host.
    #[serde(default)]
    pub model_catalog: Option<PathBuf>,
}
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum EngineKind {
    #[default]
    Codex,
    Antigravity,
}
fn turn_timeout() -> u64 {
    120
}
impl Default for Settings {
    fn default() -> Self {
        Self {
            engine: EngineKind::Codex,
            binary: crate::deployment::native_binary(EngineKind::Codex),
            model: None,
            turn_timeout_seconds: turn_timeout(),
            allowed_tools: None,
            model_catalog: None,
        }
    }
}
impl Settings {
    /// A queued policy selects its provider without changing another job's settings.
    pub fn for_job(&self, job: &runtime_jobs::Job) -> Result<Self> {
        let Some(profile) = &job.execution_profile else {
            return Ok(self.clone());
        };
        profile.validate()?;
        selected_profile_tools(Some(profile), self.allowed_tools.as_deref())?;
        let engine = if profile.engine == "codex" {
            EngineKind::Codex
        } else {
            EngineKind::Antigravity
        };
        let mut settings = self.clone();
        if settings.engine != engine {
            settings.binary = crate::deployment::native_binary(engine);
        }
        settings.engine = engine;
        settings.model = profile.model.clone();
        if engine == EngineKind::Antigravity {
            settings.model_catalog = None;
        }
        Ok(settings)
    }
    pub fn antigravity_config(&self, state: &Path) -> Result<runtime_engine::AntigravityConfig> {
        selected_tools(self.allowed_tools.as_deref())?;
        if !self.binary.is_absolute()
            || !(1..=600).contains(&self.turn_timeout_seconds)
            || self
                .model
                .as_ref()
                .is_some_and(|m| m.is_empty() || m.len() > 128 || m.chars().any(char::is_control))
        {
            return Err("invalid Antigravity engine settings".into());
        }
        if self.model_catalog.is_some() {
            return Err("model-catalog is a Codex option".into());
        }
        runtime_core::resources::private_dir(state)?;
        let state = state.canonicalize().map_err(|e| e.to_string())?;
        let workspace = state.join("agent-workspace");
        runtime_core::resources::private_dir(&workspace)?;
        let mut config = runtime_engine::AntigravityConfig::new(
            &self.binary,
            state.join("antigravity"),
            workspace,
        );
        config.timeout = std::time::Duration::from_secs(self.turn_timeout_seconds);
        config.model = self.model.clone();
        config.bridge_binary =
            std::env::current_exe().map_err(|_| "cannot locate Runtime bridge executable")?;
        Ok(config)
    }
    pub fn engine_config(&self, state: &Path) -> Result<CodexConfig> {
        selected_tools(self.allowed_tools.as_deref())?;
        if !self.binary.is_absolute()
            || !(1..=600).contains(&self.turn_timeout_seconds)
            || self
                .model
                .as_ref()
                .is_some_and(|m| m.is_empty() || m.len() > 128 || m.chars().any(char::is_control))
        {
            return Err("invalid agent engine settings".into());
        }
        runtime_core::resources::private_dir(state)?;
        let state = state.canonicalize().map_err(|e| e.to_string())?;
        let workspace = state.join("agent-workspace");
        runtime_core::resources::private_dir(&workspace)?;
        let mut config = CodexConfig::new(&self.binary, state.join("codex"), workspace);
        config.timeout = std::time::Duration::from_secs(self.turn_timeout_seconds);
        let catalog = match &self.model_catalog {
            Some(path) if path.is_absolute() => path.clone(),
            Some(_) => return Err("model catalog must be an absolute path".into()),
            None => install_model_catalog(&state)?,
        };
        config.args.pop(); // startup-only options must precede app-server
        config.args.extend([
            "-c".into(),
            format!(
                "model_catalog_json={}",
                serde_json::to_string(&catalog).map_err(|e| e.to_string())?
            ),
            "app-server".into(),
        ]);
        Ok(config)
    }
}

fn install_model_catalog(state: &Path) -> Result<PathBuf> {
    let directory = state.join("engine-config");
    runtime_core::resources::private_dir(&directory)?;
    let path = directory.join("codex-0.165.0-direct-12c95136.json");
    let expected: Value = serde_json::from_slice(runtime_engine::CODEX_DIRECT_MODEL_CATALOG)
        .map_err(|_| "invalid bundled model catalog")?;
    match fs::symlink_metadata(&path) {
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            crate::app::save_private_new(&path, &expected)?;
        }
        Err(_) => return Err("cannot inspect Runtime model catalog".into()),
        Ok(_) => {
            let existing = crate::app::read_bounded(&path, 1024 * 1024)?;
            if serde_json::from_slice::<Value>(&existing).ok().as_ref() != Some(&expected) {
                return Err("Runtime model catalog is damaged or changed; configure an explicit replacement".into());
            }
        }
    }
    Ok(path)
}

#[derive(Debug, Serialize)]
pub struct RunReport {
    pub engine: String,
    pub job_id: Option<String>,
    pub status: String,
    pub tool_calls: u32,
    pub operation_ids: Vec<String>,
    pub thread_id: Option<String>,
    pub turn_id: Option<String>,
    pub output_truncated: bool,
    pub action_evidence: Option<crate::agent_evidence::EvidenceSummary>,
    pub context_version: Option<runtime_jobs::ContextVersion>,
    pub history_persisted: Option<bool>,
    pub context_error: Option<String>,
}
fn report(claim: &ClaimedJob, progress: &Progress, status: &Status, truncated: bool) -> RunReport {
    RunReport {
        engine: "codex".into(),
        job_id: Some(claim.job.id.clone()),
        status: serde_json::to_value(status)
            .ok()
            .and_then(|v| v.as_str().map(str::to_owned))
            .unwrap_or_else(|| "unknown".into()),
        tool_calls: progress.tool_calls,
        operation_ids: progress.operation_ids.clone(),
        thread_id: progress.thread_id.clone(),
        turn_id: progress.turn_id.clone(),
        output_truncated: truncated,
        action_evidence: None,
        context_version: None,
        history_persisted: None,
        context_error: None,
    }
}
pub(crate) fn engine_error(detail: &str) -> EngineError {
    EngineError {
        kind: ErrorKind::State,
        detail: detail.into(),
    }
}
fn bounded(text: &str) -> (&str, bool) {
    if text.len() <= MAX_RESULT_BYTES {
        return (text, false);
    }
    let mut end = MAX_RESULT_BYTES;
    while !text.is_char_boundary(end) {
        end -= 1
    }
    (&text[..end], true)
}
pub(crate) fn finish(
    queue: &Queue,
    claim: &ClaimedJob,
    progress: &Progress,
    status: Status,
    text: Option<&str>,
    error: Option<String>,
) -> Result<RunReport> {
    let (text, truncated) = text
        .map(bounded)
        .map(|(s, t)| (Some(s.to_owned()), t))
        .unwrap_or((None, false));
    queue.finish(
        &claim.lease,
        Outcome {
            status: status.clone(),
            text,
            error: {
                let mut detail = error.unwrap_or_default();
                if truncated {
                    if !detail.is_empty() {
                        detail.push_str("; ")
                    }
                    detail.push_str("Stored model output was truncated to 64 KiB");
                }
                if detail.is_empty() {
                    None
                } else {
                    let mut end = detail.len().min(8192);
                    while !detail.is_char_boundary(end) {
                        end -= 1
                    }
                    Some(detail[..end].into())
                }
            },
        },
        now()?,
    )?;
    Ok(report(claim, progress, &status, truncated))
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn operation_namespaces_are_bounded_and_change_for_a_new_execution() {
        let mut call = ToolCall {
            thread_id: "native-thread-1".into(),
            turn_id: "turn-1".into(),
            call_id: "call-1".into(),
            tool: "social_send".into(),
            arguments: json!({}),
        };
        let job = "j".repeat(96);
        let first = operation_id(&job, &call, 32);
        assert!(first.len() <= 96);
        assert_eq!(first, operation_id(&job, &call, 32));
        call.thread_id = "native-thread-2".into();
        assert_ne!(first, operation_id(&job, &call, 32));
        call.thread_id = "native-thread-1".into();
        call.turn_id = "turn-2".into();
        assert_ne!(first, operation_id(&job, &call, 32));
    }
}

/// Claim at most one job; spawn at most one native engine. The engine is dropped
/// at the end of this call. No credential import or API-key fallback is used.
pub fn run_once(
    runtime: &mut Runtime,
    queue: &Queue,
    config: CodexConfig,
    model: Option<&str>,
) -> Result<RunReport> {
    run_once_with_stop(runtime, queue, config, model, &AtomicBool::new(false))
}
pub fn run_once_with_stop(
    runtime: &mut Runtime,
    queue: &Queue,
    config: CodexConfig,
    model: Option<&str>,
    stop: &AtomicBool,
) -> Result<RunReport> {
    run_once_controlled(runtime, queue, config, model, stop, None)
}
pub fn run_once_controlled(
    runtime: &mut Runtime,
    queue: &Queue,
    config: CodexConfig,
    model: Option<&str>,
    stop: &AtomicBool,
    allowed_tools: Option<&[String]>,
) -> Result<RunReport> {
    run_selected(runtime, queue, config, model, stop, allowed_tools, None)
}
pub fn run_job_controlled(
    runtime: &mut Runtime,
    queue: &Queue,
    config: CodexConfig,
    model: Option<&str>,
    stop: &AtomicBool,
    allowed_tools: Option<&[String]>,
    job_id: &str,
) -> Result<RunReport> {
    run_selected(
        runtime,
        queue,
        config,
        model,
        stop,
        allowed_tools,
        Some(job_id),
    )
}
fn run_selected(
    runtime: &mut Runtime,
    queue: &Queue,
    config: CodexConfig,
    model: Option<&str>,
    stop: &AtomicBool,
    allowed_tools: Option<&[String]>,
    job_id: Option<&str>,
) -> Result<RunReport> {
    let previous_context = runtime.agent_context().clone();
    let previous_job = runtime.mind_job().map(str::to_owned);
    let result = run_once_inner(runtime, queue, config, model, stop, allowed_tools, job_id);
    runtime.set_agent_context(previous_context)?;
    runtime.set_mind_job(previous_job);
    finalize_report(runtime, queue, result?)
}

pub(crate) fn finalize_report(
    runtime: &Runtime,
    queue: &Queue,
    mut report: RunReport,
) -> Result<RunReport> {
    if let Some(id) = &report.job_id {
        let job = queue.get(id)?;
        report.context_version = job.context_version.clone();
        if job.activity.is_some() {
            match crate::agent_activity::ActivityStore::open(runtime.state_dir())?
                .finish(&job, now()?)
            {
                Ok(()) => report.history_persisted = Some(true),
                Err(_) => {
                    report.history_persisted = Some(false);
                    report.context_error=Some("Activity episode could not be saved; original turn/actions remain recorded. No automatic replay.".into());
                }
            }
        } else if job.context.conversation.is_some() && job.progress.thread_id.is_some() {
            match crate::agent_mind::record_job(runtime.state_dir(), &job) {
                Ok(()) => report.history_persisted = Some(true),
                Err(_) => {
                    report.history_persisted = Some(false);
                    report.context_error = Some("Recent dialogue could not be saved; the completed job and action ledger are unchanged. No automatic replay.".into());
                }
            }
        }
    }
    report.action_evidence = Some(crate::agent_evidence::summarize(
        runtime.state_dir(),
        &report.operation_ids,
    )?);
    Ok(report)
}
fn run_once_inner(
    runtime: &mut Runtime,
    queue: &Queue,
    mut config: CodexConfig,
    model: Option<&str>,
    stop: &AtomicBool,
    allowed_tools: Option<&[String]>,
    job_id: Option<&str>,
) -> Result<RunReport> {
    selected_tools(allowed_tools)?;
    if stop.load(Ordering::Relaxed) {
        return Ok(RunReport {
            engine: "codex".into(),
            job_id: None,
            status: "stopped".into(),
            tool_calls: 0,
            operation_ids: vec![],
            thread_id: None,
            turn_id: None,
            output_truncated: false,
            action_evidence: None,
            context_version: None,
            history_persisted: None,
            context_error: None,
        });
    }
    let claimed = match job_id {
        Some(id) => queue.claim_job(id, now()?)?,
        None => queue.claim_next(now()?)?,
    };
    let Some(claim) = claimed else {
        return Ok(RunReport {
            engine: "codex".into(),
            job_id: None,
            status: "idle".into(),
            tool_calls: 0,
            operation_ids: vec![],
            thread_id: None,
            turn_id: None,
            output_truncated: false,
            action_evidence: None,
            context_version: None,
            history_persisted: None,
            context_error: None,
        });
    };
    let mut progress = Progress::default();
    if claim
        .job
        .execution_profile
        .as_ref()
        .is_some_and(|p| p.engine != "codex")
    {
        return finish(
            queue,
            &claim,
            &progress,
            Status::Failed,
            None,
            Some("Job profile selects another engine; no model turn started".into()),
        );
    }
    let specs = selected_profile_tools(claim.job.execution_profile.as_ref(), allowed_tools)?;
    let model = match claim.job.execution_profile.as_ref() {
        Some(profile) => profile.model.as_deref(),
        None => model,
    };
    runtime.set_agent_context(claim.job.context.clone())?;
    runtime.set_mind_job(Some(claim.job.id.clone()));
    let prepared = match crate::agent_mind::assemble_job(runtime.state_dir(), runtime.mind_state(), &claim.job) {
        Ok(prepared) => prepared,
        Err(_) => return finish(queue,&claim,&progress,Status::Failed,None,Some("Shared identity/context is unavailable or exceeds its budget; no model turn started".into())),
    };
    config.developer_instructions = Some(prepared.developer_instructions);
    let mut client = match CodexClient::spawn(config) {
        Ok(client) => client,
        Err(error) => {
            return finish(
                queue,
                &claim,
                &progress,
                Status::Failed,
                None,
                Some(error.to_string()),
            );
        }
    };
    if let Err(error) = client.initialize() {
        return finish(
            queue,
            &claim,
            &progress,
            Status::Failed,
            None,
            Some(error.to_string()),
        );
    }
    let account = match client.account_read() {
        Ok(account) => account,
        Err(error) => {
            return finish(
                queue,
                &claim,
                &progress,
                Status::Failed,
                None,
                Some(error.to_string()),
            );
        }
    };
    if stop.load(Ordering::Relaxed) {
        return interrupted(
            queue,
            &claim,
            &progress,
            "Agent stopped before model execution",
        );
    }
    if !account.authenticated || account.auth_kind.as_deref() != Some("chatgpt") {
        return finish(queue,&claim,&progress,Status::NeedsAuth,None,Some("A ChatGPT subscription login in the dedicated CODEX_HOME is required; no model turn was started".into()));
    }
    queue.record_context(&claim.lease, prepared.version, now()?)?;
    queue.record_engine(&claim.lease, "codex", now()?)?;
    let thread = match client.thread_start_with_tools(model, &specs) {
        Ok(thread) => thread,
        Err(error) => {
            return finish(
                queue,
                &claim,
                &progress,
                Status::Failed,
                None,
                Some(error.to_string()),
            );
        }
    };
    progress.thread_id = Some(thread.id.clone());
    queue.update_progress(&claim.lease, progress.clone(), now()?)?;
    if stop.load(Ordering::Relaxed) {
        return interrupted(
            queue,
            &claim,
            &progress,
            "Agent stopped before model execution",
        );
    }
    let inputs = prepared.inputs;
    let mut persistence_error = None::<String>;
    let output = client.turn_start_with_tools(
        &thread.id,
        &inputs,
        |_| {},
        |call| {
            dispatch_tool(
                runtime,
                queue,
                &claim,
                &mut progress,
                &mut persistence_error,
                stop,
                &call,
            )
        },
    );
    match output {
        Ok(output) => {
            progress.turn_id = Some(output.turn_id.clone());
            queue.update_progress(&claim.lease, progress.clone(), now()?)?;
            if stop.load(Ordering::Relaxed) || persistence_error.is_some() {
                return interrupted(
                    queue,
                    &claim,
                    &progress,
                    persistence_error
                        .as_deref()
                        .unwrap_or("Agent stop requested during model execution"),
                );
            }
            match output.status {
                TurnStatus::Completed => finish(
                    queue,
                    &claim,
                    &progress,
                    Status::Completed,
                    Some(&output.text),
                    None,
                ),
                TurnStatus::Failed => finish(
                    queue,
                    &claim,
                    &progress,
                    Status::Failed,
                    Some(&output.text),
                    Some("Model turn failed; this job will not be rerun automatically".into()),
                ),
                TurnStatus::Interrupted => {
                    interrupted(queue, &claim, &progress, "Model turn was interrupted")
                }
            }
        }
        Err(error) => interrupted(
            queue,
            &claim,
            &progress,
            persistence_error.as_deref().unwrap_or(&error.to_string()),
        ),
    }
}
/// All engines persist operation identity through this one callback before dispatch.
pub(crate) fn dispatch_tool(
    runtime: &mut Runtime,
    queue: &Queue,
    claim: &ClaimedJob,
    progress: &mut Progress,
    persistence_error: &mut Option<String>,
    stop: &AtomicBool,
    call: &ToolCall,
) -> runtime_engine::Result<runtime_engine::ToolResult> {
    if progress.tool_calls >= 32 {
        return Err(engine_error("Agent tool-call budget exhausted"));
    }
    if persistence_error.is_some() {
        return Err(engine_error(
            "Agent persistence failed; no further tools will execute",
        ));
    }
    if stop.load(Ordering::Relaxed) {
        return Err(engine_error(
            "Agent stop requested; no further tool was executed",
        ));
    }
    let mut call = call.clone();
    if let Some(profile) = &claim.job.execution_profile {
        if profile
            .allowed_tools
            .as_ref()
            .is_some_and(|allowed| !allowed.contains(&call.tool))
        {
            return Err(engine_error("Tool is outside the frozen agent allowlist"));
        }
        crate::agent_profiles::apply_default_account(profile, &call.tool, &mut call.arguments);
    }
    progress.tool_calls = progress
        .tool_calls
        .checked_add(1)
        .ok_or_else(|| engine_error("agent tool counter exhausted"))?;
    progress.turn_id = Some(call.turn_id.clone());
    let mut interaction_denied = None;
    if is_write(&call) {
        if !call.arguments.is_object() {
            return Err(engine_error("write arguments must be an object"));
        }
        if call.arguments.get("operation_id").is_none() {
            call.arguments["operation_id"] =
                json!(operation_id(&claim.job.id, &call, progress.tool_calls));
        }
        if claim.job.activity.is_some() {
            interaction_denied = crate::agent_activity::ActivityStore::open(runtime.state_dir())
                .and_then(|store| {
                    store.reserve_interaction(
                        runtime.state_dir(),
                        &claim.job,
                        &call.tool,
                        &call.arguments,
                    )
                })
                .err();
        }
        if let Some(id) = call.arguments["operation_id"]
            .as_str()
            .filter(|_| interaction_denied.is_none())
        {
            if !progress.operation_ids.iter().any(|v| v == id) {
                progress.operation_ids.push(id.into());
            }
        }
    }
    if let Err(error) = queue.update_progress(
        &claim.lease,
        progress.clone(),
        now().map_err(|_| engine_error("agent clock unavailable"))?,
    ) {
        *persistence_error = Some(error);
        return Err(engine_error(
            "Cannot durably record tool identity; the tool was not executed",
        ));
    }
    if stop.load(Ordering::Relaxed) {
        return Err(engine_error(
            "Agent stopped after recording tool identity; the tool was not executed",
        ));
    }
    if let Some(detail) = interaction_denied {
        return Ok(runtime_engine::ToolResult::text(
            json!({"status":"not_executed","detail":detail}).to_string(),
            false,
        ));
    }
    Ok(runtime.dispatch_engine_tool(&call))
}

/// Shared validated tool selection for the durable host and one-shot engine CLI.
pub fn selected_tools(allowed: Option<&[String]>) -> Result<Vec<runtime_engine::DynamicToolSpec>> {
    let specs = tools::specs();
    let Some(allowed) = allowed else {
        return Ok(specs);
    };
    let names = allowed.iter().collect::<std::collections::BTreeSet<_>>();
    if names.len() != allowed.len()
        || allowed.len() > 32
        || names
            .iter()
            .any(|name| !specs.iter().any(|spec| &spec.name == *name))
    {
        return Err("agent allowed-tools must be distinct registered Runtime tools".into());
    }
    Ok(specs
        .into_iter()
        .filter(|spec| names.contains(&spec.name))
        .collect())
}
pub(crate) fn selected_profile_tools(
    profile: Option<&runtime_jobs::ExecutionProfile>,
    allowed: Option<&[String]>,
) -> Result<Vec<runtime_engine::DynamicToolSpec>> {
    let mut specs = selected_tools(allowed)?;
    if let Some(profile) = profile {
        profile.validate()?;
        selected_tools(profile.allowed_tools.as_deref())?;
        if let Some(names) = &profile.allowed_tools {
            specs.retain(|spec| names.contains(&spec.name));
        }
    }
    Ok(specs)
}
fn is_write(call: &ToolCall) -> bool {
    crate::tools::is_write(&call.tool, &call.arguments)
}
fn operation_id(job: &str, call: &ToolCall, ordinal: u32) -> String {
    let mut a = 0xcbf29ce484222325_u64;
    let mut b = 0x84222325cbf29ce4_u64;
    for part in [
        "agent-job-operation-v1",
        job,
        &call.thread_id,
        &call.turn_id,
    ] {
        for byte in (part.len() as u64)
            .to_le_bytes()
            .into_iter()
            .chain(part.bytes())
        {
            a = (a ^ u64::from(byte)).wrapping_mul(0x100000001b3);
            b = (b ^ u64::from(byte)).wrapping_mul(0x100000001b3);
        }
    }
    format!("job-{a:016x}{b:016x}-{ordinal}")
}
pub(crate) fn interrupted(
    queue: &Queue,
    claim: &ClaimedJob,
    progress: &Progress,
    error: &str,
) -> Result<RunReport> {
    // Recovery requires the daemon's exclusive Runtime lock. Do not requeue the
    // model: a dispatched platform action may already have been accepted.
    finish(
        queue,
        claim,
        progress,
        Status::Interrupted,
        None,
        Some(error.chars().take(4096).collect()),
    )
}

/// A small status snapshot is independent of the Runtime lock, so management
/// readers remain usable while a model turn owns the client kernel.
pub fn write_status(state: &Path, value: &Value) -> Result<()> {
    runtime_core::resources::private_dir(state)?;
    let bytes = serde_json::to_vec(value).map_err(|e| e.to_string())?;
    if bytes.len() > 64 * 1024 {
        return Err("agent status exceeds limit".into());
    }
    let path = state.join("agent-status.json");
    let nonce = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_err(|_| "invalid status clock")?
        .as_nanos();
    let temporary = state.join(format!(".agent-status-{}-{nonce}.next", std::process::id()));
    let saved = (|| -> Result<()> {
        crate::app::save_private_new(&temporary, value)?;
        fs::rename(&temporary, &path).map_err(|e| e.to_string())?;
        fs::File::open(state)
            .and_then(|f| f.sync_all())
            .map_err(|e| e.to_string())
    })();
    if saved.is_err() {
        let _ = fs::remove_file(temporary);
    }
    saved
}
