// SPDX-License-Identifier: MIT
//! Bounded, read-only X web-session client. No browser, JavaScript runtime, login
//! automation, arbitrary endpoint, writes, media eager downloads, or stdout logs.
mod bootstrap;
mod model;
mod transaction;
mod transport;

pub use model::{Media, MediaKind, TimelinePage, Tweet, TweetCounts, User, VideoVariant};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use std::{fmt, path::Path};
pub use transport::{Config, DownloadInfo, RequestDiagnostic, RequestStage, ResponseKind};

pub type Result<T> = std::result::Result<T, Error>;

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Error {
    InvalidConfig,
    InvalidInput,
    LoginRequired,
    VerificationRequired,
    RateLimited { reset_at: Option<u64> },
    PermissionDenied,
    RequestDenied,
    NotFound,
    ProtocolChanged,
    RedirectDenied,
    Network,
    Limit,
    Io,
}
impl Error {
    pub fn code(&self) -> &'static str {
        match self {
            Self::InvalidConfig => "invalid_config",
            Self::InvalidInput => "invalid_input",
            Self::LoginRequired => "needs_auth",
            Self::VerificationRequired => "verification_required",
            Self::RateLimited { .. } => "rate_limited",
            Self::PermissionDenied => "permission_denied",
            Self::RequestDenied => "request_denied",
            Self::NotFound => "not_found",
            Self::ProtocolChanged => "protocol_changed",
            Self::RedirectDenied => "redirect_denied",
            Self::Network => "network_error",
            Self::Limit => "limit_exceeded",
            Self::Io => "io_error",
        }
    }
}
impl fmt::Display for Error {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "X adapter: {}", self.code())
    }
}
impl std::error::Error for Error {}

/// Private cookie material; Serialize is for private management storage only.
#[derive(Clone, Serialize, Deserialize)]
#[serde(try_from = "RawSession")]
pub struct Session {
    auth_token: String,
    ct0: String,
}
#[derive(Deserialize)]
struct RawSession {
    auth_token: String,
    ct0: String,
}
impl TryFrom<RawSession> for Session {
    type Error = Error;
    fn try_from(v: RawSession) -> Result<Self> {
        Self::new(v.auth_token, v.ct0)
    }
}
impl fmt::Debug for Session {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("Session([REDACTED])")
    }
}
impl Session {
    pub fn new(auth_token: impl Into<String>, ct0: impl Into<String>) -> Result<Self> {
        let out = Self {
            auth_token: auth_token.into(),
            ct0: ct0.into(),
        };
        if !cookie_value(&out.auth_token, 4096) || !cookie_value(&out.ct0, 512) {
            return Err(Error::InvalidInput);
        }
        Ok(out)
    }
    /// Accept minimal JSON or a Cookie header. Retains only auth_token and ct0.
    pub fn from_private_input(input: &str) -> Result<Self> {
        if input.len() > 32 * 1024 {
            return Err(Error::Limit);
        }
        let text = input.trim();
        if text.starts_with('{') {
            return serde_json::from_str(text).map_err(|_| Error::InvalidInput);
        }
        if text.bytes().any(|b| b.is_ascii_control()) {
            return Err(Error::InvalidInput);
        }
        let text = if text
            .get(..7)
            .is_some_and(|s| s.eq_ignore_ascii_case("cookie:"))
        {
            &text[7..]
        } else {
            text
        };
        let mut auth = None;
        let mut csrf = None;
        for item in text.split(';') {
            let Some((key, value)) = item.trim().split_once('=') else {
                continue;
            };
            let slot = match key.trim() {
                "auth_token" => &mut auth,
                "ct0" => &mut csrf,
                _ => continue,
            };
            if slot.is_some() {
                return Err(Error::InvalidInput);
            }
            *slot = Some(value.trim().to_owned());
        }
        Self::new(
            auth.ok_or(Error::InvalidInput)?,
            csrf.ok_or(Error::InvalidInput)?,
        )
    }
    fn cookie_header(&self) -> String {
        format!("auth_token={}; ct0={}", self.auth_token, self.ct0)
    }
}

#[cfg(test)]
mod tests;
fn cookie_value(v: &str, max: usize) -> bool {
    !v.is_empty()
        && v.len() <= max
        && v.bytes()
            .all(|b| (0x21..=0x7e).contains(&b) && !b"\";,\\".contains(&b))
}
pub(crate) fn id(v: &str) -> Result<()> {
    if v.is_empty()
        || v.len() > 24
        || !v.bytes().all(|b| b.is_ascii_digit())
        || v.bytes().all(|b| b == b'0')
    {
        Err(Error::InvalidInput)
    } else {
        Ok(())
    }
}
pub(crate) fn screen_name(v: &str) -> Result<&str> {
    let v = v.strip_prefix('@').unwrap_or(v);
    if v.is_empty() || v.len() > 15 || !v.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'_') {
        Err(Error::InvalidInput)
    } else {
        Ok(v)
    }
}

pub struct Client {
    transport: transport::Transport,
    web: Option<bootstrap::WebState>,
}
impl Client {
    /// Bounded request metadata only: never URLs, headers, bodies or credentials.
    pub fn last_request(&self) -> Option<RequestDiagnostic> {
        self.transport.last_request()
    }
    pub fn clear_diagnostic(&self) {
        self.transport.clear_diagnostic();
    }
    pub fn new(config: Config, session: Session) -> Result<Self> {
        Ok(Self {
            transport: transport::Transport::new(config, session)?,
            web: None,
        })
    }
    /// Authenticated settings identifies the viewer; user-provided twid is never trusted.
    pub fn current_user(&mut self) -> Result<User> {
        self.ensure_web()?;
        let value = self.get_json("/i/api/1.1/account/settings.json", &[])?;
        let name = value
            .get("screen_name")
            .and_then(Value::as_str)
            .ok_or(Error::ProtocolChanged)?;
        let user = self.user_by_screen_name(name)?;
        if let Some(actual) = value.get("id_str").and_then(Value::as_str) {
            if actual != user.id {
                return Err(Error::ProtocolChanged);
            }
        }
        Ok(user)
    }
    pub fn user_by_screen_name(&mut self, name: &str) -> Result<User> {
        let name = screen_name(name)?.to_owned();
        let op = self.operation(bootstrap::OperationKind::UserName)?;
        let mut vars = user_name_variables(op.modern, &name);
        op.provided_variables(&mut vars);
        let raw = self.graphql(&op, vars)?;
        let user = model::user_response(&raw)?;
        if !user.screen_name.eq_ignore_ascii_case(&name) {
            return Err(Error::ProtocolChanged);
        }
        Ok(user)
    }
    pub fn user_by_id(&mut self, user_id: &str) -> Result<User> {
        id(user_id)?;
        let op = self.operation(bootstrap::OperationKind::UserId)?;
        let raw = self.graphql(
            &op,
            json!({"userId":user_id,"withSafetyModeUserFields":true}),
        )?;
        if op.modern {
            let name = model::redirect_screen_name(&raw)?;
            let user = self.user_by_screen_name(&name)?;
            if user.id != user_id {
                return Err(Error::ProtocolChanged);
            }
            Ok(user)
        } else {
            let user = model::user_response(&raw)?;
            if user.id != user_id {
                return Err(Error::ProtocolChanged);
            }
            Ok(user)
        }
    }
    pub fn tweet(&mut self, tweet_id: &str) -> Result<Tweet> {
        id(tweet_id)?;
        let op = self.operation(bootstrap::OperationKind::Tweet)?;
        let mut vars = if op.modern {
            json!({"restId":tweet_id,"withFocalTweet":true,"withCommunityNotesPivot":false,"withTranslateButton":false,"withEndCard":false,"withNestedQuotePreview":false})
        } else {
            json!({"focalTweetId":tweet_id,"with_rux_injections":false,"includePromotedContent":false,"withCommunity":true,"withQuickPromoteEligibilityTweetFields":true,"withBirdwatchNotes":true,"withVoice":true,"withV2Timeline":true})
        };
        op.provided_variables(&mut vars);
        model::tweet_response(&self.graphql(&op, vars)?, tweet_id)
    }
    pub fn user_timeline(
        &mut self,
        user_id: &str,
        cursor: Option<&str>,
        limit: u32,
    ) -> Result<TimelinePage> {
        id(user_id)?;
        if !(1..=20).contains(&limit)
            || cursor.is_some_and(|v| v.len() > 4096 || v.bytes().any(|b| b.is_ascii_control()))
        {
            return Err(Error::InvalidInput);
        }
        let op = self.operation(bootstrap::OperationKind::Timeline)?;
        let mut vars = if op.modern {
            let user = self.user_by_id(user_id)?;
            json!({"screenName":user.screen_name,"count":limit,"cursor":cursor,"sortByMostLiked":false})
        } else {
            json!({"userId":user_id,"count":limit,"includePromotedContent":false,"withQuickPromoteEligibilityTweetFields":true,"withVoice":true,"withV2Timeline":true})
        };
        if let Some(cursor) = cursor {
            vars["cursor"] = json!(cursor);
        }
        op.provided_variables(&mut vars);
        model::timeline_response(&self.graphql(&op, vars)?, user_id, cursor, limit as usize)
    }
    pub fn download_media(
        &self,
        url: &str,
        destination: &Path,
        max_bytes: u64,
    ) -> Result<DownloadInfo> {
        self.transport.download_media(url, destination, max_bytes)
    }
    /// The authenticated account's real home query. No public/Following
    /// substitute is attempted when the requested For You query is unavailable.
    /// Query IDs and Relay providers come from the published website bundles.
    pub fn home_timeline(
        &mut self,
        recommended: bool,
        cursor: Option<&str>,
        limit: u32,
    ) -> Result<TimelinePage> {
        if !(1..=20).contains(&limit)
            || cursor.is_some_and(|v| {
                v.is_empty() || v.len() > 4096 || v.bytes().any(|b| b.is_ascii_control())
            })
        {
            return Err(Error::InvalidInput);
        }
        let op = self.operation(if recommended {
            bootstrap::OperationKind::HomeRecommended
        } else {
            bootstrap::OperationKind::HomeFollowing
        })?;
        let mut vars = home_variables(op.modern, recommended, cursor, limit);
        op.provided_variables(&mut vars);
        // The Runtime retains a bounded server-page tail before continuing.
        model::home_response(&self.graphql(&op, vars)?, recommended, cursor, 100)
    }
    fn ensure_web(&mut self) -> Result<()> {
        if self.web.is_none() {
            self.web = Some(bootstrap::WebState::load(&self.transport)?);
        }
        Ok(())
    }
    fn operation(&mut self, kind: bootstrap::OperationKind) -> Result<bootstrap::Operation> {
        self.ensure_web()?;
        self.web
            .as_mut()
            .ok_or(Error::ProtocolChanged)?
            .operation(&self.transport, kind)
    }
    fn get_json(&self, path: &str, query: &[(&str, String)]) -> Result<Value> {
        let web = self.web.as_ref().ok_or(Error::ProtocolChanged)?;
        let tid = web.transaction.generate("GET", path)?;
        let value = self.transport.json(path, query, &tid)?;
        model::check_errors(&value)?;
        Ok(value)
    }
    fn graphql(&self, op: &bootstrap::Operation, vars: Value) -> Result<Value> {
        if op.modern {
            let path = format!("/graphql/{}/{}", op.id, op.name);
            self.get_json(&path, &[("variables", vars.to_string())])
        } else {
            let path = format!("/i/api/graphql/{}/{}", op.id, op.name);
            self.get_json(
                &path,
                &[
                    ("variables", vars.to_string()),
                    ("features", op.features.to_string()),
                    ("fieldToggles", "{\"withAuxiliaryUserLabels\":false}".into()),
                ],
            )
        }
    }
}

pub(crate) fn user_name_variables(modern: bool, name: &str) -> Value {
    if modern {
        json!({"screenName":name,"withIdentity":true})
    } else {
        json!({"screen_name":name,"withSafetyModeUserFields":false})
    }
}
pub(crate) fn home_variables(
    modern: bool,
    recommended: bool,
    cursor: Option<&str>,
    count: u32,
) -> Value {
    let mut value = if modern {
        json!({"count":count,"seenTweetIds":[],"requestContext":if cursor.is_some(){"scroll"}else{"launch"}})
    } else {
        json!({"count":count,"includePromotedContent":false,"latestControlAvailable":true,"withCommunity":true,"seenTweetIds":[]})
    };
    if !recommended && modern {
        value["enableRanking"] = json!(false);
    }
    if recommended && !modern {
        value["requestContext"] = json!(if cursor.is_some() { "scroll" } else { "launch" });
    }
    if let Some(cursor) = cursor {
        value["cursor"] = json!(cursor);
    }
    value
}
