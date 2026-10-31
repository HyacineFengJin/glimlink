// SPDX-License-Identifier: AGPL-3.0-only
//! Native NTQQ image negotiation and single-stream Highway upload.
//! Wire reference: LagrangeGo f7f8abfaa7f1, packets/oidb/*image_upload.go,
//! client/internal/highway/bdh.go. Upload negotiation never sends a chat message.
use crate::{
    Client, Result, SendTarget,
    directory::{num, oidb, oidb_body, text},
    wire::{self, Fields, Value},
};
use md5::{Digest, Md5};
use serde::{Deserialize, Serialize};
use sha1::Sha1;
use std::{
    fs::{File, OpenOptions},
    io::{Read, Seek, SeekFrom, Write},
    net::{Ipv4Addr, SocketAddr, TcpStream},
    os::unix::fs::OpenOptionsExt,
    path::Path,
    time::{Duration, Instant},
};
const BLOCK: usize = 1024 * 1024;
pub const MAX_IMAGE_BYTES: u64 = 20 * 1024 * 1024;
fn hex(data: &[u8]) -> String {
    data.iter().map(|v| format!("{v:02x}")).collect()
}
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ImageInfo {
    pub bytes: u64,
    pub md5: String,
    pub sha1: String,
    pub width: u32,
    pub height: u32,
    pub format: u32,
    pub extension: String,
}
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct NativeImage {
    pub msg_info: String,
    pub compat: String,
    pub group: bool,
}
fn open(path: &Path) -> Result<File> {
    let f = OpenOptions::new()
        .read(true)
        .custom_flags(0x20000 | 0x800)
        .open(path)
        .map_err(|_| "QQ image resource cannot be opened")?;
    let m = f.metadata().map_err(|_| "QQ image metadata unavailable")?;
    if !m.is_file() || m.len() == 0 || m.len() > MAX_IMAGE_BYTES {
        return Err("QQ image must be a regular resource of 1..20 MiB".into());
    }
    Ok(f)
}
fn dimensions(data: &[u8]) -> Result<(u32, u32, u32, &'static str)> {
    if data.starts_with(b"\x89PNG\r\n\x1a\n") && data.len() >= 24 && &data[12..16] == b"IHDR" {
        return Ok((
            u32::from_be_bytes(data[16..20].try_into().unwrap()),
            u32::from_be_bytes(data[20..24].try_into().unwrap()),
            1001,
            "png",
        ));
    }
    if (data.starts_with(b"GIF87a") || data.starts_with(b"GIF89a")) && data.len() >= 10 {
        return Ok((
            u16::from_le_bytes(data[6..8].try_into().unwrap()).into(),
            u16::from_le_bytes(data[8..10].try_into().unwrap()).into(),
            2000,
            "gif",
        ));
    }
    if data.starts_with(b"\xff\xd8") {
        let mut at = 2;
        while at + 4 <= data.len() {
            if data[at] != 255 {
                return Err("invalid JPEG marker".into());
            }
            while data.get(at) == Some(&255) {
                at += 1
            }
            let marker = *data.get(at).ok_or("truncated JPEG")?;
            at += 1;
            if matches!(marker, 0xd8 | 0xd9 | 0x01 | 0xd0..=0xd7) {
                continue;
            }
            let len = u16::from_be_bytes(
                data.get(at..at + 2)
                    .ok_or("truncated JPEG segment")?
                    .try_into()
                    .unwrap(),
            ) as usize;
            if len < 2 {
                return Err("invalid JPEG segment".into());
            }
            if matches!(marker,0xc0..=0xc3|0xc5..=0xc7|0xc9..=0xcb|0xcd..=0xcf) {
                let b = data
                    .get(at + 2..at + 7)
                    .ok_or("JPEG dimensions truncated")?;
                return Ok((
                    u16::from_be_bytes(b[3..5].try_into().unwrap()).into(),
                    u16::from_be_bytes(b[1..3].try_into().unwrap()).into(),
                    1000,
                    "jpg",
                ));
            }
            at = at.checked_add(len).ok_or("JPEG size overflow")?
        }
    }
    if data.starts_with(b"RIFF") && data.get(8..12) == Some(b"WEBP") {
        if data.get(12..16) == Some(b"VP8X") && data.len() >= 30 {
            let n = |b: &[u8]| u32::from(b[0]) | (u32::from(b[1]) << 8) | (u32::from(b[2]) << 16);
            return Ok((n(&data[24..27]) + 1, n(&data[27..30]) + 1, 1002, "webp"));
        }
        if data.get(12..16) == Some(b"VP8 ")
            && data.len() >= 30
            && data.get(23..26) == Some(b"\x9d\x01\x2a")
        {
            return Ok((
                (u16::from_le_bytes(data[26..28].try_into().unwrap()) & 0x3fff).into(),
                (u16::from_le_bytes(data[28..30].try_into().unwrap()) & 0x3fff).into(),
                1002,
                "webp",
            ));
        }
        if data.get(12..16) == Some(b"VP8L") && data.len() >= 25 && data[20] == 0x2f {
            let b = u32::from_le_bytes(data[21..25].try_into().unwrap());
            return Ok(((b & 0x3fff) + 1, ((b >> 14) & 0x3fff) + 1, 1002, "webp"));
        }
    }
    Err("QQ image needs PNG, JPEG, GIF or WebP with bounded header dimensions".into())
}
fn inspect(f: &mut File) -> Result<ImageInfo> {
    f.seek(SeekFrom::Start(0))
        .map_err(|_| "QQ image seek failed")?;
    let mut md5 = Md5::new();
    let mut sha = Sha1::new();
    let mut prefix = Vec::new();
    let mut bytes = 0u64;
    let mut block = [0; 16384];
    loop {
        let n = f.read(&mut block).map_err(|_| "QQ image read failed")?;
        if n == 0 {
            break;
        }
        bytes += n as u64;
        if bytes > MAX_IMAGE_BYTES {
            return Err("QQ image exceeds20 MiB".into());
        }
        md5.update(&block[..n]);
        sha.update(&block[..n]);
        if prefix.len() < 128 * 1024 {
            prefix.extend_from_slice(&block[..n.min(128 * 1024 - prefix.len())])
        }
    }
    let (width, height, format, extension) = dimensions(&prefix)?;
    if width == 0
        || height == 0
        || width > 32768
        || height > 32768
        || u64::from(width) * u64::from(height) > 100_000_000
    {
        return Err("QQ image dimensions exceed bounds".into());
    }
    f.seek(SeekFrom::Start(0))
        .map_err(|_| "QQ image seek failed")?;
    Ok(ImageInfo {
        bytes,
        md5: hex(&md5.finalize()),
        sha1: hex(&sha.finalize()),
        width,
        height,
        format,
        extension: extension.into(),
    })
}
pub fn inspect_image(path: &Path) -> Result<ImageInfo> {
    inspect(&mut open(path)?)
}
fn head(target: &SendTarget, own_uid: &str) -> Result<(u32, bool, Vec<u8>)> {
    let mut common = vec![];
    wire::number(1, 1, &mut common);
    wire::number(2, 100, &mut common);
    let mut scene = vec![];
    wire::number(101, 2, &mut scene);
    wire::number(102, 1, &mut scene);
    let group = matches!(target, SendTarget::Group { .. });
    wire::number(200, if group { 2 } else { 1 }, &mut scene);
    match target {
        SendTarget::Group { code } => {
            if *code == 0 {
                return Err("QQ group empty".into());
            }
            let mut g = vec![];
            wire::number(1, (*code).into(), &mut g);
            wire::bytes(202, &g, &mut scene)
        }
        _ => {
            let uid = match target {
                SendTarget::Private { uid } => uid.as_str(),
                _ => own_uid,
            };
            if !crate::rich::valid_uid(uid) {
                return Err("QQ image target UID invalid".into());
            }
            let mut c = vec![];
            wire::number(1, 2, &mut c);
            wire::bytes(2, uid.as_bytes(), &mut c);
            wire::bytes(201, &c, &mut scene)
        }
    }
    let mut h = vec![];
    wire::bytes(1, &common, &mut h);
    wire::bytes(2, &scene, &mut h);
    wire::bytes(3, &[8, 2], &mut h);
    Ok((if group { 0x11c4 } else { 0x11c5 }, group, h))
}
pub(crate) fn image_request(
    target: &SendTarget,
    uid: &str,
    info: &ImageInfo,
    random: u32,
) -> Result<(u32, bool, Vec<u8>)> {
    let (cmd, group, h) = head(target, uid)?;
    let mut typ = vec![];
    wire::number(1, 1, &mut typ);
    wire::number(2, info.format.into(), &mut typ);
    let mut fi = vec![];
    wire::number(1, info.bytes, &mut fi);
    wire::bytes(2, info.md5.as_bytes(), &mut fi);
    wire::bytes(3, info.sha1.as_bytes(), &mut fi);
    wire::bytes(
        4,
        format!("{}.{}", info.md5, info.extension).as_bytes(),
        &mut fi,
    );
    wire::bytes(5, &typ, &mut fi);
    wire::number(6, info.width.into(), &mut fi);
    wire::number(7, info.height.into(), &mut fi);
    wire::number(9, 1, &mut fi);
    let mut upload_info = vec![];
    wire::bytes(1, &fi, &mut upload_info);
    let mut upload = vec![];
    wire::bytes(1, &upload_info, &mut upload);
    wire::number(2, 1, &mut upload);
    wire::number(4, random.into(), &mut upload);
    wire::number(5, if group { 2 } else { 1 }, &mut upload);
    let mut ext = vec![];
    let mut pic = vec![];
    wire::bytes(2, "[图片]".as_bytes(), &mut pic);
    let mut pdata = vec![];
    wire::bytes(9, "[图片]".as_bytes(), &mut pdata);
    wire::bytes(12, &pdata, &mut pic);
    wire::bytes(1, &pic, &mut ext);
    wire::bytes(2, &[0x1a, 0], &mut ext);
    wire::bytes(3, &[0x5a, 0, 0x62, 0, 0x6a, 0], &mut ext);
    wire::bytes(6, &ext, &mut upload);
    let mut body = vec![];
    wire::bytes(1, &h, &mut body);
    wire::bytes(2, &upload, &mut body);
    Ok((cmd, group, body))
}
fn unhex(s: &str, size: usize) -> Result<Vec<u8>> {
    if s.len() != size * 2 || !s.is_ascii() {
        return Err("QQ upload checksum malformed".into());
    }
    (0..s.len())
        .step_by(2)
        .map(|n| {
            u8::from_str_radix(&s[n..n + 2], 16).map_err(|_| "QQ upload checksum malformed".into())
        })
        .collect()
}
pub(crate) struct Highway {
    ticket: Vec<u8>,
    addresses: Vec<SocketAddr>,
}
pub(crate) fn public_ip(ip: Ipv4Addr) -> bool {
    let [a, b, _, _] = ip.octets();
    !ip.is_unspecified()
        && !ip.is_loopback()
        && !ip.is_private()
        && !ip.is_link_local()
        && !ip.is_multicast()
        && !ip.is_broadcast()
        && a != 0
        && a < 224
        && !(a == 100 && (64..=127).contains(&b))
        && !(a == 192 && b == 0)
        && !(a == 198 && (b == 18 || b == 19))
}
impl Client {
    pub(crate) fn highway(&mut self) -> Result<Highway> {
        let mut body = vec![];
        for (t, n) in [(3, 16), (4, 1), (6, 3), (9, 2), (10, 9), (11, 8)] {
            wire::number(t, n, &mut body)
        }
        wire::bytes(5, hex(&self.session().tgt()?).as_bytes(), &mut body);
        wire::bytes(7, &[1, 5, 10, 21], &mut body);
        wire::bytes(15, b"1.0.1", &mut body);
        let mut req = vec![];
        wire::bytes(0x501, &body, &mut req);
        let raw = self.request("HttpConn.0x6ff_501", &req)?;
        let f = Fields::parse(&raw)?;
        let c = Fields::parse(f.data(0x501).ok_or("QQ Highway response missing")?)?;
        let ticket = c
            .data(1)
            .filter(|b| !b.is_empty() && b.len() <= 8192)
            .ok_or("QQ Highway ticket missing")?
            .to_vec();
        let mut addresses = vec![];
        for (t, v) in &c.0 {
            if *t != 3 {
                continue;
            }
            let Value::Bytes(b) = v else { continue };
            let info = Fields::parse(b)?;
            if num(&info, 1)? != 1 {
                continue;
            }
            for (t, v) in info.0 {
                if t != 2 {
                    continue;
                }
                let Value::Bytes(b) = v else { continue };
                let a = Fields::parse(b)?;
                let ip =
                    a.0.iter()
                        .find_map(|(t, v)| {
                            if *t == 2 {
                                match v {
                                    Value::Fixed32(ip) => Some(Ipv4Addr::from(ip.to_le_bytes())),
                                    _ => None,
                                }
                            } else {
                                None
                            }
                        })
                        .ok_or("QQ Highway address missing")?;
                let port = u16::try_from(num(&a, 3)?).map_err(|_| "QQ Highway port invalid")?;
                if !public_ip(ip) || port == 0 {
                    return Err("QQ Highway requires a public address".into());
                }
                if addresses.len() < 16 {
                    addresses.push(SocketAddr::from((ip, port)))
                }
            }
        }
        if addresses.is_empty() {
            return Err("QQ Highway server unavailable".into());
        }
        Ok(Highway { ticket, addresses })
    }
    /// Negotiates upload and streams bytes; caller owns the durable one-shot operation.
    pub fn upload_image(
        &mut self,
        target: &SendTarget,
        path: &Path,
        expected: &ImageInfo,
        random: u32,
    ) -> Result<NativeImage> {
        if !self.is_online() {
            return Err("QQ session is not registered".into());
        }
        let mut file = open(path)?;
        if inspect(&mut file)? != *expected {
            return Err("QQ image resource changed after preparing the operation".into());
        }
        let (cmd, group, body) = image_request(target, &self.session().uid, expected, random)?;
        let raw = self.request(
            &format!("OidbSvcTrpcTcp.0x{cmd:x}_100"),
            &oidb(cmd, 100, &body, true),
        )?;
        let f = Fields::parse(oidb_body(&raw, cmd, 100)?)?;
        let rh = Fields::parse(f.data(1).ok_or("QQ upload response head missing")?)?;
        if num(&rh, 2)? != 0 {
            return Err(format!("QQ image negotiation rejected ({})", num(&rh, 2)?));
        }
        let up = Fields::parse(f.data(2).ok_or("QQ upload response missing")?)?;
        let msg = up
            .data(6)
            .filter(|b| !b.is_empty() && b.len() <= 65536)
            .ok_or("QQ image MsgInfo missing")?;
        let compat = up
            .data(8)
            .filter(|b| !b.is_empty() && b.len() <= 65536)
            .ok_or("QQ image compatibility metadata missing")?;
        Fields::parse(compat)?;
        let mf = Fields::parse(msg)?;
        let first = Fields::parse(mf.data(1).ok_or("QQ image index missing")?)?;
        let index = Fields::parse(first.data(1).ok_or("QQ image node missing")?)?;
        let fi = Fields::parse(index.data(1).ok_or("QQ image metadata missing")?)?;
        if u64::from(num(&fi, 1)?) != expected.bytes
            || text(&fi, 2, 64)? != expected.md5
            || text(&fi, 3, 64)? != expected.sha1
        {
            return Err("QQ upload response checksum identity mismatch".into());
        }
        let ukey = text(&up, 1, 16384)?;
        if !ukey.is_empty() {
            let hw = self.highway()?;
            let mut ext = vec![];
            wire::bytes(1, text(&index, 2, 2048)?.as_bytes(), &mut ext);
            wire::bytes(2, ukey.as_bytes(), &mut ext);
            let mut network = vec![];
            for (t, v) in &up.0 {
                if *t == 3 {
                    if let Value::Bytes(b) = v {
                        let a = Fields::parse(b)?;
                        let ip = Ipv4Addr::from(num(&a, 1)?.to_le_bytes());
                        if !public_ip(ip) {
                            return Err("QQ upload address not public".into());
                        }
                        let mut d = vec![];
                        wire::number(1, 1, &mut d);
                        wire::bytes(2, ip.to_string().as_bytes(), &mut d);
                        let mut n = vec![];
                        wire::bytes(1, &d, &mut n);
                        wire::number(2, num(&a, 2)?.into(), &mut n);
                        wire::bytes(1, &n, &mut network)
                    }
                }
            }
            wire::bytes(5, &network, &mut ext);
            for (t, v) in &mf.0 {
                if *t == 1 {
                    if let Value::Bytes(b) = v {
                        wire::bytes(6, b, &mut ext)
                    }
                }
            }
            wire::number(10, BLOCK as u64, &mut ext);
            let mut hash = vec![];
            wire::bytes(1, &unhex(&expected.sha1, 20)?, &mut hash);
            wire::bytes(11, &hash, &mut ext);
            self.highway_stream(
                &mut file,
                expected,
                &hw,
                &ext,
                if group { 1004 } else { 1003 },
                random,
            )?;
        }
        use base64::Engine;
        Ok(NativeImage {
            msg_info: base64::engine::general_purpose::STANDARD.encode(msg),
            compat: base64::engine::general_purpose::STANDARD.encode(compat),
            group,
        })
    }
    pub(crate) fn highway_stream(
        &self,
        file: &mut File,
        image: &ImageInfo,
        hw: &Highway,
        ext: &[u8],
        command: u32,
        sequence: u32,
    ) -> Result<()> {
        let mut stream = None;
        for address in &hw.addresses {
            if let Ok(s) = TcpStream::connect_timeout(address, Duration::from_secs(5)) {
                stream = Some(s);
                break;
            }
        }
        let mut stream = stream.ok_or("QQ Highway connection failed")?;
        stream
            .set_read_timeout(Some(Duration::from_secs(15)))
            .map_err(|_| "QQ Highway timeout setup")?;
        stream
            .set_write_timeout(Some(Duration::from_secs(15)))
            .map_err(|_| "QQ Highway timeout setup")?;
        let deadline = Instant::now() + Duration::from_secs(40);
        let mut block = vec![0; BLOCK];
        let mut offset = 0;
        let mut ticket = hw.ticket.clone();
        let sum = unhex(&image.md5, 16)?;
        let tgt = self.session().tgt()?;
        while offset < image.bytes {
            if Instant::now() >= deadline {
                return Err("QQ Highway upload deadline reached".into());
            }
            let length = BLOCK.min((image.bytes - offset) as usize);
            file.read_exact(&mut block[..length])
                .map_err(|_| "QQ upload resource truncated")?;
            let head = block_head(
                self.session().app_profile,
                self.session().uin,
                sequence.wrapping_add((offset / BLOCK as u64) as u32),
                command,
                image.bytes,
                offset,
                &block[..length],
                &sum,
                &ticket,
                &tgt,
                ext,
            );
            let frame = frame(&head, &block[..length]);
            stream
                .write_all(&frame)
                .map_err(|_| "QQ Highway write failed")?;
            let response = read_frame(&mut stream)?;
            let rf = Fields::parse(&response)?;
            if num(&rf, 3)? != 0 {
                return Err(format!("QQ Highway rejected block ({})", num(&rf, 3)?));
            }
            if let Some(seg) = rf.data(2) {
                let seg = Fields::parse(seg)?;
                if num(&seg, 5)? != 0 {
                    return Err("QQ Highway segment rejected".into());
                }
                if let Some(v) = seg.data(6) {
                    if !v.is_empty() && v.len() <= 8192 {
                        ticket = v.to_vec()
                    }
                }
            }
            offset += length as u64;
        }
        Ok(())
    }
}
fn block_head(
    profile: crate::AppProfile,
    uin: u32,
    sequence: u32,
    command: u32,
    size: u64,
    offset: u64,
    block: &[u8],
    md5: &[u8],
    ticket: &[u8],
    tgt: &[u8],
    ext: &[u8],
) -> Vec<u8> {
    let mut base = vec![];
    wire::number(1, 1, &mut base);
    wire::bytes(2, uin.to_string().as_bytes(), &mut base);
    wire::bytes(3, b"PicUp.DataUp", &mut base);
    for (t, n) in [
        (4, sequence.into()),
        (5, 0),
        (6, u64::from(profile.info().sub_app_id)),
        (7, 16),
        (8, command.into()),
    ] {
        wire::number(t, n, &mut base)
    }
    let mut seg = vec![];
    for (t, n) in [
        (1, 0),
        (2, size),
        (3, offset),
        (4, block.len() as u64),
        (5, 0),
    ] {
        wire::number(t, n, &mut seg)
    }
    wire::bytes(6, ticket, &mut seg);
    wire::bytes(8, &Md5::digest(block), &mut seg);
    wire::bytes(9, md5, &mut seg);
    wire::number(10, 0, &mut seg);
    wire::number(13, 0, &mut seg);
    let mut login = vec![];
    wire::number(1, 8, &mut login);
    wire::bytes(2, tgt, &mut login);
    wire::number(3, profile.info().app_id.into(), &mut login);
    let mut head = vec![];
    wire::bytes(1, &base, &mut head);
    wire::bytes(2, &seg, &mut head);
    wire::bytes(3, ext, &mut head);
    wire::bytes(5, &login, &mut head);
    head
}
fn frame(head: &[u8], body: &[u8]) -> Vec<u8> {
    let mut out = Vec::with_capacity(head.len() + body.len() + 10);
    out.push(0x28);
    out.extend_from_slice(&(head.len() as u32).to_be_bytes());
    out.extend_from_slice(&(body.len() as u32).to_be_bytes());
    out.extend_from_slice(head);
    out.extend_from_slice(body);
    out.push(0x29);
    out
}
fn read_frame(stream: &mut dyn Read) -> Result<Vec<u8>> {
    let mut h = [0; 9];
    stream
        .read_exact(&mut h)
        .map_err(|_| "QQ Highway response truncated")?;
    let head = u32::from_be_bytes(h[1..5].try_into().unwrap()) as usize;
    let body = u32::from_be_bytes(h[5..9].try_into().unwrap()) as usize;
    if h[0] != 0x28 || head > 100 * 1024 || body > 100 * 1024 {
        return Err("QQ Highway response exceeds bound".into());
    }
    let mut data = vec![0; head + body + 1];
    stream
        .read_exact(&mut data)
        .map_err(|_| "QQ Highway response incomplete")?;
    if data.last() != Some(&0x29) {
        return Err("QQ Highway response frame invalid".into());
    }
    data.truncate(head);
    Ok(data)
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn native_highway_header_matches_independent_go() {
        let sum = Md5::digest(b"abc");
        let actual = block_head(
            crate::AppProfile::default(),
            12345,
            2,
            1004,
            3,
            0,
            b"abc",
            &sum,
            b"T",
            b"G",
            b"E",
        );
        let expected=unhex("0a260801120531323334351a0c50696355702e446174615570200228003096859b8002381040ec071235080010031800200328003201544210900150983cd24fb0d6963f7d28e17f724a10900150983cd24fb0d6963f7d28e17f72500068001a01452a0b080812014718cfacf8fa05",111).unwrap();
        assert_eq!(actual, expected);
    }
    #[test]
    fn frames_reject_truncation_oversize_and_bad_terminators() {
        let good = frame(&[8, 0], &[]);
        assert_eq!(read_frame(&mut good.as_slice()).unwrap(), [8, 0]);
        for n in 0..good.len() {
            assert!(read_frame(&mut &good[..n]).is_err())
        }
        let mut wrong = good.clone();
        *wrong.last_mut().unwrap() = 0;
        assert!(read_frame(&mut wrong.as_slice()).is_err());
        let mut huge = vec![0x28];
        huge.extend_from_slice(&u32::MAX.to_be_bytes());
        huge.extend_from_slice(&0u32.to_be_bytes());
        assert!(read_frame(&mut huge.as_slice()).is_err());
    }
    #[test]
    fn image_header_and_checksums_are_read_without_decoding_pixels() {
        let raw=b"GIF89a\x01\x00\x01\x00\x80\x00\x00\x00\x00\x00\xff\xff\xff\x21\xf9\x04\x01\x00\x00\x00\x00\x2c\x00\x00\x00\x00\x01\x00\x01\x00\x00\x02\x02\x44\x01\x00\x3b";
        let path = std::env::temp_dir().join(format!("qq-image-fixture-{}", std::process::id()));
        std::fs::write(&path, raw).unwrap();
        let info = inspect_image(&path).unwrap();
        std::fs::remove_file(&path).unwrap();
        assert_eq!((info.width, info.height, info.format), (1, 1, 2000));
        assert_eq!(info.bytes, raw.len() as u64);
        assert_eq!(info.md5, hex(&Md5::digest(raw)));
        assert_eq!(info.sha1, hex(&Sha1::digest(raw)));
        let set: serde_json::Value =
            serde_json::from_str(include_str!("../tests/reference.json")).unwrap();
        for (target, label) in [
            (
                SendTarget::Private {
                    uid: "u_peer_fixture".into(),
                },
                "private",
            ),
            (SendTarget::Group { code: 123456 }, "group"),
        ] {
            let expected = set[format!("image_{label}")].as_str().unwrap();
            assert_eq!(
                image_request(&target, "u_own_fixture", &info, 7).unwrap().2,
                unhex(expected, expected.len() / 2).unwrap()
            )
        }
        assert!(dimensions(b"not an image").is_err());
    }
}
