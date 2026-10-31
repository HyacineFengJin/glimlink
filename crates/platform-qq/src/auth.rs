// SPDX-License-Identifier: AGPL-3.0-only
// Adapted from the Agent Runtime QQ Rust experiment; protocol references in NOTICE.
//! NTQQ TempPwd authentication, without network or account-state access.
//!
//! Reference: LagrangeGo f7f8abfaa7f1, packets/wtlogin/exchange.go,
//! packets/pb/login/{ecdh,ntlogin}.pb.go, client/ntlogin.go and crypto/aes.go.
//! The caller owns connect -> key exchange -> EasyLogin -> Register -> save.
//! A challenge is returned as data; this module does not bypass or solve it.
use crate::{
    Result, tea,
    wire::{self, Fields},
};
use aes_gcm::{Aes128Gcm, Aes256Gcm, KeyInit, Nonce, aead::Aead};
use p256::{PublicKey, SecretKey, ecdh::diffie_hellman, elliptic_curve::sec1::ToEncodedPoint};
use sha2::{Digest, Sha256};
use std::time::{SystemTime, UNIX_EPOCH};

type Aes192Gcm = aes_gcm::AesGcm<aes_gcm::aes::Aes192, aes_gcm::aead::consts::U12>;

pub const KEY_EXCHANGE_COMMAND: &str = "trpc.login.ecdh.EcdhService.SsoKeyExchange";
pub const PASSWORD_LOGIN_COMMAND: &str = "trpc.login.ecdh.EcdhService.SsoNTLoginPasswordLogin";
pub const PASSWORD_NEW_DEVICE_COMMAND: &str =
    "trpc.login.ecdh.EcdhService.SsoNTLoginPasswordLoginNewDevice";
pub const EASY_LOGIN_COMMAND: &str = "trpc.login.ecdh.EcdhService.SsoNTLoginEasyLogin";
const MAX_AUTH_FRAME: usize = 128 * 1024;
const MAX_TOKEN: usize = 16 * 1024;
// Public protocol constants from the pinned implementation, not account keys.
const SERVER_PUBLIC_HEX: &str = "049d1423332735980edabe7e9ea451b3395b6f35250db8fc56f25889f628cbae3e8e73077914071eeebc108f4e0170057792bb17aa303af652313d17c1ac815e79";
const HASH_KEY: [u8; 32] = [
    0xe2, 0x73, 0x3b, 0xf4, 0x03, 0x14, 0x99, 0x13, 0xcb, 0xf8, 0x0c, 0x7a, 0x95, 0x16, 0x8b, 0xd4,
    0xca, 0x69, 0x35, 0xee, 0x53, 0xcd, 0x39, 0x76, 0x4b, 0xee, 0xbe, 0x2e, 0x00, 0x7e, 0x3a, 0xee,
];

// Deliberately no Debug/Serialize for structs that contain credential material.
pub struct KeyExchange {
    secret: SecretKey,
}
pub struct ExchangeCredentials {
    pub exchange_key: Vec<u8>,
    pub key_sig: Vec<u8>,
    pub expiration: u32,
}
pub struct LoginIdentity<'a> {
    pub uin: u32,
    pub guid: &'a [u8],
    pub device_name: &'a str,
    pub kernel_version: &'a str,
    pub cookies: Option<&'a str>,
    pub app_profile: crate::AppProfile,
}
pub struct LoginCredentials {
    pub tgt: Vec<u8>,
    pub d2: Vec<u8>,
    pub d2_key: Vec<u8>,
    pub temp_pwd: Vec<u8>,
    pub uid: Option<String>,
}
pub enum LoginOutcome {
    Success(LoginCredentials),
    VerificationRequired(LoginChallenge),
    Rejected(LoginFailure),
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ChallengeKind {
    Captcha,
    NewDevice,
    Unusual,
}
pub struct LoginChallenge {
    pub code: u32,
    pub kind: ChallengeKind,
    // URLs/cookies/signatures may contain credentials. Keep out of public logs.
    pub verify_url: Option<String>,
    pub cookies: Option<String>,
    pub unusual_sig: Vec<u8>,
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum LoginFailureKind {
    TokenExpired,
    UserTokenExpired,
    SecurityRestricted,
    /// Native LOGIN_ERROR_SEC_BEAT. This name does not identify the precise
    /// rejected security check, an incorrect CAPTCHA, or an account restriction.
    SecBeatRejected,
    WrongArgument,
    ServerFailure,
    LoginFailure,
    Unknown,
}
#[derive(Debug)]
pub struct LoginFailure {
    pub code: u32,
    pub kind: LoginFailureKind,
    pub reason: Option<String>,
}

/// Hash explicit operator password input locally; callers must not log it.
pub fn password_md5(value: &[u8]) -> [u8; 16] {
    use md5::Md5;
    Md5::digest(value).into()
}

fn unhex(value: &str) -> Result<Vec<u8>> {
    if value.len() % 2 != 0 || !value.is_ascii() {
        return Err("invalid protocol hex constant".into());
    }
    (0..value.len())
        .step_by(2)
        .map(|at| {
            u8::from_str_radix(&value[at..at + 2], 16)
                .map_err(|_| "invalid protocol hex constant".into())
        })
        .collect()
}
fn token(value: &[u8]) -> Result<()> {
    if value.is_empty() || value.len() > MAX_TOKEN {
        return Err("missing or oversized authentication material".into());
    }
    Ok(())
}
fn fields(data: &[u8]) -> Result<Fields<'_>> {
    if data.len() > MAX_AUTH_FRAME {
        return Err("authentication response exceeds limit".into());
    }
    Fields::parse(data)
}
fn required<'a>(f: &Fields<'a>, field: u32) -> Result<&'a [u8]> {
    f.data(field)
        .ok_or_else(|| "authentication field missing".into())
}
fn string(data: Option<&[u8]>) -> Result<Option<String>> {
    data.map(|v| {
        if v.len() > MAX_TOKEN {
            return Err("authentication text exceeds limit".into());
        }
        String::from_utf8(v.to_vec()).map_err(|_| "invalid authentication text".into())
    })
    .transpose()
}
fn seal_with_nonce(plain: &[u8], key: &[u8], nonce: &[u8; 12]) -> Result<Vec<u8>> {
    let cipher = match key.len() {
        16 => Aes128Gcm::new_from_slice(key)
            .map_err(|_| "invalid AES key")?
            .encrypt(Nonce::from_slice(nonce), plain),
        24 => Aes192Gcm::new_from_slice(key)
            .map_err(|_| "invalid AES key")?
            .encrypt(Nonce::from_slice(nonce), plain),
        32 => Aes256Gcm::new_from_slice(key)
            .map_err(|_| "invalid AES key")?
            .encrypt(Nonce::from_slice(nonce), plain),
        _ => return Err("authentication AES key must be 16, 24 or 32 bytes".into()),
    }
    .map_err(|_| "cannot encrypt authentication frame")?;
    let mut out = nonce.to_vec();
    out.extend(cipher);
    Ok(out)
}
fn seal(plain: &[u8], key: &[u8]) -> Result<Vec<u8>> {
    let mut nonce = [0; 12];
    tea::random(&mut nonce)?;
    seal_with_nonce(plain, key, &nonce)
}
fn open(data: &[u8], key: &[u8]) -> Result<Vec<u8>> {
    // Go's helper slices without checking. Reject truncated nonce/tag safely.
    if !(28..=MAX_AUTH_FRAME).contains(&data.len()) {
        return Err("invalid authentication GCM frame length".into());
    }
    let nonce = Nonce::from_slice(&data[..12]);
    match key.len() {
        16 => Aes128Gcm::new_from_slice(key)
            .map_err(|_| "invalid AES key")?
            .decrypt(nonce, &data[12..]),
        24 => Aes192Gcm::new_from_slice(key)
            .map_err(|_| "invalid AES key")?
            .decrypt(nonce, &data[12..]),
        32 => Aes256Gcm::new_from_slice(key)
            .map_err(|_| "invalid AES key")?
            .decrypt(nonce, &data[12..]),
        _ => return Err("authentication AES key must be 16, 24 or 32 bytes".into()),
    }
    .map_err(|_| "authentication GCM verification failed".into())
}

impl KeyExchange {
    pub fn new() -> Result<Self> {
        for _ in 0..128 {
            let mut bytes = [0; 32];
            tea::random(&mut bytes)?;
            let secret = SecretKey::from_slice(&bytes);
            bytes.fill(0);
            if let Ok(secret) = secret {
                return Ok(Self { secret });
            }
        }
        Err("cannot generate authentication ephemeral key".into())
    }
    pub fn public_key(&self) -> Vec<u8> {
        self.secret
            .public_key()
            .to_encoded_point(false)
            .as_bytes()
            .to_vec()
    }
    fn shared(&self, remote: &[u8]) -> Result<[u8; 32]> {
        // Go crypto/ecdh accepts only the uncompressed SEC1 form here.
        if remote.len() != 65 || remote[0] != 4 {
            return Err("invalid authentication P-256 public key encoding".into());
        }
        let remote =
            PublicKey::from_sec1_bytes(remote).map_err(|_| "invalid authentication P-256 point")?;
        let shared = diffie_hellman(self.secret.to_nonzero_scalar(), remote.as_affine());
        Ok((*shared.raw_secret_bytes()).into())
    }
    pub fn request(&self, uin: u32, guid: &[u8]) -> Result<Vec<u8>> {
        let now = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map_err(|_| "invalid authentication clock")?
            .as_secs();
        let timestamp = u32::try_from(now).map_err(|_| "authentication timestamp overflow")?;
        self.request_at(uin, guid, timestamp)
    }
    fn request_at(&self, uin: u32, guid: &[u8], timestamp: u32) -> Result<Vec<u8>> {
        if uin == 0 || guid.len() != 16 {
            return Err("invalid login identity".into());
        }
        let mut plain = Vec::new();
        wire::bytes(1, uin.to_string().as_bytes(), &mut plain);
        wire::bytes(2, guid, &mut plain);
        let encrypted = seal(&plain, &self.shared(&unhex(SERVER_PUBLIC_HEX)?)?)?;
        let public_key = self.public_key();
        let mut hash = Sha256::new();
        hash.update(&public_key);
        hash.update(1_u32.to_be_bytes());
        hash.update(&encrypted);
        hash.update(0_u32.to_be_bytes());
        hash.update(timestamp.to_be_bytes());
        let proof = seal(&hash.finalize(), &HASH_KEY)?;
        let mut request = Vec::new();
        wire::bytes(1, &public_key, &mut request);
        wire::number(2, 1, &mut request);
        wire::bytes(3, &encrypted, &mut request);
        wire::number(4, timestamp.into(), &mut request);
        wire::bytes(5, &proof, &mut request);
        Ok(request)
    }
    pub fn parse_response(&self, response: &[u8]) -> Result<ExchangeCredentials> {
        let response = fields(response)?;
        let shared = self.shared(required(&response, 3)?)?;
        let plain = open(required(&response, 1)?, &shared)?;
        let decoded = fields(&plain)?;
        let exchange_key = required(&decoded, 1)?.to_vec();
        if !matches!(exchange_key.len(), 16 | 24 | 32) {
            return Err("invalid exchanged AES key length".into());
        }
        let key_sig = required(&decoded, 2)?.to_vec();
        token(&key_sig)?;
        let expiration = u32::try_from(decoded.num(3).unwrap_or(0))
            .map_err(|_| "invalid exchange expiration")?;
        Ok(ExchangeCredentials {
            exchange_key,
            key_sig,
            expiration,
        })
    }
}

/// Credential, header and challenge framing shared by native login methods.
pub fn build_easy_login_request(
    identity: &LoginIdentity<'_>,
    temp_pwd: &[u8],
    exchange: &ExchangeCredentials,
) -> Result<Vec<u8>> {
    build_login_request(identity, temp_pwd, exchange, None)
}
pub struct CaptchaSubmit<'a> {
    pub ticket: &'a str,
    pub randstr: &'a str,
    /// Original login challenge URL `sid`, not the TencentCaptcha widget `aid`.
    /// Pinned Go calls protobuf field3 `Aid`, but SubmitCaptcha passes `sid`.
    pub session_id: &'a str,
}
/// Password material is encrypted as in the pinned client/ntlogin.go. It never
/// appears as plaintext or a hash in the outer protobuf/login transport.
pub fn build_password_login_request(
    identity: &LoginIdentity<'_>,
    password_md5: &[u8; 16],
    exchange: &ExchangeCredentials,
    captcha: Option<&CaptchaSubmit<'_>>,
) -> Result<Vec<u8>> {
    let mut entropy = [0u8; 20];
    tea::random(&mut entropy)?;
    let credential =
        password_credential_at(identity, password_md5, crate::unix_seconds()?, &entropy)?;
    build_login_request(identity, &credential, exchange, captcha)
}
fn password_credential_at(
    identity: &LoginIdentity<'_>,
    password_md5: &[u8; 16],
    now: u32,
    entropy: &[u8; 20],
) -> Result<Vec<u8>> {
    if identity.uin == 0 || identity.guid.len() != 16 {
        return Err("invalid password login identity".into());
    }
    let mut key = Vec::from(password_md5.as_slice());
    key.extend_from_slice(&[0; 4]);
    key.extend_from_slice(&identity.uin.to_be_bytes());
    let mut plain = 4u16.to_be_bytes().to_vec();
    plain.extend_from_slice(&entropy[..4]);
    for n in [
        0,
        identity.app_profile.info().app_id,
        8001,
        0,
        identity.uin,
        now,
        0,
    ] {
        plain.extend_from_slice(&n.to_be_bytes());
    }
    plain.push(1);
    plain.extend_from_slice(password_md5);
    plain.extend_from_slice(&entropy[4..]);
    plain.extend_from_slice(&0u32.to_be_bytes());
    plain.push(1);
    plain.extend_from_slice(identity.guid);
    plain.extend_from_slice(&1u32.to_be_bytes());
    plain.extend_from_slice(&1u32.to_be_bytes());
    let uin = identity.uin.to_string();
    plain.extend_from_slice(&(uin.len() as u16).to_be_bytes());
    plain.extend_from_slice(uin.as_bytes());
    use md5::Md5;
    tea::encrypt(&plain, &Md5::digest(&key))
}
/// This does not solve CAPTCHA or new-device verification. They remain explicit
/// operator-managed inputs bound to the pending login exchange.
pub fn build_login_request(
    identity: &LoginIdentity<'_>,
    credential: &[u8],
    exchange: &ExchangeCredentials,
    captcha: Option<&CaptchaSubmit<'_>>,
) -> Result<Vec<u8>> {
    if identity.uin == 0
        || identity.guid.len() != 16
        || identity.device_name.is_empty()
        || identity.device_name.len() > 4096
        || identity.kernel_version.len() > 4096
    {
        return Err("invalid login identity".into());
    }
    token(credential)?;
    token(&exchange.key_sig)?;
    if identity.cookies.is_some_and(|v| v.len() > MAX_TOKEN) {
        return Err("authentication cookie exceeds limit".into());
    }
    let mut uin = Vec::new();
    wire::bytes(1, identity.uin.to_string().as_bytes(), &mut uin);
    let mut system = Vec::new();
    wire::bytes(1, identity.app_profile.info().os.as_bytes(), &mut system);
    wire::bytes(2, identity.device_name.as_bytes(), &mut system);
    wire::number(3, 1, &mut system);
    wire::bytes(4, identity.guid, &mut system);
    let mut version = Vec::new();
    wire::bytes(1, identity.kernel_version.as_bytes(), &mut version);
    wire::number(2, identity.app_profile.info().app_id.into(), &mut version);
    wire::bytes(
        3,
        identity.app_profile.info().package_name.as_bytes(),
        &mut version,
    );
    let mut cookie = Vec::new();
    wire::bytes(1, identity.cookies.unwrap_or("").as_bytes(), &mut cookie);
    let mut header = Vec::new();
    wire::bytes(1, &uin, &mut header);
    wire::bytes(2, &system, &mut header);
    wire::bytes(3, &version, &mut header);
    wire::bytes(5, &cookie, &mut header);
    let mut body = Vec::new();
    wire::bytes(1, credential, &mut body);
    if let Some(captcha) = captcha {
        let mut submit = Vec::new();
        for (tag, value) in [
            (1, captcha.ticket),
            (2, captcha.randstr),
            (3, captcha.session_id),
        ] {
            if value.is_empty() || value.len() > 8192 {
                return Err("missing or oversized CAPTCHA submission".into());
            }
            wire::bytes(tag, value.as_bytes(), &mut submit)
        }
        wire::bytes(2, &submit, &mut body);
    }
    let mut base = Vec::new();
    wire::bytes(1, &header, &mut base);
    wire::bytes(2, &body, &mut base);
    let mut nonce = [0u8; 12];
    tea::random(&mut nonce)?;
    encrypt_login_request(&base, exchange, &nonce)
}
fn encrypt_login_request(
    base: &[u8],
    exchange: &ExchangeCredentials,
    nonce: &[u8; 12],
) -> Result<Vec<u8>> {
    let encrypted = seal_with_nonce(base, &exchange.exchange_key, nonce)?;
    let mut request = Vec::new();
    wire::bytes(1, &exchange.key_sig, &mut request);
    wire::bytes(3, &encrypted, &mut request);
    wire::number(4, 1, &mut request);
    Ok(request)
}

pub fn parse_easy_login_response(
    response: &[u8],
    exchange: &ExchangeCredentials,
) -> Result<LoginOutcome> {
    let frame = fields(response)?;
    let plain = open(required(&frame, 3)?, &exchange.exchange_key)?;
    let base = fields(&plain)?;
    let header = fields(base.data(1).unwrap_or_default())?;
    let body = fields(base.data(2).unwrap_or_default())?;
    let error = header.data(4).map(fields).transpose()?;
    let code = u32::try_from(error.as_ref().and_then(|f| f.num(1)).unwrap_or(0))
        .map_err(|_| "invalid authentication error code")?;
    if let Some(raw) = body.data(1) {
        if code != 0 {
            return Err("conflicting authentication success and error".into());
        }
        let credentials = fields(raw)?;
        let tgt = required(&credentials, 4)?.to_vec();
        let d2 = required(&credentials, 5)?.to_vec();
        let d2_key = required(&credentials, 6)?.to_vec();
        let temp_pwd = required(&credentials, 3)?.to_vec();
        for value in [&tgt, &d2, &temp_pwd] {
            token(value)?;
        }
        if d2_key.len() != 16 {
            return Err("invalid login D2Key length".into());
        }
        let uid = body.data(4).map(fields).transpose()?;
        let uid = string(uid.as_ref().and_then(|f| f.data(2)))?.filter(|s| !s.is_empty());
        return Ok(LoginOutcome::Success(LoginCredentials {
            tgt,
            d2,
            d2_key,
            temp_pwd,
            uid,
        }));
    }
    if error.is_none() || code == 0 {
        return Err("authentication response has no result".into());
    }
    let kind = match code {
        140_022_008 => Some(ChallengeKind::Captcha),
        140_022_010 => Some(ChallengeKind::NewDevice),
        140_022_011 => Some(ChallengeKind::Unusual),
        _ => None,
    };
    if let Some(kind) = kind {
        let captcha = body.data(2).map(fields).transpose()?;
        let verify_url = if kind == ChallengeKind::Captcha {
            string(captcha.as_ref().and_then(|f| f.data(3)))?
        } else {
            string(error.as_ref().and_then(|f| f.data(5)))?
        };
        let cookie = header.data(5).map(fields).transpose()?;
        let cookies = string(cookie.as_ref().and_then(|f| f.data(1)))?;
        let unusual = body.data(3).map(fields).transpose()?;
        let unusual_sig = unusual
            .as_ref()
            .and_then(|f| f.data(2))
            .unwrap_or_default()
            .to_vec();
        if unusual_sig.len() > MAX_TOKEN {
            return Err("verification signature exceeds limit".into());
        }
        return Ok(LoginOutcome::VerificationRequired(LoginChallenge {
            code,
            kind,
            verify_url,
            cookies,
            unusual_sig,
        }));
    }
    let kind = match code {
        140_022_015 => LoginFailureKind::TokenExpired,
        140_022_016 => LoginFailureKind::UserTokenExpired,
        // LagrangeGo's older enumWrongCaptcha label is misleading. The native
        // NTLogin enum calls this LOGIN_ERROR_STRICT; server tips take priority.
        140_022_007 => LoginFailureKind::SecurityRestricted,
        140_022_017 => LoginFailureKind::SecBeatRejected,
        140_022_001 => LoginFailureKind::WrongArgument,
        140_022_002 => LoginFailureKind::ServerFailure,
        140_022_013 => LoginFailureKind::LoginFailure,
        _ => LoginFailureKind::Unknown,
    };
    let title = string(error.as_ref().and_then(|f| f.data(2)))?.unwrap_or_default();
    let message = string(error.as_ref().and_then(|f| f.data(3)))?.unwrap_or_default();
    let safe = crate::qr::safe_login_reason(&format!("{title} {message}"));
    Ok(LoginOutcome::Rejected(LoginFailure {
        code,
        kind,
        reason: (!safe.is_empty()).then_some(safe),
    }))
}

#[cfg(test)]
mod tests {
    use super::*;
    fn scalar(n: u8) -> KeyExchange {
        let mut bytes = [0; 32];
        bytes[31] = n;
        KeyExchange {
            secret: SecretKey::from_slice(&bytes).unwrap(),
        }
    }
    fn exchange() -> ExchangeCredentials {
        ExchangeCredentials {
            exchange_key: vec![0x33; 32],
            key_sig: b"synthetic-signature".to_vec(),
            expiration: 123,
        }
    }
    fn response(header: &[u8], body: &[u8]) -> Vec<u8> {
        let mut base = Vec::new();
        wire::bytes(1, header, &mut base);
        wire::bytes(2, body, &mut base);
        let encrypted = seal_with_nonce(&base, &exchange().exchange_key, &[7; 12]).unwrap();
        let mut frame = Vec::new();
        wire::bytes(3, &encrypted, &mut frame);
        frame
    }
    #[test]
    fn nist_gcm_vector_and_tamper_detection() {
        // NIST SP800-38D: empty message, zero AES-128 key and zero nonce.
        let expected = unhex("00000000000000000000000058e2fccefa7e3061367f1d57a4e7455a").unwrap();
        assert_eq!(seal_with_nonce(b"", &[0; 16], &[0; 12]).unwrap(), expected);
        assert!(open(&expected, &[0; 16]).unwrap().is_empty());
        let mut damaged = expected;
        damaged[12] ^= 1;
        assert!(open(&damaged, &[0; 16]).is_err());
        for n in 0..28 {
            assert!(open(&vec![0; n], &[0; 16]).is_err());
        }
    }
    #[test]
    fn p256_matches_known_generator_and_raw_shared_x() {
        let key = scalar(1);
        let generator = unhex("046b17d1f2e12c4247f8bce6e563a440f277037d812deb33a0f4a13945d898c2964fe342e2fe1a7f9b8ee7eb4a7c0f9e162bce33576b315ececbb6406837bf51f5").unwrap();
        assert_eq!(key.public_key(), generator);
        let remote = unhex(SERVER_PUBLIC_HEX).unwrap();
        assert_eq!(key.shared(&remote).unwrap().as_slice(), &remote[1..33]);
        assert_eq!(
            key.shared(&scalar(2).public_key()).unwrap(),
            scalar(2).shared(&key.public_key()).unwrap()
        );
        assert!(key.shared(&[4; 65]).is_err());
        assert!(key.shared(&[2; 33]).is_err());
    }
    #[test]
    fn key_exchange_request_identity_and_authenticated_proof() {
        let key = scalar(1);
        let request = key.request_at(123456, &[0x44; 16], 1_700_000_000).unwrap();
        let f = fields(&request).unwrap();
        assert_eq!(f.num(2), Some(1));
        assert_eq!(f.num(4), Some(1_700_000_000));
        let static_public = unhex(SERVER_PUBLIC_HEX).unwrap();
        let plain = open(f.data(3).unwrap(), &static_public[1..33]).unwrap();
        let plain = fields(&plain).unwrap();
        assert_eq!(plain.data(1).unwrap(), b"123456");
        assert_eq!(plain.data(2).unwrap(), &[0x44; 16]);
        let mut proof = Vec::new();
        proof.extend_from_slice(f.data(1).unwrap());
        proof.extend(1_u32.to_be_bytes());
        proof.extend(f.data(3).unwrap());
        proof.extend(0_u32.to_be_bytes());
        proof.extend(1_700_000_000_u32.to_be_bytes());
        assert_eq!(
            open(f.data(5).unwrap(), &HASH_KEY).unwrap(),
            Sha256::digest(&proof).to_vec()
        );
    }
    #[test]
    fn key_exchange_response_uses_ephemeral_peer_key_and_validates_payload() {
        let (client, server) = (scalar(1), scalar(2));
        let mut plain = Vec::new();
        wire::bytes(1, &[0x33; 32], &mut plain);
        wire::bytes(2, b"key-signature", &mut plain);
        wire::number(3, 123456, &mut plain);
        let encrypted = seal(&plain, &server.shared(&client.public_key()).unwrap()).unwrap();
        let mut reply = Vec::new();
        wire::bytes(1, &encrypted, &mut reply);
        wire::bytes(3, &server.public_key(), &mut reply);
        let result = client.parse_response(&reply).unwrap();
        assert_eq!(result.exchange_key, vec![0x33; 32]);
        assert_eq!(result.key_sig, b"key-signature");
        assert_eq!(result.expiration, 123456);
        assert!(scalar(3).parse_response(&reply).is_err());
        assert!(client.parse_response(&[]).is_err());
    }
    #[test]
    fn easy_login_request_preserves_binary_token_and_profile() {
        let identity = LoginIdentity {
            uin: 123456,
            guid: &[0x22; 16],
            device_name: "synthetic-device",
            kernel_version: "6.1",
            cookies: Some("synthetic-cookie"),
            app_profile: crate::AppProfile::default(),
        };
        let request = build_easy_login_request(&identity, &[0, 0xff, 7], &exchange()).unwrap();
        let frame = fields(&request).unwrap();
        assert_eq!(frame.data(1).unwrap(), b"synthetic-signature");
        assert_eq!(frame.num(4), Some(1));
        let base = open(frame.data(3).unwrap(), &exchange().exchange_key).unwrap();
        let base = fields(&base).unwrap();
        let header = fields(base.data(1).unwrap()).unwrap();
        assert_eq!(
            fields(header.data(1).unwrap()).unwrap().data(1).unwrap(),
            b"123456"
        );
        let system = fields(header.data(2).unwrap()).unwrap();
        assert_eq!(system.data(1).unwrap(), b"Linux");
        assert_eq!(system.num(3), Some(1));
        assert_eq!(system.data(4).unwrap(), &[0x22; 16]);
        assert_eq!(
            fields(header.data(3).unwrap()).unwrap().num(2),
            Some(1_600_001_615)
        );
        assert_eq!(
            fields(header.data(5).unwrap()).unwrap().data(1).unwrap(),
            b"synthetic-cookie"
        );
        assert_eq!(
            fields(base.data(2).unwrap()).unwrap().data(1).unwrap(),
            &[0, 0xff, 7]
        );
        assert!(build_easy_login_request(&identity, &[], &exchange()).is_err());
    }
    #[test]
    fn easy_login_success_requires_complete_new_credentials() {
        let mut credentials = Vec::new();
        for (field, value) in [
            (3, b"temp".as_slice()),
            (4, b"tgt"),
            (5, b"d2"),
            (6, &[7; 16]),
        ] {
            wire::bytes(field, value, &mut credentials);
        }
        let mut body = Vec::new();
        wire::bytes(1, &credentials, &mut body);
        let mut uid = Vec::new();
        wire::bytes(2, b"synthetic-uid", &mut uid);
        wire::bytes(4, &uid, &mut body);
        match parse_easy_login_response(&response(&[], &body), &exchange()).unwrap() {
            LoginOutcome::Success(c) => {
                assert_eq!(c.d2_key, vec![7; 16]);
                assert_eq!(c.temp_pwd, b"temp");
                assert_eq!(c.uid.as_deref(), Some("synthetic-uid"));
            }
            _ => panic!("expected synthetic login success"),
        }
        assert!(parse_easy_login_response(&response(&[], &[]), &exchange()).is_err());
        assert!(parse_easy_login_response(&response(&[], &[0x0a, 0]), &exchange()).is_err());
        assert!(parse_easy_login_response(&[], &exchange()).is_err());
    }
    #[test]
    fn challenges_are_typed_and_never_mistaken_for_success() {
        for (code, expected) in [
            (140_022_008, ChallengeKind::Captcha),
            (140_022_010, ChallengeKind::NewDevice),
            (140_022_011, ChallengeKind::Unusual),
        ] {
            let mut error = Vec::new();
            wire::number(1, code, &mut error);
            wire::bytes(5, b"https://invalid.example/verify", &mut error);
            let mut header = Vec::new();
            wire::bytes(4, &error, &mut header);
            let mut captcha = Vec::new();
            wire::bytes(3, b"https://invalid.example/captcha", &mut captcha);
            let mut body = Vec::new();
            wire::bytes(2, &captcha, &mut body);
            match parse_easy_login_response(&response(&header, &body), &exchange()).unwrap() {
                LoginOutcome::VerificationRequired(c) => {
                    assert_eq!(c.kind, expected);
                    assert!(c.verify_url.is_some());
                }
                _ => panic!("challenge must require user action"),
            }
        }
        let mut error = Vec::new();
        wire::number(1, 140_022_015, &mut error);
        let mut header = Vec::new();
        wire::bytes(4, &error, &mut header);
        match parse_easy_login_response(&response(&header, &[]), &exchange()).unwrap() {
            LoginOutcome::Rejected(f) => assert_eq!(f.kind, LoginFailureKind::TokenExpired),
            _ => panic!("expired token must not succeed"),
        }
    }
}

#[cfg(test)]
mod password_vectors {
    use super::*;
    fn vector(name: &str) -> Vec<u8> {
        let v: serde_json::Value =
            serde_json::from_str(include_str!("../tests/auth-reference.json")).unwrap();
        unhex(v[name].as_str().unwrap()).unwrap()
    }
    fn identity() -> LoginIdentity<'static> {
        LoginIdentity {
            uin: 12345678,
            guid: &[0, 1, 2, 3, 4, 5, 6, 7, 8, 9, 10, 11, 12, 13, 14, 15],
            device_name: "synthetic-device",
            kernel_version: "6.1",
            cookies: Some("synthetic-cookie"),
            app_profile: crate::AppProfile::Linux32646494,
        }
    }
    #[test]
    fn password_credential_matches_independent_pinned_go_plaintext() {
        let identity = identity();
        let mut entropy = [0u8; 20];
        entropy[..4].copy_from_slice(&[1, 2, 3, 4]);
        for (n, value) in entropy[4..].iter_mut().enumerate() {
            *value = n as u8
        }
        let protected = password_credential_at(&identity, &[17; 16], 1700000000, &entropy).unwrap();
        let mut key = vec![17u8; 16];
        key.extend([0; 4]);
        key.extend(identity.uin.to_be_bytes());
        let plain = tea::decrypt(&protected, &md5::Md5::digest(&key)).unwrap();
        assert_eq!(plain, vector("password_plain"));
        assert_eq!(
            password_md5(b"password").as_slice(),
            unhex("5f4dcc3b5aa765d61d8327deb882cf99").unwrap()
        );
    }
    #[test]
    fn captcha_and_native_header_match_independent_go_protobuf() {
        let exchange = ExchangeCredentials {
            exchange_key: vec![7; 16],
            key_sig: b"synthetic-key-signature".to_vec(),
            expiration: 300,
        };
        let captcha = CaptchaSubmit {
            ticket: "synthetic-ticket",
            randstr: "synthetic-rand",
            session_id: "208811",
        };
        let raw = build_login_request(
            &identity(),
            b"synthetic-credential",
            &exchange,
            Some(&captcha),
        )
        .unwrap();
        let encrypted = Fields::parse(&raw).unwrap();
        let plain = open(encrypted.data(3).unwrap(), &exchange.exchange_key).unwrap();
        assert_eq!(plain, vector("login_with_captcha"));
        let bad = CaptchaSubmit {
            ticket: "",
            ..captcha
        };
        assert!(
            build_login_request(&identity(), b"synthetic-credential", &exchange, Some(&bad))
                .is_err()
        );
    }
    #[test]
    fn complete_native_password_frames_match_real_pinned_go_builder() {
        let vectors: serde_json::Value =
            serde_json::from_str(include_str!("../tests/auth-full-reference.json")).unwrap();
        for (version, profile) in [
            ("3.2.19-39038", crate::AppProfile::Linux31939038),
            ("3.2.26-46494", crate::AppProfile::Linux32646494),
        ] {
            for with_captcha in [false, true] {
                let suffix = if with_captcha { "captcha" } else { "initial" };
                let raw = unhex(vectors[format!("{version}/{suffix}")].as_str().unwrap()).unwrap();
                let frame = fields(&raw).unwrap();
                let exchange = ExchangeCredentials {
                    exchange_key: vec![7; 16],
                    key_sig: b"synthetic-key-signature".to_vec(),
                    expiration: 300,
                };
                let go_gcm = frame.data(3).unwrap();
                let go_base = open(go_gcm, &exchange.exchange_key).unwrap();
                let base = fields(&go_base).unwrap();
                let body = fields(base.data(2).unwrap()).unwrap();
                let go_credential = body.data(1).unwrap();
                let mut id = identity();
                id.app_profile = profile;
                let captcha = with_captcha.then_some(CaptchaSubmit {
                    ticket: "synthetic-ticket",
                    randstr: "synthetic-rand",
                    session_id: "synthetic%2Blogin-session",
                });
                let ours =
                    build_login_request(&id, go_credential, &exchange, captcha.as_ref()).unwrap();
                let ours = fields(&ours).unwrap();
                let our_base = open(ours.data(3).unwrap(), &exchange.exchange_key).unwrap();
                assert_eq!(
                    our_base, go_base,
                    "complete native protobuf {version}/{suffix}"
                );
                // Compare every byte of the full encrypted upstream frame using
                // its public synthetic nonce; production uses a fresh random one.
                assert_eq!(
                    encrypt_login_request(&our_base, &exchange, go_gcm[..12].try_into().unwrap())
                        .unwrap(),
                    raw
                );
                let mut key = vec![17; 16];
                key.extend([0; 4]);
                key.extend(id.uin.to_be_bytes());
                let go_plain = tea::decrypt(go_credential, &md5::Md5::digest(&key)).unwrap();
                let mut entropy = [0u8; 20];
                entropy[..4].copy_from_slice(&go_plain[2..6]);
                entropy[4..].copy_from_slice(&go_plain[51..67]);
                let now = u32::from_be_bytes(go_plain[26..30].try_into().unwrap());
                let ours = password_credential_at(&id, &[17; 16], now, &entropy).unwrap();
                assert_eq!(
                    tea::decrypt(&ours, &md5::Md5::digest(&key)).unwrap(),
                    go_plain
                );
                if with_captcha {
                    let proof = fields(body.data(2).unwrap()).unwrap();
                    assert_eq!(proof.data(3).unwrap(), b"synthetic%2Blogin-session");
                    assert_ne!(
                        proof.data(3).unwrap(),
                        vectors["widget_app_id"].as_str().unwrap().as_bytes()
                    );
                }
            }
        }
    }
}

#[cfg(test)]
mod missing_body_test {
    use super::*;
    #[test]
    fn header_only_error_keeps_typed_failure() {
        let mut error = Vec::new();
        wire::number(1, 140022013, &mut error);
        wire::bytes(2, b"login failed", &mut error);
        let mut header = Vec::new();
        wire::bytes(4, &error, &mut header);
        let mut base = Vec::new();
        wire::bytes(1, &header, &mut base);
        let exchange = ExchangeCredentials {
            exchange_key: vec![7; 16],
            key_sig: vec![2],
            expiration: 300,
        };
        let mut frame = Vec::new();
        wire::bytes(3, &seal(&base, &exchange.exchange_key).unwrap(), &mut frame);
        match parse_easy_login_response(&frame, &exchange).unwrap() {
            LoginOutcome::Rejected(failure) => {
                assert_eq!(failure.code, 140022013);
                assert_eq!(failure.reason.as_deref(), Some("login failed"));
            }
            _ => panic!("header-only failure cannot succeed"),
        }
    }
    #[test]
    fn strict_rejection_does_not_blame_the_human_captcha_answer() {
        let detail = "当前账号存在安全风险，请使用常用设备登录或通过手机QQ扫码登录。";
        let mut error = Vec::new();
        wire::number(1, 140022007, &mut error);
        wire::bytes(2, "安全提醒".as_bytes(), &mut error);
        wire::bytes(3, detail.as_bytes(), &mut error);
        let mut header = Vec::new();
        wire::bytes(4, &error, &mut header);
        let mut base = Vec::new();
        wire::bytes(1, &header, &mut base);
        let exchange = ExchangeCredentials {
            exchange_key: vec![7; 16],
            key_sig: vec![2],
            expiration: 300,
        };
        let mut frame = Vec::new();
        wire::bytes(3, &seal(&base, &exchange.exchange_key).unwrap(), &mut frame);
        let LoginOutcome::Rejected(failure) = parse_easy_login_response(&frame, &exchange).unwrap()
        else {
            panic!("strict rejection cannot succeed")
        };
        assert_eq!(failure.kind, LoginFailureKind::SecurityRestricted);
        assert_eq!(failure.code, 140022007);
        assert!(failure.reason.unwrap().contains(detail));
    }
    #[test]
    fn sec_beat_rejection_keeps_the_native_code_without_inventing_its_cause() {
        let mut error = Vec::new();
        wire::number(1, 140022017, &mut error);
        let mut header = Vec::new();
        wire::bytes(4, &error, &mut header);
        let mut base = Vec::new();
        wire::bytes(1, &header, &mut base);
        let exchange = ExchangeCredentials {
            exchange_key: vec![7; 16],
            key_sig: vec![2],
            expiration: 300,
        };
        let mut frame = Vec::new();
        wire::bytes(3, &seal(&base, &exchange.exchange_key).unwrap(), &mut frame);
        let LoginOutcome::Rejected(failure) = parse_easy_login_response(&frame, &exchange).unwrap()
        else {
            panic!("SEC_BEAT is a rejection, not a challenge or success")
        };
        assert_eq!(failure.kind, LoginFailureKind::SecBeatRejected);
        assert_eq!(failure.code, 140022017);
        assert!(failure.reason.is_none());
        assert_ne!(failure.kind, LoginFailureKind::SecurityRestricted);
    }
}
