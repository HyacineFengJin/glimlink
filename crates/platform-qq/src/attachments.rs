// SPDX-License-Identifier: AGPL-3.0-only
//! Native voice/video/file uploads; host owns resources and one-shot Outbox.
use crate::{
    Client, Result, SendTarget,
    directory::{num, oidb, oidb_body, text},
    rich::MediaKind,
    upload::{ImageInfo, MAX_IMAGE_BYTES},
    wire::{self, Fields, Value},
};
use base64::{Engine, engine::general_purpose::STANDARD};
use md5::{Digest, Md5};
use serde::{Deserialize, Serialize};
use sha1::Sha1;
use std::{
    fs::{File, OpenOptions},
    io::{Read, Seek, SeekFrom},
    os::unix::fs::OpenOptionsExt,
    path::Path,
};
const MAX_BYTES: u64 = 128 * 1024 * 1024;
fn hex(b: &[u8]) -> String {
    b.iter().map(|v| format!("{v:02x}")).collect()
}
fn unhex(s: &str, n: usize) -> Result<Vec<u8>> {
    if s.len() != n * 2 || !s.is_ascii() {
        return Err("QQ blob checksum invalid".into());
    }
    (0..s.len())
        .step_by(2)
        .map(|i| {
            u8::from_str_radix(&s[i..i + 2], 16).map_err(|_| "QQ blob checksum invalid".into())
        })
        .collect()
}
fn open(path: &Path) -> Result<File> {
    let f = OpenOptions::new()
        .read(true)
        .custom_flags(0x20000 | 0x800)
        .open(path)
        .map_err(|_| "QQ attachment resource unavailable")?;
    let m = f
        .metadata()
        .map_err(|_| "QQ attachment metadata unavailable")?;
    if !m.is_file() || m.len() == 0 || m.len() > MAX_BYTES {
        return Err("QQ attachment must be a regular 1..128 MiB resource".into());
    }
    Ok(f)
}
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct BlobInfo {
    pub bytes: u64,
    pub md5: String,
    pub sha1: String,
    pub md5_first_10m: String,
}
impl BlobInfo {
    fn transport(&self) -> ImageInfo {
        ImageInfo {
            bytes: self.bytes,
            md5: self.md5.clone(),
            sha1: self.sha1.clone(),
            width: 0,
            height: 0,
            format: 0,
            extension: String::new(),
        }
    }
}
fn inspect(f: &mut File) -> Result<BlobInfo> {
    f.seek(SeekFrom::Start(0))
        .map_err(|_| "QQ blob seek failed")?;
    let (mut md5, mut first, mut sha) = (Md5::new(), Md5::new(), Sha1::new());
    let mut total = 0;
    let mut b = [0; 16384];
    loop {
        let n = f.read(&mut b).map_err(|_| "QQ blob read failed")?;
        if n == 0 {
            break;
        }
        if total < 10 * 1024 * 1024 {
            first.update(&b[..n.min((10 * 1024 * 1024 - total) as usize)])
        }
        total += n as u64;
        if total > MAX_BYTES {
            return Err("QQ attachment exceeds128 MiB".into());
        }
        md5.update(&b[..n]);
        sha.update(&b[..n])
    }
    f.seek(SeekFrom::Start(0))
        .map_err(|_| "QQ blob seek failed")?;
    Ok(BlobInfo {
        bytes: total,
        md5: hex(&md5.finalize()),
        sha1: hex(&sha.finalize()),
        md5_first_10m: hex(&first.finalize()),
    })
}
pub fn inspect_blob(path: &Path) -> Result<BlobInfo> {
    inspect(&mut open(path)?)
}
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct VoiceInfo {
    pub blob: BlobInfo,
    pub duration_seconds: u32,
    pub codec: String,
}
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct VideoInfo {
    pub blob: BlobInfo,
    pub duration_seconds: u32,
    pub thumbnail: ImageInfo,
}
pub fn inspect_voice(path: &Path) -> Result<VoiceInfo> {
    let mut f = open(path)?;
    let blob = inspect(&mut f)?;
    if blob.bytes > MAX_IMAGE_BYTES {
        return Err("QQ voice exceeds20 MiB".into());
    }
    let mut header = [0; 10];
    let length = f
        .read(&mut header)
        .map_err(|_| "QQ voice header read failed")?;
    let (mut at, codec) = if header[..length].starts_with(b"\x02#!SILK_V3") {
        (10u64, "silk_v3")
    } else if header[..length].starts_with(b"#!SILK_V3") {
        (9, "silk_v3")
    } else if header[..length].starts_with(b"#!AMR\n") {
        (6, "amr_nb")
    } else {
        return Err("QQ voice needs native SILK v3 or AMR-NB bytes".into());
    };
    f.seek(SeekFrom::Start(at))
        .map_err(|_| "QQ voice seek failed")?;
    let mut frames = 0u64;
    while at < blob.bytes {
        let len = if codec == "silk_v3" {
            let mut b = [0; 2];
            f.read_exact(&mut b)
                .map_err(|_| "QQ SILK packet header truncated")?;
            at += 2;
            let n = i16::from_le_bytes(b);
            if n == -1 {
                if at != blob.bytes {
                    return Err("QQ SILK trailing bytes".into());
                }
                break;
            }
            if n <= 0 || n > 8192 {
                return Err("QQ SILK packet length invalid".into());
            }
            n as u64
        } else {
            let mut b = [0];
            f.read_exact(&mut b)
                .map_err(|_| "QQ AMR frame header truncated")?;
            at += 1;
            let mode = (b[0] >> 3) & 15;
            if b[0] & 0x83 != 0 || !matches!(mode, 0..=8 | 15) {
                return Err("QQ AMR frame invalid".into());
            }
            [12, 13, 15, 17, 19, 20, 26, 31, 5, 0, 0, 0, 0, 0, 0, 0][mode as usize]
        };
        at = at
            .checked_add(len)
            .filter(|n| *n <= blob.bytes)
            .ok_or("QQ voice frame truncated")?;
        f.seek(SeekFrom::Start(at))
            .map_err(|_| "QQ voice frame seek failed")?;
        frames += 1;
        if frames > 6000 {
            return Err("QQ voice exceeds120 seconds".into());
        }
    }
    if frames == 0 {
        return Err("QQ voice has no frames".into());
    }
    Ok(VoiceInfo {
        blob,
        duration_seconds: u32::try_from((frames * 20).div_ceil(1000)).unwrap(),
        codec: codec.into(),
    })
}
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct NativeMedia {
    pub kind: MediaKind,
    pub msg_info: String,
    pub group: bool,
}
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct NativeFile {
    pub name: String,
    pub bytes: u64,
    pub md5: String,
    pub uuid: String,
    pub hash: String,
}
impl NativeFile {
    pub(crate) fn content(&self, time: u32) -> Result<Vec<u8>> {
        validate_name(&self.name)?;
        if self.bytes == 0
            || self.bytes > MAX_BYTES
            || self.uuid.is_empty()
            || self.uuid.len() > 2048
            || self.hash.is_empty()
            || self.hash.len() > 2048
        {
            return Err("QQ file send metadata invalid".into());
        }
        let mut f = vec![];
        wire::number(1, 0, &mut f);
        wire::bytes(3, self.uuid.as_bytes(), &mut f);
        wire::bytes(4, &unhex(&self.md5, 16)?, &mut f);
        wire::bytes(5, self.name.as_bytes(), &mut f);
        wire::number(6, self.bytes, &mut f);
        wire::number(9, 1, &mut f);
        wire::number(50, 0, &mut f);
        wire::number(
            55,
            (time.saturating_add(7 * 86400) as i32) as i64 as u64,
            &mut f,
        );
        wire::bytes(57, self.hash.as_bytes(), &mut f);
        let mut content = vec![];
        wire::bytes(1, &f, &mut content);
        Ok(content)
    }
}
pub fn validate_name(name: &str) -> Result<()> {
    if name.is_empty()
        || name.len() > 512
        || name.contains(['\\', '/', '\0'])
        || name.chars().any(char::is_control)
        || matches!(name, "." | "..")
    {
        return Err("QQ filename must be a bounded basename".into());
    }
    Ok(())
}
fn upload_head(
    target: &SendTarget,
    uid: &str,
    business: u32,
    request: u32,
) -> Result<(bool, Vec<u8>)> {
    let group = matches!(target, SendTarget::Group { .. });
    let mut common = vec![];
    wire::number(1, request.into(), &mut common);
    wire::number(2, 100, &mut common);
    let mut scene = vec![];
    wire::number(101, 2, &mut scene);
    wire::number(102, business.into(), &mut scene);
    wire::number(200, if group { 2 } else { 1 }, &mut scene);
    if let SendTarget::Group { code } = target {
        let mut g = vec![];
        wire::number(1, (*code).into(), &mut g);
        wire::bytes(202, &g, &mut scene)
    } else {
        let dest = match target {
            SendTarget::Private { uid } => uid.as_str(),
            _ => uid,
        };
        if !crate::rich::valid_uid(dest) {
            return Err("QQ attachment target UID invalid".into());
        }
        let mut c = vec![];
        wire::number(1, 2, &mut c);
        wire::bytes(2, dest.as_bytes(), &mut c);
        wire::bytes(201, &c, &mut scene)
    }
    let mut h = vec![];
    wire::bytes(1, &common, &mut h);
    wire::bytes(2, &scene, &mut h);
    wire::bytes(3, &[8, 2], &mut h);
    Ok((group, h))
}
fn file_info(
    blob: &BlobInfo,
    name: &str,
    typ: u32,
    width: u32,
    height: u32,
    duration: u32,
) -> Vec<u8> {
    let mut t = vec![];
    wire::number(1, typ.into(), &mut t);
    if typ == 3 {
        wire::number(4, 1, &mut t)
    }
    let mut f = vec![];
    wire::number(1, blob.bytes, &mut f);
    wire::bytes(2, blob.md5.as_bytes(), &mut f);
    wire::bytes(3, blob.sha1.as_bytes(), &mut f);
    wire::bytes(4, name.as_bytes(), &mut f);
    wire::bytes(5, &t, &mut f);
    if width != 0 {
        wire::number(6, width.into(), &mut f)
    }
    if height != 0 {
        wire::number(7, height.into(), &mut f)
    }
    if duration != 0 {
        wire::number(8, duration.into(), &mut f)
    }
    f
}
pub(crate) fn voice_request(
    target: &SendTarget,
    uid: &str,
    info: &VoiceInfo,
    random: u32,
) -> Result<(u32, Vec<u8>)> {
    let (group, h) = upload_head(
        target,
        uid,
        3,
        if matches!(target, SendTarget::Group { .. }) {
            1
        } else {
            4
        },
    )?;
    let fi = file_info(
        &info.blob,
        &format!("{}.amr", info.blob.md5),
        3,
        0,
        0,
        info.duration_seconds,
    );
    let mut i = vec![];
    wire::bytes(1, &fi, &mut i);
    let mut u = vec![];
    wire::bytes(1, &i, &mut u);
    wire::number(2, 1, &mut u);
    wire::number(4, random.into(), &mut u);
    wire::number(5, if group { 2 } else { 1 }, &mut u);
    let mut pic = vec![];
    wire::bytes(2, "[语音]".as_bytes(), &mut pic);
    let mut ptt = vec![];
    wire::bytes(if group { 12 } else { 11 }, &[8, 0, 56, 0], &mut ptt);
    wire::bytes(
        13,
        if group {
            &[0x9a, 1, 7, 0xaa, 3, 4, 8, 8, 0x12, 0][..]
        } else {
            &[0x9a, 1, 11, 0xaa, 3, 8, 8, 4, 0x12, 4, 0, 0, 0, 0][..]
        },
        &mut ptt,
    );
    let mut ext = vec![];
    wire::bytes(1, &pic, &mut ext);
    if group {
        wire::bytes(2, &[], &mut ext)
    }
    wire::bytes(3, &ptt, &mut ext);
    wire::bytes(6, &ext, &mut u);
    let mut body = vec![];
    wire::bytes(1, &h, &mut body);
    wire::bytes(2, &u, &mut body);
    Ok((if group { 0x126e } else { 0x126d }, body))
}
fn nt_response(raw: &[u8], cmd: u32) -> Result<Vec<u8>> {
    let f = Fields::parse(oidb_body(raw, cmd, 100)?)?;
    let h = Fields::parse(f.data(1).ok_or("QQ attachment response head missing")?)?;
    if num(&h, 2)? != 0 {
        return Err(format!(
            "QQ attachment negotiation rejected ({})",
            num(&h, 2)?
        ));
    }
    Ok(f.data(2)
        .filter(|b| b.len() <= 256 * 1024)
        .ok_or("QQ attachment upload response missing")?
        .to_vec())
}
fn msg_info(up: &Fields<'_>) -> Result<Vec<u8>> {
    let msg = up
        .data(6)
        .filter(|b| !b.is_empty() && b.len() <= 65536)
        .ok_or("QQ attachment MsgInfo missing")?;
    Fields::parse(msg)?;
    Ok(msg.to_vec())
}
fn nodes(msg: &[u8]) -> Result<Vec<Vec<u8>>> {
    let mut out = vec![];
    for (t, v) in Fields::parse(msg)?.0 {
        if t == 1 {
            let Value::Bytes(b) = v else {
                return Err("QQ upload index malformed".into());
            };
            let f = Fields::parse(b)?;
            out.push(f.data(1).ok_or("QQ upload node missing")?.to_vec());
            if out.len() > 4 {
                return Err("QQ upload index bound".into());
            }
        }
    }
    Ok(out)
}
fn check_node(node: &[u8], blob: &BlobInfo) -> Result<()> {
    let n = Fields::parse(node)?;
    let f = Fields::parse(n.data(1).ok_or("QQ upload FileInfo missing")?)?;
    if u64::from(num(&f, 1)?) != blob.bytes
        || text(&f, 2, 64)? != blob.md5
        || text(&f, 3, 64)? != blob.sha1
    {
        return Err("QQ upload checksum identity mismatch".into());
    }
    Ok(())
}
fn nt_ext(
    msg: &[u8],
    node: &[u8],
    key: &str,
    addresses: &Fields<'_>,
    address_tag: u32,
    hashes: &[Vec<u8>],
) -> Result<Vec<u8>> {
    let n = Fields::parse(node)?;
    let mut ext = vec![];
    wire::bytes(1, text(&n, 2, 2048)?.as_bytes(), &mut ext);
    wire::bytes(2, key.as_bytes(), &mut ext);
    let mut network = vec![];
    for (t, v) in &addresses.0 {
        if *t != address_tag {
            continue;
        }
        let Value::Bytes(b) = v else { continue };
        let f = Fields::parse(b)?;
        let ip = std::net::Ipv4Addr::from(num(&f, 1)?.to_le_bytes());
        let port = num(&f, 2)?;
        if !crate::upload::public_ip(ip) || port == 0 || port > 65535 {
            return Err("QQ upload address invalid".into());
        }
        let mut d = vec![];
        wire::number(1, 1, &mut d);
        wire::bytes(2, ip.to_string().as_bytes(), &mut d);
        let mut a = vec![];
        wire::bytes(1, &d, &mut a);
        wire::number(2, port.into(), &mut a);
        wire::bytes(1, &a, &mut network)
    }
    wire::bytes(5, &network, &mut ext);
    for (t, v) in Fields::parse(msg)?.0 {
        if t == 1 {
            if let Value::Bytes(b) = v {
                wire::bytes(6, b, &mut ext)
            }
        }
    }
    wire::number(10, 1024 * 1024, &mut ext);
    let mut hash = vec![];
    for h in hashes {
        wire::bytes(1, h, &mut hash)
    }
    wire::bytes(11, &hash, &mut ext);
    Ok(ext)
}
impl Client {
    pub fn upload_video(
        &mut self,
        target: &SendTarget,
        path: &Path,
        thumb_path: &Path,
        expected: &VideoInfo,
        random: u32,
    ) -> Result<NativeMedia> {
        if !self.is_online() || expected.duration_seconds == 0 || expected.duration_seconds > 3600 {
            return Err("QQ video request duration/session invalid".into());
        }
        let mut file = open(path)?;
        if inspect(&mut file)? != expected.blob
            || crate::upload::inspect_image(thumb_path)? != expected.thumbnail
        {
            return Err("QQ video/thumbnail resource changed".into());
        }
        if expected.thumbnail.format != 1000 {
            return Err("QQ video thumbnail requires JPEG".into());
        }
        let (cmd, request) = video_request(target, &self.session().uid, expected, random)?;
        let raw = self.request(
            &format!("OidbSvcTrpcTcp.0x{cmd:x}_100"),
            &oidb(cmd, 100, &request, true),
        )?;
        let raw = nt_response(&raw, cmd)?;
        let up = Fields::parse(&raw)?;
        let msg = msg_info(&up)?;
        let ns = nodes(&msg)?;
        if ns.len() != 2 {
            return Err("QQ video upload must identify video and thumbnail".into());
        }
        check_node(&ns[0], &expected.blob)?;
        let thumb = BlobInfo {
            bytes: expected.thumbnail.bytes,
            md5: expected.thumbnail.md5.clone(),
            sha1: expected.thumbnail.sha1.clone(),
            md5_first_10m: expected.thumbnail.md5.clone(),
        };
        check_node(&ns[1], &thumb)?;
        let main_key = text(&up, 1, 16384)?;
        let mut sub = None;
        for (t, v) in &up.0 {
            if *t == 10 {
                let Value::Bytes(b) = v else { continue };
                let s = Fields::parse(b)?;
                if num(&s, 1)? == 100 {
                    sub = Some(s);
                    break;
                }
            }
        }
        let sub = sub.ok_or("QQ thumbnail upload decision missing")?;
        let thumb_key = text(&sub, 2, 16384)?;
        let group = matches!(target, SendTarget::Group { .. });
        if !main_key.is_empty() || !thumb_key.is_empty() {
            let hw = self.highway()?;
            if !main_key.is_empty() {
                let hashes = video_block_sha1(&mut file)?;
                let ext = nt_ext(&msg, &ns[0], &main_key, &up, 3, &hashes)?;
                self.highway_stream(
                    &mut file,
                    &expected.blob.transport(),
                    &hw,
                    &ext,
                    if group { 1005 } else { 1001 },
                    random,
                )?
            }
            if !thumb_key.is_empty() {
                let ext = nt_ext(
                    &msg,
                    &ns[1],
                    &thumb_key,
                    &sub,
                    4,
                    &[unhex(&thumb.sha1, 20)?],
                )?;
                let mut f = open(thumb_path)?;
                self.highway_stream(
                    &mut f,
                    &thumb.transport(),
                    &hw,
                    &ext,
                    if group { 1006 } else { 1002 },
                    random.wrapping_add(1),
                )?
            }
        }
        Ok(NativeMedia {
            kind: MediaKind::Video,
            msg_info: STANDARD.encode(msg),
            group,
        })
    }
    pub fn upload_file(
        &mut self,
        target: &SendTarget,
        path: &Path,
        expected: &BlobInfo,
        name: &str,
        random: u32,
    ) -> Result<NativeFile> {
        if !self.is_online() {
            return Err("QQ session not online".into());
        }
        validate_name(name)?;
        let mut file = open(path)?;
        if inspect(&mut file)? != *expected {
            return Err("QQ file resource changed".into());
        }
        let group = match target {
            SendTarget::Group { code } => Some(*code),
            _ => None,
        };
        let (cmd, sub, request) = file_request(target, &self.session().uid, expected, name)?;
        let raw = self.request(&format!("OidbSvcTrpcTcp.0x{cmd:x}_{sub}"), &request)?;
        let f = Fields::parse(oidb_body(&raw, cmd, sub)?)?;
        let up = Fields::parse(
            f.data(if group.is_some() { 1 } else { 19 })
                .ok_or("QQ file upload result missing")?,
        )?;
        let (ret, uuid, hash, exists, host, port, check, key) = if group.is_some() {
            (
                num(&up, 1)?,
                text(&up, 7, 2048)?,
                String::new(),
                num(&up, 10)? != 0,
                text(&up, 4, 255)?,
                num(&up, 14)?,
                up.data(8).unwrap_or_default().to_vec(),
                up.data(9).unwrap_or_default().to_vec(),
            )
        } else {
            (
                num(&up, 10)?,
                text(&up, 90, 2048)?,
                text(&up, 200, 2048)?,
                num(&up, 110)? != 0,
                text(&up, 60, 255)?,
                num(&up, 80)?,
                unhex(&expected.sha1, 20)?,
                up.data(220).unwrap_or_default().to_vec(),
            )
        };
        if ret != 0 {
            return Err(format!("QQ file negotiation rejected ({ret})"));
        }
        if uuid.is_empty() || (group.is_none() && hash.is_empty()) {
            return Err("QQ file upload identity missing".into());
        }
        if !exists {
            let ext = file_extension(
                self.session().uin,
                group,
                expected,
                name,
                &uuid,
                &host,
                port,
                &check,
                &key,
            )?;
            let hw = self.highway()?;
            self.highway_stream(
                &mut file,
                &expected.transport(),
                &hw,
                &ext,
                if group.is_some() { 71 } else { 95 },
                random,
            )?
        }
        Ok(NativeFile {
            name: name.into(),
            bytes: expected.bytes,
            md5: expected.md5.clone(),
            uuid,
            hash,
        })
    }
    /// Publishing a group file is itself the chat write. Never call this from a read/upload helper.
    pub fn publish_group_file(
        &mut self,
        code: u32,
        file: &NativeFile,
        random: u32,
    ) -> Result<serde_json::Value> {
        if !self.is_online() || code == 0 || file.uuid.is_empty() {
            return Err("QQ group file publish request invalid".into());
        }
        let mut info = vec![];
        wire::number(1, 102, &mut info);
        wire::bytes(2, file.uuid.as_bytes(), &mut info);
        wire::number(3, random.into(), &mut info);
        wire::bytes(4, b"{}", &mut info);
        wire::number(5, 1, &mut info);
        let mut body = vec![];
        wire::number(1, code.into(), &mut body);
        wire::number(2, 4, &mut body);
        wire::bytes(3, &info, &mut body);
        let mut b = vec![];
        wire::bytes(5, &body, &mut b);
        let raw = self.request("OidbSvcTrpcTcp.0x6d9_4", &oidb(0x6d9, 4, &b, true))?;
        let f = Fields::parse(&raw)?;
        if num(&f, 1)? != 0x6d9 || num(&f, 2)? != 4 {
            return Err("QQ file publish response mismatch".into());
        }
        let result = num(&f, 3)?;
        Ok(
            serde_json::json!({"result":result,"group":code,"file_id":file.uuid,"acknowledgement":"native_oidb_publish_result","message_sequence":null}),
        )
    }
    pub fn upload_voice(
        &mut self,
        target: &SendTarget,
        path: &Path,
        expected: &VoiceInfo,
        random: u32,
    ) -> Result<NativeMedia> {
        if !self.is_online() {
            return Err("QQ session not online".into());
        }
        let mut f = open(path)?;
        if inspect(&mut f)? != expected.blob || inspect_voice(path)? != *expected {
            return Err("QQ voice resource changed".into());
        }
        let (cmd, body) = voice_request(target, &self.session().uid, expected, random)?;
        let raw = self.request(
            &format!("OidbSvcTrpcTcp.0x{cmd:x}_100"),
            &oidb(cmd, 100, &body, true),
        )?;
        let raw = nt_response(&raw, cmd)?;
        let up = Fields::parse(&raw)?;
        let msg = msg_info(&up)?;
        let ns = nodes(&msg)?;
        let node = ns.first().ok_or("QQ voice node missing")?;
        check_node(node, &expected.blob)?;
        let key = text(&up, 1, 16384)?;
        if !key.is_empty() {
            let hw = self.highway()?;
            let ext = nt_ext(&msg, node, &key, &up, 3, &[unhex(&expected.blob.sha1, 20)?])?;
            self.highway_stream(
                &mut f,
                &expected.blob.transport(),
                &hw,
                &ext,
                if matches!(target, SendTarget::Group { .. }) {
                    1008
                } else {
                    1007
                },
                random,
            )?
        }
        Ok(NativeMedia {
            kind: MediaKind::Voice,
            msg_info: STANDARD.encode(msg),
            group: matches!(target, SendTarget::Group { .. }),
        })
    }
}
pub(crate) fn file_request(
    target: &SendTarget,
    self_uid: &str,
    expected: &BlobInfo,
    name: &str,
) -> Result<(u32, u32, Vec<u8>)> {
    validate_name(name)?;
    let group = match target {
        SendTarget::Group { code } => Some(*code),
        _ => None,
    };
    let result = if let Some(code) = group {
        let mut f = vec![];
        for (t, n) in [
            (1, u64::from(code)),
            (2, 4),
            (3, 102),
            (4, 6),
            (8, expected.bytes),
            (15, 1),
        ] {
            wire::number(t, n, &mut f)
        }
        wire::bytes(5, b"/", &mut f);
        wire::bytes(6, name.as_bytes(), &mut f);
        wire::bytes(7, format!("/{name}").as_bytes(), &mut f);
        wire::bytes(9, &unhex(&expected.sha1, 20)?, &mut f);
        wire::bytes(10, &[], &mut f);
        wire::bytes(11, &unhex(&expected.md5, 16)?, &mut f);
        let mut b = vec![];
        wire::bytes(1, &f, &mut b);
        (0x6d6, 0, oidb(0x6d6, 0, &b, true))
    } else {
        let uid = match target {
            SendTarget::Private { uid } => uid.as_str(),
            _ => self_uid,
        };
        let mut f = vec![];
        wire::bytes(10, self_uid.as_bytes(), &mut f);
        wire::bytes(20, uid.as_bytes(), &mut f);
        wire::number(30, expected.bytes, &mut f);
        wire::bytes(40, name.as_bytes(), &mut f);
        wire::bytes(50, &unhex(&expected.md5_first_10m, 16)?, &mut f);
        wire::bytes(60, &unhex(&expected.sha1, 20)?, &mut f);
        wire::bytes(70, b"/", &mut f);
        wire::bytes(110, &unhex(&expected.md5, 16)?, &mut f);
        wire::bytes(120, &[], &mut f);
        let mut b = vec![];
        wire::number(1, 1700, &mut b);
        wire::bytes(19, &f, &mut b);
        for (t, n) in [(101, 3), (102, 1), (200, 1)] {
            wire::number(t, n, &mut b)
        }
        (0xe37, 1700, oidb(0xe37, 1700, &b, false))
    };
    Ok(result)
}
pub(crate) fn video_request(
    target: &SendTarget,
    uid: &str,
    info: &VideoInfo,
    random: u32,
) -> Result<(u32, Vec<u8>)> {
    let (group, h) = upload_head(target, uid, 2, 3)?;
    let mut up = vec![];
    let main = file_info(&info.blob, "video.mp4", 2, 0, 0, info.duration_seconds);
    let mut m = vec![];
    wire::bytes(1, &main, &mut m);
    wire::bytes(1, &m, &mut up);
    let thumb = BlobInfo {
        bytes: info.thumbnail.bytes,
        md5: info.thumbnail.md5.clone(),
        sha1: info.thumbnail.sha1.clone(),
        md5_first_10m: info.thumbnail.md5.clone(),
    };
    let fi = file_info(
        &thumb,
        "video.jpg",
        1,
        info.thumbnail.width,
        info.thumbnail.height,
        0,
    );
    let mut t = vec![];
    wire::bytes(1, &fi, &mut t);
    wire::number(2, 100, &mut t);
    wire::bytes(1, &t, &mut up);
    wire::number(2, 1, &mut up);
    wire::number(4, random.into(), &mut up);
    wire::number(5, 2, &mut up);
    let mut pic = vec![];
    wire::bytes(2, "[视频]".as_bytes(), &mut pic);
    let mut video = vec![];
    wire::bytes(3, &[0x80, 1, 0], &mut video);
    let mut ext = vec![];
    wire::bytes(1, &pic, &mut ext);
    wire::bytes(2, &video, &mut ext);
    wire::bytes(3, &[0x5a, 0, 0x62, 0, 0x6a, 0], &mut ext);
    wire::bytes(6, &ext, &mut up);
    let mut b = vec![];
    wire::bytes(1, &h, &mut b);
    wire::bytes(2, &up, &mut b);
    Ok((if group { 0x11ea } else { 0x11e9 }, b))
}
fn video_block_sha1(f: &mut File) -> Result<Vec<Vec<u8>>> {
    f.seek(SeekFrom::Start(0))
        .map_err(|_| "QQ video hash seek failed")?;
    let mut normal = Sha1::new();
    let mut state = [0x67452301, 0xefcdab89, 0x98badcfe, 0x10325476, 0xc3d2e1f0];
    let mut hashes = vec![];
    let mut block = vec![0; 1024 * 1024];
    loop {
        let mut n = 0;
        while n < block.len() {
            let count = f
                .read(&mut block[n..])
                .map_err(|_| "QQ video hash read failed")?;
            if count == 0 {
                break;
            }
            n += count
        }
        normal.update(&block[..n]);
        if n < block.len() {
            break;
        }
        for b in block.chunks_exact(64) {
            sha1::compress(
                &mut state,
                std::slice::from_ref(sha1::digest::generic_array::GenericArray::from_slice(b)),
            )
        }
        hashes.push(state.iter().flat_map(|v| v.to_le_bytes()).collect());
        if hashes.len() > 128 {
            return Err("QQ video hash exceeds128 MiB".into());
        }
    }
    hashes.push(normal.finalize().to_vec());
    f.seek(SeekFrom::Start(0))
        .map_err(|_| "QQ video hash reset failed")?;
    Ok(hashes)
}
#[cfg(test)]
mod tests {
    use super::*;
    fn vector(name: &str) -> Vec<u8> {
        let set: serde_json::Value =
            serde_json::from_str(include_str!("../tests/reference.json")).unwrap();
        let s = set[name].as_str().unwrap();
        unhex(s, s.len() / 2).unwrap()
    }
    fn blob() -> BlobInfo {
        BlobInfo {
            bytes: 3,
            md5: "900150983cd24fb0d6963f7d28e17f72".into(),
            sha1: "a9993e364706816aba3e25717850c26c9cd0d89d".into(),
            md5_first_10m: "900150983cd24fb0d6963f7d28e17f72".into(),
        }
    }
    #[test]
    fn voice_and_video_negotiation_match_pinned_go() {
        let voice = VoiceInfo {
            blob: blob(),
            duration_seconds: 2,
            codec: "amr_nb".into(),
        };
        let thumb = ImageInfo {
            bytes: 3,
            md5: blob().md5,
            sha1: blob().sha1,
            width: 32,
            height: 16,
            format: 1000,
            extension: "jpg".into(),
        };
        let video = VideoInfo {
            blob: blob(),
            duration_seconds: 2,
            thumbnail: thumb,
        };
        for (target, label) in [
            (
                SendTarget::Private {
                    uid: "u_peer_fixture".into(),
                },
                "private",
            ),
            (SendTarget::Group { code: 123456 }, "group"),
        ] {
            assert_eq!(
                voice_request(&target, "u_own_fixture", &voice, 7)
                    .unwrap()
                    .1,
                vector(&format!("voice_{label}"))
            );
            assert_eq!(
                video_request(&target, "u_own_fixture", &video, 7)
                    .unwrap()
                    .1,
                vector(&format!("video_{label}"))
            );
        }
    }
    #[test]
    fn video_sha1_intermediate_states_match_independent_go() {
        let path =
            std::env::temp_dir().join(format!("qq-video-sha-fixture-{}", std::process::id()));
        std::fs::write(&path, vec![b'a'; 2 * 1024 * 1024 + 3]).unwrap();
        let mut file = open(&path).unwrap();
        let values = video_block_sha1(&mut file).unwrap();
        std::fs::remove_file(path).unwrap();
        assert_eq!(values.len(), 3);
        for (i, value) in values.into_iter().enumerate() {
            assert_eq!(value, vector(&format!("video_sha1_{i}")))
        }
        assert_eq!(file.stream_position().unwrap(), 0);
    }
    #[test]
    fn voice_framing_rejects_truncation_and_measures_amr_frames() {
        let path = std::env::temp_dir().join(format!("qq-voice-fixture-{}", std::process::id()));
        let mut data = b"#!AMR\n".to_vec();
        for _ in 0..51 {
            data.push(4);
            data.extend_from_slice(&[0; 12])
        }
        std::fs::write(&path, &data).unwrap();
        let info = inspect_voice(&path).unwrap();
        assert_eq!(info.codec, "amr_nb");
        assert_eq!(info.duration_seconds, 2);
        data.pop();
        std::fs::write(&path, &data).unwrap();
        assert!(inspect_voice(&path).is_err());
        std::fs::remove_file(path).unwrap();
        for name in ["../a", "a/b", "a\\b", "\0", "", ".."] {
            assert!(validate_name(name).is_err())
        }
    }
    #[test]
    fn private_and_group_file_negotiation_match_native_go_field_values() {
        // Protobuf field order and omitted default scalar values do not change
        // the message. Recurse only into known envelope and request messages.
        fn canonical(bytes: &[u8], path: &[u32], group: bool) -> serde_json::Value {
            let f = Fields::parse(bytes).unwrap();
            let mut values = Vec::new();
            for (t, v) in f.0 {
                let value = match v {
                    Value::Number(0) => continue,
                    Value::Number(n) => serde_json::json!({"n":n}),
                    Value::Bytes(b) => {
                        if (path.is_empty() && t == 4)
                            || (path == [4] && t == if group { 1 } else { 19 })
                        {
                            let mut next = path.to_vec();
                            next.push(t);
                            canonical(b, &next, group)
                        } else {
                            serde_json::json!({"hex":hex(b)})
                        }
                    }
                    _ => panic!("unexpected file request wire"),
                };
                values.push((t, value))
            }
            values.sort_by_key(|p| p.0);
            serde_json::to_value(values).unwrap()
        }
        for (target, label) in [
            (
                SendTarget::Private {
                    uid: "u_peer_fixture".into(),
                },
                "private",
            ),
            (SendTarget::Group { code: 123456 }, "group"),
        ] {
            let actual = file_request(&target, "u_own_fixture", &blob(), "notes.txt")
                .unwrap()
                .2;
            let expected = vector(&format!("file_{label}"));
            let group = label == "group";
            assert_eq!(
                canonical(&actual, &[], group),
                canonical(&expected, &[], group)
            );
        }
    }
}
fn file_extension(
    actor: u32,
    group: Option<u32>,
    blob: &BlobInfo,
    name: &str,
    id: &str,
    host: &str,
    port: u32,
    check: &[u8],
    key: &[u8],
) -> Result<Vec<u8>> {
    if check.is_empty()
        || check.len() > 16384
        || key.is_empty()
        || key.len() > 16384
        || port == 0
        || port > 65535
        || host.len() > 255
        || host.contains(['/', ':', '@'])
    {
        return Err("QQ file transfer metadata invalid".into());
    }
    if let Ok(ip) = host.parse::<std::net::Ipv4Addr>() {
        if !crate::upload::public_ip(ip) {
            return Err("QQ file upload endpoint not public".into());
        }
    } else {
        crate::media::validate_media_url(&format!("https://{host}/"))?
    }
    let mut business = vec![];
    wire::number(100, actor.into(), &mut business);
    if let Some(g) = group {
        wire::number(200, g.into(), &mut business);
        wire::number(400, g.into(), &mut business)
    }
    let mut f = vec![];
    wire::number(100, blob.bytes, &mut f);
    wire::bytes(200, &unhex(&blob.md5, 16)?, &mut f);
    wire::bytes(300, check, &mut f);
    wire::bytes(400, &unhex(&blob.md5, 16)?, &mut f);
    wire::bytes(600, id.as_bytes(), &mut f);
    wire::bytes(700, key, &mut f);
    let mut client = vec![];
    for (t, n) in [(100, 3), (300, 3), (600, 4)] {
        wire::number(t, n, &mut client)
    }
    wire::bytes(200, b"100", &mut client);
    wire::bytes(400, b"1.1.1", &mut client);
    let mut fname = vec![];
    wire::bytes(100, name.as_bytes(), &mut fname);
    let mut url = vec![];
    wire::number(1, 1, &mut url);
    wire::bytes(2, host.as_bytes(), &mut url);
    let mut h = vec![];
    wire::bytes(1, &url, &mut h);
    wire::number(2, port.into(), &mut h);
    let mut hosts = vec![];
    wire::bytes(200, &h, &mut hosts);
    let mut entry = vec![];
    for (t, b) in [
        (100, &business),
        (200, &f),
        (300, &client),
        (400, &fname),
        (500, &hosts),
    ] {
        wire::bytes(t, b, &mut entry)
    }
    let mut ext = vec![];
    wire::number(1, 100, &mut ext);
    wire::number(2, 1, &mut ext);
    wire::bytes(100, &entry, &mut ext);
    if group.is_none() {
        wire::number(200, 1, &mut ext)
    }
    Ok(ext)
}
