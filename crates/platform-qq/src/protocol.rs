// SPDX-License-Identifier: AGPL-3.0-only
//! Synchronous NTQQ transport, adapted from the lab and pinned LagrangeGo; NOTICE.
use crate::{
    IncomingEvent, Result,
    events::PUSH,
    message::{self, SendResponse, Target},
    session::{Device, Session},
    signer::{self, Signer},
    tea,
    wire::{self, Fields, MAX_FRAME, Reader},
};
use flate2::read::ZlibDecoder;
use serde::{Deserialize, Serialize};
use std::{
    collections::VecDeque,
    io::{Read, Write},
    net::{TcpStream, ToSocketAddrs},
    path::PathBuf,
    sync::atomic::{AtomicBool, Ordering},
    time::{Duration, Instant},
};

const REGISTER: &str = "trpc.qq_new_tech.status_svc.StatusService.Register";
const HEARTBEAT: &str = "trpc.qq_new_tech.status_svc.StatusService.SsoHeartBeat";
const MAX_EVENTS: usize = 32;
const MAX_EVENT_BYTES: usize = 8 * 1024 * 1024;
const REQUEST_TIMEOUT: Duration = Duration::from_secs(15);
const MAX_POLL: Duration = Duration::from_secs(5);

#[derive(Clone, Debug)]
pub struct ClientConfig {
    pub session_file: PathBuf,
    pub device_file: PathBuf,
    pub signer_module: Option<PathBuf>,
}
impl ClientConfig {
    fn validate(&self) -> Result<()> {
        if !self.session_file.is_absolute()
            || !self.device_file.is_absolute()
            || self.session_file == self.device_file
            || self
                .signer_module
                .as_ref()
                .is_some_and(|p| !p.is_absolute())
        {
            return Err(
                "QQ private state and optional signer paths must be distinct absolute paths".into(),
            );
        }
        Ok(())
    }
}
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum SendTarget {
    SelfAccount,
    Private { uid: String },
    Group { code: u32 },
}
impl SendTarget {
    fn borrowed(&self) -> Result<Target<'_>> {
        match self {
            Self::Private { uid } => Ok(Target::Private(uid)),
            Self::Group { code } => Ok(Target::Group(*code)),
            Self::SelfAccount => {
                Err("self destination must be resolved against authenticated UID".into())
            }
        }
    }
}

// Credential-bearing packet: intentionally no Clone/Serialize/Debug.
pub struct PreparedText {
    actor_uin: u32,
    actor_uid: String,
    target: SendTarget,
    sequence: u32,
    sso_sequence: u32,
    random: u32,
    timestamp: u32,
    text_bytes: usize,
    connection: [u8; 16],
    generation: u64,
    packet: Vec<u8>,
}
impl PreparedText {
    pub fn actor_uin(&self) -> u32 {
        self.actor_uin
    }
    pub fn actor_uid(&self) -> &str {
        &self.actor_uid
    }
    pub fn target(&self) -> &SendTarget {
        &self.target
    }
    pub fn sequence(&self) -> u32 {
        self.sequence
    }
    pub fn sso_sequence(&self) -> u32 {
        self.sso_sequence
    }
    pub fn random(&self) -> u32 {
        self.random
    }
    pub fn timestamp(&self) -> u32 {
        self.timestamp
    }
    pub fn text_bytes(&self) -> usize {
        self.text_bytes
    }
    pub fn packet_bytes(&self) -> usize {
        self.packet.len()
    }
    fn check_bound(
        &self,
        session: &Session,
        connection: &[u8; 16],
        generation: u64,
        persisted: Option<u32>,
    ) -> Result<()> {
        if self.actor_uin != session.uin
            || self.actor_uid != session.uid
            || self.connection != *connection
            || self.generation != generation
            || self.sequence != session.sequence
        {
            return Err("prepared QQ send belongs to a changed session/connection/sequence".into());
        }
        if persisted != Some(self.sequence) {
            return Err("persist prepared QQ session sequence before transmitting".into());
        }
        Ok(())
    }
}
struct Response {
    sequence: u32,
    command: String,
    body: Vec<u8>,
}
pub struct Client {
    pub(crate) session: Session,
    pub(crate) config: ClientConfig,
    device: Device,
    stream: TcpStream,
    incoming: Vec<u8>,
    events: VecDeque<IncomingEvent>,
    event_bytes: usize,
    signer: Option<Signer>,
    connection: [u8; 16],
    pub(crate) generation: u64,
    pub(crate) online: bool,
    persisted_sequence: Option<u32>,
}
fn packet16(value: &[u8]) -> Result<Vec<u8>> {
    let length = u16::try_from(value.len() + 2).map_err(|_| "16-bit field overflow")?;
    let mut out = length.to_be_bytes().to_vec();
    out.extend_from_slice(value);
    Ok(out)
}
impl Client {
    /// No device creation, QR, native signer loading or login is implicit.
    pub fn open(config: ClientConfig) -> Result<Self> {
        config.validate()?;
        let session = Session::load(&config.session_file)?;
        let device = Device::load(&config.device_file)?;
        Self::connect(session, device, config)
    }
    pub fn connect(session: Session, device: Device, config: ClientConfig) -> Result<Self> {
        config.validate()?;
        device.guid_bytes()?;
        session.key()?;
        let addresses = ("msfwifi.3g.qq.com", 8080)
            .to_socket_addrs()
            .map_err(|_| "cannot resolve QQ chat server")?;
        let mut stream = None;
        for address in addresses.filter(|a| a.is_ipv4()).take(8) {
            if let Ok(s) = TcpStream::connect_timeout(&address, Duration::from_secs(4)) {
                stream = Some(s);
                break;
            }
        }
        let stream = stream.ok_or("cannot connect to QQ chat server")?;
        Self::from_tcp_stream(session, device, config, stream)
    }
    /// Adopt an already connected stream. Useful for an explicit transport
    /// owner and isolated protocol fixtures; this never starts authentication.
    pub fn from_tcp_stream(
        session: Session,
        device: Device,
        config: ClientConfig,
        stream: TcpStream,
    ) -> Result<Self> {
        config.validate()?;
        device.guid_bytes()?;
        session.key()?;
        stream
            .set_read_timeout(Some(Duration::from_millis(250)))
            .map_err(|_| "cannot set QQ receive timeout")?;
        stream
            .set_write_timeout(Some(Duration::from_secs(10)))
            .map_err(|_| "cannot set QQ send timeout")?;
        let mut connection = [0; 16];
        tea::random(&mut connection)?;
        Ok(Self {
            session,
            config,
            device,
            stream,
            incoming: Vec::with_capacity(8192),
            events: VecDeque::new(),
            event_bytes: 0,
            signer: None,
            connection,
            generation: 0,
            online: false,
            persisted_sequence: None,
        })
    }
    pub fn session(&self) -> &Session {
        &self.session
    }
    pub(crate) fn password_connection(&self) -> [u8; 16] {
        self.connection
    }
    #[cfg(test)]
    pub(crate) fn use_fixture_signer(&mut self) {
        self.signer = Some(crate::signer::Signer::fixture());
    }
    pub fn device(&self) -> &Device {
        &self.device
    }
    pub fn into_session(self) -> Session {
        self.session
    }
    pub fn signer_loaded(&self) -> bool {
        self.signer.is_some()
    }
    pub fn is_online(&self) -> bool {
        self.online
    }
    pub fn queued_events(&self) -> usize {
        self.events.len()
    }
    /// Call after prepare_text and before the host records/transmits its attempt.
    pub fn persist_session(&mut self) -> Result<()> {
        self.session.save(&self.config.session_file)?;
        self.persisted_sequence = Some(self.session.sequence);
        Ok(())
    }
    pub(crate) fn require_signer(&mut self) -> Result<()> {
        if self.signer.is_none() {
            let path = self
                .config
                .signer_module
                .as_deref()
                .ok_or("native signing module must be configured explicitly")?;
            self.signer = Some(Signer::load(path)?);
        }
        Ok(())
    }
    fn encode(&mut self, sequence: u32, command: &str, body: &[u8]) -> Result<Vec<u8>> {
        if body.len() > MAX_FRAME - 4096 {
            return Err("request exceeds packet limit".into());
        }
        if !matches!(
            command,
            REGISTER
                | HEARTBEAT
                | "wtlogin.trans_emp"
                | "wtlogin.login"
                | "trpc.login.ecdh.EcdhService.SsoKeyExchange"
                | "trpc.login.ecdh.EcdhService.SsoNTLoginEasyLogin"
                | crate::auth::PASSWORD_LOGIN_COMMAND
                | crate::auth::PASSWORD_NEW_DEVICE_COMMAND
                | "MessageSvc.PbSendMsg"
                | "OidbSvcTrpcTcp.0xfd4_1"
                | "OidbSvcTrpcTcp.0xfe5_2"
                | "OidbSvcTrpcTcp.0xfe7_3"
                | "trpc.msg.register_proxy.RegisterProxy.SsoGetRoamMsg"
                | "trpc.msg.register_proxy.RegisterProxy.SsoGetGroupMsg"
                | "OidbSvcTrpcTcp.0x88d_0"
                | "OidbSvcTrpcTcp.0xe37_1200"
                | "OidbSvcTrpcTcp.0x6d6_2"
                | "HttpConn.0x6ff_501"
                | "OidbSvcTrpcTcp.0x126d_100"
                | "OidbSvcTrpcTcp.0x126e_100"
                | "OidbSvcTrpcTcp.0xe37_1700"
                | "OidbSvcTrpcTcp.0x6d6_0"
                | "OidbSvcTrpcTcp.0x6d9_4"
                | "OidbSvcTrpcTcp.0x11e9_100"
                | "OidbSvcTrpcTcp.0x11ea_100"
                | "OidbSvcTrpcTcp.0x11c4_100"
                | "OidbSvcTrpcTcp.0x11c5_100"
                | "OidbSvcTrpcTcp.0x11c4_200"
                | "OidbSvcTrpcTcp.0x11c5_200"
                | "OidbSvcTrpcTcp.0x126d_200"
                | "OidbSvcTrpcTcp.0x126e_200"
                | "OidbSvcTrpcTcp.0x11e9_200"
        ) {
            return Err("command outside the fixed chat/login allowlist".into());
        }
        let mut random = [0; 24];
        tea::random(&mut random)?;
        let hex = |b: &[u8]| b.iter().map(|v| format!("{v:02x}")).collect::<String>();
        let trace = format!("00-{}-{}-01", hex(&random[..16]), hex(&random[16..]));
        let mut extension = Vec::new();
        wire::bytes(15, trace.as_bytes(), &mut extension);
        wire::bytes(16, self.session.uid.as_bytes(), &mut extension);
        if signer::needs_signature(command) {
            self.require_signer()?;
            let signature = self
                .signer
                .as_mut()
                .unwrap()
                .sign(command, sequence, body)?;
            let mut signed = Vec::new();
            wire::bytes(1, &signature.sign, &mut signed);
            wire::bytes(2, &signature.token, &mut signed);
            wire::bytes(3, &signature.extra, &mut signed);
            wire::bytes(24, &signed, &mut extension);
        }
        let mut header = Vec::new();
        header.extend_from_slice(&sequence.to_be_bytes());
        header.extend_from_slice(&self.session.app_profile.info().sub_app_id.to_be_bytes());
        header.extend_from_slice(&2052_u32.to_be_bytes());
        header.extend_from_slice(&[2, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0]);
        header.extend(wire::packet(&self.session.tgt()?));
        header.extend(wire::packet(command.as_bytes()));
        header.extend(wire::packet(&[]));
        header.extend(wire::packet(&self.device.guid_bytes()?));
        header.extend(wire::packet(&[]));
        header.extend(packet16(
            self.session.app_profile.info().version.as_bytes(),
        )?);
        header.extend(wire::packet(&extension));
        let mut payload = wire::packet(&header);
        payload.extend(wire::packet(body));
        let encrypted = tea::encrypt(&payload, &self.session.key()?)?;
        let mut service = 12_u32.to_be_bytes().to_vec();
        let d2 = self.session.d2()?;
        service.push(if d2.is_empty() { 2 } else { 1 });
        service.extend(wire::packet(&d2));
        service.push(0);
        service.extend(wire::packet(self.session.uin.to_string().as_bytes()));
        service.extend(encrypted);
        Ok(wire::packet(&service))
    }

    fn decode(&self, frame: &[u8]) -> Result<Response> {
        decode_frame(&self.session, frame)
    }

    fn buffered_frame(&mut self) -> Result<Option<Vec<u8>>> {
        extract_frame(&mut self.incoming)
    }
    fn poll_response(&mut self, wait: Duration) -> Result<Option<Response>> {
        if let Some(frame) = self.buffered_frame()? {
            return self.decode(&frame).map(Some);
        }
        self.stream
            .set_read_timeout(Some(wait.max(Duration::from_millis(1))))
            .map_err(|_| "cannot set QQ poll timeout")?;
        let mut chunk = [0; 8192];
        match self.stream.read(&mut chunk) {
            Ok(0) => {
                self.online = false;
                return Err("QQ closed the connection".into());
            }
            Ok(n) => {
                if self.incoming.len() + n > MAX_FRAME + 8192 {
                    return Err("receive buffer limit".into());
                }
                self.incoming.extend_from_slice(&chunk[..n]);
            }
            Err(e)
                if matches!(
                    e.kind(),
                    std::io::ErrorKind::WouldBlock | std::io::ErrorKind::TimedOut
                ) =>
            {
                return Ok(None);
            }
            Err(_) => {
                self.online = false;
                return Err("QQ receive transport failed".into());
            }
        }
        self.buffered_frame()?
            .map(|frame| self.decode(&frame))
            .transpose()
    }
    fn observe(&mut self, response: Response) -> Result<()> {
        if response.command == "Heartbeat.Alive" {
            return Ok(());
        }
        if self.events.len() >= MAX_EVENTS
            || self.event_bytes + response.body.len() > MAX_EVENT_BYTES
        {
            return Err("QQ event queue limit; drain events before more requests".into());
        }
        let event = if response.command == PUSH {
            IncomingEvent::from_push(response.sequence, response.body)?
        } else {
            use sha2::{Digest, Sha256};
            let mut hash = Sha256::new();
            hash.update(response.command.as_bytes());
            hash.update(&response.body);
            IncomingEvent {
                key: format!("qq-event-{:x}", hash.finalize()),
                command: response.command,
                sso_sequence: response.sequence,
                view: None,
                raw: response.body,
            }
        };
        self.event_bytes += event.raw.len();
        self.events.push_back(event);
        Ok(())
    }
    /// One bounded socket read, plus any already queued push. Zero wait uses a
    /// 1ms socket timeout; max_wait is capped at five seconds, never unbounded.
    pub fn poll(&mut self, max_wait: Duration) -> Result<Option<IncomingEvent>> {
        if max_wait > MAX_POLL {
            return Err("QQ poll duration exceeds five seconds".into());
        }
        if let Some(event) = self.events.pop_front() {
            self.event_bytes -= event.raw.len();
            return Ok(Some(event));
        }
        if let Some(response) = self.poll_response(max_wait)? {
            self.observe(response)?;
        }
        let event = self.events.pop_front();
        if let Some(event) = &event {
            self.event_bytes -= event.raw.len();
        }
        Ok(event)
    }
    fn wait_for(&mut self, sequence: u32, command: &str) -> Result<Vec<u8>> {
        self.wait_for_controlled(sequence, command, Instant::now() + REQUEST_TIMEOUT, None)
    }
    fn wait_for_controlled(
        &mut self,
        sequence: u32,
        command: &str,
        deadline: Instant,
        stop: Option<&AtomicBool>,
    ) -> Result<Vec<u8>> {
        while Instant::now() < deadline {
            if stop.is_some_and(|flag| flag.load(Ordering::Relaxed)) {
                return Err("QR login canceled".into());
            }
            // Stop before consuming another frame when there is insufficient
            // queue space for even the largest permitted push. Existing events
            // stay available to poll; no consumed push is silently discarded.
            if self.events.len() >= MAX_EVENTS || self.event_bytes > MAX_EVENT_BYTES - MAX_FRAME {
                return Err(
                    "QQ event queue needs draining before response wait can continue".into(),
                );
            }
            let wait = deadline
                .saturating_duration_since(Instant::now())
                .min(Duration::from_millis(250));
            if let Some(response) = self.poll_response(wait)? {
                if response.sequence == sequence && response.command == command {
                    return Ok(response.body);
                }
                self.observe(response)?;
            }
        }
        Err(format!("response timeout: {command}"))
    }
    /// Management-only deadline/cancellation path. Continuing a partial TCP
    /// write is not a second protocol submission; the packet is encoded once.
    pub(crate) fn request_until(
        &mut self,
        command: &str,
        body: &[u8],
        deadline: Instant,
        stop: &AtomicBool,
    ) -> Result<Vec<u8>> {
        check_request_control(deadline, stop)?;
        self.session.sequence = self.session.sequence.wrapping_add(1);
        let sequence = self.session.sequence % 0x8000;
        let packet = self.encode(sequence, command, body)?;
        let result = (|| {
            let mut written = 0;
            while written < packet.len() {
                check_request_control(deadline, stop)?;
                let wait = deadline
                    .saturating_duration_since(Instant::now())
                    .min(Duration::from_millis(250))
                    .max(Duration::from_millis(1));
                self.stream
                    .set_write_timeout(Some(wait))
                    .map_err(|_| "cannot set QR request timeout")?;
                match self.stream.write(&packet[written..]) {
                    Ok(0) => return Err("QQ closed QR request stream".into()),
                    Ok(n) => written += n,
                    Err(error)
                        if matches!(
                            error.kind(),
                            std::io::ErrorKind::WouldBlock
                                | std::io::ErrorKind::TimedOut
                                | std::io::ErrorKind::Interrupted
                        ) => {}
                    Err(_) => return Err("QQ QR request transport failed".into()),
                }
            }
            check_request_control(deadline, stop)?;
            self.wait_for_controlled(
                sequence,
                command,
                deadline.min(Instant::now() + REQUEST_TIMEOUT),
                Some(stop),
            )
        })();
        let _ = self.stream.set_write_timeout(Some(Duration::from_secs(10)));
        result
    }
    pub(crate) fn request(&mut self, command: &str, body: &[u8]) -> Result<Vec<u8>> {
        self.session.sequence = self.session.sequence.wrapping_add(1);
        let sequence = self.session.sequence % 0x8000;
        let packet = self.encode(sequence, command, body)?;
        self.stream
            .write_all(&packet)
            .map_err(|_| "QQ request transport failed")?;
        self.wait_for(sequence, command)
    }
    /// Encode/sign only. The caller then persists this sequence, records its
    /// outbox attempt, and explicitly consumes the prepared packet once.
    pub fn prepare_text(
        &mut self,
        target: SendTarget,
        text: &str,
        random: u32,
    ) -> Result<PreparedText> {
        self.prepare_rich(target, &crate::rich::Message::plain(text), random)
    }
    pub fn prepare_rich(
        &mut self,
        target: SendTarget,
        content: &crate::rich::Message,
        random: u32,
    ) -> Result<PreparedText> {
        if !self.online {
            return Err("QQ session is not registered".into());
        }
        let target = match target {
            SendTarget::SelfAccount => SendTarget::Private {
                uid: self.session.uid.clone(),
            },
            other => other,
        };
        let timestamp = crate::unix_seconds()?;
        let body = message::encode_rich(
            target.borrowed()?,
            content,
            self.session.sequence % 0x8000,
            random,
            timestamp,
        )?;
        let sequence = self.session.sequence.wrapping_add(1);
        let sso_sequence = sequence % 0x8000;
        let packet = self.encode(sso_sequence, message::SEND_COMMAND, &body)?;
        self.session.sequence = sequence;
        Ok(PreparedText {
            actor_uin: self.session.uin,
            actor_uid: self.session.uid.clone(),
            target,
            sequence,
            sso_sequence,
            random,
            timestamp,
            text_bytes: content.text.len(),
            connection: self.connection,
            generation: self.generation,
            packet,
        })
    }
    /// No retries. Any transport/parse failure after the single write is an
    /// unknown send result; keep the host outbox attempt and reconcile read-only.
    pub fn transmit_text(&mut self, prepared: PreparedText) -> Result<SendResponse> {
        if !self.online {
            return Err("QQ session is not registered; packet not transmitted".into());
        }
        prepared.check_bound(
            &self.session,
            &self.connection,
            self.generation,
            self.persisted_sequence,
        )?;
        if self.stream.write_all(&prepared.packet).is_err() {
            self.online = false;
            return Err("QQ send outcome unknown after write; do not retry automatically".into());
        }
        let response = self
            .wait_for(prepared.sso_sequence, message::SEND_COMMAND)
            .and_then(|body| message::decode_response(&body));
        let response = match response {
            Ok(response) => response,
            Err(_) => {
                self.online = false;
                return Err("QQ send outcome unknown while waiting for acknowledgement; do not retry automatically".into());
            }
        };
        if response.result == 0
            && response
                .confirmed_sequence(prepared.target.borrowed()?)
                .is_err()
        {
            return Err("QQ acknowledgement lacks route-specific sequence; outcome unknown, do not retry automatically".into());
        }
        Ok(response)
    }
    pub(crate) fn register(&mut self) -> Result<()> {
        let body = self.registration_body();
        let response = self.request(REGISTER, &body)?;
        self.registration_complete(&response)
    }
    pub(crate) fn register_until(&mut self, deadline: Instant, stop: &AtomicBool) -> Result<()> {
        let body = self.registration_body();
        let response = self.request_until(REGISTER, &body, deadline, stop)?;
        self.registration_complete(&response)
    }
    fn registration_body(&self) -> Vec<u8> {
        let mut device = Vec::new();
        for (field, text) in [
            (1, self.device.device_name.as_str()),
            (2, "Linux"),
            (3, self.device.system_kernel.as_str()),
            (4, ""),
            (5, "linux"),
        ] {
            wire::bytes(field, text.as_bytes(), &mut device);
        }
        let mut body = Vec::new();
        wire::bytes(1, self.device.guid.to_uppercase().as_bytes(), &mut body);
        wire::number(2, 0, &mut body);
        wire::bytes(
            3,
            self.session.app_profile.info().version.as_bytes(),
            &mut body,
        );
        wire::number(4, 0, &mut body);
        wire::number(5, 2052, &mut body);
        wire::bytes(6, &device, &mut body);
        wire::number(7, 0, &mut body);
        wire::number(8, 0, &mut body);
        wire::number(9, 1, &mut body);
        body
    }
    fn registration_complete(&mut self, response: &[u8]) -> Result<()> {
        let fields = Fields::parse(response)?;
        let message = fields.data(2).ok_or("missing registration result")?;
        if message != b"register success" {
            return Err("register rejected".into());
        }
        self.online = true;
        Ok(())
    }
    pub fn heartbeat(&mut self) -> Result<()> {
        if !self.online {
            return Err("QQ session is not registered".into());
        }
        self.request(HEARTBEAT, &[8, 1]).map(|_| ())
    }
}

fn check_request_control(deadline: Instant, stop: &AtomicBool) -> Result<()> {
    if stop.load(Ordering::Relaxed) {
        return Err("QR login canceled".into());
    }
    if Instant::now() >= deadline {
        return Err("QR login deadline exceeded".into());
    }
    Ok(())
}

fn decode_frame(session: &Session, frame: &[u8]) -> Result<Response> {
    let mut r = Reader::new(frame);
    if !matches!(r.u32()?, 10..=12) {
        return Err("unknown response packet type".into());
    }
    let encryption = r.byte()?;
    r.byte()?;
    let _uin = r.packet()?;
    let decrypted = match encryption {
        0 => r.rest().to_vec(),
        1 => tea::decrypt(r.rest(), &session.key()?)?,
        2 => tea::decrypt(r.rest(), &[0; 16])?,
        _ => return Err("unknown response encryption".into()),
    };
    let mut r = Reader::new(&decrypted);
    let mut head = Reader::new(r.packet()?);
    let sequence = head.u32()?;
    let status = head.u32()? as i32;
    head.packet()?; // Server text can contain verification URLs; keep it out of logs.
    if status != 0 {
        return Err(format!("QQ rejected session ({status})"));
    }
    let command =
        String::from_utf8(head.packet()?.to_vec()).map_err(|_| "invalid response command")?;
    if command == "Heartbeat.Alive" {
        return Ok(Response {
            sequence,
            command,
            body: Vec::new(),
        });
    }
    head.packet()?;
    let compression = head.u32()?;
    let declared = r
        .u32()?
        .checked_sub(4)
        .ok_or("invalid response body length")? as usize;
    let body = if declared == 0 {
        r.rest()
    } else {
        r.take(declared)?
    };
    let body = match compression {
        0 => body.to_vec(),
        1 => {
            let mut decoded = Vec::new();
            ZlibDecoder::new(body)
                .take((MAX_FRAME + 1) as u64)
                .read_to_end(&mut decoded)
                .map_err(|e| e.to_string())?;
            if decoded.len() > MAX_FRAME {
                return Err("decompressed frame limit".into());
            }
            decoded
        }
        8 => body
            .get(4..)
            .ok_or("truncated compression-8 body")?
            .to_vec(),
        _ => return Err("unsupported response compression".into()),
    };
    Ok(Response {
        sequence,
        command,
        body,
    })
}

fn extract_frame(incoming: &mut Vec<u8>) -> Result<Option<Vec<u8>>> {
    if incoming.len() < 4 {
        return Ok(None);
    }
    let length = u32::from_be_bytes(incoming[..4].try_into().unwrap()) as usize;
    if !(4..=MAX_FRAME).contains(&length) {
        return Err("incoming frame exceeds limit".into());
    }
    if incoming.len() < length {
        return Ok(None);
    }
    let frame = incoming[4..length].to_vec();
    incoming.drain(..length);
    Ok(Some(frame))
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn management_requests_reject_cancellation_or_expired_deadline_before_encoding() {
        assert!(
            check_request_control(
                Instant::now() + Duration::from_secs(180),
                &AtomicBool::new(true)
            )
            .is_err()
        );
        assert!(check_request_control(Instant::now(), &AtomicBool::new(false)).is_err());
        assert!(
            check_request_control(
                Instant::now() + Duration::from_secs(180),
                &AtomicBool::new(false)
            )
            .is_ok()
        );
    }
    #[test]
    fn native_password_commands_reach_actual_client_signing_boundary_without_transmission() {
        let listener = std::net::TcpListener::bind((std::net::Ipv4Addr::LOCALHOST, 0)).unwrap();
        let stream = TcpStream::connect(listener.local_addr().unwrap()).unwrap();
        let (mut peer, _) = listener.accept().unwrap();
        peer.set_read_timeout(Some(Duration::from_millis(20)))
            .unwrap();
        let mut client=Client{session:session(),config:ClientConfig{session_file:"/tmp/qq-auth-boundary-session".into(),device_file:"/tmp/qq-auth-boundary-device".into(),signer_module:None},device:serde_json::from_value(serde_json::json!({"guid":"000102030405060708090a0b0c0d0e0f","device_name":"synthetic-device","system_kernel":"Linux"})).unwrap(),stream,incoming:Vec::new(),events:VecDeque::new(),event_bytes:0,signer:None,connection:[1;16],generation:1,online:false,persisted_sequence:None};
        for command in [
            crate::auth::PASSWORD_LOGIN_COMMAND,
            crate::auth::PASSWORD_NEW_DEVICE_COMMAND,
            crate::auth::EASY_LOGIN_COMMAND,
        ] {
            assert!(signer::needs_signature(command));
            let error = client
                .encode(1, command, b"synthetic-auth-body")
                .unwrap_err();
            assert_eq!(error, "native signing module must be configured explicitly");
        }
        assert_eq!(
            client
                .encode(1, "unregistered.auth.command", b"synthetic")
                .unwrap_err(),
            "command outside the fixed chat/login allowlist"
        );
        let mut byte = [0u8; 1];
        assert!(
            matches!(peer.read(&mut byte),Err(error) if matches!(error.kind(),std::io::ErrorKind::WouldBlock|std::io::ErrorKind::TimedOut))
        );
    }
    fn session() -> Session {
        let mut s = Session::empty();
        s.uin = 12345;
        s.uid = "u_fixture_only".into();
        s.sequence = 9;
        s
    }
    fn prepared() -> PreparedText {
        PreparedText {
            actor_uin: 12345,
            actor_uid: "u_fixture_only".into(),
            target: SendTarget::Private {
                uid: "u_target_only".into(),
            },
            sequence: 9,
            sso_sequence: 9,
            random: 1,
            timestamp: 1,
            text_bytes: 1,
            connection: [1; 16],
            generation: 2,
            packet: vec![],
        }
    }
    #[test]
    fn prepared_packets_require_persisted_sequence_and_exact_actor_connection_generation() {
        let p = prepared();
        let mut s = session();
        assert!(p.check_bound(&s, &[1; 16], 2, Some(9)).is_ok());
        for saved in [None, Some(8)] {
            assert!(p.check_bound(&s, &[1; 16], 2, saved).is_err());
        }
        assert!(p.check_bound(&s, &[2; 16], 2, Some(9)).is_err());
        assert!(p.check_bound(&s, &[1; 16], 3, Some(9)).is_err());
        s.uin = 12346;
        assert!(p.check_bound(&s, &[1; 16], 2, Some(9)).is_err());
        s = session();
        s.uid = "u_other".into();
        assert!(p.check_bound(&s, &[1; 16], 2, Some(9)).is_err());
        s = session();
        s.sequence = 10;
        assert!(p.check_bound(&s, &[1; 16], 2, Some(9)).is_err());
    }
    #[test]
    fn incremental_frame_parser_keeps_split_and_coalesced_frames_bounded() {
        let packet = wire::packet(b"first");
        let mut incoming = packet[..3].to_vec();
        assert!(extract_frame(&mut incoming).unwrap().is_none());
        incoming.extend_from_slice(&packet[3..]);
        incoming.extend(wire::packet(b"second"));
        assert_eq!(extract_frame(&mut incoming).unwrap().unwrap(), b"first");
        assert_eq!(extract_frame(&mut incoming).unwrap().unwrap(), b"second");
        assert!(incoming.is_empty());
        for n in [0_u32, 1, 2, 3, MAX_FRAME as u32 + 1, u32::MAX] {
            assert!(extract_frame(&mut n.to_be_bytes().to_vec()).is_err());
        }
    }
    fn response(body: &[u8], compression: u32, status: i32) -> Vec<u8> {
        let mut head = 9_u32.to_be_bytes().to_vec();
        head.extend(status.to_be_bytes());
        head.extend(wire::packet(b"private server message"));
        head.extend(wire::packet(b"fixture.Command"));
        head.extend(wire::packet(&[]));
        head.extend(compression.to_be_bytes());
        let mut frame = 10_u32.to_be_bytes().to_vec();
        frame.extend([0, 0]);
        frame.extend(wire::packet(b"12345"));
        frame.extend(wire::packet(&head));
        frame.extend(wire::packet(body));
        frame
    }
    #[test]
    fn response_codec_rejects_truncation_compression_errors_and_hidden_server_error_text() {
        let s = session();
        let raw = response(b"body", 0, 0);
        assert_eq!(decode_frame(&s, &raw).unwrap().body, b"body");
        for n in 0..raw.len() {
            assert!(decode_frame(&s, &raw[..n]).is_err(), "truncation {n}");
        }
        assert!(decode_frame(&s, &response(&[1, 2], 8, 0)).is_err());
        assert!(decode_frame(&s, &response(b"bad", 99, 0)).is_err());
        let error = decode_frame(&s, &response(b"", 0, -10001)).err().unwrap();
        assert_eq!(error, "QQ rejected session (-10001)");
        assert!(!error.contains("private"));
    }
    #[test]
    fn explicit_absolute_paths_and_serialized_destinations_are_required() {
        let c = ClientConfig {
            session_file: "relative.json".into(),
            device_file: "/tmp/device.json".into(),
            signer_module: None,
        };
        assert!(c.validate().is_err());
        let target = SendTarget::Private {
            uid: "u_fixture_only".into(),
        };
        assert_eq!(
            serde_json::from_str::<SendTarget>(&serde_json::to_string(&target).unwrap()).unwrap(),
            target
        );
    }
}
