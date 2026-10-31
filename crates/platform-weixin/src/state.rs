// SPDX-License-Identifier: MIT
use crate::{
    Error, LoginProfile, Result,
    http::{base_host, validate_cookie, validate_redirect},
    model::{Contact, SessionInfo, SyncKey, Ticket},
};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use std::{
    fmt,
    fs::{self, File, OpenOptions},
    io::{Read, Write},
    os::unix::fs::{DirBuilderExt, OpenOptionsExt, PermissionsExt},
    path::Path,
    time::{SystemTime, UNIX_EPOCH},
};

#[derive(Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct Cookie {
    pub name: String,
    pub value: String,
    pub domain: String,
    pub path: String,
    pub host_only: bool,
    pub expires_at: Option<u64>,
}

#[derive(Clone)]
pub struct LoginAttempt {
    schema: u32,
    profile: LoginProfile,
    pub(crate) uuid: String,
    pub(crate) device_id: String,
    pub(crate) created_at: u64,
    pub(crate) scanned: bool,
    pub(crate) redirect: Option<String>,
    pub(crate) cookies: Vec<Cookie>,
}
#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct LoginAttemptFile {
    schema: u32,
    #[serde(default)]
    profile: LoginProfile,
    uuid: String,
    device_id: String,
    created_at: u64,
    scanned: bool,
    redirect: Option<String>,
    cookies: Vec<Cookie>,
}
impl fmt::Debug for LoginAttempt {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("LoginAttempt")
            .field("profile", &self.profile)
            .field("scanned", &self.scanned)
            .field("confirmed", &self.redirect.is_some())
            .finish_non_exhaustive()
    }
}
impl LoginAttempt {
    pub(crate) fn new(uuid: String, cookies: Vec<Cookie>, profile: LoginProfile) -> Result<Self> {
        let random = random_bytes()?;
        let mut device_id = "e".to_owned();
        for b in random.iter().take(15) {
            device_id.push((b'0' + b % 10) as char);
        }
        Ok(Self {
            schema: 1,
            profile,
            uuid,
            device_id,
            created_at: now_seconds()?,
            scanned: false,
            redirect: None,
            cookies,
        })
    }
    pub(crate) fn validate(&self) -> Result<()> {
        if self.schema != 1
            || self.uuid.is_empty()
            || self.uuid.len() > 128
            || !self
                .uuid
                .bytes()
                .all(|b| b.is_ascii_alphanumeric() || b"=_-".contains(&b))
            || !valid_device(&self.device_id)
        {
            return Err(Error::InvalidState);
        }
        if let Some(url) = &self.redirect {
            validate_redirect(url)?;
        }
        validate_cookies(&self.cookies)
    }
    pub(crate) fn remaining_seconds(&self) -> Result<u64> {
        Ok(300_u64.saturating_sub(now_seconds()?.saturating_sub(self.created_at)))
    }
    pub(crate) fn ensure_fresh(&self) -> Result<()> {
        if self.created_at > now_seconds()?.saturating_add(120) || self.remaining_seconds()? == 0 {
            return Err(Error::ExpiredAttempt);
        }
        Ok(())
    }
    pub fn save(&self, path: &Path) -> Result<()> {
        self.validate()?;
        save(
            path,
            &LoginAttemptFile {
                schema: self.schema,
                profile: self.profile,
                uuid: self.uuid.clone(),
                device_id: self.device_id.clone(),
                created_at: self.created_at,
                scanned: self.scanned,
                redirect: self.redirect.clone(),
                cookies: self.cookies.clone(),
            },
        )
    }
    pub fn load(path: &Path) -> Result<Self> {
        let file: LoginAttemptFile = load(path)?;
        let value = Self {
            schema: file.schema,
            profile: file.profile,
            uuid: file.uuid,
            device_id: file.device_id,
            created_at: file.created_at,
            scanned: file.scanned,
            redirect: file.redirect,
            cookies: file.cookies,
        };
        value.validate()?;
        Ok(value)
    }
    pub fn profile(&self) -> LoginProfile {
        self.profile
    }
}

/// This handle deliberately does not implement public Serialize. The serialized
/// form is only accessible through private file save/load helpers.
#[derive(Clone)]
pub struct Session {
    pub(crate) profile: LoginProfile,
    pub(crate) host: String,
    pub(crate) skey: String,
    pub(crate) sid: String,
    pub(crate) uin: u64,
    pub(crate) pass_ticket: String,
    pub(crate) device_id: String,
    pub(crate) cookies: Vec<Cookie>,
    pub(crate) sync_key: SyncKey,
    pub(crate) user: Contact,
}
#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct SessionFile {
    schema: u32,
    #[serde(default)]
    profile: LoginProfile,
    host: String,
    skey: String,
    sid: String,
    uin: u64,
    pass_ticket: String,
    device_id: String,
    cookies: Vec<Cookie>,
    sync_key: SyncKey,
    user: Contact,
}
impl fmt::Debug for Session {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Session")
            .field("profile", &self.profile)
            .field("has_cursor", &!self.sync_key.is_empty())
            .finish_non_exhaustive()
    }
}
impl Session {
    pub(crate) fn new(
        host: String,
        ticket: Ticket,
        device_id: String,
        cookies: Vec<Cookie>,
    ) -> Result<Self> {
        let value = Self {
            profile: LoginProfile::Web,
            host,
            skey: ticket.skey,
            sid: ticket.sid,
            uin: ticket.uin,
            pass_ticket: ticket.pass_ticket,
            device_id,
            cookies,
            sync_key: Vec::new(),
            user: Contact::default(),
        };
        value.validate_credentials()?;
        Ok(value)
    }
    fn validate_credentials(&self) -> Result<()> {
        if !base_host(&self.host)
            || self.uin == 0
            || !valid_device(&self.device_id)
            || [&self.skey, &self.sid, &self.pass_ticket]
                .iter()
                .any(|v| v.is_empty() || v.len() > 8192 || v.chars().any(char::is_control))
        {
            return Err(Error::InvalidState);
        }
        validate_cookies(&self.cookies)
    }
    pub(crate) fn validate(&self) -> Result<()> {
        self.validate_credentials()?;
        if self.user.username.is_empty()
            || self.user.username.len() > 256
            || self.user.username.chars().any(char::is_control)
            || self.user.nickname.len() > 4096
            || self.user.remark.len() > 4096
            || self.sync_key.is_empty()
            || self.sync_key.len() > 1024
        {
            return Err(Error::InvalidState);
        }
        let mut seen = std::collections::BTreeSet::new();
        if self.sync_key.iter().any(|(key, _)| !seen.insert(key)) {
            return Err(Error::InvalidState);
        }
        Ok(())
    }
    pub(crate) fn endpoint(&self, name: &str) -> String {
        format!("https://{}/cgi-bin/mmwebwx-bin/{name}", self.host)
    }
    pub(crate) fn base_request(&self) -> Value {
        json!({"Uin":self.uin,"Sid":self.sid,"Skey":self.skey,"DeviceID":self.device_id})
    }
    pub(crate) fn sync_string(&self) -> String {
        self.sync_key
            .iter()
            .map(|(k, v)| format!("{k}_{v}"))
            .collect::<Vec<_>>()
            .join("|")
    }
    pub fn info(&self) -> SessionInfo {
        SessionInfo {
            user: self.user.clone(),
            cursor_entries: self.sync_key.len(),
        }
    }
    /// Control-plane identity binding only: do not expose this value to models/logs.
    pub fn stable_identity(&self) -> String {
        format!("webwx:{}", self.uin)
    }
    pub fn profile(&self) -> LoginProfile {
        self.profile
    }
    pub fn save(&self, path: &Path) -> Result<()> {
        self.validate()?;
        save(
            path,
            &SessionFile {
                schema: 1,
                profile: self.profile,
                host: self.host.clone(),
                skey: self.skey.clone(),
                sid: self.sid.clone(),
                uin: self.uin,
                pass_ticket: self.pass_ticket.clone(),
                device_id: self.device_id.clone(),
                cookies: self.cookies.clone(),
                sync_key: self.sync_key.clone(),
                user: self.user.clone(),
            },
        )
    }
    pub fn load(path: &Path) -> Result<Self> {
        let file: SessionFile = load(path)?;
        if file.schema != 1 {
            return Err(Error::InvalidState);
        }
        let value = Self {
            profile: file.profile,
            host: file.host,
            skey: file.skey,
            sid: file.sid,
            uin: file.uin,
            pass_ticket: file.pass_ticket,
            device_id: file.device_id,
            cookies: file.cookies,
            sync_key: file.sync_key,
            user: file.user,
        };
        value.validate()?;
        Ok(value)
    }
}
fn validate_cookies(cookies: &[Cookie]) -> Result<()> {
    if cookies.len() > 128
        || cookies
            .iter()
            .map(|c| c.name.len() + c.value.len() + c.domain.len() + c.path.len())
            .sum::<usize>()
            > 32768
    {
        return Err(Error::Limit);
    }
    for cookie in cookies {
        validate_cookie(cookie)?;
    }
    Ok(())
}
fn valid_device(value: &str) -> bool {
    value.len() == 16 && value.starts_with('e') && value[1..].bytes().all(|b| b.is_ascii_digit())
}
pub(crate) fn now_millis() -> Result<u64> {
    u64::try_from(
        SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map_err(|_| Error::InvalidState)?
            .as_millis(),
    )
    .map_err(|_| Error::InvalidState)
}
fn now_seconds() -> Result<u64> {
    Ok(now_millis()? / 1000)
}
fn random_bytes() -> Result<[u8; 16]> {
    let mut random = [0; 16];
    File::open("/dev/urandom")
        .and_then(|mut f| f.read_exact(&mut random))
        .map_err(|_| Error::Io)?;
    Ok(random)
}
pub(crate) fn ensure_private_parent(path: &Path) -> Result<()> {
    let parent = path.parent().ok_or(Error::InvalidInput)?;
    match fs::DirBuilder::new()
        .recursive(true)
        .mode(0o700)
        .create(parent)
    {
        Ok(()) => {}
        Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => {}
        Err(_) => return Err(Error::Io),
    }
    let meta = fs::symlink_metadata(parent).map_err(|_| Error::Io)?;
    if !meta.is_dir() || meta.permissions().mode() & 0o077 != 0 {
        return Err(Error::InvalidState);
    }
    Ok(())
}
fn save(path: &Path, value: &impl Serialize) -> Result<()> {
    ensure_private_parent(path)?;
    let bytes = serde_json::to_vec(value).map_err(|_| Error::InvalidState)?;
    if bytes.len() > 256 * 1024 {
        return Err(Error::Limit);
    }
    if let Ok(meta) = fs::symlink_metadata(path) {
        if !meta.is_file() || meta.permissions().mode() & 0o077 != 0 {
            return Err(Error::InvalidState);
        }
    }
    let nonce = random_bytes()?
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect::<String>();
    let tmp = path
        .parent()
        .ok_or(Error::InvalidInput)?
        .join(format!(".webwx-{nonce}.tmp"));
    let result = (|| {
        let mut file = OpenOptions::new()
            .write(true)
            .create_new(true)
            .mode(0o600)
            .custom_flags(libc::O_NOFOLLOW | libc::O_NONBLOCK)
            .open(&tmp)
            .map_err(|_| Error::Io)?;
        file.write_all(&bytes)
            .and_then(|_| file.sync_all())
            .map_err(|_| Error::Io)?;
        fs::rename(&tmp, path).map_err(|_| Error::Io)?;
        File::open(path.parent().ok_or(Error::InvalidInput)?)
            .and_then(|f| f.sync_all())
            .map_err(|_| Error::Io)
    })();
    if result.is_err() {
        let _ = fs::remove_file(tmp);
    }
    result
}
fn load<T: for<'de> Deserialize<'de>>(path: &Path) -> Result<T> {
    let meta = fs::symlink_metadata(path).map_err(|_| Error::Io)?;
    if !meta.is_file() || meta.len() > 256 * 1024 || meta.permissions().mode() & 0o077 != 0 {
        return Err(Error::InvalidState);
    }
    let file = OpenOptions::new()
        .read(true)
        .custom_flags(libc::O_NOFOLLOW | libc::O_NONBLOCK)
        .open(path)
        .map_err(|_| Error::Io)?;
    if !file.metadata().map_err(|_| Error::Io)?.is_file() {
        return Err(Error::InvalidState);
    }
    let mut bytes = Vec::new();
    file.take(256 * 1024 + 1)
        .read_to_end(&mut bytes)
        .map_err(|_| Error::Io)?;
    if bytes.len() > 256 * 1024 {
        return Err(Error::Limit);
    }
    serde_json::from_slice(&bytes).map_err(|_| Error::InvalidState)
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn old_attempt_files_default_to_web_and_profile_roundtrip_is_explicit() {
        let dir = std::env::temp_dir().join(format!(
            "webwx-profile-{}",
            random_bytes()
                .unwrap()
                .iter()
                .map(|b| format!("{b:02x}"))
                .collect::<String>()
        ));
        fs::DirBuilder::new().mode(0o700).create(&dir).unwrap();
        let path = dir.join("attempt.json");
        let desktop = LoginAttempt::new(
            "synthetic_uuid==".into(),
            Vec::new(),
            LoginProfile::DesktopCompatibility,
        )
        .unwrap();
        desktop.save(&path).unwrap();
        assert_eq!(
            LoginAttempt::load(&path).unwrap().profile(),
            LoginProfile::DesktopCompatibility
        );
        let mut value: serde_json::Value =
            serde_json::from_slice(&fs::read(&path).unwrap()).unwrap();
        value.as_object_mut().unwrap().remove("profile");
        fs::write(&path, serde_json::to_vec(&value).unwrap()).unwrap();
        assert_eq!(
            LoginAttempt::load(&path).unwrap().profile(),
            LoginProfile::Web
        );
        fs::remove_dir_all(dir).unwrap();
    }
    #[test]
    fn private_roundtrip_redacts_debug_and_rejects_permissions() {
        let dir = std::env::temp_dir().join(format!(
            "webwx-state-{}",
            random_bytes()
                .unwrap()
                .iter()
                .map(|b| format!("{b:02x}"))
                .collect::<String>()
        ));
        fs::create_dir(&dir).unwrap();
        fs::set_permissions(&dir, fs::Permissions::from_mode(0o700)).unwrap();
        let mut session = Session::new(
            "wx.qq.com".into(),
            Ticket {
                skey: "private-skey".into(),
                sid: "private-sid".into(),
                uin: 123,
                pass_ticket: "private-ticket".into(),
            },
            "e123456789012345".into(),
            Vec::new(),
        )
        .unwrap();
        session.user = Contact {
            username: "@self".into(),
            ..Default::default()
        };
        session.sync_key = vec![(1, 2)];
        let path = dir.join("session.json");
        session.save(&path).unwrap();
        let restored = Session::load(&path).unwrap();
        assert_eq!(restored.stable_identity(), session.stable_identity());
        assert_eq!(restored.sync_string(), "1_2");
        assert!(!format!("{session:?}").contains("private-"));
        assert_eq!(
            fs::metadata(&path).unwrap().permissions().mode() & 0o777,
            0o600
        );
        fs::set_permissions(&path, fs::Permissions::from_mode(0o644)).unwrap();
        assert!(Session::load(&path).is_err());
        fs::remove_dir_all(dir).unwrap();
    }
}
