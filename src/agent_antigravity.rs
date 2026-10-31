// SPDX-License-Identifier: MIT
//! On-demand Antigravity ACP jobs, using the same Runtime, memory and action ledger.
use crate::{agent_host, app::Runtime, engine_bridge::Bridge, mcp::Session};
use runtime_core::{Result, resources::private_dir};
use runtime_engine::{AntigravityClient, AntigravityConfig, ErrorKind, ToolResult, TurnStatus};
use runtime_jobs::{ClaimedJob, Progress, Queue, Status};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};

static NEXT_SOCKET: AtomicU64 = AtomicU64::new(0);

/// Cached-login probe; no model prompt or authentication flow is started.
pub fn probe(mut config: AntigravityConfig) -> Result<serde_json::Value> {
    let parent = config
        .workspace
        .parent()
        .ok_or("workspace parent is missing")?
        .join("engine-bridge");
    private_dir(&parent)?;
    let socket = parent.join(format!(
        "probe-{}-{}.sock",
        std::process::id(),
        NEXT_SOCKET.fetch_add(1, Ordering::Relaxed)
    ));
    config.bridge_socket = socket.clone();
    let mut bridge = Bridge::bind(&socket)?;
    let mut mcp = Session::new_with_tools(Vec::new())?;
    let mut client = AntigravityClient::spawn(config).map_err(|e| e.to_string())?;
    let server = client.initialize().map_err(|e| e.to_string())?;
    let session = client.new_session(&mut || {
        bridge
            .poll(&mut mcp, &mut |_| {
                ToolResult::text("No business tools in a login probe", false)
            })
            .map_err(|_| agent_host::engine_error("Login probe MCP bridge failed"))?;
        Ok(())
    });
    match session {
        Ok(_) => Ok(
            serde_json::json!({"engine":"antigravity","server":server,"account":{"authenticated":true,"login_route":"dedicated_native_google_cache"},"model_turn_started":false,"login_started":false}),
        ),
        Err(error) if error.kind == ErrorKind::Authentication => Ok(
            serde_json::json!({"engine":"antigravity","server":server,"account":{"authenticated":false},"model_turn_started":false,"login_started":false}),
        ),
        Err(error) => Err(error.to_string()),
    }
}

pub fn run_once_controlled(
    runtime: &mut Runtime,
    queue: &Queue,
    config: AntigravityConfig,
    stop: &AtomicBool,
    allowed_tools: Option<&[String]>,
) -> Result<agent_host::RunReport> {
    run_selected(runtime, queue, config, stop, allowed_tools, None)
}

pub fn run_job_controlled(
    runtime: &mut Runtime,
    queue: &Queue,
    config: AntigravityConfig,
    stop: &AtomicBool,
    allowed_tools: Option<&[String]>,
    job_id: &str,
) -> Result<agent_host::RunReport> {
    run_selected(runtime, queue, config, stop, allowed_tools, Some(job_id))
}

fn run_selected(
    runtime: &mut Runtime,
    queue: &Queue,
    config: AntigravityConfig,
    stop: &AtomicBool,
    allowed_tools: Option<&[String]>,
    job_id: Option<&str>,
) -> Result<agent_host::RunReport> {
    let old_context = runtime.agent_context().clone();
    let old_job = runtime.mind_job().map(str::to_owned);
    let result = run_inner(runtime, queue, config, stop, allowed_tools, job_id);
    runtime.set_agent_context(old_context)?;
    runtime.set_mind_job(old_job);
    let mut report = result?;
    report.engine = "antigravity".into();
    agent_host::finalize_report(runtime, queue, report)
}

fn empty(status: &str) -> agent_host::RunReport {
    agent_host::RunReport {
        engine: "antigravity".into(),
        job_id: None,
        status: status.into(),
        tool_calls: 0,
        operation_ids: Vec::new(),
        thread_id: None,
        turn_id: None,
        output_truncated: false,
        action_evidence: None,
        context_version: None,
        history_persisted: None,
        context_error: None,
    }
}

fn run_inner(
    runtime: &mut Runtime,
    queue: &Queue,
    mut config: AntigravityConfig,
    stop: &AtomicBool,
    allowed_tools: Option<&[String]>,
    job_id: Option<&str>,
) -> Result<agent_host::RunReport> {
    agent_host::selected_tools(allowed_tools)?;
    if stop.load(Ordering::Relaxed) {
        return Ok(empty("stopped"));
    }
    let claimed = match job_id {
        Some(id) => queue.claim_job(id, agent_host::now()?)?,
        None => queue.claim_next(agent_host::now()?)?,
    };
    let Some(claim) = claimed else {
        return Ok(empty("idle"));
    };
    let mut progress = Progress::default();
    if claim
        .job
        .execution_profile
        .as_ref()
        .is_some_and(|p| p.engine != "antigravity")
    {
        return agent_host::finish(
            queue,
            &claim,
            &progress,
            Status::Failed,
            None,
            Some("Job profile selects another engine; no model turn started".into()),
        );
    }
    let specs =
        agent_host::selected_profile_tools(claim.job.execution_profile.as_ref(), allowed_tools)?;
    if let Some(profile) = &claim.job.execution_profile {
        config.model = profile.model.clone();
    }
    runtime.set_agent_context(claim.job.context.clone())?;
    runtime.set_mind_job(Some(claim.job.id.clone()));
    let prepared = match crate::agent_mind::assemble_with_store(
        runtime.state_dir(), runtime.mind_state(), &claim.job.context,
        &claim.job.prompt, claim.job.event.as_ref(), Some(&claim.job.id),
    ) {
        Ok(value) => value,
        Err(_) => return agent_host::finish(queue, &claim, &progress, Status::Failed, None,
            Some("Shared identity/context is unavailable or exceeds its budget; no model turn started".into())),
    };
    config.developer_instructions = Some(prepared.developer_instructions);
    let directory = runtime.state_dir().join("engine-bridge");
    private_dir(&directory)?;
    let socket = directory.join(format!(
        "a-{}-{}.sock",
        std::process::id(),
        NEXT_SOCKET.fetch_add(1, Ordering::Relaxed)
    ));
    config.bridge_socket = socket.clone();
    let mut bridge = match Bridge::bind(&socket) {
        Ok(value) => value,
        Err(_) => {
            return agent_host::finish(
                queue,
                &claim,
                &progress,
                Status::Failed,
                None,
                Some("Cannot create private Runtime MCP bridge; no model turn started".into()),
            );
        }
    };
    let mut mcp = Session::new_with_tools(specs)?;
    let mut client = match AntigravityClient::spawn(config) {
        Ok(value) => value,
        Err(error) => {
            return agent_host::finish(
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
        return agent_host::finish(
            queue,
            &claim,
            &progress,
            Status::Failed,
            None,
            Some(error.to_string()),
        );
    }
    let mut persistence_error = None;
    let session = client.new_session(&mut || {
        pump(
            runtime,
            queue,
            &claim,
            &mut progress,
            &mut persistence_error,
            stop,
            &mut bridge,
            &mut mcp,
        )
    });
    let session = match session {
        Ok(session) => session,
        Err(error) if error.kind == ErrorKind::Authentication && persistence_error.is_none() => {
            return agent_host::finish(queue, &claim, &progress, Status::NeedsAuth, None,
                Some("Google subscription login in the dedicated Antigravity HOME is required; no model turn started. Use the separate login command.".into()));
        }
        Err(error) => {
            return agent_host::finish(
                queue,
                &claim,
                &progress,
                Status::Failed,
                None,
                Some(persistence_error.unwrap_or_else(|| error.to_string())),
            );
        }
    };
    progress.thread_id = Some(session.id.clone());
    progress.turn_id = Some(client.next_turn_id());
    queue.update_progress(&claim.lease, progress.clone(), agent_host::now()?)?;
    queue.record_context(&claim.lease, prepared.version, agent_host::now()?)?;
    queue.record_engine(&claim.lease, "antigravity", agent_host::now()?)?;
    if stop.load(Ordering::Relaxed) {
        return agent_host::interrupted(
            queue,
            &claim,
            &progress,
            "Agent stopped before model execution",
        );
    }
    let output = client.prompt(&session.id, &prepared.inputs, |_| {}, &mut || {
        pump(
            runtime,
            queue,
            &claim,
            &mut progress,
            &mut persistence_error,
            stop,
            &mut bridge,
            &mut mcp,
        )
    });
    match output {
        Ok(output) => {
            progress.turn_id = Some(output.turn_id);
            queue.update_progress(&claim.lease, progress.clone(), agent_host::now()?)?;
            if stop.load(Ordering::Relaxed) || persistence_error.is_some() {
                return agent_host::interrupted(
                    queue,
                    &claim,
                    &progress,
                    persistence_error
                        .as_deref()
                        .unwrap_or("Agent stopped during model execution"),
                );
            }
            match output.status {
                TurnStatus::Completed => agent_host::finish(
                    queue,
                    &claim,
                    &progress,
                    Status::Completed,
                    Some(&output.text),
                    None,
                ),
                TurnStatus::Failed => agent_host::finish(
                    queue,
                    &claim,
                    &progress,
                    Status::Failed,
                    Some(&output.text),
                    Some(
                        "Antigravity model turn failed; this job will not be rerun automatically"
                            .into(),
                    ),
                ),
                TurnStatus::Interrupted => agent_host::interrupted(
                    queue,
                    &claim,
                    &progress,
                    "Antigravity model turn was interrupted",
                ),
            }
        }
        Err(error) => agent_host::interrupted(
            queue,
            &claim,
            &progress,
            persistence_error.as_deref().unwrap_or(&error.to_string()),
        ),
    }
}

#[allow(clippy::too_many_arguments)]
fn pump(
    runtime: &mut Runtime,
    queue: &Queue,
    claim: &ClaimedJob,
    progress: &mut Progress,
    fatal: &mut Option<String>,
    stop: &AtomicBool,
    bridge: &mut Bridge,
    mcp: &mut Session,
) -> runtime_engine::Result<()> {
    if stop.load(Ordering::Relaxed) || fatal.is_some() {
        return Err(agent_host::engine_error(
            "Agent stopped or durable tool progress failed",
        ));
    }
    bridge.poll(mcp, &mut |call| {
        let Some(thread) = &progress.thread_id else {
            *fatal = Some("Business tool requested before native session identity was saved; not executed".into());
            return ToolResult::text("Runtime session is not ready; tool not executed", false);
        };
        let mut bound = call.clone();
        bound.thread_id = thread.clone();
        bound.turn_id = progress.turn_id.clone().unwrap_or_default();
        match agent_host::dispatch_tool(runtime, queue, claim, progress, fatal, stop, &bound) {
            Ok(value) => value,
            Err(error) => {
                *fatal = Some(error.to_string());
                ToolResult::text("Runtime dispatch stopped; tool not executed. Do not replay the action.", false)
            }
        }
    }).map_err(|_| agent_host::engine_error("Runtime MCP bridge failed"))?;
    if fatal.is_some() {
        return Err(agent_host::engine_error("Runtime dispatch stopped"));
    }
    Ok(())
}
