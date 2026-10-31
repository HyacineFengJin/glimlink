// SPDX-License-Identifier: MIT
//! Explicit management QR orchestration. Never invoked by Auto authentication.
use crate::{
    Client, LoginOutcome, LoginSource, Result, Session,
    qr::{self, QrLogin, QrState},
};
use serde_json::json;
use std::{
    fs,
    os::unix::fs::{DirBuilderExt, PermissionsExt},
    path::{Component, Path},
    sync::atomic::{AtomicBool, Ordering},
    time::{Duration, Instant},
};

const LOGIN_WINDOW: Duration = Duration::from_secs(180);
const POLL_INTERVAL: Duration = Duration::from_secs(3);

fn check(deadline: Instant, stop: &AtomicBool) -> Result<()> {
    if stop.load(Ordering::Relaxed) {
        return Err("QR login canceled; existing saved session was kept".into());
    }
    if Instant::now() >= deadline {
        return Err("QR login timed out; existing saved session was kept".into());
    }
    Ok(())
}
fn output_directory(path: &Path) -> Result<()> {
    if !path.is_absolute()
        || path
            .components()
            .any(|part| matches!(part, Component::ParentDir))
    {
        return Err("QR output must be an absolute private directory".into());
    }
    let mut prefix = std::path::PathBuf::new();
    for part in path.components() {
        prefix.push(part.as_os_str());
        match fs::symlink_metadata(&prefix) {
            Ok(meta) if meta.file_type().is_symlink() => {
                return Err("QR output directory cannot contain symlinks".into());
            }
            Ok(_) => {}
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
            Err(_) => return Err("cannot inspect QR output directory".into()),
        }
    }
    fs::DirBuilder::new()
        .recursive(true)
        .mode(0o700)
        .create(path)
        .map_err(|_| "cannot create private QR output directory")?;
    let meta = fs::symlink_metadata(path).map_err(|_| "cannot inspect QR output permissions")?;
    if !meta.is_dir() || meta.file_type().is_symlink() || meta.permissions().mode() & 0o077 != 0 {
        return Err("QR output directory must be real and private (0700)".into());
    }
    Ok(())
}
fn write_code(output: &Path, code: &qr::QrCode) -> Result<std::path::PathBuf> {
    if code.png.is_empty() || code.png.len() > u16::MAX as usize || code.url.len() > 4096 {
        return Err("QR response image or URL exceeds the private artifact limit".into());
    }
    let image = output.join("qrcode.png");
    crate::session::write_private_bytes(&image, &code.png)?;
    crate::session::write_private_json(&output.join("qrcode.json"), &json!({"url":code.url}))?;
    Ok(image)
}
fn validate_scanned_account(previous: &Session, uin: u32) -> Result<()> {
    if uin == 0 || previous.uin != 0 && previous.uin != uin {
        return Err(
            "QR account differs from the saved account; existing saved session was kept".into(),
        );
    }
    Ok(())
}
fn credential_session(
    previous: &Session,
    confirmed: u32,
    credentials: &qr::LoginCredentials,
    sequence: u32,
) -> Result<Session> {
    validate_scanned_account(previous, confirmed)?;
    if credentials.uin != confirmed
        || credentials.uid.is_empty()
        || credentials.uid.len() > 128
        || !credentials.uid.is_ascii()
        || credentials
            .uid
            .bytes()
            .any(|c| c.is_ascii_control() || c.is_ascii_whitespace())
        || !previous.uid.is_empty() && previous.uid != credentials.uid
    {
        return Err("QR credential identity mismatch; existing saved session was kept".into());
    }
    // Preserve forward-compatible session fields and existing private material;
    // the candidate is not installed until all required credentials validate.
    let mut candidate = previous.clone();
    candidate.uin = confirmed;
    candidate.sequence = sequence;
    candidate.clear_outer();
    candidate.set_credentials(
        &credentials.tgt,
        &credentials.d2,
        &credentials.d2_key,
        &credentials.temp_pwd,
        Some(&credentials.uid),
    )?;
    candidate.set_tgtgt(&credentials.tgtgt);
    Ok(candidate)
}
fn wait_poll(deadline: Instant, stop: &AtomicBool) -> Result<()> {
    let next = (Instant::now() + POLL_INTERVAL).min(deadline);
    loop {
        check(deadline, stop)?;
        let remaining = next.saturating_duration_since(Instant::now());
        if remaining.is_zero() {
            return Ok(());
        }
        std::thread::sleep(remaining.min(Duration::from_millis(100)));
    }
}

/// Explicit human-management entry. No account or device is silently replaced.
/// The 180-second deadline covers key retrieval and SSO waits. Callbacks should
/// return promptly; this function itself never prints, displays or launches apps.
/// `on_qr` receives a private 0600 PNG path only after both QR artifacts persist.
pub fn login_qr(
    client: &mut Client,
    output_dir: &Path,
    stop: &AtomicBool,
    mut on_qr: impl FnMut(&Path),
    mut on_status: impl FnMut(QrState),
) -> Result<LoginOutcome> {
    let deadline = Instant::now() + LOGIN_WINDOW;
    check(deadline, stop)?;
    output_directory(output_dir)?;
    let previous = client.session.clone();
    client.generation = client
        .generation
        .checked_add(1)
        .ok_or("QQ login generation exhausted")?;
    client.online = false;
    let result = (|| {
        client.require_signer()?;
        check(deadline, stop)?;
        let (version, public_key) = crate::key_rotation::fetch_server_key_until(deadline, stop)?;
        check(deadline, stop)?;
        let mut login = QrLogin::with_server_key_profile(
            client.device(),
            version,
            &public_key,
            previous.app_profile,
        )?;
        // QR fetch/poll use the pinned protocol's empty outer tickets. The
        // saved identity is held separately and checked before wtlogin.login.
        let sequence = client.session.sequence;
        client.session = Session::empty();
        client.session.app_profile = previous.app_profile;
        client.session.sequence = sequence;
        let request = login.fetch_request(crate::unix_seconds()?)?;
        let reply = client.request_until(qr::TRANS_EMP, &request, deadline, stop)?;
        let code = login.handle_fetch(&reply)?;
        check(deadline, stop)?;
        let path = write_code(output_dir, &code)?;
        on_qr(&path);
        check(deadline, stop)?;
        let mut last = None;
        loop {
            check(deadline, stop)?;
            let request = login.poll_request(crate::unix_seconds()?)?;
            let reply = client.request_until(qr::TRANS_EMP, &request, deadline, stop)?;
            let state = login.handle_poll(&reply)?;
            if let QrState::Confirmed { uin } = state {
                validate_scanned_account(&previous, uin)?;
            }
            if last != Some(state) {
                on_status(state);
                last = Some(state);
            }
            match state {
                QrState::Confirmed { uin } => {
                    check(deadline, stop)?;
                    client.session.uin = uin;
                    let request = login.login_request()?;
                    let reply = client.request_until(qr::LOGIN, &request, deadline, stop)?;
                    let credentials = login.handle_login(&reply)?;
                    client.session =
                        credential_session(&previous, uin, &credentials, client.session.sequence)?;
                    client.register_until(deadline, stop)?;
                    check(deadline, stop)?;
                    client.persist_session()?;
                    return Ok(LoginOutcome::Online {
                        source: LoginSource::Qr,
                    });
                }
                QrState::Expired | QrState::Canceled => {
                    return Err(
                        "QR code expired or canceled; existing saved session was kept".into(),
                    );
                }
                QrState::Unknown(_) => {
                    return Err("unknown QR login state; existing saved session was kept".into());
                }
                _ => wait_poll(deadline, stop)?,
            }
        }
    })();
    if result.is_err() {
        let sequence = client.session.sequence;
        client.session = previous;
        client.session.sequence = sequence;
        client.online = false;
    }
    result
}

#[cfg(test)]
mod tests {
    use super::*;
    fn previous() -> Session {
        let mut session = Session::empty();
        session.uin = 12345;
        session.uid = "u_fixture_only".into();
        session.sequence = 7;
        session
            .set_credentials(&[1], &[2], &[3; 16], &[4], None)
            .unwrap();
        session
    }
    fn credentials() -> qr::LoginCredentials {
        qr::LoginCredentials {
            uin: 12345,
            uid: "u_fixture_only".into(),
            tgt: vec![5],
            d2: vec![6],
            d2_key: [7; 16],
            tgtgt: [8; 16],
            temp_pwd: vec![9],
            nickname: "fixture".into(),
        }
    }
    #[test]
    fn wrong_scan_and_mismatched_uid_fail_without_modifying_saved_identity() {
        let previous = previous();
        let original = serde_json::to_value(&previous).unwrap();
        assert!(validate_scanned_account(&previous, 12346).is_err());
        assert!(validate_scanned_account(&previous, 0).is_err());
        let mut wrong = credentials();
        wrong.uid = "u_another_fixture".into();
        assert!(credential_session(&previous, 12345, &wrong, 10).is_err());
        wrong = credentials();
        wrong.uin = 12346;
        assert!(credential_session(&previous, 12345, &wrong, 10).is_err());
        assert_eq!(serde_json::to_value(&previous).unwrap(), original);
    }
    #[test]
    fn validated_same_account_candidate_keeps_sequence_and_new_credentials() {
        let previous = previous();
        let candidate = credential_session(&previous, 12345, &credentials(), 12).unwrap();
        assert_eq!(candidate.uin, previous.uin);
        assert_eq!(candidate.uid, previous.uid);
        assert_eq!(candidate.sequence, 12);
        assert_eq!(candidate.tgt().unwrap(), [5]);
        assert_eq!(candidate.d2().unwrap(), [6]);
        assert_eq!(candidate.key().unwrap(), [7; 16]);
        assert_eq!(candidate.temp_pwd().unwrap(), [9]);
        assert_eq!(previous.tgt().unwrap(), [1]);
    }
    #[test]
    fn cancellation_and_expired_deadlines_do_not_start_waits() {
        assert!(
            wait_poll(
                Instant::now() + Duration::from_secs(180),
                &AtomicBool::new(true)
            )
            .is_err()
        );
        assert!(wait_poll(Instant::now(), &AtomicBool::new(false)).is_err());
        assert!(output_directory(Path::new("relative/path")).is_err());
    }
    #[test]
    fn qr_artifacts_are_private_and_do_not_touch_the_session_file() {
        let mut nonce = [0; 8];
        crate::tea::random(&mut nonce).unwrap();
        let directory =
            std::env::temp_dir().join(format!("qq-qr-fixture-{:x}", u64::from_le_bytes(nonce)));
        output_directory(&directory).unwrap();
        let saved = directory.join("session.json");
        previous().save(&saved).unwrap();
        let bytes = fs::read(&saved).unwrap();
        let code = qr::QrCode {
            png: b"synthetic-png".to_vec(),
            url: "https://example.invalid/fixture".into(),
        };
        let path = write_code(&directory, &code).unwrap();
        assert_eq!(
            fs::metadata(path).unwrap().permissions().mode() & 0o777,
            0o600
        );
        assert_eq!(
            fs::metadata(directory.join("qrcode.json"))
                .unwrap()
                .permissions()
                .mode()
                & 0o777,
            0o600
        );
        assert_eq!(fs::read(saved).unwrap(), bytes);
        fs::remove_dir_all(directory).unwrap();
    }
}
