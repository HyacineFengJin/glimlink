// SPDX-License-Identifier: MIT
//! A small synchronous durable queue. The queue lock covers one disk transaction,
//! never a model turn. An interrupted turn is never made pending automatically.
//! There is no network, timer, executor, event source, or platform-write retry here.
use runtime_core::{
    Result,
    resources::{open_regular, private_dir},
};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::{
    collections::{BTreeMap, BTreeSet},
    fs::{self, File, OpenOptions},
    io::{Read, Write},
    os::unix::fs::{OpenOptionsExt, PermissionsExt},
    path::{Path, PathBuf},
    time::{Duration, Instant},
};

pub const MAX_JOBS: usize = 128;
pub const MAX_DISK_BYTES: u64 = 16 * 1024 * 1024;
pub const MAX_PROMPT_BYTES: usize = 16 * 1024;
pub const MAX_EVENT_BYTES: usize = 32 * 1024;
pub const MAX_TEXT_BYTES: usize = 64 * 1024;
pub const MAX_OPERATION_IDS: usize = 128;
const MAX_ERROR_BYTES: usize = 8 * 1024;
// Allows JSON escaping of the raw text limits, plus bounded event and progress.
const MAX_RECORD_BYTES: u64 = 640 * 1024;
const MAX_ENTRIES: usize = MAX_JOBS * 2 + 1;
const LOCK_WAIT: Duration = Duration::from_secs(2);
const CANCELED_MARKER: &str = "Canceled explicitly before any model turn or tool execution.";

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Status {
    Pending,
    Running,
    Completed,
    Failed,
    Interrupted,
    NeedsAuth,
    Canceled,
}

/// Trusted execution identity, fixed when a task is enqueued. Transport client
/// names and platform content never select this identity.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct AgentContext {
    pub persona: String,
    pub agent: String,
    pub conversation: Option<String>,
}
impl Default for AgentContext {
    fn default() -> Self {
        Self {
            persona: "main".into(),
            agent: "main".into(),
            conversation: None,
        }
    }
}
impl AgentContext {
    pub fn validate(&self) -> Result<()> {
        fn id(value: &str) -> bool {
            !value.is_empty()
                && value.len() <= 96
                && value
                    .bytes()
                    .all(|b| b.is_ascii_alphanumeric() || b"_:-".contains(&b))
        }
        if !id(&self.persona)
            || !id(&self.agent)
            || self.conversation.as_ref().is_some_and(|v| !id(v))
        {
            return Err("agent/persona/conversation identifiers must be 1..96 ASCII letters, digits, '_', ':' or '-'".into());
        }
        Ok(())
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ContextVersion {
    pub persona_revision: u64,
    pub role_revision: u64,
    pub memory_revision: u64,
    pub history_epoch: u64,
}

/// Operator execution policy copied into the job before any model runs.
/// Editing a stored agent profile cannot retarget an already queued task.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ExecutionProfile {
    pub revision: u64,
    pub engine: String,
    #[serde(default)]
    pub model: Option<String>,
    #[serde(default)]
    pub allowed_tools: Option<Vec<String>>,
    #[serde(default)]
    pub default_accounts: BTreeMap<String, String>,
}
impl ExecutionProfile {
    pub fn validate(&self) -> Result<()> {
        if self.revision == 0
            || !matches!(self.engine.as_str(), "codex" | "antigravity")
            || self
                .model
                .as_ref()
                .is_some_and(|m| m.is_empty() || m.len() > 128 || m.chars().any(char::is_control))
            || self.default_accounts.len() > 4
        {
            return Err("invalid frozen agent execution profile".into());
        }
        if let Some(tools) = &self.allowed_tools {
            let names: BTreeSet<_> = tools.iter().collect();
            if tools.len() > 32
                || names.len() != tools.len()
                || tools.iter().any(|name| {
                    name.is_empty()
                        || name.len() > 96
                        || !name.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'_')
                })
            {
                return Err("invalid profile tool allowlist".into());
            }
        }
        for (platform, account) in &self.default_accounts {
            let platform = match platform.as_str() {
                "bilibili" => runtime_core::Platform::Bilibili,
                "tieba" => runtime_core::Platform::Tieba,
                "qq" => runtime_core::Platform::Qq,
                "github" => runtime_core::Platform::Github,
                _ => return Err("unsupported profile default account platform".into()),
            };
            runtime_core::Scope {
                platform,
                account: account.clone(),
            }
            .validate()?;
        }
        Ok(())
    }
}

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Progress {
    pub thread_id: Option<String>,
    pub turn_id: Option<String>,
    pub operation_ids: Vec<String>,
    pub tool_calls: u32,
}
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Job {
    pub schema: u32,
    pub id: String,
    pub status: Status,
    pub prompt: String,
    pub event: Option<Value>,
    #[serde(default)]
    pub context: AgentContext,
    #[serde(default)]
    pub execution_profile: Option<ExecutionProfile>,
    #[serde(default)]
    pub context_version: Option<ContextVersion>,
    #[serde(default)]
    pub engine: Option<String>,
    pub created_at: u64,
    pub updated_at: u64,
    pub started_at: Option<u64>,
    pub finished_at: Option<u64>,
    pub progress: Progress,
    pub text: Option<String>,
    pub error: Option<String>,
}
#[derive(Clone, Debug)]
pub struct Outcome {
    /// Only Completed, Failed, Interrupted, and NeedsAuth are accepted by finish.
    pub status: Status,
    pub text: Option<String>,
    pub error: Option<String>,
}
impl Outcome {
    pub fn completed(text: impl Into<String>) -> Self {
        Self {
            status: Status::Completed,
            text: Some(text.into()),
            error: None,
        }
    }
    pub fn failed(error: impl Into<String>) -> Self {
        Self {
            status: Status::Failed,
            text: None,
            error: Some(error.into()),
        }
    }
    pub fn needs_auth(error: impl Into<String>) -> Self {
        Self {
            status: Status::NeedsAuth,
            text: None,
            error: Some(error.into()),
        }
    }
    pub fn interrupted(error: impl Into<String>) -> Self {
        Self {
            status: Status::Interrupted,
            text: None,
            error: Some(error.into()),
        }
    }
}

/// Only claim_next returns this capability. get/list never expose a run token.
#[derive(Clone)]
pub struct Lease {
    job_id: String,
    token: String,
}
impl Lease {
    pub fn job_id(&self) -> &str {
        &self.job_id
    }
}
impl std::fmt::Debug for Lease {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Lease")
            .field("job_id", &self.job_id)
            .field("token", &"[redacted]")
            .finish()
    }
}
#[derive(Clone, Debug)]
pub struct ClaimedJob {
    pub job: Job,
    pub lease: Lease,
}

#[derive(Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Record {
    job: Job,
    run_token: Option<String>,
}
struct Entry {
    record: Record,
    bytes: u64,
}
struct Scan {
    entries: Vec<Entry>,
    temporary: Vec<PathBuf>,
    bytes: u64,
}

pub struct Queue {
    root: PathBuf,
}
struct Transaction {
    file: File,
}
impl Drop for Transaction {
    fn drop(&mut self) {
        let _ = self.file.unlock();
    }
}

impl Queue {
    pub fn open(state: &Path) -> Result<Self> {
        let root = state.join("jobs");
        private_dir(&root)?;
        Ok(Self { root })
    }
    /// Same id plus equal prompt/event returns the original job in any state.
    /// created_at is deliberately excluded from idempotency comparison.
    pub fn enqueue(
        &self,
        id: &str,
        prompt: &str,
        event: Option<Value>,
        created_at: u64,
    ) -> Result<Job> {
        self.enqueue_with_context(id, prompt, event, AgentContext::default(), created_at)
    }
    pub fn enqueue_with_context(
        &self,
        id: &str,
        prompt: &str,
        event: Option<Value>,
        context: AgentContext,
        created_at: u64,
    ) -> Result<Job> {
        self.enqueue_configured(id, prompt, event, context, None, created_at)
    }
    pub fn enqueue_configured(
        &self,
        id: &str,
        prompt: &str,
        event: Option<Value>,
        context: AgentContext,
        execution_profile: Option<ExecutionProfile>,
        created_at: u64,
    ) -> Result<Job> {
        validate_id(id)?;
        validate_input(prompt, event.as_ref())?;
        context.validate()?;
        if let Some(profile) = &execution_profile {
            profile.validate()?;
        }
        let _lock = self.lock()?;
        let mut scan = self.scan()?;
        self.clean_temporaries(&mut scan)?;
        if let Some(entry) = scan.entries.iter().find(|e| e.record.job.id == id) {
            if entry.record.job.prompt != prompt
                || entry.record.job.event != event
                || entry.record.job.context != context
                || entry.record.job.execution_profile != execution_profile
            {
                return Err(
                    "job ID already belongs to a different prompt, event, agent context or execution profile".into(),
                );
            }
            return Ok(entry.record.job.clone());
        }
        if scan.entries.len() >= MAX_JOBS {
            return Err(
                "job quota reached; explicitly GC completed/failed/canceled history".into(),
            );
        }
        let record = Record {
            job: Job {
                schema: 1,
                id: id.into(),
                status: Status::Pending,
                prompt: prompt.into(),
                event,
                context,
                execution_profile,
                context_version: None,
                engine: None,
                created_at,
                updated_at: created_at,
                started_at: None,
                finished_at: None,
                progress: Progress::default(),
                text: None,
                error: None,
            },
            run_token: None,
        };
        self.save(&record, &scan, true)?;
        Ok(record.job)
    }
    /// A read transaction only. In particular it never recovers a Running job.
    pub fn get(&self, id: &str) -> Result<Job> {
        validate_id(id)?;
        let _lock = self.lock()?;
        let scan = self.scan()?;
        scan.entries
            .into_iter()
            .find(|e| e.record.job.id == id)
            .map(|e| e.record.job)
            .ok_or_else(|| "job not found".into())
    }
    /// Record the exact shared context version before creating a model thread.
    /// A different version cannot replace a running job's initial snapshot.
    pub fn record_context(&self, lease: &Lease, version: ContextVersion, now: u64) -> Result<Job> {
        let _lock = self.lock()?;
        let mut scan = self.scan()?;
        self.clean_temporaries(&mut scan)?;
        let mut record = leased(&scan, lease)?.clone();
        check_now(&record.job, now)?;
        if let Some(saved) = &record.job.context_version {
            if saved != &version {
                return Err("job context version is already fixed".into());
            }
            return Ok(record.job);
        }
        record.job.context_version = Some(version);
        record.job.updated_at = now.max(record.job.updated_at);
        self.save(&record, &scan, true)?;
        Ok(record.job)
    }
    /// Provider identity is fixed before a model turn, never inferred from a recollection.
    pub fn record_engine(&self, lease: &Lease, engine: &str, now: u64) -> Result<Job> {
        if !matches!(engine, "codex" | "antigravity") {
            return Err("unknown job engine".into());
        }
        let _lock = self.lock()?;
        let mut scan = self.scan()?;
        self.clean_temporaries(&mut scan)?;
        let mut record = leased(&scan, lease)?.clone();
        check_now(&record.job, now)?;
        if let Some(saved) = &record.job.engine {
            if saved != engine {
                return Err("job engine is already fixed".into());
            }
            return Ok(record.job);
        }
        record.job.engine = Some(engine.into());
        record.job.updated_at = now;
        self.save(&record, &scan, true)?;
        Ok(record.job)
    }
    /// Newest first; the entire queue is bounded independently of this limit.
    pub fn list(&self, limit: usize) -> Result<Vec<Job>> {
        if !(1..=MAX_JOBS).contains(&limit) {
            return Err("job list limit must be 1..128".into());
        }
        let _lock = self.lock()?;
        let scan = self.scan()?;
        let mut jobs: Vec<_> = scan.entries.into_iter().map(|e| e.record.job).collect();
        jobs.sort_by(|a, b| {
            b.created_at
                .cmp(&a.created_at)
                .then_with(|| a.id.cmp(&b.id))
        });
        jobs.truncate(limit);
        Ok(jobs)
    }
    /// Claim at most one global model task. Pending ordering is (created_at,id).
    pub fn claim_next(&self, now: u64) -> Result<Option<ClaimedJob>> {
        self.claim_selected(None, now)
    }
    /// Explicit one-shot execution must not accidentally consume another queued task.
    pub fn claim_job(&self, id: &str, now: u64) -> Result<Option<ClaimedJob>> {
        validate_id(id)?;
        self.claim_selected(Some(id), now)
    }
    fn claim_selected(&self, id: Option<&str>, now: u64) -> Result<Option<ClaimedJob>> {
        let _lock = self.lock()?;
        let mut scan = self.scan()?;
        self.clean_temporaries(&mut scan)?;
        if scan
            .entries
            .iter()
            .any(|e| e.record.job.status == Status::Running)
        {
            return Ok(None);
        }
        let Some(entry) = scan
            .entries
            .iter()
            .filter(|e| {
                e.record.job.status == Status::Pending && id.is_none_or(|id| e.record.job.id == id)
            })
            .min_by(|a, b| {
                a.record
                    .job
                    .created_at
                    .cmp(&b.record.job.created_at)
                    .then_with(|| a.record.job.id.cmp(&b.record.job.id))
            })
        else {
            return Ok(None);
        };
        let mut record = entry.record.clone();
        check_now(&record.job, now)?;
        let token = random_token()?;
        record.run_token = Some(token.clone());
        record.job.status = Status::Running;
        record.job.started_at = Some(now);
        record.job.updated_at = now;
        self.save(&record, &scan, true)?;
        Ok(Some(ClaimedJob {
            lease: Lease {
                job_id: record.job.id.clone(),
                token,
            },
            job: record.job,
        }))
    }
    /// Must succeed before the host executes the corresponding tool call.
    /// IDs are merged monotonically, and tool_calls never decreases.
    pub fn update_progress(&self, lease: &Lease, progress: Progress, now: u64) -> Result<Job> {
        validate_progress(&progress, false)?;
        let _lock = self.lock()?;
        let mut scan = self.scan()?;
        self.clean_temporaries(&mut scan)?;
        let mut record = leased(&scan, lease)?.clone();
        check_now(&record.job, now)?;
        let saved = &mut record.job.progress;
        if progress
            .thread_id
            .as_ref()
            .zip(saved.thread_id.as_ref())
            .is_some_and(|(a, b)| a != b)
        {
            return Err("claimed job is already bound to another model thread".into());
        }
        if progress.thread_id.is_some() {
            saved.thread_id = progress.thread_id;
        }
        if progress.turn_id.is_some() {
            saved.turn_id = progress.turn_id;
        }
        for id in progress.operation_ids {
            if !saved.operation_ids.contains(&id) {
                saved.operation_ids.push(id);
            }
        }
        saved.tool_calls = saved.tool_calls.max(progress.tool_calls);
        validate_progress(saved, true)?;
        record.job.updated_at = now;
        self.save(&record, &scan, true)?;
        Ok(record.job)
    }
    pub fn finish(&self, lease: &Lease, outcome: Outcome, now: u64) -> Result<Job> {
        validate_outcome(&outcome)?;
        let _lock = self.lock()?;
        let mut scan = self.scan()?;
        self.clean_temporaries(&mut scan)?;
        let mut record = leased(&scan, lease)?.clone();
        check_now(&record.job, now)?;
        record.job.status = outcome.status;
        record.job.text = outcome.text;
        record.job.error = outcome.error;
        record.job.finished_at = Some(now);
        record.job.updated_at = now;
        // Admission/progress reserve one maximum-sized record so finishing the
        // sole Running job can grow its result without competing enqueue calls
        // consuming the headroom. The temporary copy counts toward disk quota.
        self.save(&record, &scan, false)?;
        Ok(record.job)
    }
    /// Startup maintenance only: the host must first hold its own exclusive
    /// daemon/Runtime lock and know the previous worker has exited. Never call
    /// from CLI status/list or periodically against a live worker.
    pub fn recover_interrupted(&self, now: u64) -> Result<Vec<Job>> {
        let _lock = self.lock()?;
        let mut scan = self.scan()?;
        self.clean_temporaries(&mut scan)?;
        let Some(entry) = scan
            .entries
            .iter()
            .find(|e| e.record.job.status == Status::Running)
        else {
            return Ok(Vec::new());
        };
        let mut record = entry.record.clone();
        check_now(&record.job, now)?;
        record.job.status = Status::Interrupted;
        record.job.updated_at = now;
        record.job.finished_at = Some(now);
        record.job.error=Some("Host interrupted after durable claim; platform writes may have occurred. This job will not be replayed automatically.".into());
        self.save(&record, &scan, false)?;
        Ok(vec![record.job])
    }
    /// Explicit human resume after subscription login. Only NeedsAuth jobs with
    /// no model thread, turn, tool call, or operation evidence can be requeued.
    /// The caller must not invoke this automatically after engine/auth failure.
    pub fn resume_needs_auth(&self, ids: &[String], now: u64) -> Result<usize> {
        if ids.is_empty() || ids.len() > MAX_JOBS {
            return Err("resume requires 1..128 explicit job IDs".into());
        }
        let mut wanted = BTreeSet::new();
        for id in ids {
            validate_id(id)?;
            if !wanted.insert(id) {
                return Err("duplicate job ID in resume request".into());
            }
        }
        let _lock = self.lock()?;
        let mut scan = self.scan()?;
        self.clean_temporaries(&mut scan)?;
        let mut indices = Vec::new();
        // Validate the entire request before changing any eligible record.
        for id in &wanted {
            let index = scan
                .entries
                .iter()
                .position(|e| &e.record.job.id == *id)
                .ok_or("resume job not found")?;
            let job = &scan.entries[index].record.job;
            if job.status != Status::NeedsAuth
                || job.progress != Progress::default()
                || job.context_version.is_some()
                || job.engine.is_some()
            {
                return Err("only NeedsAuth jobs without model/tool/operation evidence can be explicitly resumed".into());
            }
            check_now(job, now)?;
            indices.push(index);
        }
        for index in indices {
            let mut record = scan.entries[index].record.clone();
            record.job.status = Status::Pending;
            record.job.updated_at = now;
            record.job.started_at = None;
            record.job.finished_at = None;
            record.job.text = None;
            record.job.error = None;
            record.run_token = None;
            self.save(&record, &scan, true)?;
            let bytes = encode_bounded(
                &record,
                MAX_RECORD_BYTES as usize,
                "job record exceeds byte limit",
            )?
            .len() as u64;
            scan.bytes = scan.bytes - scan.entries[index].bytes + bytes;
            scan.entries[index] = Entry { record, bytes };
        }
        Ok(wanted.len())
    }
    /// Explicit cancellation never preempts a model turn or a tool action.
    /// Only Pending and execution-free NeedsAuth jobs can be canceled.
    pub fn cancel_pending(&self, ids: &[String], now: u64) -> Result<usize> {
        if ids.is_empty() || ids.len() > MAX_JOBS {
            return Err("cancel requires 1..128 explicit job IDs".into());
        }
        let mut wanted = BTreeSet::new();
        for id in ids {
            validate_id(id)?;
            if !wanted.insert(id) {
                return Err("duplicate job ID in cancel request".into());
            }
        }
        let _lock = self.lock()?;
        let mut scan = self.scan()?;
        self.clean_temporaries(&mut scan)?;
        let mut changed = Vec::new();
        let mut simulated = scan.bytes;
        // Validate statuses and disk budget for the entire request first.
        for id in &wanted {
            let index = scan
                .entries
                .iter()
                .position(|e| &e.record.job.id == *id)
                .ok_or("cancel job not found")?;
            let mut record = scan.entries[index].record.clone();
            if !matches!(record.job.status, Status::Pending | Status::NeedsAuth)
                || record.job.progress != Progress::default()
            {
                return Err("only Pending or execution-free NeedsAuth jobs can be canceled; Running/Interrupted or executed jobs cannot be preempted".into());
            }
            check_now(&record.job, now)?;
            record.job.status = Status::Canceled;
            record.job.updated_at = now;
            record.job.finished_at = Some(now);
            record.job.text = None;
            record.job.error = Some(CANCELED_MARKER.into());
            validate_record(&record)?;
            let bytes = encode_bounded(
                &record,
                MAX_RECORD_BYTES as usize,
                "job record exceeds byte limit",
            )?
            .len() as u64;
            if simulated + bytes > MAX_DISK_BYTES {
                return Err(
                    "job disk quota reached; explicitly GC completed/failed/canceled history"
                        .into(),
                );
            }
            simulated = simulated - scan.entries[index].bytes + bytes;
            changed.push((index, record, bytes));
        }
        for (index, record, bytes) in changed {
            self.save(&record, &scan, false)?;
            scan.bytes = scan.bytes - scan.entries[index].bytes + bytes;
            scan.entries[index] = Entry { record, bytes };
        }
        Ok(wanted.len())
    }
    /// Explicit history deletion only. Unfinished, Interrupted, and NeedsAuth
    /// jobs remain visible and cannot be discarded through this method.
    pub fn gc_finished(&self, ids: &[String]) -> Result<usize> {
        if ids.is_empty() || ids.len() > MAX_JOBS {
            return Err("GC requires 1..128 explicit job IDs".into());
        }
        let mut wanted = BTreeSet::new();
        for id in ids {
            validate_id(id)?;
            if !wanted.insert(id) {
                return Err("duplicate job ID in GC request".into());
            }
        }
        let _lock = self.lock()?;
        let mut scan = self.scan()?;
        self.clean_temporaries(&mut scan)?;
        for id in &wanted {
            let entry = scan
                .entries
                .iter()
                .find(|e| &e.record.job.id == *id)
                .ok_or("GC job not found")?;
            if !matches!(
                entry.record.job.status,
                Status::Completed | Status::Failed | Status::Canceled
            ) {
                return Err(
                    "only explicitly selected completed/failed/canceled history can be deleted"
                        .into(),
                );
            }
        }
        for id in &wanted {
            fs::remove_file(self.path(id)?).map_err(|e| e.to_string())?;
        }
        sync_directory(&self.root)?;
        Ok(wanted.len())
    }
    fn path(&self, id: &str) -> Result<PathBuf> {
        validate_id(id)?;
        Ok(self.root.join(format!("{id}.json")))
    }
    fn lock(&self) -> Result<Transaction> {
        let path = self.root.join("jobs.lock");
        let file = match OpenOptions::new()
            .read(true)
            .write(true)
            .create_new(true)
            .mode(0o600)
            .open(&path)
        {
            Ok(file) => file,
            Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => {
                open_regular(&path, 0)?
            }
            Err(error) => return Err(error.to_string()),
        };
        check_private(&file)?;
        let start = Instant::now();
        loop {
            match file.try_lock() {
                Ok(()) => return Ok(Transaction { file }),
                Err(std::fs::TryLockError::WouldBlock) if start.elapsed() < LOCK_WAIT => {
                    std::thread::sleep(Duration::from_millis(5))
                }
                Err(std::fs::TryLockError::WouldBlock) => {
                    return Err("job queue transaction busy; retry later".into());
                }
                Err(std::fs::TryLockError::Error(error)) => return Err(error.to_string()),
            }
        }
    }
    fn scan(&self) -> Result<Scan> {
        let mut entries = Vec::new();
        let mut temporary = Vec::new();
        let mut bytes = 0u64;
        let mut count = 0usize;
        for entry in fs::read_dir(&self.root).map_err(|e| e.to_string())? {
            count += 1;
            if count > MAX_ENTRIES {
                return Err("job directory entry limit exceeded".into());
            }
            let entry = entry.map_err(|e| e.to_string())?;
            let name = entry
                .file_name()
                .into_string()
                .map_err(|_| "unrecognized job directory entry")?;
            if name == "jobs.lock" {
                continue;
            }
            let is_temporary = name
                .strip_prefix(".tmp-")
                .and_then(|s| s.strip_suffix(".json"))
                .is_some_and(valid_token);
            let id = if is_temporary {
                None
            } else {
                Some(
                    name.strip_suffix(".json")
                        .ok_or("unrecognized job directory entry")?,
                )
            };
            if let Some(id) = id {
                validate_id(id)?;
            }
            let file = open_regular(&entry.path(), MAX_RECORD_BYTES)?;
            check_private(&file)?;
            let size = file.metadata().map_err(|e| e.to_string())?.len();
            bytes = bytes.checked_add(size).ok_or("job disk byte overflow")?;
            if bytes > MAX_DISK_BYTES {
                return Err("job disk quota exceeded".into());
            }
            if is_temporary {
                temporary.push(entry.path());
                continue;
            }
            if entries.len() >= MAX_JOBS {
                return Err("job record count exceeded".into());
            }
            let record: Record = serde_json::from_slice(&read_bounded(file, MAX_RECORD_BYTES)?)
                .map_err(|_| "invalid private job record")?;
            validate_record(&record)?;
            if Some(record.job.id.as_str()) != id {
                return Err("job file and record ID differ".into());
            }
            entries.push(Entry {
                record,
                bytes: size,
            });
        }
        if entries
            .iter()
            .filter(|e| e.record.job.status == Status::Running)
            .count()
            > 1
        {
            return Err(
                "queue contains multiple Running records; manual attention required".into(),
            );
        }
        Ok(Scan {
            entries,
            temporary,
            bytes,
        })
    }
    fn clean_temporaries(&self, scan: &mut Scan) -> Result<()> {
        if scan.temporary.is_empty() {
            return Ok(());
        }
        for path in &scan.temporary {
            let size = fs::symlink_metadata(path).map_err(|e| e.to_string())?.len();
            fs::remove_file(path).map_err(|e| e.to_string())?;
            scan.bytes = scan
                .bytes
                .checked_sub(size)
                .ok_or("job temporary byte accounting mismatch")?;
        }
        sync_directory(&self.root)?;
        scan.temporary.clear();
        Ok(())
    }
    fn save(&self, record: &Record, scan: &Scan, reserve_finish: bool) -> Result<()> {
        validate_record(record)?;
        let encoded = encode_bounded(
            record,
            MAX_RECORD_BYTES as usize,
            "job record exceeds byte limit",
        )?;
        let old = scan
            .entries
            .iter()
            .find(|e| e.record.job.id == record.job.id)
            .map_or(0, |e| e.bytes);
        let final_bytes = scan
            .bytes
            .checked_sub(old)
            .and_then(|n| n.checked_add(encoded.len() as u64))
            .ok_or("job byte accounting overflow")?;
        if scan.bytes + encoded.len() as u64 > MAX_DISK_BYTES
            || (reserve_finish && final_bytes + MAX_RECORD_BYTES > MAX_DISK_BYTES)
        {
            return Err(
                "job disk quota reached; explicitly GC completed/failed/canceled history".into(),
            );
        }
        let temporary = self.root.join(format!(".tmp-{}.json", random_token()?));
        let result = (|| {
            let mut file = OpenOptions::new()
                .write(true)
                .create_new(true)
                .mode(0o600)
                .open(&temporary)
                .map_err(|e| e.to_string())?;
            file.write_all(&encoded)
                .and_then(|_| file.sync_all())
                .map_err(|e| e.to_string())?;
            fs::rename(&temporary, self.path(&record.job.id)?).map_err(|e| e.to_string())?;
            sync_directory(&self.root)
        })();
        if result.is_err() {
            let _ = fs::remove_file(&temporary);
        }
        result
    }
}

fn leased<'a>(scan: &'a Scan, lease: &Lease) -> Result<&'a Record> {
    validate_id(&lease.job_id)?;
    let record = &scan
        .entries
        .iter()
        .find(|e| e.record.job.id == lease.job_id)
        .ok_or("claimed job not found")?
        .record;
    if record.job.status != Status::Running
        || !valid_token(&lease.token)
        || record.run_token.as_deref() != Some(lease.token.as_str())
    {
        return Err("stale or invalid job claim; result was not applied".into());
    }
    Ok(record)
}
fn validate_id(id: &str) -> Result<()> {
    if id.is_empty()
        || id.len() > 96
        || !id
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b"_-".contains(&b))
    {
        Err("job/operation ID must contain 1..96 ASCII letters, digits, '-' or '_'".into())
    } else {
        Ok(())
    }
}
fn valid_token(token: &str) -> bool {
    token.len() == 32
        && token
            .bytes()
            .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
}
fn random_token() -> Result<String> {
    let mut bytes = [0u8; 16];
    File::open("/dev/urandom")
        .and_then(|mut f| f.read_exact(&mut bytes))
        .map_err(|e| e.to_string())?;
    Ok(bytes.iter().map(|b| format!("{b:02x}")).collect())
}
fn check_private(file: &File) -> Result<()> {
    let metadata = file.metadata().map_err(|e| e.to_string())?;
    if !metadata.is_file() || metadata.permissions().mode() & 0o777 != 0o600 {
        return Err("job files and lock must be private regular files (0600)".into());
    }
    Ok(())
}
fn read_bounded(file: File, limit: u64) -> Result<Vec<u8>> {
    let mut bytes = Vec::new();
    file.take(limit + 1)
        .read_to_end(&mut bytes)
        .map_err(|e| e.to_string())?;
    if bytes.len() as u64 > limit {
        Err("job record exceeds byte limit".into())
    } else {
        Ok(bytes)
    }
}
fn sync_directory(path: &Path) -> Result<()> {
    File::open(path)
        .and_then(|f| f.sync_all())
        .map_err(|e| e.to_string())
}
fn validate_input(prompt: &str, event: Option<&Value>) -> Result<()> {
    if prompt.trim().is_empty() || prompt.len() > MAX_PROMPT_BYTES || prompt.contains('\0') {
        return Err("job prompt requires 1..16 KiB of text without NUL".into());
    }
    if let Some(event) = event {
        let mut stack = vec![(event, 0usize)];
        let mut count = 0usize;
        while let Some((value, depth)) = stack.pop() {
            count += 1;
            if count > 8192 || depth > 64 {
                return Err("job event structure exceeds bounds".into());
            }
            let mut push = |child| -> Result<()> {
                if count + stack.len() >= 8192 {
                    return Err("job event structure exceeds bounds".into());
                }
                stack.push((child, depth + 1));
                Ok(())
            };
            match value {
                Value::Array(items) => {
                    for item in items {
                        push(item)?;
                    }
                }
                Value::Object(items) => {
                    for item in items.values() {
                        push(item)?;
                    }
                }
                _ => {}
            }
        }
        encode_bounded(event, MAX_EVENT_BYTES, "job event exceeds 32 KiB")?;
    }
    Ok(())
}
fn validate_progress(progress: &Progress, unique: bool) -> Result<()> {
    for id in [&progress.thread_id, &progress.turn_id]
        .into_iter()
        .flatten()
    {
        if id.is_empty() || id.len() > 256 || id.chars().any(char::is_control) {
            return Err("invalid bounded model thread/turn identity".into());
        }
    }
    if progress.operation_ids.len() > MAX_OPERATION_IDS {
        return Err("job operation ID budget reached".into());
    }
    let mut seen = BTreeSet::new();
    for id in &progress.operation_ids {
        validate_id(id)?;
        if !seen.insert(id) && unique {
            return Err("duplicate saved job operation ID".into());
        }
    }
    Ok(())
}
fn validate_outcome(outcome: &Outcome) -> Result<()> {
    if !matches!(
        outcome.status,
        Status::Completed | Status::Failed | Status::Interrupted | Status::NeedsAuth
    ) || outcome
        .text
        .as_ref()
        .is_some_and(|s| s.len() > MAX_TEXT_BYTES)
        || outcome
            .error
            .as_ref()
            .is_some_and(|s| s.is_empty() || s.len() > MAX_ERROR_BYTES)
        || (outcome.status == Status::Completed && outcome.text.is_none())
        || (outcome.status != Status::Completed && outcome.error.is_none())
    {
        return Err("invalid bounded terminal job outcome".into());
    }
    Ok(())
}
fn check_now(job: &Job, now: u64) -> Result<()> {
    if now < job.updated_at {
        Err("job clock precedes its last durable update".into())
    } else {
        Ok(())
    }
}
fn validate_record(record: &Record) -> Result<()> {
    let job = &record.job;
    validate_id(&job.id)?;
    validate_input(&job.prompt, job.event.as_ref())?;
    job.context.validate()?;
    if let Some(profile) = &job.execution_profile {
        profile.validate()?;
        if job
            .engine
            .as_ref()
            .is_some_and(|engine| engine != &profile.engine)
        {
            return Err("job engine differs from its frozen execution profile".into());
        }
    }
    validate_progress(&job.progress, true)?;
    if job.schema != 1
        || job
            .engine
            .as_deref()
            .is_some_and(|engine| !matches!(engine, "codex" | "antigravity"))
        || job.updated_at < job.created_at
        || job
            .started_at
            .is_some_and(|t| t < job.created_at || t > job.updated_at)
        || job.finished_at.is_some_and(|t| {
            t < job.created_at || t > job.updated_at || job.started_at.is_some_and(|s| t < s)
        })
    {
        return Err("invalid job schema or timestamps".into());
    }
    if job.status == Status::Pending {
        if job.context_version.is_some()
            || job.engine.is_some()
            || job.started_at.is_some()
            || job.finished_at.is_some()
            || record.run_token.is_some()
            || job.text.is_some()
            || job.error.is_some()
            || job.progress != Progress::default()
        {
            return Err("Pending job contains execution state".into());
        }
    } else if job.status == Status::Canceled {
        if job.finished_at.is_none()
            || job.text.is_some()
            || job.error.as_deref() != Some(CANCELED_MARKER)
            || job.progress != Progress::default()
            || job.started_at.is_some() != record.run_token.is_some()
            || record
                .run_token
                .as_deref()
                .is_some_and(|token| !valid_token(token))
        {
            return Err("invalid execution-free canceled job record".into());
        }
    } else {
        if job.started_at.is_none()
            || record
                .run_token
                .as_deref()
                .is_none_or(|token| !valid_token(token))
        {
            return Err("started job lacks its durable claim".into());
        }
        if job.status == Status::Running {
            if job.finished_at.is_some() || job.text.is_some() || job.error.is_some() {
                return Err("Running job contains a terminal result".into());
            }
        } else {
            if job.finished_at.is_none() {
                return Err("terminal job lacks its finish time".into());
            }
            validate_outcome(&Outcome {
                status: job.status,
                text: job.text.clone(),
                error: job.error.clone(),
            })?;
        }
    }
    Ok(())
}
struct LimitedWriter {
    bytes: Vec<u8>,
    limit: usize,
}
impl Write for LimitedWriter {
    fn write(&mut self, bytes: &[u8]) -> std::io::Result<usize> {
        if bytes.len() > self.limit.saturating_sub(self.bytes.len()) {
            return Err(std::io::Error::other("bounded JSON output exceeded"));
        }
        self.bytes.extend_from_slice(bytes);
        Ok(bytes.len())
    }
    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}
fn encode_bounded(value: &impl Serialize, limit: usize, error: &str) -> Result<Vec<u8>> {
    let mut writer = LimitedWriter {
        bytes: Vec::new(),
        limit,
    };
    serde_json::to_writer(&mut writer, value).map_err(|_| error.to_owned())?;
    Ok(writer.bytes)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::{Arc, Barrier};

    struct Lab(PathBuf);
    impl Lab {
        fn new() -> Self {
            Self(std::env::temp_dir().join(format!(
                "runtime-jobs-{}-{}",
                std::process::id(),
                random_token().unwrap()
            )))
        }
        fn queue(&self) -> Queue {
            Queue::open(&self.0).unwrap()
        }
    }
    impl Drop for Lab {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.0);
        }
    }
    fn enqueue(queue: &Queue, id: &str, now: u64) -> Job {
        queue
            .enqueue(
                id,
                "synthetic offline task",
                Some(serde_json::json!({"source":"fixture"})),
                now,
            )
            .unwrap()
    }
    fn progress() -> Progress {
        Progress {
            thread_id: Some("thread-fixture".into()),
            turn_id: Some("turn-fixture".into()),
            operation_ids: vec!["op-before-write".into()],
            tool_calls: 1,
        }
    }

    #[test]
    fn auth_resume_rejects_bound_execution_metadata_before_resuming_any_batch_member() {
        let lab = Lab::new();
        let queue = lab.queue();
        enqueue(&queue, "a-eligible", 1);
        let eligible = queue.claim_next(2).unwrap().unwrap();
        queue
            .finish(&eligible.lease, Outcome::needs_auth("login"), 3)
            .unwrap();
        enqueue(&queue, "z-bound", 4);
        let bound = queue.claim_next(5).unwrap().unwrap();
        queue.record_engine(&bound.lease, "antigravity", 6).unwrap();
        queue
            .finish(&bound.lease, Outcome::needs_auth("stamped elsewhere"), 7)
            .unwrap();
        assert!(
            queue
                .resume_needs_auth(&["a-eligible".into(), "z-bound".into()], 8)
                .is_err()
        );
        assert_eq!(queue.get("a-eligible").unwrap().status, Status::NeedsAuth);
        assert_eq!(
            queue.get("z-bound").unwrap().engine.as_deref(),
            Some("antigravity")
        );
    }

    #[test]
    fn explicit_job_claim_keeps_fifo_neighbors_and_engine_binding_cannot_change() {
        let lab = Lab::new();
        let queue = lab.queue();
        enqueue(&queue, "older", 10);
        enqueue(&queue, "selected", 11);
        assert!(queue.claim_job("missing", 12).unwrap().is_none());
        let selected = queue.claim_job("selected", 12).unwrap().unwrap();
        assert_eq!(selected.job.id, "selected");
        assert_eq!(queue.get("older").unwrap().status, Status::Pending);
        assert!(queue.claim_next(13).unwrap().is_none());
        queue
            .record_engine(&selected.lease, "antigravity", 13)
            .unwrap();
        let bytes = fs::read(queue.path("selected").unwrap()).unwrap();
        queue
            .record_engine(&selected.lease, "antigravity", 14)
            .unwrap();
        assert!(queue.record_engine(&selected.lease, "codex", 14).is_err());
        assert!(queue.record_engine(&selected.lease, "unknown", 14).is_err());
        assert_eq!(fs::read(queue.path("selected").unwrap()).unwrap(), bytes);
        queue
            .finish(&selected.lease, Outcome::completed("done"), 15)
            .unwrap();
        assert!(queue.claim_job("selected", 16).unwrap().is_none());
        assert_eq!(queue.claim_next(16).unwrap().unwrap().job.id, "older");
    }

    #[test]
    fn repeated_enqueue_is_semantic_idempotency_not_a_replay_or_a_timestamp_conflict() {
        let lab = Lab::new();
        let queue = lab.queue();
        let first = enqueue(&queue, "one", 10);
        assert_eq!(enqueue(&queue, "one", 999), first);
        assert!(
            queue
                .enqueue("one", "changed prompt", first.event.clone(), 10)
                .is_err()
        );
        assert!(
            queue
                .enqueue(
                    "one",
                    &first.prompt,
                    Some(serde_json::json!({"source":"changed"})),
                    10
                )
                .is_err()
        );
        let claimed = queue.claim_next(11).unwrap().unwrap();
        let done = queue
            .finish(&claimed.lease, Outcome::completed("done"), 12)
            .unwrap();
        assert_eq!(enqueue(&queue, "one", 1000), done);
        assert!(queue.claim_next(1001).unwrap().is_none());
        assert_eq!(queue.list(128).unwrap().len(), 1);
    }

    #[test]
    fn read_only_status_does_not_interrupt_live_work_and_cli_can_enqueue_during_a_turn() {
        let lab = Lab::new();
        let queue = lab.queue();
        enqueue(&queue, "one", 1);
        let claimed = queue.claim_next(2).unwrap().unwrap();
        let path = queue.path("one").unwrap();
        let before = fs::read(&path).unwrap();
        let concurrent = Queue::open(&lab.0).unwrap();
        assert_eq!(concurrent.get("one").unwrap().status, Status::Running);
        assert_eq!(concurrent.list(1).unwrap()[0].status, Status::Running);
        assert_eq!(fs::read(&path).unwrap(), before);
        enqueue(&concurrent, "two", 3);
        assert!(concurrent.claim_next(4).unwrap().is_none());
        queue
            .finish(&claimed.lease, Outcome::completed("done"), 5)
            .unwrap();
        assert_eq!(concurrent.claim_next(6).unwrap().unwrap().job.id, "two");
        assert!(
            serde_json::to_value(concurrent.get("two").unwrap())
                .unwrap()
                .get("run_token")
                .is_none()
        );
        assert!(!format!("{:?}", claimed.lease).contains(&claimed.lease.token));
    }

    #[test]
    fn concurrent_claims_have_exactly_one_running_model_task() {
        let lab = Arc::new(Lab::new());
        let queue = lab.queue();
        for n in 0..8 {
            enqueue(&queue, &format!("task-{n}"), 1);
        }
        let barrier = Arc::new(Barrier::new(8));
        let handles: Vec<_> = (0..8)
            .map(|_| {
                let lab = lab.clone();
                let barrier = barrier.clone();
                std::thread::spawn(move || {
                    let queue = lab.queue();
                    barrier.wait();
                    queue.claim_next(2).unwrap()
                })
            })
            .collect();
        let claimed: Vec<_> = handles
            .into_iter()
            .filter_map(|h| h.join().unwrap())
            .collect();
        assert_eq!(claimed.len(), 1);
        assert_eq!(
            queue
                .list(128)
                .unwrap()
                .iter()
                .filter(|j| j.status == Status::Running)
                .count(),
            1
        );
        assert_eq!(claimed[0].job.id, "task-0");
    }

    #[test]
    fn interrupted_claim_keeps_pre_tool_progress_and_is_never_pending_again() {
        let lab = Lab::new();
        let queue = lab.queue();
        enqueue(&queue, "one", 1);
        enqueue(&queue, "two", 2);
        let claimed = queue.claim_next(3).unwrap().unwrap();
        queue
            .update_progress(&claimed.lease, progress(), 4)
            .unwrap();
        drop(queue);
        let restarted = lab.queue();
        assert_eq!(restarted.get("one").unwrap().status, Status::Running);
        let recovered = restarted.recover_interrupted(5).unwrap();
        assert_eq!(recovered.len(), 1);
        assert_eq!(recovered[0].status, Status::Interrupted);
        assert_eq!(recovered[0].progress, progress());
        assert_eq!(enqueue(&restarted, "one", 100).status, Status::Interrupted);
        assert!(
            restarted
                .finish(&claimed.lease, Outcome::completed("stale"), 6)
                .is_err()
        );
        assert!(
            restarted
                .update_progress(&claimed.lease, Progress::default(), 6)
                .is_err()
        );
        assert!(restarted.gc_finished(&["one".into()]).is_err());
        assert!(restarted.recover_interrupted(6).unwrap().is_empty());
        assert_eq!(restarted.claim_next(6).unwrap().unwrap().job.id, "two");
    }

    #[test]
    fn progress_is_monotonic_and_foreign_or_expired_claim_cannot_finish_another_job() {
        let lab = Lab::new();
        let queue = lab.queue();
        enqueue(&queue, "one", 1);
        enqueue(&queue, "two", 2);
        let first = queue.claim_next(3).unwrap().unwrap();
        queue.update_progress(&first.lease, progress(), 4).unwrap();
        let newer = Progress {
            thread_id: Some("thread-fixture".into()),
            turn_id: Some("turn-next".into()),
            operation_ids: vec!["op-before-write".into(), "op-next".into()],
            tool_calls: 2,
        };
        queue.update_progress(&first.lease, newer, 5).unwrap();
        let job = queue
            .update_progress(&first.lease, Progress::default(), 6)
            .unwrap();
        assert_eq!(
            job.progress.operation_ids,
            vec!["op-before-write", "op-next"]
        );
        assert_eq!(job.progress.tool_calls, 2);
        assert_eq!(job.progress.turn_id.as_deref(), Some("turn-next"));
        assert!(
            queue
                .update_progress(
                    &first.lease,
                    Progress {
                        thread_id: Some("other-thread".into()),
                        ..Default::default()
                    },
                    7
                )
                .is_err()
        );
        queue
            .finish(&first.lease, Outcome::completed("first"), 7)
            .unwrap();
        let second = queue.claim_next(8).unwrap().unwrap();
        let forged = Lease {
            job_id: second.job.id.clone(),
            token: first.lease.token.clone(),
        };
        assert!(
            queue
                .finish(&forged, Outcome::completed("belongs elsewhere"), 9)
                .is_err()
        );
        assert!(
            queue
                .finish(&first.lease, Outcome::completed("again"), 9)
                .is_err()
        );
        assert_eq!(queue.get("two").unwrap().status, Status::Running);
        queue
            .finish(&second.lease, Outcome::needs_auth("login required"), 9)
            .unwrap();
        assert_eq!(enqueue(&queue, "two", 10).status, Status::NeedsAuth);
        assert!(queue.gc_finished(&["two".into()]).is_err());
    }

    #[test]
    fn invalid_transitions_text_and_input_limits_do_not_change_durable_claim() {
        let lab = Lab::new();
        let queue = lab.queue();
        assert!(queue.enqueue("../escape", "task", None, 1).is_err());
        assert!(queue.enqueue("empty", " \n", None, 1).is_err());
        assert!(
            queue
                .enqueue("large", &"x".repeat(MAX_PROMPT_BYTES + 1), None, 1)
                .is_err()
        );
        assert!(
            queue
                .enqueue(
                    "event",
                    "task",
                    Some(serde_json::json!({"large":"x".repeat(MAX_EVENT_BYTES)})),
                    1
                )
                .is_err()
        );
        enqueue(&queue, "one", 2);
        assert!(queue.claim_next(1).is_err());
        let claim = queue.claim_next(3).unwrap().unwrap();
        let before = fs::read(queue.path("one").unwrap()).unwrap();
        for status in [Status::Pending, Status::Running] {
            assert!(
                queue
                    .finish(
                        &claim.lease,
                        Outcome {
                            status,
                            text: Some("invalid".into()),
                            error: None
                        },
                        4
                    )
                    .is_err()
            );
        }
        assert!(
            queue
                .finish(
                    &claim.lease,
                    Outcome::completed("x".repeat(MAX_TEXT_BYTES + 1)),
                    4
                )
                .is_err()
        );
        assert!(
            queue
                .update_progress(
                    &claim.lease,
                    Progress {
                        operation_ids: vec!["bad/id".into()],
                        ..Default::default()
                    },
                    4
                )
                .is_err()
        );
        assert_eq!(fs::read(queue.path("one").unwrap()).unwrap(), before);
        queue
            .finish(&claim.lease, Outcome::failed("synthetic failure"), 4)
            .unwrap();
    }

    #[test]
    fn count_quota_requires_explicit_terminal_gc_and_never_discards_pending() {
        let lab = Lab::new();
        let queue = lab.queue();
        for n in 0..MAX_JOBS {
            enqueue(&queue, &format!("job-{n:03}"), 1);
        }
        assert!(
            queue
                .enqueue("over", "task", None, 2)
                .unwrap_err()
                .contains("quota")
        );
        assert_eq!(queue.list(128).unwrap().len(), 128);
        let claim = queue.claim_next(2).unwrap().unwrap();
        queue
            .finish(&claim.lease, Outcome::completed("done"), 3)
            .unwrap();
        assert!(
            queue
                .gc_finished(&["job-000".into(), "job-001".into()])
                .is_err()
        );
        assert_eq!(
            queue.get("job-000").unwrap().status,
            Status::Completed,
            "GC validates the entire request before deleting anything"
        );
        assert_eq!(queue.gc_finished(&["job-000".into()]).unwrap(), 1);
        enqueue(&queue, "replacement", 4);
        assert_eq!(queue.list(128).unwrap().len(), 128);
        assert!(queue.list(0).is_err());
        assert!(queue.list(129).is_err());
    }

    #[test]
    fn disk_budget_reserves_finish_headroom_including_atomic_temporary_copy() {
        let lab = Lab::new();
        let queue = lab.queue();
        let mut completed = Vec::new();
        // JSON escapes multiply this result's wire bytes, exercising disk quota
        // independently of the 128-record quota and 64 KiB decoded text budget.
        let text = "\u{0001}".repeat(MAX_TEXT_BYTES);
        for n in 0..MAX_JOBS {
            let id = format!("disk-{n:03}");
            match queue.enqueue(&id, "synthetic offline task", None, 1) {
                Ok(_) => {}
                Err(error) => {
                    assert!(error.contains("disk quota"));
                    break;
                }
            }
            let claim = queue.claim_next(2).unwrap().unwrap();
            queue
                .finish(&claim.lease, Outcome::completed(text.clone()), 3)
                .unwrap();
            completed.push(id);
        }
        assert!(!completed.is_empty() && completed.len() < MAX_JOBS);
        assert_eq!(
            queue
                .get(completed.last().unwrap())
                .unwrap()
                .text
                .as_deref(),
            Some(text.as_str())
        );
        let bytes: u64 = fs::read_dir(&queue.root)
            .unwrap()
            .map(|e| e.unwrap().metadata().unwrap().len())
            .sum();
        assert!(bytes <= MAX_DISK_BYTES);
        queue.gc_finished(&[completed[0].clone()]).unwrap();
        queue.enqueue("after-gc", "task", None, 4).unwrap();
    }

    #[test]
    fn private_files_orphan_cleanup_and_corruption_fail_closed() {
        use std::os::unix::fs::symlink;
        let lab = Lab::new();
        let queue = lab.queue();
        enqueue(&queue, "one", 1);
        let temp = queue
            .root
            .join(format!(".tmp-{}.json", random_token().unwrap()));
        let mut file = OpenOptions::new()
            .write(true)
            .create_new(true)
            .mode(0o600)
            .open(&temp)
            .unwrap();
        file.write_all(b"partial interrupted staging bytes")
            .unwrap();
        drop(file);
        assert_eq!(queue.list(1).unwrap().len(), 1);
        assert!(
            temp.exists(),
            "read-only status must not clean or recover anything"
        );
        enqueue(&queue, "two", 2);
        assert!(!temp.exists());
        let path = queue.path("one").unwrap();
        fs::set_permissions(&path, fs::Permissions::from_mode(0o644)).unwrap();
        assert!(queue.get("one").is_err());
        fs::set_permissions(&path, fs::Permissions::from_mode(0o600)).unwrap();
        let before = fs::read(&path).unwrap();
        fs::write(&path, b"invalid JSON").unwrap();
        assert!(queue.claim_next(3).is_err());
        fs::write(&path, before).unwrap();
        fs::remove_file(&path).unwrap();
        symlink(queue.path("two").unwrap(), &path).unwrap();
        assert!(queue.get("one").is_err());
    }
    #[test]
    fn same_id_reuse_cannot_revive_an_old_lease_and_explicit_interrupt_keeps_reason() {
        let lab = Lab::new();
        let queue = lab.queue();
        enqueue(&queue, "one", 1);
        let old = queue.claim_next(2).unwrap().unwrap();
        queue
            .finish(&old.lease, Outcome::completed("old turn"), 3)
            .unwrap();
        queue.gc_finished(&["one".into()]).unwrap();
        queue.enqueue("one", "new explicit task", None, 4).unwrap();
        let current = queue.claim_next(5).unwrap().unwrap();
        assert_ne!(old.lease.token, current.lease.token);
        assert!(
            queue
                .finish(&old.lease, Outcome::completed("stale result"), 6)
                .is_err()
        );
        let interrupted = queue
            .finish(
                &current.lease,
                Outcome {
                    status: Status::Interrupted,
                    text: Some("partial model output".into()),
                    error: Some("model transport disconnected after a tool was dispatched".into()),
                },
                6,
            )
            .unwrap();
        assert_eq!(interrupted.status, Status::Interrupted);
        assert_eq!(
            interrupted.error.as_deref(),
            Some("model transport disconnected after a tool was dispatched")
        );
        assert_eq!(interrupted.text.as_deref(), Some("partial model output"));
        assert_eq!(
            queue
                .enqueue("one", "new explicit task", None, 999)
                .unwrap(),
            interrupted
        );
        assert!(queue.claim_next(1000).unwrap().is_none());
        assert!(queue.gc_finished(&["one".into()]).is_err());
    }
    #[test]
    fn explicit_auth_resume_keeps_identity_and_invalidates_the_previous_claim() {
        let lab = Lab::new();
        let queue = lab.queue();
        let original = enqueue(&queue, "auth", 1);
        let old = queue.claim_next(2).unwrap().unwrap();
        queue
            .finish(
                &old.lease,
                Outcome::needs_auth("subscription login needed before any model turn"),
                3,
            )
            .unwrap();
        assert_eq!(queue.resume_needs_auth(&["auth".into()], 4).unwrap(), 1);
        let resumed = queue.get("auth").unwrap();
        assert_eq!(resumed.status, Status::Pending);
        assert_eq!(resumed.created_at, original.created_at);
        assert_eq!(resumed.prompt, original.prompt);
        assert_eq!(resumed.event, original.event);
        assert_eq!(resumed.started_at, None);
        assert_eq!(resumed.finished_at, None);
        assert_eq!(resumed.error, None);
        assert!(
            queue
                .finish(&old.lease, Outcome::completed("stale"), 5)
                .is_err()
        );
        let new = queue.claim_next(5).unwrap().unwrap();
        assert_ne!(old.lease.token, new.lease.token);
        assert!(
            queue
                .finish(&old.lease, Outcome::completed("stale"), 6)
                .is_err()
        );
        queue
            .finish(
                &new.lease,
                Outcome::completed("fresh model turn after human resume"),
                6,
            )
            .unwrap();
    }
    #[test]
    fn auth_resume_refuses_execution_evidence_and_other_states_before_modifying_any_job() {
        let lab = Lab::new();
        let queue = lab.queue();
        for (id, status, evidence) in [
            ("eligible", Status::NeedsAuth, false),
            ("has-tools", Status::NeedsAuth, true),
            ("interrupted", Status::Interrupted, false),
            ("failed", Status::Failed, false),
        ] {
            enqueue(&queue, id, 1);
            let claim = queue.claim_next(2).unwrap().unwrap();
            if evidence {
                queue.update_progress(&claim.lease, progress(), 3).unwrap();
            }
            queue
                .finish(
                    &claim.lease,
                    Outcome {
                        status,
                        text: None,
                        error: Some("synthetic reason".into()),
                    },
                    4,
                )
                .unwrap();
        }
        for (id, evidence) in [
            (
                "only-thread",
                Progress {
                    thread_id: Some("thread".into()),
                    ..Default::default()
                },
            ),
            (
                "only-turn",
                Progress {
                    turn_id: Some("turn".into()),
                    ..Default::default()
                },
            ),
            (
                "only-operation",
                Progress {
                    operation_ids: vec!["operation".into()],
                    ..Default::default()
                },
            ),
            (
                "only-tool-count",
                Progress {
                    tool_calls: 1,
                    ..Default::default()
                },
            ),
        ] {
            enqueue(&queue, id, 1);
            let claim = queue.claim_next(2).unwrap().unwrap();
            queue.update_progress(&claim.lease, evidence, 3).unwrap();
            queue
                .finish(
                    &claim.lease,
                    Outcome::needs_auth("model login expired after execution evidence"),
                    4,
                )
                .unwrap();
            assert!(queue.resume_needs_auth(&[id.into()], 5).is_err(), "{id}");
        }
        for rejected in ["has-tools", "interrupted", "failed"] {
            assert!(
                queue
                    .resume_needs_auth(&["eligible".into(), rejected.into()], 5)
                    .is_err()
            );
            assert_eq!(queue.get("eligible").unwrap().status, Status::NeedsAuth);
        }
        assert!(
            queue
                .resume_needs_auth(&["eligible".into(), "eligible".into()], 5)
                .is_err()
        );
        assert!(queue.resume_needs_auth(&["eligible".into()], 3).is_err());
        assert_eq!(queue.get("has-tools").unwrap().progress, progress());
        assert_eq!(queue.resume_needs_auth(&["eligible".into()], 5).unwrap(), 1);
        assert!(queue.resume_needs_auth(&["eligible".into()], 6).is_err());
    }
    #[test]
    fn cancel_validates_the_whole_set_and_never_requeues_or_discards_executed_jobs() {
        let lab = Lab::new();
        let queue = lab.queue();
        enqueue(&queue, "auth", 1);
        let auth = queue.claim_next(2).unwrap().unwrap();
        queue
            .finish(
                &auth.lease,
                Outcome::needs_auth("login needed before a turn"),
                3,
            )
            .unwrap();
        enqueue(&queue, "executed-auth", 1);
        let executed = queue.claim_next(2).unwrap().unwrap();
        queue
            .update_progress(
                &executed.lease,
                Progress {
                    tool_calls: 1,
                    ..Default::default()
                },
                3,
            )
            .unwrap();
        queue
            .finish(
                &executed.lease,
                Outcome::needs_auth("auth failed after a tool"),
                4,
            )
            .unwrap();
        enqueue(&queue, "interrupted", 1);
        let interrupted = queue.claim_next(2).unwrap().unwrap();
        queue
            .finish(
                &interrupted.lease,
                Outcome::interrupted("transport lost"),
                4,
            )
            .unwrap();
        enqueue(&queue, "pending", 5);
        for bad in ["executed-auth", "interrupted", "missing"] {
            assert!(
                queue
                    .cancel_pending(&["pending".into(), bad.into()], 6)
                    .is_err()
            );
            assert_eq!(queue.get("pending").unwrap().status, Status::Pending);
        }
        assert_eq!(
            queue
                .cancel_pending(&["pending".into(), "auth".into()], 6)
                .unwrap(),
            2
        );
        let pending = queue.get("pending").unwrap();
        let canceled_auth = queue.get("auth").unwrap();
        assert_eq!(pending.status, Status::Canceled);
        assert_eq!(pending.finished_at, Some(6));
        assert_eq!(pending.started_at, None);
        assert_eq!(pending.error.as_deref(), Some(CANCELED_MARKER));
        assert_eq!(canceled_auth.status, Status::Canceled);
        assert_eq!(canceled_auth.started_at, Some(2));
        assert!(
            queue
                .resume_needs_auth(&["pending".into(), "auth".into()], 7)
                .is_err()
        );
        assert!(
            queue
                .finish(&auth.lease, Outcome::completed("late"), 7)
                .is_err()
        );
        assert!(queue.claim_next(7).unwrap().is_none());
        assert!(queue.recover_interrupted(7).unwrap().is_empty());
        assert_eq!(enqueue(&queue, "pending", 999).status, Status::Canceled);
        assert_eq!(
            queue
                .gc_finished(&["pending".into(), "auth".into()])
                .unwrap(),
            2
        );
    }
    #[test]
    fn concurrent_cancel_and_claim_have_one_winner_without_preempting_running_work() {
        let lab = Arc::new(Lab::new());
        let queue = lab.queue();
        enqueue(&queue, "one", 1);
        let barrier = Arc::new(Barrier::new(2));
        let claim_thread = {
            let lab = lab.clone();
            let barrier = barrier.clone();
            std::thread::spawn(move || {
                let queue = lab.queue();
                barrier.wait();
                queue.claim_next(2).unwrap()
            })
        };
        let cancel_thread = {
            let lab = lab.clone();
            let barrier = barrier.clone();
            std::thread::spawn(move || {
                let queue = lab.queue();
                barrier.wait();
                queue.cancel_pending(&["one".into()], 2)
            })
        };
        let claimed = claim_thread.join().unwrap();
        let canceled = cancel_thread.join().unwrap();
        match claimed {
            None => {
                assert_eq!(canceled.unwrap(), 1);
                assert_eq!(queue.get("one").unwrap().status, Status::Canceled);
            }
            Some(claim) => {
                assert!(canceled.is_err());
                assert_eq!(queue.get("one").unwrap().status, Status::Running);
                assert!(
                    queue
                        .finish(
                            &claim.lease,
                            Outcome {
                                status: Status::Canceled,
                                text: None,
                                error: Some(CANCELED_MARKER.into())
                            },
                            3
                        )
                        .is_err()
                );
                queue
                    .finish(
                        &claim.lease,
                        Outcome::completed("claimed job finished normally"),
                        3,
                    )
                    .unwrap();
            }
        }
    }
}
