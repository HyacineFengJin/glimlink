// SPDX-License-Identifier: AGPL-3.0-only
//! Native operator-managed password authentication. No browser, QR fallback,
//! password storage, CAPTCHA solver or automatic login retry.
use crate::{Client, Result, auth};
use base64::{Engine, engine::general_purpose::STANDARD};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use std::{io::Read, time::Duration};
#[derive(Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum PasswordChallengeKind {
    Captcha,
    NewDevice,
    Unusual,
}
/// Secret continuation, saved only by an explicit operator flow in a0600 file.
/// It deliberately has no Debug; no password or password hash is retained.
#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PasswordContinuation {
    /// A process-local transport binding. Disk continuations deliberately lose
    /// it and require the explicit reconnect API instead of pretending resume.
    #[serde(skip)]
    connection: Option<[u8; 16]>,
    version: u32,
    uin: u32,
    uid: String,
    guid: Vec<u8>,
    profile: crate::AppProfile,
    expires_at: u32,
    exchange_key: Vec<u8>,
    key_sig: Vec<u8>,
    exchange_expiration: u32,
    cookies: Option<String>,
    pub kind: PasswordChallengeKind,
    pub verify_url: Option<String>,
    new_device: Option<NewDevice>,
}
#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct NewDevice {
    poll_token: String,
}
pub enum PasswordOutcome {
    Online,
    VerificationRequired {
        continuation: PasswordContinuation,
        code: u32,
    },
    Rejected(auth::LoginFailure),
}
impl PasswordContinuation {
    pub fn check_session(&self, session: &crate::Session, device: &crate::Device) -> Result<()> {
        let now = crate::unix_seconds()?;
        if self.version != 1
            || self.uin != session.uin
            || self.uid != session.uid
            || self.guid != device.guid_bytes()?
            || self.profile != session.app_profile
            || self.expires_at <= now
            || self.expires_at > now.saturating_add(600)
            || !matches!(self.exchange_key.len(), 16 | 24 | 32)
            || self.key_sig.is_empty()
            || self.key_sig.len() > 16384
            || self.cookies.as_ref().is_some_and(|s| s.len() > 16384)
            || self.verify_url.as_ref().is_some_and(|s| s.len() > 16384)
            || self
                .new_device
                .as_ref()
                .is_some_and(|s| s.poll_token.len() > 16384)
        {
            return Err(
                "password login continuation expired or belongs to another account/device/profile"
                    .into(),
            );
        }
        Ok(())
    }
    pub fn captcha_fingerprint(&self) -> Result<String> {
        if self.kind != PasswordChallengeKind::Captcha {
            return Err("pending login is not a CAPTCHA challenge".into());
        }
        use sha2::{Digest, Sha256};
        let mut hash = Sha256::new();
        hash.update(b"runtime-qq-captcha-v1");
        hash.update(self.uin.to_be_bytes());
        hash.update(self.expires_at.to_be_bytes());
        hash.update(
            self.verify_url
                .as_deref()
                .ok_or("CAPTCHA URL missing")?
                .as_bytes(),
        );
        Ok(format!("{:x}", hash.finalize()))
    }
    pub fn captcha_session_id(&self) -> Result<String> {
        if self.kind != PasswordChallengeKind::Captcha {
            return Err("pending login is not a CAPTCHA challenge".into());
        }
        raw_query(
            self.verify_url.as_deref().ok_or("CAPTCHA URL missing")?,
            "sid",
        )
    }
    pub fn check_captcha(&self, captcha: &auth::CaptchaSubmit<'_>) -> Result<()> {
        if captcha.session_id != self.captcha_session_id()? {
            return Err(
                "CAPTCHA session_id must match the original challenge sid, not its widget app ID"
                    .into(),
            );
        }
        Ok(())
    }
    fn check_connection(&self, connection: [u8; 16]) -> Result<()> {
        if self.connection != Some(connection) {
            return Err("QQ same_connection_unavailable: pending password login belongs to another TCP connection; use the explicit disk fallback if intended".into());
        }
        Ok(())
    }
    pub fn expires_at(&self) -> u32 {
        self.expires_at
    }
    pub fn uin(&self) -> u32 {
        self.uin
    }
    pub fn app_profile(&self) -> crate::AppProfile {
        self.profile
    }
}
impl Client {
    /// The only successful path persists Session after Register. Failed attempts
    /// preserve old tickets on disk and in memory, retaining allocated sequences.
    pub fn password_login(
        &mut self,
        password_md5: &[u8; 16],
        captcha: Option<&auth::CaptchaSubmit<'_>>,
        continuation: Option<PasswordContinuation>,
    ) -> Result<PasswordOutcome> {
        self.password_login_mode(password_md5, captcha, continuation, false)
    }
    /// Explicit cross-process fallback. This reuses saved exchange credentials
    /// over a different TCP connection, and does not claim native continuity.
    pub fn password_login_from_disk(
        &mut self,
        password_md5: &[u8; 16],
        captcha: Option<&auth::CaptchaSubmit<'_>>,
        continuation: PasswordContinuation,
    ) -> Result<PasswordOutcome> {
        self.password_login_mode(password_md5, captcha, Some(continuation), true)
    }
    fn password_login_mode(
        &mut self,
        password_md5: &[u8; 16],
        captcha: Option<&auth::CaptchaSubmit<'_>>,
        mut continuation: Option<PasswordContinuation>,
        reconnect_from_disk: bool,
    ) -> Result<PasswordOutcome> {
        if self.session.uin == 0 {
            return Err("password login requires an explicit account UIN".into());
        }
        if self.is_online() {
            return Err("QQ account is already registered".into());
        }
        if captcha.is_some() && continuation.is_none() {
            return Err("CAPTCHA submission requires its saved login continuation".into());
        }
        if let Some(state) = &continuation {
            state.check_session(self.session(), self.device())?;
            if !reconnect_from_disk {
                state.check_connection(self.password_connection())?;
            }
            if let Some(captcha) = captcha {
                state.check_captcha(captcha)?;
            }
        }
        if reconnect_from_disk {
            if let Some(state) = &mut continuation {
                state.connection = Some(self.password_connection());
            }
        }
        self.generation = self
            .generation
            .checked_add(1)
            .ok_or("QQ login generation exhausted")?;
        let previous = self.session.clone();
        let result = self.password_inner(password_md5, captcha, continuation);
        if !matches!(result, Ok(PasswordOutcome::Online)) {
            let sequence = self.session.sequence;
            self.session = previous;
            self.session.sequence = sequence;
            self.online = false;
        }
        result
    }
    fn password_inner(
        &mut self,
        password_md5: &[u8; 16],
        captcha: Option<&auth::CaptchaSubmit<'_>>,
        pending: Option<PasswordContinuation>,
    ) -> Result<PasswordOutcome> {
        self.require_signer()?;
        let device = self.device().clone();
        let guid = device.guid_bytes()?;
        self.session.clear_outer();
        let mut resumed = pending;
        let exchange = if let Some(state) = &resumed {
            auth::ExchangeCredentials {
                exchange_key: state.exchange_key.clone(),
                key_sig: state.key_sig.clone(),
                expiration: state.exchange_expiration,
            }
        } else {
            let kex = auth::KeyExchange::new()?;
            let request = kex.request(self.session.uin, &guid)?;
            let reply = self.request(auth::KEY_EXCHANGE_COMMAND, &request)?;
            kex.parse_response(&reply)?
        };
        let mut new_device_token = None;
        if let Some(state) = &mut resumed {
            match state.kind {
                PasswordChallengeKind::Captcha if captcha.is_none()=>return Err("provide CAPTCHA ticket,randstr,session_id from the operator verification page".into()),
                PasswordChallengeKind::NewDevice=>{
                    let verify=state.new_device.as_ref().ok_or("new-device verification setup missing")?;
                    new_device_token=poll_new_device(self.session.uin,&verify.poll_token)?;
                    if new_device_token.is_none(){return Ok(PasswordOutcome::VerificationRequired{continuation:resumed.take().unwrap(),code:140022010})}
                }
                PasswordChallengeKind::Unusual=>return Err("unusual-device verification requires operator completion; no automatic substitute login was attempted".into()),
                _=>{}
            }
        }
        let identity = auth::LoginIdentity {
            uin: self.session.uin,
            guid: &guid,
            device_name: &device.device_name,
            kernel_version: &device.kernel_version,
            cookies: resumed
                .as_ref()
                .and_then(|s| s.cookies.as_deref())
                .or_else(|| {
                    (!self.session.cookies.is_empty()).then_some(self.session.cookies.as_str())
                }),
            app_profile: self.session.app_profile,
        };
        let (command, request) = if let Some(token) = new_device_token {
            (
                auth::PASSWORD_NEW_DEVICE_COMMAND,
                auth::build_login_request(&identity, token.as_bytes(), &exchange, None)?,
            )
        } else {
            (
                auth::PASSWORD_LOGIN_COMMAND,
                auth::build_password_login_request(&identity, password_md5, &exchange, captcha)?,
            )
        };
        let reply = self.request(command, &request)?;
        match auth::parse_easy_login_response(&reply, &exchange)? {
            auth::LoginOutcome::Success(credentials) => {
                if credentials
                    .uid
                    .as_deref()
                    .is_some_and(|uid| !self.session.uid.is_empty() && uid != self.session.uid)
                {
                    return Err("password login returned a different account UID".into());
                }
                self.session.set_credentials(
                    &credentials.tgt,
                    &credentials.d2,
                    &credentials.d2_key,
                    &credentials.temp_pwd,
                    credentials.uid.as_deref(),
                )?;
                self.register()?;
                self.persist_session()?;
                Ok(PasswordOutcome::Online)
            }
            auth::LoginOutcome::Rejected(failure) => Ok(PasswordOutcome::Rejected(failure)),
            auth::LoginOutcome::VerificationRequired(challenge) => {
                let kind = match challenge.kind {
                    auth::ChallengeKind::Captcha => PasswordChallengeKind::Captcha,
                    auth::ChallengeKind::NewDevice => PasswordChallengeKind::NewDevice,
                    auth::ChallengeKind::Unusual => PasswordChallengeKind::Unusual,
                };
                let mut state = PasswordContinuation {
                    connection: Some(self.password_connection()),
                    version: 1,
                    uin: self.session.uin,
                    uid: self.session.uid.clone(),
                    guid,
                    profile: self.session.app_profile,
                    expires_at: crate::unix_seconds()?.saturating_add(300),
                    exchange_key: exchange.exchange_key,
                    key_sig: exchange.key_sig,
                    exchange_expiration: exchange.expiration,
                    cookies: challenge.cookies,
                    kind,
                    verify_url: challenge.verify_url,
                    new_device: None,
                };
                if kind == PasswordChallengeKind::NewDevice {
                    let (url, poll_token) = prepare_new_device(
                        self.session.uin,
                        self.session.app_profile,
                        &device.device_name,
                        state
                            .verify_url
                            .as_deref()
                            .ok_or("new-device URL missing")?,
                    )?;
                    state.verify_url = Some(url);
                    state.new_device = Some(NewDevice { poll_token });
                }
                Ok(PasswordOutcome::VerificationRequired {
                    continuation: state,
                    code: challenge.code,
                })
            }
        }
    }
}
fn endpoint(uin: u32) -> String {
    format!(
        "https://oidb.tim.qq.com/v3/oidbinterface/oidb_0xc9e_8?uid={uin}&getqrcode=1&sdkappid=39998&actype=2"
    )
}
fn device_request(uin: u32, body: Value) -> Result<Value> {
    let bytes = serde_json::to_vec(&body).map_err(|_| "invalid device-verification request")?;
    if bytes.len() > 65536 {
        return Err("device verification request exceeds budget".into());
    }
    let agent = ureq::Agent::config_builder()
        .proxy(None)
        .max_redirects(0)
        .timeout_global(Some(Duration::from_secs(15)))
        .build()
        .new_agent();
    let mut response = agent
        .post(&endpoint(uin))
        .header("Content-Type", "application/json")
        .send(bytes.as_slice())
        .map_err(|_| "QQ device verification request failed")?;
    let mut bytes = Vec::new();
    response
        .body_mut()
        .as_reader()
        .take(65537)
        .read_to_end(&mut bytes)
        .map_err(|_| "QQ device verification response failed")?;
    if bytes.len() > 65536 {
        return Err("device verification response exceeds budget".into());
    }
    let body: Value =
        serde_json::from_slice(&bytes).map_err(|_| "invalid device verification response")?;
    if body
        .get("ErrorCode")
        .and_then(Value::as_i64)
        .is_some_and(|code| code != 0)
    {
        return Err("QQ device verification rejected request".into());
    }
    Ok(body)
}
fn decode_query(value: &str) -> Result<String> {
    if value.len() > 16384 {
        return Err("verification query exceeds budget".into());
    }
    let bytes = value.as_bytes();
    let mut out = Vec::new();
    let mut at = 0;
    while at < bytes.len() {
        if bytes[at] == b'%' {
            if at + 2 >= bytes.len() {
                return Err("invalid verification query encoding".into());
            }
            let part = std::str::from_utf8(&bytes[at + 1..at + 3])
                .map_err(|_| "invalid verification query encoding")?;
            out.push(
                u8::from_str_radix(part, 16).map_err(|_| "invalid verification query encoding")?,
            );
            at += 3
        } else {
            out.push(if bytes[at] == b'+' { b' ' } else { bytes[at] });
            at += 1
        }
    }
    String::from_utf8(out).map_err(|_| "invalid verification query text".into())
}
fn raw_query(url: &str, key: &str) -> Result<String> {
    let uri: ureq::http::Uri = url.parse().map_err(|_| "invalid verification URL")?;
    if uri.scheme_str() != Some("https")
        || uri.authority().is_none_or(|a| a.as_str().contains('@'))
        || url.len() > 16384
    {
        return Err("verification URL must use HTTPS".into());
    }
    let mut values = url
        .split_once('?')
        .map(|(_, q)| q)
        .unwrap_or("")
        .split('&')
        .filter_map(|p| p.split_once('='))
        .filter(|(k, _)| *k == key);
    let value = values.next().ok_or("device verification token missing")?.1;
    if value.is_empty() || values.next().is_some() {
        return Err("empty or duplicate device verification token".into());
    }
    Ok(value.to_owned())
}
fn query(url: &str, key: &str) -> Result<String> {
    let uri: ureq::http::Uri = url.parse().map_err(|_| "invalid verification URL")?;
    if uri.scheme_str() != Some("https")
        || uri.authority().is_none_or(|a| a.as_str().contains('@'))
        || url.len() > 16384
    {
        return Err("verification URL must use HTTPS".into());
    }
    let mut values = url
        .split_once('?')
        .map(|(_, q)| q)
        .unwrap_or("")
        .split('&')
        .filter_map(|part| part.split_once('='))
        .filter(|(k, _)| *k == key);
    let raw = values.next().ok_or("device verification token missing")?.1;
    if values.next().is_some() {
        return Err("duplicate device verification token".into());
    }
    let value = decode_query(raw)?;
    if value.is_empty() {
        return Err("empty device verification token".into());
    }
    Ok(value)
}
fn prepare_new_device(
    uin: u32,
    profile: crate::AppProfile,
    device: &str,
    url: &str,
) -> Result<(String, String)> {
    let body = device_request(
        uin,
        json!({"str_dev_auth_token":query(url,"sig")?,"uint32_flag":1,"uint32_url_type":0,"str_uin_token":query(url,"uin-token")?,"str_dev_type":profile.info().os,"str_dev_name":device}),
    )?;
    let url = body
        .get("str_url")
        .and_then(Value::as_str)
        .filter(|s| !s.is_empty() && s.len() <= 16384)
        .ok_or("device verification page missing")?
        .to_owned();
    let token = raw_query(&url, "str_url")?;
    Ok((url, token))
}
fn poll_new_device(uin: u32, token: &str) -> Result<Option<String>> {
    let body = device_request(
        uin,
        json!({"uint32_flag":0,"bytes_token":STANDARD.encode(token)}),
    )?;
    let token = body
        .get("str_nt_succ_token")
        .and_then(Value::as_str)
        .unwrap_or("");
    if token.len() > 16384 {
        return Err("device verification token exceeds budget".into());
    }
    Ok((!token.is_empty()).then(|| token.to_owned()))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::wire::{self, Fields, Reader};
    use aes_gcm::{Aes128Gcm, KeyInit, Nonce, aead::Aead};
    use std::{
        io::Write,
        net::{TcpListener, TcpStream},
    };
    fn fixture() -> (crate::Session, crate::Device, PasswordContinuation) {
        let mut session = crate::Session::empty();
        session.uin = 12345;
        session.uid = "u_fixture".into();
        let device:crate::Device=serde_json::from_value(json!({"guid":"000102030405060708090a0b0c0d0e0f","device_name":"synthetic-device","system_kernel":"Linux"})).unwrap();
        let continuation = PasswordContinuation {
            connection: None,
            version: 1,
            uin: session.uin,
            uid: session.uid.clone(),
            guid: device.guid_bytes().unwrap(),
            profile: session.app_profile,
            expires_at: crate::unix_seconds().unwrap() + 300,
            exchange_key: vec![1; 16],
            key_sig: vec![2; 32],
            exchange_expiration: 300,
            cookies: None,
            kind: PasswordChallengeKind::Captcha,
            verify_url: Some("https://verify.qq.com/synthetic".into()),
            new_device: None,
        };
        (session, device, continuation)
    }
    #[test]
    fn continuation_rejects_expired_account_device_and_profile_changes_before_login() {
        let (mut session, device, mut continuation) = fixture();
        continuation.check_session(&session, &device).unwrap();
        session.uin += 1;
        assert!(continuation.check_session(&session, &device).is_err());
        session.uin -= 1;
        session.app_profile = crate::AppProfile::Linux32646494;
        assert!(continuation.check_session(&session, &device).is_err());
        session.app_profile = crate::AppProfile::default();
        let mut other = device.clone();
        other.guid = "01010101010101010101010101010101".into();
        assert!(continuation.check_session(&session, &other).is_err());
        continuation.expires_at = 1;
        assert!(continuation.check_session(&session, &device).is_err());
        let encoded = serde_json::to_string(&continuation).unwrap();
        assert!(!encoded.contains("password_md5"));
        assert!(!encoded.contains("password\":"));
    }
    #[test]
    fn captcha_proof_uses_original_session_not_widget_application_id() {
        let (_, _, mut pending) = fixture();
        pending.verify_url = Some("https://ti.qq.com/safe/tools/captcha/sms-verify-login?aid=2086582797&login_appid=1600001615&sid=synthetic%2Bsession&uin=12345".into());
        let correct = auth::CaptchaSubmit {
            ticket: "synthetic-ticket",
            randstr: "synthetic-rand",
            session_id: "synthetic%2Bsession",
        };
        pending.check_captcha(&correct).unwrap();
        let wrong = auth::CaptchaSubmit {
            session_id: "2086582797",
            ..correct
        };
        assert!(pending.check_captcha(&wrong).is_err());
        let decoded = auth::CaptchaSubmit {
            session_id: "synthetic+session",
            ..correct
        };
        assert!(pending.check_captcha(&decoded).is_err());
    }
    #[test]
    fn device_query_preserves_raw_poll_token_and_decodes_initial_tokens_once() {
        let url = "https://verify.qq.com/path?sig=alpha%2Bbeta&uin-token=own%3Dtoken&str_url=encoded%252Fvalue";
        assert_eq!(query(url, "sig").unwrap(), "alpha+beta");
        assert_eq!(query(url, "uin-token").unwrap(), "own=token");
        assert_eq!(raw_query(url, "str_url").unwrap(), "encoded%252Fvalue");
        assert!(query("http://verify.qq.com/?sig=a", "sig").is_err());
        assert!(query("https://verify.qq.com/?sig=a&sig=b", "sig").is_err());
        assert!(query("https://verify.qq.com/?sig=%QZ", "sig").is_err());
    }
    fn connected() -> (Client, TcpStream, PasswordContinuation) {
        let (session, device, mut pending) = fixture();
        let listener = TcpListener::bind((std::net::Ipv4Addr::LOCALHOST, 0)).unwrap();
        let stream = TcpStream::connect(listener.local_addr().unwrap()).unwrap();
        let (peer, _) = listener.accept().unwrap();
        peer.set_read_timeout(Some(Duration::from_secs(3))).unwrap();
        let client = Client::from_tcp_stream(
            session,
            device,
            crate::ClientConfig {
                session_file: "/tmp/qq-synthetic-login-session.json".into(),
                device_file: "/tmp/qq-synthetic-login-device.json".into(),
                signer_module: None,
            },
            stream,
        )
        .unwrap();
        pending.connection = Some(client.password_connection());
        pending.verify_url = Some("https://ti.qq.com/safe/tools/captcha/sms-verify-login?aid=2086582797&login_appid=1600001615&sid=synthetic0&uin=12345".into());
        (client, peer, pending)
    }
    #[test]
    fn serialized_or_foreign_continuations_require_the_explicit_reconnect_api() {
        let (mut client, mut peer, pending) = connected();
        let proof = auth::CaptchaSubmit {
            ticket: "synthetic-ticket",
            randstr: "synthetic-rand",
            session_id: "synthetic0",
        };
        let bytes = serde_json::to_vec(&pending).unwrap();
        assert!(!String::from_utf8_lossy(&bytes).contains("connection"));
        let disk: PasswordContinuation = serde_json::from_slice(&bytes).unwrap();
        assert!(
            client
                .password_login(&[17; 16], Some(&proof), Some(disk))
                .err()
                .unwrap()
                .contains("same_connection_unavailable")
        );
        let (mut other, _other_peer, _) = connected();
        assert!(
            other
                .password_login(&[17; 16], Some(&proof), Some(pending))
                .err()
                .unwrap()
                .contains("same_connection_unavailable")
        );
        let disk: PasswordContinuation = serde_json::from_slice(&bytes).unwrap();
        assert_eq!(
            client
                .password_login_from_disk(&[17; 16], Some(&proof), disk)
                .err()
                .unwrap(),
            "native signing module must be configured explicitly"
        );
        peer.set_read_timeout(Some(Duration::from_millis(20)))
            .unwrap();
        assert!(peer.read(&mut [0u8; 1]).is_err());
    }
    fn seal_test(plain: &[u8], key: &[u8]) -> Vec<u8> {
        let nonce = [9u8; 12];
        let mut out = nonce.to_vec();
        out.extend(
            Aes128Gcm::new_from_slice(key)
                .unwrap()
                .encrypt(Nonce::from_slice(&nonce), plain)
                .unwrap(),
        );
        out
    }
    fn open_test(cipher: &[u8], key: &[u8]) -> Vec<u8> {
        Aes128Gcm::new_from_slice(key)
            .unwrap()
            .decrypt(Nonce::from_slice(&cipher[..12]), &cipher[12..])
            .unwrap()
    }
    fn read_login(peer: &mut TcpStream) -> (u32, Vec<u8>) {
        let mut length = [0u8; 4];
        peer.read_exact(&mut length).unwrap();
        let length = u32::from_be_bytes(length) as usize;
        assert!(length < 65536);
        let mut frame = vec![0; length - 4];
        peer.read_exact(&mut frame).unwrap();
        let mut r = Reader::new(&frame);
        assert_eq!(r.u32().unwrap(), 12);
        assert_eq!(r.byte().unwrap(), 2);
        assert_eq!(r.packet().unwrap(), b"");
        assert_eq!(r.byte().unwrap(), 0);
        assert_eq!(r.packet().unwrap(), b"12345");
        let plain = crate::tea::decrypt(r.rest(), &[0; 16]).unwrap();
        let mut r = Reader::new(&plain);
        let mut head = Reader::new(r.packet().unwrap());
        let sequence = head.u32().unwrap();
        head.take(20).unwrap();
        head.packet().unwrap();
        assert_eq!(
            head.packet().unwrap(),
            auth::PASSWORD_LOGIN_COMMAND.as_bytes()
        );
        (sequence, r.packet().unwrap().to_vec())
    }
    fn reply(peer: &mut TcpStream, sequence: u32, body: &[u8]) {
        let mut head = sequence.to_be_bytes().to_vec();
        head.extend(0u32.to_be_bytes());
        head.extend(wire::packet(b""));
        head.extend(wire::packet(auth::PASSWORD_LOGIN_COMMAND.as_bytes()));
        head.extend(wire::packet(b""));
        head.extend(0u32.to_be_bytes());
        let mut payload = wire::packet(&head);
        payload.extend(wire::packet(body));
        let mut outer = 12u32.to_be_bytes().to_vec();
        outer.push(2);
        outer.push(0);
        outer.extend(wire::packet(b"12345"));
        outer.extend(crate::tea::encrypt(&payload, &[0; 16]).unwrap());
        peer.write_all(&wire::packet(&outer)).unwrap();
    }
    #[test]
    fn consecutive_human_verifications_use_one_tcp_and_the_same_exchange() {
        let (mut client, mut peer, mut pending) = connected();
        client.use_fixture_signer();
        pending.cookies = Some("cookie0".into());
        let server = std::thread::spawn(move || {
            for step in 0..2 {
                let (sequence, frame) = read_login(&mut peer);
                let frame = Fields::parse(&frame).unwrap();
                assert_eq!(frame.data(1).unwrap(), &[2; 32]);
                let plain = open_test(frame.data(3).unwrap(), &[1; 16]);
                let base = Fields::parse(&plain).unwrap();
                let header = Fields::parse(base.data(1).unwrap()).unwrap();
                let cookie = Fields::parse(header.data(5).unwrap()).unwrap();
                assert_eq!(cookie.data(1).unwrap(), format!("cookie{step}").as_bytes());
                let body = Fields::parse(base.data(2).unwrap()).unwrap();
                let proof = Fields::parse(body.data(2).unwrap()).unwrap();
                assert_eq!(
                    proof.data(3).unwrap(),
                    format!("synthetic{step}").as_bytes()
                );
                let mut error = Vec::new();
                wire::number(1, if step == 0 { 140022008 } else { 140022013 }, &mut error);
                let mut header = Vec::new();
                wire::bytes(4, &error, &mut header);
                let mut cookie = Vec::new();
                wire::bytes(1, b"cookie1", &mut cookie);
                wire::bytes(5, &cookie, &mut header);
                let mut captcha = Vec::new();
                wire::bytes(3,b"https://ti.qq.com/safe/tools/captcha/sms-verify-login?aid=2086582797&login_appid=1600001615&sid=synthetic1&uin=12345",&mut captcha);
                let mut body = Vec::new();
                wire::bytes(2, &captcha, &mut body);
                let mut base = Vec::new();
                wire::bytes(1, &header, &mut base);
                wire::bytes(2, &body, &mut base);
                let mut frame = Vec::new();
                wire::bytes(3, &seal_test(&base, &[1; 16]), &mut frame);
                reply(&mut peer, sequence, &frame);
            }
        });
        let proof0 = auth::CaptchaSubmit {
            ticket: "human-fixture0",
            randstr: "rand-fixture0",
            session_id: "synthetic0",
        };
        let outcome = client
            .password_login(&[17; 16], Some(&proof0), Some(pending))
            .unwrap();
        let PasswordOutcome::VerificationRequired { continuation, .. } = outcome else {
            panic!("expected second synthetic challenge")
        };
        let proof1 = auth::CaptchaSubmit {
            ticket: "human-fixture1",
            randstr: "rand-fixture1",
            session_id: "synthetic1",
        };
        assert!(matches!(
            client
                .password_login(&[17; 16], Some(&proof1), Some(continuation))
                .unwrap(),
            PasswordOutcome::Rejected(_)
        ));
        server.join().unwrap();
    }
}
