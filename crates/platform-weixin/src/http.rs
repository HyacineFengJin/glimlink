// SPDX-License-Identifier: MIT
use crate::{Error, LoginProfile, Result, state::Cookie};
use serde_json::Value;
use std::{
    io::{Read, Write},
    time::{Duration, SystemTime, UNIX_EPOCH},
};
use ureq::{
    Agent, Body,
    http::{Response, Uri},
};

#[derive(Debug, Clone)]
pub struct Config {
    pub profile: LoginProfile,
    pub request_timeout: Duration,
    pub download_timeout: Duration,
    pub max_json_bytes: usize,
    pub max_media_bytes: u64,
    #[cfg(test)]
    pub(crate) test_endpoint: Option<String>,
}
impl Default for Config {
    fn default() -> Self {
        Self {
            profile: LoginProfile::Web,
            request_timeout: Duration::from_secs(35),
            download_timeout: Duration::from_secs(90),
            max_json_bytes: 2 * 1024 * 1024,
            max_media_bytes: 64 * 1024 * 1024,
            #[cfg(test)]
            test_endpoint: None,
        }
    }
}
pub(crate) fn base_host(host: &str) -> bool {
    matches!(
        host,
        "wx.qq.com" | "wx2.qq.com" | "wx8.qq.com" | "web.wechat.com" | "web2.wechat.com"
    )
}
fn allowed_host(host: &str) -> bool {
    base_host(host)
        || matches!(host, "login.wx.qq.com" | "login.weixin.qq.com")
        || host.strip_prefix("webpush.").is_some_and(base_host)
}
pub(crate) fn validate_url(value: &str) -> Result<Uri> {
    if value.len() > 16384
        || value.contains(['\\', '#'])
        || value.bytes().any(|b| b <= 32 || b == 127)
    {
        return Err(Error::UrlPolicy);
    }
    let uri: Uri = value.parse().map_err(|_| Error::UrlPolicy)?;
    let host = uri.host().ok_or(Error::UrlPolicy)?;
    if uri.scheme_str() != Some("https")
        || !matches!(uri.port_u16(), None | Some(443))
        || uri.authority().is_none_or(|v| v.as_str().contains('@'))
        || !allowed_host(host)
    {
        return Err(Error::UrlPolicy);
    }
    Ok(uri)
}
pub(crate) fn validate_redirect(value: &str) -> Result<String> {
    let uri = validate_url(value)?;
    let host = uri.host().ok_or(Error::UrlPolicy)?;
    if !base_host(host) || uri.path() != "/cgi-bin/mmwebwx-bin/webwxnewloginpage" {
        return Err(Error::UrlPolicy);
    }
    Ok(host.to_owned())
}
pub(crate) fn ticket_url(redirect: &str) -> Result<String> {
    validate_redirect(redirect)?;
    let (base, raw) = redirect.split_once('?').unwrap_or((redirect, ""));
    let mut parts = raw
        .split('&')
        .filter(|v| !v.is_empty() && v.split('=').next() != Some("version"))
        .map(str::to_owned)
        .collect::<Vec<_>>();
    parts.push("version=v2".into());
    Ok(format!("{base}?{}", parts.join("&")))
}
fn encode(input: &str) -> String {
    let mut out = String::new();
    for &b in input.as_bytes() {
        if b.is_ascii_alphanumeric() || b"-_.~".contains(&b) {
            out.push(b as char)
        } else {
            use std::fmt::Write;
            let _ = write!(&mut out, "%{b:02X}");
        }
    }
    out
}
pub(crate) fn query(base: &str, values: &[(&str, String)]) -> String {
    let mut url = base.to_owned();
    url.push('?');
    for (i, (key, value)) in values.iter().enumerate() {
        if i > 0 {
            url.push('&');
        }
        url.push_str(&encode(key));
        url.push('=');
        url.push_str(&encode(value));
    }
    url
}

pub(crate) struct Transport {
    api: Agent,
    media: Agent,
    config: Config,
}
pub(crate) struct TicketResponse {
    pub status: u16,
    pub content_type: Option<String>,
    pub location: Option<String>,
    pub body: Vec<u8>,
}
impl Transport {
    pub fn new(config: Config) -> Result<Self> {
        if config.request_timeout.is_zero()
            || config.download_timeout.is_zero()
            || config.request_timeout.as_secs() > 120
            || config.download_timeout.as_secs() > 300
            || !(1024..=8 * 1024 * 1024).contains(&config.max_json_bytes)
            || config.max_media_bytes == 0
            || config.max_media_bytes > 128 * 1024 * 1024
        {
            return Err(Error::InvalidInput);
        }
        let agent = |timeout| -> Agent {
            Agent::config_builder().https_only(!cfg!(test)).max_redirects(0).http_status_as_error(false).proxy(None)
            .timeout_global(Some(timeout)).timeout_connect(Some(Duration::from_secs(8))).timeout_per_read(Some(Duration::from_secs(30)))
            .max_response_header_size(32768).max_idle_connections(2).max_idle_connections_per_host(1)
            .user_agent("Mozilla/5.0 (X11; Linux x86_64) AppleWebKit/537.36 Chrome/131.0.0.0 Safari/537.36").build().into()
        };
        Ok(Self {
            api: agent(config.request_timeout),
            media: agent(config.download_timeout),
            config,
        })
    }
    fn request(
        &self,
        url: &str,
        cookies: &mut Vec<Cookie>,
        body: Option<&[u8]>,
        range: bool,
        media: bool,
        follow_redirects: bool,
        ticket_request: bool,
    ) -> Result<Response<Body>> {
        let mut url = url.to_owned();
        for _ in 0..4 {
            let uri = validate_url(&url)?;
            let cookie = cookie_header(cookies, &uri)?;
            let referer = format!("https://{}/", uri.host().ok_or(Error::UrlPolicy)?);
            let agent = if media { &self.media } else { &self.api };
            #[cfg(test)]
            let request_url = self.config.test_endpoint.as_ref().map(|base| {
                format!(
                    "{base}{}",
                    uri.path_and_query().map(|v| v.as_str()).unwrap_or("/")
                )
            });
            #[cfg(test)]
            let request_url = request_url.as_deref().unwrap_or(&url);
            #[cfg(not(test))]
            let request_url = &url;
            let mut response = if let Some(body) = body {
                let mut req = agent
                    .post(request_url)
                    .header("Content-Type", "application/json; charset=utf-8")
                    .header("Referer", &referer);
                if !cookie.is_empty() {
                    req = req.header("Cookie", &cookie);
                }
                req.send(body).map_err(|_| Error::Network)?
            } else {
                let mut req = agent.get(request_url).header("Referer", &referer);
                if !cookie.is_empty() {
                    req = req.header("Cookie", &cookie);
                }
                if range {
                    req = req.header("Range", "bytes=0-");
                }
                if ticket_request && self.config.profile == LoginProfile::DesktopCompatibility {
                    validate_redirect(&url)?;
                    req = req
                        .header("client-version", crate::desktop::CLIENT_VERSION)
                        .header("extspam", crate::desktop::EXTSPAM);
                }
                req.call().map_err(|_| Error::Network)?
            };
            for value in response.headers().get_all("set-cookie").iter() {
                let value = value.to_str().map_err(|_| Error::Protocol)?;
                set_cookie(cookies, &uri, value)?;
            }
            let status = response.status().as_u16();
            if matches!(status, 301 | 302 | 303 | 307 | 308) {
                if !follow_redirects {
                    return Ok(response);
                }
                if body.is_some() {
                    return Err(Error::Protocol);
                }
                let location = response
                    .headers()
                    .get("location")
                    .and_then(|v| v.to_str().ok())
                    .ok_or(Error::Protocol)?;
                let candidate = if location.starts_with("https://") {
                    location.to_owned()
                } else if location.starts_with('/') && !location.starts_with("//") {
                    format!(
                        "https://{}{location}",
                        uri.authority().ok_or(Error::UrlPolicy)?
                    )
                } else {
                    return Err(Error::UrlPolicy);
                };
                validate_url(&candidate)?;
                url = candidate;
                continue;
            }
            if status != 200 && !(range && status == 206) {
                return Err(Error::Http { status });
            }
            // Make ownership explicit; response bodies are consumed by bounded callers.
            let _ = &mut response;
            return Ok(response);
        }
        Err(Error::Protocol)
    }
    pub fn bytes(
        &self,
        url: &str,
        cookies: &mut Vec<Cookie>,
        maximum: usize,
        body: Option<&[u8]>,
        range: Option<bool>,
    ) -> Result<Vec<u8>> {
        if maximum == 0 || maximum as u64 > self.config.max_media_bytes {
            return Err(Error::Limit);
        }
        let mut response = self.request(
            url,
            cookies,
            body,
            range.unwrap_or(false),
            false,
            true,
            false,
        )?;
        read_limited(response.body_mut().as_reader(), maximum)
    }
    /// WebWx ticket redirects can carry the XML credentials in their own body.
    /// Following Location would discard them and load the ordinary landing page.
    pub fn ticket_response(&self, url: &str, cookies: &mut Vec<Cookie>) -> Result<TicketResponse> {
        validate_redirect(url)?;
        let mut response = self.request(url, cookies, None, false, false, false, true)?;
        let status = response.status().as_u16();
        let content_type = response
            .headers()
            .get("content-type")
            .and_then(|v| v.to_str().ok())
            .map(str::to_owned);
        let location = response
            .headers()
            .get("location")
            .and_then(|v| v.to_str().ok())
            .map(|value| {
                if value == "/" {
                    return "/".to_string();
                }
                match validate_url(value) {
                    Ok(uri) => format!(
                        "https://{}{}",
                        uri.host().unwrap_or("[redacted]"),
                        if uri.path() == "/"
                            || uri.path() == "/cgi-bin/mmwebwx-bin/webwxnewloginpage"
                        {
                            uri.path()
                        } else {
                            "/[path-redacted]"
                        }
                    ),
                    Err(_) => "[redacted]".into(),
                }
            });
        let body = read_limited(response.body_mut().as_reader(), 64 * 1024)?;
        Ok(TicketResponse {
            status,
            content_type,
            location,
            body,
        })
    }
    pub fn json(
        &self,
        url: &str,
        cookies: &mut Vec<Cookie>,
        body: Option<&Value>,
    ) -> Result<Value> {
        let encoded = body
            .map(serde_json::to_vec)
            .transpose()
            .map_err(|_| Error::Protocol)?;
        if encoded
            .as_ref()
            .is_some_and(|v| v.len() > self.config.max_json_bytes)
        {
            return Err(Error::Limit);
        }
        let bytes = self.bytes(
            url,
            cookies,
            self.config.max_json_bytes,
            encoded.as_deref(),
            None,
        )?;
        serde_json::from_slice(&bytes).map_err(|_| Error::Protocol)
    }
    pub fn download<W: Write + ?Sized>(
        &self,
        url: &str,
        cookies: &mut Vec<Cookie>,
        output: &mut W,
        maximum: u64,
        range: bool,
    ) -> Result<(u64, Option<String>)> {
        if maximum == 0 || maximum > self.config.max_media_bytes {
            return Err(Error::Limit);
        }
        let mut response = self.request(url, cookies, None, range, true, true, false)?;
        let mime = response
            .headers()
            .get("content-type")
            .and_then(|v| v.to_str().ok())
            .map(str::to_owned);
        if mime.as_deref().is_some_and(|v| {
            v.starts_with("text/")
                || v.starts_with("application/json")
                || v.starts_with("application/xml")
        }) {
            return Err(Error::Protocol);
        }
        let length = response
            .headers()
            .get("content-length")
            .and_then(|v| v.to_str().ok())
            .and_then(|v| v.parse::<u64>().ok());
        if length.is_some_and(|v| v > maximum) {
            return Err(Error::Limit);
        }
        let mut reader = response.body_mut().as_reader();
        let mut written = 0;
        let mut buffer = [0_u8; 32768];
        loop {
            let cap = (maximum - written).min(buffer.len() as u64) as usize;
            if cap == 0 {
                let mut extra = [0];
                if reader.read(&mut extra).map_err(|_| Error::Network)? != 0 {
                    return Err(Error::Limit);
                }
                break;
            }
            let n = reader
                .read(&mut buffer[..cap])
                .map_err(|_| Error::Network)?;
            if n == 0 {
                break;
            }
            output.write_all(&buffer[..n]).map_err(|_| Error::Io)?;
            written += n as u64;
        }
        if length.is_some_and(|n| n != written) {
            return Err(Error::Protocol);
        }
        Ok((written, mime))
    }
}
fn read_limited(mut input: impl Read, max: usize) -> Result<Vec<u8>> {
    let mut bytes = Vec::new();
    input
        .by_ref()
        .take(max as u64 + 1)
        .read_to_end(&mut bytes)
        .map_err(|_| Error::Network)?;
    if bytes.len() > max {
        return Err(Error::Limit);
    }
    Ok(bytes)
}
pub(crate) fn validate_cookie(cookie: &Cookie) -> Result<()> {
    if cookie.name.is_empty()
        || cookie.name.len() > 128
        || !cookie
            .name
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b"_-.".contains(&b))
        || cookie.value.len() > 4096
        || cookie
            .value
            .bytes()
            .any(|b| b < 0x21 || b > 0x7e || b == b';')
        || cookie.path.len() > 1024
        || !cookie.path.starts_with('/')
        || !(allowed_host(&cookie.domain)
            || matches!(
                cookie.domain.as_str(),
                "qq.com" | "wechat.com" | "weixin.qq.com"
            ))
    {
        return Err(Error::InvalidState);
    }
    Ok(())
}
fn domain_matches(host: &str, domain: &str) -> bool {
    host == domain || host.strip_suffix(domain).is_some_and(|v| v.ends_with('.'))
}
fn cookie_header(cookies: &[Cookie], uri: &Uri) -> Result<String> {
    let host = uri.host().ok_or(Error::UrlPolicy)?;
    let path = uri.path();
    let now = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_err(|_| Error::InvalidState)?
        .as_secs();
    let mut values = Vec::new();
    for cookie in cookies {
        validate_cookie(cookie)?;
        let path_matches = path == cookie.path
            || path
                .strip_prefix(&cookie.path)
                .is_some_and(|tail| cookie.path.ends_with('/') || tail.starts_with('/'));
        if (!cookie.host_only && domain_matches(host, &cookie.domain)
            || cookie.host_only && host == cookie.domain)
            && path_matches
            && cookie.expires_at.is_none_or(|v| v > now)
        {
            values.push(format!("{}={}", cookie.name, cookie.value));
        }
    }
    let joined = values.join("; ");
    if joined.len() > 32768 {
        return Err(Error::Limit);
    }
    Ok(joined)
}
fn set_cookie(cookies: &mut Vec<Cookie>, uri: &Uri, value: &str) -> Result<()> {
    if value.len() > 8192 {
        return Err(Error::Limit);
    }
    let mut parts = value.split(';');
    let (name, value) = parts
        .next()
        .ok_or(Error::Protocol)?
        .split_once('=')
        .ok_or(Error::Protocol)?;
    let host = uri.host().ok_or(Error::UrlPolicy)?;
    let mut cookie = Cookie {
        name: name.trim().into(),
        value: value.trim().into(),
        domain: host.into(),
        path: "/".into(),
        host_only: true,
        expires_at: None,
    };
    let mut remove = false;
    for attr in parts {
        let (key, value) = attr.trim().split_once('=').unwrap_or((attr.trim(), ""));
        match key.to_ascii_lowercase().as_str() {
            "domain" => {
                cookie.domain = value.trim_start_matches('.').to_ascii_lowercase();
                cookie.host_only = false;
                if !domain_matches(host, &cookie.domain) {
                    return Err(Error::UrlPolicy);
                }
            }
            "path" => cookie.path = value.into(),
            "max-age" => {
                let age = value.parse::<i64>().map_err(|_| Error::Protocol)?;
                if age <= 0 {
                    remove = true;
                } else {
                    cookie.expires_at = Some(
                        SystemTime::now()
                            .duration_since(UNIX_EPOCH)
                            .map_err(|_| Error::InvalidState)?
                            .as_secs()
                            .saturating_add((age as u64).min(366 * 86400)),
                    );
                }
            }
            "expires" => {
                if value.contains("1970") || value.contains("1969") {
                    remove = true;
                }
            }
            _ => {}
        }
    }
    validate_cookie(&cookie)?;
    cookies.retain(|old| {
        !(old.name == cookie.name && old.domain == cookie.domain && old.path == cookie.path)
    });
    if !remove {
        cookies.push(cookie);
    }
    if cookies.len() > 128
        || cookies
            .iter()
            .map(|c| c.name.len() + c.value.len() + c.domain.len() + c.path.len())
            .sum::<usize>()
            > 32768
    {
        return Err(Error::Limit);
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn url_and_cookie_boundaries() {
        for url in [
            "https://localhost/x",
            "https://wx.qq.com.evil.invalid/x",
            "https://wx.qq.com:8443/x",
            "http://wx.qq.com/x",
            "https://u@wx.qq.com/x",
            "https://wx.qq.com/x#frag",
        ] {
            assert!(validate_url(url).is_err());
        }
        assert!(
            validate_redirect(
                "https://wx2.qq.com/cgi-bin/mmwebwx-bin/webwxnewloginpage?ticket=private"
            )
            .is_ok()
        );
        assert!(
            validate_redirect("https://login.wx.qq.com/cgi-bin/mmwebwx-bin/webwxnewloginpage")
                .is_err()
        );
        let uri = validate_url("https://wx.qq.com/cgi-bin/mmwebwx-bin/webwxinit").unwrap();
        let mut jar = Vec::new();
        set_cookie(
            &mut jar,
            &uri,
            "sid=private; Domain=.wx.qq.com; Path=/cgi-bin",
        )
        .unwrap();
        assert_eq!(cookie_header(&jar, &uri).unwrap(), "sid=private");
        assert_eq!(
            cookie_header(
                &jar,
                &validate_url("https://login.wx.qq.com/jslogin").unwrap()
            )
            .unwrap(),
            ""
        );
        assert!(set_cookie(&mut jar, &uri, "x=secret; Domain=evil.invalid").is_err());
        set_cookie(
            &mut jar,
            &uri,
            "sid=gone; Domain=.wx.qq.com; Path=/cgi-bin; Max-Age=0",
        )
        .unwrap();
        assert!(jar.is_empty());
    }
    #[test]
    fn bytes_and_parameters_are_bounded() {
        assert_eq!(read_limited(&b"12345"[..], 4), Err(Error::Limit));
        assert_eq!(
            query("https://wx.qq.com/x", &[("skey", "@a|b&c".into())]),
            "https://wx.qq.com/x?skey=%40a%7Cb%26c"
        );
    }
}
