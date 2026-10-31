// SPDX-License-Identifier: MIT
use crate::{Error, MediaResource, Result};
use serde::{Deserialize, Serialize};
use std::{
    fmt,
    io::{Read, Write},
    time::Duration,
};
use ureq::{
    Agent, Body,
    http::{Response, Uri},
};

/// Cookie is supplied explicitly by the embedding runtime. This crate never
/// searches environment variables, home directories, browser stores or accounts.
#[derive(Clone)]
pub struct Config {
    pub cookie: Option<String>,
    pub max_json_bytes: usize,
    pub max_download_bytes: u64,
    pub request_timeout: Duration,
    pub connect_timeout: Duration,
    pub download_timeout: Duration,
}
impl Default for Config {
    fn default() -> Self {
        Self {
            cookie: None,
            max_json_bytes: 2 * 1024 * 1024,
            max_download_bytes: 64 * 1024 * 1024,
            request_timeout: Duration::from_secs(20),
            connect_timeout: Duration::from_secs(8),
            download_timeout: Duration::from_secs(120),
        }
    }
}
impl fmt::Debug for Config {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Config")
            .field("cookie", &self.cookie.as_ref().map(|_| "[redacted]"))
            .field("max_json_bytes", &self.max_json_bytes)
            .field("max_download_bytes", &self.max_download_bytes)
            .finish_non_exhaustive()
    }
}
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct DownloadReceipt {
    pub bytes: u64,
    pub content_type: Option<String>,
}

#[derive(Clone, Copy)]
pub(crate) enum UrlRole {
    Api,
    Resource,
    Video,
}
fn host_under(host: &str, root: &str) -> bool {
    host == root
        || host
            .strip_suffix(root)
            .is_some_and(|prefix| prefix.ends_with('.'))
}
pub(crate) fn parse_url(value: &str, role: UrlRole) -> Result<Uri> {
    if value.len() > 16384
        || value.contains(['\\', '#'])
        || value.bytes().any(|b| b <= 32 || b == 127)
    {
        return Err(Error::UrlPolicy);
    }
    let uri: Uri = value.parse().map_err(|_| Error::UrlPolicy)?;
    let host = uri.host().ok_or(Error::UrlPolicy)?.to_ascii_lowercase();
    if uri.scheme_str() != Some("https")
        || !matches!(uri.port_u16(), None | Some(443))
        || uri.authority().is_none_or(|a| a.as_str().contains('@'))
        || host.ends_with('.')
    {
        return Err(Error::UrlPolicy);
    }
    let allowed = match role {
        UrlRole::Api => host == "api.bilibili.com",
        UrlRole::Video => matches!(
            host.as_str(),
            "bilibili.com" | "www.bilibili.com" | "m.bilibili.com"
        ),
        UrlRole::Resource => ["bilivideo.com", "bilivideo.cn", "hdslb.com"]
            .iter()
            .any(|root| host_under(&host, root)),
    };
    if !allowed {
        return Err(Error::UrlPolicy);
    }
    Ok(uri)
}
pub fn validate_resource_url(value: &str) -> Result<()> {
    parse_url(value, UrlRole::Resource).map(|_| ())
}
pub(crate) fn normalize_resource_url(value: &str) -> Result<String> {
    let value = if value.starts_with("//") {
        format!("https:{value}")
    } else {
        value.to_owned()
    };
    validate_resource_url(&value)?;
    Ok(value)
}
fn redirect_url(current: &str, target: &str, role: UrlRole) -> Result<String> {
    let value = if target.starts_with("https://") {
        target.to_owned()
    } else if target.starts_with("//") {
        format!("https:{target}")
    } else if target.starts_with('/') {
        let previous = parse_url(current, role)?;
        format!(
            "https://{}{target}",
            previous.authority().ok_or(Error::UrlPolicy)?
        )
    } else {
        return Err(Error::UrlPolicy);
    };
    parse_url(&value, role)?;
    Ok(value)
}
pub(crate) struct Transport {
    api: Agent,
    media: Agent,
    config: Config,
}

pub(crate) enum EngagementPost {
    LikeVideo,
    FollowUploader,
}
impl Transport {
    pub(crate) fn cookie_header(&self) -> Option<&str> {
        self.config.cookie.as_deref()
    }

    /// Exactly one fixed-path write request, with a fresh non-pooling agent.
    /// The caller must durably reserve its operation before invoking this method.
    pub(crate) fn post_root_comment(&self, body: &[u8], bvid: &str) -> Result<serde_json::Value> {
        self.post_comment_payload(
            "/x/v2/reply/add",
            body,
            bvid,
            "application/x-www-form-urlencoded; charset=UTF-8",
            32768,
        )
    }

    pub(crate) fn post_comment_image(
        &self,
        body: &[u8],
        bvid: &str,
        content_type: &str,
    ) -> Result<serde_json::Value> {
        self.post_comment_payload(
            "/x/dynamic/feed/draw/upload_bfs",
            body,
            bvid,
            content_type,
            2 * 1024 * 1024 + 4096,
        )
    }

    /// Only positive like/follow endpoints are reachable through this enum.
    pub(crate) fn post_engagement(
        &self,
        action: EngagementPost,
        body: &[u8],
        bvid: &str,
    ) -> Result<serde_json::Value> {
        let path = match action {
            EngagementPost::LikeVideo => "/x/web-interface/archive/like",
            EngagementPost::FollowUploader => "/x/relation/modify",
        };
        self.post_comment_payload(
            path,
            body,
            bvid,
            "application/x-www-form-urlencoded; charset=UTF-8",
            4096,
        )
    }

    fn post_comment_payload(
        &self,
        path: &str,
        body: &[u8],
        bvid: &str,
        content_type: &str,
        limit: usize,
    ) -> Result<serde_json::Value> {
        let video = bvid.parse::<crate::VideoId>()?;
        if !matches!(video, crate::VideoId::Bv(_)) || body.is_empty() || body.len() > limit {
            return Err(Error::InvalidComment);
        }
        let cookie = self.config.cookie.as_deref().ok_or(Error::LoginRequired)?;
        let agent: Agent = Agent::config_builder()
            .https_only(true)
            .max_redirects(0)
            .http_status_as_error(false)
            .proxy(None)
            .timeout_global(Some(self.config.request_timeout))
            .timeout_connect(Some(self.config.connect_timeout))
            .timeout_per_read(Some(Duration::from_secs(15)))
            .max_response_header_size(32 * 1024)
            .max_idle_connections(0)
            .user_agent(
                "Mozilla/5.0 (X11; Linux x86_64) AppleWebKit/537.36 Chrome/131.0.0.0 Safari/537.36",
            )
            .build()
            .into();
        let mut response = agent
            .post(&format!("https://api.bilibili.com{path}"))
            .header("Content-Type", content_type)
            .header("Origin", "https://www.bilibili.com")
            .header(
                "Referer",
                &format!("https://www.bilibili.com/video/{bvid}/"),
            )
            .header("Cookie", cookie)
            .send(body)
            .map_err(|_| Error::WriteOutcomeUnknown("network"))?;
        // A redirect or malformed response is not proof the write failed.
        if response.status().as_u16() != 200 {
            return Err(Error::WriteOutcomeUnknown("http_status"));
        }
        let bytes = read_limited(response.body_mut().as_reader(), self.config.max_json_bytes)
            .map_err(|_| Error::WriteOutcomeUnknown("response_unavailable"))?;
        serde_json::from_slice(&bytes).map_err(|_| Error::WriteOutcomeUnknown("invalid_json"))
    }

    pub(crate) fn post_feature(
        &self,
        path: &str,
        csrf: &str,
        body: &[u8],
        json: bool,
    ) -> Result<serde_json::Value> {
        self.post_feature_payload(
            path,
            csrf,
            body,
            if json {
                "application/json"
            } else {
                "application/x-www-form-urlencoded; charset=UTF-8"
            },
            json,
        )
    }
    pub(crate) fn post_feature_payload(
        &self,
        path: &str,
        csrf: &str,
        body: &[u8],
        content_type: &str,
        json: bool,
    ) -> Result<serde_json::Value> {
        if !matches!(
            path,
            "/x/v3/fav/resource/deal"
                | "/x/web-interface/coin/add"
                | "/x/v2/dm/post"
                | "/x/dynamic/feed/create/dyn"
                | "/x/dynamic/feed/dyn/thumb"
                | "/x/v2/reply/add"
                | "/x/dynamic/feed/draw/upload_bfs"
        ) || body.is_empty()
            || body.len() > 2 * 1024 * 1024 + 4096
        {
            return Err(Error::InvalidConfig);
        }
        let cookie = self.config.cookie.as_deref().ok_or(Error::LoginRequired)?;
        let url = if json {
            format!("https://api.bilibili.com{path}?csrf={csrf}")
        } else {
            format!("https://api.bilibili.com{path}")
        };
        let agent: Agent = Agent::config_builder()
            .https_only(true)
            .max_redirects(0)
            .http_status_as_error(false)
            .proxy(None)
            .timeout_global(Some(self.config.request_timeout))
            .timeout_connect(Some(self.config.connect_timeout))
            .timeout_per_read(Some(Duration::from_secs(15)))
            .max_idle_connections(0)
            .user_agent("agent-runtime/0.1.0")
            .build()
            .into();
        let mut response = agent
            .post(&url)
            .header("Cookie", cookie)
            .header("Origin", "https://www.bilibili.com")
            .header("Referer", "https://www.bilibili.com/")
            .header("Content-Type", content_type)
            .send(body)
            .map_err(|_| Error::WriteOutcomeUnknown("network"))?;
        if response.status().as_u16() != 200 {
            return Err(Error::WriteOutcomeUnknown("http_status"));
        }
        let bytes = read_limited(response.body_mut().as_reader(), self.config.max_json_bytes)
            .map_err(|_| Error::WriteOutcomeUnknown("response_unavailable"))?;
        serde_json::from_slice(&bytes).map_err(|_| Error::WriteOutcomeUnknown("invalid_json"))
    }
    pub(crate) fn api_bytes(&self, url: &str) -> Result<Vec<u8>> {
        let mut response = self.get(url, UrlRole::Api, false)?;
        read_limited(response.body_mut().as_reader(), self.config.max_json_bytes)
    }
    pub fn new(config: Config) -> Result<Self> {
        if !(1024..=8 * 1024 * 1024).contains(&config.max_json_bytes)
            || config.max_download_bytes == 0
            || config.max_download_bytes > 512 * 1024 * 1024
            || config
                .cookie
                .as_ref()
                .is_some_and(|v| v.len() > 16384 || v.bytes().any(|b| b < 32 || b == 127))
            || [
                config.connect_timeout,
                config.request_timeout,
                config.download_timeout,
            ]
            .iter()
            .any(|t| t.is_zero() || t.as_secs() > 600)
        {
            return Err(Error::InvalidConfig);
        }
        let agent = |timeout| -> Agent {
            Agent::config_builder().https_only(true).max_redirects(0)
                .http_status_as_error(false).proxy(None)
                .timeout_global(Some(timeout)).timeout_connect(Some(config.connect_timeout))
                .timeout_per_read(Some(Duration::from_secs(15)))
                .max_response_header_size(32 * 1024).max_idle_connections(4)
                .max_idle_connections_per_host(1)
                .user_agent("Mozilla/5.0 (X11; Linux x86_64) AppleWebKit/537.36 Chrome/131.0.0.0 Safari/537.36")
                .build().into()
        };
        Ok(Self {
            api: agent(config.request_timeout),
            media: agent(config.download_timeout),
            config,
        })
    }
    fn get(&self, input: &str, role: UrlRole, media: bool) -> Result<Response<Body>> {
        let mut url = input.to_owned();
        let agent = if media { &self.media } else { &self.api };
        for _ in 0..=3 {
            parse_url(&url, role)?;
            let mut request = agent
                .get(&url)
                .header("Referer", "https://www.bilibili.com/");
            // No cookie leaves api.bilibili.com, including across redirects.
            if matches!(role, UrlRole::Api) {
                if let Some(cookie) = &self.config.cookie {
                    request = request.header("Cookie", cookie);
                }
            }
            let response = request.call().map_err(|_| Error::Network)?;
            let status = response.status().as_u16();
            if matches!(status, 301 | 302 | 303 | 307 | 308) {
                let location = response
                    .headers()
                    .get("Location")
                    .and_then(|v| v.to_str().ok())
                    .ok_or(Error::UrlPolicy)?;
                url = redirect_url(&url, location, role)?;
                continue;
            }
            if status == 412 || status == 429 {
                return Err(Error::RiskControl(status.into()));
            }
            if status != 200 {
                return Err(Error::HttpStatus(status));
            }
            return Ok(response);
        }
        Err(Error::UrlPolicy)
    }
    pub fn json(&self, url: &str) -> Result<serde_json::Value> {
        self.read_json(url, UrlRole::Api)
    }
    pub fn json_resource(&self, url: &str) -> Result<serde_json::Value> {
        self.read_json(url, UrlRole::Resource)
    }
    fn read_json(&self, url: &str, role: UrlRole) -> Result<serde_json::Value> {
        let mut response = self.get(url, role, false)?;
        let bytes = read_limited(response.body_mut().as_reader(), self.config.max_json_bytes)?;
        serde_json::from_slice(&bytes).map_err(|_| Error::InvalidJson)
    }
    pub fn download<W: Write + ?Sized>(
        &self,
        resource: &MediaResource,
        output: &mut W,
        maximum: u64,
    ) -> Result<DownloadReceipt> {
        if maximum == 0 || maximum > self.config.max_download_bytes {
            return Err(Error::DownloadLimit);
        }
        // Primary failures may use an explicitly advertised alternative, but no
        // automatic retry follows bytes already written to the caller's writer.
        let mut response = match self.get(&resource.url, UrlRole::Resource, true) {
            Ok(response) => response,
            Err(first) => {
                let mut result = None;
                for backup in resource.backup_urls.iter().take(2) {
                    if let Ok(response) = self.get(backup, UrlRole::Resource, true) {
                        result = Some(response);
                        break;
                    }
                }
                result.ok_or(first)?
            }
        };
        let length = response
            .headers()
            .get("Content-Length")
            .and_then(|v| v.to_str().ok())
            .and_then(|v| v.parse::<u64>().ok());
        if length.is_some_and(|n| n > maximum) {
            return Err(Error::DownloadLimit);
        }
        let content_type = response
            .headers()
            .get("Content-Type")
            .and_then(|v| v.to_str().ok())
            .map(str::to_owned);
        if content_type
            .as_ref()
            .is_some_and(|v| v.starts_with("text/html"))
        {
            return Err(Error::Schema("media response is HTML"));
        }
        let bytes = copy_limited(response.body_mut().as_reader(), output, maximum)?;
        if length.is_some_and(|expected| expected != bytes) {
            return Err(Error::DownloadIncomplete);
        }
        Ok(DownloadReceipt {
            bytes,
            content_type,
        })
    }
}
fn read_limited(mut reader: impl Read, maximum: usize) -> Result<Vec<u8>> {
    let mut out = Vec::with_capacity(maximum.min(8192));
    reader
        .by_ref()
        .take(maximum as u64 + 1)
        .read_to_end(&mut out)
        .map_err(|_| Error::Network)?;
    if out.len() > maximum {
        return Err(Error::JsonLimit);
    }
    Ok(out)
}
fn copy_limited<W: Write + ?Sized>(
    mut reader: impl Read,
    output: &mut W,
    maximum: u64,
) -> Result<u64> {
    let mut buffer = [0; 32 * 1024];
    let mut written = 0;
    loop {
        let available = (maximum - written).min(buffer.len() as u64) as usize;
        if available == 0 {
            let mut probe = [0];
            if reader.read(&mut probe).map_err(|_| Error::Network)? != 0 {
                return Err(Error::DownloadLimit);
            }
            return Ok(written);
        }
        let count = reader
            .read(&mut buffer[..available])
            .map_err(|_| Error::Network)?;
        if count == 0 {
            return Ok(written);
        }
        output.write_all(&buffer[..count]).map_err(|_| Error::Io)?;
        written += count as u64;
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn strict_url_scope_and_redirect_revalidation() {
        for url in [
            "https://upos-sz-mirrorali.bilivideo.com/a?token=x",
            "https://aisubtitle.hdslb.com/a.json",
            "https://x.mcdn.bilivideo.cn/a",
        ] {
            assert!(validate_resource_url(url).is_ok());
        }
        for url in [
            "http://i0.hdslb.com/a",
            "https://localhost/a",
            "https://127.0.0.1/a",
            "https://[::1]/a",
            "https://hdslb.com.evil.invalid/a",
            "https://evilhdslb.com/a",
            "https://x@i0.hdslb.com/a",
            "https://i0.hdslb.com:8080/a",
            "https://i0.hdslb.com./a",
            "https://i0.hdslb.com/a#frag",
            "https://i0.hdslb.com/\\evil",
        ] {
            assert!(validate_resource_url(url).is_err(), "{url}");
        }
        assert!(
            redirect_url(
                "https://i0.hdslb.com/a",
                "https://localhost/secret",
                UrlRole::Resource
            )
            .is_err()
        );
        assert_eq!(
            redirect_url("https://i0.hdslb.com/a", "/b", UrlRole::Resource).unwrap(),
            "https://i0.hdslb.com/b"
        );
        assert!(
            redirect_url(
                "https://api.bilibili.com/a",
                "https://i0.hdslb.com/b",
                UrlRole::Api
            )
            .is_err()
        );
    }
    #[test]
    fn decompressed_json_and_streamed_files_have_hard_limits() {
        assert_eq!(read_limited(&b"12345"[..], 4), Err(Error::JsonLimit));
        assert_eq!(read_limited(&b"1234"[..], 4).unwrap(), b"1234");
        let mut output = Vec::new();
        assert_eq!(
            copy_limited(&b"12345"[..], &mut output, 4),
            Err(Error::DownloadLimit)
        );
        assert_eq!(output, b"1234");
        assert_eq!(copy_limited(&b"1234"[..], &mut Vec::new(), 4).unwrap(), 4);
    }
    #[test]
    fn malformed_cookie_is_rejected_without_network() {
        let mut config = Config::default();
        config.cookie = Some("secret\r\nHost: localhost".into());
        assert!(matches!(Transport::new(config), Err(Error::InvalidConfig)));
    }
}
