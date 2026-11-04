// SPDX-License-Identifier: MIT
//! Host-side locators and account control state. Never enumerate these through AI resources.
use base64::{Engine, engine::general_purpose::URL_SAFE_NO_PAD};
use runtime_core::{
    Result, Scope,
    resources::{open_regular, private_dir},
};
use serde::{Deserialize, Serialize, de::DeserializeOwned};
use std::{
    cell::RefCell,
    collections::{BTreeMap, BTreeSet, VecDeque, hash_map::DefaultHasher},
    fs::{self, File, OpenOptions},
    hash::{Hash, Hasher},
    io::{Read, Write},
    os::unix::fs::{OpenOptionsExt, PermissionsExt},
    path::{Path, PathBuf},
    time::{SystemTime, UNIX_EPOCH},
};

const MAX_LOCATORS: usize = 2048;
const MAX_RECORD: u64 = 1024 * 1024;
const MAX_TOTAL_BYTES: u64 = 32 * 1024 * 1024;
const TASK_REF_TTL: u64 = 24 * 60 * 60;
const HIGH_COUNT: usize = MAX_LOCATORS * 7 / 8;
const LOW_COUNT: usize = MAX_LOCATORS * 3 / 4;
const HIGH_BYTES: u64 = MAX_TOTAL_BYTES * 7 / 8;
const LOW_BYTES: u64 = MAX_TOTAL_BYTES * 3 / 4;
const RECENT_TASK_REFS: usize = 256;
const CLOCK_SKEW: u64 = 300;

pub(crate) fn account_path(state: &Path, platform: &str, account: &str) -> Result<PathBuf> {
    if !matches!(platform, "bilibili" | "weixin" | "tieba" | "qq" | "github" | "x" | "pixiv" | "reddit" | "discord") {
        return Err("unsupported private account namespace".into());
    }
    let scope = Scope {
        platform: runtime_core::Platform::Local,
        account: account.into(),
    };
    scope.validate()?;
    Ok(state
        .join("accounts")
        .join(platform)
        .join(URL_SAFE_NO_PAD.encode(account.as_bytes())))
}
pub fn account_directory(state: &Path, platform: &str, account: &str) -> Result<PathBuf> {
    let path = account_path(state, platform, account)?;
    private_dir(&path)?;
    Ok(path)
}

#[derive(Serialize, Deserialize)]
struct Record<T> {
    scope: Scope,
    kind: String,
    value: T,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    created_at: Option<u64>,
}
fn now() -> Result<u64> {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|v| v.as_secs())
        .map_err(|_| "invalid reference clock".into())
}
fn expired(kind: &str, created: Option<u64>, current: u64) -> bool {
    kind.starts_with("task-") && created.is_some_and(|t| current.saturating_sub(t) >= TASK_REF_TTL)
}
fn check_time(kind: &str, created: Option<u64>, current: u64) -> Result<()> {
    if kind.starts_with("task-") && created.is_some_and(|t| t > current.saturating_add(CLOCK_SKEW))
    {
        return Err(
            "private task reference has an invalid future timestamp; reopen its source".into(),
        );
    }
    if expired(kind, created, current) {
        return Err("private task reference expired; reopen its source".into());
    }
    Ok(())
}
fn valid_id(value: &str) -> bool {
    value.len() == 32
        && value
            .bytes()
            .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
}
fn id() -> Result<String> {
    let mut bytes = [0; 16];
    File::open("/dev/urandom")
        .and_then(|mut f| f.read_exact(&mut bytes))
        .map_err(|e| e.to_string())?;
    Ok(bytes.iter().map(|b| format!("{b:02x}")).collect())
}
fn fingerprint(scope: &Scope, kind: &str, value: &serde_json::Value) -> Result<u64> {
    let mut hash = DefaultHasher::new();
    serde_json::to_vec(scope)
        .map_err(|e| e.to_string())?
        .hash(&mut hash);
    kind.hash(&mut hash);
    serde_json::to_vec(value)
        .map_err(|e| e.to_string())?
        .hash(&mut hash);
    Ok(hash.finish())
}
struct IndexedRecord {
    kind: String,
    created_at: Option<u64>,
    bytes: u64,
    fingerprint: u64,
}
#[derive(Default)]
struct LocatorIndex {
    count: usize,
    bytes: u64,
    entries: BTreeMap<String, IndexedRecord>,
    reusable: BTreeMap<u64, Vec<String>>,
    recent: VecDeque<String>,
}
impl LocatorIndex {
    fn forget(&mut self, id: &str) {
        if let Some(entry) = self.entries.remove(id) {
            self.count = self.count.saturating_sub(1);
            self.bytes = self.bytes.saturating_sub(entry.bytes);
            self.recent.retain(|recent| recent != id);
            if let Some(ids) = self.reusable.get_mut(&entry.fingerprint) {
                ids.retain(|existing| existing != id);
                if ids.is_empty() {
                    self.reusable.remove(&entry.fingerprint);
                }
            }
        }
    }
    fn remember(&mut self, id: String, entry: IndexedRecord) {
        self.reusable
            .entry(entry.fingerprint)
            .or_default()
            .push(id.clone());
        self.entries.insert(id, entry);
    }
    fn touch(&mut self, id: &str) {
        if !self
            .entries
            .get(id)
            .is_some_and(|entry| entry.kind.starts_with("task-"))
        {
            return;
        }
        self.recent.retain(|recent| recent != id);
        self.recent.push_back(id.into());
        while self.recent.len() > RECENT_TASK_REFS {
            self.recent.pop_front();
        }
    }
}

pub struct PrivateLocators {
    root: PathBuf,
    index: RefCell<Option<LocatorIndex>>,
}
pub struct LocatorBatch<'a> {
    store: &'a PrivateLocators,
    created: Vec<(Scope, String, String)>,
    committed: bool,
}
impl LocatorBatch<'_> {
    pub fn put<T: Serialize>(&mut self, scope: &Scope, kind: &str, value: &T) -> Result<String> {
        let id = self.store.put(scope, kind, value)?;
        self.created.push((scope.clone(), kind.into(), id.clone()));
        Ok(id)
    }
    pub fn commit(mut self) {
        self.committed = true;
    }
}
impl Drop for LocatorBatch<'_> {
    fn drop(&mut self) {
        if !self.committed {
            for (scope, kind, id) in &self.created {
                let _ = self.store.remove(scope, kind, id);
            }
        }
    }
}
impl PrivateLocators {
    pub fn open(state: &Path) -> Result<Self> {
        let root = state.join("locators");
        private_dir(&root)?;
        Ok(Self {
            root,
            index: RefCell::new(None),
        })
    }
    pub fn batch(&self) -> LocatorBatch<'_> {
        LocatorBatch {
            store: self,
            created: Vec::new(),
            committed: false,
        }
    }
    pub fn put<T: Serialize>(&self, scope: &Scope, kind: &str, value: &T) -> Result<String> {
        self.insert_value(
            scope,
            kind,
            serde_json::to_value(value).map_err(|e| e.to_string())?,
        )
    }
    /// A digest is only an index. Compare the complete owner/kind/value before
    /// reusing an ID; a hit never changes its object or renews its fixed TTL.
    pub fn put_reusable<T: Serialize>(
        &self,
        scope: &Scope,
        kind: &str,
        value: &T,
    ) -> Result<String> {
        scope.validate()?;
        if kind.len() > 64 {
            return Err("locator kind exceeds limit".into());
        }
        let value = serde_json::to_value(value).map_err(|e| e.to_string())?;
        self.ensure_index()?;
        let key = fingerprint(scope, kind, &value)?;
        let candidates = self
            .index
            .borrow()
            .as_ref()
            .unwrap()
            .reusable
            .get(&key)
            .cloned()
            .unwrap_or_default();
        let current = now()?;
        for candidate in candidates {
            let record: Record<serde_json::Value> = match self.read_record(&candidate) {
                Ok(record) => record,
                Err(error) => {
                    if fs::symlink_metadata(self.root.join(format!("{candidate}.json")))
                        .is_err_and(|e| e.kind() == std::io::ErrorKind::NotFound)
                    {
                        self.index.borrow_mut().as_mut().unwrap().forget(&candidate);
                        continue;
                    }
                    return Err(error);
                }
            };
            if record.scope != *scope || record.kind != kind || record.value != value {
                continue;
            }
            if check_time(&record.kind, record.created_at, current).is_err() {
                self.erase(&candidate)?;
            } else {
                self.index.borrow_mut().as_mut().unwrap().touch(&candidate);
                return Ok(candidate);
            }
        }
        self.insert_value(scope, kind, value)
    }
    fn read_record<T: DeserializeOwned>(&self, id: &str) -> Result<Record<T>> {
        if !valid_id(id) {
            return Err("invalid private asset reference".into());
        }
        let path = self.root.join(format!("{id}.json"));
        let file = open_regular(&path, MAX_RECORD).map_err(|error| {
            if fs::symlink_metadata(&path).is_err_and(|e| e.kind() == std::io::ErrorKind::NotFound)
            {
                "private reference is missing or evicted; reopen its source".to_string()
            } else {
                error
            }
        })?;
        if file
            .metadata()
            .map_err(|e| e.to_string())?
            .permissions()
            .mode()
            & 0o077
            != 0
        {
            return Err("private reference record must be private (0600)".into());
        }
        let mut bytes = Vec::new();
        file.take(MAX_RECORD + 1)
            .read_to_end(&mut bytes)
            .map_err(|e| e.to_string())?;
        if bytes.len() as u64 > MAX_RECORD {
            return Err("private reference record exceeds limit".into());
        }
        serde_json::from_slice(&bytes).map_err(|_| "invalid private asset record".into())
    }
    /// Runtime's exclusive state lock owns mutations. Build one small metadata
    /// index, then account for this instance's inserts/deletes without rescans.
    fn ensure_index(&self) -> Result<()> {
        if self.index.borrow().is_some() {
            return Ok(());
        }
        let mut index = LocatorIndex::default();
        for entry in fs::read_dir(&self.root).map_err(|e| e.to_string())? {
            let entry = entry.map_err(|e| e.to_string())?;
            let bytes = fs::symlink_metadata(entry.path())
                .map_err(|e| e.to_string())?
                .len();
            index.bytes = index
                .bytes
                .checked_add(bytes)
                .ok_or("private asset byte overflow")?;
            index.count += 1;
            if index.count > MAX_LOCATORS * 2 {
                return Err("private locator directory entry limit".into());
            }
            let name = entry.file_name();
            let Some(id) = name
                .to_str()
                .and_then(|n| n.strip_suffix(".json"))
                .filter(|id| valid_id(id))
            else {
                continue;
            };
            // Malformed/unrecognized files count against quota but are protected
            // from GC. Only parsed internal task records can be reclaimed.
            if let Ok(record) = self.read_record::<serde_json::Value>(id) {
                if record.scope.validate().is_err() || record.kind.len() > 64 {
                    continue;
                }
                let key = fingerprint(&record.scope, &record.kind, &record.value)?;
                index.remember(
                    id.into(),
                    IndexedRecord {
                        kind: record.kind,
                        created_at: record.created_at,
                        bytes,
                        fingerprint: key,
                    },
                );
            }
        }
        *self.index.borrow_mut() = Some(index);
        Ok(())
    }
    fn insert_value(&self, scope: &Scope, kind: &str, value: serde_json::Value) -> Result<String> {
        scope.validate()?;
        if kind.len() > 64 {
            return Err("locator kind exceeds limit".into());
        }
        let current = now()?;
        let created_at = kind.starts_with("task-").then_some(current);
        let bytes = serde_json::to_vec(&Record {
            scope: scope.clone(),
            kind: kind.into(),
            value: &value,
            created_at,
        })
        .map_err(|e| e.to_string())?;
        if bytes.len() as u64 > MAX_RECORD {
            return Err("private locator exceeds byte limit".into());
        }
        self.ensure_index()?;
        let mut protected = BTreeSet::new();
        let mut pending = vec![&value];
        while let Some(part) = pending.pop() {
            match part {
                serde_json::Value::String(id) if valid_id(id) => {
                    protected.insert(id.clone());
                }
                serde_json::Value::Array(items) => pending.extend(items),
                serde_json::Value::Object(items) => pending.extend(items.values()),
                _ => {}
            }
        }
        self.reclaim(bytes.len() as u64, &protected, current)?;
        let id = id()?;
        let path = self.root.join(format!("{id}.json"));
        let mut file = OpenOptions::new()
            .write(true)
            .create_new(true)
            .mode(0o600)
            .open(&path)
            .map_err(|e| e.to_string())?;
        if let Err(error) = file.write_all(&bytes).and_then(|_| file.sync_all()) {
            drop(file);
            let _ = fs::remove_file(path);
            return Err(error.to_string());
        }
        if let Err(error) = File::open(&self.root).and_then(|file| file.sync_all()) {
            drop(file);
            let _ = fs::remove_file(path);
            return Err(error.to_string());
        }
        let key = fingerprint(scope, kind, &value)?;
        let mut index = self.index.borrow_mut();
        let index = index.as_mut().unwrap();
        index.count += 1;
        index.bytes += bytes.len() as u64;
        index.remember(
            id.clone(),
            IndexedRecord {
                kind: kind.into(),
                created_at,
                bytes: bytes.len() as u64,
                fingerprint: key,
            },
        );
        index.touch(&id);
        Ok(id)
    }
    fn reclaim(&self, incoming: u64, protected: &BTreeSet<String>, current: u64) -> Result<()> {
        let mut index = self.index.borrow_mut();
        let index = index.as_mut().ok_or("locator index unavailable")?;
        if index.count.saturating_add(1) >= HIGH_COUNT
            || index.bytes.saturating_add(incoming) >= HIGH_BYTES
        {
            // A readable result may still be assembling targets/creators when
            // another insertion crosses the watermark in the same second.
            // Keep recently returned capabilities alive even before their
            // containing continuation has been written.
            let mut protected = protected.clone();
            protected.extend(index.recent.iter().cloned());
            let mut candidates = index
                .entries
                .iter()
                .filter_map(|(id, entry)| {
                    let created = entry.created_at?;
                    if !entry.kind.starts_with("task-") || protected.contains(id) {
                        return None;
                    }
                    Some((
                        check_time(&entry.kind, entry.created_at, current).is_ok(),
                        matches!(
                            entry.kind.as_str(),
                            "task-target" | "task-github-target" | "task-qq-chat"
                        ),
                        created,
                        id.clone(),
                    ))
                })
                .collect::<Vec<_>>();
            candidates.sort();
            let mut deleted = false;
            for (_, _, _, id) in candidates {
                if index.count.saturating_add(1) <= LOW_COUNT
                    && index.bytes.saturating_add(incoming) <= LOW_BYTES
                {
                    break;
                }
                fs::remove_file(self.root.join(format!("{id}.json"))).map_err(|e| e.to_string())?;
                index.forget(&id);
                deleted = true;
            }
            if deleted {
                File::open(&self.root)
                    .and_then(|f| f.sync_all())
                    .map_err(|e| e.to_string())?;
            }
        }
        if index.count.saturating_add(1) > MAX_LOCATORS {
            return Err(
                "private locator quota reached; no reclaimable internal task references remain"
                    .into(),
            );
        }
        if index.bytes.saturating_add(incoming) > MAX_TOTAL_BYTES {
            return Err(
                "private locator byte quota reached; protected legacy locators were kept".into(),
            );
        }
        Ok(())
    }
    fn erase(&self, id: &str) -> Result<()> {
        fs::remove_file(self.root.join(format!("{id}.json"))).map_err(|e| e.to_string())?;
        if let Some(index) = self.index.borrow_mut().as_mut() {
            index.forget(id);
        }
        File::open(&self.root)
            .and_then(|f| f.sync_all())
            .map_err(|e| e.to_string())
    }
    pub fn get<T: DeserializeOwned>(&self, scope: &Scope, kind: &str, id: &str) -> Result<T> {
        let record: Record<T> = self.read_record(id)?;
        if &record.scope != scope || record.kind != kind {
            return Err("private asset scope mismatch".into());
        }
        check_time(&record.kind, record.created_at, now()?)?;
        Ok(record.value)
    }
    pub fn scope_for(&self, kind: &str, id: &str) -> Result<Scope> {
        let record: Record<serde::de::IgnoredAny> = self.read_record(id)?;
        if record.kind != kind {
            return Err("private reference kind mismatch".into());
        }
        record.scope.validate()?;
        check_time(&record.kind, record.created_at, now()?)?;
        Ok(record.scope)
    }
    /// Routing may inspect ownership before choosing a platform adapter.
    /// Adapters still validate the reference kind before reading its value.
    pub fn scope_for_reference(&self, id: &str) -> Result<Scope> {
        let record: Record<serde::de::IgnoredAny> = self.read_record(id)?;
        record.scope.validate()?;
        check_time(&record.kind, record.created_at, now()?)?;
        Ok(record.scope)
    }
    pub fn remove(&self, scope: &Scope, kind: &str, id: &str) -> Result<()> {
        // Hosts and batch rollback may also remove expired references.
        let record: Record<serde_json::Value> = self.read_record(id)?;
        if &record.scope != scope || record.kind != kind {
            return Err("private asset scope mismatch".into());
        }
        self.erase(id)
    }
    pub fn list(
        &self,
        scope: &Scope,
        offset: usize,
        limit: usize,
    ) -> Result<Vec<serde_json::Value>> {
        if limit == 0 || limit > 128 {
            return Err("asset list limit must be 1..128".into());
        }
        let mut paths = Vec::new();
        for entry in fs::read_dir(&self.root).map_err(|e| e.to_string())? {
            let entry = entry.map_err(|e| e.to_string())?;
            if paths.len() >= MAX_LOCATORS {
                return Err("asset directory entry limit".into());
            }
            paths.push(entry.path());
        }
        paths.sort();
        let mut result = Vec::new();
        let mut matched = 0;
        let current = now()?;
        for path in paths {
            let Some(id) = path
                .file_stem()
                .and_then(|v| v.to_str())
                .filter(|v| valid_id(v))
            else {
                continue;
            };
            let record: Record<serde_json::Value> = self.read_record(id)?;
            if &record.scope != scope
                || check_time(&record.kind, record.created_at, current).is_err()
            {
                continue;
            }
            if matched >= offset {
                result.push(serde_json::json!({"asset":id,"kind":record.kind}));
                if result.len() == limit {
                    break;
                }
            }
            matched += 1;
        }
        Ok(result)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    fn root() -> PathBuf {
        let root = std::env::temp_dir().join(format!(
            "private-state-{}-{}",
            std::process::id(),
            id().unwrap()
        ));
        private_dir(&root).unwrap();
        root
    }
    fn scope(account: &str) -> Scope {
        Scope {
            platform: runtime_core::Platform::Tieba,
            account: account.into(),
        }
    }
    fn fixture(
        store: &PrivateLocators,
        id: &str,
        owner: &Scope,
        kind: &str,
        value: serde_json::Value,
        created_at: Option<u64>,
    ) {
        let mut file = OpenOptions::new()
            .write(true)
            .create_new(true)
            .mode(0o600)
            .open(store.root.join(format!("{id}.json")))
            .unwrap();
        let record = serde_json::json!({"scope":owner,"kind":kind,"value":value});
        let mut record = record.as_object().unwrap().clone();
        if let Some(created) = created_at {
            record.insert("created_at".into(), created.into());
        }
        file.write_all(&serde_json::to_vec(&record).unwrap())
            .unwrap();
    }
    #[test]
    fn account_paths_and_private_locators_do_not_cross_scopes() {
        let root = root();
        let path = account_directory(&root, "weixin", "../../another/账号").unwrap();
        assert_eq!(path.parent().unwrap(), root.join("accounts/weixin"));
        let store = PrivateLocators::open(&root).unwrap();
        let a = scope("a");
        let b = scope("b");
        let asset = store
            .put(
                &a,
                "tieba-media",
                &serde_json::json!({"url":"https://example.invalid/private"}),
            )
            .unwrap();
        assert!(
            store
                .get::<serde_json::Value>(&b, "tieba-media", &asset)
                .is_err()
        );
        assert!(
            store
                .get::<serde_json::Value>(&a, "weixin-media", &asset)
                .is_err()
        );
        assert!(
            store
                .get::<serde_json::Value>(&a, "tieba-media", "../../x")
                .is_err()
        );
        assert!(
            store
                .get::<serde_json::Value>(&a, "tieba-media", &asset)
                .unwrap()["url"]
                .as_str()
                .unwrap()
                .contains("private")
        );
        store.remove(&a, "tieba-media", &asset).unwrap();
        fs::remove_dir_all(root).unwrap();
    }
    #[test]
    fn reusable_refs_survive_restart_without_crossing_owner_kind_or_value() {
        let root = root();
        let owner = scope("a");
        let value = serde_json::json!({"kind":"thread","thread":"700"});
        let store = PrivateLocators::open(&root).unwrap();
        let first = store.put_reusable(&owner, "task-target", &value).unwrap();
        for _ in 0..20 {
            assert_eq!(
                store.put_reusable(&owner, "task-target", &value).unwrap(),
                first
            );
        }
        assert_eq!(fs::read_dir(&store.root).unwrap().count(), 1);
        assert_ne!(
            store
                .put_reusable(&scope("b"), "task-target", &value)
                .unwrap(),
            first
        );
        assert_ne!(
            store.put_reusable(&owner, "task-other", &value).unwrap(),
            first
        );
        assert_ne!(
            store
                .put_reusable(&owner, "task-target", &serde_json::json!({"thread":"701"}))
                .unwrap(),
            first
        );
        drop(store);
        let store = PrivateLocators::open(&root).unwrap();
        assert_eq!(
            store.put_reusable(&owner, "task-target", &value).unwrap(),
            first
        );
        assert_eq!(
            fs::metadata(store.root.join(format!("{first}.json")))
                .unwrap()
                .permissions()
                .mode()
                & 0o777,
            0o600
        );
        fs::remove_dir_all(root).unwrap();
    }
    #[test]
    fn expired_refs_fail_explicitly_and_legacy_refs_keep_their_behavior() {
        let root = root();
        let owner = scope("a");
        let store = PrivateLocators::open(&root).unwrap();
        let stale = "1".repeat(32);
        let legacy = "2".repeat(32);
        let future = "3".repeat(32);
        let value = serde_json::json!({"thread":"700"});
        fixture(
            &store,
            &stale,
            &owner,
            "task-target",
            value.clone(),
            Some(now().unwrap() - TASK_REF_TTL - 1),
        );
        fixture(
            &store,
            &legacy,
            &owner,
            "task-target",
            serde_json::json!({"thread":"701"}),
            None,
        );
        fixture(
            &store,
            &future,
            &owner,
            "task-target",
            value.clone(),
            Some(now().unwrap() + CLOCK_SKEW + 60),
        );
        assert!(
            store
                .get::<serde_json::Value>(&owner, "task-target", &stale)
                .unwrap_err()
                .contains("expired")
        );
        assert!(
            store
                .scope_for("task-target", &stale)
                .unwrap_err()
                .contains("expired")
        );
        assert!(
            store
                .get::<serde_json::Value>(&owner, "task-target", &future)
                .unwrap_err()
                .contains("future timestamp")
        );
        assert!(
            store
                .scope_for("task-target", &future)
                .unwrap_err()
                .contains("future timestamp")
        );
        assert!(
            store
                .get::<serde_json::Value>(&scope("b"), "task-target", &stale)
                .unwrap_err()
                .contains("scope mismatch")
        );
        assert_eq!(
            store
                .get::<serde_json::Value>(&owner, "task-target", &legacy)
                .unwrap()["thread"],
            "701"
        );
        let fresh = store.put_reusable(&owner, "task-target", &value).unwrap();
        assert_ne!(fresh, stale);
        assert_ne!(fresh, future);
        assert!(
            store
                .get::<serde_json::Value>(&owner, "task-target", &stale)
                .is_err()
        );
        assert_eq!(
            store
                .get::<serde_json::Value>(&owner, "task-target", &fresh)
                .unwrap(),
            value
        );
        fs::remove_dir_all(root).unwrap();
    }
    #[test]
    fn count_pressure_evicts_old_internal_caches_but_keeps_targets_and_legacy_media() {
        let root = root();
        let owner = scope("a");
        let store = PrivateLocators::open(&root).unwrap();
        let current = now().unwrap();
        for n in 1..=(HIGH_COUNT - 3) {
            fixture(
                &store,
                &format!("{n:032x}"),
                &owner,
                "task-list",
                serde_json::json!({"page":n}),
                Some(current - 100),
            );
        }
        let media = format!("{:032x}", HIGH_COUNT);
        let legacy_target = format!("{:032x}", HIGH_COUNT + 1);
        let active_target = format!("{:032x}", HIGH_COUNT + 2);
        fixture(
            &store,
            &media,
            &owner,
            "tieba-media",
            serde_json::json!({"url":"private"}),
            None,
        );
        fixture(
            &store,
            &legacy_target,
            &owner,
            "task-target",
            serde_json::json!({"thread":"700"}),
            None,
        );
        fixture(
            &store,
            &active_target,
            &owner,
            "task-target",
            serde_json::json!({"thread":"701"}),
            Some(current - 200),
        );
        let fresh = store
            .put(
                &owner,
                "task-list",
                &serde_json::json!({"target_ref":active_target}),
            )
            .unwrap();
        assert!(fs::read_dir(&store.root).unwrap().count() <= LOW_COUNT);
        assert!(
            store
                .get::<serde_json::Value>(&owner, "task-list", &format!("{:032x}", 1))
                .is_err()
        );
        assert!(
            store
                .get::<serde_json::Value>(&owner, "tieba-media", &media)
                .is_ok()
        );
        assert!(
            store
                .get::<serde_json::Value>(&owner, "task-target", &legacy_target)
                .is_ok()
        );
        assert!(
            store
                .get::<serde_json::Value>(&owner, "task-target", &active_target)
                .is_ok()
        );
        assert!(
            store
                .get::<serde_json::Value>(&owner, "task-list", &fresh)
                .is_ok()
        );
        fs::remove_dir_all(root).unwrap();
    }
    #[test]
    fn byte_pressure_reclaims_internal_caches_without_deleting_legacy_records() {
        let root = root();
        let owner = scope("a");
        let store = PrivateLocators::open(&root).unwrap();
        let current = now().unwrap();
        let payload = "x".repeat(MAX_RECORD as usize - 512);
        for n in 1..=29 {
            fixture(
                &store,
                &format!("{n:032x}"),
                &owner,
                "task-media-cache",
                serde_json::json!({"data":payload}),
                Some(current - 100),
            );
        }
        let legacy = format!("{:032x}", 100);
        fixture(
            &store,
            &legacy,
            &scope("b"),
            "bilibili-media",
            serde_json::json!({"url":"private"}),
            None,
        );
        store
            .put(&owner, "task-list", &serde_json::json!({"page":1}))
            .unwrap();
        let bytes: u64 = fs::read_dir(&store.root)
            .unwrap()
            .map(|e| e.unwrap().metadata().unwrap().len())
            .sum();
        assert!(bytes <= LOW_BYTES);
        assert!(
            store
                .get::<serde_json::Value>(&scope("b"), "bilibili-media", &legacy)
                .is_ok()
        );
        assert!(fs::read_dir(&store.root).unwrap().count() < 30);
        fs::remove_dir_all(root).unwrap();
    }
    #[test]
    fn recently_reused_target_survives_same_second_gc_while_a_result_is_assembling() {
        let root = root();
        let owner = scope("a");
        let store = PrivateLocators::open(&root).unwrap();
        let current = now().unwrap();
        for n in 1..=HIGH_COUNT {
            fixture(
                &store,
                &format!("{n:032x}"),
                &owner,
                "task-target",
                serde_json::json!({"thread":n.to_string()}),
                Some(current),
            );
        }
        let active = store
            .put_reusable(&owner, "task-target", &serde_json::json!({"thread":"1"}))
            .unwrap();
        assert_eq!(active, format!("{:032x}", 1));
        // This later creator has no explicit reference to the active target.
        let creator = store
            .put(&owner, "task-target", &serde_json::json!({"creator":42}))
            .unwrap();
        assert!(
            store
                .get::<serde_json::Value>(&owner, "task-target", &active)
                .is_ok()
        );
        assert!(
            store
                .get::<serde_json::Value>(&owner, "task-target", &format!("{:032x}", 2))
                .is_err()
        );
        assert!(
            store
                .get::<serde_json::Value>(&owner, "task-target", &creator)
                .is_ok()
        );
        assert!(fs::read_dir(&store.root).unwrap().count() <= LOW_COUNT);
        fs::remove_dir_all(root).unwrap();
    }
}
