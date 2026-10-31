// SPDX-License-Identifier: MIT
use crate::{API_VERSION, Error, MAX_BODY_BYTES, Result, encode};
use serde_json::Value;
use std::{fmt, io::Read, time::Duration};

#[derive(Clone)]
pub struct Config {
    pub token: Option<String>,
    pub timeout: Duration,
    pub max_response_bytes: usize,
    pub max_file_bytes: usize,
}
impl Default for Config {
    fn default() -> Self {
        Self {
            token: None,
            timeout: Duration::from_secs(30),
            max_response_bytes: MAX_BODY_BYTES,
            max_file_bytes: MAX_BODY_BYTES,
        }
    }
}
impl fmt::Debug for Config {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Config")
            .field("token", &self.token.as_ref().map(|_| "[redacted]"))
            .field("timeout", &self.timeout)
            .field("max_response_bytes", &self.max_response_bytes)
            .field("max_file_bytes", &self.max_file_bytes)
            .finish()
    }
}

pub(crate) struct Transport {
    config: Config,
    read: ureq::Agent,
    write: ureq::Agent,
}
pub(crate) struct JsonResponse {
    pub value: Value,
    pub link: Option<String>,
    pub poll_interval_seconds: Option<u64>,
}
impl Transport {
    pub fn new(config: Config) -> Result<Self> {
        if config.timeout.is_zero()
            || config.timeout > Duration::from_secs(180)
            || !(1..=MAX_BODY_BYTES).contains(&config.max_response_bytes)
            || !(1..=MAX_BODY_BYTES).contains(&config.max_file_bytes)
            || config.token.as_ref().is_some_and(|t| {
                t.is_empty() || t.len() > 8192 || !t.bytes().all(|b| (b'!'..=b'~').contains(&b))
            })
        {
            return Err(Error::InvalidConfig);
        }
        fn agent(timeout: Duration, pooled: bool) -> ureq::Agent {
            ureq::Agent::config_builder()
                .https_only(true)
                .max_redirects(0)
                .http_status_as_error(false)
                .proxy(None)
                .timeout_global(Some(timeout))
                .timeout_connect(Some(Duration::from_secs(8)))
                .timeout_per_read(Some(Duration::from_secs(15)))
                .max_response_header_size(32 * 1024)
                .max_idle_connections(if pooled { 2 } else { 0 })
                .max_idle_connections_per_host(if pooled { 1 } else { 0 })
                .user_agent("agent-runtime/0.1.0")
                .build()
                .into()
        }
        let read = agent(config.timeout, true);
        // Writes never reuse a possibly stale pooled connection. The request is
        // submitted once; ureq has no application-level retry loop here.
        let write = agent(config.timeout, false);
        Ok(Self {
            config,
            read,
            write,
        })
    }
    pub fn has_token(&self) -> bool {
        self.config.token.is_some()
    }
    pub fn require_token(&self) -> Result<()> {
        if self.has_token() {
            Ok(())
        } else {
            Err(Error::LoginRequired)
        }
    }
    pub fn max_file_bytes(&self) -> usize {
        self.config.max_file_bytes
    }
    pub fn get_json(
        &self,
        path: &str,
        query: &[(&str, String)],
        accept: &str,
    ) -> Result<JsonResponse> {
        let mut response = self.get_response(path, query, accept)?;
        let status = response.status().as_u16();
        let headers = StatusHeaders::from_response(&response);
        let link = response
            .headers()
            .get("link")
            .map(|v| v.to_str().unwrap_or("invalid-link-header").to_owned());
        let poll_interval_seconds = header_u64(&response, "x-poll-interval");
        let bytes = read_bounded(
            response.body_mut().as_reader(),
            self.config.max_response_bytes,
        )
        .or_else(|error| {
            status_result(status, 200, &headers, &[])?;
            Err(error)
        })?;
        status_result(status, 200, &headers, &bytes)?;
        let value = parse_json(&bytes)?;
        Ok(JsonResponse {
            value,
            link,
            poll_interval_seconds,
        })
    }
    pub fn get_raw(&self, path: &str, query: &[(&str, String)]) -> Result<Vec<u8>> {
        self.get_bytes(
            path,
            query,
            "application/vnd.github.raw+json",
            self.config.max_file_bytes,
        )
    }
    pub fn get_bytes(
        &self,
        path: &str,
        query: &[(&str, String)],
        accept: &str,
        limit: usize,
    ) -> Result<Vec<u8>> {
        let mut response = self.get_response(path, query, accept)?;
        let status = response.status().as_u16();
        let headers = StatusHeaders::from_response(&response);
        let limit = if status == 200 {
            limit.min(self.config.max_response_bytes)
        } else {
            self.config.max_response_bytes
        };
        let bytes = read_bounded(response.body_mut().as_reader(), limit).or_else(|error| {
            status_result(status, 200, &headers, &[])?;
            Err(error)
        })?;
        status_result(status, 200, &headers, &bytes)?;
        Ok(bytes)
    }
    fn get_response(
        &self,
        path: &str,
        query: &[(&str, String)],
        accept: &str,
    ) -> Result<ureq::http::Response<ureq::Body>> {
        let url = build_url(path, query)?;
        let mut request = self
            .read
            .get(&url)
            .header("Accept", accept)
            .header("X-GitHub-Api-Version", API_VERSION);
        if let Some(token) = &self.config.token {
            request = request.header("Authorization", &format!("Bearer {token}"));
        }
        request.call().map_err(|_| Error::Network)
    }
    pub fn post_json(&self, path: &str, body: &Value) -> Result<Value> {
        self.write_json("POST", path, body, 201)
    }
    pub(crate) fn write_json(
        &self,
        method: &str,
        path: &str,
        body: &Value,
        expected: u16,
    ) -> Result<Value> {
        self.require_token()?;
        let url = build_url(path, &[])?;
        let bytes = serde_json::to_vec(body).map_err(|_| Error::Schema("request encoding"))?;
        if bytes.len() > MAX_BODY_BYTES
            || !matches!(method, "POST" | "PATCH" | "PUT")
            || !matches!(expected, 200 | 201)
        {
            return Err(Error::Limit);
        }
        let token = self.config.token.as_ref().ok_or(Error::LoginRequired)?;
        macro_rules! send {
            ($r:expr) => {
                $r.header("Accept", "application/vnd.github+json")
                    .header("X-GitHub-Api-Version", API_VERSION)
                    .header("Content-Type", "application/json")
                    .header("Authorization", &format!("Bearer {token}"))
                    .send(bytes.as_slice())
            };
        }
        let mut response = match method {
            "POST" => send!(self.write.post(&url)),
            "PATCH" => send!(self.write.patch(&url)),
            "PUT" => send!(self.write.put(&url)),
            _ => unreachable!(),
        }
        .map_err(|_| Error::WriteResultUnknown)?;
        let status = response.status().as_u16();
        let h = StatusHeaders::from_response(&response);
        let bytes = read_bounded(
            response.body_mut().as_reader(),
            self.config.max_response_bytes,
        );
        write_reply(status, expected, &h, bytes)
    }
}

#[cfg(test)]
fn post_reply(status: u16, headers: &StatusHeaders, bytes: Result<Vec<u8>>) -> Result<Value> {
    write_reply(status, 201, headers, bytes)
}
fn write_reply(
    status: u16,
    expected: u16,
    headers: &StatusHeaders,
    bytes: Result<Vec<u8>>,
) -> Result<Value> {
    // An acknowledged rejection remains a rejection even if its body was lost.
    // A success/5xx with a lost or malformed response remains unknown.
    if status != expected {
        if (300..500).contains(&status) {
            status_result(status, expected, headers, bytes.as_deref().unwrap_or(&[]))?;
        }
        return Err(Error::WriteResultUnknown);
    }
    parse_json(&bytes.map_err(|_| Error::WriteResultUnknown)?)
        .map_err(|_| Error::WriteResultUnknown)
}

pub(crate) fn fixed_api_uri(url: &str) -> Result<ureq::http::Uri> {
    if url.contains('#') || url.contains('\\') || url.chars().any(|c| c.is_control()) {
        return Err(Error::InvalidPath);
    }
    let uri: ureq::http::Uri = url.parse().map_err(|_| Error::InvalidPath)?;
    if uri.scheme_str() != Some("https")
        || uri.host() != Some("api.github.com")
        || !matches!(uri.port_u16(), None | Some(443))
        || uri.authority().is_none_or(|a| a.as_str().contains('@'))
    {
        return Err(Error::InvalidPath);
    }
    Ok(uri)
}
fn build_url(path: &str, query: &[(&str, String)]) -> Result<String> {
    if !path.starts_with('/')
        || path.starts_with("//")
        || path.contains(['?', '#', '\\'])
        || path.chars().any(|c| c.is_control())
    {
        return Err(Error::InvalidPath);
    }
    let mut url = format!("https://api.github.com{path}");
    if !query.is_empty() {
        url.push('?');
        url.push_str(
            &query
                .iter()
                .map(|(k, v)| format!("{}={}", encode(k), encode(v)))
                .collect::<Vec<_>>()
                .join("&"),
        );
    }
    fixed_api_uri(&url)?;
    Ok(url)
}
fn parse_json(bytes: &[u8]) -> Result<Value> {
    if bytes.is_empty() {
        return Err(Error::Schema("empty response"));
    }
    let value: Value = serde_json::from_slice(bytes).map_err(|_| Error::Schema("invalid JSON"))?;
    if !value.is_object() && !value.is_array() {
        return Err(Error::Schema("expected object or array"));
    }
    if value.get("message").is_some()
        && value.get("documentation_url").is_some()
        && value.get("id").is_none()
    {
        return Err(Error::Schema("error object in success response"));
    }
    Ok(value)
}
fn read_bounded(reader: impl Read, limit: usize) -> Result<Vec<u8>> {
    let mut bytes = Vec::new();
    reader
        .take(limit as u64 + 1)
        .read_to_end(&mut bytes)
        .map_err(|_| Error::Network)?;
    if bytes.len() > limit {
        Err(Error::Limit)
    } else {
        Ok(bytes)
    }
}
#[derive(Default)]
struct StatusHeaders {
    retry_after_seconds: Option<u64>,
    reset_at: Option<u64>,
    exhausted: bool,
}
impl StatusHeaders {
    fn from_response(response: &ureq::http::Response<ureq::Body>) -> Self {
        Self {
            retry_after_seconds: header_u64(response, "retry-after"),
            reset_at: header_u64(response, "x-ratelimit-reset"),
            exhausted: header_u64(response, "x-ratelimit-remaining") == Some(0),
        }
    }
}
fn header_u64(response: &ureq::http::Response<ureq::Body>, name: &str) -> Option<u64> {
    response.headers().get(name)?.to_str().ok()?.parse().ok()
}
fn status_result(status: u16, expected: u16, headers: &StatusHeaders, bytes: &[u8]) -> Result<()> {
    if status == expected {
        return Ok(());
    }
    // Only inspect the bounded message for classification; never expose a raw
    // server error, request URL, header, or credential in an error string.
    let rate_message = status == 403
        && serde_json::from_slice::<Value>(bytes)
            .ok()
            .and_then(|v| v["message"].as_str().map(|s| s.to_ascii_lowercase()))
            .is_some_and(|m| m.contains("rate limit"));
    Err(match status {
        401 => Error::LoginRequired,
        403 | 429
            if status == 429
                || headers.exhausted
                || headers.retry_after_seconds.is_some()
                || rate_message =>
        {
            Error::RateLimited {
                retry_after_seconds: headers.retry_after_seconds,
                reset_at: headers.reset_at,
            }
        }
        403 => Error::PermissionDenied,
        404 => Error::NotFound,
        304 => Error::NotModified,
        300..=399 => Error::RedirectDenied,
        422 => Error::ValidationFailed,
        _ => Error::HttpStatus(status),
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn rejects_external_endpoints_and_credentials() {
        for url in [
            "http://api.github.com/x",
            "https://api.github.com.evil/x",
            "https://u@api.github.com/x",
            "https://api.github.com:444/x",
            "https://api.github.com/x#y",
            "https://api.github.com\\@evil.test/x",
        ] {
            assert!(fixed_api_uri(url).is_err(), "{url}");
        }
        assert!(fixed_api_uri("https://api.github.com:443/repos/o/r").is_ok());
        assert_eq!(
            build_url("/repos/o/r/contents/a%20b", &[("ref", "x/y?z".into())]).unwrap(),
            "https://api.github.com/repos/o/r/contents/a%20b?ref=x%2Fy%3Fz"
        );
    }
    #[test]
    fn bounds_decoded_response_and_rejects_empty_success() {
        assert_eq!(read_bounded(&b"abcd"[..], 3).unwrap_err(), Error::Limit);
        assert_eq!(read_bounded(&b"abc"[..], 3).unwrap(), b"abc");
        assert!(parse_json(b"").is_err());
        assert!(parse_json(b"null").is_err());
        assert!(parse_json(br#"{"message":"denied","documentation_url":"ignored"}"#).is_err());
    }
    #[test]
    fn failures_are_distinct_and_do_not_echo_server_secrets() {
        let headers = StatusHeaders {
            retry_after_seconds: Some(30),
            reset_at: Some(123),
            exhausted: false,
        };
        assert_eq!(
            status_result(403, 200, &headers, b"not json"),
            Err(Error::RateLimited {
                retry_after_seconds: Some(30),
                reset_at: Some(123)
            })
        );
        for (status, error) in [
            (401, Error::LoginRequired),
            (403, Error::PermissionDenied),
            (404, Error::NotFound),
            (304, Error::NotModified),
            (302, Error::RedirectDenied),
            (422, Error::ValidationFailed),
        ] {
            assert_eq!(
                status_result(
                    status,
                    200,
                    &StatusHeaders::default(),
                    b"secret_fixture_token"
                ),
                Err(error)
            );
        }
        assert!(
            !status_result(500, 200, &headers, b"secret_fixture_token")
                .unwrap_err()
                .to_string()
                .contains("secret_fixture_token")
        );
    }
    #[test]
    fn writes_with_lost_or_ambiguous_response_remain_unknown() {
        let headers = StatusHeaders::default();
        for bytes in [
            Err(Error::Network),
            Err(Error::Limit),
            Ok(b"not json".to_vec()),
            Ok(Vec::new()),
        ] {
            assert_eq!(
                post_reply(201, &headers, bytes),
                Err(Error::WriteResultUnknown)
            );
        }
        assert_eq!(
            post_reply(500, &headers, Ok(br#"{"message":"server error"}"#.to_vec())),
            Err(Error::WriteResultUnknown)
        );
        assert_eq!(
            post_reply(403, &headers, Err(Error::Network)),
            Err(Error::PermissionDenied)
        );
        assert_eq!(
            post_reply(302, &headers, Err(Error::Network)),
            Err(Error::RedirectDenied)
        );
        assert!(post_reply(201, &headers, Ok(br#"{"id":17,"body":"hello"}"#.to_vec())).is_ok());
    }
}
