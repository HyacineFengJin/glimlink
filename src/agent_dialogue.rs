// SPDX-License-Identifier: MIT
//! Engine-independent, bounded recent dialogue. This is not the durable memory
//! or action ledger: dropping old turns never changes an Outbox operation.
use runtime_core::{
    Result,
    resources::{open_regular, private_dir},
};
use runtime_jobs::AgentContext;
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use std::{
    collections::BTreeSet,
    fs::{self, File, OpenOptions},
    io::{Read, Write},
    os::unix::fs::{OpenOptionsExt, PermissionsExt},
    path::{Path, PathBuf},
    sync::atomic::{AtomicU64, Ordering},
    time::{Duration, Instant},
};

pub const MAX_CONVERSATIONS: usize = 128;
/// Committed JSON budget. Atomic replacement temporarily needs one more file
/// of at most this size; abandoned replacements are cleaned under the lock.
pub const MAX_DISK_BYTES: usize = 2 * 1024 * 1024;
pub const MAX_HISTORY_BYTES: usize = 16 * 1024;
pub const MAX_TURNS: usize = 8;
pub const MAX_TEXT_BYTES: usize = 2048;
pub const MAX_OPERATION_IDS: usize = 8;
const MAX_DIRECTORY_ENTRIES: usize = 32;
const LOCK_WAIT: Duration = Duration::from_secs(2);
static NEXT_TEMP: AtomicU64 = AtomicU64::new(0);

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Turn {
    pub job_id: String,
    /// A Queue ID can be reused after explicit GC. Execution identity keeps
    /// that new task distinct from the old task's retained dialogue turn.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub execution_id: Option<String>,
    pub at: u64,
    pub status: String,
    pub user: String,
    pub assistant: Option<String>,
    pub operation_ids: Vec<String>,
    /// May exceed the retained IDs. The complete evidence stays in the job and
    /// Outbox, never in a model's recollection of this short transcript.
    pub operation_count: usize,
}
impl Turn {
    fn validate(&self) -> Result<()> {
        if !record_id(&self.job_id)
            || self.execution_id.as_ref().is_some_and(|id| !record_id(id))
            || self.status.is_empty()
            || self.status.len() > 32
            || !self
                .status
                .bytes()
                .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b"_-".contains(&b))
            || self.user.len() > MAX_TEXT_BYTES
            || self.user.contains('\0')
            || self
                .assistant
                .as_ref()
                .is_some_and(|s| s.len() > MAX_TEXT_BYTES || s.contains('\0'))
            || self.operation_ids.len() > MAX_OPERATION_IDS
            || self.operation_count < self.operation_ids.len()
            || self.operation_count > runtime_jobs::MAX_OPERATION_IDS
            || self.operation_ids.iter().any(|id| !record_id(id))
            || self.operation_ids.iter().collect::<BTreeSet<_>>().len() != self.operation_ids.len()
        {
            return Err("invalid bounded dialogue turn".into());
        }
        // A single retained turn must fit together with the History envelope,
        // even if control characters expand six-fold in JSON serialization.
        encode(self, MAX_HISTORY_BYTES - 128)?;
        Ok(())
    }
}

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct History {
    pub turns: Vec<Turn>,
    pub epoch: u64,
    /// At least one older turn was dropped to satisfy retention limits.
    pub truncated: bool,
}
impl History {
    fn validate(&self) -> Result<()> {
        if self.turns.len() > MAX_TURNS
            || self
                .turns
                .iter()
                .map(|turn| (&turn.job_id, turn.execution_id.as_deref()))
                .collect::<BTreeSet<_>>()
                .len()
                != self.turns.len()
        {
            return Err("invalid dialogue history bounds or duplicate job".into());
        }
        for turn in &self.turns {
            turn.validate()?;
        }
        encode(self, MAX_HISTORY_BYTES)?;
        Ok(())
    }
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Conversation {
    persona: String,
    conversation: String,
    history: History,
}
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Record {
    schema: u32,
    revision: u64,
    conversations: Vec<Conversation>,
}
impl Default for Record {
    fn default() -> Self {
        Self {
            schema: 1,
            revision: 0,
            conversations: Vec::new(),
        }
    }
}
impl Record {
    fn validate(&self) -> Result<()> {
        if self.schema != 1 || self.conversations.len() > MAX_CONVERSATIONS {
            return Err("invalid dialogue store schema or quota".into());
        }
        let mut keys = BTreeSet::new();
        for entry in &self.conversations {
            AgentContext {
                persona: entry.persona.clone(),
                agent: "main".into(),
                conversation: Some(entry.conversation.clone()),
            }
            .validate()?;
            if !keys.insert((&entry.persona, &entry.conversation)) {
                return Err("duplicate dialogue conversation".into());
            }
            entry.history.validate()?;
        }
        Ok(())
    }
    fn index(&self, ctx: &AgentContext, conversation: &str) -> Option<usize> {
        self.conversations
            .iter()
            .position(|entry| entry.persona == ctx.persona && entry.conversation == conversation)
    }
    fn ensure(&mut self, ctx: &AgentContext, conversation: &str) -> Result<usize> {
        if let Some(index) = self.index(ctx, conversation) {
            return Ok(index);
        }
        if self.conversations.len() >= MAX_CONVERSATIONS {
            return Err("dialogue conversation quota reached; no other history was removed".into());
        }
        self.conversations.push(Conversation {
            persona: ctx.persona.clone(),
            conversation: conversation.into(),
            history: History::default(),
        });
        Ok(self.conversations.len() - 1)
    }
    fn advance(&mut self) -> Result<()> {
        self.revision = self
            .revision
            .checked_add(1)
            .ok_or("dialogue store revision exhausted")?;
        Ok(())
    }
}

struct Transaction(File);
impl Drop for Transaction {
    fn drop(&mut self) {
        let _ = self.0.unlock();
    }
}

/// Only paths stay resident. A read/write transaction acquires a short lock,
/// reloads at most 2 MiB and releases it before any engine can be awaited.
#[derive(Clone)]
pub struct Store {
    state: PathBuf,
    root: PathBuf,
}
impl Store {
    /// Validate existing paths, without creating a directory or lock file.
    pub fn open(state: &Path) -> Result<Self> {
        check_directory(state)?;
        let root = state.join("agent-dialogue");
        check_directory(&root)?;
        Ok(Self {
            state: state.into(),
            root,
        })
    }

    pub fn history(&self, ctx: &AgentContext) -> Result<History> {
        ctx.validate()?;
        let Some(conversation) = ctx.conversation.as_deref() else {
            return Ok(History::default());
        };
        let Some(_lock) = self.transaction(false)? else {
            return Ok(History::default());
        };
        let record = self.read()?;
        Ok(record
            .index(ctx, conversation)
            .map(|index| record.conversations[index].history.clone())
            .unwrap_or_default())
    }

    /// Operator-only bounded conversation directory; never a model tool.
    pub fn conversations(&self) -> Result<Value> {
        let Some(_lock) = self.transaction(false)? else { return Ok(json!([])); };
        Ok(json!(self.read()?.conversations.iter().map(|c|json!({"persona":c.persona,"conversation":c.conversation,"turns":c.history.turns.len(),"truncated":c.history.truncated})).collect::<Vec<_>>()))
    }

    /// Idempotent for a retained (job ID, execution ID) with an identical turn. A different
    /// Agent in the same persona/conversation appends to the same transcript.
    /// For a model turn, use append_at_epoch with the captured context epoch.
    pub fn append(&self, ctx: &AgentContext, turn: Turn) -> Result<()> {
        self.append_inner(ctx, turn, None)
    }

    /// Atomically prevent a pre-reset model turn from restoring cleared history.
    pub fn append_at_epoch(
        &self,
        ctx: &AgentContext,
        turn: Turn,
        expected_epoch: u64,
    ) -> Result<()> {
        self.append_inner(ctx, turn, Some(expected_epoch))
    }

    fn append_inner(
        &self,
        ctx: &AgentContext,
        turn: Turn,
        expected_epoch: Option<u64>,
    ) -> Result<()> {
        ctx.validate()?;
        turn.validate()?;
        let Some(conversation) = ctx.conversation.as_deref() else {
            return Ok(());
        };
        let _lock = self
            .transaction(true)?
            .ok_or("dialogue write lock unavailable")?;
        self.clean_temporaries()?;
        let mut record = self.read()?;
        let index = record.ensure(ctx, conversation)?;
        let history = &mut record.conversations[index].history;
        if expected_epoch.is_some_and(|epoch| epoch != history.epoch) {
            return Err("dialogue was reset after this turn's context was captured".into());
        }
        if let Some(existing) = history
            .turns
            .iter()
            .find(|old| old.job_id == turn.job_id && old.execution_id == turn.execution_id)
        {
            return if existing == &turn {
                Ok(())
            } else {
                Err("dialogue execution already has a different saved turn".into())
            };
        }
        history.turns.push(turn);
        while history.turns.len() > MAX_TURNS || encode(history, MAX_HISTORY_BYTES).is_err() {
            history.turns.remove(0);
            history.truncated = true;
        }
        history.validate()?;
        record.advance()?;
        self.write(&record)
    }

    /// Clears this portable transcript, not engine rollouts or action evidence.
    pub fn reset(&self, ctx: &AgentContext) -> Result<()> {
        ctx.validate()?;
        let Some(conversation) = ctx.conversation.as_deref() else {
            return Ok(());
        };
        let _lock = self
            .transaction(true)?
            .ok_or("dialogue write lock unavailable")?;
        self.clean_temporaries()?;
        let mut record = self.read()?;
        let index = record.ensure(ctx, conversation)?;
        let epoch = record.conversations[index]
            .history
            .epoch
            .checked_add(1)
            .ok_or("dialogue epoch exhausted")?;
        record.conversations[index].history = History {
            epoch,
            ..History::default()
        };
        record.advance()?;
        self.write(&record)
    }

    /// Metadata only; no conversation names, prompts or answer bodies.
    pub fn status(&self) -> Result<Value> {
        let (record, bytes) = if let Some(_lock) = self.transaction(false)? {
            let record = self.read()?;
            let bytes = match fs::symlink_metadata(self.path()) {
                Ok(meta) => meta.len(),
                Err(error) if error.kind() == std::io::ErrorKind::NotFound => 0,
                Err(_) => return Err("cannot inspect dialogue store".into()),
            };
            (record, bytes)
        } else {
            (Record::default(), 0)
        };
        Ok(json!({
            "conversations":record.conversations.len(),
            "turns":record.conversations.iter().map(|entry|entry.history.turns.len()).sum::<usize>(),
            "truncated_conversations":record.conversations.iter().filter(|entry|entry.history.truncated).count(),
            "store_revision":record.revision,"committed_disk_bytes":bytes,
            "max_conversations":MAX_CONVERSATIONS,"max_disk_bytes":MAX_DISK_BYTES,
            "max_turns_per_conversation":MAX_TURNS,"max_history_bytes":MAX_HISTORY_BYTES,
            "bounded_recent_history":true,"action_ledger_changed":false
        }))
    }

    fn path(&self) -> PathBuf {
        self.root.join("state.json")
    }
    fn transaction(&self, create: bool) -> Result<Option<Transaction>> {
        check_directory(&self.state)?;
        let root_exists = check_directory(&self.root)?;
        if create {
            private_dir(&self.state)?;
            private_dir(&self.root)?;
        } else if !root_exists {
            return Ok(None);
        }
        let lock_path = self.root.join("store.lock");
        let opened = OpenOptions::new()
            .read(true)
            .write(true)
            .create(create)
            .truncate(false)
            .mode(0o600)
            .custom_flags(libc::O_NOFOLLOW | libc::O_NONBLOCK)
            .open(&lock_path);
        let file = match opened {
            Ok(file) => file,
            Err(error) if !create && error.kind() == std::io::ErrorKind::NotFound => {
                return match fs::symlink_metadata(self.path()) {
                    Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(None),
                    _ => Err("dialogue store exists without its transaction lock".into()),
                };
            }
            Err(_) => return Err("cannot open private dialogue transaction lock".into()),
        };
        let metadata = file
            .metadata()
            .map_err(|_| "cannot inspect dialogue lock")?;
        if !metadata.is_file() || metadata.permissions().mode() & 0o077 != 0 {
            return Err("dialogue lock must be a private regular file (0600)".into());
        }
        let deadline = Instant::now() + LOCK_WAIT;
        loop {
            match file.try_lock() {
                Ok(()) => return Ok(Some(Transaction(file))),
                Err(std::fs::TryLockError::WouldBlock) if Instant::now() < deadline => {
                    std::thread::sleep(Duration::from_millis(10));
                }
                Err(_) => return Err("dialogue store is busy or its lock is unavailable".into()),
            }
        }
    }
    fn read(&self) -> Result<Record> {
        check_directory(&self.state)?;
        check_directory(&self.root)?;
        match fs::symlink_metadata(self.path()) {
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                return Ok(Record::default());
            }
            Err(_) => return Err("cannot inspect dialogue state".into()),
            Ok(_) => (),
        }
        let file = open_regular(&self.path(), MAX_DISK_BYTES as u64)?;
        if file
            .metadata()
            .map_err(|_| "cannot inspect dialogue state")?
            .permissions()
            .mode()
            & 0o077
            != 0
        {
            return Err("dialogue state must be private (0600)".into());
        }
        let mut bytes = Vec::new();
        file.take(MAX_DISK_BYTES as u64 + 1)
            .read_to_end(&mut bytes)
            .map_err(|_| "cannot read dialogue state")?;
        if bytes.len() > MAX_DISK_BYTES {
            return Err("dialogue disk quota exceeded".into());
        }
        let record: Record = serde_json::from_slice(&bytes).map_err(|_| "invalid dialogue JSON")?;
        record.validate()?;
        Ok(record)
    }
    fn clean_temporaries(&self) -> Result<()> {
        let mut entries = 0;
        let mut temporary = Vec::new();
        for entry in fs::read_dir(&self.root).map_err(|_| "cannot inspect dialogue directory")? {
            entries += 1;
            if entries > MAX_DIRECTORY_ENTRIES {
                return Err("dialogue directory entry quota exceeded".into());
            }
            let entry = entry.map_err(|_| "cannot inspect dialogue directory entry")?;
            let name = entry.file_name();
            let name = name.to_str().ok_or("invalid dialogue directory entry")?;
            if name == "state.json" || name == "store.lock" {
                continue;
            }
            if !temp_name(name) {
                return Err("unexpected file in dialogue directory; nothing was removed".into());
            }
            let metadata = fs::symlink_metadata(entry.path())
                .map_err(|_| "cannot inspect abandoned dialogue replacement")?;
            if !metadata.is_file()
                || metadata.file_type().is_symlink()
                || metadata.permissions().mode() & 0o077 != 0
                || metadata.len() > MAX_DISK_BYTES as u64
            {
                return Err("invalid abandoned dialogue replacement".into());
            }
            temporary.push(entry.path());
        }
        if !temporary.is_empty() {
            for path in temporary {
                fs::remove_file(path).map_err(|_| "cannot clean dialogue replacement")?;
            }
            File::open(&self.root)
                .and_then(|file| file.sync_all())
                .map_err(|_| "cannot persist dialogue cleanup")?;
        }
        Ok(())
    }
    fn write(&self, record: &Record) -> Result<()> {
        record.validate()?;
        let bytes = encode(record, MAX_DISK_BYTES)?;
        let temporary = self.root.join(format!(
            ".dialogue-{}-{}.next",
            std::process::id(),
            NEXT_TEMP.fetch_add(1, Ordering::Relaxed)
        ));
        let result = (|| -> Result<()> {
            let mut file = OpenOptions::new()
                .write(true)
                .create_new(true)
                .mode(0o600)
                .custom_flags(libc::O_NOFOLLOW | libc::O_NONBLOCK)
                .open(&temporary)
                .map_err(|_| "cannot create dialogue replacement")?;
            file.write_all(&bytes)
                .and_then(|_| file.sync_all())
                .map_err(|_| "cannot persist dialogue replacement")?;
            fs::rename(&temporary, self.path())
                .map_err(|_| "cannot commit dialogue replacement")?;
            File::open(&self.root)
                .and_then(|file| file.sync_all())
                .map_err(|_| "cannot persist dialogue commit".into())
        })();
        if result.is_err() {
            let _ = fs::remove_file(temporary);
        }
        result
    }
}

fn check_directory(path: &Path) -> Result<bool> {
    match fs::symlink_metadata(path) {
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(false),
        Err(_) => Err("cannot inspect private dialogue directory".into()),
        Ok(metadata)
            if metadata.is_dir()
                && !metadata.file_type().is_symlink()
                && metadata.permissions().mode() & 0o077 == 0 =>
        {
            Ok(true)
        }
        Ok(_) => Err("dialogue directory must be a real private directory (0700)".into()),
    }
}
fn record_id(id: &str) -> bool {
    !id.is_empty()
        && id.len() <= 96
        && id
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || b"_-".contains(&byte))
}
fn temp_name(name: &str) -> bool {
    let Some(body) = name
        .strip_prefix(".dialogue-")
        .and_then(|s| s.strip_suffix(".next"))
    else {
        return false;
    };
    let Some((pid, nonce)) = body.split_once('-') else {
        return false;
    };
    !pid.is_empty()
        && !nonce.is_empty()
        && pid.bytes().all(|b| b.is_ascii_digit())
        && nonce.bytes().all(|b| b.is_ascii_digit())
}
struct BoundedWriter {
    bytes: Vec<u8>,
    limit: usize,
}
impl Write for BoundedWriter {
    fn write(&mut self, bytes: &[u8]) -> std::io::Result<usize> {
        if bytes.len() > self.limit.saturating_sub(self.bytes.len()) {
            return Err(std::io::Error::other("dialogue JSON quota exceeded"));
        }
        self.bytes.extend_from_slice(bytes);
        Ok(bytes.len())
    }
    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}
fn encode(value: &impl Serialize, limit: usize) -> Result<Vec<u8>> {
    let mut writer = BoundedWriter {
        bytes: Vec::new(),
        limit,
    };
    serde_json::to_writer(&mut writer, value)
        .map_err(|_| "dialogue JSON exceeds its byte quota")?;
    Ok(writer.bytes)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::{os::unix::fs::symlink, sync::Arc};

    struct Lab(PathBuf);
    impl Lab {
        fn new() -> Self {
            Self(std::env::temp_dir().join(format!(
                "runtime-dialogue-{}-{}",
                std::process::id(),
                NEXT_TEMP.fetch_add(1, Ordering::Relaxed)
            )))
        }
        fn store(&self) -> Store {
            Store::open(&self.0).unwrap()
        }
    }
    impl Drop for Lab {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.0);
        }
    }
    fn context(persona: &str, agent: &str, conversation: &str) -> AgentContext {
        AgentContext {
            persona: persona.into(),
            agent: agent.into(),
            conversation: Some(conversation.into()),
        }
    }
    fn turn(id: &str) -> Turn {
        Turn {
            job_id: id.into(),
            execution_id: None,
            at: 10,
            status: "completed".into(),
            user: "synthetic conversation text".into(),
            assistant: Some("synthetic model response".into()),
            operation_ids: Vec::new(),
            operation_count: 0,
        }
    }

    #[test]
    fn different_agents_share_exact_persona_and_conversation_only_across_restarts() {
        let lab = Lab::new();
        let writer = context("shared", "reader", "qq-owner-a-peer-1");
        let other_agent = context("shared", "speaker", "qq-owner-a-peer-1");
        lab.store().append(&writer, turn("first")).unwrap();
        let reopened = lab.store();
        assert_eq!(
            reopened.history(&other_agent).unwrap().turns,
            vec![turn("first")]
        );
        reopened.append(&other_agent, turn("second")).unwrap();
        assert_eq!(lab.store().history(&writer).unwrap().turns.len(), 2);
        for isolated in [
            context("other", "reader", "qq-owner-a-peer-1"),
            context("shared", "reader", "qq-owner-b-peer-1"),
            context("shared", "reader", "qq-owner-a-peer-2"),
            context("shared", "reader", "tieba-owner-a-peer-1"),
        ] {
            assert_eq!(reopened.history(&isolated).unwrap(), History::default());
        }
    }

    #[test]
    fn empty_reads_and_contextless_writes_do_not_create_state() {
        let lab = Lab::new();
        let store = lab.store();
        let ctx = context("main", "worker", "operator-session");
        assert_eq!(store.history(&ctx).unwrap(), History::default());
        assert_eq!(store.status().unwrap()["conversations"], 0);
        store
            .append(&AgentContext::default(), turn("ignored"))
            .unwrap();
        store.reset(&AgentContext::default()).unwrap();
        assert_eq!(
            store.history(&AgentContext::default()).unwrap(),
            History::default()
        );
        assert!(!lab.0.exists());
    }

    #[test]
    fn a_pre_reset_turn_cannot_restore_history_even_after_a_fresh_turn_is_saved() {
        let lab = Lab::new();
        let ctx = context("main", "worker", "reset-during-model-turn");
        let store = lab.store();
        let captured_epoch = store.history(&ctx).unwrap().epoch;
        store
            .append_at_epoch(&ctx, turn("seed"), captured_epoch)
            .unwrap();
        store.reset(&ctx).unwrap();
        let reset_bytes = fs::read(store.path()).unwrap();
        assert!(
            store
                .append_at_epoch(&ctx, turn("old-result"), captured_epoch)
                .is_err()
        );
        assert_eq!(fs::read(store.path()).unwrap(), reset_bytes);
        assert!(store.history(&ctx).unwrap().turns.is_empty());
        let fresh_epoch = store.history(&ctx).unwrap().epoch;
        store
            .append_at_epoch(&ctx, turn("fresh-result"), fresh_epoch)
            .unwrap();
        let fresh_bytes = fs::read(store.path()).unwrap();
        assert!(
            lab.store()
                .append_at_epoch(&ctx, turn("late-old-result"), captured_epoch)
                .is_err()
        );
        assert_eq!(fs::read(store.path()).unwrap(), fresh_bytes);
        assert_eq!(
            store.history(&ctx).unwrap().turns,
            vec![turn("fresh-result")]
        );
    }

    #[test]
    fn retained_job_is_idempotent_but_changed_snapshot_is_rejected_without_write() {
        let lab = Lab::new();
        let ctx = context("main", "worker", "session");
        let store = lab.store();
        let original = turn("same-job");
        store.append(&ctx, original.clone()).unwrap();
        let bytes = fs::read(store.path()).unwrap();
        store.append(&ctx, original.clone()).unwrap();
        assert_eq!(fs::read(store.path()).unwrap(), bytes);
        let mut changed = original;
        changed.assistant = Some("different snapshot".into());
        assert!(store.append(&ctx, changed).is_err());
        assert_eq!(fs::read(store.path()).unwrap(), bytes);
    }

    #[test]
    fn reused_queue_id_has_distinct_executions_and_legacy_json_keeps_none_identity() {
        let lab = Lab::new();
        let ctx = context("main", "worker", "session");
        let store = lab.store();
        let mut legacy_json = serde_json::to_value(turn("reused-job")).unwrap();
        legacy_json.as_object_mut().unwrap().remove("execution_id");
        let legacy: Turn = serde_json::from_value(legacy_json).unwrap();
        assert!(legacy.execution_id.is_none());
        store.append(&ctx, legacy.clone()).unwrap();
        let mut first = turn("reused-job");
        first.execution_id = Some("run-first".into());
        first.user = "first task after Queue GC".into();
        let mut second = turn("reused-job");
        second.execution_id = Some("run-second".into());
        second.user = "different task after another Queue GC".into();
        store.append(&ctx, first.clone()).unwrap();
        store.append(&ctx, second.clone()).unwrap();
        let before = fs::read(store.path()).unwrap();
        store.append(&ctx, first.clone()).unwrap();
        store.append(&ctx, legacy.clone()).unwrap();
        assert_eq!(fs::read(store.path()).unwrap(), before);
        let mut changed = first.clone();
        changed.assistant = Some("conflicting output for the same execution".into());
        assert!(store.append(&ctx, changed).is_err());
        let mut invalid = second.clone();
        invalid.execution_id = Some("../invalid".into());
        assert!(store.append(&ctx, invalid).is_err());
        assert_eq!(fs::read(store.path()).unwrap(), before);
        assert_eq!(
            lab.store().history(&ctx).unwrap().turns,
            vec![legacy.clone(), first, second]
        );
        assert!(
            History {
                turns: vec![legacy.clone(), legacy],
                ..History::default()
            }
            .validate()
            .is_err()
        );
    }

    #[test]
    fn turn_count_and_actual_encoded_json_budget_drop_oldest_and_report_truncation() {
        let lab = Lab::new();
        let ctx = context("main", "worker", "count-budget");
        let store = lab.store();
        for i in 0..MAX_TURNS + 3 {
            store.append(&ctx, turn(&format!("job-{i}"))).unwrap();
        }
        let history = store.history(&ctx).unwrap();
        assert_eq!(history.turns.len(), MAX_TURNS);
        assert_eq!(history.turns.first().unwrap().job_id, "job-3");
        assert!(history.truncated);
        let ctx = context("main", "worker", "json-budget");
        for i in 0..5 {
            let mut next = turn(&format!("escaped-{i}"));
            next.user = "\\".repeat(MAX_TEXT_BYTES);
            next.assistant = Some("\\".repeat(MAX_TEXT_BYTES));
            store.append(&ctx, next).unwrap();
        }
        let history = store.history(&ctx).unwrap();
        assert!(history.turns.len() < 5);
        assert!(history.truncated);
        assert_eq!(history.turns.last().unwrap().job_id, "escaped-4");
        assert!(serde_json::to_vec(&history).unwrap().len() <= MAX_HISTORY_BYTES);
    }

    #[test]
    fn reset_advances_only_selected_epoch_and_preserves_action_ledger_bytes() {
        let lab = Lab::new();
        let first = context("main", "worker", "first");
        let other = context("main", "speaker", "second");
        let store = lab.store();
        let mut attempted = turn("unfinished");
        attempted.status = "interrupted".into();
        attempted.assistant = None;
        attempted.operation_ids = vec!["undecided-action".into()];
        attempted.operation_count = 2;
        store.append(&first, attempted).unwrap();
        store.append(&other, turn("keep")).unwrap();
        let ledger = lab.0.join("outbox");
        private_dir(&ledger).unwrap();
        let path = ledger.join("undecided-action.json");
        fs::write(&path, b"synthetic unknown action evidence").unwrap();
        fs::set_permissions(&path, fs::Permissions::from_mode(0o600)).unwrap();
        let bytes = fs::read(&path).unwrap();
        store.reset(&first).unwrap();
        let cleared = store.history(&first).unwrap();
        assert!(cleared.turns.is_empty() && !cleared.truncated);
        assert_eq!(cleared.epoch, 1);
        store.reset(&first).unwrap();
        assert_eq!(store.history(&first).unwrap().epoch, 2);
        assert_eq!(store.history(&other).unwrap().turns, vec![turn("keep")]);
        assert_eq!(fs::read(path).unwrap(), bytes);
    }

    #[test]
    fn metadata_status_omits_conversation_and_text_and_conversation_quota_is_conservative() {
        let lab = Lab::new();
        let store = lab.store();
        for i in 0..MAX_CONVERSATIONS {
            store
                .append(
                    &context(
                        "private-persona-marker",
                        "worker",
                        &format!("private-session-{i}"),
                    ),
                    turn(&format!("job-{i}")),
                )
                .unwrap();
        }
        let before = fs::read(store.path()).unwrap();
        assert!(
            store
                .append(&context("main", "worker", "over-quota"), turn("new"))
                .is_err()
        );
        assert_eq!(fs::read(store.path()).unwrap(), before);
        let status = store.status().unwrap();
        assert_eq!(status["conversations"], MAX_CONVERSATIONS);
        let text = status.to_string();
        assert!(!text.contains("private-persona-marker"));
        assert!(!text.contains("private-session"));
        assert!(!text.contains("synthetic conversation text"));
        assert!(before.len() <= MAX_DISK_BYTES);
    }

    #[test]
    fn symlinks_permissions_directory_replacement_and_bad_records_fail_without_repair() {
        let lab = Lab::new();
        let store = lab.store();
        let ctx = context("main", "worker", "session");
        store.append(&ctx, turn("valid")).unwrap();
        let path = store.path();
        let before = fs::read(&path).unwrap();
        fs::set_permissions(&path, fs::Permissions::from_mode(0o644)).unwrap();
        assert!(store.history(&ctx).is_err());
        assert_eq!(
            fs::metadata(&path).unwrap().permissions().mode() & 0o777,
            0o644
        );
        fs::set_permissions(&path, fs::Permissions::from_mode(0o600)).unwrap();
        let saved = lab.0.join("outside.json");
        fs::rename(&path, &saved).unwrap();
        symlink(&saved, &path).unwrap();
        assert!(store.history(&ctx).is_err());
        assert_eq!(fs::read(&saved).unwrap(), before);
        fs::remove_file(&path).unwrap();
        fs::rename(&saved, &path).unwrap();
        let old_root = lab.0.join("old-dialogue");
        fs::rename(&store.root, &old_root).unwrap();
        symlink(&old_root, &store.root).unwrap();
        assert!(Store::open(&lab.0).is_err());
        assert!(store.history(&ctx).is_err());
        assert!(store.append(&ctx, turn("blocked")).is_err());
        assert_eq!(fs::read(old_root.join("state.json")).unwrap(), before);
    }

    #[test]
    fn malformed_input_and_oversized_encoded_turn_never_create_or_modify_history() {
        let lab = Lab::new();
        let store = lab.store();
        let ctx = context("main", "worker", "session");
        let mut oversized = turn("oversized");
        oversized.user = "\u{0001}".repeat(MAX_TEXT_BYTES);
        oversized.assistant = Some("\u{0001}".repeat(MAX_TEXT_BYTES));
        assert!(store.append(&ctx, oversized).is_err());
        assert!(!lab.0.exists());
        store.append(&ctx, turn("seed")).unwrap();
        let before = fs::read(store.path()).unwrap();
        let mut invalid = turn("bad");
        invalid.operation_ids = vec!["op".into()];
        invalid.operation_count = 0;
        assert!(store.append(&ctx, invalid).is_err());
        let mut invalid = turn("bad");
        invalid.operation_ids = vec!["same".into(); MAX_OPERATION_IDS + 1];
        invalid.operation_count = invalid.operation_ids.len();
        assert!(store.append(&ctx, invalid).is_err());
        assert_eq!(fs::read(store.path()).unwrap(), before);
        let mut record: Value = serde_json::from_slice(&before).unwrap();
        record["schema"] = json!(999);
        fs::write(store.path(), serde_json::to_vec(&record).unwrap()).unwrap();
        assert!(store.history(&ctx).is_err());
    }

    #[test]
    fn concurrent_agents_append_through_short_transactions_without_lost_turns() {
        let lab = Lab::new();
        let store = Arc::new(lab.store());
        let barrier = Arc::new(std::sync::Barrier::new(3));
        let threads = (0..2)
            .map(|worker| {
                let store = Arc::clone(&store);
                let barrier = Arc::clone(&barrier);
                std::thread::spawn(move || {
                    let ctx = context("main", &format!("agent-{worker}"), "same-session");
                    barrier.wait();
                    for i in 0..3 {
                        store
                            .append(&ctx, turn(&format!("worker-{worker}-{i}")))
                            .unwrap();
                    }
                })
            })
            .collect::<Vec<_>>();
        barrier.wait();
        for thread in threads {
            thread.join().unwrap();
        }
        let history = store
            .history(&context("main", "reader", "same-session"))
            .unwrap();
        assert_eq!(history.turns.len(), 6);
        assert!(!history.truncated);
        assert_eq!(
            history
                .turns
                .iter()
                .map(|turn| &turn.job_id)
                .collect::<BTreeSet<_>>()
                .len(),
            6
        );
    }

    #[test]
    fn abandoned_replacement_cleanup_is_bounded_and_does_not_delete_unknown_files() {
        let lab = Lab::new();
        let store = lab.store();
        let ctx = context("main", "worker", "session");
        store.append(&ctx, turn("first")).unwrap();
        let abandoned = store.root.join(".dialogue-42-0.next");
        fs::write(&abandoned, b"abandoned replacement").unwrap();
        fs::set_permissions(&abandoned, fs::Permissions::from_mode(0o600)).unwrap();
        store.append(&ctx, turn("second")).unwrap();
        assert!(!abandoned.exists());
        let unknown = store.root.join("operator-notes");
        fs::write(&unknown, b"must remain").unwrap();
        fs::set_permissions(&unknown, fs::Permissions::from_mode(0o600)).unwrap();
        assert!(store.append(&ctx, turn("blocked")).is_err());
        assert_eq!(fs::read(unknown).unwrap(), b"must remain");
        assert_eq!(store.history(&ctx).unwrap().turns.len(), 2);
    }
}
