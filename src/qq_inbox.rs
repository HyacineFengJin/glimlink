// SPDX-License-Identifier: MIT
//! Bounded, account-local history of messages actually observed by the QQ worker.
//!
//! The caller supplies a sanitized message projection, never session credentials.
//! Message text and nested user JSON are otherwise preserved without key-name
//! filtering. Only the small index stays resident; message bodies are read on demand.
use base64::{Engine, engine::general_purpose::URL_SAFE_NO_PAD};
use runtime_core::{
    Platform, Result, Scope,
    resources::{FileLock, lock_private, open_regular, private_dir},
};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::{
    collections::{HashMap, HashSet, VecDeque},
    fs::{self, File, OpenOptions},
    io::{Read, Write},
    os::unix::fs::{OpenOptionsExt, PermissionsExt},
    path::{Path, PathBuf},
    sync::atomic::{AtomicU64, Ordering},
};

pub const MAX_EVENTS: usize = 512;
pub const MAX_EVENT_BYTES: usize = 64 * 1024;
/// The committed index plus retained event files; a transaction temporarily also
/// needs one index replacement and one event file, bounded by the limits below.
pub const MAX_DISK_BYTES: u64 = 8 * 1024 * 1024;
pub const MAX_DEDUPE_KEYS: usize = 4096;
const MAX_KEY_BYTES: usize = 256;
const MAX_EVENT_NODES: usize = 4096;
const MAX_RETAINED_NODES: usize = 65536;
const MAX_EVENT_DEPTH: usize = 32;
const MAX_EVENT_FILE_BYTES: u64 = (MAX_EVENT_BYTES + 2048) as u64;
const MAX_INDEX_BYTES: u64 = 3 * 1024 * 1024;
const MAX_DIRECTORY_ENTRIES: usize = MAX_EVENTS + 16;
const SCHEMA: u32 = 1;
static NEXT_TEMP: AtomicU64 = AtomicU64::new(0);

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Item {
    pub sequence: u64,
    pub key: String,
    pub event: Value,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Gap {
    pub reason: String,
    pub missing_from: Option<u64>,
    pub missing_through: Option<u64>,
    pub requested_after: u64,
    pub reset_required: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Retention {
    pub oldest_available: Option<u64>,
    pub newest_sequence: u64,
    pub retained_count: usize,
    pub retained_event_bytes: u64,
    pub committed_disk_bytes: u64,
    pub dropped_total: u64,
    pub max_events: usize,
    pub max_event_bytes: usize,
    pub max_disk_bytes: u64,
    pub dedupe_keys: usize,
    pub dedupe_oldest_sequence: Option<u64>,
    pub max_dedupe_keys: usize,
    pub history: String,
    pub dedupe_policy: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Page {
    pub scope: Scope,
    pub items: Vec<Item>,
    pub next_cursor: u64,
    pub has_more: bool,
    pub gap: Option<Gap>,
    pub retention: Retention,
}

#[derive(Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct EventFile {
    schema: u32,
    scope: Scope,
    sequence: u64,
    key: String,
    event: Value,
}

#[derive(Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Meta {
    sequence: u64,
    key: String,
    event_bytes: usize,
    disk_bytes: u64,
    nodes: usize,
}

#[derive(Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Seen {
    sequence: u64,
    key: String,
}

#[derive(Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Index {
    schema: u32,
    scope: Scope,
    next_sequence: u64,
    dropped_total: u64,
    entries: VecDeque<Meta>,
    dedupe: VecDeque<Seen>,
}

/// One worker owns this handle. The exclusive lock also rejects accidentally
/// opening a second writer or reading a half-finished transaction in-process.
pub struct Inbox {
    directory: PathBuf,
    index: Index,
    index_bytes: u64,
    seen: HashSet<String>,
    poisoned: bool,
    _lock: FileLock,
}

impl Inbox {
    pub fn open(state: &Path, scope: &Scope) -> Result<Self> {
        scope.validate()?;
        if scope.platform != Platform::Qq || scope.account == "public" {
            return Err("QQ inbox requires a private QQ account scope".into());
        }
        private_dir(state)?;
        let state = fs::canonicalize(state).map_err(|e| e.to_string())?;
        let root = state.join("qq-inbox");
        private_dir(&root)?;
        // Scope aliases are arbitrary valid UTF-8; never interpolate one as a path.
        let directory = root.join(URL_SAFE_NO_PAD.encode(scope.account.as_bytes()));
        private_dir(&directory)?;
        let lock = lock_private(&directory.join("inbox.lock"))?;
        private_file(&directory.join("inbox.lock"), 0)?;
        let path = directory.join("index.json");
        let index_present = match fs::symlink_metadata(&path) {
            Ok(_) => true,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => false,
            Err(e) => return Err(e.to_string()),
        };
        let (index, index_bytes) = if index_present {
            let bytes = read_private(&path, MAX_INDEX_BYTES)?;
            let index: Index =
                serde_json::from_slice(&bytes).map_err(|_| "invalid QQ inbox index".to_string())?;
            validate_index(&index, scope, bytes.len() as u64)?;
            (index, bytes.len() as u64)
        } else {
            let index = Index {
                schema: SCHEMA,
                scope: scope.clone(),
                next_sequence: 1,
                dropped_total: 0,
                entries: VecDeque::new(),
                dedupe: VecDeque::new(),
            };
            (index, 0)
        };
        let seen = index.dedupe.iter().map(|v| v.key.clone()).collect();
        let mut inbox = Self {
            directory,
            index,
            index_bytes,
            seen,
            poisoned: false,
            _lock: lock,
        };
        inbox.recover(index_present)?;
        if !index_present {
            let encoded = encode_index(&inbox.index)?;
            atomic_write(&inbox.directory, "index.json", &encoded, true)?;
            inbox.index_bytes = encoded.len() as u64;
        }
        Ok(inbox)
    }

    /// Deduplication covers the most recent MAX_DEDUPE_KEYS distinct accepted
    /// keys, even after their message bodies have left the retention window.
    pub fn contains(&self, key: &str) -> bool {
        key.len() <= MAX_KEY_BYTES && self.seen.contains(key)
    }

    pub fn append(&mut self, key: &str, event: &Value) -> Result<bool> {
        self.ensure_usable()?;
        validate_key(key)?;
        if self.contains(key) {
            return Ok(false);
        }
        let nodes = event_nodes(event)?;
        let event_bytes = encode_bounded(event, MAX_EVENT_BYTES, "QQ inbox event")?;
        let sequence = self.index.next_sequence;
        let next_sequence = sequence
            .checked_add(1)
            .ok_or("QQ inbox sequence exhausted")?;
        let file = EventFile {
            schema: SCHEMA,
            scope: self.index.scope.clone(),
            sequence,
            key: key.into(),
            event: event.clone(),
        };
        let encoded = encode_bounded(
            &file,
            MAX_EVENT_FILE_BYTES as usize,
            "QQ inbox event envelope",
        )?;
        let mut next = self.index.clone();
        next.next_sequence = next_sequence;
        next.entries.push_back(Meta {
            sequence,
            key: key.into(),
            event_bytes: event_bytes.len(),
            disk_bytes: encoded.len() as u64,
            nodes,
        });
        next.dedupe.push_back(Seen {
            sequence,
            key: key.into(),
        });
        while next.dedupe.len() > MAX_DEDUPE_KEYS {
            next.dedupe.pop_front();
        }
        let mut retained_nodes: usize = next.entries.iter().map(|v| v.nodes).sum();
        while next.entries.len() > MAX_EVENTS || retained_nodes > MAX_RETAINED_NODES {
            let removed = next
                .entries
                .pop_front()
                .ok_or("QQ inbox retention invariant")?;
            retained_nodes -= removed.nodes;
            next.dropped_total += 1;
        }
        let mut index_encoded = encode_index(&next)?;
        let mut retained_bytes: u64 = next.entries.iter().map(|v| v.disk_bytes).sum();
        while retained_bytes + index_encoded.len() as u64 > MAX_DISK_BYTES {
            // Drop a contiguous prefix; local sequence numbers never get renumbered.
            let required = retained_bytes + index_encoded.len() as u64 - MAX_DISK_BYTES;
            let mut freed = 0;
            while freed < required && next.entries.len() > 1 {
                let removed = next
                    .entries
                    .pop_front()
                    .ok_or("QQ inbox retention invariant")?;
                freed += removed.disk_bytes;
                retained_bytes -= removed.disk_bytes;
                next.dropped_total += 1;
            }
            if freed == 0 {
                return Err("QQ inbox item cannot fit disk budget".into());
            }
            index_encoded = encode_index(&next)?;
        }
        validate_index(&next, &self.index.scope, index_encoded.len() as u64)?;
        // Commit order: new body, then authoritative index, then obsolete bodies.
        // On uncertain I/O, require reopen rather than risking reuse of a sequence.
        let commit = (|| {
            atomic_write(&self.directory, &event_name(sequence), &encoded, false)?;
            atomic_write(&self.directory, "index.json", &index_encoded, true)?;
            let oldest = next
                .entries
                .front()
                .map(|v| v.sequence)
                .unwrap_or(next.next_sequence);
            for meta in &self.index.entries {
                if meta.sequence < oldest {
                    fs::remove_file(self.directory.join(event_name(meta.sequence)))
                        .map_err(|e| e.to_string())?;
                }
            }
            sync_directory(&self.directory)
        })();
        if let Err(error) = commit {
            self.poisoned = true;
            return Err(format!("QQ inbox commit failed; reopen required: {error}"));
        }
        self.index = next;
        self.index_bytes = index_encoded.len() as u64;
        self.seen = self.index.dedupe.iter().map(|v| v.key.clone()).collect();
        Ok(true)
    }

    /// `None` begins at the earliest retained message, not the beginning of QQ
    /// account history. An explicit cursor before that window reports a gap.
    pub fn read(&self, after: Option<u64>, limit: usize) -> Result<Page> {
        self.ensure_usable()?;
        if !(1..=MAX_EVENTS).contains(&limit) {
            return Err("QQ inbox read limit must be 1..512".into());
        }
        let newest = self.index.next_sequence - 1;
        let oldest = self.index.entries.front().map(|v| v.sequence);
        let gap = match after {
            Some(cursor) if cursor > newest => Some(Gap {
                reason: "cursor_ahead".into(),
                missing_from: None,
                missing_through: None,
                requested_after: cursor,
                reset_required: true,
            }),
            Some(cursor) if oldest.is_some_and(|v| cursor < v - 1) => Some(Gap {
                reason: "retention".into(),
                missing_from: Some(cursor + 1),
                missing_through: oldest.map(|v| v - 1),
                requested_after: cursor,
                reset_required: false,
            }),
            _ => None,
        };
        let mut items = Vec::new();
        if !gap.as_ref().is_some_and(|g| g.reset_required) {
            for meta in &self.index.entries {
                if after.is_none_or(|cursor| meta.sequence > cursor) {
                    items.push(self.read_event(meta)?);
                    if items.len() == limit {
                        break;
                    }
                }
            }
        }
        let next_cursor = items
            .last()
            .map(|v| v.sequence)
            .unwrap_or_else(|| after.unwrap_or(newest).min(newest));
        let has_more = self.index.entries.iter().any(|v| v.sequence > next_cursor);
        Ok(Page {
            scope: self.index.scope.clone(),
            items,
            next_cursor,
            has_more,
            gap,
            retention: Retention {
                oldest_available: oldest,
                newest_sequence: newest,
                retained_count: self.index.entries.len(),
                retained_event_bytes: self
                    .index
                    .entries
                    .iter()
                    .map(|v| v.event_bytes as u64)
                    .sum(),
                committed_disk_bytes: self.index_bytes
                    + self.index.entries.iter().map(|v| v.disk_bytes).sum::<u64>(),
                dropped_total: self.index.dropped_total,
                max_events: MAX_EVENTS,
                max_event_bytes: MAX_EVENT_BYTES,
                max_disk_bytes: MAX_DISK_BYTES,
                dedupe_keys: self.index.dedupe.len(),
                dedupe_oldest_sequence: self.index.dedupe.front().map(|v| v.sequence),
                max_dedupe_keys: MAX_DEDUPE_KEYS,
                history: "runtime_observed_only".into(),
                dedupe_policy: "most_recent_distinct_keys; older replay may be accepted again"
                    .into(),
            },
        })
    }

    fn ensure_usable(&self) -> Result<()> {
        if self.poisoned {
            Err("QQ inbox requires reopen after a failed commit".into())
        } else {
            Ok(())
        }
    }

    fn read_event(&self, meta: &Meta) -> Result<Item> {
        let bytes = read_private(
            &self.directory.join(event_name(meta.sequence)),
            MAX_EVENT_FILE_BYTES,
        )?;
        if bytes.len() as u64 != meta.disk_bytes {
            return Err("QQ inbox event length differs from index".into());
        }
        let file = parse_event(&bytes, &self.index.scope, meta.sequence)?;
        if file.key != meta.key
            || event_nodes(&file.event)? != meta.nodes
            || serde_json::to_vec(&file.event)
                .map_err(|e| e.to_string())?
                .len()
                != meta.event_bytes
        {
            return Err("QQ inbox event differs from index".into());
        }
        Ok(Item {
            sequence: file.sequence,
            key: file.key,
            event: file.event,
        })
    }

    fn recover(&self, index_present: bool) -> Result<()> {
        let mut paths = Vec::new();
        for entry in fs::read_dir(&self.directory).map_err(|e| e.to_string())? {
            if paths.len() >= MAX_DIRECTORY_ENTRIES {
                return Err("QQ inbox recovery entry limit exceeded".into());
            }
            paths.push(entry.map_err(|e| e.to_string())?.path());
        }
        let expected: HashMap<u64, &Meta> =
            self.index.entries.iter().map(|v| (v.sequence, v)).collect();
        let mut found = HashSet::new();
        let mut cleanup = Vec::new();
        for path in paths {
            let name = path
                .file_name()
                .and_then(|v| v.to_str())
                .ok_or("invalid QQ inbox filename")?;
            if name == "inbox.lock" {
                private_file(&path, 0)?;
                continue;
            }
            if name == "index.json" {
                private_file(&path, MAX_INDEX_BYTES)?;
                continue;
            }
            if is_temp(name) {
                private_file(&path, MAX_INDEX_BYTES)?;
                cleanup.push(path);
                continue;
            }
            let sequence = parse_event_name(name).ok_or("unexpected file in QQ inbox")?;
            if !index_present {
                return Err("QQ inbox index missing while message files remain".into());
            }
            if let Some(meta) = expected.get(&sequence) {
                self.read_event(meta)?;
                found.insert(sequence);
            } else {
                let oldest = self
                    .index
                    .entries
                    .front()
                    .map(|v| v.sequence)
                    .unwrap_or(self.index.next_sequence);
                if sequence >= oldest && sequence != self.index.next_sequence {
                    return Err("unexpected QQ inbox orphan sequence".into());
                }
                // Only remove files that actually belong to this account/index.
                let bytes = read_private(&path, MAX_EVENT_FILE_BYTES)?;
                parse_event(&bytes, &self.index.scope, sequence)?;
                cleanup.push(path);
            }
        }
        if found.len() != expected.len() {
            return Err("QQ inbox retained message file is missing".into());
        }
        for path in cleanup {
            fs::remove_file(path).map_err(|e| e.to_string())?;
        }
        sync_directory(&self.directory)
    }
}

fn validate_key(key: &str) -> Result<()> {
    if key.is_empty() || key.len() > MAX_KEY_BYTES || key.chars().any(char::is_control) {
        Err("invalid QQ inbox deduplication key".into())
    } else {
        Ok(())
    }
}

fn event_nodes(event: &Value) -> Result<usize> {
    fn visit(value: &Value, depth: usize, nodes: &mut usize) -> Result<()> {
        *nodes += 1;
        if depth > MAX_EVENT_DEPTH || *nodes > MAX_EVENT_NODES {
            return Err("QQ inbox event exceeds structural limit".into());
        }
        match value {
            Value::Array(values) => {
                for value in values {
                    visit(value, depth + 1, nodes)?;
                }
            }
            Value::Object(values) => {
                for value in values.values() {
                    *nodes += 1; // Account for object keys as well as values.
                    visit(value, depth + 1, nodes)?;
                }
            }
            _ => {}
        }
        Ok(())
    }
    let mut nodes = 0;
    visit(event, 0, &mut nodes)?;
    Ok(nodes)
}

fn encode_index(index: &Index) -> Result<Vec<u8>> {
    encode_bounded(index, MAX_INDEX_BYTES as usize, "QQ inbox index")
}

fn encode_bounded<T: Serialize>(value: &T, limit: usize, label: &str) -> Result<Vec<u8>> {
    struct Bounded {
        bytes: Vec<u8>,
        limit: usize,
    }
    impl Write for Bounded {
        fn write(&mut self, bytes: &[u8]) -> std::io::Result<usize> {
            if bytes.len() > self.limit.saturating_sub(self.bytes.len()) {
                return Err(std::io::Error::other("byte limit exceeded"));
            }
            self.bytes.extend_from_slice(bytes);
            Ok(bytes.len())
        }
        fn flush(&mut self) -> std::io::Result<()> {
            Ok(())
        }
    }
    let mut writer = Bounded {
        bytes: Vec::new(),
        limit,
    };
    serde_json::to_writer(&mut writer, value)
        .map_err(|e| format!("{label} could not be encoded within {limit} bytes: {e}"))?;
    Ok(writer.bytes)
}

fn validate_index(index: &Index, scope: &Scope, index_bytes: u64) -> Result<()> {
    if index.schema != SCHEMA
        || &index.scope != scope
        || index.next_sequence == 0
        || index.entries.len() > MAX_EVENTS
        || index.dedupe.len() > MAX_DEDUPE_KEYS
    {
        return Err("invalid QQ inbox index scope or bounds".into());
    }
    let newest = index.next_sequence - 1;
    if index.dropped_total
        != newest
            .checked_sub(index.entries.len() as u64)
            .ok_or("invalid QQ inbox retained count")?
    {
        return Err("invalid QQ inbox retention sequence".into());
    }
    let expected_seen = newest.min(MAX_DEDUPE_KEYS as u64) as usize;
    if index.dedupe.len() != expected_seen {
        return Err("incomplete QQ inbox deduplication index".into());
    }
    let mut keys = HashMap::new();
    let seen_start = index.next_sequence - index.dedupe.len() as u64;
    for (offset, seen) in index.dedupe.iter().enumerate() {
        validate_key(&seen.key)?;
        if seen.sequence != seen_start + offset as u64
            || keys.insert(seen.key.as_str(), seen.sequence).is_some()
        {
            return Err("invalid QQ inbox deduplication sequence".into());
        }
    }
    let mut disk_bytes = index_bytes;
    let mut nodes = 0;
    for (offset, meta) in index.entries.iter().enumerate() {
        validate_key(&meta.key)?;
        if meta.sequence != index.dropped_total + offset as u64 + 1
            || keys.get(meta.key.as_str()).copied() != Some(meta.sequence)
            || meta.event_bytes > MAX_EVENT_BYTES
            || meta.disk_bytes > MAX_EVENT_FILE_BYTES
            || meta.disk_bytes < meta.event_bytes as u64
            || meta.nodes == 0
            || meta.nodes > MAX_EVENT_NODES
        {
            return Err("invalid QQ inbox event index".into());
        }
        disk_bytes += meta.disk_bytes;
        nodes += meta.nodes;
    }
    if disk_bytes > MAX_DISK_BYTES || nodes > MAX_RETAINED_NODES {
        return Err("QQ inbox exceeds retention budget".into());
    }
    Ok(())
}

fn parse_event(bytes: &[u8], scope: &Scope, sequence: u64) -> Result<EventFile> {
    let file: EventFile =
        serde_json::from_slice(bytes).map_err(|_| "invalid QQ inbox message file")?;
    if file.schema != SCHEMA || &file.scope != scope || file.sequence != sequence {
        return Err("QQ inbox message scope or sequence mismatch".into());
    }
    validate_key(&file.key)?;
    event_nodes(&file.event)?;
    if serde_json::to_vec(&file.event)
        .map_err(|e| e.to_string())?
        .len()
        > MAX_EVENT_BYTES
    {
        return Err("QQ inbox message exceeds byte limit".into());
    }
    Ok(file)
}

fn private_file(path: &Path, max: u64) -> Result<File> {
    let file = open_regular(path, max)?;
    if file
        .metadata()
        .map_err(|e| e.to_string())?
        .permissions()
        .mode()
        & 0o077
        != 0
    {
        return Err("QQ inbox file must be private (0600)".into());
    }
    Ok(file)
}

fn read_private(path: &Path, max: u64) -> Result<Vec<u8>> {
    let file = private_file(path, max)?;
    let mut bytes = Vec::new();
    file.take(max + 1)
        .read_to_end(&mut bytes)
        .map_err(|e| e.to_string())?;
    if bytes.len() as u64 > max {
        return Err("QQ inbox file grew past byte limit".into());
    }
    Ok(bytes)
}

fn event_name(sequence: u64) -> String {
    format!("{sequence:020}.json")
}

fn parse_event_name(name: &str) -> Option<u64> {
    let number = name.strip_suffix(".json")?;
    if number.len() != 20 || !number.bytes().all(|b| b.is_ascii_digit()) {
        return None;
    }
    number.parse::<u64>().ok().filter(|v| *v > 0)
}

fn is_temp(name: &str) -> bool {
    let Some(middle) = name
        .strip_prefix(".inbox-")
        .and_then(|v| v.strip_suffix(".tmp"))
    else {
        return false;
    };
    let mut parts = middle.split('-');
    matches!((parts.next(), parts.next(), parts.next()), (Some(a),Some(b),None) if !a.is_empty() && !b.is_empty() && a.bytes().all(|v|v.is_ascii_digit()) && b.bytes().all(|v|v.is_ascii_digit()))
}

fn atomic_write(directory: &Path, name: &str, bytes: &[u8], replace: bool) -> Result<()> {
    let temporary = directory.join(format!(
        ".inbox-{}-{}.tmp",
        std::process::id(),
        NEXT_TEMP.fetch_add(1, Ordering::Relaxed)
    ));
    let mut file = OpenOptions::new()
        .write(true)
        .create_new(true)
        .mode(0o600)
        .open(&temporary)
        .map_err(|e| e.to_string())?;
    file.write_all(bytes)
        .and_then(|_| file.sync_all())
        .map_err(|e| e.to_string())?;
    let destination = directory.join(name);
    if replace {
        fs::rename(&temporary, &destination).map_err(|e| e.to_string())?;
    } else {
        // link is an atomic no-clobber publish, including when a symlink exists.
        fs::hard_link(&temporary, &destination).map_err(|e| e.to_string())?;
        fs::remove_file(&temporary).map_err(|e| e.to_string())?;
    }
    sync_directory(directory)
}

fn sync_directory(directory: &Path) -> Result<()> {
    File::open(directory)
        .and_then(|f| f.sync_all())
        .map_err(|e| e.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    use std::os::unix::fs::symlink;

    struct Lab(PathBuf);
    impl Lab {
        fn new() -> Self {
            Self(std::env::temp_dir().join(format!(
                "qq-inbox-fixture-{}-{}",
                std::process::id(),
                NEXT_TEMP.fetch_add(1, Ordering::Relaxed)
            )))
        }
        fn scope(alias: &str) -> Scope {
            Scope {
                platform: Platform::Qq,
                account: alias.into(),
            }
        }
        fn inbox(&self, alias: &str) -> Inbox {
            Inbox::open(&self.0, &Self::scope(alias)).unwrap()
        }
    }
    impl Drop for Lab {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.0);
        }
    }

    #[test]
    fn restart_keeps_local_sequences_deduplication_and_user_json() {
        let lab = Lab::new();
        let body = json!({"text":"the words token and password are ordinary chat", "json":{"token":"user supplied content"}});
        let mut inbox = lab.inbox("alice");
        assert!(inbox.append("server-key-a", &body).unwrap());
        assert!(
            !inbox
                .append("server-key-a", &json!({"text":"different projection"}))
                .unwrap()
        );
        assert!(Inbox::open(&lab.0, &Lab::scope("alice")).is_err());
        drop(inbox);
        let mut inbox = lab.inbox("alice");
        assert!(inbox.contains("server-key-a"));
        assert!(!inbox.append("server-key-a", &body).unwrap());
        assert!(inbox.append("server-key-b", &json!("second")).unwrap());
        let page = inbox.read(None, 1).unwrap();
        assert_eq!(page.items[0].sequence, 1);
        assert_eq!(page.items[0].event, body);
        assert!(page.has_more);
        let page = inbox.read(Some(page.next_cursor), 512).unwrap();
        assert_eq!(page.items.len(), 1);
        assert_eq!(page.items[0].sequence, 2);
        assert_eq!(page.next_cursor, 2);
        assert!(!page.has_more);
        assert!(inbox.read(Some(2), 512).unwrap().items.is_empty());
        let ahead = inbox.read(Some(99), 1).unwrap();
        assert!(ahead.gap.unwrap().reset_required);
        assert_eq!(ahead.next_cursor, 2);
    }

    #[test]
    fn retention_reports_a_cursor_gap_without_renumbering_or_forgetting_recent_keys() {
        let lab = Lab::new();
        let mut inbox = lab.inbox("alice");
        for i in 0..MAX_EVENTS + 3 {
            inbox.append(&format!("key-{i}"), &json!({"i":i})).unwrap();
        }
        assert!(inbox.contains("key-0"));
        assert!(!inbox.append("key-0", &json!({"i":0})).unwrap());
        let page = inbox.read(Some(0), 512).unwrap();
        assert_eq!(page.items.len(), 512);
        assert_eq!(page.items[0].sequence, 4);
        assert_eq!(page.retention.dropped_total, 3);
        let gap = page.gap.unwrap();
        assert_eq!((gap.missing_from, gap.missing_through), (Some(1), Some(3)));
        assert_eq!(page.next_cursor, 515);
        assert!(inbox.read(None, 512).unwrap().gap.is_none());
        drop(inbox);
        let inbox = lab.inbox("alice");
        assert!(inbox.contains("key-0"));
        assert_eq!(inbox.read(None, 512).unwrap().items[0].sequence, 4);
    }

    #[test]
    fn bytes_and_value_nodes_bound_retention_and_reject_oversized_events() {
        let lab = Lab::new();
        let mut inbox = lab.inbox("alice");
        assert!(
            inbox
                .append("too-big", &json!("x".repeat(MAX_EVENT_BYTES)))
                .is_err()
        );
        assert!(
            inbox
                .append("too-many-nodes", &json!(vec![0; MAX_EVENT_NODES]))
                .is_err()
        );
        assert!(inbox.append("", &Value::Null).is_err());
        assert!(
            inbox
                .append(&"k".repeat(MAX_KEY_BYTES + 1), &Value::Null)
                .is_err()
        );
        let mut deep = Value::Null;
        for _ in 0..MAX_EVENT_DEPTH + 1 {
            deep = json!([deep]);
        }
        assert!(inbox.append("too-deep", &deep).is_err());
        assert!(!inbox.contains("too-big"));
        assert_eq!(inbox.read(None, 1).unwrap().next_cursor, 0);
        let body = json!({"text":"x".repeat(MAX_EVENT_BYTES - 32)});
        for i in 0..140 {
            inbox.append(&format!("large-{i}"), &body).unwrap();
        }
        let page = inbox.read(Some(0), 512).unwrap();
        assert!(page.retention.dropped_total > 0);
        assert!(page.retention.committed_disk_bytes <= MAX_DISK_BYTES);
        assert!(page.gap.is_some());
        let actual: u64 = fs::read_dir(&inbox.directory)
            .unwrap()
            .map(|entry| entry.unwrap().metadata().unwrap().len())
            .sum();
        assert!(actual <= MAX_DISK_BYTES);
    }

    #[test]
    fn account_aliases_are_isolated_and_foreign_index_or_symlink_is_rejected() {
        let lab = Lab::new();
        let mut alice = lab.inbox("../alice");
        alice.append("same-key", &json!("alice")).unwrap();
        let alice_dir = alice.directory.clone();
        let mut bob = lab.inbox("bob");
        bob.append("same-key", &json!("bob")).unwrap();
        let bob_dir = bob.directory.clone();
        assert_ne!(alice_dir, bob_dir);
        assert_eq!(bob.read(None, 512).unwrap().items[0].event, json!("bob"));
        drop(bob);
        fs::copy(alice_dir.join("index.json"), bob_dir.join("index.json")).unwrap();
        assert!(Inbox::open(&lab.0, &Lab::scope("bob")).is_err());
        fs::remove_file(bob_dir.join("index.json")).unwrap();
        symlink(alice_dir.join("index.json"), bob_dir.join("index.json")).unwrap();
        assert!(Inbox::open(&lab.0, &Lab::scope("bob")).is_err());
        assert_eq!(
            alice.read(None, 512).unwrap().items[0].event,
            json!("alice")
        );
        assert!(
            Inbox::open(
                &lab.0,
                &Scope {
                    platform: Platform::Bilibili,
                    account: "alice".into()
                }
            )
            .is_err()
        );
        assert!(Inbox::open(&lab.0, &Lab::scope("public")).is_err());
    }

    #[test]
    fn recovery_discards_uncommitted_body_without_advancing_cursor() {
        let lab = Lab::new();
        let mut inbox = lab.inbox("alice");
        inbox.append("committed", &json!("first")).unwrap();
        let directory = inbox.directory.clone();
        let orphan = EventFile {
            schema: SCHEMA,
            scope: Lab::scope("alice"),
            sequence: 2,
            key: "uncommitted".into(),
            event: json!("not committed"),
        };
        atomic_write(
            &directory,
            &event_name(2),
            &serde_json::to_vec(&orphan).unwrap(),
            false,
        )
        .unwrap();
        drop(inbox);
        let mut inbox = lab.inbox("alice");
        assert!(!inbox.contains("uncommitted"));
        assert!(!directory.join(event_name(2)).exists());
        assert_eq!(inbox.read(None, 512).unwrap().next_cursor, 1);
        inbox.append("retried", &json!("second")).unwrap();
        assert_eq!(inbox.read(Some(1), 512).unwrap().items[0].sequence, 2);
    }
}
