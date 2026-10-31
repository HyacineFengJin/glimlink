// SPDX-License-Identifier: MIT
//! Personal-account WebWx experiment. No Bot identity, browser, UOS patch or send action.
mod desktop;
mod http;
mod model;
mod state;

pub use http::Config;
use http::{Transport, query, ticket_url, validate_redirect};
pub use model::{Contact, ContactPage, MediaKind, Message, SessionInfo, SyncBatch, SyncStatus};
use serde::{Deserialize, Serialize};
use serde_json::json;
pub use state::{LoginAttempt, Session};
use std::{fmt, fs::OpenOptions, io::Write, os::unix::fs::OpenOptionsExt, path::Path};

pub type Result<T> = std::result::Result<T, Error>;
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum LoginProfile {
    #[default]
    Web,
    DesktopCompatibility,
}
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum Error {
    InvalidInput,
    InvalidState,
    ExpiredAttempt,
    Network,
    UrlPolicy,
    Http { status: u16 },
    Limit,
    Protocol,
    LoginHtmlResponse { status: u16 },
    UnexpectedLoginRedirect { status: u16 },
    WebLoginPageUnavailable,
    Io,
    Platform { code: i64 },
    AuthExpired { code: i64 },
    WebLoginUnavailable { code: i64 },
    IdentityMismatch,
    UnsupportedMedia,
}
impl fmt::Display for Error {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "Weixin WebWx error: {self:?}")
    }
}
impl std::error::Error for Error {}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum LoginStatus {
    Waiting,
    Scanned,
    Confirmed,
    Expired,
}
#[derive(Debug, Clone, Serialize)]
pub struct QrReceipt {
    pub bytes: u64,
    pub mime: String,
    pub expires_in_seconds: u64,
}
#[derive(Debug, Clone, Serialize)]
pub struct MediaReceipt {
    pub bytes: u64,
    pub mime: Option<String>,
    pub kind: MediaKind,
}

pub struct Client {
    transport: Transport,
    profile: LoginProfile,
}
impl Client {
    pub fn new(config: Config) -> Result<Self> {
        Ok(Self {
            profile: config.profile,
            transport: Transport::new(config)?,
        })
    }
    pub fn begin_login(&self) -> Result<LoginAttempt> {
        let mut cookies = Vec::new();
        let url = query(
            "https://login.wx.qq.com/jslogin",
            &[
                ("appid", "wx782c26e4c19acffb".into()),
                (
                    "redirect_uri",
                    match self.profile {
                        LoginProfile::Web => {
                            "https://wx.qq.com/cgi-bin/mmwebwx-bin/webwxnewloginpage"
                        }
                        LoginProfile::DesktopCompatibility => {
                            "https://wx.qq.com/cgi-bin/mmwebwx-bin/webwxnewloginpage?mod=desktop"
                        }
                    }
                    .into(),
                ),
                ("fun", "new".into()),
                ("lang", "zh_CN".into()),
                ("_", state::now_millis()?.to_string()),
            ],
        );
        let body = self
            .transport
            .bytes(&url, &mut cookies, 16384, None, None)?;
        let text = std::str::from_utf8(&body).map_err(|_| Error::Protocol)?;
        if parse_integer_assignment(text, "window.QRLogin.code")? != 200 {
            return Err(Error::Protocol);
        }
        let uuid = parse_quoted_assignment(text, "window.QRLogin.uuid")?;
        let mut attempt = LoginAttempt::new(uuid, cookies, self.profile)?;
        attempt.validate()?;
        // No session is displaced by fetching an independent QR attempt.
        attempt.scanned = false;
        Ok(attempt)
    }
    pub fn download_qr(&self, attempt: &mut LoginAttempt, path: &Path) -> Result<QrReceipt> {
        if attempt.profile() != self.profile {
            return Err(Error::InvalidState);
        }
        attempt.validate()?;
        attempt.ensure_fresh()?;
        let url = format!("https://login.weixin.qq.com/qrcode/{}", attempt.uuid);
        let bytes = self
            .transport
            .bytes(&url, &mut attempt.cookies, 256 * 1024, None, None)?;
        let mime = if bytes.starts_with(b"\x89PNG\r\n\x1a\n") {
            "image/png"
        } else if bytes.starts_with(b"\xff\xd8\xff") {
            "image/jpeg"
        } else if bytes.starts_with(b"GIF8") {
            "image/gif"
        } else {
            return Err(Error::Protocol);
        };
        state::ensure_private_parent(path)?;
        let mut out = OpenOptions::new()
            .write(true)
            .create_new(true)
            .mode(0o600)
            .custom_flags(libc::O_NOFOLLOW | libc::O_NONBLOCK)
            .open(path)
            .map_err(|_| Error::Io)?;
        if out.write_all(&bytes).and_then(|_| out.sync_all()).is_err() {
            drop(out);
            let _ = std::fs::remove_file(path);
            return Err(Error::Io);
        }
        Ok(QrReceipt {
            bytes: bytes.len() as u64,
            mime: mime.into(),
            expires_in_seconds: attempt.remaining_seconds()?,
        })
    }
    /// Exactly one bounded long-poll; the caller controls repeated polling.
    pub fn poll_login(&self, attempt: &mut LoginAttempt) -> Result<LoginStatus> {
        if attempt.profile() != self.profile {
            return Err(Error::InvalidState);
        }
        attempt.validate()?;
        attempt.ensure_fresh()?;
        if attempt.redirect.is_some() {
            return Ok(LoginStatus::Confirmed);
        }
        let now = state::now_millis()?;
        let url = query(
            "https://login.wx.qq.com/cgi-bin/mmwebwx-bin/login",
            &[
                ("uuid", attempt.uuid.clone()),
                ("tip", if attempt.scanned { "0" } else { "1" }.into()),
                ("loginicon", "true".into()),
                ("r", (now / 1000 / 1579).to_string()),
                ("_", (now / 1000).to_string()),
            ],
        );
        let bytes = self
            .transport
            .bytes(&url, &mut attempt.cookies, 512 * 1024, None, None)?;
        let text = std::str::from_utf8(&bytes).map_err(|_| Error::Protocol)?;
        match parse_integer_assignment(text, "window.code")? {
            200 => {
                let redirect = parse_quoted_assignment(text, "window.redirect_uri")?;
                validate_redirect(&redirect)?;
                attempt.redirect = Some(redirect);
                Ok(LoginStatus::Confirmed)
            }
            201 => {
                attempt.scanned = true;
                Ok(LoginStatus::Scanned)
            }
            408 => Ok(LoginStatus::Waiting),
            400 => Ok(LoginStatus::Expired),
            code => Err(Error::Platform { code }),
        }
    }
    pub fn finish_login(&self, attempt: &mut LoginAttempt) -> Result<Session> {
        if attempt.profile() != self.profile {
            return Err(Error::InvalidState);
        }
        attempt.validate()?;
        attempt.ensure_fresh()?;
        let redirect = attempt
            .redirect
            .as_ref()
            .ok_or(Error::InvalidState)?
            .clone();
        let host = validate_redirect(&redirect)?;
        let url = ticket_url(&redirect)?;
        let response = self.transport.ticket_response(&url, &mut attempt.cookies)?;
        let ticket = model::ticket_response(response)?;
        let mut session = Session::new(
            host,
            ticket,
            attempt.device_id.clone(),
            attempt.cookies.clone(),
        )?;
        session.profile = attempt.profile();
        self.initialize(&mut session)?;
        Ok(session)
    }
    fn initialize(&self, session: &mut Session) -> Result<()> {
        let url = query(
            &session.endpoint("webwxinit"),
            &[
                ("r", (!((state::now_millis()? / 1000) as i64)).to_string()),
                ("lang", "zh_CN".into()),
                ("pass_ticket", session.pass_ticket.clone()),
            ],
        );
        let body = json!({"BaseRequest":session.base_request()});
        let value = self
            .transport
            .json(&url, &mut session.cookies, Some(&body))?;
        model::check_response(&value)?;
        let user = model::contact(value.get("User").ok_or(Error::Protocol)?)?;
        if !session.user.username.is_empty() && session.user.username != user.username {
            return Err(Error::IdentityMismatch);
        }
        let key = model::parse_sync_key(value.get("SyncKey").ok_or(Error::Protocol)?)?;
        session.user = user;
        session.sync_key = key;
        session.validate()?;
        Ok(())
    }
    /// Validate the persisted cursor. Never webwxinit over an existing cursor.
    pub fn resume(&self, session: &mut Session) -> Result<SessionInfo> {
        session.validate()?;
        self.sync_check(session)?;
        Ok(session.info())
    }
    pub fn contacts(&self, session: &mut Session, sequence: u64) -> Result<ContactPage> {
        session.validate()?;
        let url = query(
            &session.endpoint("webwxgetcontact"),
            &[
                ("r", state::now_millis()?.to_string()),
                ("skey", session.skey.clone()),
                ("seq", sequence.to_string()),
            ],
        );
        let value = self.transport.json(&url, &mut session.cookies, None)?;
        model::contacts(value)
    }
    pub fn sync_check(&self, session: &mut Session) -> Result<SyncStatus> {
        session.validate()?;
        let url = query(
            &format!(
                "https://webpush.{}/cgi-bin/mmwebwx-bin/synccheck",
                session.host
            ),
            &[
                ("r", state::now_millis()?.to_string()),
                ("skey", session.skey.clone()),
                ("sid", session.sid.clone()),
                ("uin", session.uin.to_string()),
                ("deviceid", session.device_id.clone()),
                ("synckey", session.sync_string()),
                ("_", state::now_millis()?.to_string()),
            ],
        );
        let body = self
            .transport
            .bytes(&url, &mut session.cookies, 4096, None, None)?;
        model::parse_sync_check(&body)
    }
    /// Updates the in-memory cursor only after the complete batch has parsed.
    /// Persist this Session only AFTER the caller durably stores the messages.
    pub fn sync(&self, session: &mut Session) -> Result<SyncBatch> {
        session.validate()?;
        let url = query(
            &session.endpoint("webwxsync"),
            &[
                ("sid", session.sid.clone()),
                ("skey", session.skey.clone()),
                ("pass_ticket", session.pass_ticket.clone()),
            ],
        );
        let body = json!({"BaseRequest":session.base_request(),"SyncKey":model::sync_key_value(&session.sync_key),"rr":!((state::now_millis()?/1000) as i64)});
        let value = self
            .transport
            .json(&url, &mut session.cookies, Some(&body))?;
        let (batch, next) = model::sync_batch(value, &session.user.username)?;
        session.sync_key = next;
        Ok(batch)
    }
    pub fn download_media<W: Write + ?Sized>(
        &self,
        session: &mut Session,
        message: &Message,
        output: &mut W,
        max_bytes: u64,
    ) -> Result<MediaReceipt> {
        session.validate()?;
        if message.id.is_empty()
            || message.id.len() > 64
            || !message.id.bytes().all(|b| b.is_ascii_digit())
        {
            return Err(Error::InvalidInput);
        }
        let kind = message.media.clone().ok_or(Error::UnsupportedMedia)?;
        let (endpoint, key) = match kind {
            MediaKind::Image => ("webwxgetmsgimg", "MsgID"),
            MediaKind::Voice => ("webwxgetvoice", "msgid"),
            MediaKind::Video => ("webwxgetvideo", "msgid"),
        };
        let url = query(
            &session.endpoint(endpoint),
            &[(key, message.id.clone()), ("skey", session.skey.clone())],
        );
        let receipt = self.transport.download(
            &url,
            &mut session.cookies,
            output,
            max_bytes,
            !matches!(kind, MediaKind::Image),
        )?;
        Ok(MediaReceipt {
            bytes: receipt.0,
            mime: receipt.1,
            kind,
        })
    }
}

fn parse_integer_assignment(text: &str, name: &str) -> Result<i64> {
    let tail = text.split_once(name).ok_or(Error::Protocol)?.1.trim_start();
    let tail = tail.strip_prefix('=').ok_or(Error::Protocol)?.trim_start();
    let digits = tail
        .split(|c: char| !c.is_ascii_digit() && c != '-')
        .next()
        .ok_or(Error::Protocol)?;
    if digits.is_empty() || digits.len() > 12 {
        return Err(Error::Protocol);
    }
    digits.parse().map_err(|_| Error::Protocol)
}
fn parse_quoted_assignment(text: &str, name: &str) -> Result<String> {
    let tail = text.split_once(name).ok_or(Error::Protocol)?.1.trim_start();
    let tail = tail.strip_prefix('=').ok_or(Error::Protocol)?.trim_start();
    let quote = tail.chars().next().ok_or(Error::Protocol)?;
    if !matches!(quote, '\'' | '"') {
        return Err(Error::Protocol);
    }
    let content = tail[1..].split_once(quote).ok_or(Error::Protocol)?.0;
    if content.len() > 8192 || content.contains('\\') || content.chars().any(char::is_control) {
        return Err(Error::Protocol);
    }
    Ok(content.into())
}

#[cfg(test)]
mod workflow_tests;
