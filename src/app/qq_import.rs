// SPDX-License-Identifier: MIT
//! Human-only, offline import of the QQ Rust experiment's private state.
//!
//! Preconditions: the trusted management wrapper holds the old client.lock for
//! the entire import and mounts its directory read-only; Runtime already holds
//! runtime.lock. No QQ connection, signer, login or send is started here.
use super::*;
use platform_qq::session::{Device, Session};
use std::{collections::BTreeSet, os::unix::fs::DirBuilderExt, path::Component};

const MAX_SESSION: u64 = 64 * 1024;
const MAX_DEVICE: u64 = 4096;
const MAX_RECORD: u64 = 64 * 1024;
const MAX_FILES: usize = 1024;
const MAX_BYTES: u64 = 16 * 1024 * 1024;
const MANIFEST: &str = "import-manifest.json";

struct ImportBudget {
    bytes: u64,
    files: usize,
    inventory: Vec<Value>,
}
impl ImportBudget {
    fn record(&mut self, name: &str, bytes: &[u8]) -> Result<()> {
        self.files = self
            .files
            .checked_add(1)
            .ok_or("legacy import file count overflow")?;
        self.bytes = self
            .bytes
            .checked_add(bytes.len() as u64)
            .ok_or("legacy import byte count overflow")?;
        if self.files > MAX_FILES || self.bytes > MAX_BYTES {
            return Err("legacy QQ import exceeds file/byte budget".into());
        }
        // Names are fixed whitelist entries or validated operation IDs. Do not
        // include host paths, message bodies, tickets, UID, or device GUID.
        self.inventory
            .push(json!({"name":name,"bytes":bytes.len()}));
        Ok(())
    }
}
struct Staging(Option<PathBuf>);
impl Drop for Staging {
    fn drop(&mut self) {
        if let Some(path) = &self.0 {
            let _ = fs::remove_dir_all(path);
        }
    }
}

fn metadata_if_present(path: &Path) -> Result<Option<fs::Metadata>> {
    match fs::symlink_metadata(path) {
        Ok(metadata) => Ok(Some(metadata)),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(error) => Err(error.to_string()),
    }
}
fn private_directory(path: &Path) -> Result<()> {
    let metadata = fs::symlink_metadata(path).map_err(|e| e.to_string())?;
    if !metadata.is_dir()
        || metadata.file_type().is_symlink()
        || metadata.permissions().mode() & 0o077 != 0
    {
        return Err("legacy QQ directory must be real and private (0700)".into());
    }
    Ok(())
}
fn source_directory(path: &Path) -> Result<()> {
    if !path.is_absolute() {
        return Err("legacy QQ source must be an absolute private directory".into());
    }
    let mut prefix = PathBuf::new();
    for component in path.components() {
        if matches!(component, Component::ParentDir) {
            return Err("legacy QQ source must not contain parent traversal".into());
        }
        prefix.push(component.as_os_str());
        if fs::symlink_metadata(&prefix)
            .map_err(|e| e.to_string())?
            .file_type()
            .is_symlink()
        {
            return Err("legacy QQ source path must not contain symlinks".into());
        }
    }
    private_directory(path)
}
fn read_private(path: &Path, limit: u64) -> Result<Vec<u8>> {
    let file = open_regular(path, limit)?;
    if file
        .metadata()
        .map_err(|e| e.to_string())?
        .permissions()
        .mode()
        & 0o077
        != 0
    {
        return Err("legacy QQ files must be private (0600)".into());
    }
    let mut bytes = Vec::new();
    file.take(limit + 1)
        .read_to_end(&mut bytes)
        .map_err(|e| e.to_string())?;
    if bytes.len() as u64 > limit {
        return Err("legacy QQ file exceeds byte limit".into());
    }
    Ok(bytes)
}
fn optional_private(path: &Path, limit: u64) -> Result<Option<Vec<u8>>> {
    if metadata_if_present(path)?.is_none() {
        return Ok(None);
    }
    read_private(path, limit).map(Some)
}
fn write_private(path: &Path, bytes: &[u8]) -> Result<()> {
    let mut file = OpenOptions::new()
        .write(true)
        .create_new(true)
        .mode(0o600)
        .open(path)
        .map_err(|e| e.to_string())?;
    file.write_all(bytes)
        .and_then(|_| file.sync_all())
        .map_err(|e| e.to_string())
}
fn valid_operation_id(id: &str) -> bool {
    !id.is_empty()
        && id.len() <= 64
        && id
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || b"-_".contains(&byte))
}
fn validate_session(bytes: &[u8]) -> Result<Session> {
    let session: Session =
        serde_json::from_slice(bytes).map_err(|_| "invalid legacy QQ session")?;
    if session.version != 1
        || session.uin == 0
        || session.uid.is_empty()
        || session.uid.len() > 128
        || !session.uid.is_ascii()
        || session
            .uid
            .bytes()
            .any(|b| b.is_ascii_control() || b.is_ascii_whitespace())
        || session.key()?.len() != 16
    {
        return Err("legacy QQ session identity/key is incomplete".into());
    }
    let has_tickets = session.has_tickets()?;
    let has_refresh = !session.temp_pwd()?.is_empty();
    if !has_tickets && !has_refresh {
        return Err("legacy QQ session has no cached or refresh credentials".into());
    }
    // Validate encodings even if the other login path would be sufficient.
    session.tgt()?;
    session.d2()?;
    Ok(session)
}
fn validate_device(bytes: &[u8]) -> Result<()> {
    let device: Device = serde_json::from_slice(bytes).map_err(|_| "invalid legacy QQ device")?;
    device.guid_bytes()?;
    if device.device_name.is_empty()
        || device.device_name.len() > 256
        || device.system_kernel.is_empty()
        || device.system_kernel.len() > 128
        || device.kernel_version.is_empty()
        || device.kernel_version.len() > 128
        || [
            &device.device_name,
            &device.system_kernel,
            &device.kernel_version,
        ]
        .iter()
        .any(|v| v.chars().any(char::is_control))
    {
        return Err("invalid legacy QQ device identity".into());
    }
    Ok(())
}

impl Runtime {
    /// Import-only control-plane entry. The caller must hold the source lock;
    /// this function never acquires or creates files in the read-only source.
    pub fn qq_import(&mut self, source: &Path) -> Result<Value> {
        if self.account == "public" {
            return Err("QQ import requires a dedicated account alias".into());
        }
        let scope = self.scope(Platform::Qq);
        scope.validate()?;
        let destination = crate::private_state::account_path(&self.state, "qq", &self.account)?;
        let existing = metadata_if_present(&destination)?;
        if existing.is_some() {
            private_directory(&destination)?;
            if metadata_if_present(&destination.join(MANIFEST))?.is_some() {
                // Never consult the source or replace newer Runtime tickets on
                // repeated import. Validate the marker's storage, not its body.
                read_private(&destination.join(MANIFEST), 256 * 1024)?;
                return Ok(
                    json!({"status":"already_imported","scope":scope,"source_read":false,
                    "credentials_replaced":false,"network_requests":false,"messages_sent":0}),
                );
            }
            if fs::read_dir(&destination)
                .map_err(|e| e.to_string())?
                .next()
                .transpose()
                .map_err(|e| e.to_string())?
                .is_some()
            {
                return Err("QQ alias already has private state without an import manifest; existing session was not replaced".into());
            }
        }
        source_directory(source)?;
        let session_bytes = read_private(&source.join("session-rust.json"), MAX_SESSION)?;
        let session = validate_session(&session_bytes)?;
        let device_bytes = read_private(&source.join("device.json"), MAX_DEVICE)?;
        validate_device(&device_bytes)?;
        let mut budget = ImportBudget {
            bytes: 0,
            files: 0,
            inventory: Vec::new(),
        };
        budget.record("session-rust.json", &session_bytes)?;
        budget.record("device.json", &device_bytes)?;

        let accounts = self.state.join("accounts");
        private_dir(&accounts)?;
        let parent = accounts.join("qq");
        private_dir(&parent)?;
        let mut random = [0u8; 16];
        File::open("/dev/urandom")
            .and_then(|mut f| f.read_exact(&mut random))
            .map_err(|e| e.to_string())?;
        let nonce = random
            .iter()
            .map(|v| format!("{v:02x}"))
            .collect::<String>();
        let staged = parent.join(format!(".qq-import-{nonce}"));
        fs::DirBuilder::new()
            .mode(0o700)
            .create(&staged)
            .map_err(|e| e.to_string())?;
        let mut cleanup = Staging(Some(staged.clone()));
        let legacy = staged.join("legacy");
        private_dir(&legacy)?;
        write_private(&staged.join("session.json"), &session_bytes)?;
        write_private(&staged.join("device.json"), &device_bytes)?;
        super::auth::bind_identity(&staged, &session.uin.to_string())?;

        let mut consumed = BTreeSet::new();
        let mut uncertain_records = 0usize;
        let source_outbox = source.join("outbox");
        if metadata_if_present(&source_outbox)?.is_some() {
            private_directory(&source_outbox)?;
            let archive = legacy.join("outbox");
            private_dir(&archive)?;
            let mut names = Vec::new();
            for entry in fs::read_dir(&source_outbox).map_err(|e| e.to_string())? {
                if names.len() >= MAX_FILES {
                    return Err("legacy QQ outbox exceeds entry budget".into());
                }
                names.push(entry.map_err(|e| e.to_string())?.file_name());
            }
            names.sort();
            for name in names {
                let name = name.to_str().ok_or("invalid legacy QQ outbox filename")?;
                let operation = name
                    .strip_suffix(".json")
                    .filter(|id| valid_operation_id(id))
                    .ok_or("legacy QQ outbox accepts only operation-ID JSON files")?;
                let bytes = read_private(&source_outbox.join(name), MAX_RECORD)?;
                match serde_json::from_slice::<Value>(&bytes) {
                    Ok(value) if value.is_object() => {
                        if value
                            .get("id")
                            .is_some_and(|id| id.as_str() != Some(operation))
                        {
                            return Err("legacy QQ outbox filename and operation ID differ".into());
                        }
                        if value.get("id").is_none() {
                            uncertain_records += 1;
                        }
                    }
                    _ => uncertain_records += 1,
                }
                // Even a crash-created empty/truncated record consumes its ID.
                // It remains evidence, never a new Prepared operation.
                consumed.insert(operation.to_string());
                budget.record(&format!("outbox/{name}"), &bytes)?;
                write_private(&archive.join(name), &bytes)?;
            }
            File::open(&archive)
                .and_then(|f| f.sync_all())
                .map_err(|e| e.to_string())?;
        }
        let mut go_consumed = false;
        for name in [
            "self-send.lock",
            "self-send.result.json",
            "rust-self-test.json",
            "outgoing.json",
        ] {
            let Some(bytes) = optional_private(&source.join(name), MAX_RECORD)? else {
                continue;
            };
            if matches!(name, "self-send.lock" | "self-send.result.json") {
                go_consumed = true;
            }
            if matches!(name, "rust-self-test.json" | "outgoing.json") {
                if let Ok(value) = serde_json::from_slice::<Value>(&bytes) {
                    if value
                        .get("account_uin")
                        .is_some_and(|uin| uin.as_u64() != Some(u64::from(session.uin)))
                    {
                        return Err("legacy QQ draft belongs to another account".into());
                    }
                    if let Some(id) = value.get("id") {
                        let id = id
                            .as_str()
                            .filter(|id| valid_operation_id(id))
                            .ok_or("invalid legacy QQ draft operation ID")?;
                        // Old drafts are archived, not executable migration work.
                        consumed.insert(id.to_string());
                    }
                }
            }
            budget.record(name, &bytes)?;
            write_private(&legacy.join(name), &bytes)?;
        }
        let consumed_count = consumed.len();
        let consumed_index = json!({"schema":1,
            "operation_ids":consumed.into_iter().collect::<Vec<_>>(),"go_self_send_consumed":go_consumed});
        if serde_json::to_vec_pretty(&consumed_index)
            .map_err(|e| e.to_string())?
            .len()
            .saturating_add(1)
            > MAX_RECORD as usize
        {
            return Err("legacy QQ consumed-operation index exceeds Runtime read budget".into());
        }
        save_private_new(&staged.join("legacy-consumed.json"), &consumed_index)?;
        File::open(&legacy)
            .and_then(|f| f.sync_all())
            .map_err(|e| e.to_string())?;
        // The manifest is published only with the complete account directory.
        // Summaries contain no credentials, device identifiers or host paths.
        save_private_new(
            &staged.join(MANIFEST),
            &json!({"schema":1,"source_format":"agent-runtime-qq-rust-lab-v1",
            "identity":session.uin.to_string(),"imported_at":crate::outbox::now()?,"source_files":budget.inventory,
            "source_bytes":budget.bytes,"reserved_operation_count":consumed_count,"uncertain_records_reserved":uncertain_records,
            "go_self_send_consumed":go_consumed,"online_checked":false}),
        )?;
        File::open(&staged)
            .and_then(|f| f.sync_all())
            .map_err(|e| e.to_string())?;
        if metadata_if_present(&destination)?.is_some() {
            // Only an empty, real directory from pre-import management is safe
            // to replace. Runtime's exclusive lock serializes this check/rename.
            private_directory(&destination)?;
            fs::remove_dir(&destination)
                .map_err(|_| "QQ destination changed or is not empty; import was not published")?;
        }
        fs::rename(&staged, &destination).map_err(|e| e.to_string())?;
        cleanup.0 = None;
        File::open(&parent)
            .and_then(|f| f.sync_all())
            .map_err(|e| e.to_string())?;
        Ok(
            json!({"status":"imported","scope":scope,"identity":session.uin.to_string(),
            "operation_ids_reserved":consumed_count,"go_self_send_consumed":go_consumed,"legacy_files_archived":budget.files-2,
            "online_checked":false,"network_requests":false,"messages_sent":0}),
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use base64::{Engine, engine::general_purpose::STANDARD};
    use std::os::unix::fs::symlink;

    struct Lab(PathBuf);
    impl Lab {
        fn new() -> Self {
            let path = std::env::temp_dir().join(format!(
                "qq-import-{}-{}",
                std::process::id(),
                std::time::SystemTime::now()
                    .duration_since(std::time::UNIX_EPOCH)
                    .unwrap()
                    .as_nanos()
            ));
            private_dir(&path).unwrap();
            Self(path)
        }
        fn source(&self) -> PathBuf {
            self.0.join("legacy")
        }
        fn state(&self) -> PathBuf {
            self.0.join("state")
        }
        fn seed(&self) -> Vec<u8> {
            private_dir(&self.source()).unwrap();
            let session = serde_json::to_vec(&json!({"version":1,"uin":42,"uid":"u_synthetic_only","sequence":4242,
                "tgt":STANDARD.encode(b"fixture-tgt"),"d2":STANDARD.encode(b"fixture-d2"),"d2_key":STANDARD.encode([7;16]),
                "temp_pwd":STANDARD.encode(b"fixture-refresh"),"tgtgt":STANDARD.encode(b"fixture-tgtgt"),"cookies":"fixture-secret",
                "future_credential_field":{"preserved":true}})).unwrap();
            write_private(&self.source().join("session-rust.json"), &session).unwrap();
            write_private(&self.source().join("device.json"), &serde_json::to_vec(&json!({"guid":"01".repeat(16),"device_name":"fixture","system_kernel":"Linux","kernel_version":"6.1.0"})).unwrap()).unwrap();
            session
        }
    }
    impl Drop for Lab {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.0);
        }
    }

    #[test]
    fn import_preserves_credentials_device_sequence_and_consumed_evidence_without_replay() {
        let lab = Lab::new();
        let session = lab.seed();
        private_dir(&lab.source().join("outbox")).unwrap();
        write_private(
            &lab.source().join("outbox/confirmed-once.json"),
            br#"{"id":"confirmed-once","status":"confirmed"}"#,
        )
        .unwrap();
        write_private(
            &lab.source().join("outbox/unknown-once.json"),
            b"{interrupted",
        )
        .unwrap();
        write_private(&lab.source().join("self-send.lock"), b"").unwrap();
        write_private(
            &lab.source().join("self-send.result.json"),
            br#"{"result":0}"#,
        )
        .unwrap();
        write_private(
            &lab.source().join("rust-self-test.json"),
            br#"{"id":"reserved-old-test","account_uin":42,"text":"fixture only"}"#,
        )
        .unwrap();
        let mut runtime = Runtime::open(&lab.state(), "qq-fixture", None).unwrap();
        let result = runtime.qq_import(&lab.source()).unwrap();
        assert_eq!(result["status"], "imported");
        assert_eq!(result["messages_sent"], 0);
        assert!(!result.to_string().contains("fixture-secret"));
        let target = crate::private_state::account_path(&lab.state(), "qq", "qq-fixture").unwrap();
        assert_eq!(
            read_private(&target.join("session.json"), MAX_SESSION).unwrap(),
            session
        );
        assert_eq!(
            read_private(&target.join("device.json"), MAX_DEVICE).unwrap(),
            read_private(&lab.source().join("device.json"), MAX_DEVICE).unwrap()
        );
        let consumed: Value = serde_json::from_slice(
            &read_private(&target.join("legacy-consumed.json"), MAX_RECORD).unwrap(),
        )
        .unwrap();
        assert_eq!(
            consumed["operation_ids"],
            json!(["confirmed-once", "reserved-old-test", "unknown-once"])
        );
        assert_eq!(consumed["go_self_send_consumed"], true);
        assert_eq!(
            read_private(&target.join("legacy/outbox/unknown-once.json"), MAX_RECORD).unwrap(),
            b"{interrupted"
        );
        assert!(!lab.state().join("outbox").exists());
        let mut newer: Value = serde_json::from_slice(&session).unwrap();
        newer["sequence"] = json!(9000);
        let newer = serde_json::to_vec(&newer).unwrap();
        fs::write(target.join("session.json"), &newer).unwrap();
        fs::remove_dir_all(lab.source()).unwrap();
        let again = runtime.qq_import(&lab.source()).unwrap();
        assert_eq!(again["status"], "already_imported");
        assert_eq!(again["source_read"], false);
        assert_eq!(
            read_private(&target.join("session.json"), MAX_SESSION).unwrap(),
            newer
        );
    }
    #[test]
    fn existing_nonimported_session_is_never_replaced() {
        let lab = Lab::new();
        lab.seed();
        let mut runtime = Runtime::open(&lab.state(), "qq-fixture", None).unwrap();
        let target =
            crate::private_state::account_directory(&lab.state(), "qq", "qq-fixture").unwrap();
        write_private(&target.join("session.json"), b"existing state").unwrap();
        assert!(
            runtime
                .qq_import(&lab.source())
                .unwrap_err()
                .contains("not replaced")
        );
        assert_eq!(
            read_private(&target.join("session.json"), MAX_SESSION).unwrap(),
            b"existing state"
        );
    }
    #[test]
    fn broad_permissions_and_symlinks_fail_without_publishing_partial_accounts() {
        let lab = Lab::new();
        lab.seed();
        let mut runtime = Runtime::open(&lab.state(), "qq-fixture", None).unwrap();
        let session = lab.source().join("session-rust.json");
        fs::set_permissions(&session, fs::Permissions::from_mode(0o644)).unwrap();
        assert!(
            runtime
                .qq_import(&lab.source())
                .unwrap_err()
                .contains("private")
        );
        fs::set_permissions(&session, fs::Permissions::from_mode(0o600)).unwrap();
        let device = lab.source().join("device.json");
        let other = lab.0.join("private-device.json");
        fs::rename(&device, &other).unwrap();
        symlink(&other, &device).unwrap();
        assert!(runtime.qq_import(&lab.source()).is_err());
        let target = crate::private_state::account_path(&lab.state(), "qq", "qq-fixture").unwrap();
        assert!(!target.exists());
        fs::remove_file(&device).unwrap();
        fs::rename(&other, &device).unwrap();
        private_dir(&lab.source().join("outbox")).unwrap();
        write_private(
            &lab.source().join("outbox/safe-id.json"),
            br#"{"id":"different-id"}"#,
        )
        .unwrap();
        assert!(
            runtime
                .qq_import(&lab.source())
                .unwrap_err()
                .contains("differ")
        );
        assert!(!target.exists());
        let parent = lab.state().join("accounts/qq");
        assert_eq!(fs::read_dir(parent).unwrap().count(), 0);
    }
}
