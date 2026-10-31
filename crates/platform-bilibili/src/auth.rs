// SPDX-License-Identifier: MIT
//! Human-managed web QR authentication or explicit cookie import. No password
//! capture, automatic re-login, cookie refresh, or browser/account discovery.
use crate::{Config, Error, Result, api_data};
use serde::{Deserialize, Serialize, de::DeserializeOwned};
use serde_json::Value;
use std::{
    collections::BTreeMap,
    fmt,
    fs::{self, File, OpenOptions},
    io::{Read, Write},
    os::unix::fs::{OpenOptionsExt, PermissionsExt},
    path::Path,
    time::{Duration, SystemTime, UNIX_EPOCH},
};
use ureq::{Agent, http::Uri};

const QR_LIFETIME: u64 = 180;
const PRIVATE_LIMIT: u64 = 64 * 1024;
const COOKIE_NAMES: [&str; 5] = [
    "DedeUserID",
    "DedeUserID__ckMd5",
    "SESSDATA",
    "bili_jct",
    "sid",
];

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum LoginStatus {
    Waiting,
    Scanned,
    Expired,
    Confirmed,
}

/// Deliberately not publicly serializable: URL and key are login capabilities.
pub struct LoginAttempt {
    record: AttemptRecord,
}
#[derive(Serialize, Deserialize)]
struct AttemptRecord {
    schema: u32,
    qr_url: String,
    key: String,
    created_at: u64,
    last_poll: Option<u64>,
    status: LoginStatus,
    session: Option<SessionRecord>,
}
impl fmt::Debug for LoginAttempt {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("LoginAttempt")
            .field("status", &self.record.status)
            .field("credentials", &"[redacted]")
            .finish()
    }
}
impl LoginAttempt {
    pub fn qr_url(&self) -> &str {
        &self.record.qr_url
    }
    pub fn expires_in_seconds(&self) -> u64 {
        self.record
            .created_at
            .saturating_add(QR_LIFETIME)
            .saturating_sub(now().unwrap_or(u64::MAX))
    }
    pub fn status(&self) -> LoginStatus {
        self.record.status
    }
    pub fn save(&self, path: &Path) -> Result<()> {
        self.validate()?;
        save_private(path, &self.record)
    }
    pub fn load(path: &Path) -> Result<Self> {
        let attempt = Self {
            record: load_private(path)?,
        };
        attempt.validate()?;
        Ok(attempt)
    }
    pub fn take_session(&mut self) -> Result<Session> {
        if self.record.status != LoginStatus::Confirmed {
            return Err(Error::LoginState);
        }
        let session = Session {
            record: self.record.session.take().ok_or(Error::LoginState)?,
        };
        session.validate()?;
        Ok(session)
    }
    fn validate(&self) -> Result<()> {
        if self.record.schema != 1 || self.record.created_at > now()?.saturating_add(10) {
            return Err(Error::LoginState);
        }
        validate_key(&self.record.key)?;
        validate_qr(&self.record.qr_url, &self.record.key)?;
        if let Some(session) = &self.record.session {
            validate_session(session)?;
            if self.record.status != LoginStatus::Confirmed {
                return Err(Error::LoginState);
            }
        }
        Ok(())
    }
}

/// Credential-bearing state exposes only explicit, private persistence methods.
pub struct Session {
    record: SessionRecord,
}
#[derive(Serialize, Deserialize)]
struct SessionRecord {
    schema: u32,
    mid: u64,
    cookies: BTreeMap<String, String>,
    refresh_token: String,
    authenticated_at: u64,
}
impl fmt::Debug for Session {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Session")
            .field("credentials", &"[redacted]")
            .finish()
    }
}
impl Session {
    pub fn stable_identity(&self) -> String {
        self.record.mid.to_string()
    }
    pub fn mid(&self) -> u64 {
        self.record.mid
    }
    /// Supply only to the Bilibili Config; never expose this in model results.
    pub fn cookie_header(&self) -> String {
        self.record
            .cookies
            .iter()
            .map(|(k, v)| format!("{k}={v}"))
            .collect::<Vec<_>>()
            .join("; ")
    }
    pub fn save(&self, path: &Path) -> Result<()> {
        self.validate()?;
        save_private(path, &self.record)
    }
    pub fn load(path: &Path) -> Result<Self> {
        let session = Self {
            record: load_private(path)?,
        };
        session.validate()?;
        Ok(session)
    }
    fn validate(&self) -> Result<()> {
        validate_session(&self.record)
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AccountInfo {
    pub mid: u64,
    pub uname: String,
    pub level: Option<u32>,
    pub vip_status: Option<u32>,
}
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FavoriteFolder {
    pub id: u64,
    pub title: String,
    pub media_count: u64,
    pub attr: u64,
}
pub(crate) fn account_info(value: Value) -> Result<AccountInfo> {
    if value.get("isLogin").and_then(Value::as_bool) != Some(true) {
        return Err(Error::LoginRequired);
    }
    let mid = value
        .get("mid")
        .and_then(Value::as_u64)
        .filter(|mid| *mid > 0 && *mid <= i64::MAX as u64)
        .ok_or(Error::Schema("account UID"))?;
    let uname = value
        .get("uname")
        .and_then(Value::as_str)
        .filter(|s| s.len() <= 1024)
        .ok_or(Error::Schema("account name"))?
        .to_owned();
    let number = |value: Option<&Value>| -> Option<u32> {
        value
            .and_then(Value::as_u64)
            .and_then(|v| v.try_into().ok())
    };
    Ok(AccountInfo {
        mid,
        uname,
        level: number(value.pointer("/level_info/current_level")),
        vip_status: number(value.get("vipStatus")),
    })
}
pub(crate) fn favorite_folders(value: Value) -> Result<Vec<FavoriteFolder>> {
    let list = match value.get("list") {
        Some(Value::Array(list)) => list,
        Some(Value::Null) if value.get("count").and_then(Value::as_u64) == Some(0) => {
            return Ok(Vec::new());
        }
        _ => return Err(Error::Schema("favorite folder list")),
    };
    if list.len() > 512 {
        return Err(Error::JsonLimit);
    }
    let mut result = Vec::with_capacity(list.len());
    for value in list {
        let folder: FavoriteFolder =
            serde_json::from_value(value.clone()).map_err(|_| Error::Schema("favorite folder"))?;
        if folder.id == 0 || folder.title.len() > 4096 {
            return Err(Error::Schema("favorite folder identity"));
        }
        result.push(folder);
    }
    Ok(result)
}

pub struct AuthClient {
    agent: Agent,
    max_json_bytes: usize,
}
impl AuthClient {
    pub fn new(config: Config) -> Result<Self> {
        if !(1024..=8 * 1024 * 1024).contains(&config.max_json_bytes)
            || [config.request_timeout, config.connect_timeout]
                .iter()
                .any(|t| t.is_zero() || t.as_secs() > 600)
        {
            return Err(Error::InvalidConfig);
        }
        let agent = Agent::config_builder()
            .https_only(true)
            .max_redirects(0)
            .http_status_as_error(false)
            .proxy(None)
            .timeout_global(Some(config.request_timeout))
            .timeout_connect(Some(config.connect_timeout))
            .timeout_per_read(Some(Duration::from_secs(15)))
            .max_response_header_size(32 * 1024)
            .max_idle_connections(1)
            .max_idle_connections_per_host(1)
            .user_agent(
                "Mozilla/5.0 (X11; Linux x86_64) AppleWebKit/537.36 Chrome/131.0.0.0 Safari/537.36",
            )
            .build()
            .into();
        Ok(Self {
            agent,
            max_json_bytes: config.max_json_bytes.min(256 * 1024),
        })
    }
    /// Explicit operator import. Never starts QR login or changes a saved file.
    /// The current-user endpoint establishes the identity; a supplied DedeUserID
    /// is only a consistency claim and cannot select another account.
    pub fn import_cookie_header(&self, header: &str) -> Result<(Session, AccountInfo)> {
        let cookies = import_cookies(header)?;
        let mut config = Config::default();
        config.cookie = Some(
            cookies
                .iter()
                .map(|(k, v)| format!("{k}={v}"))
                .collect::<Vec<_>>()
                .join("; "),
        );
        let user = crate::Client::new(config)?.current_user()?;
        let session = imported_session(cookies, user.mid)?;
        Ok((session, user))
    }
    pub fn begin_login(&self) -> Result<LoginAttempt> {
        let (data, _) =
            self.get("https://passport.bilibili.com/x/passport-login/web/qrcode/generate")?;
        parse_begin(data, now()?)
    }
    /// One bounded poll. Waiting/scanned attempts may be polled no faster than
    /// every two seconds; terminal attempts never issue another request.
    pub fn poll_login(&self, attempt: &mut LoginAttempt) -> Result<LoginStatus> {
        attempt.validate()?;
        if matches!(
            attempt.record.status,
            LoginStatus::Confirmed | LoginStatus::Expired
        ) {
            return Ok(attempt.record.status);
        }
        let timestamp = now()?;
        if timestamp >= attempt.record.created_at.saturating_add(QR_LIFETIME) {
            attempt.record.status = LoginStatus::Expired;
            return Ok(LoginStatus::Expired);
        }
        if attempt
            .record
            .last_poll
            .is_some_and(|last| timestamp.saturating_sub(last) < 2)
        {
            return Err(Error::PollTooSoon);
        }
        attempt.record.last_poll = Some(timestamp);
        let (data, cookies) = self.get(&format!(
            "https://passport.bilibili.com/x/passport-login/web/qrcode/poll?qrcode_key={}",
            attempt.record.key
        ))?;
        parse_poll(attempt, data, &cookies, timestamp)
    }
    fn get(&self, url: &str) -> Result<(Value, Vec<String>)> {
        validate_auth_url(url)?;
        let mut response = self
            .agent
            .get(url)
            .header("Referer", "https://www.bilibili.com/")
            .call()
            .map_err(|_| Error::Network)?;
        let status = response.status().as_u16();
        if status == 412 || status == 429 {
            return Err(Error::RiskControl(status.into()));
        }
        // Do not follow any returned Location, including the game SSO URL.
        if status != 200 {
            return Err(Error::HttpStatus(status));
        }
        let cookies = response
            .headers()
            .get_all("Set-Cookie")
            .iter()
            .map(|v| v.to_str().map(str::to_owned).map_err(|_| Error::LoginState))
            .collect::<Result<Vec<_>>>()?;
        let mut bytes = Vec::new();
        response
            .body_mut()
            .as_reader()
            .take(self.max_json_bytes as u64 + 1)
            .read_to_end(&mut bytes)
            .map_err(|_| Error::Network)?;
        if bytes.len() > self.max_json_bytes {
            return Err(Error::JsonLimit);
        }
        let value = serde_json::from_slice(&bytes).map_err(|_| Error::InvalidJson)?;
        Ok((api_data(value, false)?, cookies))
    }
}
fn parse_begin(value: Value, timestamp: u64) -> Result<LoginAttempt> {
    let key = value
        .get("qrcode_key")
        .and_then(Value::as_str)
        .ok_or(Error::Schema("QR key"))?
        .to_owned();
    let qr_url = value
        .get("url")
        .and_then(Value::as_str)
        .ok_or(Error::Schema("QR URL"))?
        .to_owned();
    validate_key(&key)?;
    validate_qr(&qr_url, &key)?;
    Ok(LoginAttempt {
        record: AttemptRecord {
            schema: 1,
            qr_url,
            key,
            created_at: timestamp,
            last_poll: None,
            status: LoginStatus::Waiting,
            session: None,
        },
    })
}
fn parse_poll(
    attempt: &mut LoginAttempt,
    data: Value,
    headers: &[String],
    timestamp: u64,
) -> Result<LoginStatus> {
    let status = match data
        .get("code")
        .and_then(Value::as_i64)
        .ok_or(Error::Schema("QR poll code"))?
    {
        86101 => LoginStatus::Waiting,
        86090 => LoginStatus::Scanned,
        86038 => LoginStatus::Expired,
        0 => {
            let cookies = parse_cookies(headers)?;
            let mid = cookies
                .get("DedeUserID")
                .and_then(|v| v.parse::<u64>().ok())
                .filter(|mid| *mid > 0 && *mid <= i64::MAX as u64)
                .ok_or(Error::Schema("login UID"))?;
            let refresh_token = data
                .get("refresh_token")
                .and_then(Value::as_str)
                .unwrap_or_default()
                .to_owned();
            let record = SessionRecord {
                schema: 1,
                mid,
                cookies,
                refresh_token,
                authenticated_at: timestamp,
            };
            validate_session(&record)?;
            attempt.record.session = Some(record);
            LoginStatus::Confirmed
        }
        code => {
            return Err(match code {
                -352 | -412 | -401 => Error::RiskControl(code),
                _ => Error::Platform(code),
            });
        }
    };
    attempt.record.status = status;
    Ok(status)
}
fn validate_key(key: &str) -> Result<()> {
    if key.len() != 32 || !key.bytes().all(|v| v.is_ascii_hexdigit()) {
        Err(Error::LoginState)
    } else {
        Ok(())
    }
}
fn uri(value: &str) -> Result<Uri> {
    if value.len() > 16384
        || value.contains(['\\', '#'])
        || value.bytes().any(|v| v <= 32 || v == 127)
    {
        return Err(Error::UrlPolicy);
    }
    let parsed: Uri = value.parse().map_err(|_| Error::UrlPolicy)?;
    if parsed.scheme_str() != Some("https")
        || !matches!(
            parsed.host(),
            Some("passport.bilibili.com" | "account.bilibili.com")
        )
        || !matches!(parsed.port_u16(), None | Some(443))
        || parsed.authority().is_none_or(|v| v.as_str().contains('@'))
    {
        return Err(Error::UrlPolicy);
    }
    Ok(parsed)
}
fn validate_auth_url(value: &str) -> Result<()> {
    let parsed = uri(value)?;
    if parsed.host() != Some("passport.bilibili.com") {
        return Err(Error::UrlPolicy);
    }
    match parsed.path() {
        "/x/passport-login/web/qrcode/generate" if parsed.query().is_none() => Ok(()),
        "/x/passport-login/web/qrcode/poll" => validate_key(
            parsed
                .query()
                .and_then(|v| v.strip_prefix("qrcode_key="))
                .ok_or(Error::UrlPolicy)?,
        ),
        _ => Err(Error::UrlPolicy),
    }
}
fn validate_qr(value: &str, key: &str) -> Result<()> {
    let parsed = uri(value)?;
    let keys: Vec<_> = parsed
        .query()
        .unwrap_or_default()
        .split('&')
        .filter_map(|v| v.strip_prefix("qrcode_key="))
        .collect();
    let route = matches!(
        (parsed.host(), parsed.path()),
        (Some("passport.bilibili.com"), "/h5-app/passport/login/scan")
            | (Some("account.bilibili.com"), "/h5/account-h5/auth/scan-web")
    );
    if !route || keys != [key] {
        return Err(Error::UrlPolicy);
    }
    Ok(())
}
fn cookie_value(value: &str) -> bool {
    !value.is_empty()
        && value.len() <= 8192
        && value
            .bytes()
            .all(|v| (0x21..=0x7e).contains(&v) && !b"\";,\\".contains(&v))
}
fn parse_cookies(headers: &[String]) -> Result<BTreeMap<String, String>> {
    let mut result = BTreeMap::new();
    if headers.len() > 32 {
        return Err(Error::LoginState);
    }
    for header in headers {
        if header.len() > 16384 || header.bytes().any(|v| v < 32 || v == 127) {
            return Err(Error::LoginState);
        }
        let mut parts = header.split(';');
        let (name, value) = parts
            .next()
            .and_then(|s| s.trim().split_once('='))
            .ok_or(Error::LoginState)?;
        if !COOKIE_NAMES.contains(&name) {
            continue;
        }
        let mut domain = None;
        let mut path = None;
        let mut deleted = false;
        for part in parts {
            let (attribute, value) = part.trim().split_once('=').unwrap_or((part.trim(), ""));
            if attribute.eq_ignore_ascii_case("Domain") {
                if domain
                    .replace(
                        value
                            .strip_prefix('.')
                            .unwrap_or(value)
                            .to_ascii_lowercase(),
                    )
                    .is_some()
                {
                    return Err(Error::LoginState);
                }
            } else if attribute.eq_ignore_ascii_case("Path") {
                if path.replace(value).is_some() {
                    return Err(Error::LoginState);
                }
            } else if attribute.eq_ignore_ascii_case("Max-Age") {
                deleted = value.parse::<i64>().map_err(|_| Error::LoginState)? <= 0;
            }
        }
        if domain.as_deref() != Some("bilibili.com")
            || path != Some("/")
            || deleted
            || !cookie_value(value)
            || result.insert(name.into(), value.into()).is_some()
        {
            return Err(Error::LoginState);
        }
    }
    for name in &COOKIE_NAMES[..4] {
        if !result.contains_key(*name) {
            return Err(Error::Schema("missing login cookies"));
        }
    }
    Ok(result)
}
fn import_cookies(header: &str) -> Result<BTreeMap<String, String>> {
    let header = header.trim();
    let header = if header
        .get(..7)
        .is_some_and(|prefix| prefix.eq_ignore_ascii_case("Cookie:"))
    {
        header[7..].trim()
    } else {
        header
    };
    if header.is_empty() || header.len() > 24 * 1024 || header.chars().any(char::is_control) {
        return Err(Error::LoginState);
    }
    let mut result: BTreeMap<String, String> = BTreeMap::new();
    for (index, field) in header
        .split(';')
        .filter(|v| !v.trim().is_empty())
        .enumerate()
    {
        if index >= 128 {
            return Err(Error::LoginState);
        }
        let (name, value) = field.trim().split_once('=').ok_or(Error::LoginState)?;
        if name.is_empty()
            || name.len() > 128
            || !name
                .bytes()
                .all(|b| b.is_ascii_alphanumeric() || b"_-".contains(&b))
        {
            return Err(Error::LoginState);
        }
        if COOKIE_NAMES.contains(&name) {
            if !cookie_value(value) || result.insert(name.into(), value.into()).is_some() {
                return Err(Error::LoginState);
            }
        }
    }
    for required in ["SESSDATA", "bili_jct"] {
        if !result.contains_key(required) {
            return Err(Error::Schema("missing import cookies"));
        }
    }
    if result.get("DedeUserID").is_some_and(|v| {
        v.parse::<u64>()
            .ok()
            .is_none_or(|id| id == 0 || id > i64::MAX as u64)
    }) {
        return Err(Error::LoginState);
    }
    Ok(result)
}
fn imported_session(mut cookies: BTreeMap<String, String>, mid: u64) -> Result<Session> {
    if cookies
        .get("DedeUserID")
        .is_some_and(|id| id.parse::<u64>().ok() != Some(mid))
    {
        return Err(Error::LoginState);
    }
    cookies.insert("DedeUserID".into(), mid.to_string());
    let session = Session {
        record: SessionRecord {
            schema: 1,
            mid,
            cookies,
            refresh_token: String::new(),
            authenticated_at: now()?,
        },
    };
    session.validate()?;
    Ok(session)
}
fn validate_session(record: &SessionRecord) -> Result<()> {
    if record.schema != 1
        || record.mid == 0
        || record.mid > i64::MAX as u64
        || record.authenticated_at > now()?.saturating_add(10)
        || record.refresh_token.len() > 8192
        || record.refresh_token.chars().any(char::is_control)
        || record.cookies.len() > COOKIE_NAMES.len()
        || record
            .cookies
            .iter()
            .any(|(k, v)| !COOKIE_NAMES.contains(&k.as_str()) || !cookie_value(v))
        || ["DedeUserID", "SESSDATA", "bili_jct"]
            .iter()
            .any(|k| !record.cookies.contains_key(*k))
        || record
            .cookies
            .get("DedeUserID")
            .and_then(|v| v.parse::<u64>().ok())
            != Some(record.mid)
    {
        return Err(Error::LoginState);
    }
    Ok(())
}
fn now() -> Result<u64> {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|v| v.as_secs())
        .map_err(|_| Error::LoginState)
}
fn load_private<T: DeserializeOwned>(path: &Path) -> Result<T> {
    let before = fs::symlink_metadata(path).map_err(|_| Error::Io)?;
    if !before.is_file() || before.permissions().mode() & 0o077 != 0 || before.len() > PRIVATE_LIMIT
    {
        return Err(Error::Io);
    }
    let file = OpenOptions::new()
        .read(true)
        .custom_flags(libc::O_NOFOLLOW | libc::O_NONBLOCK)
        .open(path)
        .map_err(|_| Error::Io)?;
    let metadata = file.metadata().map_err(|_| Error::Io)?;
    if !metadata.is_file()
        || metadata.permissions().mode() & 0o077 != 0
        || metadata.len() > PRIVATE_LIMIT
    {
        return Err(Error::Io);
    }
    let mut bytes = Vec::new();
    file.take(PRIVATE_LIMIT + 1)
        .read_to_end(&mut bytes)
        .map_err(|_| Error::Io)?;
    if bytes.len() as u64 > PRIVATE_LIMIT {
        return Err(Error::Io);
    }
    serde_json::from_slice(&bytes).map_err(|_| Error::LoginState)
}
fn save_private<T: Serialize>(path: &Path, value: &T) -> Result<()> {
    let parent = path.parent().ok_or(Error::Io)?;
    let metadata = fs::symlink_metadata(parent).map_err(|_| Error::Io)?;
    if !metadata.is_dir() || metadata.permissions().mode() & 0o077 != 0 {
        return Err(Error::Io);
    }
    let bytes = serde_json::to_vec(value).map_err(|_| Error::LoginState)?;
    if bytes.len() as u64 > PRIVATE_LIMIT {
        return Err(Error::Io);
    }
    let filename = path.file_name().and_then(|v| v.to_str()).ok_or(Error::Io)?;
    let nonce = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_err(|_| Error::Io)?
        .as_nanos();
    let temp = parent.join(format!(".{filename}-{}-{nonce}.next", std::process::id()));
    let result = (|| {
        let mut file = OpenOptions::new()
            .write(true)
            .create_new(true)
            .mode(0o600)
            .open(&temp)
            .map_err(|_| Error::Io)?;
        file.write_all(&bytes)
            .and_then(|_| file.sync_all())
            .map_err(|_| Error::Io)?;
        fs::rename(&temp, path).map_err(|_| Error::Io)?;
        File::open(parent)
            .and_then(|file| file.sync_all())
            .map_err(|_| Error::Io)
    })();
    if result.is_err() {
        let _ = fs::remove_file(temp);
    }
    result
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    use std::os::unix::fs::{DirBuilderExt, symlink};
    #[test]
    fn current_account_qr_route_is_accepted_without_expanding_request_hosts() {
        let key = "00000000000000000000000000000001";
        let url = format!(
            "https://account.bilibili.com/h5/account-h5/auth/scan-web?navhide=1&callback=&qrcode_key={key}&from=main_web"
        );
        assert!(validate_qr(&url, key).is_ok());
        assert!(validate_qr(&url.replace("/scan-web", "/other"), key).is_err());
        assert!(
            validate_qr(
                &url.replace("account.bilibili.com", "account.bilibili.com.evil.invalid"),
                key
            )
            .is_err()
        );
        assert!(
            validate_auth_url("https://account.bilibili.com/x/passport-login/web/qrcode/generate")
                .is_err()
        );
    }
    fn attempt() -> LoginAttempt {
        parse_begin(json!({"qrcode_key":"00000000000000000000000000000001","url":"https://passport.bilibili.com/h5-app/passport/login/scan?navhide=1&qrcode_key=00000000000000000000000000000001&from="}), now().unwrap()).unwrap()
    }
    fn headers() -> Vec<String> {
        [
            ("DedeUserID", "42"),
            ("DedeUserID__ckMd5", "synthetic-hash"),
            ("SESSDATA", "synthetic-session%2Cx"),
            ("bili_jct", "synthetic-csrf"),
        ]
        .iter()
        .map(|(k, v)| format!("{k}={v}; Path=/; Domain=.bilibili.com; Secure; HttpOnly"))
        .collect()
    }
    #[test]
    fn qr_status_and_cookie_domain_do_not_skip_confirmation_or_follow_sso() {
        let mut attempt = attempt();
        assert!(attempt.take_session().is_err());
        for (code, status) in [(86101, LoginStatus::Waiting), (86090, LoginStatus::Scanned)] {
            assert_eq!(
                parse_poll(&mut attempt, json!({"code":code}), &[], now().unwrap()).unwrap(),
                status
            );
            assert!(attempt.take_session().is_err());
        }
        assert!(
            parse_poll(
                &mut attempt,
                json!({"code":0,"url":"https://evil.invalid/?SESSDATA=secret"}),
                &[],
                now().unwrap()
            )
            .is_err()
        );
        assert!(attempt.take_session().is_err());
        parse_poll(&mut attempt,json!({"code":0,"url":"https://passport.biligame.com/crossDomain?secret=x","refresh_token":"synthetic-refresh"}),&headers(),now().unwrap()).unwrap();
        let session = attempt.take_session().unwrap();
        assert_eq!(session.stable_identity(), "42");
        assert!(
            session
                .cookie_header()
                .contains("SESSDATA=synthetic-session%2Cx")
        );
        assert!(!format!("{session:?}").contains("synthetic"));
        assert!(!format!("{attempt:?}").contains("012345"));
        assert!(attempt.take_session().is_err());
        for bad in [
            "https://evil.invalid/x/passport-login/web/qrcode/generate",
            "https://passport.bilibili.com.evil.invalid/x/passport-login/web/qrcode/generate",
            "http://passport.bilibili.com/x/passport-login/web/qrcode/generate",
            "https://passport.bilibili.com@evil.invalid/x/passport-login/web/qrcode/generate",
        ] {
            assert!(validate_auth_url(bad).is_err());
        }
        let mut wrong = headers();
        wrong[2] = wrong[2].replace(".bilibili.com", ".biligame.com");
        assert!(parse_cookies(&wrong).is_err());
        let mut wrong = headers();
        wrong.push(wrong[2].clone());
        assert!(parse_cookies(&wrong).is_err());
        let mut wrong = headers();
        wrong[2].push_str("; Max-Age=0");
        assert!(parse_cookies(&wrong).is_err());
        assert!(
            validate_qr(
                "https://passport.bilibili.com/h5-app/passport/login/scan?qrcode_key=other",
                &attempt.record.key
            )
            .is_err()
        );
    }
    #[test]
    fn private_login_state_roundtrip_permissions_and_terminal_poll_are_local() {
        let dir = std::env::temp_dir().join(format!(
            "bili-auth-{}-{}",
            std::process::id(),
            SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        fs::DirBuilder::new().mode(0o700).create(&dir).unwrap();
        let path = dir.join("attempt.json");
        let mut attempt = attempt();
        attempt.save(&path).unwrap();
        let mut restored = LoginAttempt::load(&path).unwrap();
        assert_eq!(restored.qr_url(), attempt.qr_url());
        let client = AuthClient::new(Config::default()).unwrap();
        restored.record.created_at = now().unwrap() - QR_LIFETIME;
        assert_eq!(
            client.poll_login(&mut restored).unwrap(),
            LoginStatus::Expired
        );
        assert_eq!(
            client.poll_login(&mut restored).unwrap(),
            LoginStatus::Expired
        );
        restored.record.created_at = now().unwrap();
        restored.record.status = LoginStatus::Waiting;
        restored.record.last_poll = Some(now().unwrap());
        assert_eq!(client.poll_login(&mut restored), Err(Error::PollTooSoon));
        parse_poll(&mut attempt, json!({"code":0}), &headers(), now().unwrap()).unwrap();
        attempt.save(&path).unwrap();
        let mut restored = LoginAttempt::load(&path).unwrap();
        assert_eq!(
            client.poll_login(&mut restored).unwrap(),
            LoginStatus::Confirmed
        );
        let session = restored.take_session().unwrap();
        let session_path = dir.join("session.json");
        session.save(&session_path).unwrap();
        assert_eq!(
            Session::load(&session_path).unwrap().cookie_header(),
            session.cookie_header()
        );
        assert_eq!(
            fs::metadata(&session_path).unwrap().permissions().mode() & 0o777,
            0o600
        );
        let link = dir.join("link");
        symlink(&session_path, &link).unwrap();
        assert!(Session::load(&link).is_err());
        fs::set_permissions(&session_path, fs::Permissions::from_mode(0o644)).unwrap();
        assert!(Session::load(&session_path).is_err());
        fs::remove_dir_all(dir).unwrap();
    }
    #[test]
    fn account_and_favorite_projection_never_exposes_extra_server_fields() {
        let user = account_info(json!({"isLogin":true,"mid":42,"uname":"账号","level_info":{"current_level":6},"vipStatus":1,"wallet":{"secret":"drop-me"},"token":"drop-me"})).unwrap();
        assert_eq!(user.mid, 42);
        assert!(!serde_json::to_string(&user).unwrap().contains("drop-me"));
        assert_eq!(
            account_info(json!({"isLogin":false})).unwrap_err(),
            Error::LoginRequired
        );
        let folders = favorite_folders(json!({"count":1,"list":[{"id":7,"title":"收藏","media_count":2,"attr":0,"token":"drop-me"}]})).unwrap();
        assert_eq!(folders[0].media_count, 2);
        assert!(!serde_json::to_string(&folders).unwrap().contains("drop-me"));
        assert!(favorite_folders(json!({"list":null,"count":1})).is_err());
        assert!(
            favorite_folders(json!({"list":null,"count":0}))
                .unwrap()
                .is_empty()
        );
    }
    #[test]
    fn cookie_import_is_filtered_and_bound_to_verified_identity_without_fabricating_refresh() {
        let cookies=import_cookies(r#"cookie: SESSDATA=synthetic-session%2Cx; bili_jct=synthetic-csrf; DedeUserID=42; unrelated=discard-me; empty=; trailing="ignored";"#).unwrap();
        assert!(!cookies.contains_key("unrelated"));
        let session = imported_session(cookies, 42).unwrap();
        assert_eq!(session.stable_identity(), "42");
        assert!(session.record.refresh_token.is_empty());
        assert!(!session.cookie_header().contains("discard-me"));
        assert!(!format!("{session:?}").contains("synthetic-session"));
        assert!(
            imported_session(
                import_cookies("SESSDATA=synthetic; bili_jct=csrf; DedeUserID=43").unwrap(),
                42
            )
            .is_err()
        );
        assert_eq!(
            imported_session(
                import_cookies("SESSDATA=synthetic; bili_jct=csrf").unwrap(),
                42
            )
            .unwrap()
            .record
            .cookies["DedeUserID"],
            "42"
        );
        for bad in [
            "SESSDATA=one",
            "SESSDATA=one; bili_jct=two; SESSDATA=three",
            "SESSDATA=one\r\nX-Injected: yes; bili_jct=two",
            "SESSDATA=one; bili_jct=two; DedeUserID=0",
        ] {
            assert!(import_cookies(bad).is_err());
        }
    }
}
