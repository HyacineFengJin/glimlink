// SPDX-License-Identifier: MIT
use crate::{Error, Result, Session};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::{
    cell::RefCell,
    fs::OpenOptions,
    io::{Read, Write},
    path::Path,
    time::Duration,
};

pub(crate) const USER_AGENT: &str = "Mozilla/5.0 (X11; Linux x86_64) AppleWebKit/537.36 (KHTML, like Gecko) Chrome/131.0.0.0 Safari/537.36";
// Public X web application bearer, shared by all users; not an account/API key.
const WEB_BEARER: &str = "AAAAAAAAAAAAAAAAAAAAANRILgAAAAAAnNwIzUejRCOuH5E6I8xnZz4puTs%3D1Zv7ttfk8LF81IUq16cHjhLTvJu4FA33AGWWjCpTnA";

#[derive(Clone, Debug)]
pub struct Config {
    pub request_timeout: Duration,
    pub connect_timeout: Duration,
    pub max_response_bytes: usize,
    pub language: String,
}
impl Default for Config {
    fn default() -> Self {
        Self {
            request_timeout: Duration::from_secs(25),
            connect_timeout: Duration::from_secs(8),
            max_response_bytes: 8 * 1024 * 1024,
            language: "en-US".into(),
        }
    }
}
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct DownloadInfo {
    pub mime_type: Option<String>,
    pub bytes: u64,
}

#[derive(Clone, Copy, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum RequestStage {
    Page,
    Script,
    AccountSettings,
    UserLookup,
    Timeline,
    Post,
    OtherApi,
    Media,
}
#[derive(Clone, Copy, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum ResponseKind {
    Json,
    Html,
    Other,
}
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub struct RequestDiagnostic {
    pub stage: RequestStage,
    pub http_status: Option<u16>,
    pub response_kind: Option<ResponseKind>,
    pub provider_codes: Vec<u16>,
}
impl RequestDiagnostic {
    fn new(stage: RequestStage) -> Self {
        Self {
            stage,
            http_status: None,
            response_kind: None,
            provider_codes: Vec::new(),
        }
    }
    fn body(&mut self, body: &str, json: Option<&Value>) {
        // Fixed enums and bounded numeric error codes are the entire public
        // diagnostic. Do not retain message text, keys, URLs or account IDs.
        self.provider_codes.clear();
        if let Some(value) = json {
            self.response_kind = Some(ResponseKind::Json);
            for error in value["errors"].as_array().into_iter().flatten().take(8) {
                if let Some(code) = error["code"].as_u64().filter(|n| *n <= 9999) {
                    self.provider_codes.push(code as u16);
                }
            }
        } else {
            self.response_kind = Some(if body.trim_start().starts_with('<') {
                ResponseKind::Html
            } else {
                ResponseKind::Other
            });
        }
    }
}
fn api_stage(path: &str) -> RequestStage {
    match path.rsplit('/').next().unwrap_or("") {
        "settings.json" => RequestStage::AccountSettings,
        "UserByScreenName" | "UserByRestId" | "userIdRedirectQuery" => RequestStage::UserLookup,
        "UserTweets" | "UserOriginalsTimeline" => RequestStage::Timeline,
        "TweetDetail" | "TweetResultByRestId" => RequestStage::Post,
        _ => RequestStage::OtherApi,
    }
}

pub(crate) struct Transport {
    pub(crate) config: Config,
    agent: ureq::Agent,
    session: Session,
    diagnostic: RefCell<Option<RequestDiagnostic>>,
}
impl Transport {
    pub(crate) fn new(config: Config, session: Session) -> Result<Self> {
        if config.request_timeout.is_zero()
            || config.request_timeout > Duration::from_secs(120)
            || config.connect_timeout.is_zero()
            || config.connect_timeout > config.request_timeout
            || !(1024..=16 * 1024 * 1024).contains(&config.max_response_bytes)
            || config.language.len() > 32
            || !config
                .language
                .bytes()
                .all(|b| b.is_ascii_alphanumeric() || b == b'-')
        {
            return Err(Error::InvalidConfig);
        }
        let agent = ureq::Agent::config_builder()
            .https_only(true)
            .max_redirects(0)
            .http_status_as_error(false)
            .proxy(None)
            .timeout_global(Some(config.request_timeout))
            .timeout_connect(Some(config.connect_timeout))
            .timeout_per_read(Some(Duration::from_secs(15)))
            .max_response_header_size(32 * 1024)
            .max_idle_connections(3)
            .max_idle_connections_per_host(2)
            .user_agent(USER_AGENT)
            .build()
            .into();
        Ok(Self {
            config,
            agent,
            session,
            diagnostic: RefCell::new(None),
        })
    }
    pub(crate) fn last_request(&self) -> Option<RequestDiagnostic> {
        self.diagnostic.borrow().clone()
    }
    pub(crate) fn clear_diagnostic(&self) {
        *self.diagnostic.borrow_mut() = None;
    }
    fn begin(&self, stage: RequestStage) {
        *self.diagnostic.borrow_mut() = Some(RequestDiagnostic::new(stage));
    }
    fn response(&self, code: u16) {
        if let Some(value) = self.diagnostic.borrow_mut().as_mut() {
            value.http_status = Some(code);
        }
    }
    pub(crate) fn page(&self) -> Result<String> {
        self.begin(RequestStage::Page);
        let mut url = "https://x.com/";
        for _ in 0..2 {
            let mut r = self
                .agent
                .get(url)
                .header("Cookie", self.session.cookie_header())
                .header("Accept-Language", &self.config.language)
                .header("Accept", "text/html")
                .call()
                .map_err(|_| Error::Network)?;
            self.response(r.status().as_u16());
            if r.status().is_redirection() {
                let location = r
                    .headers()
                    .get("location")
                    .and_then(|v| v.to_str().ok())
                    .ok_or(Error::RedirectDenied)?;
                if matches!(location, "/home" | "https://x.com/home") && url != "https://x.com/home"
                {
                    url = "https://x.com/home";
                    continue;
                }
                if location.starts_with("/i/flow/login")
                    || location.starts_with("https://x.com/i/flow/login")
                {
                    return Err(Error::LoginRequired);
                }
                if location.starts_with("/account/access")
                    || location.starts_with("https://x.com/account/access")
                {
                    return Err(Error::VerificationRequired);
                }
                return Err(Error::RedirectDenied);
            }
            status(&r, true)?;
            return string_body(r.body_mut().as_reader(), self.config.max_response_bytes);
        }
        Err(Error::RedirectDenied)
    }
    pub(crate) fn script(&self, url: &str) -> Result<String> {
        if !script_url(url) {
            return Err(Error::ProtocolChanged);
        }
        self.begin(RequestStage::Script);
        let mut r = self
            .agent
            .get(url)
            .header("Referer", "https://x.com/")
            .header("Accept", "*/*")
            .call()
            .map_err(|_| Error::Network)?;
        self.response(r.status().as_u16());
        status(&r, false)?;
        string_body(r.body_mut().as_reader(), self.config.max_response_bytes)
    }
    pub(crate) fn json(
        &self,
        path: &str,
        query: &[(&str, String)],
        transaction: &str,
    ) -> Result<Value> {
        if !(path == "/i/api/1.1/account/settings.json"
            || path.starts_with("/i/api/graphql/")
            || path.starts_with("/graphql/"))
            || path.contains(['?', '#', '\\'])
            || path.bytes().any(|b| b.is_ascii_control())
        {
            return Err(Error::InvalidInput);
        }
        let host = if path.starts_with("/graphql/") {
            "api.x.com"
        } else {
            "x.com"
        };
        self.begin(api_stage(path));
        let mut request = self
            .agent
            .get(format!("https://{host}{path}"))
            .header("Cookie", self.session.cookie_header())
            .header("Authorization", format!("Bearer {WEB_BEARER}"))
            .header("x-csrf-token", &self.session.ct0)
            .header("x-twitter-auth-type", "OAuth2Session")
            .header("x-twitter-active-user", "yes")
            .header("x-twitter-client-language", "en")
            .header("x-client-transaction-id", transaction)
            .header("Referer", "https://x.com/")
            .header("Accept-Language", &self.config.language)
            .header("Accept", "application/json");
        for (k, v) in query {
            request = request.query(k, v);
        }
        let mut r = request.call().map_err(|_| Error::Network)?;
        self.response(r.status().as_u16());
        let response_status = status(&r, true);
        let body = match string_body(r.body_mut().as_reader(), self.config.max_response_bytes) {
            Ok(b) => b,
            Err(e) => {
                response_status?;
                return Err(e);
            }
        };
        let parsed = serde_json::from_str::<Value>(&body);
        if let Some(value) = self.diagnostic.borrow_mut().as_mut() {
            value.body(&body, parsed.as_ref().ok());
        }
        if let Ok(v) = &parsed {
            #[cfg(test)]
            if std::env::var("PRIVATE_X_CANDIDATE").ok().as_deref() == Some("1")
                && v.get("errors").is_some()
            {
                let feature=regex::Regex::new(r"\b(?:responsive_web|rweb|graphql|creator_subscriptions|hidden_profile|subscriptions_verification|profile_label|premium_content|communities_web|c9s|tweet|longform|view_counts|verified_phone|standardized_nudges|freedom_of_speech)[a-z0-9_]{1,140}\b").map_err(|_|Error::ProtocolChanged)?;
                let variable = regex::Regex::new(r"\$[A-Za-z_][A-Za-z0-9_]{0,80}")
                    .map_err(|_| Error::ProtocolChanged)?;
                for error in v["errors"].as_array().into_iter().flatten().take(8) {
                    let keys: Vec<_> = error
                        .as_object()
                        .into_iter()
                        .flat_map(|o| o.keys())
                        .filter(|k| {
                            k.len() <= 64
                                && k.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'_')
                        })
                        .cloned()
                        .collect();
                    let message = error["message"].as_str().unwrap_or("");
                    let features: Vec<_> = feature
                        .find_iter(message)
                        .take(32)
                        .map(|m| m.as_str())
                        .collect();
                    let variables: Vec<_> = variable
                        .find_iter(message)
                        .take(16)
                        .map(|m| m.as_str())
                        .collect();
                    eprintln!(
                        "PRIVATE_X stage=json_error_schema http_status={} keys={} feature_names={} variable_names={} feature_related={} variable_related={}",
                        r.status().as_u16(),
                        serde_json::to_string(&keys).map_err(|_| Error::ProtocolChanged)?,
                        serde_json::to_string(&features).map_err(|_| Error::ProtocolChanged)?,
                        serde_json::to_string(&variables).map_err(|_| Error::ProtocolChanged)?,
                        message.to_ascii_lowercase().contains("feature"),
                        message.to_ascii_lowercase().contains("variable")
                    );
                }
            }
            crate::model::check_errors(v)?;
        }
        response_status?;
        if body.trim_start().starts_with('<') {
            // An HTML response from a JSON endpoint does not establish that
            // the account itself requires verification.
            return Err(Error::RequestDenied);
        }
        parsed.map_err(|_| Error::ProtocolChanged)
    }
    pub(crate) fn download_media(
        &self,
        url: &str,
        destination: &Path,
        max_bytes: u64,
    ) -> Result<DownloadInfo> {
        if !crate::model::media_url(url) || max_bytes == 0 || max_bytes > 256 * 1024 * 1024 {
            return Err(Error::InvalidInput);
        }
        self.begin(RequestStage::Media);
        let mut r = self
            .agent
            .get(url)
            .header("Accept", "*/*")
            .call()
            .map_err(|_| Error::Network)?;
        self.response(r.status().as_u16());
        status(&r, false)?;
        if r.headers()
            .get("content-length")
            .and_then(|h| h.to_str().ok())
            .and_then(|h| h.parse::<u64>().ok())
            .is_some_and(|n| n > max_bytes)
        {
            return Err(Error::Limit);
        }
        let mime_type = r
            .headers()
            .get("content-type")
            .and_then(|v| v.to_str().ok())
            .map(|s| s.split(';').next().unwrap_or(s).trim().to_owned())
            .filter(|s| s.len() < 128);
        let mut opts = OpenOptions::new();
        opts.create_new(true).write(true);
        #[cfg(unix)]
        {
            use std::os::unix::fs::OpenOptionsExt;
            opts.mode(0o600);
        }
        let mut file = opts.open(destination).map_err(|_| Error::Io)?;
        let result = (|| {
            let mut reader = r.body_mut().as_reader();
            let mut buffer = [0u8; 16 * 1024];
            let mut total = 0u64;
            loop {
                let n = reader.read(&mut buffer).map_err(|_| Error::Network)?;
                if n == 0 {
                    break;
                }
                total = total.checked_add(n as u64).ok_or(Error::Limit)?;
                if total > max_bytes {
                    return Err(Error::Limit);
                }
                file.write_all(&buffer[..n]).map_err(|_| Error::Io)?;
            }
            file.flush().map_err(|_| Error::Io)?;
            Ok(DownloadInfo {
                mime_type,
                bytes: total,
            })
        })();
        drop(file);
        if result.is_err() {
            let _ = std::fs::remove_file(destination);
        }
        result
    }
}
fn status(r: &ureq::http::Response<ureq::Body>, authenticated: bool) -> Result<()> {
    status_code(
        r.status().as_u16(),
        authenticated,
        r.headers()
            .get("x-rate-limit-reset")
            .and_then(|v| v.to_str().ok())
            .and_then(|v| v.parse().ok()),
    )
}
fn status_code(code: u16, authenticated: bool, reset_at: Option<u64>) -> Result<()> {
    match code {
        200 => Ok(()),
        301..=399 => Err(Error::RedirectDenied),
        401 if authenticated => Err(Error::LoginRequired),
        // A bare HTTP 403 is not proof of an account verification challenge.
        // JSON provider codes and explicit account/access redirects are typed
        // separately; retain this distinction for the caller.
        403 => Err(Error::RequestDenied),
        404 => Err(Error::NotFound),
        429 => Err(Error::RateLimited { reset_at }),
        400 => Err(Error::ProtocolChanged),
        _ => Err(Error::Network),
    }
}

#[cfg(test)]
mod diagnostic_tests {
    use super::*;
    #[test]
    fn bare_403_does_not_assert_a_verification_challenge() {
        assert_eq!(status_code(403, true, None), Err(Error::RequestDenied));
        assert_eq!(
            crate::model::check_errors(&serde_json::json!({"errors":[{"code":326}]})),
            Err(Error::VerificationRequired)
        );
        assert_eq!(
            status_code(429, true, Some(123)),
            Err(Error::RateLimited {
                reset_at: Some(123)
            })
        );
    }
    #[test]
    fn diagnostic_retains_only_fixed_metadata_and_clears_previous_codes() {
        let mut diagnostic = RequestDiagnostic::new(api_stage("/i/api/graphql/opaque/UserTweets"));
        diagnostic.http_status = Some(403);
        let body = r#"{"errors":[{"code":326,"message":"secret-cookie-private-url"},{"code":1234567890123456789}]}"#;
        let parsed: Value = serde_json::from_str(body).unwrap();
        diagnostic.body(body, Some(&parsed));
        let text = serde_json::to_string(&diagnostic).unwrap();
        assert!(
            !text.contains("secret")
                && !text.contains("opaque")
                && !text.contains("1234567890123456789")
        );
        assert_eq!(diagnostic.stage, RequestStage::Timeline);
        assert_eq!(diagnostic.provider_codes, vec![326]);
        diagnostic.body("<html>secret</html>", None);
        assert!(diagnostic.provider_codes.is_empty());
        assert_eq!(diagnostic.response_kind, Some(ResponseKind::Html));
    }
}
fn string_body(reader: impl Read, max: usize) -> Result<String> {
    let mut bytes = Vec::new();
    reader
        .take(max as u64 + 1)
        .read_to_end(&mut bytes)
        .map_err(|_| Error::Network)?;
    if bytes.len() > max {
        return Err(Error::Limit);
    }
    String::from_utf8(bytes).map_err(|_| Error::ProtocolChanged)
}
pub(crate) fn script_url(url: &str) -> bool {
    let Ok(u) = url.parse::<ureq::http::Uri>() else {
        return false;
    };
    u.scheme_str() == Some("https")
        && u.host() == Some("abs.twimg.com")
        && u.port_u16().is_none()
        && !url.contains(['@', '\\', '?', '#'])
        && url.len() < 512
        && u.path().ends_with(".js")
        && (u.path().starts_with("/responsive-web/client-web/")
            || u.path().starts_with("/x-web/x-web/"))
        && !u.path().split('/').any(|s| s == "..")
}
