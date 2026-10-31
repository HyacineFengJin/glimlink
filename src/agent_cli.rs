// SPDX-License-Identifier: MIT
//! Operator controls. They never become model tools or create implicit watches.
use crate::{
    agent_host::{self, Settings},
    agent_sources::{SourceStore, WatchConfig},
    app::{Runtime, read_bounded},
};
use runtime_core::{
    Result,
    resources::{open_regular, private_dir},
};
use runtime_jobs::{Job, Queue, Status};
use serde_json::{Value, json};
use std::{
    collections::BTreeSet,
    io::Write,
    os::unix::fs::PermissionsExt,
    path::Path,
    sync::atomic::{AtomicBool, Ordering},
    time::{Duration, Instant},
};

static STOP: AtomicBool = AtomicBool::new(false);
extern "C" fn stop_signal(_: libc::c_int) {
    STOP.store(true, Ordering::Relaxed);
}
struct Signals {
    old_int: libc::sigaction,
    old_term: libc::sigaction,
}
impl Signals {
    fn install() -> Result<Self> {
        let mut action: libc::sigaction = unsafe { std::mem::zeroed() };
        action.sa_sigaction = stop_signal as usize;
        let mut old_int = unsafe { std::mem::zeroed() };
        let mut old_term = unsafe { std::mem::zeroed() };
        unsafe {
            libc::sigemptyset(&mut action.sa_mask);
            if libc::sigaction(libc::SIGINT, &action, &mut old_int) != 0 {
                return Err("cannot install agent stop handler".into());
            }
            if libc::sigaction(libc::SIGTERM, &action, &mut old_term) != 0 {
                libc::sigaction(libc::SIGINT, &old_int, std::ptr::null_mut());
                return Err("cannot install agent stop handler".into());
            }
        }
        STOP.store(false, Ordering::Relaxed);
        Ok(Self { old_int, old_term })
    }
}
impl Drop for Signals {
    fn drop(&mut self) {
        unsafe {
            libc::sigaction(libc::SIGINT, &self.old_int, std::ptr::null_mut());
            libc::sigaction(libc::SIGTERM, &self.old_term, std::ptr::null_mut());
        }
    }
}

fn private_input(path: &Path, max: u64) -> Result<Vec<u8>> {
    let file = open_regular(path, max)?;
    if file
        .metadata()
        .map_err(|e| e.to_string())?
        .permissions()
        .mode()
        & 0o077
        != 0
    {
        return Err("agent management input must be private (0600)".into());
    }
    read_bounded(path, max)
}
fn project(state: &Path, job: &Job) -> Value {
    let evidence = match crate::agent_evidence::summarize(state, &job.progress.operation_ids) {
        Ok(evidence) => {
            json!({"counts":evidence.counts,"needs_attention":evidence.needs_attention,"readonly":true})
        }
        Err(_) => json!({"status":"unavailable","needs_attention":true,"readonly":true}),
    };
    json!({"id":job.id,"status":job.status,"engine":job.engine,"execution_profile":job.execution_profile,"context":job.context,"context_version":job.context_version,"created_at":job.created_at,"updated_at":job.updated_at,"started_at":job.started_at,"finished_at":job.finished_at,"progress":{"thread_id":job.progress.thread_id,"turn_id":job.progress.turn_id,"tool_calls":job.progress.tool_calls,"operation_ids":job.progress.operation_ids.iter().take(8).collect::<Vec<_>>(),"operation_count":job.progress.operation_ids.len(),"operation_ids_truncated":job.progress.operation_ids.len()>8},"action_evidence":evidence,"error":job.error.as_ref().map(|v|v.chars().take(512).collect::<String>())})
}

/// Model-facing detail deliberately excludes the operator task, observed
/// platform body and stored model response. All action statuses arrive in one
/// read instead of requiring a model to reconcile each operation separately.
pub fn job_status(state: &Path, id: &str) -> Result<Value> {
    let job = Queue::open(state)?.get(id)?;
    let evidence = crate::agent_evidence::summarize(state, &job.progress.operation_ids)?;
    Ok(
        json!({"job":project(state,&job),"action_evidence":evidence,"network_requests":0,"automatic_replay":false}),
    )
}

pub fn summary(state: &Path) -> Result<Value> {
    let jobs = Queue::open(state)?.list(runtime_jobs::MAX_JOBS)?;
    let mut counts = serde_json::Map::new();
    for job in &jobs {
        let name = serde_json::to_value(job.status)
            .map_err(|e| e.to_string())?
            .as_str()
            .ok_or("invalid job status")?
            .to_owned();
        let n = counts.get(&name).and_then(Value::as_u64).unwrap_or(0);
        counts.insert(name, json!(n + 1));
    }
    let latest = if state.join("agent-status.json").exists() {
        Some(
            serde_json::from_slice::<Value>(&private_input(
                &state.join("agent-status.json"),
                65536,
            )?)
            .map_err(|_| "invalid agent status snapshot")?,
        )
    } else {
        None
    };
    let watches = match SourceStore::open(state).and_then(|store| store.list()) {
        Ok(watches) => {
            json!({"status":"available","items":watches.iter().map(|w|json!({"id":w.config.id,"source":w.config.source,"account":w.config.account,"enabled":w.config.enabled,"initialized":w.initialized,"pending_events":w.pending_events,"next_poll_at":w.next_poll_at,"halted":w.halted,"status":w.last_status,"error":w.last_error,"head_seeded":w.head_seeded,"head_anchor_count":w.head_anchor_count,"catching_up":w.catching_up,"catch_up_pages":w.catch_up_pages,"deferred_events":w.deferred_events,"ready_events":w.ready_events})).collect::<Vec<_>>() })
        }
        Err(error) => json!({"status":"busy_or_unavailable","error":error}),
    };
    Ok(
        json!({"status":"ok","jobs":{"total":jobs.len(),"limit":runtime_jobs::MAX_JOBS,"counts":counts,"items":jobs.iter().take(20).map(|job|project(state,job)).collect::<Vec<_>>()},"sources":watches,"last_host_snapshot":latest,"model_process_policy":"one_at_a_time_on_demand","interrupted_jobs_auto_restarted":false,"network_requests":0}),
    )
}

/// Commands that only manage queue/state do not acquire Runtime's global lock;
/// they remain usable while the daemon holds the client kernel.
pub fn command(state: &Path, account: &str, args: &[String]) -> Result<Value> {
    command_with_context(state, account, args, &runtime_jobs::AgentContext::default())
}
pub fn command_with_context(
    state: &Path,
    account: &str,
    args: &[String],
    context: &runtime_jobs::AgentContext,
) -> Result<Value> {
    command_with_environment(state, account, args, context, None)
}
pub fn command_with_environment(
    state: &Path,
    account: &str,
    args: &[String],
    context: &runtime_jobs::AgentContext,
    mind_state: Option<&Path>,
) -> Result<Value> {
    context.validate()?;
    private_dir(state)?;
    let action = args.first().map(String::as_str).unwrap_or("status");
    match action {
        "status" if args.len()<=1=>summary(state),
        "jobs" if args.len()==1=>Ok(json!({"jobs":Queue::open(state)?.list(runtime_jobs::MAX_JOBS)?.iter().map(|job|project(state,job)).collect::<Vec<_>>()})),
        "job" if args.len()==2=>{
            let job=Queue::open(state)?.get(&args[1])?;
            let evidence=crate::agent_evidence::summarize(state,&job.progress.operation_ids)?;
            Ok(json!({"job":job,"action_evidence":evidence}))
        }
        "enqueue" if args.len()==3=>{
            let prompt=String::from_utf8(private_input(Path::new(&args[2]),runtime_jobs::MAX_PROMPT_BYTES as u64)?).map_err(|_|"agent task must be UTF-8")?;
            let profile=crate::agent_profiles::ProfileStore::open(state)?.get(&context.agent)?;
            let job=Queue::open(state)?.enqueue_configured(&args[1],&prompt,None,context.clone(),profile,agent_host::now()?)?;Ok(json!({"job":project(state,&job)}))
        }
        "profiles" if args.len()==1=>Ok(json!({"profiles":crate::agent_profiles::ProfileStore::open(state)?.list()})),
        "profile" if args.len()==2=>Ok(json!({"agent":args[1],"profile":crate::agent_profiles::ProfileStore::open(state)?.get(&args[1])?})),
        "profile-set" if matches!(args.len(),3|4)=>{
            let config:crate::agent_profiles::ProfileInput=serde_json::from_slice(&private_input(Path::new(&args[2]),65536)?).map_err(|_|"invalid profile config")?;
            let expected=args.get(3).map(|v|v.parse::<u64>().map_err(|_|"invalid expected profile revision")).transpose()?;
            let profile=crate::agent_profiles::ProfileStore::open(state)?.set(&args[1],config,expected)?;
            Ok(json!({"agent":args[1],"profile":profile,"queued_jobs_unchanged":true}))
        }
        "watch" if args.len()==2=>{
            let config:WatchConfig=serde_json::from_slice(&private_input(Path::new(&args[1]),65536)?).map_err(|_|"invalid watch config")?;
            Ok(json!({"watch":SourceStore::open(state)?.configure(config)?}))
        }
        "watches" if args.len()==1=>Ok(json!({"watches":SourceStore::open(state)?.list()?})),
        "watch-reset" if args.len()==2=>Ok(json!({"watch":SourceStore::open(state)?.reset(&args[1])?})),
        "watch-resume" if args.len()==2=>Ok(json!({"watch":SourceStore::open(state)?.resume_polling(&args[1])?,"cursor_and_dedup_preserved":true,"login_started":false})),
        "watch-enable"|"watch-disable" if args.len()==2=>{
            let mut store=SourceStore::open(state)?;let mut config=store.status(&args[1])?.config;config.enabled=action=="watch-enable";
            Ok(json!({"watch":store.configure(config)?}))
        }
        "watch-abandon" if args.len()==2=>{
            let mut store=SourceStore::open(state)?;let before=store.pending_job_ids()?.into_iter().collect::<BTreeSet<_>>();
            let watch=store.abandon_pending(&args[1])?;let after=store.pending_job_ids()?.into_iter().collect::<BTreeSet<_>>();
            Ok(json!({"watch":watch,"discarded_source_job_ids":before.difference(&after).collect::<Vec<_>>(),"queued_jobs_unchanged":true,"detail":"Already queued jobs retain their own lifecycle; cancel unexecuted job IDs separately when needed."}))
        }
        "resume" if args.len()>=2=>Ok(json!({"resumed":Queue::open(state)?.resume_needs_auth(&args[1..],agent_host::now()?)?,"only_pre_model_auth_failures":true})),
        "cancel" if args.len()>=2=>Ok(json!({"canceled":Queue::open(state)?.cancel_pending(&args[1..],agent_host::now()?)?,"only_unexecuted_jobs":true})),
        "gc" if args.len()>=2=>{
            let store=SourceStore::open(state)?;let protected=store.pending_job_ids()?.into_iter().collect::<BTreeSet<_>>();
            if args[1..].iter().any(|id|protected.contains(id)){return Err("job belongs to an uncommitted source batch and cannot be removed".into())}
            Ok(json!({"removed":Queue::open(state)?.gc_finished(&args[1..])?}))
        }
        "run"=>{
            if context!=&runtime_jobs::AgentContext::default(){return Err("agent run uses the identity fixed at enqueue/watch; select persona/agent/conversation when creating tasks, not on the daemon".into())}
            run(state,account,&args[1..],mind_state)
        },
        _=>Err("agent: status | jobs | job ID | enqueue ID PROMPT_FILE | profiles | profile AGENT | profile-set AGENT CONFIG_FILE [EXPECTED_REVISION] | watch CONFIG_FILE | watches | watch-enable ID | watch-disable ID | watch-abandon ID | watch-reset ID | watch-resume ID | resume ID... | cancel ID... | gc ID... | run [--engine codex|antigravity] [--once|--cycles N] [--poll-ms N] [--binary PATH] [--codex-binary PATH] [--antigravity-binary PATH] [--model NAME] [--model-catalog PATH] [--tools NAME,...] [--timeout-seconds N] [--retain-finished N]".into()),
    }
}

fn number(value: Option<&String>, name: &str) -> Result<u64> {
    value
        .ok_or_else(|| format!("missing {name}"))?
        .parse()
        .map_err(|_| format!("invalid {name}"))
}
fn run(state: &Path, account: &str, args: &[String], mind_state: Option<&Path>) -> Result<Value> {
    let mut settings = Settings::default();
    let mut binary_selected = false;
    let mut provider_binaries = std::collections::BTreeMap::<String, std::path::PathBuf>::new();
    let mut cycles = None;
    let mut poll_ms = 1000;
    let mut retain = 64;
    let mut index = 0;
    while index < args.len() {
        match args[index].as_str() {
            "--engine" => {
                index += 1;
                settings.engine = match args.get(index).map(String::as_str) {
                    Some("codex") => agent_host::EngineKind::Codex,
                    Some("antigravity") => agent_host::EngineKind::Antigravity,
                    _ => return Err("engine must be codex or antigravity".into()),
                };
            }
            "--once" => cycles = Some(1),
            "--cycles" => {
                index += 1;
                cycles = Some(number(args.get(index), "cycles")?);
            }
            "--poll-ms" => {
                index += 1;
                poll_ms = number(args.get(index), "poll-ms")?;
            }
            "--retain-finished" => {
                index += 1;
                retain = number(args.get(index), "retain-finished")?;
            }
            "--timeout-seconds" => {
                index += 1;
                settings.turn_timeout_seconds = number(args.get(index), "timeout-seconds")?;
            }
            "--binary" => {
                index += 1;
                settings.binary = args.get(index).ok_or("missing binary")?.into();
                binary_selected = true;
            }
            flag @ ("--codex-binary" | "--antigravity-binary") => {
                let engine = if flag == "--codex-binary" {
                    "codex"
                } else {
                    "antigravity"
                };
                index += 1;
                let path =
                    std::path::PathBuf::from(args.get(index).ok_or("missing provider binary")?);
                if !path.is_absolute() {
                    return Err("provider binary must be an absolute path".into());
                }
                provider_binaries.insert(engine.into(), path);
            }
            "--model" => {
                index += 1;
                settings.model = Some(args.get(index).ok_or("missing model")?.clone());
            }
            "--model-catalog" => {
                index += 1;
                settings.model_catalog =
                    Some(args.get(index).ok_or("missing model catalog")?.into());
            }
            "--tools" => {
                index += 1;
                settings.allowed_tools = Some(
                    args.get(index)
                        .ok_or("missing tools list")?
                        .split(',')
                        .filter(|v| !v.is_empty())
                        .map(str::to_owned)
                        .collect(),
                );
            }
            _ => return Err("unknown agent run option".into()),
        }
        index += 1;
    }
    if settings.engine == agent_host::EngineKind::Antigravity && !binary_selected {
        settings.binary = "/cache/bin/antigravity-native".into();
    }
    if cycles.is_some_and(|n| n == 0 || n > 1_000_000)
        || !(250..=60000).contains(&poll_ms)
        || retain > 96
    {
        return Err("invalid agent run limits".into());
    }
    let _signals = Signals::install()?;
    let mut runtime = Runtime::open(state, account, None)?;
    if let Some(mind_state) = mind_state {
        runtime.set_mind_state(mind_state)?;
    }
    let queue = Queue::open(state)?;
    let recovered = queue.recover_interrupted(agent_host::now()?)?;
    let mut count = 0_u64;
    let mut completed = 0_u64;
    let mut last_publish = Instant::now() - Duration::from_secs(60);
    let mut last_job = None;
    let mut last_source = None::<String>;
    let mut stopped_auth = false;
    loop {
        if STOP.load(Ordering::Relaxed) {
            break;
        }
        let time = agent_host::now()?;
        // A broken/full source journal must not prevent already queued jobs
        // from draining. GC also stops conservatively if source pins cannot be
        // inspected; no uncommitted job is deleted to repair that error.
        let source = poll_one_source(state, &mut runtime, &queue, time, last_source.as_deref())
            .unwrap_or_else(
                |_| json!({"polled":false,"status":"source_unavailable","needs_attention":true}),
            );
        if let Some(id) = source["watch_id"].as_str() {
            last_source = Some(id.into());
        }
        let maintenance = match gc_terminal(state, &queue, retain as usize) {
            Ok(()) => json!({"status":"ok"}),
            Err(_) => {
                json!({"status":"unavailable","deletion_skipped":true,"needs_attention":true})
            }
        };
        let queued = queue
            .list(runtime_jobs::MAX_JOBS)?
            .into_iter()
            .filter(|j| j.status == Status::Pending)
            .min_by(|a, b| {
                a.created_at
                    .cmp(&b.created_at)
                    .then_with(|| a.id.cmp(&b.id))
            });
        let run = if let Some(job) = queued {
            let mut selected = settings.for_job(&job)?;
            let engine_name = match selected.engine {
                agent_host::EngineKind::Codex => "codex",
                agent_host::EngineKind::Antigravity => "antigravity",
            };
            if let Some(binary) = provider_binaries.get(engine_name) {
                selected.binary = binary.clone();
            }
            agent_host::write_status(
                state,
                &json!({"phase":"running_job","updated_at":time,"pid":std::process::id(),"cycles":count,"model_process_limit":1}),
            )?;
            let report = match selected.engine {
                agent_host::EngineKind::Codex => agent_host::run_job_controlled(
                    &mut runtime,
                    &queue,
                    selected.engine_config(state)?,
                    selected.model.as_deref(),
                    &STOP,
                    selected.allowed_tools.as_deref(),
                    &job.id,
                )?,
                agent_host::EngineKind::Antigravity => {
                    crate::agent_antigravity::run_job_controlled(
                        &mut runtime,
                        &queue,
                        selected.antigravity_config(state)?,
                        &STOP,
                        selected.allowed_tools.as_deref(),
                        &job.id,
                    )?
                }
            };
            if report.status == "completed" {
                completed += 1
            }
            stopped_auth = report.status == "needs_auth";
            // Heartbeats stay small even when a job has 128 detailed action
            // records. The explicit job query returns the complete evidence.
            if let Some(id) = &report.job_id {
                let mut projected = project(state, &queue.get(id)?);
                projected["engine"] = json!(report.engine);
                projected["history_persisted"] = json!(report.history_persisted);
                projected["context_error"] = json!(report.context_error);
                last_job = Some(projected);
            }
            Some(report)
        } else {
            None
        };
        count += 1;
        if last_publish.elapsed() >= Duration::from_secs(15)
            || run.is_some()
            || source["polled"] == true
        {
            let snapshot = json!({"phase":if stopped_auth{"needs_auth"}else{"waiting"},"updated_at":agent_host::now()?,"pid":std::process::id(),"cycles":count,"completed_jobs_this_run":completed,"last_job":last_job,"last_source":source,"maintenance":maintenance,"model_process_active":false,"model_process_limit":1});
            agent_host::write_status(state, &snapshot)?;
            if run.is_some() {
                let mut out = std::io::stdout().lock();
                serde_json::to_writer(&mut out, &json!({"agent_job":last_job}))
                    .map_err(|e| e.to_string())?;
                out.write_all(b"\n")
                    .and_then(|_| out.flush())
                    .map_err(|e| e.to_string())?;
            }
            last_publish = Instant::now();
        }
        if stopped_auth || cycles.is_some_and(|n| count >= n) {
            break;
        }
        let until = Instant::now() + Duration::from_millis(poll_ms);
        while !STOP.load(Ordering::Relaxed) && Instant::now() < until {
            std::thread::sleep(
                until
                    .saturating_duration_since(Instant::now())
                    .min(Duration::from_millis(100)),
            );
        }
    }
    let result = json!({"status":if stopped_auth{"needs_auth"}else{"stopped"},"cycles":count,"completed_jobs":completed,"recovered_interrupted":recovered.len(),"last_job":last_job,"automatic_replay":false,"model_process_active":false,"updated_at":agent_host::now()?,"pid":std::process::id()});
    agent_host::write_status(state, &result)?;
    Ok(result)
}
fn poll_one_source(
    state: &Path,
    runtime: &mut Runtime,
    queue: &Queue,
    time: u64,
    after: Option<&str>,
) -> Result<Value> {
    let mut store = SourceStore::open(state)?;
    let mut watches = store.list()?;
    watches.retain(|w| w.config.enabled && !w.halted && (w.pending || w.next_poll_at <= time));
    watches.sort_by(|a, b| a.config.id.cmp(&b.config.id));
    let Some(watch) = watches
        .iter()
        .find(|w| after.is_none_or(|id| w.config.id.as_str() > id))
        .or_else(|| watches.first())
    else {
        return Ok(json!({"polled":false}));
    };
    let batch = match store.collect(runtime, &watch.config.id, time) {
        Ok(batch) => batch,
        Err(_) => {
            return Ok(
                json!({"polled":false,"watch_id":watch.config.id,"status":"source_unavailable","needs_attention":true,"checkpoint_committed":false}),
            );
        }
    };
    let already_queued = match enqueue_events(queue, &batch.operator_prompt, &batch.events) {
        Ok(count) => count,
        Err(error) => {
            return Ok(
                json!({"polled":true,"watch_id":batch.watch_id,"status":"queue_blocked","error":error,"checkpoint_committed":false,"pending_batch_retained":true}),
            );
        }
    };
    let status = if batch.checkpoint.is_some() {
        Some(store.commit(&batch)?)
    } else {
        None
    };
    Ok(
        json!({"polled":true,"watch_id":batch.watch_id,"status":batch.status,"events_enqueued":batch.events.len()-already_queued,"events_already_queued":already_queued,"checkpoint_committed":status.is_some(),"coverage":batch.coverage,"error":batch.error,"next_poll_at":batch.next_poll_at}),
    )
}
fn gc_terminal(state: &Path, queue: &Queue, retain: usize) -> Result<()> {
    let store = SourceStore::open(state)?;
    let protected = store
        .pending_job_ids()?
        .into_iter()
        .collect::<BTreeSet<_>>();
    let mut jobs = queue
        .list(runtime_jobs::MAX_JOBS)?
        .into_iter()
        .filter(|j| {
            matches!(
                j.status,
                Status::Completed | Status::Failed | Status::Canceled
            )
        })
        .collect::<Vec<_>>();
    jobs.sort_by_key(|j| std::cmp::Reverse((j.finished_at.unwrap_or(j.updated_at), j.id.clone())));
    let remove = jobs
        .into_iter()
        .skip(retain)
        .filter(|j| !protected.contains(&j.id))
        .map(|j| j.id)
        .collect::<Vec<_>>();
    if !remove.is_empty() {
        queue.gc_finished(&remove)?;
    }
    Ok(())
}
fn same_source_event(job: &Job, event: &crate::agent_sources::SourceEvent) -> bool {
    let Some(saved) = &job.event else {
        return false;
    };
    job.context == event.context
        && saved.get("watch_id").and_then(Value::as_str) == Some(event.watch_id.as_str())
        && saved.get("source_key").and_then(Value::as_str) == Some(event.source_key.as_str())
        && serde_json::from_value::<runtime_core::Scope>(saved["scope"].clone())
            .ok()
            .as_ref()
            == Some(&event.scope)
        && serde_json::to_value(&event.source).ok().as_ref() == saved.get("source")
}
fn enqueue_events(
    queue: &Queue,
    prompt: &str,
    events: &[crate::agent_sources::SourceEvent],
) -> Result<usize> {
    let existing = queue.list(runtime_jobs::MAX_JOBS)?;
    let mut already = 0;
    for event in events {
        if existing
            .iter()
            .any(|job| job.id == event.job_id && same_source_event(job, event))
        {
            already += 1;
            continue;
        }
        queue.enqueue_configured(
            &event.job_id,
            prompt,
            Some(serde_json::to_value(event).map_err(|e| e.to_string())?),
            event.context.clone(),
            event.execution_profile.clone(),
            event.observed_at,
        )?;
    }
    Ok(already)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::agent_sources::{Source, SourceEvent};
    #[test]
    fn repeated_provider_event_keeps_its_original_queued_snapshot() {
        let root = std::env::temp_dir().join(format!(
            "agent-dedup-{}-{}",
            std::process::id(),
            agent_host::now().unwrap()
        ));
        let queue = Queue::open(&root).unwrap();
        let event = SourceEvent {
            execution_profile: None,
            context: runtime_jobs::AgentContext::default(),
            job_id: "source-fixture".into(),
            watch_id: "messages".into(),
            scope: runtime_core::Scope {
                platform: runtime_core::Platform::Qq,
                account: "owner".into(),
            },
            source: Source::QqMessages,
            source_key: "sequence:7".into(),
            identity_quality: "local_sequence".into(),
            observed_at: 100,
            payload: json!({"text":"hello","target_ref":"original"}),
        };
        assert_eq!(
            enqueue_events(&queue, "original operator task", &[event.clone()]).unwrap(),
            0
        );
        let mut updated = event.clone();
        updated.observed_at = 200;
        updated.payload["target_ref"] = json!("new opaque ref");
        assert_eq!(
            enqueue_events(&queue, "updated operator task", &[updated.clone()]).unwrap(),
            1
        );
        let stored = queue.get(&event.job_id).unwrap();
        assert_eq!(stored.prompt, "original operator task");
        assert_eq!(stored.event.unwrap()["payload"]["target_ref"], "original");
        updated.source_key = "sequence:8".into();
        assert!(enqueue_events(&queue, "task", &[updated]).is_err());
        std::fs::remove_dir_all(root).unwrap();
    }
}
