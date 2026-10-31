// SPDX-License-Identifier: MIT
//! Private, client-local window lifecycle. This is not the core control protocol
//! and carries neither platform credentials nor an arbitrary browser URL.
use crate::{Result, VERSION};
use serde::{Deserialize, Serialize};

pub const MAX_WINDOW_FRAME: usize = 8192;
pub const MAX_WINDOW_EVENT: usize = 4096;

#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct WindowRequest {
    pub version: u32,
    pub action: WindowAction,
    pub url: String,
    #[serde(default = "title")]
    pub title: String,
    #[serde(default = "lifetime")]
    pub timeout_secs: u64,
}
fn title() -> String {
    "Agent Runtime 登录".into()
}
fn lifetime() -> u64 {
    900
}
#[derive(Clone, Copy, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum WindowAction {
    Open,
}
impl WindowRequest {
    pub fn new(url: &str) -> Self {
        Self {
            version: VERSION,
            action: WindowAction::Open,
            url: url.into(),
            title: title(),
            timeout_secs: lifetime(),
        }
    }
    pub fn validate(&self) -> Result<()> {
        if self.version != VERSION
            || !(1..=3600).contains(&self.timeout_secs)
            || self.title.is_empty()
            || self.title.len() > 128
            || self.title.chars().any(char::is_control)
        {
            return Err("invalid client window request".into());
        }
        local_page(&self.url).map(|_| ())
    }
}

/// The helper accepts only our ephemeral human pages. Remote OAuth and general
/// platform navigation belong to the user's system browser, not this window.
pub fn local_page(url: &str) -> Result<&str> {
    if url.len() > 256 || url.chars().any(char::is_control) {
        return Err("invalid local login page".into());
    }
    let tail = url
        .strip_prefix("http://127.0.0.1:")
        .ok_or("window requires an IPv4 loopback login page")?;
    let (port, path) = tail.split_once('/').ok_or("login page path missing")?;
    if port.is_empty()
        || !port.bytes().all(|v| v.is_ascii_digit())
        || port.parse::<u16>().ok().is_none_or(|p| p == 0)
        || path.len() != 48
        || !path.bytes().all(|v| v.is_ascii_hexdigit())
    {
        return Err("invalid loopback login page address".into());
    }
    Ok(url)
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum WindowErrorCode {
    MissingWebkit,
    NoDisplay,
    InitFailed,
    NavigationFailed,
    InvalidInput,
    Expired,
}
#[derive(Debug, Serialize, Deserialize)]
#[serde(tag = "event", rename_all = "snake_case", deny_unknown_fields)]
pub enum WindowEvent {
    Ready { version: u32 },
    Closed { version: u32 },
    Error { version: u32, code: WindowErrorCode },
}
impl WindowEvent {
    pub fn ready() -> Self {
        Self::Ready { version: VERSION }
    }
    pub fn closed() -> Self {
        Self::Closed { version: VERSION }
    }
    pub fn error(code: WindowErrorCode) -> Self {
        Self::Error {
            version: VERSION,
            code,
        }
    }
    pub fn version(&self) -> u32 {
        match self {
            Self::Ready { version } | Self::Closed { version } | Self::Error { version, .. } => {
                *version
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn accepts_only_bounded_nonce_scoped_client_pages() {
        let good = format!("http://127.0.0.1:19799/{}", "a".repeat(48));
        WindowRequest::new(&good).validate().unwrap();
        for bad in [
            good.replace("127.0.0.1", "example.com"),
            good.replace("127.0.0.1", "127.0.0.1.evil"),
            good.replace("http:", "https:"),
            format!("{good}?token=secret"),
            format!("{good}#fragment"),
            good.replace(":19799/", ":0/"),
            "file:///etc/passwd".into(),
            "https://accounts.google.com/o/oauth2/auth".into(),
        ] {
            assert!(WindowRequest::new(&bad).validate().is_err());
        }
        let mut request = WindowRequest::new(&good);
        request.timeout_secs = 3601;
        assert!(request.validate().is_err());
    }
    #[test]
    fn window_events_cannot_carry_page_urls_credentials_or_native_error_text() {
        let event = serde_json::to_string(&WindowEvent::error(WindowErrorCode::NoDisplay)).unwrap();
        assert!(!event.contains("url") && !event.contains("message"));
        assert!(
            serde_json::from_str::<WindowEvent>(
                r#"{"version":1,"event":"ready","token":"private"}"#
            )
            .is_err()
        );
        assert!(
            serde_json::from_str::<WindowRequest>(
                r#"{"version":1,"action":"eval","url":"http://127.0.0.1:1/anything"}"#
            )
            .is_err()
        );
    }
}
