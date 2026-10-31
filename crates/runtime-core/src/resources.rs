// SPDX-License-Identifier: MIT
//! Small disk-backed resource store. Model-visible IDs never contain paths or signed URLs.
use crate::{Result, Scope, content::ResourceRef};
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use std::fs::{self, File, OpenOptions};
use std::io::{Read, Write};
use std::os::unix::fs::{DirBuilderExt, OpenOptionsExt, PermissionsExt};
use std::path::{Path, PathBuf};
use std::time::{Duration, SystemTime};

const CACHE_METADATA_BYTES: u64 = 8192;
const RECENT_CACHE_RESOURCES: usize = 32;
const RECENT_CACHE_SECONDS: u64 = 120;
const MAX_MAINTENANCE_ENTRIES: usize = 16 * 1024;

/// Payload byte/count accounting follows Budget and ingest: bytes refer to
/// `.blob` payloads. Unknown files and invalid records are never GC candidates.
#[derive(Debug, Clone, Serialize)]
pub struct CacheMaintenance {
    pub inspected_resources: usize,
    pub removed_resources: usize,
    pub removed_bytes: u64,
    pub remaining_resources: usize,
    pub remaining_bytes: u64,
    pub protected_resources: usize,
    pub high_water_reached: bool,
    pub target_reached: bool,
}

struct CacheEntry {
    id: String,
    bytes: u64,
    modified: SystemTime,
}

#[derive(Debug, Clone, Copy)]
pub struct Budget {
    pub max_resource_bytes: u64,
    pub max_cache_bytes: u64,
    pub max_resources: usize,
}

impl Default for Budget {
    fn default() -> Self {
        Self {
            max_resource_bytes: 128 * 1024 * 1024,
            max_cache_bytes: 256 * 1024 * 1024,
            max_resources: 2048,
        }
    }
}

#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Record {
    scope: Scope,
    object_id: String,
    resource: ResourceRef,
}

pub struct ResourceStore {
    root: PathBuf,
    budget: Budget,
}

pub struct FileLock {
    file: File,
}

impl Drop for FileLock {
    fn drop(&mut self) {
        // Release explicitly: a concurrently forked child can briefly retain the
        // open file description even when it has CLOEXEC set.
        let _ = self.file.unlock();
    }
}

pub fn lock_private(path: &Path) -> Result<FileLock> {
    let file = OpenOptions::new()
        .write(true)
        .create(true)
        .truncate(false)
        .mode(0o600)
        .custom_flags(libc::O_NOFOLLOW | libc::O_NONBLOCK)
        .open(path)
        .map_err(|e| e.to_string())?;
    if !file.metadata().map_err(|e| e.to_string())?.is_file() {
        return Err("lock must be a regular file".into());
    }
    file.try_lock()
        .map_err(|_| "state is already in use".to_string())?;
    Ok(FileLock { file })
}

pub struct ResourceBatch<'a> {
    store: &'a ResourceStore,
    created: Vec<(Scope, String)>,
    committed: bool,
}

impl ResourceBatch<'_> {
    pub fn ingest<F>(
        &mut self,
        scope: &Scope,
        object_id: &str,
        mime: &str,
        write: F,
    ) -> Result<ResourceRef>
    where
        F: FnOnce(&mut dyn Write) -> Result<()>,
    {
        let resource = self.store.ingest(scope, object_id, mime, write)?;
        self.created.push((scope.clone(), resource.id.clone()));
        Ok(resource)
    }
    pub fn import(
        &mut self,
        scope: &Scope,
        object_id: &str,
        mime: &str,
        reader: &mut dyn Read,
    ) -> Result<ResourceRef> {
        self.ingest(scope, object_id, mime, |writer| {
            let mut buffer = [0_u8; 65536];
            loop {
                let n = reader.read(&mut buffer).map_err(|e| e.to_string())?;
                if n == 0 {
                    return Ok(());
                }
                writer.write_all(&buffer[..n]).map_err(|e| e.to_string())?;
            }
        })
    }
    pub fn commit(mut self) {
        self.committed = true;
    }
}
impl Drop for ResourceBatch<'_> {
    fn drop(&mut self) {
        if !self.committed {
            for (scope, id) in &self.created {
                let _ = self.store.remove(scope, id);
            }
        }
    }
}

pub fn private_dir(path: &Path) -> Result<()> {
    fs::DirBuilder::new()
        .recursive(true)
        .mode(0o700)
        .create(path)
        .map_err(|e| e.to_string())?;
    let meta = fs::symlink_metadata(path).map_err(|e| e.to_string())?;
    if !meta.is_dir() || meta.file_type().is_symlink() {
        return Err("resource directory must be a real directory".into());
    }
    fs::set_permissions(path, fs::Permissions::from_mode(0o700)).map_err(|e| e.to_string())
}

pub fn open_regular(path: &Path, max_bytes: u64) -> Result<File> {
    let meta = fs::symlink_metadata(path).map_err(|e| e.to_string())?;
    if !meta.is_file() || meta.len() > max_bytes {
        return Err("input is not a bounded regular file".into());
    }
    // NONBLOCK prevents a FIFO swapped in after the metadata check from hanging open().
    let file = OpenOptions::new()
        .read(true)
        .custom_flags(libc::O_NOFOLLOW | libc::O_NONBLOCK)
        .open(path)
        .map_err(|e| e.to_string())?;
    let meta = file.metadata().map_err(|e| e.to_string())?;
    if !meta.is_file() || meta.len() > max_bytes {
        return Err("input changed type or exceeds byte limit".into());
    }
    Ok(file)
}

fn random_id() -> Result<String> {
    let mut value = [0_u8; 16];
    File::open("/dev/urandom")
        .and_then(|mut f| f.read_exact(&mut value))
        .map_err(|e| e.to_string())?;
    Ok(value.iter().map(|b| format!("{b:02x}")).collect())
}

fn validate_id(id: &str) -> Result<()> {
    if id.len() != 32
        || !id
            .bytes()
            .all(|v| v.is_ascii_hexdigit() && !v.is_ascii_uppercase())
    {
        return Err("invalid resource reference".into());
    }
    Ok(())
}

impl ResourceStore {
    /// Run only at a host-controlled task boundary, never during ResourceBatch
    /// assembly. At 7/8 of either quota, evict oldest metadata mtimes until both
    /// quotas are at most 3/4. Among the newest 32 resources, those at most 120s
    /// old are protected. If protection prevents enough reclamation, leave them
    /// intact; ingest retains its explicit quota failure and never retries GC.
    ///
    /// All resource pairs are checked before deleting any file. Malformed,
    /// unpaired or symlink state fails closed. Non-resource names are untouched.
    pub fn maintain_cache(&self) -> Result<CacheMaintenance> {
        let root = fs::symlink_metadata(&self.root).map_err(|e| e.to_string())?;
        if !root.is_dir() || root.file_type().is_symlink() {
            return Err("resource maintenance root must remain a real directory".into());
        }
        let _lock = self.writer_lock()?;
        let now = SystemTime::now();
        let mut pairs = BTreeMap::<String, (bool, bool)>::new();
        let mut bytes = 0_u64;
        let mut count = 0_usize;
        let scan_limit = self
            .budget
            .max_resources
            .saturating_mul(3)
            .saturating_add(16)
            .min(MAX_MAINTENANCE_ENTRIES);
        for (position, entry) in fs::read_dir(&self.root)
            .map_err(|e| e.to_string())?
            .enumerate()
        {
            if position >= scan_limit {
                return Err("resource maintenance directory entry limit".into());
            }
            let entry = entry.map_err(|e| e.to_string())?;
            let path = entry.path();
            let metadata = fs::symlink_metadata(&path).map_err(|e| e.to_string())?;
            // Never follow even an unrecognized symlink inside the cache root.
            if metadata.file_type().is_symlink() {
                return Err("resource maintenance refuses symlink state".into());
            }
            let extension = path.extension().and_then(|v| v.to_str());
            if extension == Some("blob") {
                // Include unmanaged .blob entries in quota accounting just as
                // ingest does, but do not treat them as deletable resources.
                bytes = bytes
                    .checked_add(metadata.len())
                    .ok_or("cache size overflow")?;
                count = count
                    .checked_add(1)
                    .ok_or("cache resource count overflow")?;
            }
            let Some(id) = path
                .file_stem()
                .and_then(|v| v.to_str())
                .filter(|id| validate_id(id).is_ok())
            else {
                continue;
            };
            if !matches!(extension, Some("json" | "blob")) {
                continue;
            }
            if !metadata.is_file() || metadata.permissions().mode() & 0o077 != 0 {
                return Err("resource maintenance requires private regular resource files".into());
            }
            let pair = pairs.entry(id.into()).or_default();
            if extension == Some("json") {
                pair.0 = true;
            } else {
                pair.1 = true;
            }
        }
        let mut entries = Vec::with_capacity(pairs.len());
        for (id, (metadata, blob)) in pairs {
            if !metadata || !blob {
                return Err("resource maintenance found an unpaired resource; explicit orphan recovery is required".into());
            }
            entries.push(self.cache_entry(&id)?);
        }
        // A tie is resolved by ID solely for deterministic eviction ordering.
        entries.sort_by(|a, b| b.modified.cmp(&a.modified).then_with(|| a.id.cmp(&b.id)));
        let protected = entries
            .iter()
            .take(RECENT_CACHE_RESOURCES)
            .filter(|entry| recent_cache(entry.modified, now))
            .map(|entry| entry.id.clone())
            .collect::<std::collections::BTreeSet<_>>();
        let high_water_reached = (count as u128) * 8 >= (self.budget.max_resources as u128) * 7
            || (bytes as u128) * 8 >= (self.budget.max_cache_bytes as u128) * 7;
        let target_count = ((self.budget.max_resources as u128) * 3 / 4) as usize;
        let target_bytes = ((self.budget.max_cache_bytes as u128) * 3 / 4) as u64;
        let mut report = CacheMaintenance {
            inspected_resources: entries.len(),
            removed_resources: 0,
            removed_bytes: 0,
            remaining_resources: count,
            remaining_bytes: bytes,
            protected_resources: protected.len(),
            high_water_reached,
            target_reached: count <= target_count && bytes <= target_bytes,
        };
        if !high_water_reached {
            return Ok(report);
        }
        for entry in entries.into_iter().rev() {
            if report.remaining_resources <= target_count && report.remaining_bytes <= target_bytes
            {
                break;
            }
            if protected.contains(&entry.id) {
                continue;
            }
            // The writer lock excludes cooperative writers. Recheck a candidate
            // as well so external changes cannot turn a valid old pair into an
            // unrelated deletion target after the initial validation pass.
            let current = self.cache_entry(&entry.id)?;
            if current.bytes != entry.bytes || current.modified != entry.modified {
                report.protected_resources += 1;
                continue;
            }
            // Remove authority first: a crash cannot leave a valid reference to
            // a missing/partially deleted payload. Existing orphan recovery can
            // later remove the remaining blob after an interrupted eviction.
            fs::remove_file(self.root.join(format!("{}.json", entry.id)))
                .map_err(|e| e.to_string())?;
            fs::remove_file(self.root.join(format!("{}.blob", entry.id)))
                .map_err(|e| e.to_string())?;
            report.removed_resources += 1;
            report.removed_bytes += entry.bytes;
            report.remaining_resources -= 1;
            report.remaining_bytes -= entry.bytes;
        }
        if report.removed_resources > 0 {
            File::open(&self.root)
                .and_then(|file| file.sync_all())
                .map_err(|e| e.to_string())?;
        }
        report.target_reached =
            report.remaining_resources <= target_count && report.remaining_bytes <= target_bytes;
        Ok(report)
    }

    fn cache_entry(&self, id: &str) -> Result<CacheEntry> {
        validate_id(id)?;
        let path = self.root.join(format!("{id}.json"));
        let file = open_regular(&path, CACHE_METADATA_BYTES)?;
        let metadata = file.metadata().map_err(|e| e.to_string())?;
        if metadata.permissions().mode() & 0o077 != 0 {
            return Err("resource maintenance metadata must be private".into());
        }
        let mut encoded = Vec::new();
        file.take(CACHE_METADATA_BYTES + 1)
            .read_to_end(&mut encoded)
            .map_err(|e| e.to_string())?;
        if encoded.len() as u64 > CACHE_METADATA_BYTES {
            return Err("resource maintenance metadata grew beyond limit".into());
        }
        let record: Record = serde_json::from_slice(&encoded)
            .map_err(|_| "resource maintenance refuses malformed metadata")?;
        record.scope.validate()?;
        if record.resource.id != id
            || record.object_id.len() > 1024
            || record.resource.mime.len() > 128
            || record.resource.mime.chars().any(char::is_control)
            || record.resource.bytes > self.budget.max_resource_bytes
        {
            return Err("resource maintenance metadata contract mismatch".into());
        }
        let blob = open_regular(
            &self.root.join(format!("{id}.blob")),
            self.budget.max_resource_bytes,
        )?;
        let blob = blob.metadata().map_err(|e| e.to_string())?;
        if blob.len() != record.resource.bytes || blob.permissions().mode() & 0o077 != 0 {
            return Err("resource maintenance payload contract mismatch".into());
        }
        Ok(CacheEntry {
            id: id.into(),
            bytes: blob.len(),
            modified: metadata.modified().map_err(|e| e.to_string())?,
        })
    }

    pub fn batch(&self) -> ResourceBatch<'_> {
        ResourceBatch {
            store: self,
            created: Vec::new(),
            committed: false,
        }
    }

    pub fn recover_orphans(&self) -> Result<usize> {
        let _lock = self.writer_lock()?;
        let mut removed = 0;
        for (count, entry) in fs::read_dir(&self.root)
            .map_err(|e| e.to_string())?
            .enumerate()
        {
            if count
                > self
                    .budget
                    .max_resources
                    .saturating_mul(3)
                    .saturating_add(16)
            {
                return Err("resource recovery entry limit".into());
            }
            let path = entry.map_err(|e| e.to_string())?.path();
            let Some(id) = path.file_stem().and_then(|v| v.to_str()) else {
                continue;
            };
            if validate_id(id).is_err() {
                continue;
            }
            let counterpart = match path.extension().and_then(|v| v.to_str()) {
                Some("blob") => self.root.join(format!("{id}.json")),
                Some("json") => self.root.join(format!("{id}.blob")),
                _ => continue,
            };
            if !counterpart.exists() {
                fs::remove_file(path).map_err(|e| e.to_string())?;
                removed += 1;
            }
        }
        Ok(removed)
    }
    pub fn open(root: &Path, budget: Budget) -> Result<Self> {
        if budget.max_resource_bytes == 0
            || budget.max_cache_bytes < budget.max_resource_bytes
            || budget.max_resources == 0
        {
            return Err("invalid resource budget".into());
        }
        private_dir(root)?;
        Ok(Self {
            root: root.canonicalize().map_err(|e| e.to_string())?,
            budget,
        })
    }

    fn writer_lock(&self) -> Result<FileLock> {
        lock_private(&self.root.join(".lock"))
    }

    fn usage(&self) -> Result<(u64, usize)> {
        let mut bytes = 0_u64;
        let mut count = 0;
        for (i, entry) in fs::read_dir(&self.root)
            .map_err(|e| e.to_string())?
            .enumerate()
        {
            if i > self
                .budget
                .max_resources
                .saturating_mul(3)
                .saturating_add(16)
            {
                return Err("resource directory entry limit".into());
            }
            let entry = entry.map_err(|e| e.to_string())?;
            let meta = entry.metadata().map_err(|e| e.to_string())?;
            if entry.path().extension().is_some_and(|v| v == "blob") {
                bytes = bytes.checked_add(meta.len()).ok_or("cache size overflow")?;
                count += 1;
            }
        }
        Ok((bytes, count))
    }

    /// The closure writes through a bounded writer, so network callers need not buffer downloads.
    pub fn ingest<F>(
        &self,
        scope: &Scope,
        object_id: &str,
        mime: &str,
        write: F,
    ) -> Result<ResourceRef>
    where
        F: FnOnce(&mut dyn Write) -> Result<()>,
    {
        scope.validate()?;
        if object_id.len() > 1024 || mime.len() > 128 || mime.chars().any(char::is_control) {
            return Err("invalid resource metadata".into());
        }
        let _lock = self.writer_lock()?;
        let (used, count) = self.usage()?;
        if count >= self.budget.max_resources || used >= self.budget.max_cache_bytes {
            return Err("resource cache is full".into());
        }
        let limit = self
            .budget
            .max_resource_bytes
            .min(self.budget.max_cache_bytes - used);
        let id = random_id()?;
        let path = self.root.join(format!("{id}.blob"));
        let mut file = OpenOptions::new()
            .write(true)
            .create_new(true)
            .mode(0o600)
            .open(&path)
            .map_err(|e| e.to_string())?;
        let result = (|| {
            let mut writer = BoundedWriter {
                inner: &mut file,
                written: 0,
                limit,
                exceeded: false,
            };
            write(&mut writer)?;
            if writer.exceeded {
                return Err("resource exceeds byte budget".into());
            }
            let bytes = writer.written;
            file.sync_all().map_err(|e| e.to_string())?;
            let resource = ResourceRef {
                id: id.clone(),
                mime: mime.into(),
                bytes,
            };
            let record = Record {
                scope: scope.clone(),
                object_id: object_id.into(),
                resource: resource.clone(),
            };
            let metadata_path = self.root.join(format!("{id}.json"));
            let mut metadata = OpenOptions::new()
                .write(true)
                .create_new(true)
                .mode(0o600)
                .open(&metadata_path)
                .map_err(|e| e.to_string())?;
            if let Err(error) = serde_json::to_writer(&mut metadata, &record)
                .map_err(|e| e.to_string())
                .and_then(|_| metadata.sync_all().map_err(|e| e.to_string()))
            {
                let _ = fs::remove_file(metadata_path);
                return Err(error);
            }
            File::open(&self.root)
                .and_then(|f| f.sync_all())
                .map_err(|e| e.to_string())?;
            Ok(resource)
        })();
        if result.is_err() {
            let _ = fs::remove_file(&path);
            let _ = fs::remove_file(self.root.join(format!("{id}.json")));
        }
        result
    }

    pub fn import(
        &self,
        scope: &Scope,
        object_id: &str,
        mime: &str,
        reader: &mut dyn Read,
    ) -> Result<ResourceRef> {
        self.ingest(scope, object_id, mime, |writer| {
            let mut buffer = [0_u8; 65536];
            loop {
                let size = reader.read(&mut buffer).map_err(|e| e.to_string())?;
                if size == 0 {
                    return Ok(());
                }
                writer
                    .write_all(&buffer[..size])
                    .map_err(|e| e.to_string())?;
            }
        })
    }

    pub fn resolve(&self, scope: &Scope, id: &str) -> Result<(ResourceRef, PathBuf)> {
        scope.validate()?;
        validate_id(id)?;
        let meta_path = self.root.join(format!("{id}.json"));
        let meta = fs::symlink_metadata(&meta_path).map_err(|_| "resource missing or expired")?;
        if !meta.is_file() || meta.len() > 8192 {
            return Err("invalid resource metadata file".into());
        }
        let record: Record = serde_json::from_reader(open_regular(&meta_path, 8192)?)
            .map_err(|_| "invalid resource metadata")?;
        if &record.scope != scope || record.resource.id != id {
            return Err("resource scope mismatch".into());
        }
        let path = self.root.join(format!("{id}.blob"));
        let blob = fs::symlink_metadata(&path).map_err(|_| "resource missing or expired")?;
        if !blob.is_file()
            || blob.len() != record.resource.bytes
            || blob.len() > self.budget.max_resource_bytes
        {
            return Err("resource size mismatch".into());
        }
        Ok((record.resource, path))
    }

    /// Owner lookup for host-side capability routing. Scoped `resolve` remains
    /// mandatory before accessing bytes; this does not change its isolation.
    pub fn scope_for(&self, id: &str) -> Result<Scope> {
        validate_id(id)?;
        let record: Record =
            serde_json::from_reader(open_regular(&self.root.join(format!("{id}.json")), 8192)?)
                .map_err(|_| "invalid resource metadata")?;
        if record.resource.id != id {
            return Err("resource identity mismatch".into());
        }
        record.scope.validate()?;
        self.resolve(&record.scope, id)?;
        Ok(record.scope)
    }

    pub fn read_range(
        &self,
        scope: &Scope,
        id: &str,
        offset: u64,
        max_bytes: usize,
    ) -> Result<Vec<u8>> {
        use std::io::{Seek, SeekFrom};
        if max_bytes == 0 || max_bytes > 1024 * 1024 {
            return Err("read range must be 1..1048576 bytes".into());
        }
        let (resource, path) = self.resolve(scope, id)?;
        if offset > resource.bytes {
            return Err("read offset outside resource".into());
        }
        let mut file = open_regular(&path, self.budget.max_resource_bytes)?;
        file.seek(SeekFrom::Start(offset))
            .map_err(|e| e.to_string())?;
        let mut result = Vec::new();
        file.take(max_bytes as u64)
            .read_to_end(&mut result)
            .map_err(|e| e.to_string())?;
        Ok(result)
    }

    pub fn remove(&self, scope: &Scope, id: &str) -> Result<()> {
        let _lock = self.writer_lock()?;
        let (_, path) = self.resolve(scope, id)?;
        fs::remove_file(self.root.join(format!("{id}.json"))).map_err(|e| e.to_string())?;
        fs::remove_file(path).map_err(|e| e.to_string())
    }

    pub fn list(&self, scope: &Scope, offset: usize, limit: usize) -> Result<Vec<ResourceRef>> {
        if limit == 0 || limit > 128 {
            return Err("resource list limit must be 1..128".into());
        }
        let _lock = self.writer_lock()?;
        self.usage()?;
        let mut ids: Vec<_> = fs::read_dir(&self.root)
            .map_err(|e| e.to_string())?
            .filter_map(|entry| entry.ok())
            .filter_map(|entry| entry.file_name().into_string().ok())
            .filter_map(|name| name.strip_suffix(".json").map(str::to_owned))
            .collect();
        ids.sort();
        Ok(ids
            .iter()
            .filter_map(|id| self.resolve(scope, id).ok().map(|v| v.0))
            .skip(offset)
            .take(limit)
            .collect())
    }
}

fn recent_cache(modified: SystemTime, now: SystemTime) -> bool {
    now.duration_since(modified)
        .map_or(true, |age| age <= Duration::from_secs(RECENT_CACHE_SECONDS))
}

struct BoundedWriter<'a> {
    inner: &'a mut File,
    written: u64,
    limit: u64,
    exceeded: bool,
}
impl Write for BoundedWriter<'_> {
    fn write(&mut self, input: &[u8]) -> std::io::Result<usize> {
        if input.len() as u64 > self.limit - self.written {
            self.exceeded = true;
            return Err(std::io::Error::other("resource exceeds byte budget"));
        }
        let n = self.inner.write(input)?;
        self.written += n as u64;
        Ok(n)
    }
    fn flush(&mut self) -> std::io::Result<()> {
        self.inner.flush()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::Platform;
    fn scope(account: &str) -> Scope {
        Scope {
            platform: Platform::Local,
            account: account.into(),
        }
    }

    #[test]
    fn dropping_lock_unlocks_even_with_an_inherited_description() {
        let dir = std::env::temp_dir().join(random_id().unwrap());
        private_dir(&dir).unwrap();
        let path = dir.join("lock");
        let guard = lock_private(&path).unwrap();
        let inherited = guard.file.try_clone().unwrap();
        assert!(lock_private(&path).is_err());
        drop(guard);
        let restored = lock_private(&path).unwrap();
        drop(restored);
        drop(inherited);
        fs::remove_dir_all(dir).unwrap();
    }
    #[test]
    fn resources_are_scoped_bounded_and_persistent() {
        let dir = std::env::temp_dir().join(random_id().unwrap());
        let budget = Budget {
            max_resource_bytes: 8,
            max_cache_bytes: 12,
            max_resources: 3,
        };
        let store = ResourceStore::open(&dir, budget).unwrap();
        let item = store
            .import(&scope("a"), "example", "text/plain", &mut &b"hello"[..])
            .unwrap();
        assert!(store.resolve(&scope("b"), &item.id).is_err());
        assert!(store.resolve(&scope("a"), "../../etc/passwd").is_err());
        assert_eq!(
            store.read_range(&scope("a"), &item.id, 1, 3).unwrap(),
            b"ell"
        );
        assert!(
            store
                .import(&scope("a"), "large", "text/plain", &mut &b"123456789"[..])
                .is_err()
        );
        assert_eq!(store.usage().unwrap(), (5, 1));
        assert!(
            store
                .import(
                    &scope("a"),
                    "aggregate",
                    "text/plain",
                    &mut &b"12345678"[..]
                )
                .is_err()
        );
        assert_eq!(store.usage().unwrap(), (5, 1));
        let reloaded = ResourceStore::open(&dir, budget).unwrap();
        assert_eq!(
            reloaded.read_range(&scope("a"), &item.id, 0, 8).unwrap(),
            b"hello"
        );
        {
            let mut batch = store.batch();
            batch
                .import(
                    &scope("a"),
                    "will-rollback",
                    "text/plain",
                    &mut &b"123456"[..],
                )
                .unwrap();
            assert!(
                batch
                    .import(&scope("a"), "too-large", "text/plain", &mut &b"123456"[..])
                    .is_err()
            );
        }
        assert_eq!(store.usage().unwrap(), (5, 1));
        assert!(store.remove(&scope("b"), &item.id).is_err());
        store.remove(&scope("a"), &item.id).unwrap();
        assert!(store.resolve(&scope("a"), &item.id).is_err());
        fs::remove_dir_all(dir).unwrap();
    }

    struct CacheLab(PathBuf);
    impl CacheLab {
        fn new() -> Self {
            Self(
                std::env::temp_dir().join(format!("resource-maintenance-{}", random_id().unwrap())),
            )
        }
        fn store(&self, budget: Budget) -> ResourceStore {
            ResourceStore::open(&self.0, budget).unwrap()
        }
    }
    impl Drop for CacheLab {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.0);
        }
    }
    fn old(store: &ResourceStore, id: &str, order: u64) {
        File::open(store.root.join(format!("{id}.json")))
            .unwrap()
            .set_modified(SystemTime::UNIX_EPOCH + Duration::from_secs(1000 + order))
            .unwrap();
    }
    fn fixture_resource(store: &ResourceStore, owner: &Scope, size: usize) -> ResourceRef {
        store
            .import(
                owner,
                "cache-fixture",
                "application/octet-stream",
                &mut vec![7_u8; size].as_slice(),
            )
            .unwrap()
    }

    #[test]
    fn maintenance_crosses_high_count_then_stops_at_low_count_oldest_first() {
        let lab = CacheLab::new();
        let store = lab.store(Budget {
            max_resource_bytes: 128,
            max_cache_bytes: 1024,
            max_resources: 16,
        });
        let mut refs = Vec::new();
        for n in 0..14 {
            let resource = fixture_resource(&store, &scope("fixture"), 8);
            old(&store, &resource.id, n);
            refs.push(resource);
        }
        let report = store.maintain_cache().unwrap();
        assert!(report.high_water_reached);
        assert!(report.target_reached);
        assert_eq!(report.inspected_resources, 14);
        assert_eq!(report.removed_resources, 2);
        assert_eq!(report.removed_bytes, 16);
        assert_eq!(report.remaining_resources, 12);
        assert_eq!(report.remaining_bytes, 96);
        for (n, resource) in refs.iter().enumerate() {
            assert_eq!(
                store.resolve(&scope("fixture"), &resource.id).is_ok(),
                n >= 2
            );
        }
        assert_eq!(store.maintain_cache().unwrap().removed_resources, 0);
    }

    #[test]
    fn maintenance_below_high_water_preserves_old_resources_even_above_low_water() {
        let lab = CacheLab::new();
        let store = lab.store(Budget {
            max_resource_bytes: 128,
            max_cache_bytes: 1024,
            max_resources: 16,
        });
        for n in 0..13 {
            let resource = fixture_resource(&store, &scope("fixture"), 8);
            old(&store, &resource.id, n);
        }
        let before = store.usage().unwrap();
        let report = store.maintain_cache().unwrap();
        assert!(!report.high_water_reached);
        assert_eq!(report.removed_resources, 0);
        assert!(!report.target_reached);
        assert_eq!(store.usage().unwrap(), before);
    }

    #[test]
    fn maintenance_byte_watermark_works_when_count_is_low() {
        let lab = CacheLab::new();
        let store = lab.store(Budget {
            max_resource_bytes: 512,
            max_cache_bytes: 1024,
            max_resources: 16,
        });
        let mut resources = Vec::new();
        for n in 0..3 {
            let resource = fixture_resource(&store, &scope("fixture"), 300);
            old(&store, &resource.id, n);
            resources.push(resource);
        }
        let report = store.maintain_cache().unwrap();
        assert!(report.high_water_reached);
        assert!(report.target_reached);
        assert_eq!(report.removed_resources, 1);
        assert_eq!(report.removed_bytes, 300);
        assert_eq!(report.remaining_bytes, 600);
        assert!(store.resolve(&scope("fixture"), &resources[0].id).is_err());
        assert!(store.resolve(&scope("fixture"), &resources[1].id).is_ok());
    }

    #[test]
    fn tiny_fresh_full_cache_keeps_quota_failure_instead_of_collecting_a_live_batch() {
        let lab = CacheLab::new();
        let store = lab.store(Budget {
            max_resource_bytes: 512,
            max_cache_bytes: 1024,
            max_resources: 2,
        });
        let first = fixture_resource(&store, &scope("fixture"), 512);
        let second = fixture_resource(&store, &scope("fixture"), 512);
        let report = store.maintain_cache().unwrap();
        assert!(report.high_water_reached);
        assert!(!report.target_reached);
        assert_eq!(report.protected_resources, 2);
        assert_eq!(report.removed_resources, 0);
        assert!(
            store
                .import(&scope("fixture"), "full", "text/plain", &mut &b"x"[..])
                .unwrap_err()
                .contains("full")
        );
        assert!(store.resolve(&scope("fixture"), &first.id).is_ok());
        assert!(store.resolve(&scope("fixture"), &second.id).is_ok());
        old(&store, &first.id, 1);
        let report = store.maintain_cache().unwrap();
        assert_eq!(report.removed_resources, 1);
        assert_eq!(report.protected_resources, 1);
        assert!(report.target_reached);
        assert!(store.resolve(&scope("fixture"), &second.id).is_ok());
    }

    #[test]
    fn newest_thirty_two_recent_resources_are_protected_even_if_low_water_cannot_be_reached() {
        let lab = CacheLab::new();
        let store = lab.store(Budget {
            max_resource_bytes: 128,
            max_cache_bytes: 4096,
            max_resources: 40,
        });
        for _ in 0..40 {
            fixture_resource(&store, &scope("fixture"), 1);
        }
        let report = store.maintain_cache().unwrap();
        assert_eq!(report.protected_resources, 32);
        assert_eq!(report.removed_resources, 8);
        assert_eq!(report.remaining_resources, 32);
        assert!(!report.target_reached);
        assert_eq!(store.list(&scope("fixture"), 0, 128).unwrap().len(), 32);
    }

    #[test]
    fn main_cache_may_collect_old_pairs_across_scopes_but_never_unrelated_files() {
        let lab = CacheLab::new();
        let store = lab.store(Budget {
            max_resource_bytes: 128,
            max_cache_bytes: 1024,
            max_resources: 8,
        });
        let a = Scope {
            platform: Platform::Bilibili,
            account: "fixture-a".into(),
        };
        let b = Scope {
            platform: Platform::Github,
            account: "fixture-b".into(),
        };
        let mut resources = Vec::new();
        for n in 0..8 {
            let owner = if n % 2 == 0 { &a } else { &b };
            let resource = fixture_resource(&store, owner, 8);
            old(&store, &resource.id, n);
            resources.push((owner.clone(), resource));
        }
        let notes = store.root.join("notes.txt");
        let unknown = store.root.join("unmanaged.blob");
        let foreign = store.root.join("session.json");
        fs::write(&notes, b"not a resource").unwrap();
        fs::write(&unknown, b"opaque unmanaged data").unwrap();
        fs::write(&foreign, b"unrelated host fixture").unwrap();
        let report = store.maintain_cache().unwrap();
        assert_eq!(report.inspected_resources, 8);
        assert_eq!(report.removed_resources, 3);
        assert_eq!(report.remaining_resources, 6);
        assert!(report.target_reached);
        assert_eq!(fs::read(&notes).unwrap(), b"not a resource");
        assert_eq!(fs::read(&unknown).unwrap(), b"opaque unmanaged data");
        assert_eq!(fs::read(&foreign).unwrap(), b"unrelated host fixture");
        for (n, (owner, resource)) in resources.iter().enumerate() {
            assert_eq!(store.resolve(owner, &resource.id).is_ok(), n >= 3);
            assert!(
                store
                    .resolve(if owner == &a { &b } else { &a }, &resource.id)
                    .is_err()
            );
        }
        let replacement = fixture_resource(&store, &a, 8);
        assert!(!resources.iter().any(|(_, old)| old.id == replacement.id));
        assert!(store.resolve(&a, &resources[0].1.id).is_err());
    }

    #[test]
    fn malformed_or_symlink_pairs_are_not_a_license_to_delete_any_cache_file() {
        use std::os::unix::fs::symlink;
        let lab = CacheLab::new();
        let store = lab.store(Budget {
            max_resource_bytes: 128,
            max_cache_bytes: 1024,
            max_resources: 8,
        });
        let mut refs = Vec::new();
        for n in 0..8 {
            let resource = fixture_resource(&store, &scope("fixture"), 8);
            old(&store, &resource.id, n);
            refs.push(resource);
        }
        let metadata = store.root.join(format!("{}.json", refs[0].id));
        let original = fs::read(&metadata).unwrap();
        fs::write(&metadata, b"{broken-cache-record").unwrap();
        assert!(store.maintain_cache().unwrap_err().contains("malformed"));
        assert_eq!(store.usage().unwrap().1, 8);
        assert!(
            refs.iter()
                .all(|resource| store.root.join(format!("{}.blob", resource.id)).exists())
        );
        fs::write(&metadata, original).unwrap();
        let payload = store.root.join(format!("{}.blob", refs[1].id));
        fs::remove_file(&payload).unwrap();
        let outside = lab.0.with_extension("outside-fixture");
        fs::write(&outside, b"outside must survive").unwrap();
        symlink(&outside, &payload).unwrap();
        assert!(store.maintain_cache().unwrap_err().contains("symlink"));
        assert_eq!(fs::read(&outside).unwrap(), b"outside must survive");
        assert!(metadata.exists());
        assert!(store.root.join(format!("{}.json", refs[1].id)).exists());
        fs::remove_file(&outside).unwrap();
    }

    #[test]
    fn maintenance_directory_scan_is_bounded_before_any_deletion() {
        let lab = CacheLab::new();
        let store = lab.store(Budget {
            max_resource_bytes: 128,
            max_cache_bytes: 1024,
            max_resources: 2,
        });
        let first = fixture_resource(&store, &scope("fixture"), 8);
        let second = fixture_resource(&store, &scope("fixture"), 8);
        old(&store, &first.id, 1);
        old(&store, &second.id, 2);
        for n in 0..25 {
            fs::write(store.root.join(format!("unmanaged-{n}.txt")), b"fixture").unwrap();
        }
        assert!(store.maintain_cache().unwrap_err().contains("entry limit"));
        assert!(store.resolve(&scope("fixture"), &first.id).is_ok());
        assert!(store.resolve(&scope("fixture"), &second.id).is_ok());
    }

    #[test]
    fn maintenance_rejects_a_cache_root_replaced_with_a_symlink() {
        use std::os::unix::fs::symlink;
        let lab = CacheLab::new();
        let budget = Budget {
            max_resource_bytes: 128,
            max_cache_bytes: 1024,
            max_resources: 2,
        };
        let root = lab.0.join("cache");
        let store = ResourceStore::open(&root, budget).unwrap();
        let other_root = lab.0.join("other");
        let other = ResourceStore::open(&other_root, budget).unwrap();
        let a = fixture_resource(&other, &scope("other"), 8);
        let b = fixture_resource(&other, &scope("other"), 8);
        old(&other, &a.id, 1);
        old(&other, &b.id, 2);
        fs::rename(&root, lab.0.join("original-cache")).unwrap();
        symlink(&other_root, &root).unwrap();
        assert!(
            store
                .maintain_cache()
                .unwrap_err()
                .contains("root must remain")
        );
        assert!(other.resolve(&scope("other"), &a.id).is_ok());
        assert!(other.resolve(&scope("other"), &b.id).is_ok());
    }
}
