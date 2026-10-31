// SPDX-License-Identifier: MIT
//! Offline action evidence for a completed/interrupted model job. Ledger status
//! is evidence about an action, never proof that the operator's whole goal was met.
use crate::outbox::{Outbox, Status};
use runtime_core::{Result, Scope};
use serde::Serialize;
use std::{collections::BTreeSet, fs, io::ErrorKind, os::unix::fs::PermissionsExt, path::Path};

pub const MAX_OPERATION_IDS: usize = 128;
pub const MAX_SUMMARY_BYTES: usize = 64 * 1024;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum EvidenceStatus {
    Verified,
    AcknowledgedOnly,
    Unknown,
    Rejected,
    NotRecorded,
    Prepared,
    AlreadySatisfied,
    InvalidRecord,
}

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize)]
pub struct EvidenceCounts {
    pub total: usize,
    pub verified: usize,
    pub acknowledged_only: usize,
    pub unknown: usize,
    pub rejected: usize,
    pub not_recorded: usize,
    pub prepared: usize,
    pub already_satisfied: usize,
    pub invalid_record: usize,
}

#[derive(Clone, Debug, Serialize)]
pub struct ActionEvidence {
    pub operation_id: String,
    pub status: EvidenceStatus,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub scope: Option<Scope>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub detail: Option<String>,
    #[serde(skip_serializing_if = "is_false")]
    pub execution_may_be_in_flight: bool,
}

#[derive(Clone, Debug, Serialize)]
pub struct EvidenceSummary {
    pub counts: EvidenceCounts,
    pub actions: Vec<ActionEvidence>,
    pub needs_attention: bool,
    pub readonly: bool,
    /// Optional explanations may be omitted to preserve every action/status/scope
    /// within the total response budget. No action is omitted.
    pub details_omitted: bool,
}

/// Inspect exactly the supplied operation IDs, deduplicated in input order.
/// No accounts, credentials, target/request bodies, provider APIs, state recovery,
/// or resend paths are consulted. An empty/missing store is never created.
pub fn summarize(state: &Path, operation_ids: &[String]) -> Result<EvidenceSummary> {
    if operation_ids.len() > MAX_OPERATION_IDS {
        return Err("action evidence accepts at most 128 operation IDs".into());
    }
    // Validate before constructing any path, even for an absent outbox.
    for id in operation_ids {
        validate_id(id)?;
    }
    let mut seen = BTreeSet::new();
    let ids = operation_ids
        .iter()
        .filter(|id| seen.insert(id.as_str()))
        .collect::<Vec<_>>();
    let mut summary = EvidenceSummary {
        counts: EvidenceCounts::default(),
        actions: Vec::new(),
        needs_attention: false,
        readonly: true,
        details_omitted: false,
    };
    if ids.is_empty() {
        return Ok(summary);
    }

    let root = state.join("outbox");
    enum Store {
        Missing,
        Invalid,
        Available(Outbox),
    }
    let store = match fs::symlink_metadata(&root) {
        Err(error) if error.kind() == ErrorKind::NotFound => Store::Missing,
        Err(_) => Store::Invalid,
        Ok(metadata)
            if metadata.is_dir()
                && !metadata.file_type().is_symlink()
                && metadata.permissions().mode() & 0o077 == 0 =>
        {
            // open_existing must not mkdir/chmod an allegedly read-only store.
            match Outbox::open_existing(state) {
                Ok(outbox) => Store::Available(outbox),
                Err(_) => Store::Invalid,
            }
        }
        Ok(_) => Store::Invalid,
    };
    for id in ids {
        let evidence = match &store {
            Store::Missing => unavailable(id, EvidenceStatus::NotRecorded),
            Store::Invalid => unavailable(id, EvidenceStatus::InvalidRecord),
            Store::Available(outbox) => {
                // exists() currently collapses all metadata errors into false.
                // Only an actual NotFound is absence; permission/type/read or
                // validation failures must remain invalid evidence.
                match fs::symlink_metadata(root.join(format!("{id}.json"))) {
                    Err(error) if error.kind() == ErrorKind::NotFound => {
                        unavailable(id, EvidenceStatus::NotRecorded)
                    }
                    Err(_) => unavailable(id, EvidenceStatus::InvalidRecord),
                    Ok(_) => match outbox.inspect(id) {
                        Ok(record) => {
                            let (status, detail, in_flight) = match record.status {
                                Status::Verified => (EvidenceStatus::Verified, None, false),
                                Status::Sent => (
                                    EvidenceStatus::AcknowledgedOnly,
                                    Some("Server acknowledged; exact readback is not recorded."),
                                    false,
                                ),
                                Status::Attempting => (
                                    EvidenceStatus::Unknown,
                                    Some(
                                        "Recorded as attempting; execution may still be in flight. Do not resend.",
                                    ),
                                    true,
                                ),
                                Status::Unknown => (
                                    EvidenceStatus::Unknown,
                                    Some(
                                        "Action outcome is unknown. Do not replace its operation ID to resend.",
                                    ),
                                    true,
                                ),
                                Status::Rejected => (
                                    EvidenceStatus::Rejected,
                                    Some("The durable action record reports rejection."),
                                    false,
                                ),
                                Status::Prepared => (
                                    EvidenceStatus::Prepared,
                                    Some(
                                        "Prepared record has no durable attempt; no completion is evidenced.",
                                    ),
                                    false,
                                ),
                                Status::AlreadySatisfied => (
                                    EvidenceStatus::AlreadySatisfied,
                                    Some(
                                        "The requested state was already satisfied without a write.",
                                    ),
                                    false,
                                ),
                            };
                            ActionEvidence {
                                operation_id: id.clone(),
                                status,
                                scope: Some(record.scope),
                                detail: detail.map(str::to_owned),
                                execution_may_be_in_flight: in_flight,
                            }
                        }
                        Err(_) => unavailable(id, EvidenceStatus::InvalidRecord),
                    },
                }
            }
        };
        count(&mut summary.counts, evidence.status);
        summary.needs_attention |= matches!(
            evidence.status,
            EvidenceStatus::Unknown
                | EvidenceStatus::Rejected
                | EvidenceStatus::NotRecorded
                | EvidenceStatus::Prepared
                | EvidenceStatus::InvalidRecord
        );
        summary.actions.push(evidence);
    }
    // All projected strings are bounded by input/Scope validation or fixed text.
    // At the pathological limit (128 maximal escaped account aliases), optional
    // prose is removed; action identity, status, owner and in-flight flag remain.
    if serde_json::to_vec(&summary)
        .map_err(|_| "could not encode action evidence")?
        .len()
        > MAX_SUMMARY_BYTES
    {
        for action in &mut summary.actions {
            action.detail = None;
        }
        summary.details_omitted = true;
    }
    if serde_json::to_vec(&summary)
        .map_err(|_| "could not encode action evidence")?
        .len()
        > MAX_SUMMARY_BYTES
    {
        return Err("action evidence exceeds its 64 KiB response budget".into());
    }
    Ok(summary)
}

fn is_false(value: &bool) -> bool {
    !*value
}
fn validate_id(id: &str) -> Result<()> {
    if id.is_empty()
        || id.len() > 96
        || !id
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b"_-".contains(&b))
    {
        Err("action evidence requires valid 1..96 character operation IDs".into())
    } else {
        Ok(())
    }
}
fn unavailable(id: &str, status: EvidenceStatus) -> ActionEvidence {
    let detail = match status {
        EvidenceStatus::NotRecorded => {
            "No durable record exists in this snapshot; execution was not inferred."
        }
        _ => "Operation evidence could not be safely read or validated.",
    };
    ActionEvidence {
        operation_id: id.into(),
        status,
        scope: None,
        detail: Some(detail.into()),
        execution_may_be_in_flight: false,
    }
}
fn count(counts: &mut EvidenceCounts, status: EvidenceStatus) {
    counts.total += 1;
    *match status {
        EvidenceStatus::Verified => &mut counts.verified,
        EvidenceStatus::AcknowledgedOnly => &mut counts.acknowledged_only,
        EvidenceStatus::Unknown => &mut counts.unknown,
        EvidenceStatus::Rejected => &mut counts.rejected,
        EvidenceStatus::NotRecorded => &mut counts.not_recorded,
        EvidenceStatus::Prepared => &mut counts.prepared,
        EvidenceStatus::AlreadySatisfied => &mut counts.already_satisfied,
        EvidenceStatus::InvalidRecord => &mut counts.invalid_record,
    } += 1;
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::outbox::Operation;
    use runtime_core::Platform;
    use std::{
        fs::{File, OpenOptions},
        io::Write,
        os::unix::fs::OpenOptionsExt,
        time::{SystemTime, UNIX_EPOCH},
    };

    struct Lab(std::path::PathBuf);
    impl Lab {
        fn new() -> Self {
            Self(std::env::temp_dir().join(format!(
                    "action-evidence-{}-{}",
                    std::process::id(),
                    SystemTime::now()
                        .duration_since(UNIX_EPOCH)
                        .unwrap()
                        .as_nanos()
                )))
        }
    }
    impl Drop for Lab {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.0);
        }
    }
    fn save(outbox: &Outbox, id: &str, status: Status, scope: Scope) -> Operation {
        let mut record = Operation::new(
            id,
            scope,
            "secret-actor-fixture".into(),
            serde_json::json!({"target":"private-target-fixture"}),
            "private-message-fixture",
        )
        .unwrap();
        record.request = Some(serde_json::json!({"credential":"secret-request-fixture"}));
        outbox.insert(&record).unwrap();
        if status == Status::Prepared {
            return record;
        }
        if status == Status::AlreadySatisfied {
            record.status = status;
            record.verification =
                Some(serde_json::json!({"verified":true,"private":"secret-verification-fixture"}));
            outbox.finish(&record).unwrap();
            return record;
        }
        outbox.begin_attempt(&mut record).unwrap();
        if status == Status::Attempting {
            return record;
        }
        if status == Status::Verified {
            record.status = Status::Sent;
            record.receipt = Some(serde_json::json!({"private":"secret-receipt-fixture"}));
            outbox.finish(&record).unwrap();
            record.status = Status::Verified;
            record.verification =
                Some(serde_json::json!({"verified":true,"private":"secret-verification-fixture"}));
            outbox.finish(&record).unwrap();
            return record;
        }
        record.status = status;
        record.error = Some("secret-error-fixture".into());
        outbox.finish(&record).unwrap();
        record
    }
    fn scope(platform: Platform, account: &str) -> Scope {
        Scope {
            platform,
            account: account.into(),
        }
    }
    fn write_private(path: &Path, value: &serde_json::Value) {
        let mut file = OpenOptions::new()
            .create_new(true)
            .write(true)
            .mode(0o600)
            .open(path)
            .unwrap();
        file.write_all(&serde_json::to_vec(value).unwrap()).unwrap();
    }

    #[test]
    fn aggregates_all_ledger_states_without_turning_acknowledgement_into_verification_or_mutating_attempts()
     {
        let lab = Lab::new();
        let outbox = Outbox::open(&lab.0).unwrap();
        let mut ids = Vec::new();
        for (id, status, owner) in [
            (
                "verified",
                Status::Verified,
                scope(Platform::Github, "github-owner"),
            ),
            ("sent", Status::Sent, scope(Platform::Qq, "qq-owner")),
            (
                "attempting",
                Status::Attempting,
                scope(Platform::Bilibili, "bili-owner"),
            ),
            (
                "unknown",
                Status::Unknown,
                scope(Platform::Tieba, "tieba-owner"),
            ),
            (
                "rejected",
                Status::Rejected,
                scope(Platform::Github, "github-owner"),
            ),
            (
                "prepared",
                Status::Prepared,
                scope(Platform::Github, "github-owner"),
            ),
            (
                "already",
                Status::AlreadySatisfied,
                scope(Platform::Bilibili, "bili-owner"),
            ),
        ] {
            save(&outbox, id, status, owner);
            ids.push(id.to_owned());
        }
        ids.extend(["missing".into(), "invalid".into(), "sent".into()]);
        write_private(
            &lab.0.join("outbox/invalid.json"),
            &serde_json::json!({"schema":999}),
        );
        let before = fs::read(lab.0.join("outbox/attempting.json")).unwrap();
        let result = summarize(&lab.0, &ids).unwrap();
        assert_eq!(
            result.counts,
            EvidenceCounts {
                total: 9,
                verified: 1,
                acknowledged_only: 1,
                unknown: 2,
                rejected: 1,
                not_recorded: 1,
                prepared: 1,
                already_satisfied: 1,
                invalid_record: 1
            }
        );
        assert!(result.readonly && result.needs_attention);
        assert_eq!(result.actions[1].status, EvidenceStatus::AcknowledgedOnly);
        assert!(result.actions[2].execution_may_be_in_flight);
        assert!(
            result.actions[2]
                .detail
                .as_deref()
                .unwrap()
                .contains("attempting")
        );
        assert_eq!(
            result.actions[1].scope,
            Some(scope(Platform::Qq, "qq-owner"))
        );
        assert_eq!(
            result.actions.len(),
            9,
            "duplicate IDs must not produce another action"
        );
        assert_eq!(
            fs::read(lab.0.join("outbox/attempting.json")).unwrap(),
            before
        );
        assert_eq!(
            outbox.inspect("attempting").unwrap().status,
            Status::Attempting
        );
        let json = serde_json::to_string(&result).unwrap();
        for private in [
            "secret-actor-fixture",
            "private-target-fixture",
            "private-message-fixture",
            "secret-request-fixture",
            "secret-receipt-fixture",
            "secret-error-fixture",
            "secret-verification-fixture",
        ] {
            assert!(!json.contains(private), "{private}");
        }
    }
    #[test]
    fn missing_store_and_empty_input_remain_readonly_and_acknowledgement_alone_is_not_a_failure() {
        let lab = Lab::new();
        let empty = summarize(&lab.0, &[]).unwrap();
        assert_eq!(empty.counts.total, 0);
        assert!(!empty.needs_attention);
        let missing = summarize(&lab.0, &["missing".into()]).unwrap();
        assert_eq!(missing.counts.not_recorded, 1);
        assert!(missing.needs_attention);
        assert!(
            !lab.0.exists(),
            "a read-only summary must not create state or an outbox"
        );
        let outbox = Outbox::open(&lab.0).unwrap();
        save(&outbox, "sent", Status::Sent, scope(Platform::Qq, "owner"));
        let summary = summarize(&lab.0, &["sent".into()]).unwrap();
        assert_eq!(summary.counts.acknowledged_only, 1);
        assert_eq!(summary.counts.verified, 0);
        assert!(!summary.needs_attention);
    }
    #[test]
    fn invalid_input_bad_scope_mismatched_identity_permissions_and_symlinks_are_never_missing() {
        use std::os::unix::fs::symlink;
        let lab = Lab::new();
        let outbox = Outbox::open(&lab.0).unwrap();
        let valid = save(
            &outbox,
            "valid",
            Status::Prepared,
            scope(Platform::Github, "owner"),
        );
        assert!(summarize(&lab.0, &["../escape".into()]).is_err());
        assert!(summarize(&lab.0, &["x".repeat(97)]).is_err());
        assert!(summarize(&lab.0, &vec!["valid".into(); MAX_OPERATION_IDS + 1]).is_err());
        let mut wrong = serde_json::to_value(&valid).unwrap();
        wrong["scope"]["account"] = serde_json::json!("public");
        write_private(&lab.0.join("outbox/wrong-scope.json"), &wrong);
        wrong = serde_json::to_value(&valid).unwrap();
        write_private(&lab.0.join("outbox/wrong-id.json"), &wrong);
        let broad = lab.0.join("outbox/broad.json");
        write_private(&broad, &serde_json::to_value(&valid).unwrap());
        fs::set_permissions(&broad, fs::Permissions::from_mode(0o644)).unwrap();
        symlink(
            lab.0.join("outbox/valid.json"),
            lab.0.join("outbox/link.json"),
        )
        .unwrap();
        symlink(
            lab.0.join("outbox/absent.json"),
            lab.0.join("outbox/dangling.json"),
        )
        .unwrap();
        let ids = ["wrong-scope", "wrong-id", "broad", "link", "dangling"].map(str::to_owned);
        let summary = summarize(&lab.0, &ids).unwrap();
        assert_eq!(summary.counts.invalid_record, 5);
        assert_eq!(summary.counts.not_recorded, 0);
        assert!(summary.needs_attention);
    }
    #[test]
    fn invalid_store_type_is_not_a_missing_ledger() {
        let lab = Lab::new();
        fs::create_dir_all(&lab.0).unwrap();
        File::create(lab.0.join("outbox")).unwrap();
        let result = summarize(&lab.0, &["one".into()]).unwrap();
        assert_eq!(result.counts.invalid_record, 1);
        assert_eq!(result.counts.not_recorded, 0);
    }
    #[test]
    fn maximal_valid_batch_keeps_every_status_owner_and_inflight_flag_inside_the_total_budget() {
        let lab = Lab::new();
        let outbox = Outbox::open(&lab.0).unwrap();
        let mut ids = Vec::new();
        for n in 0..MAX_OPERATION_IDS {
            let id = format!("{}{:03}", "a".repeat(93), n);
            save(
                &outbox,
                &id,
                Status::Attempting,
                scope(Platform::Bilibili, &"\\".repeat(128)),
            );
            ids.push(id);
        }
        let result = summarize(&lab.0, &ids).unwrap();
        assert_eq!(result.counts.unknown, 128);
        assert!(result.details_omitted);
        assert_eq!(result.actions.len(), 128);
        assert!(
            result
                .actions
                .iter()
                .all(|a| a.scope.is_some() && a.execution_may_be_in_flight)
        );
        assert!(serde_json::to_vec(&result).unwrap().len() <= MAX_SUMMARY_BYTES);
    }
}
