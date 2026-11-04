// SPDX-License-Identifier: MIT
use crate::{Error, MediaResource, Result};
use serde::Serialize;
use std::{
    fmt,
    io::{Read, Write},
    time::Duration,
};
use ureq::{Agent, http::Uri};

#[derive(Clone)]
pub struct Config {
    pub bduss: Option<String>,
    pub max_response_bytes: usize,
    pub max_download_bytes: u64,
    pub timeout: Duration,
}
impl Default for Config {
    fn default() -> Self {
        Self {
            bduss: None,
            max_response_bytes: 4 * 1024 * 1024,
            max_download_bytes: 128 * 1024 * 1024,
            timeout: Duration::from_secs(25),
        }
    }
}
impl fmt::Debug for Config {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Config")
            .field("bduss", &self.bduss.as_ref().map(|_| "[redacted]"))
            .finish_non_exhaustive()
    }
}
#[derive(Debug, Clone, Serialize)]
pub struct DownloadReceipt {
    pub bytes: u64,
    pub mime: Option<String>,
}
fn parse_url(input: &str, media: bool) -> Result<Uri> {
    if input.len() > 16384
        || input.contains(['\\', '#'])
        || input.bytes().any(|b| b <= 32 || b == 127)
    {
        return Err(Error::UrlPolicy);
    }
    let uri: Uri = input.parse().map_err(|_| Error::UrlPolicy)?;
    if uri.scheme_str() != Some("https")
        || !matches!(uri.port_u16(), None | Some(443))
        || uri.authority().is_none_or(|v| v.as_str().contains('@'))
    {
        return Err(Error::UrlPolicy);
    }
    let host = uri.host().ok_or(Error::UrlPolicy)?;
    let valid = if media {
        matches!(
            host,
            "imgsrc.baidu.com"
                | "imgsa.baidu.com"
                | "tiebapic.baidu.com"
                | "tiebapic1.baidu.com"
                | "tiebapic2.baidu.com"
                | "tb.himg.baidu.com"
                | "c.tieba.baidu.com"
                | "tb-video.bdstatic.com"
        )
    } else {
        host == "tiebac.baidu.com"
    };
    if !valid {
        return Err(Error::UrlPolicy);
    }
    Ok(uri)
}
pub fn validate_media_url(input: &str) -> Result<()> {
    parse_url(input, true).map(|_| ())
}
pub(crate) fn normalize_media_url(input: &str) -> Result<String> {
    let url = if let Some(tail) = input.strip_prefix("http://") {
        format!("https://{tail}")
    } else if input.starts_with("//") {
        format!("https:{input}")
    } else {
        input.to_owned()
    };
    validate_media_url(&url)?;
    Ok(url)
}
pub(crate) struct Transport {
    agent: Agent,
    config: Config,
}
impl Transport {
    pub(crate) fn bduss(&self) -> Option<&str> {
        self.config.bduss.as_deref()
    }
    pub fn new(config: Config) -> Result<Self> {
        if config.max_response_bytes < 1024
            || config.max_response_bytes > 4 * 1024 * 1024
            || config.max_download_bytes == 0
            || config.max_download_bytes > 128 * 1024 * 1024
            || config.timeout.is_zero()
            || config.timeout > Duration::from_secs(120)
            || config.bduss.as_ref().is_some_and(|v| {
                v.is_empty()
                    || v.len() > 4096
                    || v.bytes()
                        .any(|b| !b.is_ascii_alphanumeric() && !b"-_=+~".contains(&b))
            })
        {
            return Err(Error::InvalidConfig);
        }
        let agent = Agent::config_builder()
            .https_only(true)
            .max_redirects(0)
            .http_status_as_error(false)
            .proxy(None)
            .timeout_global(Some(config.timeout))
            .timeout_connect(Some(Duration::from_secs(8)))
            .timeout_per_read(Some(Duration::from_secs(15)))
            .max_response_header_size(32 * 1024)
            .max_idle_connections(2)
            .max_idle_connections_per_host(1)
            .user_agent("agent-runtime/0.1.0")
            .build()
            .into();
        Ok(Self { agent, config })
    }
    pub fn api(&self, path: &str, proto: &[u8]) -> Result<Vec<u8>> {
        if !matches!(
            path,
            "/c/f/pb/page?cmd=302001"
                | "/c/f/frs/page?cmd=301001"
                | "/c/f/pb/floor?cmd=302002"
                | "/c/u/feed/userpost?cmd=303002"
                | "/c/f/excellent/personalized?cmd=309264"
        ) {
            return Err(Error::UrlPolicy);
        }
        let url = format!("https://tiebac.baidu.com{path}");
        parse_url(&url, false)?;
        let mut body=b"---*_r1999\r\nContent-Disposition: form-data; name=\"data\"; filename=\"file\"\r\n\r\n".to_vec();
        body.extend_from_slice(proto);
        body.extend_from_slice(b"\r\n---*_r1999--\r\n");
        let mut request = self
            .agent
            .post(&url)
            .header("x_bd_data_type", "protobuf")
            .header("Content-Type", "multipart/form-data; boundary=-*_r1999");
        if let Some(bduss) = &self.config.bduss {
            request = request.header("Cookie", &format!("BDUSS={bduss}"));
        }
        let mut response = request.send(body.as_slice()).map_err(|_| Error::Network)?;
        status(response.status().as_u16())?;
        limited(
            response.body_mut().as_reader(),
            self.config.max_response_bytes,
        )
    }
    pub(crate) fn search_global(&self, fields: &[(&str, String)]) -> Result<Vec<u8>> {
        let query = signed_form(fields, b"36770b1f34c9bbf2e7d1a99d2b82fa9e");
        let query = String::from_utf8(query).map_err(|_| Error::InvalidConfig)?;
        let mut req = self
            .agent
            .get(format!(
                "https://tieba.baidu.com/mo/q/search/thread?{query}"
            ))
            .header("Referer", "https://tieba.baidu.com/f/search/res");
        if let Some(bduss) = &self.config.bduss {
            req = req.header("Cookie", &format!("BDUSS={bduss}"));
        }
        let mut response = req.call().map_err(|_| Error::Network)?;
        status(response.status().as_u16())?;
        limited(
            response.body_mut().as_reader(),
            self.config.max_response_bytes,
        )
    }
    pub(crate) fn search_forum(&self, fields: &[(&str, String)]) -> Result<Vec<u8>> {
        let body = signed_form(fields, b"tiebaclient!!!");
        let mut response = self
            .agent
            .post("https://tiebac.baidu.com/c/s/searchpost")
            .header("Content-Type", "application/x-www-form-urlencoded")
            .send(body.as_slice())
            .map_err(|_| Error::Network)?;
        status(response.status().as_u16())?;
        limited(
            response.body_mut().as_reader(),
            self.config.max_response_bytes,
        )
    }
    pub(crate) fn notification_form(&self, path: &str, page: u32) -> Result<Vec<u8>> {
        if !matches!(
            path,
            "/c/u/feed/replyme" | "/c/u/feed/atme" | "/c/u/feed/agreeme"
        ) {
            return Err(Error::UrlPolicy);
        }
        let bduss = self.config.bduss.as_deref().ok_or(Error::LoginRequired)?;
        let body = notification_body(bduss, page)?;
        let url = format!("https://tiebac.baidu.com{path}");
        parse_url(&url, false)?;
        let mut response = self
            .agent
            .post(&url)
            .header("Content-Type", "application/x-www-form-urlencoded")
            .header("Cookie", &format!("BDUSS={bduss}"))
            .send(body.as_slice())
            .map_err(|_| Error::Network)?;
        status(response.status().as_u16())?;
        limited(
            response.body_mut().as_reader(),
            self.config.max_response_bytes.min(1024 * 1024),
        )
    }
    pub(crate) fn followed_forums(&self) -> Result<Vec<u8>> {
        let bduss = self.config.bduss.as_deref().ok_or(Error::LoginRequired)?;
        let url = "https://tieba.baidu.com/c/f/forum/forumGuide";
        let mut response = self
            .agent
            .post(url)
            .header("Content-Type", "application/x-www-form-urlencoded")
            .header("Subapp-Type", "hybrid")
            .header("Cookie", &format!("BDUSS={bduss}"))
            .send(b"sort_type=3&call_from=3".as_slice())
            .map_err(|_| Error::Network)?;
        status(response.status().as_u16())?;
        limited(
            response.body_mut().as_reader(),
            self.config.max_response_bytes.min(1024 * 1024),
        )
    }
    pub fn download<W: Write + ?Sized>(
        &self,
        resource: &MediaResource,
        writer: &mut W,
        max: u64,
    ) -> Result<DownloadReceipt> {
        if max == 0 || max > self.config.max_download_bytes {
            return Err(Error::Limit);
        }
        let mut url = resource.url.clone();
        for _ in 0..=3 {
            let uri = parse_url(&url, true)?;
            let mut response = self
                .agent
                .get(&url)
                .header("Referer", "https://tieba.baidu.com/")
                .call()
                .map_err(|_| Error::Network)?;
            let code = response.status().as_u16();
            if matches!(code, 301 | 302 | 303 | 307 | 308) {
                let location = response
                    .headers()
                    .get("Location")
                    .and_then(|v| v.to_str().ok())
                    .ok_or(Error::UrlPolicy)?;
                url = if location.starts_with('/') && !location.starts_with("//") {
                    format!(
                        "https://{}{location}",
                        uri.authority().ok_or(Error::UrlPolicy)?
                    )
                } else {
                    normalize_media_url(location)?
                };
                continue;
            }
            status(code)?;
            let mime = response
                .headers()
                .get("Content-Type")
                .and_then(|v| v.to_str().ok())
                .map(str::to_owned);
            if mime.as_ref().is_some_and(|v| v.starts_with("text/html")) {
                return Err(Error::Challenge);
            }
            let length = response
                .headers()
                .get("Content-Length")
                .and_then(|v| v.to_str().ok())
                .and_then(|v| v.parse::<u64>().ok());
            if length.is_some_and(|n| n > max) {
                return Err(Error::Limit);
            }
            let n = copy_limited(response.body_mut().as_reader(), writer, max)?;
            if length.is_some_and(|l| l != n) {
                return Err(Error::Schema("incomplete media transfer"));
            }
            return Ok(DownloadReceipt { bytes: n, mime });
        }
        Err(Error::UrlPolicy)
    }
}
pub(crate) fn percent_encode(value: &str) -> String {
    let mut out = String::new();
    for b in value.bytes() {
        if b.is_ascii_alphanumeric() || b"-_.~".contains(&b) {
            out.push(b as char)
        } else {
            out.push_str(&format!("%{b:02X}"));
        }
    }
    out
}
pub(crate) fn form_bytes(fields: &[(&str, String)]) -> Vec<u8> {
    fields
        .iter()
        .map(|(k, v)| format!("{}={}", percent_encode(k), percent_encode(v)))
        .collect::<Vec<_>>()
        .join("&")
        .into_bytes()
}
pub(crate) fn signed_form(fields: &[(&str, String)], salt: &[u8]) -> Vec<u8> {
    use md5::{Digest, Md5};
    let mut fields = fields.to_vec();
    fields.sort_unstable_by(|a, b| a.0.cmp(b.0));
    let mut hash = Md5::new();
    for (k, v) in &fields {
        hash.update(format!("{k}={v}").as_bytes());
    }
    hash.update(salt);
    fields.push(("sign", format!("{:x}", hash.finalize())));
    form_bytes(&fields)
}
fn notification_body(bduss: &str, page: u32) -> Result<Vec<u8>> {
    use md5::{Digest, Md5};
    if page == 0 || page > 100000 {
        return Err(Error::InvalidPage);
    }
    let mut fields = vec![
        ("BDUSS", bduss.to_owned()),
        ("_client_version", "22.12.1.0".into()),
        ("pn", page.to_string()),
    ];
    fields.sort_unstable_by(|a, b| a.0.cmp(b.0));
    let mut hash = Md5::new();
    for (key, value) in &fields {
        hash.update(format!("{key}={value}").as_bytes());
    }
    hash.update(b"tiebaclient!!!");
    let sign = format!("{:x}", hash.finalize());
    fields.push(("sign", sign));
    fn encode(value: &str) -> String {
        let mut out = String::new();
        for b in value.bytes() {
            if b.is_ascii_alphanumeric() || b"-_.~".contains(&b) {
                out.push(b as char);
            } else {
                out.push_str(&format!("%{b:02X}"));
            }
        }
        out
    }
    Ok(fields
        .iter()
        .map(|(key, value)| format!("{}={}", encode(key), encode(value)))
        .collect::<Vec<_>>()
        .join("&")
        .into_bytes())
}
fn status(code: u16) -> Result<()> {
    if code == 200 {
        Ok(())
    } else if matches!(code, 403 | 412 | 429) {
        Err(Error::Challenge)
    } else {
        Err(Error::HttpStatus(code))
    }
}
fn limited(reader: impl Read, max: usize) -> Result<Vec<u8>> {
    let mut out = Vec::new();
    reader
        .take(max as u64 + 1)
        .read_to_end(&mut out)
        .map_err(|_| Error::Network)?;
    if out.len() > max {
        return Err(Error::Limit);
    }
    Ok(out)
}
fn copy_limited<W: Write + ?Sized>(mut reader: impl Read, out: &mut W, max: u64) -> Result<u64> {
    let mut b = [0; 32768];
    let mut total = 0;
    loop {
        let limit = (max - total).min(b.len() as u64) as usize;
        if limit == 0 {
            let mut check = [0];
            if reader.read(&mut check).map_err(|_| Error::Network)? == 0 {
                return Ok(total);
            }
            return Err(Error::Limit);
        }
        let n = reader.read(&mut b[..limit]).map_err(|_| Error::Network)?;
        if n == 0 {
            return Ok(total);
        }
        out.write_all(&b[..n]).map_err(|_| Error::Io)?;
        total += n as u64;
    }
}

#[cfg(test)]
mod tests {
    #[test]
    fn pc_search_signature_matches_independent_utf8_vector() {
        let fields = [
            ("word", "猫 & Rust".into()),
            ("pn", "1".into()),
            ("rn", "20".into()),
            ("st", "0".into()),
            ("tt", "1".into()),
            ("subapp_type", "pc".into()),
            ("_client_type", "20".into()),
        ];
        let form = String::from_utf8(super::signed_form(
            &fields,
            b"36770b1f34c9bbf2e7d1a99d2b82fa9e",
        ))
        .unwrap();
        assert_eq!(
            form,
            "_client_type=20&pn=1&rn=20&st=0&subapp_type=pc&tt=1&word=%E7%8C%AB%20%26%20Rust&sign=7883f1129e824590bc707370be579c2e"
        );
    }
    use super::*;
    #[test]
    fn url_policy_and_secrets() {
        for url in ["https://imgsrc.baidu.com/a", "https://tiebapic.baidu.com/a"] {
            assert!(validate_media_url(url).is_ok());
        }
        for url in [
            "http://imgsrc.baidu.com/a",
            "https://localhost/a",
            "https://imgsrc.baidu.com.evil.invalid/a",
            "https://x@imgsrc.baidu.com/a",
            "https://imgsrc.baidu.com:8080/a",
        ] {
            assert!(validate_media_url(url).is_err());
        }
        let cfg = Config {
            bduss: Some("secret-token".into()),
            ..Default::default()
        };
        assert!(!format!("{cfg:?}").contains("secret-token"));
        let cfg = Config {
            bduss: Some("synthetic~opaque-token".into()),
            ..Default::default()
        };
        assert!(Transport::new(cfg).is_ok());
        for unsafe_value in ["value; other=cookie", "value\r\nInjected: header"] {
            let cfg = Config {
                bduss: Some(unsafe_value.into()),
                ..Default::default()
            };
            assert!(matches!(Transport::new(cfg), Err(Error::InvalidConfig)));
        }
    }
    #[test]
    fn body_and_download_limits() {
        assert!(limited(&b"12345"[..], 4).is_err());
        let mut output = Vec::new();
        assert!(copy_limited(&b"12345"[..], &mut output, 4).is_err());
        assert_eq!(output, b"1234");
    }
    #[test]
    fn notification_form_preserves_credential_bytes_and_cannot_call_write_paths() {
        let body =
            String::from_utf8(notification_body("synthetic~token+suffix=", 2).unwrap()).unwrap();
        assert!(body.contains("BDUSS=synthetic~token%2Bsuffix%3D"));
        assert!(body.contains("_client_version=22.12.1.0"));
        assert!(body.contains("pn=2"));
        assert_eq!(body.split("&sign=").nth(1).unwrap().len(), 32);
        assert!(notification_body("not-sent", 0).is_err());
        let transport = Transport::new(Config::default()).unwrap();
        assert!(matches!(
            transport.notification_form("/c/c/post/agree", 1),
            Err(Error::UrlPolicy)
        ));
        assert!(matches!(
            transport.notification_form("/c/u/feed/atme", 1),
            Err(Error::LoginRequired)
        ));
    }
}
