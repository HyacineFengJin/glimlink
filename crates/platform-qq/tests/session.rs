// SPDX-License-Identifier: AGPL-3.0-only
//! Synthetic private files only; no saved account state is used.
use platform_qq::{Device, Session};
use serde_json::json;
use std::{fs, os::unix::fs::PermissionsExt};

#[test]
fn legacy_session_json_preserves_credentials_unknown_fields_and_sequence() {
    let mut nonce = [0; 8];
    platform_qq::tea::random(&mut nonce).unwrap();
    let dir = std::env::temp_dir().join(format!(
        "qq-session-fixture-{:x}",
        u64::from_le_bytes(nonce)
    ));
    fs::create_dir(&dir).unwrap();
    fs::set_permissions(&dir, fs::Permissions::from_mode(0o700)).unwrap();
    let path = dir.join("session.json");
    let value = json!({"version":1,"uin":12345,"uid":"u_fixture_only","sequence":38,"tgt":"AQ==","d2":"Ag==","d2_key":"AAAAAAAAAAAAAAAAAAAAAA==","temp_pwd":"AAH/","tgtgt":"Aw==","cookies":"fixture-cookie","future_extension":{"value":"preserved"}});
    fs::write(&path, serde_json::to_vec(&value).unwrap()).unwrap();
    fs::set_permissions(&path, fs::Permissions::from_mode(0o600)).unwrap();
    let session = Session::load(&path).unwrap();
    assert_eq!(session.sequence, 38);
    assert_eq!(session.temp_pwd().unwrap(), [0, 1, 255]);
    assert!(session.has_tickets().unwrap());
    session.save(&path).unwrap();
    let roundtrip: serde_json::Value = serde_json::from_slice(&fs::read(&path).unwrap()).unwrap();
    assert_eq!(roundtrip, value);
    assert_eq!(
        fs::metadata(&path).unwrap().permissions().mode() & 0o777,
        0o600
    );
    fs::set_permissions(&path, fs::Permissions::from_mode(0o644)).unwrap();
    assert!(Session::load(&path).is_err());
    fs::remove_dir_all(dir).unwrap();
}
#[test]
fn absent_uid_is_never_replaced_by_numeric_qq_account_and_guid_is_strict() {
    let mut session = Session::empty();
    session.uin = 12345;
    assert!(
        session
            .set_credentials(&[1], &[2], &[0; 16], &[3], None)
            .is_err()
    );
    assert!(session.uid.is_empty());
    let device: Device = serde_json::from_value(
        json!({"guid":"00".repeat(16),"device_name":"fixture","system_kernel":"Linux"}),
    )
    .unwrap();
    assert_eq!(device.guid_bytes().unwrap(), vec![0; 16]);
    assert_eq!(device.kernel_version, "6.1.0");
    let mut bad = device;
    bad.guid = "喵".into();
    assert!(bad.guid_bytes().is_err());
}
