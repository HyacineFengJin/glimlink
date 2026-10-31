// SPDX-License-Identifier: AGPL-3.0-only
// Adapted from Agent Runtime QQ experiment; pinned LagrangeGo attribution in NOTICE.
//! Native NTQQ QR login, translated from the pinned LagrangeGo protocol reference.
//! References (commit f7f8abfaa7f1): client/{client,oicq}.go,
//! client/internal/oicq/{oicq,ecdh}.go, client/packets/tlv/{common,qrcode}.go.
//! This module only builds/parses packets; it performs no networking or file I/O.
use crate::{
    Result,
    session::Device,
    tea,
    wire::{self, Fields, Reader},
};
use md5::{Digest, Md5};
use p256::{PublicKey, SecretKey, ecdh::diffie_hellman, elliptic_curve::sec1::ToEncodedPoint};
use std::collections::BTreeMap;

pub const TRANS_EMP: &str = "wtlogin.trans_emp";
pub const LOGIN: &str = "wtlogin.login";
pub const KEY_ROTATION_URL: &str = "https://keyrotate.qq.com/rotate_key?cipher_suite_ver=305&uin=0";
const SERVER_PUBLIC_KEY: &str = "04EBCA94D733E399B2DB96EACDD3F69A8BB0F74224E2B44E3357812211D2E62EFBC91BB553098E25E33A799ADC7F76FEB208DA7C6522CDB0719A305180CC54A82E";
const MAX_OICQ: usize = u16::MAX as usize;

pub struct QrCode {
    pub png: Vec<u8>,
    pub url: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum QrState {
    Confirmed { uin: u32 },
    Expired,
    WaitingForScan,
    WaitingForConfirm,
    Canceled,
    Unknown(u8),
}

/// Credentials are intentionally neither Debug nor Serialize: callers must not
/// accidentally log them. Persist them using the account state's private writer.
pub struct LoginCredentials {
    pub uin: u32,
    pub uid: String,
    pub tgt: Vec<u8>,
    pub d2: Vec<u8>,
    pub d2_key: [u8; 16],
    pub tgtgt: [u8; 16],
    pub temp_pwd: Vec<u8>,
    pub nickname: String,
}

struct ScanTicket {
    uin: u32,
    t106: Vec<u8>,
    t16a: Vec<u8>,
    tgtgt: [u8; 16],
}

pub struct QrLogin {
    app_profile: crate::AppProfile,
    guid: Vec<u8>,
    device_name: String,
    public_key: Vec<u8>,
    key_version: u16,
    share_key: [u8; 16],
    random_key: [u8; 16],
    qrsig: Vec<u8>,
    ticket: Option<ScanTicket>,
}

fn md5(value: &[u8]) -> [u8; 16] {
    Md5::digest(value).into()
}

fn decode_hex(value: &str) -> Result<Vec<u8>> {
    if !value.is_ascii() || value.len() % 2 != 0 {
        return Err("invalid server public key hex".into());
    }
    (0..value.len())
        .step_by(2)
        .map(|i| {
            u8::from_str_radix(&value[i..i + 2], 16)
                .map_err(|_| "invalid server public key hex".into())
        })
        .collect()
}

/// Parse the key-rotation endpoint's JSON after the transport has fetched it over
/// HTTPS. Its result can be passed to with_server_key; no account data is needed.
pub fn parse_server_key_json(data: &[u8]) -> Result<(u16, Vec<u8>)> {
    if data.len() > 16_384 {
        return Err("key rotation response too large".into());
    }
    let json: serde_json::Value =
        serde_json::from_slice(data).map_err(|_| "invalid key rotation response")?;
    let meta = json
        .get("PubKeyMeta")
        .ok_or("missing key rotation metadata")?;
    let version = meta
        .get("KeyVer")
        .and_then(|v| v.as_u64())
        .and_then(|n| u16::try_from(n).ok())
        .filter(|n| *n != 0)
        .ok_or("invalid key rotation version")?;
    let key = decode_hex(
        meta.get("PubKey")
            .and_then(|v| v.as_str())
            .ok_or("missing rotated public key")?,
    )?;
    PublicKey::from_sec1_bytes(&key).map_err(|_| "invalid rotated P-256 public key")?;
    Ok((version, key))
}

fn short_bytes(out: &mut Vec<u8>, value: &[u8]) -> Result<()> {
    let length = u16::try_from(value.len()).map_err(|_| "QR field exceeds 16-bit length")?;
    out.extend_from_slice(&length.to_be_bytes());
    out.extend_from_slice(value);
    Ok(())
}
fn tlv(tag: u16, value: &[u8]) -> Result<Vec<u8>> {
    let mut out = tag.to_be_bytes().to_vec();
    short_bytes(&mut out, value)?;
    Ok(out)
}
fn tlvs(values: &[(u16, Vec<u8>)]) -> Result<Vec<u8>> {
    let mut out = u16::try_from(values.len())
        .map_err(|_| "too many QR TLVs")?
        .to_be_bytes()
        .to_vec();
    for (tag, value) in values {
        out.extend(tlv(*tag, value)?);
    }
    Ok(out)
}
fn read_u16(r: &mut Reader<'_>) -> Result<u16> {
    Ok(u16::from_be_bytes(r.take(2)?.try_into().unwrap()))
}
fn read_short<'a>(r: &mut Reader<'a>) -> Result<&'a [u8]> {
    let n = read_u16(r)? as usize;
    r.take(n)
}
fn read_tlvs<'a>(r: &mut Reader<'a>) -> Result<BTreeMap<u16, &'a [u8]>> {
    let count = read_u16(r)? as usize;
    if count > 4096 {
        return Err("QR TLV count exceeds limit".into());
    }
    let mut out = BTreeMap::new();
    for _ in 0..count {
        let tag = read_u16(r)?;
        if out.insert(tag, read_short(r)?).is_some() {
            return Err("duplicate QR TLV".into());
        }
    }
    Ok(out)
}
fn required<'a>(values: &BTreeMap<u16, &'a [u8]>, tag: u16) -> Result<&'a [u8]> {
    values
        .get(&tag)
        .copied()
        .filter(|v| !v.is_empty())
        .ok_or_else(|| format!("missing QR TLV 0x{tag:03x}"))
}

impl QrLogin {
    /// The pinned reference's fallback key. Prefer a freshly retrieved key from
    /// KEY_ROTATION_URL when available, as the reference Go codec does.
    pub fn new(device: &Device) -> Result<Self> {
        Self::with_server_key(device, 1, &decode_hex(SERVER_PUBLIC_KEY)?)
    }

    pub fn with_server_key(device: &Device, version: u16, server_key: &[u8]) -> Result<Self> {
        let remote =
            PublicKey::from_sec1_bytes(server_key).map_err(|_| "invalid QR P-256 server key")?;
        if version == 0 {
            return Err("invalid QR server key version".into());
        }
        let secret = loop {
            let mut scalar = [0; 32];
            tea::random(&mut scalar)?;
            if let Ok(secret) = SecretKey::from_slice(&scalar) {
                break secret;
            }
        };
        Self::from_secret(device, version, &remote, &secret)
    }

    pub fn with_server_key_profile(
        device: &Device,
        version: u16,
        server_key: &[u8],
        profile: crate::AppProfile,
    ) -> Result<Self> {
        let mut value = Self::with_server_key(device, version, server_key)?;
        value.app_profile = profile;
        Ok(value)
    }

    fn from_secret(
        device: &Device,
        version: u16,
        remote: &PublicKey,
        secret: &SecretKey,
    ) -> Result<Self> {
        let share = diffie_hellman(secret.to_nonzero_scalar(), remote.as_affine());
        let mut random_key = [0; 16];
        tea::random(&mut random_key)?;
        Ok(Self {
            app_profile: crate::AppProfile::default(),
            guid: device.guid_bytes()?,
            device_name: device.device_name.clone(),
            public_key: secret
                .public_key()
                .to_encoded_point(false)
                .as_bytes()
                .to_vec(),
            key_version: version,
            share_key: md5(&share.raw_secret_bytes()[..16]),
            random_key,
            qrsig: Vec::new(),
            ticket: None,
        })
    }

    fn seal(&self, command: u16, uin: u32, body: &[u8]) -> Result<Vec<u8>> {
        if body.len() > MAX_OICQ - 256 {
            return Err("QR login packet exceeds limit".into());
        }
        let mut out = vec![2, 0, 0];
        out.extend_from_slice(&8001_u16.to_be_bytes());
        out.extend_from_slice(&command.to_be_bytes());
        out.extend_from_slice(&1_u16.to_be_bytes());
        out.extend_from_slice(&uin.to_be_bytes());
        out.extend_from_slice(&[3, 0x87, 0]);
        out.extend_from_slice(&2_u32.to_be_bytes());
        out.extend_from_slice(&[0; 8]);
        out.extend_from_slice(&[2, 1]);
        out.extend_from_slice(&self.random_key);
        out.extend_from_slice(&0x0131_u16.to_be_bytes());
        out.extend_from_slice(&self.key_version.to_be_bytes());
        short_bytes(&mut out, &self.public_key)?;
        out.extend(tea::encrypt(body, &self.share_key)?);
        out.push(3);
        let length = u16::try_from(out.len()).map_err(|_| "QR OICQ packet exceeds limit")?;
        out[1..3].copy_from_slice(&length.to_be_bytes());
        Ok(out)
    }

    fn open(&self, expected_command: u16, packet: &[u8]) -> Result<Vec<u8>> {
        if packet.len() > MAX_OICQ || packet.len() < 33 {
            return Err("invalid QR OICQ response size".into());
        }
        let mut r = Reader::new(packet);
        if r.byte()? != 2 || read_u16(&mut r)? as usize != packet.len() {
            return Err("invalid QR OICQ response header".into());
        }
        read_u16(&mut r)?;
        if read_u16(&mut r)? != expected_command {
            return Err("unexpected QR OICQ response command".into());
        }
        read_u16(&mut r)?;
        r.u32()?;
        r.byte()?;
        let encryption = r.byte()?;
        r.byte()?;
        if encryption != 0 {
            return Err(format!(
                "unsupported QR OICQ response encryption {encryption}"
            ));
        }
        let tail = r.rest();
        if tail.last() != Some(&3) {
            return Err("invalid QR OICQ response terminator".into());
        }
        tea::decrypt(&tail[..tail.len() - 1], &self.share_key)
    }

    fn code2d(&self, command: u16, body: &[u8], now: u32) -> Result<Vec<u8>> {
        let length = u16::try_from(body.len()).map_err(|_| "QR Code2D body exceeds limit")?;
        let mut out = vec![0];
        out.extend_from_slice(
            &length
                .checked_add(53)
                .ok_or("QR Code2D length overflow")?
                .to_be_bytes(),
        );
        out.extend_from_slice(&self.app_profile.info().app_id.to_be_bytes());
        out.extend_from_slice(&0x72_u32.to_be_bytes());
        out.extend_from_slice(&[0; 3]);
        out.extend_from_slice(&now.to_be_bytes());
        out.push(2);
        out.extend_from_slice(
            &length
                .checked_add(49)
                .ok_or("QR Code2D length overflow")?
                .to_be_bytes(),
        );
        out.extend_from_slice(&command.to_be_bytes());
        out.extend_from_slice(&[0; 21]);
        out.push(3);
        out.extend_from_slice(&50_u32.to_be_bytes());
        out.extend_from_slice(&[0; 14]);
        out.extend_from_slice(&self.app_profile.info().app_id.to_be_bytes());
        out.extend_from_slice(body);
        self.seal(2066, 0, &out)
    }

    fn fetch_body(&self) -> Result<Vec<u8>> {
        let mut app = 0_u32.to_be_bytes().to_vec();
        app.extend_from_slice(&self.app_profile.info().app_id.to_be_bytes());
        app.extend_from_slice(&self.app_profile.info().sub_app_id.to_be_bytes());
        app.extend_from_slice(&self.guid);
        for s in [b"com.tencent.qq".as_slice(), b"2.0.0", b"com.tencent.qq"] {
            short_bytes(&mut app, s)?;
        }
        let mut qr = Vec::new();
        for n in [0_u32, 0, 3, 4, 72, 2, 2] {
            qr.extend_from_slice(&n.to_be_bytes());
        }
        qr.extend_from_slice(&0_u16.to_be_bytes());
        let mut misc = vec![1];
        misc.extend_from_slice(&32764_u32.to_be_bytes());
        misc.extend_from_slice(&[0; 5]);
        let mut platform = Vec::new();
        wire::bytes(1, b"Linux", &mut platform);
        wire::bytes(2, self.device_name.as_bytes(), &mut platform);
        let mut flag = Vec::new();
        wire::number(6, 1, &mut flag);
        let mut info = Vec::new();
        wire::bytes(1, &platform, &mut info);
        wire::bytes(4, &flag, &mut info);
        let entries = vec![
            (0x16, app),
            (0x1b, qr),
            (0x1d, misc),
            (0x33, self.guid.clone()),
            (0x35, 19_u32.to_be_bytes().to_vec()),
            (0x66, 19_u32.to_be_bytes().to_vec()),
            (0xd1, info),
        ];
        let mut body = vec![0; 11];
        body.extend(tlvs(&entries)?);
        body.push(3);
        Ok(body)
    }

    pub fn fetch_request(&self, now: u32) -> Result<Vec<u8>> {
        self.code2d(0x31, &self.fetch_body()?, now)
    }

    pub fn handle_fetch(&mut self, packet: &[u8]) -> Result<QrCode> {
        self.qrsig.clear();
        self.ticket = None;
        let body = self.open(2066, packet)?;
        let mut r = Reader::new(&body);
        r.take(54)?;
        let code = r.byte()?;
        if code != 0 {
            return Err(format!("QR fetch rejected ({code})"));
        }
        let signature = read_short(&mut r)?.to_vec();
        let entries = read_tlvs(&mut r)?;
        let png = required(&entries, 0x17)?.to_vec();
        let url = if let Some(value) = entries.get(&0xd1) {
            let mut r = Reader::new(value);
            r.byte()?;
            let length = r.byte()? as usize;
            String::from_utf8(r.take(length)?.to_vec()).map_err(|_| "invalid QR login URL")?
        } else {
            String::new()
        };
        if signature.is_empty() {
            return Err("missing QR signature".into());
        }
        self.qrsig = signature;
        Ok(QrCode { png, url })
    }

    pub fn poll_request(&self, now: u32) -> Result<Vec<u8>> {
        if self.qrsig.is_empty() {
            return Err("fetch QR code before polling".into());
        }
        let mut body = Vec::new();
        short_bytes(&mut body, &self.qrsig)?;
        body.extend_from_slice(&[0; 13]);
        body.push(0x83);
        self.code2d(0x12, &body, now)
    }

    pub fn handle_poll(&mut self, packet: &[u8]) -> Result<QrState> {
        if self.qrsig.is_empty() {
            return Err("fetch QR code before polling".into());
        }
        let body = self.open(2066, packet)?;
        let mut r = Reader::new(&body);
        r.take(8)?;
        if read_u16(&mut r)? != 0x12 {
            return Err("unexpected QR polling response command".into());
        }
        r.take(40)?;
        r.u32()?;
        let state = r.byte()?;
        self.ticket = None;
        match state {
            0 => {
                r.take(4)?;
                let uin = r.u32()?;
                r.take(4)?;
                if uin == 0 {
                    return Err("QR confirmation omitted account".into());
                }
                let entries = read_tlvs(&mut r)?;
                let tgtgt = required(&entries, 0x1e)?
                    .try_into()
                    .map_err(|_| "invalid QR ticket key length")?;
                self.ticket = Some(ScanTicket {
                    uin,
                    t106: required(&entries, 0x18)?.to_vec(),
                    t16a: required(&entries, 0x19)?.to_vec(),
                    tgtgt,
                });
                Ok(QrState::Confirmed { uin })
            }
            17 => {
                self.ticket = None;
                Ok(QrState::Expired)
            }
            48 => Ok(QrState::WaitingForScan),
            53 => Ok(QrState::WaitingForConfirm),
            54 => {
                self.ticket = None;
                Ok(QrState::Canceled)
            }
            other => Ok(QrState::Unknown(other)),
        }
    }

    fn login_body(&self) -> Result<Vec<u8>> {
        let ticket = self
            .ticket
            .as_ref()
            .ok_or("QR login requires confirmation")?;
        let mut version = self.app_profile.info().app_id.to_be_bytes().to_vec();
        short_bytes(&mut version, b"2.0.0")?;
        short_bytes(&mut version, b"com.tencent.qq")?;
        let mut device = vec![0, 0, 0, 1, 0, 0, 0, 0, 0];
        short_bytes(&mut device, b"Linux")?;
        short_bytes(&mut device, &self.guid)?;
        short_bytes(&mut device, b"")?;
        let protected = tlvs(&[
            (0x16e, self.device_name.as_bytes().to_vec()),
            (0x147, version),
            (0x128, device),
            (0x124, vec![0; 12]),
        ])?;
        let mut bitmap = vec![0];
        bitmap.extend_from_slice(&12_058_620_u32.to_be_bytes());
        bitmap.extend_from_slice(&[0; 5]);
        let mut package = vec![0, 0];
        short_bytes(&mut package, b"com.tencent.qq")?;
        let mut t18 = vec![0, 0];
        for value in [5_u32, 0, self.app_profile.info().client_version, ticket.uin] {
            t18.extend_from_slice(&value.to_be_bytes());
        }
        t18.extend_from_slice(&[0; 4]);
        let mut network = 7_u32.to_be_bytes().to_vec();
        network.extend_from_slice(b"Unknown");
        network.extend_from_slice(&0_u32.to_be_bytes());
        let mut sdk = vec![1, 0, 0, 0, 0];
        short_bytes(&mut sdk, b"nt.wtlogin.0.0.1")?;
        let mut t100 = vec![0, 0];
        for value in [
            5_u32,
            self.app_profile.info().app_id,
            self.app_profile.info().sub_app_id,
            8001,
            self.app_profile.info().main_sigmap,
        ] {
            t100.extend_from_slice(&value.to_be_bytes());
        }
        let mut product = 0x13_u32.to_be_bytes().to_vec();
        short_bytes(&mut product, b"basicim")?;
        let entries = vec![
            (0x106, ticket.t106.clone()),
            (0x144, tea::encrypt(&protected, &ticket.tgtgt)?),
            (0x116, bitmap),
            (0x142, package),
            (0x145, self.guid.clone()),
            (0x18, t18),
            (0x141, network),
            (0x177, sdk),
            (0x191, vec![0]),
            (0x100, t100),
            (0x107, vec![0, 1, 0x0d, 0, 0, 1]),
            (0x318, Vec::new()),
            (0x16a, ticket.t16a.clone()),
            (0x166, vec![5]),
            (0x521, product),
        ];
        let mut body = 9_u16.to_be_bytes().to_vec();
        body.extend(tlvs(&entries)?);
        Ok(body)
    }

    pub fn login_request(&self) -> Result<Vec<u8>> {
        let ticket = self
            .ticket
            .as_ref()
            .ok_or("QR login requires confirmation")?;
        self.seal(2064, ticket.uin, &self.login_body()?)
    }

    pub fn handle_login(&self, packet: &[u8]) -> Result<LoginCredentials> {
        let ticket = self
            .ticket
            .as_ref()
            .ok_or("QR login requires confirmation")?;
        let body = self.open(2064, packet)?;
        let mut r = Reader::new(&body);
        read_u16(&mut r)?;
        let code = r.byte()?;
        let entries = read_tlvs(&mut r)?;
        if code != 0 {
            // Keep useful account/device restrictions while removing URLs,
            // ticket-like ASCII strings and account/phone-sized numbers.
            let reason = login_rejection_reason(&entries)
                .map(|r| format!(": {r}"))
                .unwrap_or_default();
            return Err(format!("QR login rejected (0x{code:02x}){reason}"));
        }
        let decrypted = tea::decrypt(required(&entries, 0x119)?, &ticket.tgtgt)?;
        let mut r = Reader::new(&decrypted);
        let credentials = read_tlvs(&mut r)?;
        let d2_key: [u8; 16] = required(&credentials, 0x305)?
            .try_into()
            .map_err(|_| "invalid QR D2 key length")?;
        let uid_layer1 = Fields::parse(required(&credentials, 0x543)?)?;
        let uid_layer2 = Fields::parse(uid_layer1.data(9).ok_or("missing UID envelope")?)?;
        let uid_layer3 = Fields::parse(uid_layer2.data(11).ok_or("missing UID layer")?)?;
        let uid = String::from_utf8(uid_layer3.data(1).ok_or("missing account UID")?.to_vec())
            .map_err(|_| "invalid account UID")?;
        if uid.is_empty() {
            return Err("empty account UID".into());
        }
        let nickname = if let Some(data) = credentials.get(&0x11a) {
            let mut r = Reader::new(data);
            r.take(4)?;
            let length = r.byte()? as usize;
            String::from_utf8_lossy(r.take(length)?).into_owned()
        } else {
            String::new()
        };
        Ok(LoginCredentials {
            uin: ticket.uin,
            uid,
            tgt: required(&credentials, 0x10a)?.to_vec(),
            d2: required(&credentials, 0x143)?.to_vec(),
            d2_key,
            tgtgt: md5(&d2_key),
            temp_pwd: credentials
                .get(&0x106)
                .copied()
                .unwrap_or_default()
                .to_vec(),
            nickname,
        })
    }
}
fn login_rejection_reason(entries: &BTreeMap<u16, &[u8]>) -> Option<String> {
    for (tag, skip) in [(0x146, 4), (0x149, 2)] {
        let Some(bytes) = entries.get(&tag).filter(|b| b.len() <= 4096) else {
            continue;
        };
        let parse = (|| -> Result<String> {
            let mut r = Reader::new(bytes);
            r.take(skip)?;
            let title = std::str::from_utf8(read_short(&mut r)?)
                .map_err(|_| "invalid QR rejection title")?;
            let message = std::str::from_utf8(read_short(&mut r)?)
                .map_err(|_| "invalid QR rejection reason")?;
            Ok(format!("{title} {message}"))
        })();
        if let Ok(text) = parse {
            let safe = safe_login_reason(&text);
            if !safe.is_empty() {
                return Some(safe);
            }
        }
    }
    None
}
#[cfg(test)]
mod rejection_tests {
    use super::*;
    #[test]
    fn error_reason_preserves_restriction_and_hides_private_details() {
        let mut payload = vec![0; 4];
        short_bytes(&mut payload, "登录限制".as_bytes()).unwrap();
        short_bytes(&mut payload,"当前设备需要确认 https://verify.qq.com/?token=ABCDEFGHIJKLMNOPQRSTUVWXYZ 账号123456789 user@example.com".as_bytes()).unwrap();
        let entries = BTreeMap::from([(0x146, payload.as_slice())]);
        let reason = login_rejection_reason(&entries).unwrap();
        assert!(reason.contains("登录限制"));
        assert!(reason.contains("当前设备需要确认"));
        for private in [
            "https",
            "ABCDEFGHIJKLMNOPQRSTUVWXYZ",
            "123456789",
            "example.com",
        ] {
            assert!(!reason.contains(private))
        }
        assert!(reason.chars().count() <= 400);
        assert!(login_rejection_reason(&BTreeMap::from([(0x146, &[0u8][..])])).is_none());
    }
}
pub(crate) fn safe_login_reason(text: &str) -> String {
    let mut out = String::new();
    let mut ascii = String::new();
    fn flush(out: &mut String, s: &mut String) {
        if s.is_empty() {
            return;
        }
        let private = s.contains("://")
            || s.contains('@')
            || s.to_ascii_lowercase().starts_with("www.")
            || s.chars().filter(|c| c.is_ascii_digit()).count() >= 5
            || s.split(|c: char| !c.is_ascii_alphanumeric() && c != '_' && c != '-')
                .any(|word| word.len() >= 16);
        if private {
            out.push_str("[redacted]")
        } else {
            out.push_str(s)
        }
        s.clear();
    }
    for ch in text.chars().take(4096) {
        if ch.is_ascii_graphic() {
            ascii.push(ch)
        } else {
            flush(&mut out, &mut ascii);
            if ch.is_control() || ch.is_whitespace() {
                if !out.ends_with(' ') {
                    out.push(' ')
                }
            } else {
                out.push(ch)
            }
        }
    }
    flush(&mut out, &mut ascii);
    out.trim().chars().take(400).collect()
}
