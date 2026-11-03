// SPDX-License-Identifier: MIT
//! Small read-only Reddit/Discord website transports. The operator supplies a
//! session, not a password. No browser, scripts, OAuth-app impersonation, joins,
//! acknowledgements, messages, votes or other write requests exist here.
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use std::{
    fmt,
    io::{Read, Write},
    time::Duration,
};

pub type Result<T> = std::result::Result<T, Error>;
#[derive(Clone, Copy, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum Site {
    Reddit,
    Discord,
}
impl Site {
    pub fn name(self) -> &'static str {
        match self {
            Self::Reddit => "reddit",
            Self::Discord => "discord",
        }
    }
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Error {
    InvalidInput,
    LoginRequired,
    PermissionDenied,
    VerificationRequired,
    RateLimited,
    NotFound,
    ProtocolChanged,
    Network,
    Limit,
}
impl Error {
    pub fn code(self) -> &'static str {
        match self {
            Self::InvalidInput => "invalid_input",
            Self::LoginRequired => "needs_auth",
            Self::PermissionDenied => "permission_denied",
            Self::VerificationRequired => "verification_required",
            Self::RateLimited => "rate_limited",
            Self::NotFound => "not_found",
            Self::ProtocolChanged => "protocol_changed",
            Self::Network => "network_error",
            Self::Limit => "limit_exceeded",
        }
    }
}
impl fmt::Display for Error {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "Website adapter: {}", self.code())
    }
}
impl std::error::Error for Error {}
#[derive(Clone, Serialize, Deserialize)]
#[serde(try_from = "RawSession")]
pub struct Session {
    site: Site,
    credential: String,
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct RawSession {
    site: Site,
    credential: String,
}
impl TryFrom<RawSession> for Session {
    type Error = Error;
    fn try_from(v: RawSession) -> Result<Self> {
        Self::new(v.site, &v.credential)
    }
}
impl fmt::Debug for Session {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("Session([redacted])")
    }
}
fn credential(value: &str) -> bool {
    !value.is_empty() && value.len() <= 16384 && !value.bytes().any(|b| b.is_ascii_control())
}
impl Session {
    pub fn site(&self) -> Site {
        self.site
    }
    fn new(site: Site, value: &str) -> Result<Self> {
        if !credential(value) {
            return Err(Error::InvalidInput);
        }
        if site == Site::Discord {
            if value.len() > 4096
                || value.starts_with("Bot ")
                || value.starts_with("Bearer ")
                || !value
                    .bytes()
                    .all(|b| b.is_ascii_graphic() && !b";\"\\".contains(&b))
            {
                return Err(Error::InvalidInput);
            }
            return Ok(Self {
                site,
                credential: value.into(),
            });
        }
        let mut kept = std::collections::BTreeMap::new();
        for field in value.split(';').take(128) {
            let Some((name, value)) = field.trim().split_once('=') else {
                continue;
            };
            if !["reddit_session", "token_v2", "csrf_token"].contains(&name) {
                continue;
            }
            if value.is_empty()
                || value.len() > 8192
                || !value
                    .bytes()
                    .all(|b| b.is_ascii_graphic() && !b";\"\\".contains(&b))
                || kept.insert(name, value).is_some()
            {
                return Err(Error::InvalidInput);
            }
        }
        if !kept.contains_key("reddit_session") && !kept.contains_key("token_v2") {
            return Err(Error::InvalidInput);
        }
        Ok(Self {
            site,
            credential: kept
                .into_iter()
                .map(|(n, v)| format!("{n}={v}"))
                .collect::<Vec<_>>()
                .join("; "),
        })
    }
    pub fn from_private_input(site: Site, input: &str) -> Result<Self> {
        if site == Site::Discord {
            let value: Value = serde_json::from_str(input).map_err(|_| Error::InvalidInput)?;
            if value.as_object().is_none_or(|v| v.len() != 1) {
                return Err(Error::InvalidInput);
            }
            return Self::new(site, value["token"].as_str().ok_or(Error::InvalidInput)?);
        }
        Self::new(site, input)
    }
}
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct User {
    pub id: String,
    pub name: String,
}
impl User {
    pub fn validate(&self, site: Site) -> Result<()> {
        id(site, &self.id)?;
        if self.name.is_empty() || self.name.len() > 512 || self.name.chars().any(char::is_control)
        {
            return Err(Error::ProtocolChanged);
        }
        Ok(())
    }
}
pub fn id(site: Site, value: &str) -> Result<()> {
    let valid = match site {
        Site::Discord => !value.starts_with('0') && value.bytes().all(|b| b.is_ascii_digit()),
        Site::Reddit => value
            .bytes()
            .all(|b| b.is_ascii_digit() || (b'a'..=b'z').contains(&b)),
    };
    if value.is_empty() || value.len() > 24 || !valid {
        Err(Error::InvalidInput)
    } else {
        Ok(())
    }
}
fn limit(value: u64) -> Result<u64> {
    if (1..=20).contains(&value) {
        Ok(value)
    } else {
        Err(Error::InvalidInput)
    }
}
pub struct Client {
    session: Session,
    http: ureq::Agent,
}
impl Client {
    pub fn new(session: Session) -> Self {
        let config = ureq::Agent::config_builder()
            .timeout_global(Some(Duration::from_secs(30)))
            .max_redirects(0)
            .http_status_as_error(false)
            .proxy(None)
            .max_idle_connections(0)
            .build();
        Self {
            session,
            http: config.into(),
        }
    }
    fn get(&self, path: &str) -> Result<Value> {
        let base = match self.session.site {
            Site::Reddit => "https://www.reddit.com",
            Site::Discord => "https://discord.com/api/v10",
        };
        let mut request = self
            .http
            .get(format!("{base}{path}"))
            .header("Accept", "application/json")
            .header("User-Agent", "Glimlink/0.1 (personal website session)");
        request = match self.session.site {
            Site::Reddit => request.header("Cookie", &self.session.credential),
            Site::Discord => request.header("Authorization", &self.session.credential),
        };
        let mut response = request.call().map_err(|_| Error::Network)?;
        status(response.status().as_u16())?;
        let mut bytes = Vec::new();
        response
            .body_mut()
            .as_reader()
            .take(2 * 1024 * 1024 + 1)
            .read_to_end(&mut bytes)
            .map_err(|_| Error::Network)?;
        if bytes.len() > 2 * 1024 * 1024 {
            return Err(Error::Limit);
        }
        serde_json::from_slice(&bytes).map_err(|_| Error::ProtocolChanged)
    }
    pub fn me(&self) -> Result<User> {
        let raw = self.get(match self.session.site {
            Site::Reddit => "/api/me.json?raw_json=1",
            Site::Discord => "/users/@me",
        })?;
        identity(self.session.site, &raw)
    }
    pub fn reddit_home(&self, after: Option<&str>, count: u64) -> Result<Value> {
        if self.session.site != Site::Reddit {
            return Err(Error::InvalidInput);
        }
        let after = match after {
            Some(v) => {
                fullname(v)?;
                format!("&after={v}")
            }
            None => String::new(),
        };
        self.get(&format!(
            "/best.json?raw_json=1&limit={}{}",
            limit(count)?,
            after
        ))
    }
    pub fn reddit_thread(&self, post: &str, count: u64) -> Result<Value> {
        if self.session.site != Site::Reddit {
            return Err(Error::InvalidInput);
        }
        id(Site::Reddit, post)?;
        self.get(&format!(
            "/comments/{post}.json?raw_json=1&limit={}&depth=3",
            limit(count)?
        ))
    }
    pub fn discord_chats(&self) -> Result<Value> {
        if self.session.site != Site::Discord {
            return Err(Error::InvalidInput);
        }
        self.get("/users/@me/channels")
    }
    pub fn discord_guilds(&self) -> Result<Value> {
        if self.session.site != Site::Discord {
            return Err(Error::InvalidInput);
        }
        self.get("/users/@me/guilds?limit=100")
    }
    pub fn discord_channels(&self, guild: &str) -> Result<Value> {
        if self.session.site != Site::Discord {
            return Err(Error::InvalidInput);
        }
        id(Site::Discord, guild)?;
        self.get(&format!("/guilds/{guild}/channels"))
    }
    pub fn discord_messages(
        &self,
        channel: &str,
        before: Option<&str>,
        count: u64,
    ) -> Result<Value> {
        if self.session.site != Site::Discord {
            return Err(Error::InvalidInput);
        }
        id(Site::Discord, channel)?;
        let before = match before {
            Some(v) => {
                id(Site::Discord, v)?;
                format!("&before={v}")
            }
            None => String::new(),
        };
        self.get(&format!(
            "/channels/{channel}/messages?limit={}{}",
            limit(count)?,
            before
        ))
    }
    pub fn download(&self, url: &str, max: u64, output: &mut impl Write) -> Result<String> {
        if !media_url(self.session.site, url) || !(1..=64 * 1024 * 1024).contains(&max) {
            return Err(Error::InvalidInput);
        }
        // Signed media links are capabilities; account credentials never go to a CDN.
        let mut response = self.http.get(url).call().map_err(|_| Error::Network)?;
        status(response.status().as_u16())?;
        let mime = response
            .headers()
            .get("content-type")
            .and_then(|s| s.to_str().ok())
            .unwrap_or("")
            .split(';')
            .next()
            .unwrap_or("")
            .to_owned();
        if !mime.starts_with("image/") && !mime.starts_with("video/") && !mime.starts_with("audio/")
        {
            return Err(Error::ProtocolChanged);
        }
        let mut read = response.body_mut().as_reader();
        let mut total = 0;
        let mut buffer = [0; 32768];
        loop {
            let n = read.read(&mut buffer).map_err(|_| Error::Network)?;
            if n == 0 {
                break;
            }
            total += n as u64;
            if total > max {
                return Err(Error::Limit);
            }
            output.write_all(&buffer[..n]).map_err(|_| Error::Network)?;
        }
        if total == 0 {
            return Err(Error::ProtocolChanged);
        }
        Ok(mime)
    }
}
fn status(value: u16) -> Result<()> {
    match value {
        200 => Ok(()),
        401 => Err(Error::LoginRequired),
        403 => Err(Error::PermissionDenied),
        301 | 302 | 303 | 307 | 308 => Err(Error::VerificationRequired),
        404 => Err(Error::NotFound),
        429 => Err(Error::RateLimited),
        _ => Err(Error::Network),
    }
}
fn identity(site: Site, raw: &Value) -> Result<User> {
    let raw = if site == Site::Reddit {
        raw.get("data").unwrap_or(raw)
    } else {
        raw
    };
    if site == Site::Discord && raw["bot"] == true {
        return Err(Error::InvalidInput);
    }
    let id = raw["id"].as_str().ok_or(Error::LoginRequired)?;
    let name = if site == Site::Reddit {
        raw["name"].as_str()
    } else {
        raw["global_name"]
            .as_str()
            .or_else(|| raw["username"].as_str())
    }
    .ok_or(Error::LoginRequired)?;
    let user = User {
        id: id.into(),
        name: name.into(),
    };
    user.validate(site)?;
    Ok(user)
}
pub fn fullname(value: &str) -> Result<()> {
    let (kind, idvalue) = value.split_once('_').ok_or(Error::InvalidInput)?;
    if !matches!(kind, "t1" | "t3") {
        return Err(Error::InvalidInput);
    }
    id(Site::Reddit, idvalue)
}
pub fn media_url(site: Site, url: &str) -> bool {
    if url.len() > 4096 || url.chars().any(char::is_control) || url.contains('\\') {
        return false;
    }
    let Ok(uri) = url.parse::<ureq::http::Uri>() else {
        return false;
    };
    if uri.scheme_str() != Some("https")
        || uri.port_u16().is_some()
        || uri.authority().is_some_and(|a| a.as_str().contains('@'))
    {
        return false;
    }
    let host = uri.host().unwrap_or("");
    match site {
        Site::Reddit => matches!(
            host,
            "i.redd.it" | "preview.redd.it" | "external-preview.redd.it"
        ),
        Site::Discord => {
            matches!(host, "cdn.discordapp.com" | "media.discordapp.net")
                && uri.path().starts_with("/attachments/")
        }
    }
}
pub fn text(value: &Value, max: usize) -> Value {
    value
        .as_str()
        .map(|s| Value::String(s.chars().take(max).collect()))
        .unwrap_or(Value::Null)
}
pub fn reddit_post(raw: &Value) -> Result<Value> {
    let post = raw["id"].as_str().ok_or(Error::ProtocolChanged)?;
    id(Site::Reddit, post)?;
    Ok(
        json!({"id":post,"title":text(&raw["title"],512),"text":text(&raw["selftext"],8192),"text_truncated":raw["selftext"].as_str().is_some_and(|s|s.chars().count()>8192),"author":text(&raw["author"],128),"community":text(&raw["subreddit"],128),"counts":{"score":raw.get("score"),"comments":raw.get("num_comments"),"upvote_ratio":raw.get("upvote_ratio")},"created_at":raw.get("created_utc"),"spoiler":raw.get("spoiler"),"nsfw":raw.get("over_18")}),
    )
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn anonymous_sessions_and_public_profiles_are_not_login_proof() {
        assert_eq!(
            identity(Site::Reddit, &json!({"data":{}})).unwrap_err(),
            Error::LoginRequired
        );
        assert_eq!(
            identity(
                Site::Reddit,
                &json!({"kind":"t2","data":{"id":"ab123","name":"alice","modhash":"private"}})
            )
            .unwrap()
            .name,
            "alice"
        );
        assert_eq!(
            identity(
                Site::Discord,
                &json!({"id":"123","username":"alice","email":"private"})
            )
            .unwrap()
            .id,
            "123"
        );
        assert!(
            identity(
                Site::Discord,
                &json!({"id":"123","username":"bot","bot":true})
            )
            .is_err()
        );
    }
    #[test]
    fn credentials_are_typed_and_retained_only_for_the_selected_site() {
        let session = Session::from_private_input(
            Site::Reddit,
            "reddit_session=private; unrelated=discarded",
        )
        .unwrap();
        assert!(!format!("{session:?}").contains("private"));
        assert!(
            Session::from_private_input(Site::Reddit, "reddit_session=one; reddit_session=two")
                .is_err()
        );
        assert!(Session::from_private_input(Site::Discord, r#"{"token":"Bot abc"}"#).is_err());
        assert!(
            Session::from_private_input(
                Site::Discord,
                r#"{"token":"synthetic","password":"forbidden"}"#
            )
            .is_err()
        );
        assert!(!media_url(
            Site::Discord,
            "https://discord.com/api/v10/users/@me"
        ));
        assert!(!media_url(
            Site::Discord,
            "https://cdn.discordapp.com.evil/attachments/x"
        ));
        assert!(media_url(
            Site::Discord,
            "https://cdn.discordapp.com/attachments/1/2/image.png?ex=1&hm=2"
        ));
        for bad in ["t3_../x", "t3_ab?other", "t2_abc"] {
            assert!(fullname(bad).is_err());
        }
    }
}
