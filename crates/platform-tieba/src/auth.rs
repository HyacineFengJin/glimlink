// SPDX-License-Identifier: MIT
//! Personal-account QR login and explicit existing-cookie import using Baidu Passport, without a browser or scripts.
//! Only human-facing CLI code should call this module. No implicit refresh/retry.
use serde::{Deserialize, Serialize, de::DeserializeOwned};
use serde_json::Value;
use std::{
    fmt,
    fs::{self, DirBuilder, File, OpenOptions},
    io::{Read, Write},
    os::unix::fs::{DirBuilderExt, OpenOptionsExt, PermissionsExt},
    path::Path,
    time::{Duration, SystemTime, UNIX_EPOCH},
};
use ureq::{Agent, http::Uri};

const CALLBACK: &str = "agent_runtime_qr";
const LOGIN_CALLBACK: &str = "bd__cbs__agent_runtime";
const LOCAL_LIFETIME: u64 = 300;
const MAX_RESPONSE: usize = 1024 * 1024;
const MAX_PRIVATE: u64 = 256 * 1024;
const USERINFO_URL: &str = "https://tieba.baidu.com/f/user/json_userinfo";

pub type AuthResult<T> = std::result::Result<T, AuthError>;
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum AuthError {
    Network,
    HttpStatus(u16),
    Redirect,
    UrlPolicy,
    Limit,
    Protocol(&'static str),
    Platform(i64),
    Challenge,
    LoginRequired,
    InvalidState,
    IdentityChanged,
    PrivateFile,
}
impl fmt::Display for AuthError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        // Never include response bodies, URLs, cookies or temporary tokens.
        write!(f, "Tieba authentication error: {self:?}")
    }
}
impl std::error::Error for AuthError {}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum LoginStatus {
    Waiting,
    Scanned,
    Confirmed,
    Expired,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct AccountInfo {
    pub user_id: Option<u64>,
    pub user_name: String,
    pub display_name: String,
    pub portrait: String,
}
impl AccountInfo {
    pub fn stable_identity(&self) -> String {
        if let Some(id) = self.user_id {
            format!("uid:{id}")
        } else {
            format!("portrait:{}", self.portrait)
        }
    }
}

#[derive(Clone, Serialize, Deserialize)]
struct Cookie {
    name: String,
    value: String,
    domain: String,
    host_only: bool,
    path: String,
}
#[derive(Clone, Default, Serialize, Deserialize)]
struct Cookies(Vec<Cookie>);
impl Cookies {
    fn header(&self, host: &str, path: &str) -> AuthResult<Option<String>> {
        let mut values = Vec::new();
        for cookie in &self.0 {
            if cookie.matches(host, path) {
                values.push(format!("{}={}", cookie.name, cookie.value));
            }
        }
        let header = values.join("; ");
        if header.len() > 24 * 1024 {
            return Err(AuthError::Limit);
        }
        Ok((!header.is_empty()).then_some(header))
    }
    fn absorb(&mut self, host: &str, line: &str) -> AuthResult<()> {
        if line.len() > 20 * 1024 {
            return Err(AuthError::Limit);
        }
        let mut fields = line.split(';');
        let (name, value) = fields
            .next()
            .and_then(|v| v.trim().split_once('='))
            .ok_or(AuthError::Protocol("invalid cookie"))?;
        if !cookie_name(name) || !cookie_value(value) {
            return Err(AuthError::Protocol("invalid cookie"));
        }
        let mut cookie = Cookie {
            name: name.into(),
            value: value.into(),
            domain: host.into(),
            host_only: true,
            path: "/".into(),
        };
        let mut remove = value.is_empty();
        for field in fields {
            let (key, value) = field.trim().split_once('=').unwrap_or((field.trim(), ""));
            if key.eq_ignore_ascii_case("domain") {
                let domain = value.trim_start_matches('.').to_ascii_lowercase();
                if !matches!(
                    domain.as_str(),
                    "baidu.com" | "passport.baidu.com" | "tieba.baidu.com"
                ) || !(host == domain
                    || host.strip_suffix(&domain).is_some_and(|v| v.ends_with('.')))
                {
                    return Err(AuthError::UrlPolicy);
                }
                cookie.domain = domain;
                cookie.host_only = false;
            } else if key.eq_ignore_ascii_case("path") {
                if !value.starts_with('/')
                    || value.len() > 1024
                    || value.bytes().any(|b| b <= 32 || b == 127)
                {
                    return Err(AuthError::Protocol("invalid cookie path"));
                }
                cookie.path = value.into();
            } else if key.eq_ignore_ascii_case("max-age") {
                if value.parse::<i64>().is_ok_and(|v| v <= 0) {
                    remove = true;
                }
            }
        }
        self.0.retain(|old| {
            !(old.name == cookie.name && old.domain == cookie.domain && old.path == cookie.path)
        });
        if !remove {
            self.0.push(cookie);
        }
        if self.0.len() > 128 {
            return Err(AuthError::Limit);
        }
        Ok(())
    }
    fn bduss(&self) -> Option<&str> {
        self.0
            .iter()
            .find(|c| c.name == "BDUSS" && c.matches("tieba.baidu.com", "/") && !c.value.is_empty())
            .map(|c| c.value.as_str())
    }
    fn validate(&self) -> AuthResult<()> {
        if self.0.len() > 128 {
            return Err(AuthError::Limit);
        }
        for cookie in &self.0 {
            if !cookie_name(&cookie.name)
                || !cookie_value(&cookie.value)
                || !matches!(
                    cookie.domain.as_str(),
                    "baidu.com" | "passport.baidu.com" | "tieba.baidu.com"
                )
                || !cookie.path.starts_with('/')
                || cookie.path.len() > 1024
                || cookie.path.bytes().any(|b| b <= 32 || b == 127)
            {
                return Err(AuthError::PrivateFile);
            }
        }
        self.header("passport.baidu.com", "/")?;
        self.header("tieba.baidu.com", "/")?;
        Ok(())
    }
}
impl Cookie {
    fn matches(&self, host: &str, path: &str) -> bool {
        let domain = host == self.domain
            || (!self.host_only
                && host
                    .strip_suffix(&self.domain)
                    .is_some_and(|v| v.ends_with('.')));
        let path_matches = path == self.path
            || path
                .strip_prefix(&self.path)
                .is_some_and(|v| self.path.ends_with('/') || v.starts_with('/'));
        domain && path_matches
    }
}
fn cookie_name(value: &str) -> bool {
    !value.is_empty()
        && value.len() <= 128
        && value
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b"_-".contains(&b))
}
fn cookie_value(value: &str) -> bool {
    value.len() <= 16 * 1024
        && value
            .bytes()
            .all(|b| (33..127).contains(&b) && !b";,\"\\".contains(&b))
}
fn token(value: &str) -> bool {
    !value.is_empty()
        && value.len() <= 2048
        && value
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b"_-~=.".contains(&b))
}

/// Deliberately not Serialize/Deserialize: only its private file representation is.
pub struct LoginAttempt(AttemptRecord);
#[derive(Serialize, Deserialize)]
struct AttemptRecord {
    schema: u32,
    gid: String,
    sign: String,
    created_at: u64,
    expires_at: u64,
    cookies: Cookies,
    status: LoginStatus,
    confirmed_token: Option<String>,
    exchange_attempted: bool,
}
impl fmt::Debug for LoginAttempt {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("LoginAttempt")
            .field("status", &self.0.status)
            .finish_non_exhaustive()
    }
}
impl LoginAttempt {
    pub fn expires_in_seconds(&self) -> u64 {
        self.0.expires_at.saturating_sub(now().unwrap_or(u64::MAX))
    }
    pub fn status(&self) -> LoginStatus {
        self.0.status
    }
    pub fn exchange_attempted(&self) -> bool {
        self.0.exchange_attempted
    }
    pub fn has_formal_bduss(&self) -> bool {
        self.0.cookies.bduss().is_some()
    }
    pub fn save(&self, path: &Path) -> AuthResult<()> {
        save_private(path, &self.0)
    }
    pub fn load(path: &Path) -> AuthResult<Self> {
        let record: AttemptRecord = load_private(path)?;
        if record.schema != 1
            || !token(&record.sign)
            || !token(&record.gid)
            || record.expires_at < record.created_at
            || record.expires_at - record.created_at > LOCAL_LIFETIME
            || record.confirmed_token.as_deref().is_some_and(|v| !token(v))
            || (record.status == LoginStatus::Confirmed
                && record.confirmed_token.is_none()
                && !record.exchange_attempted)
        {
            return Err(AuthError::PrivateFile);
        }
        record.cookies.validate()?;
        Ok(Self(record))
    }
}

pub struct Session(SessionRecord);
#[derive(Serialize, Deserialize)]
struct SessionRecord {
    schema: u32,
    cookies: Cookies,
    account: AccountInfo,
}
impl fmt::Debug for Session {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Session").finish_non_exhaustive()
    }
}
impl Session {
    pub fn bduss(&self) -> &str {
        self.0.cookies.bduss().expect("validated session BDUSS")
    }
    pub fn info(&self) -> &AccountInfo {
        &self.0.account
    }
    pub fn stable_identity(&self) -> String {
        self.0.account.stable_identity()
    }
    pub fn save(&self, path: &Path) -> AuthResult<()> {
        save_private(path, &self.0)
    }
    pub fn load(path: &Path) -> AuthResult<Self> {
        let record: SessionRecord = load_private(path)?;
        record.cookies.validate()?;
        if record.schema != 1 || record.cookies.bduss().is_none() || !valid_account(&record.account)
        {
            return Err(AuthError::PrivateFile);
        }
        Ok(Self(record))
    }
}

#[derive(Clone, Debug, Serialize)]
pub struct QrReceipt {
    pub mime: String,
    pub bytes: usize,
}

pub struct AuthClient {
    agent: Agent,
}
struct AuthResponse {
    status: u16,
    location: Option<String>,
    body: Vec<u8>,
}
impl AuthClient {
    /// Fixed feature endpoints. The host must persist an attempt before writes.
    pub(crate) fn submit_feature(
        &self,
        session: &mut Session,
        path: &str,
        body: &[u8],
        protobuf: bool,
    ) -> crate::Result<Vec<u8>> {
        if !matches!(path, "/f/commit/thread/add" | "/c/c/post/add?cmd=309731") {
            return Err(crate::Error::UrlPolicy);
        }
        let (host, url) = if protobuf {
            (
                "tiebac.baidu.com",
                format!("https://tiebac.baidu.com{path}"),
            )
        } else {
            ("tieba.baidu.com", format!("https://tieba.baidu.com{path}"))
        };
        let header = session
            .0
            .cookies
            .header(host, "/")
            .map_err(|_| crate::Error::InvalidConfig)?
            .unwrap_or_else(|| format!("BDUSS={}", session.bduss()));
        let agent: Agent = Agent::config_builder()
            .https_only(true)
            .max_redirects(0)
            .http_status_as_error(false)
            .proxy(None)
            .timeout_global(Some(Duration::from_secs(40)))
            .timeout_connect(Some(Duration::from_secs(8)))
            .max_idle_connections(0)
            .build()
            .into();
        let mut request = agent
            .post(&url)
            .header("Cookie", &header)
            .header("Referer", "https://tieba.baidu.com/");
        let mut multipart = Vec::new();
        let payload = if protobuf {
            multipart.extend_from_slice(b"--agent-runtime-tieba\r\nContent-Disposition: form-data; name=\"data\"; filename=\"file\"\r\n\r\n");
            multipart.extend_from_slice(body);
            multipart.extend_from_slice(b"\r\n--agent-runtime-tieba--\r\n");
            request = request.header("x_bd_data_type", "protobuf").header(
                "Content-Type",
                "multipart/form-data; boundary=agent-runtime-tieba",
            );
            multipart.as_slice()
        } else {
            request = request
                .header(
                    "Content-Type",
                    "application/x-www-form-urlencoded; charset=UTF-8",
                )
                .header("Origin", "https://tieba.baidu.com")
                .header("X-Requested-With", "XMLHttpRequest");
            body
        };
        let mut response = request
            .send(payload)
            .map_err(|_| crate::Error::WriteResultUnknown)?;
        if response.status().as_u16() != 200 {
            return Err(crate::Error::WriteResultUnknown);
        }
        bounded(response.body_mut().as_reader(), MAX_RESPONSE)
            .map_err(|_| crate::Error::WriteResultUnknown)
    }
    pub(crate) fn upload_feature_image(
        &self,
        session: &mut Session,
        forum_id: u64,
        bytes: &[u8],
        mime: &str,
    ) -> crate::Result<Vec<u8>> {
        let mut cookies = session.0.cookies.clone();
        let raw = self
            .get(
                "https://tieba.baidu.com/dc/common/imgtbs",
                &mut cookies,
                false,
            )
            .map_err(|_| crate::Error::Challenge)?;
        let json: Value = serde_json::from_slice(&raw).map_err(|_| crate::Error::Challenge)?;
        let tbs = json
            .pointer("/data/tbs")
            .and_then(Value::as_str)
            .ok_or(crate::Error::Schema("image upload token missing"))?;
        if tbs.is_empty()
            || tbs.len() > 256
            || !tbs
                .bytes()
                .all(|b| b.is_ascii_alphanumeric() || b"_-".contains(&b))
        {
            return Err(crate::Error::Schema("image upload token invalid"));
        }
        let header = cookies
            .header("uploadphotos.baidu.com", "/")
            .map_err(|_| crate::Error::InvalidConfig)?
            .unwrap_or_else(|| format!("BDUSS={}", session.bduss()));
        let url = format!(
            "https://uploadphotos.baidu.com/upload/pic?tbs={tbs}&fid={forum_id}&save_yun_album=0&picWaterType=0"
        );
        let mut body=format!("--agent-runtime-tieba-picture\r\nContent-Disposition: form-data; name=\"file\"; filename=\"image\"\r\nContent-Type: {mime}\r\n\r\n").into_bytes();
        body.extend_from_slice(bytes);
        body.extend_from_slice(b"\r\n--agent-runtime-tieba-picture--\r\n");
        let agent: Agent = Agent::config_builder()
            .https_only(true)
            .max_redirects(0)
            .http_status_as_error(false)
            .proxy(None)
            .timeout_global(Some(Duration::from_secs(40)))
            .max_idle_connections(0)
            .build()
            .into();
        let mut response = agent
            .post(&url)
            .header("Cookie", &header)
            .header("Referer", "https://tieba.baidu.com/")
            .header(
                "Content-Type",
                "multipart/form-data; boundary=agent-runtime-tieba-picture",
            )
            .send(body.as_slice())
            .map_err(|_| crate::Error::WriteResultUnknown)?;
        if response.status().as_u16() != 200 {
            return Err(crate::Error::WriteResultUnknown);
        }
        session.0.cookies = cookies;
        bounded(response.body_mut().as_reader(), MAX_RESPONSE)
            .map_err(|_| crate::Error::WriteResultUnknown)
    }
    /// Resolve the authenticated self UID from a read-only first-party endpoint,
    /// bind it to this Session's already verified portrait, then read own content.
    /// This does not invoke a login endpoint or change the Session's identity key.
    pub fn self_publications(
        &self,
        session: &mut Session,
        kind: crate::PublicationKind,
        page: u32,
        limit: usize,
    ) -> crate::Result<crate::PublicationPage> {
        if page == 0 || page > 100000 {
            return Err(crate::Error::InvalidPage);
        }
        if !(1..=50).contains(&limit) {
            return Err(crate::Error::Limit);
        }
        let owner = self.self_identity(session)?;
        crate::Client::new(crate::Config {
            bduss: Some(session.bduss().to_owned()),
            ..Default::default()
        })?
        .publications_for_owner(owner, kind, page, limit)
    }
    /// Verify this saved personal session through the read-only self endpoint.
    pub fn self_identity(&self, session: &mut Session) -> crate::Result<crate::PublicationOwner> {
        let mut cookies = session.0.cookies.clone();
        let raw = self
            .get(
                "https://tieba.baidu.com/mo/q/newmoindex?need_user=1",
                &mut cookies,
                false,
            )
            .map_err(|error| match error {
                AuthError::Network => crate::Error::Network,
                AuthError::HttpStatus(code) => crate::Error::HttpStatus(code),
                AuthError::Platform(code) => crate::Error::Platform(code),
                AuthError::Challenge => crate::Error::Challenge,
                AuthError::LoginRequired => crate::Error::LoginRequired,
                AuthError::Limit => crate::Error::Limit,
                AuthError::UrlPolicy | AuthError::Redirect => crate::Error::UrlPolicy,
                _ => crate::Error::Schema("self account lookup failed"),
            })?;
        let owner = crate::publications::self_owner(
            &raw,
            &session.0.account.portrait,
            session.0.account.user_id,
        )?;
        session.0.cookies = cookies;
        Ok(owner)
    }
    pub(crate) fn reply_tbs(&self, session: &mut Session) -> crate::Result<String> {
        let mut cookies = session.0.cookies.clone();
        let fetch = self.get(
            "https://tieba.baidu.com/mo/q/newmoindex?need_user=1",
            &mut cookies,
            false,
        );
        let raw = fetch.map_err(|error| match error {
            AuthError::Network => crate::Error::Network,
            AuthError::HttpStatus(code) => crate::Error::HttpStatus(code),
            AuthError::Challenge => crate::Error::Challenge,
            AuthError::LoginRequired => crate::Error::LoginRequired,
            AuthError::Limit => crate::Error::Limit,
            _ => crate::Error::Schema("reply token lookup failed"),
        })?;
        let tbs = profile_reply_tbs(&raw, &session.0.account.portrait, session.0.account.user_id)?;
        session.0.cookies = cookies;
        Ok(tbs)
    }
    /// One fixed POST, without redirects or retry. Called only after the host
    /// durably records its send attempt. Any transport failure is ambiguous.
    pub(crate) fn submit_reply_form(
        &self,
        session: &mut Session,
        thread_id: u64,
        body: &[u8],
    ) -> crate::Result<Vec<u8>> {
        let url = "https://tieba.baidu.com/f/commit/post/add";
        let header = session
            .0
            .cookies
            .header("tieba.baidu.com", "/f/commit/post/add")
            .map_err(|_| crate::Error::InvalidConfig)?
            .ok_or(crate::Error::LoginRequired)?;
        // A fresh connection avoids replay on a stale pooled connection.
        let agent: Agent = Agent::config_builder()
            .https_only(true)
            .max_redirects(0)
            .http_status_as_error(false)
            .proxy(None)
            .timeout_global(Some(Duration::from_secs(40)))
            .timeout_connect(Some(Duration::from_secs(8)))
            .timeout_per_read(Some(Duration::from_secs(35)))
            .max_response_header_size(32 * 1024)
            .max_idle_connections(0)
            .user_agent(
                "Mozilla/5.0 (X11; Linux x86_64) AppleWebKit/537.36 Chrome/131.0.0.0 Safari/537.36",
            )
            .build()
            .into();
        let mut response = agent
            .post(url)
            .header("Cookie", &header)
            .header("Referer", &format!("https://tieba.baidu.com/p/{thread_id}"))
            .header("Origin", "https://tieba.baidu.com")
            .header("X-Requested-With", "XMLHttpRequest")
            .header(
                "Content-Type",
                "application/x-www-form-urlencoded; charset=UTF-8",
            )
            .send(body)
            .map_err(|_| crate::Error::WriteResultUnknown)?;
        if response.status().as_u16() != 200 {
            return Err(crate::Error::WriteResultUnknown);
        }
        let raw = bounded(response.body_mut().as_reader(), MAX_RESPONSE)
            .map_err(|_| crate::Error::WriteResultUnknown)?;
        Ok(raw)
    }
    /// Feed reads reuse the already confirmed formal BDUSS. No QR, SSO or re-login.
    pub fn notifications(
        &self,
        session: &mut Session,
        kind: crate::NotificationKind,
        page: u32,
        limit: usize,
    ) -> crate::Result<crate::NotificationPage> {
        crate::Client::new(crate::Config {
            bduss: Some(session.bduss().to_owned()),
            ..Default::default()
        })?
        .notifications(kind, page, limit)
    }
    /// Explicit operator import of a Cookie header or bare BDUSS. This only
    /// performs the existing bounded account-identity handshake; never QR.
    /// No file is persisted until the host commits the verified result.
    pub fn import_cookie_header(&self, value: &str) -> AuthResult<Session> {
        let mut cookies = import_cookies(value)?;
        let account = self.account_with_cookies(&mut cookies)?;
        Ok(Session(SessionRecord {
            schema: 1,
            cookies,
            account,
        }))
    }
    pub fn new() -> AuthResult<Self> {
        let agent = Agent::config_builder()
            .https_only(true)
            .max_redirects(0)
            .http_status_as_error(false)
            .proxy(None)
            .timeout_global(Some(Duration::from_secs(40)))
            .timeout_connect(Some(Duration::from_secs(8)))
            .timeout_per_read(Some(Duration::from_secs(35)))
            .max_response_header_size(32 * 1024)
            .max_idle_connections(2)
            .user_agent(
                "Mozilla/5.0 (X11; Linux x86_64) AppleWebKit/537.36 Chrome/131.0.0.0 Safari/537.36",
            )
            .build()
            .into();
        Ok(Self { agent })
    }
    fn get(&self, url: &str, cookies: &mut Cookies, exchange: bool) -> AuthResult<Vec<u8>> {
        let uri = check_url(url)?;
        let response = self.request(url, &uri, cookies)?;
        if matches!(response.status, 301 | 302 | 303 | 307 | 308) {
            // No QR endpoint follows redirects, including after an exchange.
            if !exchange || cookies.bduss().is_none() {
                return Err(AuthError::Redirect);
            }
        } else if response.status != 200 {
            return Err(AuthError::HttpStatus(response.status));
        }
        Ok(response.body)
    }
    fn request(&self, url: &str, uri: &Uri, cookies: &mut Cookies) -> AuthResult<AuthResponse> {
        let host = uri.host().ok_or(AuthError::UrlPolicy)?;
        let mut request = self
            .agent
            .get(url)
            .header("Referer", "https://tieba.baidu.com/");
        if let Some(header) = cookies.header(host, uri.path())? {
            request = request.header("Cookie", &header);
        }
        let mut response = request.call().map_err(|_| AuthError::Network)?;
        for header in response.headers().get_all("Set-Cookie") {
            cookies.absorb(
                host,
                header
                    .to_str()
                    .map_err(|_| AuthError::Protocol("invalid cookie header"))?,
            )?;
        }
        let status = response.status().as_u16();
        if matches!(status, 403 | 412 | 429) {
            return Err(AuthError::Challenge);
        }
        let location = response
            .headers()
            .get("Location")
            .map(|v| {
                v.to_str()
                    .map(str::to_owned)
                    .map_err(|_| AuthError::UrlPolicy)
            })
            .transpose()?;
        let body = bounded(response.body_mut().as_reader(), MAX_RESPONSE)?;
        Ok(AuthResponse {
            status,
            location,
            body,
        })
    }
    pub fn begin_login(&self) -> AuthResult<LoginAttempt> {
        let created_at = now()?;
        let gid = random_gid()?;
        let url = format!(
            "https://passport.baidu.com/v2/api/getqrcode?lp=pc&qrloginfrom=pc&gid={gid}&callback={CALLBACK}&apiver=v3&tt={}&tpl=tb",
            millis()?
        );
        let mut cookies = Cookies::default();
        let value = jsonp(&self.get(&url, &mut cookies, false)?, CALLBACK)?;
        if let Some(code) = number(&value, "errno") {
            if code != 0 {
                return Err(AuthError::Platform(code));
            }
        }
        let sign = value
            .get("sign")
            .and_then(Value::as_str)
            .filter(|v| token(v))
            .ok_or(AuthError::Protocol("missing QR sign"))?
            .to_owned();
        Ok(LoginAttempt(AttemptRecord {
            schema: 1,
            gid,
            sign,
            created_at,
            expires_at: created_at + LOCAL_LIFETIME,
            cookies,
            status: LoginStatus::Waiting,
            confirmed_token: None,
            exchange_attempted: false,
        }))
    }
    pub fn download_qr(&self, attempt: &mut LoginAttempt, path: &Path) -> AuthResult<QrReceipt> {
        if attempt.expires_in_seconds() == 0 || attempt.0.exchange_attempted {
            return Err(AuthError::InvalidState);
        }
        // Build the fixed first-party image endpoint; do not trust imgurl.
        let url = format!(
            "https://passport.baidu.com/v2/api/qrcode?sign={}&lp=pc",
            encode(&attempt.0.sign)
        );
        let bytes = self.get(&url, &mut attempt.0.cookies, false)?;
        if bytes.len() > 512 * 1024 {
            return Err(AuthError::Limit);
        }
        let mime = if bytes.starts_with(b"\x89PNG\r\n\x1a\n") {
            "image/png"
        } else if bytes.starts_with(b"\xff\xd8\xff") {
            "image/jpeg"
        } else if bytes.starts_with(b"GIF8") {
            "image/gif"
        } else {
            return Err(AuthError::Protocol("QR response is not an image"));
        };
        let mut file = OpenOptions::new()
            .write(true)
            .create_new(true)
            .mode(0o600)
            .open(path)
            .map_err(|_| AuthError::PrivateFile)?;
        if file
            .write_all(&bytes)
            .and_then(|_| file.sync_all())
            .is_err()
        {
            drop(file);
            let _ = fs::remove_file(path);
            return Err(AuthError::PrivateFile);
        }
        Ok(QrReceipt {
            mime: mime.into(),
            bytes: bytes.len(),
        })
    }
    pub fn poll_login(&self, attempt: &mut LoginAttempt) -> AuthResult<LoginStatus> {
        if attempt.0.exchange_attempted {
            return Err(AuthError::InvalidState);
        }
        if attempt.expires_in_seconds() == 0 {
            attempt.0.status = LoginStatus::Expired;
            return Ok(LoginStatus::Expired);
        }
        if attempt.0.status == LoginStatus::Confirmed {
            return Ok(LoginStatus::Confirmed);
        }
        let url = format!(
            "https://passport.baidu.com/channel/unicast?channel_id={}&tpl=tb&gid={}&apiver=v3&tt={}&callback={CALLBACK}",
            encode(&attempt.0.sign),
            encode(&attempt.0.gid),
            millis()?
        );
        let value = jsonp(&self.get(&url, &mut attempt.0.cookies, false)?, CALLBACK)?;
        apply_poll(attempt, value)
    }
    /// One exchange per in-memory attempt; use `finish_login_persisted` for a
    /// restart-safe CLI. A temporary channel token is not a successful Session.
    pub fn finish_login(&self, attempt: &mut LoginAttempt) -> AuthResult<Session> {
        self.finish_login_inner(attempt, None)
    }

    /// Durably consume the exchange before any ticket request, including on
    /// transport errors or process crashes. Does not automatically retry.
    pub fn finish_login_persisted(
        &self,
        attempt: &mut LoginAttempt,
        path: &Path,
    ) -> AuthResult<Session> {
        self.finish_login_inner(attempt, Some(path))
    }

    fn finish_login_inner(
        &self,
        attempt: &mut LoginAttempt,
        checkpoint: Option<&Path>,
    ) -> AuthResult<Session> {
        if attempt.expires_in_seconds() == 0
            || attempt.0.status != LoginStatus::Confirmed
            || attempt.0.exchange_attempted
        {
            return Err(AuthError::InvalidState);
        }
        let temporary = attempt
            .0
            .confirmed_token
            .take()
            .ok_or(AuthError::InvalidState)?;
        attempt.0.exchange_attempted = true;
        if let Some(path) = checkpoint {
            attempt.save(path)?;
        }
        let ms = millis()?;
        let url = format!(
            "https://passport.baidu.com/v3/login/main/qrbdusslogin?v={ms}&bduss={}&u=https%253A%252F%252Ftieba.baidu.com%252Findex.html&loginVersion=v4&qrcode=1&tpl=tb&apiver=v3&tt={ms}&traceid=&callback={LOGIN_CALLBACK}",
            encode(&temporary)
        );
        let body = self.get(&url, &mut attempt.0.cookies, true)?;
        if attempt.0.cookies.bduss().is_none() {
            if let Ok(value) = jsonp(&body, LOGIN_CALLBACK) {
                if let Some(code) = number(&value, "err_no")
                    .or_else(|| number(&value, "errno"))
                    .or_else(|| value.get("errInfo").and_then(|v| number(v, "no")))
                {
                    if code != 0 {
                        return Err(AuthError::Platform(code));
                    }
                }
            }
            return Err(AuthError::Protocol(
                "exchange did not issue a formal BDUSS cookie",
            ));
        }
        // The official cookie must survive a crash during the later, separate
        // child-site identity handshake. Never redo the ticket exchange.
        if let Some(path) = checkpoint {
            attempt.save(path)?;
        }
        self.resume_exchanged_login(attempt)
    }

    /// Recover a completed ticket exchange whose subsequent identity lookup
    /// failed. Only validates the formal cookie; never polls or exchanges again.
    /// QR expiry is irrelevant after the official cookie has been issued.
    pub fn resume_exchanged_login(&self, attempt: &LoginAttempt) -> AuthResult<Session> {
        if !attempt.0.exchange_attempted
            || attempt.0.status != LoginStatus::Confirmed
            || attempt.0.cookies.bduss().is_none()
        {
            return Err(AuthError::InvalidState);
        }
        let mut cookies = attempt.0.cookies.clone();
        let account = self.account_with_cookies(&mut cookies)?;
        Ok(Session(SessionRecord {
            schema: 1,
            cookies,
            account,
        }))
    }
    fn account_with_cookies(&self, cookies: &mut Cookies) -> AuthResult<AccountInfo> {
        if cookies.bduss().is_none() {
            return Err(AuthError::LoginRequired);
        }
        let mut url = USERINFO_URL.to_owned();
        // Two SSO hops, then one fixed JSON re-read after Tieba sets STOKEN.
        // The callback's homepage redirect is checked but never fetched.
        // This never invokes a QR generation, polling, or ticket endpoint.
        for step in 0..=3 {
            let uri = identity_url(&url, step)?;
            let response = self.request(&url, &uri, cookies)?;
            if response.status == 200 {
                if step == 1 {
                    return Err(AuthError::Protocol("Passport auth did not return to Tieba"));
                }
                let value: Value = serde_json::from_slice(&response.body)
                    .map_err(|_| AuthError::Protocol("account response is not JSON"))?;
                return account_info(value);
            }
            if response.status != 302 {
                return Err(AuthError::HttpStatus(response.status));
            }
            if step == 3 {
                return Err(AuthError::Redirect);
            }
            let next = response.location.ok_or(AuthError::Redirect)?;
            if step == 2 {
                url = identity_after_callback(&next, cookies)?.into();
            } else {
                identity_url(&next, step + 1)?;
                url = next;
            }
        }
        Err(AuthError::Redirect)
    }
    pub fn me(&self, session: &mut Session) -> AuthResult<AccountInfo> {
        let mut cookies = session.0.cookies.clone();
        let account = self.account_with_cookies(&mut cookies)?;
        if account.stable_identity() != session.stable_identity() {
            return Err(AuthError::IdentityChanged);
        }
        session.0.cookies = cookies;
        session.0.account = account.clone();
        Ok(account)
    }
}

fn profile_reply_tbs(raw: &[u8], portrait: &str, uid: Option<u64>) -> crate::Result<String> {
    crate::publications::self_owner(raw, portrait, uid)?;
    let value: Value =
        serde_json::from_slice(raw).map_err(|_| crate::Error::Schema("reply profile JSON"))?;
    let token_view = serde_json::json!({"is_login":value["data"].get("is_login"),"tbs":value["data"].get("tbs")});
    crate::posting::parse_tbs(
        &serde_json::to_vec(&token_view).map_err(|_| crate::Error::InvalidConfig)?,
    )
}

fn apply_poll(attempt: &mut LoginAttempt, value: Value) -> AuthResult<LoginStatus> {
    let errno = number(&value, "errno").ok_or(AuthError::Protocol("missing poll errno"))?;
    if errno == 1 {
        return Ok(attempt.0.status);
    }
    if errno != 0 {
        return Err(AuthError::Platform(errno));
    }
    if value
        .get("channel_id")
        .and_then(Value::as_str)
        .is_some_and(|v| v != attempt.0.sign)
    {
        return Err(AuthError::Protocol("channel mismatch"));
    }
    let channel = value
        .get("channel_v")
        .ok_or(AuthError::Protocol("missing channel value"))?;
    let parsed;
    let channel = if let Some(text) = channel.as_str() {
        if text.len() > 16 * 1024 {
            return Err(AuthError::Limit);
        }
        parsed = serde_json::from_str::<Value>(text)
            .map_err(|_| AuthError::Protocol("invalid channel value"))?;
        &parsed
    } else {
        channel
    };
    let status =
        match number(channel, "status").ok_or(AuthError::Protocol("missing channel status"))? {
            0 => {
                let temporary = channel
                    .get("v")
                    .and_then(Value::as_str)
                    .filter(|v| token(v))
                    .ok_or(AuthError::Protocol("confirmation lacks temporary token"))?;
                attempt.0.confirmed_token = Some(temporary.into());
                LoginStatus::Confirmed
            }
            1 => LoginStatus::Scanned,
            2 => LoginStatus::Waiting,
            code => return Err(AuthError::Platform(code)),
        };
    attempt.0.status = status;
    Ok(status)
}
fn account_info(value: Value) -> AuthResult<AccountInfo> {
    let code = number(&value, "no").ok_or(AuthError::Protocol("missing account result"))?;
    if code != 0 {
        return Err(AuthError::Platform(code));
    }
    let data = value
        .get("data")
        .filter(|v| v.is_object())
        .ok_or(AuthError::Protocol("missing account data"))?;
    let text = |name: &str| -> AuthResult<String> {
        let value = data.get(name).and_then(Value::as_str).unwrap_or_default();
        if value.len() > 4096 || value.chars().any(char::is_control) {
            return Err(AuthError::Protocol("invalid account field"));
        }
        Ok(value.into())
    };
    let user_id = number(data, "user_id").filter(|v| *v > 0).map(|v| v as u64);
    let account = AccountInfo {
        user_id,
        user_name: text("user_name")?,
        display_name: text("name_show")?,
        portrait: text("user_portrait")?,
    };
    if !valid_account(&account) {
        return Err(AuthError::LoginRequired);
    }
    Ok(account)
}
fn valid_account(account: &AccountInfo) -> bool {
    account.user_id.is_some_and(|v| v > 0)
        || (!account.portrait.is_empty()
            && account.portrait.len() <= 4096
            && !account.portrait.chars().any(char::is_control))
}
fn number(value: &Value, key: &str) -> Option<i64> {
    value.get(key).and_then(|v| {
        v.as_i64()
            .or_else(|| v.as_str().and_then(|s| s.parse().ok()))
    })
}
fn check_url(value: &str) -> AuthResult<Uri> {
    let uri = https_uri(value)?;
    match (uri.host(), uri.path()) {
        (
            Some("passport.baidu.com"),
            "/v2/api/getqrcode"
            | "/v2/api/qrcode"
            | "/channel/unicast"
            | "/v3/login/main/qrbdusslogin",
        ) => Ok(uri),
        (
            Some("tieba.baidu.com"),
            "/f/user/json_userinfo" | "/mo/q/newmoindex" | "/dc/common/tbs" | "/dc/common/imgtbs",
        ) => Ok(uri),
        _ => Err(AuthError::UrlPolicy),
    }
}
fn https_uri(value: &str) -> AuthResult<Uri> {
    if value.len() > 16384
        || value.contains(['\\', '#'])
        || value.bytes().any(|b| b <= 32 || b == 127)
    {
        return Err(AuthError::UrlPolicy);
    }
    let uri: Uri = value.parse().map_err(|_| AuthError::UrlPolicy)?;
    if uri.scheme_str() != Some("https")
        || !matches!(uri.port_u16(), None | Some(443))
        || uri.authority().is_none_or(|v| v.as_str().contains('@'))
    {
        return Err(AuthError::UrlPolicy);
    }
    Ok(uri)
}
fn identity_url(value: &str, step: usize) -> AuthResult<Uri> {
    let uri = https_uri(value)?;
    match step {
        0 | 3 => {
            if value != USERINFO_URL {
                return Err(AuthError::UrlPolicy);
            }
        }
        1 => {
            if uri.host() != Some("passport.baidu.com") || uri.path() != "/v3/login/api/auth/" {
                return Err(AuthError::UrlPolicy);
            }
            let params = query_params(&uri)?;
            if !(3..=4).contains(&params.len())
                || params.get("tpl").map(String::as_str) != Some("tb")
                || params.get("return_type").map(String::as_str) != Some("3")
                || params.get("u").map(String::as_str) != Some(USERINFO_URL)
                || params
                    .keys()
                    .any(|key| !matches!(key.as_str(), "tpl" | "return_type" | "u" | "jump"))
                || params.get("jump").is_some_and(|value| !value.is_empty())
            {
                return Err(AuthError::UrlPolicy);
            }
        }
        2 => {
            if uri.host() != Some("tieba.baidu.com") || uri.path() != "/f/user/json_userinfo" {
                return Err(AuthError::UrlPolicy);
            }
            let params = query_params(&uri)?;
            if params
                .keys()
                .any(|key| !matches!(key.as_str(), "errmsg" | "errno" | "ssnerror" | "stoken"))
                || params
                    .get("stoken")
                    .is_none_or(|v| v.is_empty() || !cookie_value(v))
            {
                return Err(AuthError::UrlPolicy);
            }
            for key in ["errno", "ssnerror"] {
                let code: i64 = params
                    .get(key)
                    .ok_or(AuthError::Protocol("SSO response lacks result"))?
                    .parse()
                    .map_err(|_| AuthError::Protocol("SSO response has invalid result"))?;
                if code != 0 {
                    return Err(AuthError::Platform(code));
                }
            }
        }
        _ => return Err(AuthError::Redirect),
    }
    Ok(uri)
}
fn identity_after_callback(location: &str, cookies: &Cookies) -> AuthResult<&'static str> {
    if location != "https://tieba.baidu.com/"
        || !cookies.0.iter().any(|cookie| {
            cookie.name == "STOKEN"
                && cookie.domain == "tieba.baidu.com"
                && !cookie.value.is_empty()
                && cookie.matches("tieba.baidu.com", "/f/user/json_userinfo")
        })
    {
        return Err(AuthError::Redirect);
    }
    Ok(USERINFO_URL)
}
fn query_params(uri: &Uri) -> AuthResult<std::collections::BTreeMap<String, String>> {
    let mut result = std::collections::BTreeMap::new();
    let query = uri.query().ok_or(AuthError::UrlPolicy)?;
    if query.len() > 8192 {
        return Err(AuthError::Limit);
    }
    for field in query.split('&') {
        if result.len() >= 8 {
            return Err(AuthError::Limit);
        }
        let (key, value) = field.split_once('=').ok_or(AuthError::UrlPolicy)?;
        let key = decode_query(key)?;
        let value = decode_query(value)?;
        if key.is_empty() || result.insert(key, value).is_some() {
            return Err(AuthError::UrlPolicy);
        }
    }
    Ok(result)
}
fn decode_query(value: &str) -> AuthResult<String> {
    let mut result = Vec::new();
    let mut bytes = value.bytes();
    while let Some(byte) = bytes.next() {
        match byte {
            b'%' => {
                let hex = |b: u8| {
                    (b as char)
                        .to_digit(16)
                        .map(|v| v as u8)
                        .ok_or(AuthError::UrlPolicy)
                };
                let high = hex(bytes.next().ok_or(AuthError::UrlPolicy)?)?;
                let low = hex(bytes.next().ok_or(AuthError::UrlPolicy)?)?;
                result.push(high * 16 + low);
            }
            b'+' => result.push(b' '),
            value => result.push(value),
        }
    }
    let result = String::from_utf8(result).map_err(|_| AuthError::UrlPolicy)?;
    if result.chars().any(char::is_control) {
        return Err(AuthError::UrlPolicy);
    }
    Ok(result)
}
fn jsonp(bytes: &[u8], callback: &str) -> AuthResult<Value> {
    let text = std::str::from_utf8(bytes)
        .map_err(|_| AuthError::Protocol("invalid JSON encoding"))?
        .trim();
    let json = if text.starts_with('{') {
        text
    } else {
        text.strip_prefix(callback)
            .and_then(|v| v.strip_prefix('('))
            .and_then(|v| v.trim_end_matches(';').trim_end().strip_suffix(')'))
            .ok_or(AuthError::Protocol("unexpected JSONP callback"))?
            .trim()
    };
    serde_json::from_str(json).map_err(|_| AuthError::Protocol("invalid JSON response"))
}
fn bounded(mut reader: impl Read, limit: usize) -> AuthResult<Vec<u8>> {
    let mut bytes = Vec::new();
    reader
        .by_ref()
        .take(limit as u64 + 1)
        .read_to_end(&mut bytes)
        .map_err(|_| AuthError::Network)?;
    if bytes.len() > limit {
        return Err(AuthError::Limit);
    }
    Ok(bytes)
}
fn encode(value: &str) -> String {
    value
        .bytes()
        .map(|b| {
            if b.is_ascii_alphanumeric() || b"-._~".contains(&b) {
                (b as char).to_string()
            } else {
                format!("%{b:02X}")
            }
        })
        .collect()
}
fn import_cookies(value: &str) -> AuthResult<Cookies> {
    let value = value.trim();
    let value = if value
        .get(..7)
        .is_some_and(|prefix| prefix.eq_ignore_ascii_case("Cookie:"))
    {
        value[7..].trim()
    } else {
        value
    };
    if value.is_empty() || value.len() > 24 * 1024 || value.chars().any(char::is_control) {
        return Err(AuthError::PrivateFile);
    }
    const ALLOWED: [&str; 6] = [
        "BDUSS",
        "BDUSS_BFESS",
        "STOKEN",
        "BAIDUID",
        "BAIDUID_BFESS",
        "PTOKEN",
    ];
    let header = value.contains(';')
        || ALLOWED.iter().any(|name| {
            value
                .strip_prefix(name)
                .is_some_and(|tail| tail.starts_with('='))
        });
    let mut result = Cookies::default();
    if !header {
        if !cookie_value(value) {
            return Err(AuthError::PrivateFile);
        }
        result.0.push(Cookie {
            name: "BDUSS".into(),
            value: value.into(),
            domain: "baidu.com".into(),
            host_only: false,
            path: "/".into(),
        });
    } else {
        let mut seen = std::collections::BTreeSet::new();
        for (index, field) in value
            .split(';')
            .filter(|v| !v.trim().is_empty())
            .enumerate()
        {
            if index >= 128 {
                return Err(AuthError::Limit);
            }
            let (name, value) = field.trim().split_once('=').ok_or(AuthError::PrivateFile)?;
            if !cookie_name(name) {
                return Err(AuthError::PrivateFile);
            }
            if ALLOWED.contains(&name) {
                if value.is_empty() || !cookie_value(value) || !seen.insert(name) {
                    return Err(AuthError::PrivateFile);
                }
                result.0.push(Cookie {
                    name: name.into(),
                    value: value.into(),
                    domain: "baidu.com".into(),
                    host_only: false,
                    path: "/".into(),
                });
            }
        }
    }
    result.validate()?;
    if result.bduss().is_none() {
        return Err(AuthError::LoginRequired);
    }
    Ok(result)
}
fn now() -> AuthResult<u64> {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|v| v.as_secs())
        .map_err(|_| AuthError::InvalidState)
}
fn millis() -> AuthResult<u128> {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|v| v.as_millis())
        .map_err(|_| AuthError::InvalidState)
}
fn random_gid() -> AuthResult<String> {
    let mut bytes = [0_u8; 16];
    File::open("/dev/urandom")
        .and_then(|mut f| f.read_exact(&mut bytes))
        .map_err(|_| AuthError::PrivateFile)?;
    bytes[6] = (bytes[6] & 15) | 0x40;
    bytes[8] = (bytes[8] & 63) | 0x80;
    let hex: String = bytes.iter().map(|b| format!("{b:02X}")).collect();
    Ok(format!(
        "{}-{}-{}-{}-{}",
        &hex[..8],
        &hex[8..12],
        &hex[12..16],
        &hex[16..20],
        &hex[20..]
    ))
}
fn load_private<T: DeserializeOwned>(path: &Path) -> AuthResult<T> {
    let mut file = OpenOptions::new()
        .read(true)
        .custom_flags(libc::O_NOFOLLOW | libc::O_NONBLOCK)
        .open(path)
        .map_err(|_| AuthError::PrivateFile)?;
    let metadata = file.metadata().map_err(|_| AuthError::PrivateFile)?;
    if !metadata.is_file()
        || metadata.permissions().mode() & 0o077 != 0
        || metadata.len() > MAX_PRIVATE
    {
        return Err(AuthError::PrivateFile);
    }
    let bytes = bounded(&mut file, MAX_PRIVATE as usize)?;
    serde_json::from_slice(&bytes).map_err(|_| AuthError::PrivateFile)
}
fn save_private<T: Serialize>(path: &Path, value: &T) -> AuthResult<()> {
    let parent = path.parent().ok_or(AuthError::PrivateFile)?;
    DirBuilder::new()
        .recursive(true)
        .mode(0o700)
        .create(parent)
        .map_err(|_| AuthError::PrivateFile)?;
    let metadata = fs::symlink_metadata(parent).map_err(|_| AuthError::PrivateFile)?;
    if !metadata.is_dir() || metadata.permissions().mode() & 0o077 != 0 {
        return Err(AuthError::PrivateFile);
    }
    if let Ok(metadata) = fs::symlink_metadata(path) {
        if !metadata.is_file() || metadata.permissions().mode() & 0o077 != 0 {
            return Err(AuthError::PrivateFile);
        }
    }
    let bytes = serde_json::to_vec(value).map_err(|_| AuthError::PrivateFile)?;
    if bytes.len() as u64 > MAX_PRIVATE {
        return Err(AuthError::Limit);
    }
    let filename = path
        .file_name()
        .and_then(|v| v.to_str())
        .ok_or(AuthError::PrivateFile)?;
    let temporary = parent.join(format!(".{filename}.{}.next", random_gid()?));
    let result = (|| {
        let mut file = OpenOptions::new()
            .write(true)
            .create_new(true)
            .mode(0o600)
            .open(&temporary)
            .map_err(|_| AuthError::PrivateFile)?;
        file.write_all(&bytes)
            .and_then(|_| file.sync_all())
            .map_err(|_| AuthError::PrivateFile)?;
        fs::rename(&temporary, path).map_err(|_| AuthError::PrivateFile)?;
        File::open(parent)
            .and_then(|f| f.sync_all())
            .map_err(|_| AuthError::PrivateFile)
    })();
    if result.is_err() {
        let _ = fs::remove_file(temporary);
    }
    result
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn reply_token_comes_from_the_same_authenticated_bound_profile() {
        let mut profile = serde_json::json!({"no":0,"data":{"id":42,"portrait":"tb.bound","is_login":true,"tbs":"syntheticTbs","itb_tbs":"differentToken","email":"never-output"}});
        assert_eq!(
            profile_reply_tbs(&serde_json::to_vec(&profile).unwrap(), "tb.bound", Some(42))
                .unwrap(),
            "syntheticTbs"
        );
        assert!(
            profile_reply_tbs(&serde_json::to_vec(&profile).unwrap(), "another", Some(42)).is_err()
        );
        profile["data"]["is_login"] = serde_json::json!(false);
        assert!(
            profile_reply_tbs(&serde_json::to_vec(&profile).unwrap(), "tb.bound", Some(42))
                .is_err()
        );
    }
    fn attempt() -> LoginAttempt {
        let created_at = now().unwrap();
        LoginAttempt(AttemptRecord {
            schema: 1,
            gid: "fixed-test-gid".into(),
            sign: "test-sign".into(),
            created_at,
            expires_at: created_at + LOCAL_LIFETIME,
            cookies: Cookies::default(),
            status: LoginStatus::Waiting,
            confirmed_token: None,
            exchange_attempted: false,
        })
    }
    #[test]
    fn qr_states_require_actual_confirmation_and_never_expose_temporary_token() {
        let mut attempt = attempt();
        assert_eq!(
            apply_poll(&mut attempt, serde_json::json!({"errno":1})).unwrap(),
            LoginStatus::Waiting
        );
        assert_eq!(
            apply_poll(
                &mut attempt,
                serde_json::json!({"errno":0,"channel_v":"{\"status\":1}"})
            )
            .unwrap(),
            LoginStatus::Scanned
        );
        assert!(
            apply_poll(
                &mut attempt,
                serde_json::json!({"errno":0,"channel_v":{"status":0}})
            )
            .is_err()
        );
        assert_eq!(apply_poll(&mut attempt,serde_json::json!({"errno":0,"channel_id":"test-sign","channel_v":{"status":0,"v":"synthetic-private-token"}})).unwrap(),LoginStatus::Confirmed);
        assert!(!format!("{attempt:?}").contains("synthetic-private"));
        assert!(attempt.0.cookies.bduss().is_none());
        assert!(apply_poll(&mut attempt,serde_json::json!({"errno":0,"channel_id":"different","channel_v":{"status":0,"v":"a"}})).is_err());
    }
    #[test]
    fn exchange_recovery_requires_confirmed_formal_cookies_before_network() {
        let api = AuthClient::new().unwrap();
        let mut attempt = attempt();
        assert!(!attempt.exchange_attempted());
        assert!(!attempt.has_formal_bduss());
        assert!(matches!(
            api.resume_exchanged_login(&attempt),
            Err(AuthError::InvalidState)
        ));
        attempt.0.status = LoginStatus::Confirmed;
        attempt.0.exchange_attempted = true;
        assert!(matches!(
            api.resume_exchanged_login(&attempt),
            Err(AuthError::InvalidState)
        ));
        attempt
            .0
            .cookies
            .absorb(
                "passport.baidu.com",
                "BDUSS=synthetic-private-bduss; Domain=.baidu.com; Path=/",
            )
            .unwrap();
        assert!(attempt.has_formal_bduss());
        attempt.0.status = LoginStatus::Scanned;
        assert!(matches!(
            api.resume_exchanged_login(&attempt),
            Err(AuthError::InvalidState)
        ));
    }
    #[test]
    fn cookie_scope_and_fixed_endpoint_policy_reject_leaks() {
        let mut cookies = Cookies::default();
        cookies
            .absorb(
                "passport.baidu.com",
                "BDUSS=synthetic-private-bduss; Domain=.baidu.com; Path=/; Secure; HttpOnly",
            )
            .unwrap();
        cookies
            .absorb("passport.baidu.com", "STOKEN=passport-only; Path=/")
            .unwrap();
        assert!(
            cookies
                .header("tieba.baidu.com", "/")
                .unwrap()
                .unwrap()
                .contains("BDUSS=")
        );
        assert!(
            !cookies
                .header("tieba.baidu.com", "/")
                .unwrap()
                .unwrap()
                .contains("STOKEN=")
        );
        assert!(
            cookies
                .header("baidu.com.evil.invalid", "/")
                .unwrap()
                .is_none()
        );
        assert!(
            cookies
                .absorb("passport.baidu.com", "BAD=x; Domain=evil.invalid")
                .is_err()
        );
        cookies
            .absorb(
                "passport.baidu.com",
                "BDUSS=deleted; Domain=.baidu.com; Path=/; Max-Age=0",
            )
            .unwrap();
        assert!(cookies.bduss().is_none());
        for url in [
            "http://passport.baidu.com/channel/unicast",
            "https://passport.baidu.com.evil.invalid/channel/unicast",
            "https://passport.baidu.com@evil.invalid/channel/unicast",
            "https://passport.baidu.com/other",
            "https://tieba.baidu.com/f/user/json_userinfo#x",
            "https://tieba.baidu.com:444/f/user/json_userinfo",
        ] {
            assert!(check_url(url).is_err(), "{url}");
        }
    }
    #[test]
    fn account_sso_has_only_two_scoped_hops_and_cannot_redirect_qr_requests() {
        let auth = format!(
            "https://passport.baidu.com/v3/login/api/auth/?tpl=tb&jump=&return_type=3&u={}",
            encode(USERINFO_URL)
        );
        let returned =
            format!("{USERINFO_URL}?errmsg=&errno=0&ssnerror=0&stoken=synthetic%2Bstoken");
        assert!(identity_url(USERINFO_URL, 0).is_ok());
        assert!(identity_url(&auth, 1).is_ok());
        assert!(identity_url(&returned, 2).is_ok());
        assert!(check_url(&auth).is_err());
        for bad in [
            auth.replace("return_type=3", "return_type=5"),
            auth.replace("jump=", "jump=1"),
            auth.replace("tpl=tb", "tpl=mn"),
            auth.replace(&encode(USERINFO_URL), &encode("https://evil.invalid/")),
            format!("{auth}&tpl=tb"),
            format!("{auth}&extra=1"),
            auth.replace("passport.baidu.com", "passport.baidu.com.evil.invalid"),
            auth.replace("https://", "http://"),
        ] {
            assert!(identity_url(&bad, 1).is_err(), "{bad}");
        }
        for bad in [
            returned.replace("tieba.baidu.com", "passport.baidu.com"),
            returned.replace("/f/user/json_userinfo", "/other"),
            format!("{returned}&u=https%3A%2F%2Fevil.invalid"),
            format!("{returned}&stoken=other"),
            returned.replace("errno=0", "errno=123"),
            returned.replace("ssnerror=0", "ssnerror=1"),
            returned.replace("synthetic%2Bstoken", "%0AInjected"),
        ] {
            assert!(identity_url(&bad, 2).is_err(), "{bad}");
        }
        assert!(identity_url(&returned, 3).is_err());
        assert!(identity_url(USERINFO_URL, 3).is_ok());
        assert!(identity_url(USERINFO_URL, 4).is_err());
        let mut cookies = Cookies::default();
        cookies
            .absorb("passport.baidu.com", "STOKEN=passport-only; Path=/")
            .unwrap();
        assert!(identity_after_callback("https://tieba.baidu.com/", &cookies).is_err());
        cookies
            .absorb("tieba.baidu.com", "STOKEN=tieba-only; Path=/")
            .unwrap();
        assert_eq!(
            identity_after_callback("https://tieba.baidu.com/", &cookies).unwrap(),
            USERINFO_URL
        );
        assert!(identity_after_callback("https://evil.invalid/", &cookies).is_err());
        assert!(
            identity_after_callback("https://tieba.baidu.com/?ticket=private", &cookies).is_err()
        );
        assert_eq!(
            cookies
                .header("passport.baidu.com", "/v3/login/api/auth/")
                .unwrap()
                .unwrap(),
            "STOKEN=passport-only"
        );
        assert_eq!(
            cookies
                .header("tieba.baidu.com", "/f/user/json_userinfo")
                .unwrap()
                .unwrap(),
            "STOKEN=tieba-only"
        );
    }
    #[test]
    fn account_projection_excludes_credentials_and_requires_stable_identity() {
        let account=account_info(serde_json::json!({"no":0,"data":{"user_id":"123","user_name":"test","name_show":"测试","user_portrait":"portrait-id","session_id":"must-not-leak","bduss":"must-not-leak"}})).unwrap();
        assert_eq!(account.stable_identity(), "uid:123");
        assert!(
            !serde_json::to_string(&account)
                .unwrap()
                .contains("must-not-leak")
        );
        assert_eq!(
            account_info(serde_json::json!({"no":0,"data":{"user_name":"anonymous"}})).unwrap_err(),
            AuthError::LoginRequired
        );
        assert!(account_info(serde_json::json!({"no":4,"data":{}})).is_err());
    }
    #[test]
    fn jsonp_is_data_not_executable_code_and_bounds_are_strict() {
        assert_eq!(
            jsonp(b"agent_runtime_qr({\"errno\":1});", CALLBACK).unwrap()["errno"],
            1
        );
        assert!(jsonp(b"wrong({\"errno\":1});", CALLBACK).is_err());
        assert!(jsonp(b"agent_runtime_qr({});alert(1)", CALLBACK).is_err());
        assert_eq!(bounded(&b"12345"[..], 4), Err(AuthError::Limit));
        assert_eq!(encode("+/?&="), "%2B%2F%3F%26%3D");
    }
    #[test]
    fn private_attempt_and_session_survive_restart_and_reject_public_files() {
        let root = std::env::temp_dir().join(format!("tieba-auth-{}", random_gid().unwrap()));
        DirBuilder::new().mode(0o700).create(&root).unwrap();
        let path = root.join("attempt.json");
        let mut attempt = attempt();
        apply_poll(
            &mut attempt,
            serde_json::json!({"errno":0,"channel_v":{"status":0,"v":"synthetic-private-token"}}),
        )
        .unwrap();
        attempt.save(&path).unwrap();
        let restored = LoginAttempt::load(&path).unwrap();
        assert_eq!(restored.status(), LoginStatus::Confirmed);
        assert_eq!(
            fs::metadata(&path).unwrap().permissions().mode() & 0o777,
            0o600
        );
        let mut cookies = Cookies::default();
        cookies
            .absorb(
                "passport.baidu.com",
                "BDUSS=synthetic-private-bduss; Domain=.baidu.com; Path=/",
            )
            .unwrap();
        cookies
            .absorb("tieba.baidu.com", "STOKEN=synthetic-tieba-stoken; Path=/")
            .unwrap();
        let session = Session(SessionRecord {
            schema: 1,
            cookies,
            account: AccountInfo {
                user_id: Some(123),
                user_name: "test".into(),
                display_name: "test".into(),
                portrait: "portrait-id".into(),
            },
        });
        let session_path = root.join("session.json");
        session.save(&session_path).unwrap();
        assert_eq!(
            Session::load(&session_path).unwrap().stable_identity(),
            "uid:123"
        );
        assert!(
            Session::load(&session_path)
                .unwrap()
                .0
                .cookies
                .header("tieba.baidu.com", "/f/user/json_userinfo")
                .unwrap()
                .unwrap()
                .contains("STOKEN=synthetic-tieba-stoken")
        );
        assert!(!format!("{session:?}").contains("synthetic-private"));
        fs::set_permissions(&path, fs::Permissions::from_mode(0o644)).unwrap();
        assert!(LoginAttempt::load(&path).is_err());
        let symlink = root.join("link.json");
        std::os::unix::fs::symlink(&session_path, &symlink).unwrap();
        assert!(Session::load(&symlink).is_err());
        assert!(session.save(&symlink).is_err());
        fs::remove_dir_all(root).unwrap();
    }
    #[test]
    fn cookie_import_supports_formal_bduss_and_retains_only_account_cookies() {
        let bare = import_cookies("synthetic-private-bduss").unwrap();
        assert_eq!(bare.bduss(), Some("synthetic-private-bduss"));
        let header = import_cookies(
            "cookie: BDUSS=synthetic-private-bduss; STOKEN=synthetic-stoken; irrelevant=discard-me",
        )
        .unwrap();
        let scoped = header.header("tieba.baidu.com", "/").unwrap().unwrap();
        assert!(scoped.contains("STOKEN=synthetic-stoken"));
        assert!(!scoped.contains("discard-me"));
        assert!(header.header("unrelated.invalid", "/").unwrap().is_none());
        for bad in [
            "",
            "STOKEN=only-stoken",
            "BDUSS=one; BDUSS=two",
            "BDUSS=one\r\nX-Injected: yes",
            "BDUSS=",
        ] {
            assert!(import_cookies(bad).is_err());
        }
    }
}
