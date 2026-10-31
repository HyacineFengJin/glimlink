// SPDX-License-Identifier: MIT
//! Private durable records for authorized tool writes. Not an enumerable AI resource.
use runtime_core::{
    Platform, Result, Scope,
    resources::{open_regular, private_dir},
};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::{
    fs::{self, File, OpenOptions},
    io::{Read, Write},
    os::unix::fs::{OpenOptionsExt, PermissionsExt},
    path::{Path, PathBuf},
    time::{SystemTime, UNIX_EPOCH},
};

const LIMIT: u64 = 64 * 1024;

pub fn now() -> Result<u64> {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs())
        .map_err(|e| e.to_string())
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Status {
    Prepared,
    Attempting,
    Sent,
    Verified,
    Unknown,
    Rejected,
    AlreadySatisfied,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Operation {
    pub schema: u32,
    pub id: String,
    pub scope: Scope,
    pub actor_id: String,
    pub target: Value,
    pub message: String,
    /// Optional exact tool payload, including media references and reply coordinates.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub request: Option<Value>,
    pub status: Status,
    pub created_at: u64,
    pub attempt_started_at: Option<u64>,
    #[serde(default)]
    pub attempt_context: Option<Value>,
    pub receipt: Option<Value>,
    pub verification: Option<Value>,
    pub error: Option<String>,
}
impl Operation {
    pub fn new(
        id: &str,
        scope: Scope,
        actor_id: String,
        target: Value,
        message: &str,
    ) -> Result<Self> {
        let record = Self {
            schema: 1,
            id: id.into(),
            scope,
            actor_id,
            target,
            message: message.into(),
            request: None,
            status: Status::Prepared,
            created_at: now()?,
            attempt_started_at: None,
            attempt_context: None,
            receipt: None,
            verification: None,
            error: None,
        };
        record.validate()?;
        Ok(record)
    }
    fn validate(&self) -> Result<()> {
        validate_id(&self.id)?;
        self.scope.validate()?;
        if self.schema != 1
            || self.scope.account == "public"
            || !matches!(
                self.scope.platform,
                Platform::Bilibili | Platform::Tieba | Platform::Qq | Platform::Github
            )
            || self.actor_id.is_empty()
            || self.actor_id.len() > 128
            || !self.target.is_object()
            || self.message.is_empty()
            || self.message.chars().count() > 1000
            || self.message.contains('\0')
        {
            return Err("invalid private outbox operation".into());
        }
        if matches!(self.status, Status::Prepared | Status::AlreadySatisfied)
            != self.attempt_started_at.is_none()
        {
            return Err("attempted operation lacks durable attempt timestamp".into());
        }
        Ok(())
    }
}
fn validate_id(id: &str) -> Result<()> {
    if id.is_empty()
        || id.len() > 96
        || !id
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b"_-".contains(&b))
    {
        Err("operation ID must contain 1..96 ASCII letters, digits, '-' or '_'".into())
    } else {
        Ok(())
    }
}

/// The embedding Runtime holds its exclusive state FileLock for all transitions.
pub struct Outbox {
    root: PathBuf,
}
impl Outbox {
    /// Open a private existing ledger without creating or repairing any files.
    /// Status/evidence readers must never mutate a store merely to inspect it.
    pub fn open_existing(state: &Path) -> Result<Self> {
        let root = state.join("outbox");
        let metadata = fs::symlink_metadata(&root).map_err(|e| e.to_string())?;
        if !metadata.is_dir()
            || metadata.file_type().is_symlink()
            || metadata.permissions().mode() & 0o077 != 0
        {
            return Err("outbox must be an existing private directory (0700)".into());
        }
        Ok(Self { root })
    }
    pub fn open(state: &Path) -> Result<Self> {
        let root = state.join("outbox");
        private_dir(&root)?;
        Ok(Self { root })
    }
    fn path(&self, id: &str) -> Result<PathBuf> {
        validate_id(id)?;
        Ok(self.root.join(format!("{id}.json")))
    }
    /// Read one explicitly identified private operation without knowing its
    /// scope first. This never enumerates, reconciles, or changes an operation.
    /// Callers exposing status must check account selection and project a safe
    /// result rather than returning the complete private record to the model.
    pub fn inspect(&self, id: &str) -> Result<Operation> {
        let path = self.path(id)?;
        let file = open_regular(&path, LIMIT)?;
        if file
            .metadata()
            .map_err(|e| e.to_string())?
            .permissions()
            .mode()
            & 0o077
            != 0
        {
            return Err("outbox record must be private (0600)".into());
        }
        let mut bytes = Vec::new();
        file.take(LIMIT + 1)
            .read_to_end(&mut bytes)
            .map_err(|e| e.to_string())?;
        if bytes.len() as u64 > LIMIT {
            return Err("outbox record limit".into());
        }
        let record: Operation =
            serde_json::from_slice(&bytes).map_err(|_| "invalid outbox record")?;
        record.validate()?;
        if record.id != id {
            return Err("outbox operation ID mismatch".into());
        }
        Ok(record)
    }
    pub fn load(&self, id: &str, scope: &Scope) -> Result<Operation> {
        let record = self.inspect(id)?;
        if &record.scope != scope {
            return Err("outbox operation scope mismatch".into());
        }
        Ok(record)
    }
    pub fn exists(&self, id: &str) -> Result<bool> {
        Ok(fs::symlink_metadata(self.path(id)?).is_ok())
    }
    pub fn insert(&self, record: &Operation) -> Result<()> {
        record.validate()?;
        let path = self.path(&record.id)?;
        let bytes = serde_json::to_vec_pretty(record).map_err(|e| e.to_string())?;
        if bytes.len() as u64 > LIMIT {
            return Err("outbox record limit".into());
        }
        let mut file = OpenOptions::new()
            .write(true)
            .create_new(true)
            .mode(0o600)
            .open(path)
            .map_err(|e| e.to_string())?;
        file.write_all(&bytes)
            .and_then(|_| file.sync_all())
            .map_err(|e| e.to_string())?;
        File::open(&self.root)
            .and_then(|f| f.sync_all())
            .map_err(|e| e.to_string())
    }
    fn replace(&self, record: &Operation) -> Result<()> {
        record.validate()?;
        let bytes = serde_json::to_vec_pretty(record).map_err(|e| e.to_string())?;
        if bytes.len() as u64 > LIMIT {
            return Err("outbox record limit".into());
        }
        let nonce = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map_err(|e| e.to_string())?
            .as_nanos();
        let temporary = self.root.join(format!(
            ".{}-{}-{nonce}.next",
            record.id,
            std::process::id()
        ));
        let result = (|| {
            let mut file = OpenOptions::new()
                .write(true)
                .create_new(true)
                .mode(0o600)
                .open(&temporary)
                .map_err(|e| e.to_string())?;
            file.write_all(&bytes)
                .and_then(|_| file.sync_all())
                .map_err(|e| e.to_string())?;
            fs::rename(&temporary, self.path(&record.id)?).map_err(|e| e.to_string())?;
            File::open(&self.root)
                .and_then(|f| f.sync_all())
                .map_err(|e| e.to_string())
        })();
        if result.is_err() {
            let _ = fs::remove_file(temporary);
        }
        result
    }
    /// Must finish successfully before any platform write is attempted.
    pub fn begin_attempt(&self, record: &mut Operation) -> Result<()> {
        let stored = self.load(&record.id, &record.scope)?;
        if stored.status != Status::Prepared
            || stored.message != record.message
            || stored.target != record.target
            || stored.actor_id != record.actor_id
            || stored.request != record.request
        {
            return Err("operation already attempted or draft changed; do not resend".into());
        }
        record.status = Status::Attempting;
        record.attempt_started_at = Some(now()?);
        self.replace(record)
    }
    pub fn finish(&self, record: &Operation) -> Result<()> {
        let stored = self.load(&record.id, &record.scope)?;
        let transition = matches!(
            (&stored.status, &record.status),
            (Status::Prepared, Status::AlreadySatisfied)
                | (Status::AlreadySatisfied, Status::AlreadySatisfied)
                | (
                    Status::Attempting,
                    Status::Sent | Status::Unknown | Status::Rejected
                )
                | (Status::Sent, Status::Sent | Status::Verified)
                | (Status::Unknown, Status::Unknown | Status::Verified)
                | (Status::Rejected, Status::Rejected)
                | (Status::Verified, Status::Verified)
        );
        if !transition
            || stored.message != record.message
            || stored.target != record.target
            || stored.actor_id != record.actor_id
            || stored.request != record.request
            || stored.attempt_started_at != record.attempt_started_at
            || stored.attempt_context != record.attempt_context
        {
            return Err("invalid attempted operation transition".into());
        }
        self.replace(record)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    struct Lab(PathBuf);
    impl Lab {
        fn new() -> Self {
            Self(std::env::temp_dir().join(format!(
                "outbox-inspect-{}-{}",
                std::process::id(),
                SystemTime::now().duration_since(UNIX_EPOCH).unwrap().as_nanos()
            )))
        }
        fn record(&self) -> Operation {
            Operation::new(
                "inspect-1",
                Scope {
                    platform: Platform::Github,
                    account: "fixture".into(),
                },
                "42".into(),
                serde_json::json!({"kind":"issue","owner":"fixture","repo":"project","number":7}),
                "synthetic local record",
            )
            .unwrap()
        }
    }
    impl Drop for Lab {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.0);
        }
    }

    #[test]
    fn inspect_restores_scope_without_mutating_an_unfinished_operation() {
        let lab = Lab::new();
        let outbox = Outbox::open(&lab.0).unwrap();
        let mut record = lab.record();
        outbox.insert(&record).unwrap();
        outbox.begin_attempt(&mut record).unwrap();
        let path = outbox.path(&record.id).unwrap();
        let before = fs::read(&path).unwrap();
        let inspected = outbox.inspect(&record.id).unwrap();
        assert_eq!(inspected.scope, record.scope);
        assert_eq!(inspected.status, Status::Attempting);
        assert_eq!(inspected.attempt_started_at, record.attempt_started_at);
        assert_eq!(
            fs::read(&path).unwrap(),
            before,
            "inspection must not convert Attempting to Unknown or rewrite the record"
        );
        assert!(outbox.load(&record.id, &record.scope).is_ok());
        assert!(
            outbox
                .load(
                    &record.id,
                    &Scope {
                        account: "other".into(),
                        ..record.scope.clone()
                    }
                )
                .is_err()
        );
        assert!(
            outbox
                .load(
                    &record.id,
                    &Scope {
                        platform: Platform::Qq,
                        ..record.scope.clone()
                    }
                )
                .is_err()
        );
    }

    #[test]
    fn inspect_rejects_mismatched_identity_and_invalid_record() {
        let lab = Lab::new();
        let outbox = Outbox::open(&lab.0).unwrap();
        let record = lab.record();
        outbox.insert(&record).unwrap();
        let path = outbox.path(&record.id).unwrap();
        let moved = outbox.path("different-id").unwrap();
        fs::rename(&path, &moved).unwrap();
        assert!(
            outbox
                .inspect("different-id")
                .unwrap_err()
                .contains("ID mismatch")
        );
        fs::rename(&moved, &path).unwrap();
        let mut invalid = serde_json::to_value(&record).unwrap();
        invalid["schema"] = serde_json::json!(99);
        fs::write(&path, serde_json::to_vec(&invalid).unwrap()).unwrap();
        assert!(outbox.inspect(&record.id).is_err());
        invalid["schema"] = serde_json::json!(1);
        invalid["status"] = serde_json::json!("sent");
        // A terminal status without a durable attempt timestamp is invalid.
        fs::write(&path, serde_json::to_vec(&invalid).unwrap()).unwrap();
        assert!(outbox.inspect(&record.id).is_err());
        assert!(outbox.inspect("../../escape").is_err());
    }

    #[test]
    fn inspect_keeps_private_regular_file_and_byte_bounds() {
        use std::os::unix::fs::symlink;
        let lab = Lab::new();
        let outbox = Outbox::open(&lab.0).unwrap();
        let record = lab.record();
        outbox.insert(&record).unwrap();
        let path = outbox.path(&record.id).unwrap();
        fs::set_permissions(&path, fs::Permissions::from_mode(0o644)).unwrap();
        assert!(outbox.inspect(&record.id).unwrap_err().contains("private"));
        fs::set_permissions(&path, fs::Permissions::from_mode(0o600)).unwrap();
        fs::write(&path, vec![b' '; LIMIT as usize + 1]).unwrap();
        assert!(outbox.inspect(&record.id).is_err());
        fs::remove_file(&path).unwrap();
        let outside = lab.0.join("private-fixture.json");
        let mut file = OpenOptions::new()
            .write(true)
            .create_new(true)
            .mode(0o600)
            .open(&outside)
            .unwrap();
        file.write_all(&serde_json::to_vec(&record).unwrap())
            .unwrap();
        symlink(&outside, &path).unwrap();
        assert!(outbox.inspect(&record.id).is_err());
    }

    #[test]
    fn durable_attempt_blocks_replay_after_restart_and_rejects_cross_scope() {
        let dir = std::env::temp_dir().join(format!(
            "outbox-{}-{}",
            std::process::id(),
            SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        let scope = Scope {
            platform: Platform::Bilibili,
            account: "a".into(),
        };
        let outbox = Outbox::open(&dir).unwrap();
        let mut record = Operation::new(
            "once-1",
            scope.clone(),
            "42".into(),
            serde_json::json!({"aid":7}),
            "晚安喵（test）",
        )
        .unwrap();
        outbox.insert(&record).unwrap();
        assert!(outbox.insert(&record).is_err());
        outbox.begin_attempt(&mut record).unwrap();
        drop(outbox);
        let reopened = Outbox::open(&dir).unwrap();
        let mut loaded = reopened.load("once-1", &scope).unwrap();
        assert_eq!(loaded.status, Status::Attempting);
        assert!(reopened.begin_attempt(&mut loaded).is_err());
        assert!(
            reopened
                .load(
                    "once-1",
                    &Scope {
                        account: "b".into(),
                        ..scope.clone()
                    }
                )
                .is_err()
        );
        assert!(reopened.load("../../escape", &scope).is_err());
        loaded.status = Status::Sent;
        loaded.receipt = Some(serde_json::json!({"rpid":8}));
        reopened.finish(&loaded).unwrap();
        let mut sent = reopened.load("once-1", &scope).unwrap();
        assert!(reopened.begin_attempt(&mut sent).is_err());
        assert_eq!(
            fs::metadata(dir.join("outbox/once-1.json"))
                .unwrap()
                .permissions()
                .mode()
                & 0o077,
            0
        );
        fs::remove_dir_all(dir).unwrap();
    }
}
