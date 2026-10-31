// SPDX-License-Identifier: AGPL-3.0-only
//! Resolve QQ rich-media references through fixed OIDB methods, then stream CDN bytes.
use crate::{
    Client, Result,
    directory::{num, oidb, oidb_body, text},
    rich::{MediaKind, MediaLocator, decode64},
    wire::{self, Fields},
};
use std::{
    io::{Read, Write},
    time::Duration,
};

pub fn validate_media_url(url: &str) -> Result<()> {
    if url.len() > 8192 || url.chars().any(char::is_control) {
        return Err("invalid QQ media URL".into());
    }
    let uri: ureq::http::Uri = url.parse().map_err(|_| "invalid QQ media URI")?;
    if !matches!(uri.scheme_str(), Some("https" | "http"))
        || uri.authority().is_none_or(|a| a.as_str().contains('@'))
        || !matches!(uri.port_u16(), None | Some(80 | 443))
    {
        return Err("QQ media URI policy".into());
    }
    let host = uri.host().ok_or("QQ media host missing")?;
    if !(host == "qpic.cn"
        || host.ends_with(".qpic.cn")
        || host == "qq.com"
        || host.ends_with(".qq.com")
        || host == "qq.com.cn"
        || host.ends_with(".qq.com.cn"))
    {
        return Err("QQ media host is not an allowed CDN".into());
    }
    Ok(())
}
pub fn download_to_writer(url: &str, out: &mut dyn Write, max: u64) -> Result<u64> {
    validate_media_url(url)?;
    download(url, out, max)
}
fn validate_native_file_url(url: &str) -> Result<()> {
    if validate_media_url(url).is_ok() {
        return Ok(());
    }
    let uri: ureq::http::Uri = url.parse().map_err(|_| "QQ file URI invalid")?;
    if url.len() > 8192
        || url.chars().any(char::is_control)
        || !matches!(uri.scheme_str(), Some("http" | "https"))
        || uri.authority().is_none_or(|a| a.as_str().contains('@'))
        || !matches!(uri.port_u16(), None | Some(80 | 443))
    {
        return Err("QQ file URI policy".into());
    }
    let ip: std::net::Ipv4Addr = uri
        .host()
        .ok_or("QQ file host missing")?
        .parse()
        .map_err(|_| "QQ file server is not a QQ CDN or public IPv4")?;
    if !crate::upload::public_ip(ip) {
        return Err("QQ file server is not public".into());
    }
    Ok(())
}
/// Only for a URL obtained from the authenticated native file download method.
pub fn download_file_to_writer(url: &str, out: &mut dyn Write, max: u64) -> Result<u64> {
    validate_native_file_url(url)?;
    download(url, out, max)
}
fn download(url: &str, out: &mut dyn Write, max: u64) -> Result<u64> {
    if max == 0 || max > 128 * 1024 * 1024 {
        return Err("QQ media download budget invalid".into());
    }
    let agent = ureq::Agent::config_builder()
        .proxy(None)
        .max_redirects(0)
        .timeout_global(Some(Duration::from_secs(40)))
        .build()
        .new_agent();
    let mut response = agent
        .get(url)
        .call()
        .map_err(|_| "QQ media download failed")?;
    if response.status() != 200 {
        return Err("QQ CDN rejected download".into());
    }
    let mut reader = response.body_mut().as_reader();
    let mut count = 0u64;
    let mut buf = [0; 16384];
    loop {
        let n = reader
            .read(&mut buf)
            .map_err(|_| "QQ media stream failed")?;
        if n == 0 {
            break;
        }
        count = count
            .checked_add(n as u64)
            .ok_or("QQ media length overflow")?;
        if count > max {
            return Err("QQ media exceeds download budget".into());
        }
        out.write_all(&buf[..n])
            .map_err(|_| "QQ media cache write failed")?
    }
    if count == 0 {
        return Err("QQ media is empty".into());
    }
    Ok(count)
}
impl Client {
    pub fn media_url(&mut self, media: &MediaLocator) -> Result<String> {
        if !self.is_online() {
            return Err("QQ session is not registered".into());
        }
        if media.kind == MediaKind::File {
            return self.file_url(media);
        }
        if let Some(url) = &media.url {
            validate_media_url(url)?;
            return Ok(url.clone());
        }
        let encoded = media.node.as_deref().ok_or("QQ media index unavailable")?;
        let node = decode64(encoded)?;
        Fields::parse(&node)?;
        let (business, request, command) = match (&media.kind, media.group.is_some()) {
            (MediaKind::Image, true) => (1, 1, 0x11c4),
            (MediaKind::Image, false) => (1, 1, 0x11c5),
            (MediaKind::Voice, true) => (3, 4, 0x126e),
            (MediaKind::Voice, false) => (3, 1, 0x126d),
            (MediaKind::Video, true) => (2, 1, 0x11e9),
            (MediaKind::Video, false) => (2, 34, 0x11e9),
            _ => return Err("QQ file requires its native file locator".into()),
        };
        let mut common = Vec::new();
        wire::number(1, request, &mut common);
        wire::number(2, 200, &mut common);
        let mut scene = Vec::new();
        wire::number(101, if business == 3 { 1 } else { 2 }, &mut scene);
        wire::number(102, business, &mut scene);
        if let Some(group) = media.group {
            let mut g = Vec::new();
            wire::number(1, group.into(), &mut g);
            wire::number(200, 2, &mut scene);
            wire::bytes(202, &g, &mut scene)
        } else {
            let mut c = Vec::new();
            wire::number(1, 2, &mut c);
            wire::bytes(2, self.session().uid.as_bytes(), &mut c);
            wire::number(200, 1, &mut scene);
            wire::bytes(201, &c, &mut scene)
        }
        let mut client = Vec::new();
        wire::number(1, 2, &mut client);
        let mut head = Vec::new();
        wire::bytes(1, &common, &mut head);
        wire::bytes(2, &scene, &mut head);
        wire::bytes(3, &client, &mut head);
        let mut extension = Vec::new();
        wire::bytes(2, &[], &mut extension);
        let mut download = Vec::new();
        wire::bytes(1, &node, &mut download);
        wire::bytes(2, &extension, &mut download);
        let mut body = Vec::new();
        wire::bytes(1, &head, &mut body);
        wire::bytes(3, &download, &mut body);
        let raw = self.request(
            &format!("OidbSvcTrpcTcp.0x{command:x}_200"),
            &oidb(command, 200, &body, true),
        )?;
        let f = Fields::parse(oidb_body(&raw, command, 200)?)?;
        if let Some(head) = f.data(1) {
            let head = Fields::parse(head)?;
            let status = num(&head, 2)?;
            if status != 0 {
                return Err(format!("QQ media download rejected ({status})"));
            }
        }
        let down = Fields::parse(f.data(3).ok_or("QQ download response missing")?)?;
        let info = Fields::parse(down.data(3).ok_or("QQ CDN locator missing")?)?;
        let host = text(&info, 1, 255)?;
        let path = text(&info, 2, 4096)?;
        let key = text(&down, 1, 4096)?;
        if host.contains(['/', ':', '@'])
            || !path.starts_with('/')
            || !(key.is_empty() || key.starts_with(['?', '&']))
        {
            return Err("QQ CDN locator malformed".into());
        }
        let url = format!("https://{host}{path}{key}");
        validate_media_url(&url)?;
        Ok(url)
    }
    fn file_url(&mut self, media: &MediaLocator) -> Result<String> {
        let uuid = media
            .file_uuid
            .as_deref()
            .filter(|s| !s.is_empty() && s.len() <= 2048)
            .ok_or("QQ file UUID missing")?;
        let url = if let Some(code) = media.group {
            let mut d = vec![];
            wire::number(1, code.into(), &mut d);
            wire::number(2, 7, &mut d);
            wire::number(3, 102, &mut d);
            wire::bytes(4, uuid.as_bytes(), &mut d);
            let mut b = vec![];
            wire::bytes(3, &d, &mut b);
            let raw = self.request("OidbSvcTrpcTcp.0x6d6_2", &oidb(0x6d6, 2, &b, true))?;
            let f = Fields::parse(oidb_body(&raw, 0x6d6, 2)?)?;
            let d = Fields::parse(f.data(3).ok_or("QQ group file result missing")?)?;
            if num(&d, 1)? != 0 {
                return Err("QQ group file download rejected".into());
            }
            let host = text(&d, 4, 255)?;
            let token = d
                .data(6)
                .filter(|s| !s.is_empty() && s.len() <= 2048)
                .ok_or("QQ group file URL missing")?;
            let token = token.iter().map(|b| format!("{b:02x}")).collect::<String>();
            if host.contains(['/', ':', '@']) {
                return Err("QQ group file host invalid".into());
            }
            format!("https://{host}:443/ftn_handler/{token}/?fname=")
        } else {
            let hash = media
                .file_hash
                .as_deref()
                .filter(|s| !s.is_empty() && s.len() <= 2048)
                .ok_or("QQ private file hash missing")?;
            let mut d = vec![];
            wire::bytes(10, self.session().uid.as_bytes(), &mut d);
            wire::bytes(20, uuid.as_bytes(), &mut d);
            wire::number(30, 2, &mut d);
            wire::bytes(60, hash.as_bytes(), &mut d);
            let mut b = vec![];
            for (t, n) in [(1, 1200), (2, 1), (101, 3), (102, 103), (200, 1)] {
                wire::number(t, n, &mut b)
            }
            wire::bytes(14, &d, &mut b);
            wire::bytes(99999, &[0xc0, 0x85, 0x2c, 1], &mut b);
            let raw = self.request("OidbSvcTrpcTcp.0xe37_1200", &oidb(0xe37, 1200, &b, false))?;
            let f = Fields::parse(oidb_body(&raw, 0xe37, 1200)?)?;
            let d = Fields::parse(f.data(14).ok_or("QQ private file result missing")?)?;
            let r = Fields::parse(d.data(30).ok_or("QQ private file URL missing")?)?;
            let host = text(&r, 20, 255)?;
            let port = num(&r, 40)?;
            let path = text(&r, 50, 4096)?;
            if host.contains(['/', ':', '@']) || !path.starts_with('/') || !matches!(port, 80 | 443)
            {
                return Err("QQ private file locator invalid".into());
            }
            format!("http://{host}:{port}{path}&isthumb=0")
        };
        validate_native_file_url(&url)?;
        Ok(url)
    }
}
