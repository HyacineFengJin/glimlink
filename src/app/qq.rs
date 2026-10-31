// SPDX-License-Identifier: MIT
//! One lazy QQ connection owned by this Runtime, with bounded commands and inbox.
//! Protocol implementation lives in platform-qq; no experiment subprocess is used.
use super::*;
use crate::{
    outbox::{Operation, Outbox, Status},
    private_state::account_directory,
    qq_inbox::Inbox,
};
use platform_qq::{
    Client as QqClient, ClientConfig, IncomingEvent, LoginMode, LoginOutcome, SendTarget,
};
use serde::{Deserialize, Serialize};
use std::{
    collections::BTreeMap,
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
        mpsc::{self, Receiver, SyncSender},
    },
    thread::{self, JoinHandle},
    time::{Duration, Instant},
};

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct PasswordInput {
    uin: Option<u32>,
    password: Option<String>,
    password_md5: Option<String>,
    app_profile: Option<String>,
    #[serde(default)]
    resume: bool,
    captcha: Option<PasswordCaptcha>,
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct PasswordCaptcha {
    ticket: String,
    #[serde(alias = "rand_str")]
    randstr: String,
    #[serde(alias = "aid")]
    session_id: String,
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct LoginImport {
    session: platform_qq::Session,
    device: Option<platform_qq::Device>,
}
fn password_digest(input: &PasswordInput) -> Result<[u8; 16]> {
    match (&input.password,&input.password_md5) {
        (Some(password),None) if !password.is_empty()&&password.len()<=4096=>{Ok(platform_qq::auth::password_md5(password.as_bytes()))},
        (None,Some(hex)) if hex.len()==32=>{let mut out=[0u8;16];for (index,part) in hex.as_bytes().chunks_exact(2).enumerate(){let value=std::str::from_utf8(part).map_err(|_|"invalid password_md5 encoding")?;out[index]=u8::from_str_radix(value,16).map_err(|_|"invalid password_md5 encoding")?;}Ok(out)},
        _=>Err("provide exactly one nonempty password or32-hex password_md5 in a private operator file".into())
    }
}
fn check_login_binding(directory: &Path, uin: u32, uid: Option<&str>) -> Result<()> {
    if uin == 0 {
        return Err("QQ login requires a nonzero UIN".into());
    }
    let identity = directory.join("identity.json");
    if identity.exists() {
        let bound: Value = serde_json::from_slice(&read_private(&identity, 1024)?)
            .map_err(|_| "invalid QQ account identity binding")?;
        if bound["identity"].as_str() != Some(uin.to_string().as_str()) {
            return Err("QQ login UIN differs from account alias; choose a new alias".into());
        }
    }
    let path = directory.join("session.json");
    if path.exists() {
        let saved = platform_qq::Session::load(&path)?;
        if saved.uin != uin || uid.is_some_and(|uid| !saved.uid.is_empty() && saved.uid != uid) {
            return Err("QQ login candidate belongs to another account identity".into());
        }
    }
    Ok(())
}
fn signer_config(state: &Path) -> Result<Option<PathBuf>> {
    let path = state.join("qq-config.json");
    if !path.exists() {
        return Ok(None);
    }
    let value: Value = serde_json::from_slice(&read_private(&path, 4096)?)
        .map_err(|_| "invalid QQ signer config")?;
    Ok(value["signer_module"].as_str().map(PathBuf::from))
}
fn clear_login_pause(directory: &Path) -> Result<()> {
    for name in [
        "login-challenge.json",
        "login-required.json",
        "password-login.json",
    ] {
        let path = directory.join(name);
        if path.exists() {
            fs::remove_file(path).map_err(|e| e.to_string())?;
        }
    }
    Ok(())
}
fn clear_password_login_state(directory: &Path) -> Result<()> {
    for name in ["password-login.json", "captcha-result.json"] {
        let path = directory.join(name);
        if path.exists() {
            fs::remove_file(path).map_err(|_| "cannot remove pending QQ password state")?;
        }
    }
    let path = directory.join("login-challenge.json");
    if path.exists() {
        let challenge: Value = serde_json::from_slice(&read_private(&path, 65536)?)
            .map_err(|_| "invalid QQ login challenge marker")?;
        if challenge["flow"] == "password" {
            fs::remove_file(path).map_err(|_| "cannot remove QQ password challenge marker")?;
        }
    }
    Ok(())
}

const CHAT_KIND: &str = "task-qq-chat";
const MEDIA_KIND: &str = "task-qq-media";
const MESSAGE_KIND: &str = "task-qq-message";
const MEMBER_KIND: &str = "task-qq-member";
const DIRECTORY_KIND: &str = "task-qq-directory";
const HISTORY_KIND: &str = "task-qq-history";
const SOURCE_KIND: &str = "task-qq-source";
const CURSOR_KIND: &str = "task-qq-messages";
const CHATS_KIND: &str = "task-qq-chats";
fn source_budget() -> Budget {
    Budget {
        max_resource_bytes: 4 * 1024 * 1024,
        max_cache_bytes: 16 * 1024 * 1024,
        max_resources: 64,
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct ChatTarget {
    actor: u32,
    key: String,
    destination: Option<SendTarget>,
}
#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Cursor {
    after: u64,
    chat: Option<ChatTarget>,
}
#[derive(Serialize, Deserialize)]
struct ChatsCursor {
    entries: Vec<(ChatTarget, Value)>,
    offset: usize,
    retention: Value,
    connection_requested: bool,
}

#[derive(Serialize, Deserialize)]
struct MessageTarget {
    chat: ChatTarget,
    sequence: u32,
    sender_uin: u32,
    sender_uid: String,
    timestamp: u32,
    text: String,
}
#[derive(Serialize, Deserialize)]
struct MemberTarget {
    actor: u32,
    group: Option<u32>,
    uid: String,
    uin: u32,
    display: String,
}
#[derive(Serialize, Deserialize)]
struct DirectoryCursor {
    kind: String,
    group: Option<ChatTarget>,
    items: Vec<Value>,
    offset: usize,
    next: Option<String>,
}
#[derive(Clone)]
struct ImageInput {
    resource: String,
    path: PathBuf,
    info: platform_qq::upload::ImageInfo,
}
#[derive(Clone)]
enum AttachmentInput {
    Voice {
        resource: String,
        path: PathBuf,
        info: platform_qq::attachments::VoiceInfo,
    },
    File {
        resource: String,
        path: PathBuf,
        info: platform_qq::attachments::BlobInfo,
        name: String,
    },
    Video {
        resource: String,
        path: PathBuf,
        thumbnail_path: PathBuf,
        thumbnail_source: String,
        info: platform_qq::attachments::VideoInfo,
    },
}
impl AttachmentInput {
    fn frozen(&self) -> Value {
        match self {
            Self::Voice { resource, info, .. } => {
                json!({"kind":"voice","resource":resource,"info":info})
            }
            Self::File {
                resource,
                info,
                name,
                ..
            } => json!({"kind":"file","resource":resource,"info":info,"filename":name}),
            Self::Video {
                resource,
                thumbnail_source,
                info,
                ..
            } => {
                json!({"kind":"video","resource":resource,"thumbnail_source":thumbnail_source,"info":info})
            }
        }
    }
    fn label(&self) -> &str {
        match self {
            Self::Voice { .. } => "[语音]",
            Self::File { .. } => "[文件]",
            Self::Video { .. } => "[视频]",
        }
    }
}
enum Command {
    Status,
    Read {
        after: Option<u64>,
        limit: usize,
        wait_ms: u64,
    },
    Send {
        target: ChatTarget,
        message: platform_qq::rich::Message,
        image: Option<ImageInput>,
        operation_id: String,
    },
    SendAttachment {
        target: ChatTarget,
        message: platform_qq::rich::Message,
        attachment: AttachmentInput,
        operation_id: String,
    },
    Directory {
        kind: String,
        group: Option<u32>,
        token: Option<String>,
    },
    History {
        target: ChatTarget,
        before: u32,
        start: u32,
        end: u32,
        count: u32,
    },
    Media {
        locator: platform_qq::rich::MediaLocator,
    },
    Stop,
}
struct Request {
    command: Command,
    response: SyncSender<Result<Value>>,
}
struct Worker {
    alias: String,
    tx: Option<SyncSender<Request>>,
    stop: Arc<AtomicBool>,
    handle: Option<JoinHandle<()>>,
}
impl Worker {
    fn request(&self, command: Command) -> Result<Value> {
        let (tx, rx) = mpsc::sync_channel(1);
        self.tx
            .as_ref()
            .ok_or("QQ worker is stopped")?
            .try_send(Request {
                command,
                response: tx,
            })
            .map_err(|_| "QQ connection stopped or command queue is full")?;
        rx.recv_timeout(Duration::from_secs(55)).map_err(|_|"QQ worker response unavailable; an interrupted send must not be retried with a new operation ID")?
    }
}
impl Drop for Worker {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Relaxed);
        let (tx, _) = mpsc::sync_channel(1);
        if let Some(sender) = self.tx.take() {
            let _ = sender.try_send(Request {
                command: Command::Stop,
                response: tx,
            });
        }
        if let Some(handle) = self.handle.take() {
            let _ = handle.join();
        }
    }
}
#[derive(Default)]
pub(super) struct QqState {
    worker: Option<Worker>,
    retry_after: Option<(String, Instant)>,
    pending_password: Option<PendingPasswordLogin>,
}
// Operator-private, process-local state. No Serialize/Debug and no password on disk.
struct PasswordDigest([u8; 16]);
impl Drop for PasswordDigest {
    fn drop(&mut self) {
        self.0.fill(0);
    }
}
struct PendingPasswordLogin {
    alias: String,
    client: QqClient,
    continuation: platform_qq::password::PasswordContinuation,
    digest: PasswordDigest,
    deadline: Instant,
}
impl QqState {
    fn start(&mut self, state: &Path, alias: &str) -> Result<()> {
        if let Some(pending) = &self.pending_password {
            if pending.alias == alias {
                if Instant::now() < pending.deadline
                    && platform_qq::unix_seconds()? < pending.continuation.expires_at()
                {
                    return Err("QQ password verification is pending on the operator connection; finish or cancel that login first".into());
                }
                self.pending_password.take();
                clear_password_login_state(&account_directory(state, "qq", alias)?)?;
            }
        }
        if self
            .worker
            .as_ref()
            .is_some_and(|w| w.alias == alias && !w.handle.as_ref().unwrap().is_finished())
        {
            return Ok(());
        }
        if let Some((name, deadline)) = &self.retry_after {
            if name == alias && Instant::now() < *deadline {
                return Err(format!(
                    "QQ login reconnect cooldown: retry after {} seconds, or read saved messages with refresh=false",
                    deadline.saturating_duration_since(Instant::now()).as_secs() + 1
                ));
            }
        }
        self.worker.take();
        let (tx, rx) = mpsc::sync_channel(4);
        let (ready_tx, ready_rx) = mpsc::sync_channel(1);
        let state = state.to_owned();
        let name = alias.to_owned();
        let stop = Arc::new(AtomicBool::new(false));
        let worker_stop = stop.clone();
        let handle = thread::Builder::new()
            .name("qq-connection".into())
            .spawn(move || match Connection::open(&state, &name) {
                Ok(mut connection) => {
                    if ready_tx.send(Ok(())).is_ok() {
                        connection.run(rx, worker_stop);
                    }
                }
                Err(error) => {
                    let _ = ready_tx.send(Err(error));
                }
            })
            .map_err(|e| e.to_string())?;
        let worker = Worker {
            alias: alias.into(),
            tx: Some(tx),
            stop,
            handle: Some(handle),
        };
        match ready_rx.recv_timeout(Duration::from_secs(55)) {
            Ok(Ok(())) => {
                self.worker = Some(worker);
                self.retry_after = None;
                Ok(())
            }
            Ok(Err(error)) => {
                self.retry_after = Some((alias.into(), Instant::now() + Duration::from_secs(30)));
                Err(error)
            }
            Err(_) => {
                self.retry_after = Some((alias.into(), Instant::now() + Duration::from_secs(30)));
                Err("QQ login response unavailable; stopping the connection, no QR login was started".into())
            }
        }
    }
    fn request(&self, command: Command) -> Result<Value> {
        self.worker
            .as_ref()
            .ok_or("QQ connection is not started")?
            .request(command)
    }
}

struct Connection {
    state: PathBuf,
    directory: PathBuf,
    scope: Scope,
    client: QqClient,
    inbox: Inbox,
    sources: ResourceStore,
    auth_source: String,
    last_heartbeat: Instant,
}
impl Connection {
    fn directory(&mut self, kind: &str, group: Option<u32>, token: Option<&str>) -> Result<Value> {
        match kind {
            "friends" => serde_json::to_value(
                self.client.friends(
                    token
                        .unwrap_or("0")
                        .parse()
                        .map_err(|_| "invalid friend cursor")?,
                )?,
            )
            .map_err(|e| e.to_string()),
            "groups" => Ok(json!({"items":self.client.groups()?,"next":null})),
            "members" => serde_json::to_value(
                self.client
                    .members(group.ok_or("group target missing")?, token.unwrap_or(""))?,
            )
            .map_err(|e| e.to_string()),
            _ => Err("QQ directory kind must be friends, groups or members".into()),
        }
    }
    fn history(
        &mut self,
        target: &ChatTarget,
        before: u32,
        start: u32,
        end: u32,
        count: u32,
    ) -> Result<Value> {
        if target.actor != self.client.session().uin {
            return Err("QQ history owner mismatch".into());
        }
        let history = match &target.destination {
            Some(SendTarget::SelfAccount) => {
                self.client
                    .private_history(&self.client.session().uid.clone(), before, count)?
            }
            Some(SendTarget::Private { uid }) => self.client.private_history(uid, before, count)?,
            Some(SendTarget::Group { code }) => {
                let end = if end == 0 {
                    self.client.group_latest_sequence(*code)?
                } else {
                    end
                };
                if end == 0 {
                    return Ok(json!({"items":[],"next_time":null,"next_sequence":null}));
                }
                let start = if start == 0 {
                    end.saturating_sub(count - 1).max(1)
                } else {
                    start
                };
                self.client.group_history(*code, start, end)?
            }
            None => return Err("QQ history target unresolved".into()),
        };
        let mut items = Vec::new();
        for event in history.events {
            if let Some(value) = self.project_event(event)? {
                let actual: ChatTarget = serde_json::from_value(value["conversation"].clone())
                    .map_err(|_| "QQ history destination missing")?;
                if actual.actor != target.actor || actual.destination != target.destination {
                    return Err("QQ server history returned another conversation".into());
                }
                items.push(json!({"event":value}));
            }
        }
        Ok(
            json!({"items":items,"next_time":history.next_time,"next_sequence":history.next_sequence}),
        )
    }
    fn open(state: &Path, alias: &str) -> Result<Self> {
        let scope = Scope {
            platform: Platform::Qq,
            account: alias.into(),
        };
        let directory = account_directory(state, "qq", alias)?;
        if directory.join("login-challenge.json").exists() {
            return Err("QQ login verification is pending in private management state; automatic login is paused".into());
        }
        if directory.join("login-required.json").exists() {
            return Err("QQ saved credentials expired; use explicit management password-login, import-login or QR; automatic login is paused".into());
        }
        let config_path = state.join("qq-config.json");
        let signer_module = if config_path.exists() {
            let value: Value = serde_json::from_slice(&read_private(&config_path, 4096)?)
                .map_err(|_| "invalid private QQ config")?;
            value
                .get("signer_module")
                .and_then(Value::as_str)
                .map(PathBuf::from)
        } else {
            None
        };
        let session = platform_qq::Session::load(&directory.join("session.json"))?;
        let expected = session.uin;
        if expected == 0 || session.uid.is_empty() {
            return Err("QQ saved account identity is incomplete".into());
        }
        super::auth::bind_identity(&directory, &expected.to_string())?;
        let device = platform_qq::Device::load(&directory.join("device.json"))?;
        let mut client = QqClient::connect(
            session,
            device,
            ClientConfig {
                session_file: directory.join("session.json"),
                device_file: directory.join("device.json"),
                signer_module,
            },
        )?;
        let source = match client.authenticate(LoginMode::Auto)? {
            LoginOutcome::Online { source } => format!("{source:?}"),
            LoginOutcome::VerificationRequired { challenge } => {
                use base64::Engine;
                platform_qq::session::write_private_json(
                    &directory.join("login-challenge.json"),
                    &json!({"code":challenge.code,"kind":format!("{:?}",challenge.kind),"verify_url":challenge.verify_url,"cookies":challenge.cookies,"unusual_sig":base64::engine::general_purpose::STANDARD.encode(challenge.unusual_sig)}),
                )?;
                return Err("QQ login verification required; details saved privately, automatic login paused".into());
            }
            LoginOutcome::Rejected { failure } => {
                if matches!(
                    failure.kind,
                    platform_qq::auth::LoginFailureKind::TokenExpired
                        | platform_qq::auth::LoginFailureKind::UserTokenExpired
                ) {
                    platform_qq::session::write_private_json(
                        &directory.join("login-required.json"),
                        &json!({"schema":1,"status":"login_required","reason":"token_expired","code":failure.code}),
                    )?;
                }
                return Err(format!(
                    "QQ saved login rejected (code {}); no QR login or chat send was started",
                    failure.code
                ));
            }
            LoginOutcome::CredentialsUnavailable => {
                return Err(
                    "QQ cached credentials unavailable; no QR login or chat send was started"
                        .into(),
                );
            }
        };
        if expected == 0 || client.session().uin != expected {
            return Err("QQ account identity changed during login".into());
        }
        client.heartbeat()?;
        client.persist_session()?;
        super::auth::bind_identity(&directory, &expected.to_string())?;
        let sources = ResourceStore::open(&directory.join("sources"), source_budget())?;
        sources.recover_orphans()?;
        Ok(Self {
            state: state.into(),
            directory: directory.clone(),
            scope: scope.clone(),
            client,
            inbox: Inbox::open(state, &scope)?,
            sources,
            auth_source: source,
            last_heartbeat: Instant::now(),
        })
    }
    fn status(&self) -> Value {
        json!({"status":if self.client.is_online(){"online"}else{"offline"},"scope":self.scope,"identity":self.client.session().uin.to_string(),"auth_source":self.auth_source,"qr_started":false,"connection":"single_background_worker","history":"runtime_observed_only","deduplication":"exact_payload"})
    }
    fn ingest(&mut self, event: IncomingEvent) -> Result<bool> {
        if self.inbox.contains(&event.key) {
            return Ok(false);
        }
        let key = event.key.clone();
        let Some(value) = self.project_event(event)? else {
            return Ok(false);
        };
        self.inbox.append(&key, &value)
    }
    // Reading server history is not a new incoming event and never wakes a watch.
    fn project_event(&mut self, event: IncomingEvent) -> Result<Option<Value>> {
        let Some(view) = event.view else {
            return Ok(None);
        };
        if !view.chat {
            return Ok(None);
        }
        let own = self.client.session().uin;
        let destination = reply_destination(&view, own, &self.client.session().uid);
        let key = match &destination {
            Some(target) => chat_key(target)?,
            None => format!(
                "unresolved:{}:{}:{}",
                view.from_uin.unwrap_or(0),
                view.to_uin.unwrap_or(0),
                view.group_uin.unwrap_or(0)
            ),
        };
        let source = (|| -> Result<ResourceRef> {
            let mut saved = self.sources.list(&self.scope, 0, 64)?;
            let mut total: u64 = saved.iter().map(|r| r.bytes).sum();
            while saved.len() >= 64
                || total.saturating_add(event.raw.len() as u64) > 16 * 1024 * 1024
            {
                if saved.is_empty() {
                    return Err("QQ source exceeds cache budget".into());
                }
                let oldest = saved.remove(0);
                self.sources.remove(&self.scope, &oldest.id)?;
                total = total.saturating_sub(oldest.bytes);
            }
            self.sources.import(
                &self.scope,
                &event.key,
                "application/x-protobuf",
                &mut event.raw.as_slice(),
            )
        })();
        let (source_resource, source_status) = match source {
            Ok(r) => (Some(r.id), "retained"),
            Err(_) => (None, "unavailable_cache_budget"),
        };
        let (text, text_cut) = bounded_projection(&view.text, 12000);
        let (html, html_cut) = bounded_projection(&view.content, 12000);
        let value = json!({"conversation":ChatTarget{actor:own,key,destination},"kind":view.kind,"message_sequence":view.sequence,"server_time":view.timestamp,"message_id":view.message_id,"new_id":view.new_id,"from_uin":view.from_uin,"to_uin":view.to_uin,"group_code":view.group_uin,"direction":if view.from_uin==Some(u64::from(own)){"outgoing"}else{"incoming"},"text":text,"html":html,"truncated":view.truncated||text_cut||html_cut,"unexpanded":view.unexpanded,"source_resource":source_resource,"source_status":source_status,"segments":view.segments,"from_uid":view.from_uid,"to_uid":view.to_uid});
        Ok(Some(value))
    }
    fn poll(&mut self, duration: Duration) -> Result<bool> {
        let mut added = false;
        if let Some(event) = self.client.poll(duration)? {
            added = self.ingest(event)?;
        }
        if self.last_heartbeat.elapsed() >= Duration::from_secs(270) {
            self.client.heartbeat()?;
            self.client.persist_session()?;
            self.last_heartbeat = Instant::now();
        }
        Ok(added)
    }
    fn run(&mut self, rx: Receiver<Request>, stop: Arc<AtomicBool>) {
        while !stop.load(Ordering::Relaxed) {
            match rx.try_recv() {
                Ok(Request {
                    command: Command::Stop,
                    response,
                }) => {
                    let _ = response.send(Ok(json!({"status":"stopped"})));
                    break;
                }
                Ok(Request { command, response }) => {
                    let result = match command {
                        Command::Status => Ok(self.status()),
                        Command::Read {
                            after,
                            limit,
                            wait_ms,
                        } => {
                            let deadline = Instant::now() + Duration::from_millis(wait_ms);
                            let read = (|| -> Result<Value> {
                                let saved = self.inbox.read(after, limit)?;
                                if !saved.items.is_empty() {
                                    return serde_json::to_value(saved).map_err(|e| e.to_string());
                                }
                                while wait_ms > 0
                                    && Instant::now() < deadline
                                    && !stop.load(Ordering::Relaxed)
                                {
                                    if self.poll(Duration::from_millis(200))? {
                                        break;
                                    }
                                }
                                serde_json::to_value(self.inbox.read(after, limit)?)
                                    .map_err(|e| e.to_string())
                            })();
                            read
                        }
                        Command::Send {
                            target,
                            message,
                            image,
                            operation_id,
                        } => self.send(&target, &message, image.as_ref(), &operation_id),
                        Command::SendAttachment {
                            target,
                            message,
                            attachment,
                            operation_id,
                        } => self.send_attachment(&target, &message, &attachment, &operation_id),
                        Command::Directory { kind, group, token } => {
                            self.directory(&kind, group, token.as_deref())
                        }
                        Command::History {
                            target,
                            before,
                            start,
                            end,
                            count,
                        } => self.history(&target, before, start, end, count),
                        Command::Media { locator } => self
                            .client
                            .media_url(&locator)
                            .map(|url| json!({"private_url":url})),
                        Command::Stop => unreachable!(),
                    };
                    let _ = response.send(result);
                }
                Err(mpsc::TryRecvError::Disconnected) => break,
                Err(mpsc::TryRecvError::Empty) => {}
            }
            if self.poll(Duration::from_millis(200)).is_err() {
                break;
            }
        }
        let _ = self.client.persist_session();
    }
    fn send_attachment(
        &mut self,
        target: &ChatTarget,
        content: &platform_qq::rich::Message,
        input: &AttachmentInput,
        id: &str,
    ) -> Result<Value> {
        let actor = self.client.session().uin;
        if actor != target.actor {
            return Err("QQ attachment owner mismatch".into());
        }
        let destination = target
            .destination
            .as_ref()
            .ok_or("QQ attachment destination unresolved")?;
        let outbox = Outbox::open(&self.state)?;
        check_legacy(&self.directory, id)?;
        if outbox.exists(id)? {
            return replay_attachment(&outbox, &self.scope, target, content, input, id);
        }
        let mut random = [0; 4];
        platform_qq::tea::random(&mut random)?;
        let random = u32::from_le_bytes(random);
        let summary = if rich_summary(content).is_empty() {
            input.label().into()
        } else {
            rich_summary(content)
        };
        let mut record = Operation::new(
            id,
            self.scope.clone(),
            actor.to_string(),
            serde_json::to_value(target).map_err(|e| e.to_string())?,
            &summary,
        )?;
        record.request = Some(
            json!({"action":"qq_attachment","attachment":input.frozen(),"rich_content":content,"random":random,"resolved_destination":destination}),
        );
        outbox.insert(&record)?;
        outbox.begin_attempt(&mut record)?;
        let attempt = (|| -> Result<(Status, Value)> {
            let mut message = content.clone();
            match input {
                AttachmentInput::Voice { path, info, .. } => message
                    .media
                    .push(self.client.upload_voice(destination, path, info, random)?),
                AttachmentInput::File {
                    path, info, name, ..
                } => {
                    let file = self
                        .client
                        .upload_file(destination, path, info, name, random)?;
                    if let SendTarget::Group { code } = destination {
                        let receipt = self.client.publish_group_file(*code, &file, random)?;
                        return Ok((
                            if receipt["result"] == 0 {
                                Status::Sent
                            } else {
                                Status::Rejected
                            },
                            receipt,
                        ));
                    }
                    message.file = Some(file);
                }
                AttachmentInput::Video {
                    path,
                    thumbnail_path,
                    info,
                    ..
                } => message.media.push(self.client.upload_video(
                    destination,
                    path,
                    thumbnail_path,
                    info,
                    random,
                )?),
            }
            let prepared = self
                .client
                .prepare_rich(destination.clone(), &message, random)?;
            self.client.persist_session()?;
            let receipt = self.client.transmit_text(prepared)?;
            let route = match destination {
                SendTarget::SelfAccount => {
                    platform_qq::message::Target::Private(&self.client.session().uid)
                }
                SendTarget::Private { uid } => platform_qq::message::Target::Private(uid),
                SendTarget::Group { code } => platform_qq::message::Target::Group(*code),
            };
            let status = if receipt.result != 0 {
                Status::Rejected
            } else if receipt.confirmed_sequence(route).is_ok() {
                Status::Sent
            } else {
                Status::Unknown
            };
            Ok((
                status,
                serde_json::to_value(receipt).map_err(|e| e.to_string())?,
            ))
        })();
        match attempt {
            Ok((status, receipt)) => {
                record.status = status;
                record.receipt = Some(receipt)
            }
            Err(e) => {
                record.status = Status::Unknown;
                record.error = Some(e)
            }
        }
        outbox.finish(&record)?;
        Ok(send_result(&record, false))
    }
    fn send_image(
        &mut self,
        target: &ChatTarget,
        content: &platform_qq::rich::Message,
        image: &ImageInput,
        id: &str,
    ) -> Result<Value> {
        let actor = self.client.session().uin;
        if target.actor != actor {
            return Err("QQ image target owner mismatch".into());
        }
        let destination = target
            .destination
            .as_ref()
            .ok_or("QQ image destination unresolved")?;
        let outbox = Outbox::open(&self.state)?;
        check_legacy(&self.directory, id)?;
        if outbox.exists(id)? {
            return replay_image(&outbox, &self.scope, target, content, image, id);
        }
        let mut random = [0; 4];
        platform_qq::tea::random(&mut random)?;
        let random = u32::from_le_bytes(random);
        let summary = if content.text.is_empty() {
            "[图片]".to_owned()
        } else {
            content.text.clone()
        };
        let mut record = Operation::new(
            id,
            self.scope.clone(),
            actor.to_string(),
            serde_json::to_value(target).map_err(|e| e.to_string())?,
            &summary,
        )?;
        record.request = Some(
            json!({"action":"qq_image","rich_content":content,"image_resource":image.resource,"image_info":image.info,"random":random,"resolved_destination":destination}),
        );
        outbox.insert(&record)?;
        outbox.begin_attempt(&mut record)?;
        let attempt = (|| -> Result<platform_qq::SendResponse> {
            let uploaded =
                self.client
                    .upload_image(destination, &image.path, &image.info, random)?;
            let mut message = content.clone();
            message.images.push(uploaded);
            let prepared = self
                .client
                .prepare_rich(destination.clone(), &message, random)?;
            self.client.persist_session()?;
            self.client.transmit_text(prepared)
        })();
        match attempt {
            Ok(receipt) => {
                let route = match destination {
                    SendTarget::SelfAccount => {
                        platform_qq::message::Target::Private(&self.client.session().uid)
                    }
                    SendTarget::Private { uid } => platform_qq::message::Target::Private(uid),
                    SendTarget::Group { code } => platform_qq::message::Target::Group(*code),
                };
                record.status = if receipt.result != 0 {
                    Status::Rejected
                } else if receipt.confirmed_sequence(route).is_ok() {
                    Status::Sent
                } else {
                    Status::Unknown
                };
                record.receipt = Some(
                    json!({"send":receipt,"image_info":image.info,"image_resource":image.resource}),
                );
            }
            Err(e) => {
                record.status = Status::Unknown;
                record.error = Some(e)
            }
        }
        outbox.finish(&record)?;
        Ok(send_result(&record, false))
    }
    fn send(
        &mut self,
        target: &ChatTarget,
        content: &platform_qq::rich::Message,
        image: Option<&ImageInput>,
        id: &str,
    ) -> Result<Value> {
        if let Some(image) = image {
            return self.send_image(target, content, image, id);
        }
        let message = rich_summary(content);
        let actor = self.client.session().uin;
        if target.actor != actor {
            return Err("QQ target belongs to another account".into());
        }
        let destination = target
            .destination
            .as_ref()
            .ok_or("QQ destination UID is unavailable; no numeric-account guess was made")?;
        let outbox = Outbox::open(&self.state)?;
        check_legacy(&self.directory, id)?;
        if outbox.exists(id)? {
            return replay_rich(&outbox, &self.scope, target, content, id);
        }
        let mut random = [0; 4];
        platform_qq::tea::random(&mut random)?;
        let random = u32::from_le_bytes(random);
        let prepared = self
            .client
            .prepare_rich(destination.clone(), content, random)?;
        self.client.persist_session()?;
        let mut record = Operation::new(
            id,
            self.scope.clone(),
            actor.to_string(),
            serde_json::to_value(target).map_err(|e| e.to_string())?,
            &message,
        )?;
        record.request = Some(
            json!({"action":if *content==platform_qq::rich::Message::plain(&content.text){"qq_text"}else{"qq_rich"},"rich_content":content,"random":prepared.random(),"session_sequence":prepared.sequence(),"sso_sequence":prepared.sso_sequence(),"timestamp":prepared.timestamp(),"resolved_destination":prepared.target()}),
        );
        outbox.insert(&record)?;
        outbox.begin_attempt(&mut record)?;
        match self.client.transmit_text(prepared) {
            Ok(receipt) => {
                let route = match destination {
                    SendTarget::SelfAccount => {
                        platform_qq::message::Target::Private(&self.client.session().uid)
                    }
                    SendTarget::Private { uid } => platform_qq::message::Target::Private(uid),
                    SendTarget::Group { code } => platform_qq::message::Target::Group(*code),
                };
                record.status = if receipt.result != 0 {
                    Status::Rejected
                } else if receipt.confirmed_sequence(route).is_ok() {
                    Status::Sent
                } else {
                    Status::Unknown
                };
                record.receipt = Some(serde_json::to_value(receipt).map_err(|e| e.to_string())?);
            }
            Err(error) => {
                record.status = Status::Unknown;
                record.error = Some(error);
            }
        }
        outbox.finish(&record)?;
        Ok(send_result(&record, false))
    }
}

fn send_result(record: &Operation, reused: bool) -> Value {
    json!({"status":record.status,"scope":record.scope,"operation_id":record.id,"message":record.message,"receipt":record.receipt,"error":record.error,"reused_operation":reused,"readback_verified":record.status==Status::Verified,"delivery_evidence":"server_acknowledgement_only","automatic_resend":false})
}
fn rich_summary(content: &platform_qq::rich::Message) -> String {
    let mut s = content.text.clone();
    for m in &content.mentions {
        s.push_str(&m.display)
    }
    for id in &content.faces {
        s.push_str(&format!("[QQ表情:{id}]"))
    }
    for part in &content.parts {
        match part {
            platform_qq::rich::Part::Text { text } => s.push_str(text),
            platform_qq::rich::Part::Mention { display, .. } => s.push_str(display),
            platform_qq::rich::Part::Face { id } => s.push_str(&format!("[QQ表情:{id}]")),
        }
    }
    s.chars().take(1000).collect()
}
fn replay_image(
    outbox: &Outbox,
    scope: &Scope,
    target: &ChatTarget,
    content: &platform_qq::rich::Message,
    image: &ImageInput,
    id: &str,
) -> Result<Value> {
    let mut r = outbox.load(id, scope)?;
    let req = r
        .request
        .as_ref()
        .ok_or("QQ image operation request missing")?;
    if r.actor_id != target.actor.to_string()
        || r.target != serde_json::to_value(target).map_err(|e| e.to_string())?
        || req["action"] != "qq_image"
        || req["rich_content"] != serde_json::to_value(content).map_err(|e| e.to_string())?
        || req["image_resource"] != image.resource
        || req["image_info"] != serde_json::to_value(&image.info).map_err(|e| e.to_string())?
    {
        return Err("operation_id belongs to another exact QQ image message".into());
    }
    if r.status == Status::Attempting {
        r.status = Status::Unknown;
        r.error = Some("Previous image attempt ended without a result; do not resend".into());
        outbox.finish(&r)?
    }
    Ok(send_result(&r, true))
}
fn replay_attachment(
    outbox: &Outbox,
    scope: &Scope,
    target: &ChatTarget,
    content: &platform_qq::rich::Message,
    input: &AttachmentInput,
    id: &str,
) -> Result<Value> {
    let mut record = outbox.load(id, scope)?;
    let request = record
        .request
        .as_ref()
        .ok_or("QQ attachment operation metadata missing")?;
    if record.actor_id != target.actor.to_string()
        || record.target != serde_json::to_value(target).map_err(|e| e.to_string())?
        || request["action"] != "qq_attachment"
        || request["attachment"] != input.frozen()
        || request["rich_content"] != serde_json::to_value(content).map_err(|e| e.to_string())?
    {
        return Err("operation_id belongs to another exact QQ attachment".into());
    }
    if record.status == Status::Attempting {
        record.status = Status::Unknown;
        record.error =
            Some("Previous attachment attempt ended without a result; do not resend".into());
        outbox.finish(&record)?
    }
    Ok(send_result(&record, true))
}
fn replay_rich(
    outbox: &Outbox,
    scope: &Scope,
    target: &ChatTarget,
    content: &platform_qq::rich::Message,
    id: &str,
) -> Result<Value> {
    let r = outbox.load(id, scope)?;
    if content == &platform_qq::rich::Message::plain(&content.text) {
        return replay(outbox, scope, target, &content.text, id);
    }
    if r.actor_id != target.actor.to_string()
        || r.target != serde_json::to_value(target).map_err(|e| e.to_string())?
        || r.request.as_ref().and_then(|r| r.get("rich_content"))
            != Some(&serde_json::to_value(content).map_err(|e| e.to_string())?)
    {
        return Err("operation_id belongs to another exact QQ rich message".into());
    }
    let mut r = r;
    if r.status == Status::Attempting {
        r.status = Status::Unknown;
        r.error = Some("Previous process ended after attempt; do not resend".into());
        outbox.finish(&r)?
    }
    Ok(send_result(&r, true))
}
fn replay(
    outbox: &Outbox,
    scope: &Scope,
    target: &ChatTarget,
    message: &str,
    id: &str,
) -> Result<Value> {
    let mut record = outbox.load(id, scope)?;
    if record.actor_id != target.actor.to_string()
        || record.target != serde_json::to_value(target).map_err(|e| e.to_string())?
        || record.message != message
        || record.request.as_ref().and_then(|r| r["action"].as_str()) != Some("qq_text")
    {
        return Err("operation_id belongs to another QQ action or target".into());
    }
    if record.status == Status::Attempting {
        record.status = Status::Unknown;
        record.error =
            Some("Previous process ended after its durable attempt; delivery is unknown".into());
        outbox.finish(&record)?;
    }
    // Prepared after an interrupted local preparation is also not auto-sent.
    let mut value = send_result(&record, true);
    if record.status == Status::Prepared {
        value["status"] = json!("not_attempted");
        value["detail"] = json!("Local preparation was interrupted; no automatic send was started");
    }
    Ok(value)
}
fn read_private(path: &Path, limit: u64) -> Result<Vec<u8>> {
    let file = open_regular(path, limit)?;
    if file
        .metadata()
        .map_err(|e| e.to_string())?
        .permissions()
        .mode()
        & 0o077
        != 0
    {
        return Err("QQ private state must have 0600 permissions".into());
    }
    read_bounded(path, limit)
}
fn check_legacy(directory: &Path, id: &str) -> Result<()> {
    let path = directory.join("legacy-consumed.json");
    if !path.exists() {
        return Ok(());
    }
    let data: Value = serde_json::from_slice(&read_private(&path, 65536)?)
        .map_err(|_| "invalid QQ legacy operation index")?;
    if data["operation_ids"]
        .as_array()
        .is_some_and(|ids| ids.iter().any(|old| old.as_str() == Some(id)))
    {
        return Err("This QQ operation was already consumed by the legacy client and will not be sent again".into());
    }
    Ok(())
}
fn valid_uid(uid: &str) -> bool {
    uid.starts_with("u_")
        && uid.len() <= 128
        && uid
            .bytes()
            .all(|v| v.is_ascii_alphanumeric() || b"_-".contains(&v))
}
fn bounded_projection(text: &str, limit: usize) -> (&str, bool) {
    let mut bytes = 0;
    for (offset, c) in text.char_indices() {
        let encoded = match c {
            '"' | '\\' => 2,
            c if c < '\u{20}' => 6,
            c => c.len_utf8(),
        };
        if bytes + encoded > limit {
            return (&text[..offset], true);
        }
        bytes += encoded;
    }
    (text, false)
}
fn reply_destination(view: &platform_qq::View, own: u32, own_uid: &str) -> Option<SendTarget> {
    if !view.chat {
        return None;
    }
    if view.kind == 82 {
        return view
            .group_uin
            .filter(|v| *v > 0)
            .and_then(|v| u32::try_from(v).ok())
            .map(|code| SendTarget::Group { code });
    }
    if view.group_uin.is_some_and(|v| v != 0) {
        return None;
    }
    let endpoint = |uin: Option<u64>, uid: Option<&str>| -> Option<bool> {
        match (uin, uid) {
            (Some(uin), Some(uid)) if (uin == u64::from(own)) != (uid == own_uid) => None,
            (Some(uin), _) => Some(uin == u64::from(own)),
            (_, Some(uid)) => Some(uid == own_uid),
            _ => None,
        }
    };
    let from = endpoint(view.from_uin, view.from_uid.as_deref())?;
    let to = endpoint(view.to_uin, view.to_uid.as_deref())?;
    match (from, to) {
        (true, true) => Some(SendTarget::SelfAccount),
        (false, true) => view
            .from_uid
            .as_ref()
            .filter(|uid| valid_uid(uid))
            .map(|uid| SendTarget::Private { uid: uid.clone() }),
        (true, false) => view
            .to_uid
            .as_ref()
            .filter(|uid| valid_uid(uid))
            .map(|uid| SendTarget::Private { uid: uid.clone() }),
        _ => None,
    }
}
fn chat_key(target: &SendTarget) -> Result<String> {
    serde_json::to_string(target).map_err(|e| e.to_string())
}
fn boolean(params: &Value, key: &str, default: bool) -> Result<bool> {
    params
        .get(key)
        .map(|v| {
            v.as_bool()
                .ok_or_else(|| format!("{key} must be a boolean"))
        })
        .unwrap_or(Ok(default))
}

fn wire_projection(bytes: &[u8], depth: usize, nodes: &mut usize) -> Value {
    use base64::Engine;
    use platform_qq::wire::{Fields, Value as WireValue};
    let Ok(fields) = Fields::parse(bytes) else {
        return json!({"bytes":bytes.len(),"decodable_protobuf":false});
    };
    let mut output = Vec::new();
    for (field, value) in fields.0 {
        if *nodes >= 256 {
            return json!({"fields":output,"truncated":true});
        }
        *nodes += 1;
        let value = match value {
            WireValue::Fixed | WireValue::Fixed32(_) => {
                json!({"field":field,"wire_value":"fixed_width","detail":"available in raw source"})
            }
            WireValue::Number(value) => json!({"field":field,"integer":value.to_string()}),
            WireValue::Bytes(bytes) => {
                if let Some(text) = std::str::from_utf8(bytes).ok().filter(|s| {
                    !s.chars()
                        .any(|c| c.is_control() && !matches!(c, '\n' | '\r' | '\t'))
                }) {
                    json!({"field":field,"text":text.chars().take(1024).collect::<String>(),"bytes":bytes.len(),"truncated":text.chars().count()>1024})
                } else if depth < 5 && !bytes.is_empty() && Fields::parse(bytes).is_ok() {
                    json!({"field":field,"message":wire_projection(bytes,depth+1,nodes)})
                } else {
                    json!({"field":field,"bytes":bytes.len(),"base64_prefix":base64::engine::general_purpose::STANDARD.encode(&bytes[..bytes.len().min(256)]),"truncated":bytes.len()>256})
                }
            }
        };
        output.push(value);
    }
    json!({"fields":output,"schema":"protobuf_wire_projection","semantic_fields_complete":false})
}

impl Runtime {
    /// Management-only synthetic signing preflight. Never opens QQ or reads tickets.
    pub fn qq_signer_check(&self) -> Result<Value> {
        let config: Value =
            serde_json::from_slice(&read_private(&self.state.join("qq-config.json"), 4096)?)
                .map_err(|_| "invalid QQ signer config")?;
        let path = Path::new(
            config["signer_module"]
                .as_str()
                .ok_or("QQ signer is not configured")?,
        );
        let mut signer = platform_qq::signer::Signer::load(path)?;
        let body = platform_qq::message::encode_text(
            platform_qq::message::Target::Private("u_synthetic_fixture"),
            "offline-signature-probe",
            7,
            19,
            1700000000,
        )?;
        let signature = signer.sign(platform_qq::message::SEND_COMMAND, 8, &body)?;
        Ok(
            json!({"status":"ready","synthetic":true,"transmitted":false,"native_signer":true,"signature_bytes":signature.sign.len(),"token_bytes":signature.token.len(),"extra_bytes":signature.extra.len()}),
        )
    }
    fn qq_page(
        &mut self,
        refresh: bool,
        after: Option<u64>,
        limit: usize,
        wait_ms: u64,
    ) -> Result<Value> {
        if refresh {
            self.qq.start(&self.state, &self.account)?;
            self.qq.request(Command::Read {
                after,
                limit,
                wait_ms,
            })
        } else if self
            .qq
            .worker
            .as_ref()
            .is_some_and(|w| w.alias == self.account && !w.handle.as_ref().unwrap().is_finished())
        {
            self.qq.request(Command::Read {
                after,
                limit,
                wait_ms: 0,
            })
        } else {
            serde_json::to_value(
                Inbox::open(&self.state, &self.scope(Platform::Qq))?.read(after, limit)?,
            )
            .map_err(|e| e.to_string())
        }
    }
    pub(super) fn qq_status(&self) -> Result<Value> {
        if let Some(pending) = self
            .qq
            .pending_password
            .as_ref()
            .filter(|p| p.alias == self.account)
        {
            return Ok(
                json!({"status":if self.qq_password_session_active(){"verification_required"}else{"verification_expired"},"same_connection_available":self.qq_password_session_active(),"expires_at":pending.continuation.expires_at(),"online_checked":false}),
            );
        }
        if self
            .qq
            .worker
            .as_ref()
            .is_some_and(|w| w.alias == self.account && !w.handle.as_ref().unwrap().is_finished())
        {
            return self.qq.request(Command::Status);
        }
        let path = super::auth::saved_session(&self.state, "qq", &self.account)?;
        let status = if let Some(path) = path {
            let session = platform_qq::session::Session::load(&path)?;
            let directory = path.parent().ok_or("QQ account directory missing")?;
            let state = if directory.join("login-required.json").exists() {
                "login_required"
            } else if directory.join("login-challenge.json").exists() {
                "verification_required"
            } else {
                "saved_session"
            };
            json!({"status":state,"identity":session.uin.to_string(),"same_connection_available":false,"online_checked":false})
        } else {
            json!({"status":"signed_out","online_checked":false})
        };
        Ok(status)
    }
    pub fn qq_login(&mut self) -> Result<Value> {
        self.qq.start(&self.state, &self.account)?;
        self.qq.request(Command::Status)
    }
    /// Operator-only native password login; FILE is never exposed as an AI tool.
    pub fn qq_password_login(&mut self, file: &Path) -> Result<Value> {
        self.qq_password_login_mode(file, false, false)
    }
    /// Continue only the live native Client/TCP retained by this Runtime.
    pub fn qq_password_resume(&mut self, file: &Path) -> Result<Value> {
        self.qq_password_login_mode(file, true, false)
    }
    /// Explicit cross-process fallback. The result never claims same-TCP resume.
    pub fn qq_password_resume_from_disk(&mut self, file: &Path) -> Result<Value> {
        self.qq_password_login_mode(file, true, true)
    }
    pub fn qq_password_session_active(&self) -> bool {
        self.qq.pending_password.as_ref().is_some_and(|pending| {
            pending.alias == self.account
                && Instant::now() < pending.deadline
                && platform_qq::unix_seconds()
                    .is_ok_and(|now| now < pending.continuation.expires_at())
        })
    }
    /// Close an expired operator socket and remove its unusable continuation.
    pub fn qq_password_expire(&mut self) -> Result<bool> {
        let now = platform_qq::unix_seconds()?;
        let expired = self.qq.pending_password.as_ref().is_some_and(|pending| {
            Instant::now() >= pending.deadline || now >= pending.continuation.expires_at()
        });
        if expired {
            self.qq_password_cancel()?;
        }
        Ok(expired)
    }
    /// Called on operator cancellation/EOF. Old registered credentials remain.
    pub fn qq_password_cancel(&mut self) -> Result<Value> {
        let pending = self.qq.pending_password.take();
        let closed = pending.is_some();
        let alias = pending
            .as_ref()
            .map(|p| p.alias.clone())
            .unwrap_or_else(|| self.account.clone());
        drop(pending);
        if alias != "public" {
            clear_password_login_state(&account_directory(&self.state, "qq", &alias)?)?;
        }
        Ok(
            json!({"status":"cancelled","same_connection_closed":closed,"saved_session_unchanged":true,"messages_sent":0}),
        )
    }
    fn qq_password_login_mode(
        &mut self,
        file: &Path,
        force_resume: bool,
        from_disk: bool,
    ) -> Result<Value> {
        if self.account == "public" {
            return Err("QQ password login requires an explicit account alias".into());
        }
        let expired = self.qq_password_expire()?;
        let mut input: PasswordInput = serde_json::from_slice(&read_private(file, 65536)?)
            .map_err(|_| "invalid private QQ password login input")?;
        input.resume |= force_resume;
        let directory = account_directory(&self.state, "qq", &self.account)?;
        let pending_path = directory.join("password-login.json");
        if !input.resume && input.captcha.is_some() {
            return Err("CAPTCHA requires a live password resume or explicit disk fallback".into());
        }
        let proof = input
            .captcha
            .as_ref()
            .map(|c| platform_qq::auth::CaptchaSubmit {
                ticket: &c.ticket,
                randstr: &c.randstr,
                session_id: &c.session_id,
            });
        let (mut client, pending, digest, continuity) = if input.resume && !from_disk {
            if expired {
                return Err("QQ password challenge expired; its TCP connection was closed".into());
            }
            let live = self.qq.pending_password.as_ref().ok_or("QQ same_connection_unavailable: this Runtime has no live password login; an explicit disk fallback reconnects instead")?;
            if live.alias != self.account
                || input
                    .uin
                    .is_some_and(|uin| uin != live.client.session().uin)
            {
                return Err("QQ password continuation belongs to another account identity".into());
            }
            if input
                .app_profile
                .as_deref()
                .map(str::parse::<platform_qq::AppProfile>)
                .transpose()?
                .is_some_and(|p| p != live.client.session().app_profile)
            {
                return Err(
                    "QQ password continuation belongs to another application profile".into(),
                );
            }
            if input.password.is_some() || input.password_md5.is_some() {
                if password_digest(&input)? != live.digest.0 {
                    return Err("QQ password continuation changed its private credential".into());
                }
            }
            check_login_binding(&directory, live.client.session().uin, None)?;
            live.continuation
                .check_session(live.client.session(), live.client.device())?;
            if let Some(proof) = &proof {
                live.continuation.check_captcha(proof)?;
            } else if live.continuation.kind
                == platform_qq::password::PasswordChallengeKind::Captcha
            {
                return Err("provide a CAPTCHA proof from the operator client".into());
            }
            let live = self.qq.pending_password.take().unwrap();
            (
                live.client,
                Some(live.continuation),
                live.digest,
                "same_connection",
            )
        } else {
            if from_disk && self.qq.pending_password.is_some() {
                return Err("a live QQ password connection exists; use same-connection resume or explicitly cancel it".into());
            }
            let pending = if from_disk {
                Some(
                    serde_json::from_slice::<platform_qq::password::PasswordContinuation>(
                        &read_private(&pending_path, 65536)?,
                    )
                    .map_err(|_| "invalid private QQ password continuation")?,
                )
            } else {
                None
            };
            let uin = input
                .uin
                .or_else(|| pending.as_ref().map(|p| p.uin()))
                .ok_or("new QQ password login requires an explicit UIN")?;
            check_login_binding(&directory, uin, None)?;
            // Only a live operator flow owns its credential in memory. An
            // explicit reconnect must receive it again, including NewDevice
            // flows that can return a further password/CAPTCHA challenge.
            let digest = PasswordDigest(password_digest(&input)?);
            let session_path = directory.join("session.json");
            let mut session = if session_path.exists() {
                platform_qq::Session::load(&session_path)?
            } else {
                platform_qq::Session::empty()
            };
            session.uin = uin;
            if let Some(profile) = input.app_profile.as_deref() {
                session.app_profile = profile.parse()?;
            } else if let Some(pending) = &pending {
                session.app_profile = pending.app_profile();
            }
            let device = platform_qq::Device::load_or_create(&directory.join("device.json"))?;
            if let Some(pending) = &pending {
                pending.check_session(&session, &device)?;
                if let Some(proof) = &proof {
                    pending.check_captcha(proof)?;
                } else if pending.kind == platform_qq::password::PasswordChallengeKind::Captcha {
                    return Err(
                        "explicit disk fallback needs a CAPTCHA proof and private credential"
                            .into(),
                    );
                }
            } else {
                self.qq_password_cancel()?;
            }
            self.qq.worker.take();
            let client = QqClient::connect(
                session,
                device,
                ClientConfig {
                    session_file: session_path,
                    device_file: directory.join("device.json"),
                    signer_module: signer_config(&self.state)?,
                },
            )?;
            (
                client,
                pending,
                digest,
                if from_disk {
                    "reconnected_from_disk"
                } else {
                    "new_connection"
                },
            )
        };
        let outcome = if from_disk {
            client.password_login_from_disk(
                &digest.0,
                proof.as_ref(),
                pending.ok_or("disk fallback continuation missing")?,
            )
        } else {
            client.password_login(&digest.0, proof.as_ref(), pending)
        };
        let outcome = match outcome {
            Ok(outcome) => outcome,
            Err(error) => {
                clear_password_login_state(&directory)?;
                return Err(error);
            }
        };
        let result = match outcome {
            platform_qq::password::PasswordOutcome::Online => {
                super::auth::bind_identity(&directory, &client.session().uin.to_string())?;
                clear_login_pause(&directory)?;
                clear_password_login_state(&directory)?;
                self.qq.retry_after = None;
                json!({"status":"online","scope":self.scope(Platform::Qq),"identity":client.session().uin.to_string(),"source":"password","app_profile":client.session().app_profile.info().version,"session_saved":true,"password_saved":false,"qr_started":false,"messages_sent":0})
            }
            platform_qq::password::PasswordOutcome::Rejected(failure) => {
                clear_password_login_state(&directory)?;
                json!({"status":"rejected","code":failure.code,"reason":format!("{:?}",failure.kind),"detail":failure.reason,"saved_session_unchanged":true,"qr_started":false,"messages_sent":0})
            }
            platform_qq::password::PasswordOutcome::VerificationRequired { continuation, code } => {
                let kind = match continuation.kind {
                    platform_qq::password::PasswordChallengeKind::Captcha => "captcha",
                    platform_qq::password::PasswordChallengeKind::NewDevice => "new_device",
                    platform_qq::password::PasswordChallengeKind::Unusual => "unusual_device",
                };
                let expires_at = continuation.expires_at();
                platform_qq::session::write_private_json(&pending_path, &continuation)?;
                platform_qq::session::write_private_json(
                    &directory.join("login-challenge.json"),
                    &json!({"schema":1,"flow":"password","code":code,"kind":kind}),
                )?;
                let deadline = Instant::now()
                    + Duration::from_secs(u64::from(
                        expires_at.saturating_sub(platform_qq::unix_seconds()?),
                    ));
                self.qq.pending_password = Some(PendingPasswordLogin {
                    alias: self.account.clone(),
                    client,
                    continuation,
                    digest,
                    deadline,
                });
                json!({"status":"verification_required","code":code,"kind":kind,"management_file":pending_path,"expires_at":expires_at,"same_connection_available":true,"saved_session_unchanged":true,"model_credentials_exposed":false,"messages_sent":0})
            }
        };
        let mut result = result;
        result["continuity"] = json!(continuity);
        Ok(result)
    }
    /// Import a dedicated Session+Device bundle, validate the actor and register
    /// the candidate before replacing an expired saved login. No source scanning.
    pub fn qq_import_login(&mut self, file: &Path) -> Result<Value> {
        if self.account == "public" {
            return Err("QQ login import requires an explicit account alias".into());
        }
        let bytes = read_private(file, 128 * 1024)?;
        let object: Value =
            serde_json::from_slice(&bytes).map_err(|_| "invalid private QQ login import JSON")?;
        let input: LoginImport = if object.get("session").is_some() {
            serde_json::from_value(object)
                .map_err(|_| "QQ login import needs a {session,device} bundle")?
        } else {
            LoginImport {
                session: serde_json::from_value(object)
                    .map_err(|_| "invalid Runtime Session login import")?,
                device: None,
            }
        };
        input.session.validate()?;
        if !input.session.has_tickets()? && input.session.temp_pwd()?.is_empty() {
            return Err("QQ login import has no usable tickets or TempPwd".into());
        }
        let directory = account_directory(&self.state, "qq", &self.account)?;
        check_login_binding(&directory, input.session.uin, Some(&input.session.uid))?;
        let device_path = directory.join("device.json");
        let existing = if device_path.exists() {
            Some(platform_qq::Device::load(&device_path)?)
        } else {
            None
        };
        let device = match (input.device, existing) {
            (Some(candidate), Some(saved)) => {
                if candidate.guid_bytes()? != saved.guid_bytes()? {
                    return Err(
                        "imported QQ login belongs to another device; use a new account alias"
                            .into(),
                    );
                }
                saved
            }
            (Some(candidate), None) => {
                candidate.guid_bytes()?;
                candidate
            }
            (None, Some(saved)) => saved,
            (None, None) => {
                return Err("new QQ alias requires the matching device in the login bundle".into());
            }
        };
        self.qq_password_cancel()?;
        let stage = WorkDir::new(&self.state)?;
        self.qq.worker.take();
        let mut client = QqClient::connect(
            input.session,
            device.clone(),
            ClientConfig {
                session_file: stage.path.join("session.json"),
                device_file: stage.path.join("device.json"),
                signer_module: signer_config(&self.state)?,
            },
        )?;
        let outcome = client.authenticate(LoginMode::Auto)?;
        match outcome {
            LoginOutcome::Online { source } => {
                let session = client.into_session();
                super::auth::bind_identity(&directory, &session.uin.to_string())?;
                if !device_path.exists() {
                    platform_qq::session::write_private_json(&device_path, &device)?;
                }
                session.save(&directory.join("session.json"))?;
                clear_login_pause(&directory)?;
                self.qq.retry_after = None;
                Ok(
                    json!({"status":"online","scope":self.scope(Platform::Qq),"source":format!("{source:?}"),"session_saved":true,"qr_started":false,"messages_sent":0}),
                )
            }
            LoginOutcome::VerificationRequired { challenge } => {
                let path = directory.join("import-login-challenge.json");
                platform_qq::session::write_private_json(
                    &path,
                    &json!({"code":challenge.code,"verify_url":challenge.verify_url,"cookies":challenge.cookies,"unusual_sig":challenge.unusual_sig}),
                )?;
                Ok(
                    json!({"status":"verification_required","code":challenge.code,"management_file":path,"saved_session_unchanged":true,"qr_started":false,"messages_sent":0}),
                )
            }
            LoginOutcome::Rejected { failure } => Ok(
                json!({"status":"rejected","code":failure.code,"saved_session_unchanged":true,"qr_started":false,"messages_sent":0}),
            ),
            LoginOutcome::CredentialsUnavailable => {
                Err("imported QQ login has no recoverable credentials".into())
            }
        }
    }

    /// Explicit human management command. Never called by account auto-recovery
    /// or registered with the model's task catalog.
    pub fn qq_qr(&mut self) -> Result<Value> {
        self.qq_qr_with_profile(None)
    }
    pub fn qq_qr_with_profile(&mut self, profile: Option<&str>) -> Result<Value> {
        let selected = profile
            .map(str::parse::<platform_qq::AppProfile>)
            .transpose()?;
        if self.account == "public" {
            return Err("QQ QR login requires an explicit account alias".into());
        }
        self.qq_password_cancel()?;
        self.qq.worker.take();
        let directory = account_directory(&self.state, "qq", &self.account)?;
        let session_path = directory.join("session.json");
        let existing = session_path.exists();
        let mut session = if existing {
            platform_qq::Session::load(&session_path)?
        } else {
            platform_qq::Session::empty()
        };
        if existing {
            if session.uin == 0 {
                return Err("saved QQ account identity is incomplete".into());
            }
            super::auth::bind_identity(&directory, &session.uin.to_string())?;
        } else if directory.join("identity.json").exists() {
            let binding: Value =
                serde_json::from_slice(&read_private(&directory.join("identity.json"), 1024)?)
                    .map_err(|_| "invalid QQ identity binding")?;
            session.uin = binding["identity"]
                .as_str()
                .ok_or("QQ identity binding missing")?
                .parse::<u32>()
                .map_err(|_| "invalid QQ identity binding")?;
            if session.uin == 0 {
                return Err("invalid QQ identity binding".into());
            }
        }
        if let Some(profile) = selected {
            session.app_profile = profile;
        }
        let device = if existing || directory.join("identity.json").exists() {
            platform_qq::Device::load(&directory.join("device.json"))?
        } else {
            platform_qq::Device::load_or_create(&directory.join("device.json"))?
        };
        let signer_module = if self.state.join("qq-config.json").exists() {
            let value: Value =
                serde_json::from_slice(&read_private(&self.state.join("qq-config.json"), 4096)?)
                    .map_err(|_| "invalid QQ signer config")?;
            value["signer_module"].as_str().map(PathBuf::from)
        } else {
            None
        };
        let mut client = QqClient::connect(
            session,
            device,
            ClientConfig {
                session_file: session_path,
                device_file: directory.join("device.json"),
                signer_module,
            },
        )?;
        let output = directory.join("qr-login");
        private_dir(&output)?;
        let stop = AtomicBool::new(false);
        let emit = |value: Value| {
            let mut stdout = std::io::stdout().lock();
            if serde_json::to_writer(&mut stdout, &value).is_err()
                || stdout.write_all(b"\n").is_err()
                || stdout.flush().is_err()
            {
                stop.store(true, Ordering::Relaxed);
            }
        };
        let outcome = platform_qq::login_qr(
            &mut client,
            &output,
            &stop,
            |path| emit(json!({"event":"qq_qr_ready","qr_path":path,"management_only":true})),
            |state| {
                let status = match state {
                    platform_qq::qr::QrState::Confirmed { .. } => "confirmed",
                    platform_qq::qr::QrState::WaitingForScan => "waiting_for_scan",
                    platform_qq::qr::QrState::WaitingForConfirm => "waiting_for_confirm",
                    platform_qq::qr::QrState::Expired => "expired",
                    platform_qq::qr::QrState::Canceled => "canceled",
                    platform_qq::qr::QrState::Unknown(_) => "unknown",
                };
                emit(json!({"event":"qq_qr_status","status":status}));
            },
        )?;
        if let LoginOutcome::Online { source } = outcome {
            super::auth::bind_identity(&directory, &client.session().uin.to_string())?;
            for name in ["login-challenge.json", "login-required.json"] {
                let path = directory.join(name);
                if path.exists() {
                    fs::remove_file(path).map_err(|e| e.to_string())?;
                }
            }
            self.qq.retry_after = None;
            return Ok(
                json!({"status":"online","scope":self.scope(Platform::Qq),"auth_source":format!("{source:?}"),"session_saved":true,"chat_messages_sent":0}),
            );
        }
        Err("QQ QR login did not return an authenticated session".into())
    }
    pub(super) fn task_qq_chats(&mut self, params: &Value) -> Result<Value> {
        if params
            .get("platform")
            .is_some_and(|v| v.as_str() != Some("qq"))
        {
            return Err("chat provider must be qq".into());
        }
        if params
            .get("kind")
            .and_then(Value::as_str)
            .is_some_and(|k| k != "observed")
            || params
                .get("continuation")
                .and_then(Value::as_str)
                .is_some_and(|id| self.locators.scope_for(DIRECTORY_KIND, id).is_ok())
        {
            return self.qq_directory(params);
        }
        let refresh = boolean(params, "refresh", true)?;
        let limit = number(params, "limit", 20)?;
        if !(1..=20).contains(&limit) {
            return Err("chat limit must be 1..20".into());
        }
        if let Some(id) = params.get("continuation") {
            let id = id.as_str().ok_or("continuation must be a reference")?;
            let scope = self.locators.scope_for(CHATS_KIND, id)?;
            tasks::check_account(params, &scope)?;
            if scope.platform != Platform::Qq {
                return Err("chat continuation scope mismatch".into());
            }
            let mut cursor = self.locators.get::<ChatsCursor>(&scope, CHATS_KIND, id)?;
            cursor.connection_requested = false;
            return self.with_task_scope(&scope, |rt| rt.qq_chats_page(cursor, limit as usize));
        }
        self.with_platform_account("qq",params,true,|rt| {
            let page=rt.qq_page(refresh,None,512,0)?;
            let dir=account_directory(&rt.state,"qq",&rt.account)?;
            let own=platform_qq::session::Session::load(&dir.join("session.json"))?.uin;
            let own_target=ChatTarget{actor:own,key:chat_key(&SendTarget::SelfAccount)?,destination:Some(SendTarget::SelfAccount)};
            let mut chats=BTreeMap::<String,(u64,ChatTarget,Value)>::new();
            chats.insert(own_target.key.clone(),(u64::MAX,own_target,json!({"kind":"self_account","label":"current account"})));
            for item in page["items"].as_array().ok_or("QQ inbox page has no items")? {
                let event=&item["event"];
                let target:ChatTarget=serde_json::from_value(event["conversation"].clone()).map_err(|_|"invalid saved QQ conversation")?;
                let summary=json!({"last_message_time":event["server_time"],"last_message_sequence":item["sequence"],"kind":target.destination.as_ref().map(|d|match d{SendTarget::SelfAccount=>"self_account",SendTarget::Private{..}=>"private",SendTarget::Group{..}=>"group"}).unwrap_or("unresolved"),"group_code":event["group_code"],"peer_uin":if event["direction"]=="outgoing"{&event["to_uin"]}else{&event["from_uin"]}});
                if target.destination!=Some(SendTarget::SelfAccount){chats.insert(target.key.clone(),(item["sequence"].as_u64().unwrap_or(0),target,summary));}
            }
            let mut ordered=chats.into_values().collect::<Vec<_>>();ordered.sort_by_key(|v|std::cmp::Reverse(v.0));
            let cursor=ChatsCursor{entries:ordered.into_iter().map(|(_,target,summary)|(target,summary)).collect(),offset:0,retention:page["retention"].clone(),connection_requested:refresh};
            rt.qq_chats_page(cursor,limit as usize)
        })
    }
    fn qq_chats_page(&self, mut cursor: ChatsCursor, limit: usize) -> Result<Value> {
        if cursor.entries.len() > 513 || cursor.offset > cursor.entries.len() {
            return Err("invalid saved QQ conversation page".into());
        }
        let total = cursor.entries.len();
        let end = (cursor.offset + limit).min(total);
        let mut output = Vec::new();
        for (target, summary) in &cursor.entries[cursor.offset..end] {
            let mut item = summary.clone();
            item["target_ref"] = json!(self.locators.put_reusable(
                &self.scope(Platform::Qq),
                CHAT_KIND,
                target
            )?);
            item["can_send_text"] = json!(target.destination.is_some());
            item["history_available"] = json!("runtime_observed_only");
            output.push(item);
        }
        cursor.offset = end;
        let continuation = if end < total {
            Some(
                self.locators
                    .put_reusable(&self.scope(Platform::Qq), CHATS_KIND, &cursor)?,
            )
        } else {
            None
        };
        Ok(
            json!({"status":"ok","scope":self.scope(Platform::Qq),"chats":output,"observed_total":total,"has_more":end<total,"continuation":continuation,"full_contacts_available":false,"retention":cursor.retention,"connection_requested":cursor.connection_requested,"snapshot":true}),
        )
    }
    pub(super) fn task_qq_messages(&mut self, params: &Value) -> Result<Value> {
        if params.get("history").and_then(Value::as_bool) == Some(true)
            || params
                .get("continuation")
                .and_then(Value::as_str)
                .is_some_and(|r| self.locators.scope_for(HISTORY_KIND, r).is_ok())
        {
            return self.qq_history(params);
        }
        if params.get("target_ref").is_some() && params.get("continuation").is_some() {
            return Err("provide target_ref or continuation, not both".into());
        }
        if params
            .get("platform")
            .is_some_and(|p| p.as_str() != Some("qq"))
        {
            return Err("message provider must be qq".into());
        }
        let refresh = boolean(params, "refresh", true)?;
        let latest = match params.get("start") {
            None => false,
            Some(Value::String(value)) if value == "earliest" => false,
            Some(Value::String(value)) if value == "latest" => true,
            _ => return Err("message start must be earliest or latest".into()),
        };
        if params.get("start").is_some() && params.get("continuation").is_some() {
            return Err("start applies only to a fresh message cursor".into());
        }
        let limit = number(params, "limit", 20)?;
        let wait_ms = number(params, "wait_ms", 0)?;
        if !(1..=20).contains(&limit) || wait_ms > 10000 || (!refresh && wait_ms > 0) {
            return Err("invalid message page or wait; cached-only reads cannot wait".into());
        }
        if latest && wait_ms != 0 {
            return Err(
                "start=latest establishes a baseline; wait on the returned continuation instead"
                    .into(),
            );
        }
        let (scope, cursor) = if let Some(id) = params.get("continuation") {
            let id = id.as_str().ok_or("continuation must be a reference")?;
            let scope = self.locators.scope_for(CURSOR_KIND, id)?;
            let cursor = self.locators.get::<Cursor>(&scope, CURSOR_KIND, id)?;
            (scope, cursor)
        } else if let Some(id) = params.get("target_ref") {
            let id = id.as_str().ok_or("target_ref must be a reference")?;
            let scope = self.locators.scope_for(CHAT_KIND, id)?;
            let chat = self.locators.get::<ChatTarget>(&scope, CHAT_KIND, id)?;
            (
                scope,
                Cursor {
                    after: 0,
                    chat: Some(chat),
                },
            )
        } else {
            return self.with_platform_account("qq", params, true, |rt| {
                rt.qq_messages_begin(
                    &Cursor {
                        after: 0,
                        chat: None,
                    },
                    refresh,
                    limit as usize,
                    wait_ms,
                    latest,
                )
            });
        };
        tasks::check_account(params, &scope)?;
        if scope.platform != Platform::Qq {
            return Err("message reference is not a QQ account".into());
        }
        self.with_task_scope(&scope, |rt| {
            rt.qq_messages_begin(&cursor, refresh, limit as usize, wait_ms, latest)
        })
    }
    fn qq_messages_begin(
        &mut self,
        cursor: &Cursor,
        refresh: bool,
        limit: usize,
        wait_ms: u64,
        latest: bool,
    ) -> Result<Value> {
        if !latest {
            return self.qq_messages_page(cursor, refresh, limit, wait_ms);
        }
        let page = self.qq_page(refresh, None, 1, 0)?;
        let newest = page["retention"]["newest_sequence"]
            .as_u64()
            .ok_or("QQ retention cursor missing")?;
        let reference = self.locators.put_reusable(
            &self.scope(Platform::Qq),
            CURSOR_KIND,
            &Cursor {
                after: newest,
                chat: cursor.chat.clone(),
            },
        )?;
        Ok(
            json!({"status":"ok","scope":self.scope(Platform::Qq),"messages":[],"continuation":reference,"has_more":false,"gap":null,"retention":page["retention"],"history":"runtime_observed_only","connection_requested":refresh,"marked_read":false,"seeded_latest":true,"baseline_sequence":newest}),
        )
    }
    fn qq_messages_page(
        &mut self,
        cursor: &Cursor,
        refresh: bool,
        limit: usize,
        wait_ms: u64,
    ) -> Result<Value> {
        let page = self.qq_page(refresh, Some(cursor.after), 512, wait_ms)?;
        let mut after = cursor.after;
        let mut items = Vec::new();
        let mut has_more = false;
        for entry in page["items"]
            .as_array()
            .ok_or("QQ inbox page has no items")?
        {
            let sequence = entry["sequence"]
                .as_u64()
                .ok_or("QQ inbox sequence missing")?;
            let mut event = entry["event"].clone();
            let target: ChatTarget = serde_json::from_value(event["conversation"].clone())
                .map_err(|_| "invalid saved QQ conversation")?;
            if cursor
                .chat
                .as_ref()
                .is_some_and(|wanted| wanted.key != target.key)
            {
                after = sequence;
                continue;
            }
            if items.len() >= limit {
                has_more = true;
                break;
            }
            after = sequence;
            let object = event.as_object_mut().ok_or("QQ event object missing")?;
            object.remove("conversation");
            self.qq_project_rich(&self.scope(Platform::Qq), &target, &mut event)?;
            if target.destination.is_some() {
                event["conversation_key"] = json!(crate::agent_mind::stable_key(&[
                    b"qq-peer-v1",
                    &target.actor.to_le_bytes(),
                    target.key.as_bytes()
                ]));
            }
            event["local_sequence"] = json!(sequence);
            event["target_ref"] = json!(self.locators.put_reusable(
                &self.scope(Platform::Qq),
                CHAT_KIND,
                &target
            )?);
            event["can_reply_text"] = json!(target.destination.is_some());
            if let Some(source) = event
                .get("source_resource")
                .and_then(Value::as_str)
                .map(str::to_owned)
            {
                event["source_ref"] = json!(self.locators.put_reusable(
                    &self.scope(Platform::Qq),
                    SOURCE_KIND,
                    &json!({"resource":source})
                )?);
            }
            event.as_object_mut().unwrap().remove("source_resource");
            items.push(event);
        }
        let next = self.locators.put_reusable(
            &self.scope(Platform::Qq),
            CURSOR_KIND,
            &Cursor {
                after,
                chat: cursor.chat.clone(),
            },
        )?;
        Ok(
            json!({"status":"ok","scope":self.scope(Platform::Qq),"messages":items,"continuation":next,"has_more":has_more||page["has_more"]==true,"gap":page["gap"],"retention":page["retention"],"history":"runtime_observed_only","connection_requested":refresh,"marked_read":false}),
        )
    }
    fn qq_project_rich(&self, scope: &Scope, chat: &ChatTarget, event: &mut Value) -> Result<()> {
        if let (Some(seq), Some(uin), Some(uid), Some(time)) = (
            event["message_sequence"]
                .as_u64()
                .and_then(|v| u32::try_from(v).ok()),
            event["from_uin"]
                .as_u64()
                .and_then(|v| u32::try_from(v).ok()),
            event["from_uid"].as_str(),
            event["server_time"]
                .as_u64()
                .and_then(|v| u32::try_from(v).ok()),
        ) {
            if seq > 0 && uin > 0 && time > 0 && valid_uid(uid) {
                event["message_ref"] = json!(
                    self.locators.put_reusable(
                        scope,
                        MESSAGE_KIND,
                        &MessageTarget {
                            chat: chat.clone(),
                            sequence: seq,
                            sender_uin: uin,
                            sender_uid: uid.into(),
                            timestamp: time,
                            text: event["text"]
                                .as_str()
                                .unwrap_or("")
                                .chars()
                                .take(512)
                                .collect()
                        }
                    )?
                );
            }
        }
        let segments = event.get("segments").cloned().unwrap_or(json!([]));
        let mut output = Vec::new();
        for mut segment in segments.as_array().into_iter().flatten().cloned() {
            if let Some(locator) = segment.as_object_mut().and_then(|o| o.remove("locator")) {
                let reference = self.locators.put_reusable(
                    scope,
                    MEDIA_KIND,
                    &json!({"chat":chat,"locator":locator}),
                )?;
                segment["media_ref"] = json!(reference);
            }
            if segment["type"] == "mention" {
                if let Some(uid) = segment["uid"].as_str().filter(|u| valid_uid(u)) {
                    let member = MemberTarget {
                        actor: chat.actor,
                        group: match chat.destination {
                            Some(SendTarget::Group { code }) => Some(code),
                            _ => None,
                        },
                        uid: uid.into(),
                        uin: 0,
                        display: segment["display"]
                            .as_str()
                            .unwrap_or("")
                            .trim_start_matches('@')
                            .into(),
                    };
                    segment["target_ref"] =
                        json!(self.locators.put_reusable(scope, MEMBER_KIND, &member)?);
                }
                segment.as_object_mut().unwrap().remove("uid");
            }
            output.push(segment);
        }
        event["segments"] = json!(output);
        event.as_object_mut().unwrap().remove("from_uid");
        event.as_object_mut().unwrap().remove("to_uid");
        Ok(())
    }
    fn qq_directory(&mut self, params: &Value) -> Result<Value> {
        let limit = number(params, "limit", 20)? as usize;
        if !(1..=20).contains(&limit) {
            return Err("QQ directory limit must be1..20".into());
        }
        if let Some(reference) = params.get("continuation").and_then(Value::as_str) {
            let scope = self.locators.scope_for(DIRECTORY_KIND, reference)?;
            tasks::check_account(params, &scope)?;
            let cursor = self
                .locators
                .get::<DirectoryCursor>(&scope, DIRECTORY_KIND, reference)?;
            return self.with_task_scope(&scope, |rt| rt.qq_directory_page(cursor, limit));
        }
        let kind = params
            .get("kind")
            .and_then(Value::as_str)
            .unwrap_or("friends");
        if !["friends", "groups", "members"].contains(&kind) {
            return Err("invalid QQ directory kind".into());
        }
        let group = if kind == "members" {
            let reference = required_str(params, "target_ref")?;
            let scope = self.locators.scope_for(CHAT_KIND, reference)?;
            tasks::check_account(params, &scope)?;
            if scope.platform != Platform::Qq {
                return Err("QQ member target mismatch".into());
            }
            Some((
                scope,
                self.locators.get::<ChatTarget>(
                    &self.locators.scope_for(CHAT_KIND, reference)?,
                    CHAT_KIND,
                    reference,
                )?,
            ))
        } else {
            None
        };
        let invoke = |rt: &mut Self| -> Result<Value> {
            let target = group.as_ref().map(|(_, t)| t.clone());
            let code = target.as_ref().and_then(|g| match g.destination {
                Some(SendTarget::Group { code }) => Some(code),
                _ => None,
            });
            if kind == "members" && code.is_none() {
                return Err("members requires a QQ group target".into());
            }
            rt.qq.start(&rt.state, &rt.account)?;
            let page = rt.qq.request(Command::Directory {
                kind: kind.into(),
                group: code,
                token: None,
            })?;
            let items = page["items"]
                .as_array()
                .ok_or("invalid nativeQQ directory page")?
                .clone();
            let cursor = DirectoryCursor {
                kind: kind.into(),
                group: target,
                items,
                offset: 0,
                next: page["next"].as_str().map(str::to_owned),
            };
            rt.qq_directory_page(cursor, limit)
        };
        if let Some((scope, _)) = &group {
            self.with_task_scope(scope, invoke)
        } else {
            self.with_platform_account("qq", params, true, invoke)
        }
    }
    fn qq_directory_page(&mut self, mut cursor: DirectoryCursor, limit: usize) -> Result<Value> {
        let scope = self.scope(Platform::Qq);
        let session = platform_qq::Session::load(
            &account_directory(&self.state, "qq", &self.account)?.join("session.json"),
        )?;
        let actor = session.uin;
        if cursor.offset == cursor.items.len() {
            if let Some(token) = cursor.next.take() {
                self.qq.start(&self.state, &self.account)?;
                let page = self.qq.request(Command::Directory {
                    kind: cursor.kind.clone(),
                    group: cursor.group.as_ref().and_then(|g| match g.destination {
                        Some(SendTarget::Group { code }) => Some(code),
                        _ => None,
                    }),
                    token: Some(token.clone()),
                })?;
                cursor.items = page["items"]
                    .as_array()
                    .ok_or("QQ directory page missingitems")?
                    .clone();
                cursor.offset = 0;
                cursor.next = page["next"]
                    .as_str()
                    .filter(|next| *next != token)
                    .map(str::to_owned);
            }
        }
        if cursor.items.len() > 2048 || cursor.offset > cursor.items.len() {
            return Err("QQ directory snapshot exceedsbound".into());
        }
        let end = (cursor.offset + limit).min(cursor.items.len());
        let mut items = Vec::new();
        for raw in &cursor.items[cursor.offset..end] {
            let mut row = raw.clone();
            if cursor.kind == "groups" {
                let code = u32::try_from(raw["code"].as_u64().ok_or("QQ group code missing")?)
                    .map_err(|_| "QQ groupcode overflow")?;
                let destination = SendTarget::Group { code };
                let target = ChatTarget {
                    actor,
                    key: chat_key(&destination)?,
                    destination: Some(destination),
                };
                row["target_ref"] = json!(self.locators.put_reusable(&scope, CHAT_KIND, &target)?);
            } else {
                let uid = required_str(raw, "uid")?;
                let uin = u32::try_from(raw["uin"].as_u64().ok_or("QQ contact UIN missing")?)
                    .map_err(|_| "QQ UIN overflow")?;
                let display = raw["card"]
                    .as_str()
                    .filter(|s| !s.is_empty())
                    .or_else(|| raw["remark"].as_str().filter(|s| !s.is_empty()))
                    .or_else(|| raw["nickname"].as_str())
                    .unwrap_or("QQ member");
                let group = cursor.group.as_ref().and_then(|g| match g.destination {
                    Some(SendTarget::Group { code }) => Some(code),
                    _ => None,
                });
                row["member_ref"] = json!(self.locators.put_reusable(
                    &scope,
                    MEMBER_KIND,
                    &MemberTarget {
                        actor,
                        group,
                        uid: uid.into(),
                        uin,
                        display: display.into()
                    }
                )?);
                let destination = if uin == actor && uid == session.uid {
                    SendTarget::SelfAccount
                } else {
                    SendTarget::Private { uid: uid.into() }
                };
                row["target_ref"] = json!(self.locators.put_reusable(
                    &scope,
                    CHAT_KIND,
                    &ChatTarget {
                        actor,
                        key: chat_key(&destination)?,
                        destination: Some(destination)
                    }
                )?);
                row.as_object_mut().unwrap().remove("uid");
            }
            items.push(row)
        }
        cursor.offset = end;
        let more = end < cursor.items.len() || cursor.next.is_some();
        let next = if more {
            Some(
                self.locators
                    .put_reusable(&scope, DIRECTORY_KIND, &cursor)?,
            )
        } else {
            None
        };
        Ok(
            json!({"status":"ok","scope":scope,"kind":cursor.kind,"items":items,"continuation":next,"has_more":more,"source":"qq_native_directory","full_contacts_available":true}),
        )
    }
    fn qq_history(&mut self, params: &Value) -> Result<Value> {
        let limit =
            u32::try_from(number(params, "limit", 20)?).map_err(|_| "history limit overflow")?;
        if !(1..=20).contains(&limit) {
            return Err("historylimitmustbe1..20".into());
        }
        let (scope, target, before, start, end) =
            if let Some(reference) = params.get("continuation").and_then(Value::as_str) {
                let scope = self.locators.scope_for(HISTORY_KIND, reference)?;
                tasks::check_account(params, &scope)?;
                let saved: Value = self.locators.get(&scope, HISTORY_KIND, reference)?;
                (
                    scope,
                    serde_json::from_value(saved["target"].clone())
                        .map_err(|_| "QQ history cursor invalid")?,
                    u32::try_from(number(&saved, "before", 0)?)
                        .map_err(|_| "history cursor timestamp overflow")?,
                    u32::try_from(number(&saved, "start", 0)?)
                        .map_err(|_| "history cursor start overflow")?,
                    u32::try_from(number(&saved, "end", 0)?)
                        .map_err(|_| "history cursor end overflow")?,
                )
            } else {
                let reference = required_str(params, "target_ref")?;
                let scope = self.locators.scope_for(CHAT_KIND, reference)?;
                tasks::check_account(params, &scope)?;
                let target = self
                    .locators
                    .get::<ChatTarget>(&scope, CHAT_KIND, reference)?;
                (
                    scope,
                    target,
                    u32::try_from(number(
                        params,
                        "before_time",
                        platform_qq::unix_seconds()?.into(),
                    )?)
                    .map_err(|_| "historytimestampoverflow")?,
                    u32::try_from(number(params, "start_sequence", 0)?)
                        .map_err(|_| "historystartoverflow")?,
                    u32::try_from(number(params, "end_sequence", 0)?)
                        .map_err(|_| "historyendoverflow")?,
                )
            };
        if scope.platform != Platform::Qq {
            return Err("QQ history scope mismatch".into());
        }
        self.with_task_scope(&scope, |rt| {
            rt.qq.start(&rt.state,&rt.account)?;
            let page=rt.qq.request(Command::History{target:target.clone(),before,start,end,count:limit})?;
            let mut messages=Vec::new();
            for raw in page["items"].as_array().ok_or("QQ history page missing")? {
                let mut event=raw["event"].clone();
                event.as_object_mut().ok_or("QQ history event missing")?.remove("conversation");
                rt.qq_project_rich(&scope,&target,&mut event)?;
                event["target_ref"]=json!(rt.locators.put_reusable(&scope,CHAT_KIND,&target)?);
                event["can_reply_text"]=json!(target.destination.is_some());
                if let Some(source)=event["source_resource"].as_str(){event["source_ref"]=json!(rt.locators.put_reusable(&scope,SOURCE_KIND,&json!({"resource":source}))?)}
                event.as_object_mut().unwrap().remove("source_resource");
                messages.push(event);
            }
            let next=if let Some(t)=page["next_time"].as_u64(){Some(json!({"target":target,"before":t,"start":0,"end":0}))}else{page["next_sequence"].as_u64().map(|n|json!({"target":target,"before":0,"end":n,"start":n.saturating_sub(u64::from(limit)-1).max(1)}))};
            let continuation=next.map(|n|rt.locators.put_reusable(&scope,HISTORY_KIND,&n)).transpose()?;
            Ok(json!({"status":"ok","scope":scope,"messages":messages,"continuation":continuation,"has_more":continuation.is_some(),"history":"server_history","marked_read":false}))
        })
    }
    pub(super) fn qq_media_download(&mut self, params: &Value) -> Result<Value> {
        let reference = required_str(params, "reference")?;
        let scope = self.locators.scope_for(MEDIA_KIND, reference)?;
        tasks::check_account(params, &scope)?;
        if scope.platform != Platform::Qq {
            return Err("QQ media scope mismatch".into());
        }
        let saved: Value = self.locators.get(&scope, MEDIA_KIND, reference)?;
        let locator: platform_qq::rich::MediaLocator =
            serde_json::from_value(saved["locator"].clone())
                .map_err(|_| "QQ media locator invalid")?;
        let is_file = locator.kind == platform_qq::rich::MediaKind::File;
        self.with_task_scope(&scope,|rt|{let url=if let Some(url)=&locator.url{platform_qq::media::validate_media_url(url)?;url.clone()}else{rt.qq.start(&rt.state,&rt.account)?;let v=rt.qq.request(Command::Media{locator})?;required_str(&v,"private_url")?.to_owned()};let resource=rt.store.ingest(&scope,"qq-received-media","application/octet-stream",|out|if is_file{platform_qq::media::download_file_to_writer(&url,out,Budget::default().max_resource_bytes)}else{platform_qq::media::download_to_writer(&url,out,Budget::default().max_resource_bytes)}.map(|_|()))?;Ok(json!({"status":"ok","scope":scope,"resource":resource,"media_ref":reference,"downloaded":true}))})
    }
    fn qq_resolve_send_resource(
        &mut self,
        scope: &Scope,
        reference: &str,
    ) -> Result<(ResourceRef, PathBuf)> {
        let source = self.store.scope_for(reference).or_else(|_| {
            [
                MEDIA_KIND,
                "bilibili-media",
                "bilibili-comment-media",
                "bilibili-dynamic-media",
                "tieba-media",
            ]
            .iter()
            .find_map(|kind| self.locators.scope_for(kind, reference).ok())
            .ok_or_else(|| "QQ attachment reference is unavailable".to_owned())
        })?;
        if source.account != scope.account {
            return Err("QQ attachment belongs to another account alias".into());
        }
        let (source, resource, path) = self.materialize_send_media(reference)?;
        if source.account != scope.account {
            return Err("QQ attachment belongs to another account alias".into());
        }
        Ok((resource, path))
    }
    fn qq_prepare_voice(&mut self, scope: &Scope, reference: &str) -> Result<AttachmentInput> {
        let (_, original) = self.qq_resolve_send_resource(scope, reference)?;
        if let Ok(info) = platform_qq::attachments::inspect_voice(&original) {
            return Ok(AttachmentInput::Voice {
                resource: reference.into(),
                path: original,
                info,
            });
        }
        let mut random = [0; 16];
        platform_qq::tea::random(&mut random)?;
        let directory = std::env::temp_dir().join(format!(
            "qq-voice-{}",
            random
                .iter()
                .map(|n| format!("{n:02x}"))
                .collect::<String>()
        ));
        runtime_core::resources::private_dir(&directory)?;
        struct Cleanup(PathBuf);
        impl Drop for Cleanup {
            fn drop(&mut self) {
                let _ = fs::remove_dir_all(&self.0);
            }
        }
        let _cleanup = Cleanup(directory.clone());
        let output = directory.join("voice.amr");
        runtime_media::audio::encode_qq_voice(
            &original,
            &output,
            &runtime_media::Limits::default(),
        )
        .map_err(|e| e.to_string())?;
        let info = platform_qq::attachments::inspect_voice(&output)?;
        let cached = self.store.import(
            scope,
            "qq-voice-encoded",
            "audio/amr",
            &mut open_regular(&output, platform_qq::upload::MAX_IMAGE_BYTES)?,
        )?;
        let (_, path) = self.store.resolve(scope, &cached.id)?;
        Ok(AttachmentInput::Voice {
            resource: reference.into(),
            path,
            info,
        })
    }
    fn qq_prepare_video(
        &mut self,
        scope: &Scope,
        reference: &str,
        thumbnail: Option<&str>,
    ) -> Result<AttachmentInput> {
        let (_, path) = self.qq_resolve_send_resource(scope, reference)?;
        let blob = platform_qq::attachments::inspect_blob(&path)?;
        let limits = runtime_media::Limits::default();
        let probe = runtime_media::video::probe_video(&path, &limits).map_err(|e| e.to_string())?;
        if probe.duration_ms == 0 || probe.duration_ms > 3_600_000 {
            return Err("QQ video duration must be 1 ms..60 min".into());
        }
        if !probe.tracks.iter().any(|track| !track.audio) {
            return Err("QQ video resource has no video track".into());
        }
        let mut random = [0; 16];
        platform_qq::tea::random(&mut random)?;
        let directory = std::env::temp_dir().join(format!(
            "qq-video-{}",
            random
                .iter()
                .map(|n| format!("{n:02x}"))
                .collect::<String>()
        ));
        runtime_core::resources::private_dir(&directory)?;
        struct Cleanup(PathBuf);
        impl Drop for Cleanup {
            fn drop(&mut self) {
                let _ = fs::remove_dir_all(&self.0);
            }
        }
        let _cleanup = Cleanup(directory.clone());
        let thumb = if let Some(id) = thumbnail {
            let (_, input) = self.qq_resolve_send_resource(scope, id)?;
            let output = directory.join("thumbnail.jpg");
            runtime_media::pictures::prepare_image(
                &input,
                &output,
                640,
                runtime_media::pictures::OutputFormat::Jpeg,
                &limits,
            )
            .map_err(|e| e.to_string())?;
            output
        } else {
            let sample = runtime_media::video::sample_video_window(
                &path,
                &directory,
                0,
                probe.duration_ms.min(1000),
                1000,
                640,
                &limits,
            )
            .map_err(|e| e.to_string())?;
            sample
                .samples
                .first()
                .ok_or("QQ video has no decodable first frame")?
                .path
                .clone()
        };
        let info = platform_qq::upload::inspect_image(&thumb)?;
        let cached = self.store.import(
            scope,
            "qq-video-thumbnail",
            "image/jpeg",
            &mut open_regular(&thumb, platform_qq::upload::MAX_IMAGE_BYTES)?,
        )?;
        let (_, thumbnail_path) = self.store.resolve(scope, &cached.id)?;
        Ok(AttachmentInput::Video {
            resource: reference.into(),
            path,
            thumbnail_path,
            thumbnail_source: thumbnail.unwrap_or("auto_first_frame_v1").into(),
            info: platform_qq::attachments::VideoInfo {
                blob,
                duration_seconds: u32::try_from(probe.duration_ms.div_ceil(1000))
                    .map_err(|_| "QQ duration overflow")?,
                thumbnail: info,
            },
        })
    }
    pub(super) fn task_qq_send(&mut self, params: &Value) -> Result<Value> {
        let reference = required_str(params, "target_ref")?;
        let scope = self.locators.scope_for(CHAT_KIND, reference)?;
        tasks::check_account(params, &scope)?;
        if scope.platform != Platform::Qq {
            return Err("QQ chat scope mismatch".into());
        }
        let target: ChatTarget = self.locators.get(&scope, CHAT_KIND, reference)?;
        let mut content = platform_qq::rich::Message::plain(
            params.get("message").and_then(Value::as_str).unwrap_or(""),
        );
        if let Some(segments) = params.get("segments") {
            let segments = segments
                .as_array()
                .filter(|s| s.len() <= 64)
                .ok_or("QQ segments must be an array of at most64")?;
            for segment in segments {
                match segment["type"].as_str() {
                    Some("text") => content.parts.push(platform_qq::rich::Part::Text {
                        text: required_str(segment, "text")?.into(),
                    }),
                    Some("face") => content.parts.push(platform_qq::rich::Part::Face {
                        id: u32::try_from(segment["id"].as_u64().ok_or("face id missing")?)
                            .map_err(|_| "face id overflow")?,
                    }),
                    Some("mention") => {
                        let member: MemberTarget = self.locators.get(
                            &scope,
                            MEMBER_KIND,
                            required_str(segment, "target_ref")?,
                        )?;
                        if member.actor != target.actor
                            || member.group
                                != match target.destination {
                                    Some(SendTarget::Group { code }) => Some(code),
                                    _ => None,
                                }
                        {
                            return Err("mention belongs to another chat/account".into());
                        }
                        content.parts.push(platform_qq::rich::Part::Mention {
                            uid: member.uid,
                            display: format!("@{}", member.display),
                            all: false,
                        });
                    }
                    _ => return Err("QQ segment supports text, mention and face".into()),
                }
            }
        }
        if let Some(reference) = params.get("reply_ref").and_then(Value::as_str) {
            let quote: MessageTarget = self.locators.get(&scope, MESSAGE_KIND, reference)?;
            if quote.chat != target {
                return Err("quoted message belongs to another QQ conversation".into());
            }
            content.quote = Some(platform_qq::rich::Quote {
                sequence: quote.sequence,
                sender_uin: quote.sender_uin,
                sender_uid: quote.sender_uid,
                timestamp: quote.timestamp,
                text: quote.text,
            });
        }
        let count = ["image_ref", "voice_ref", "video_ref", "file_ref"]
            .iter()
            .filter(|key| params.get(**key).is_some())
            .count();
        if count > 1 {
            return Err("QQ send accepts one attachment per message".into());
        }
        let attachment = if let Some(reference) = params.get("voice_ref").and_then(Value::as_str) {
            Some(self.qq_prepare_voice(&scope, reference)?)
        } else if let Some(reference) = params.get("file_ref").and_then(Value::as_str) {
            if !content.text.is_empty() || !content.parts.is_empty() || content.quote.is_some() {
                return Err(
                    "QQ files use a separate native file message; omit message/segments/reply_ref"
                        .into(),
                );
            }
            let (_, path) = self.qq_resolve_send_resource(&scope, reference)?;
            let name = params
                .get("filename")
                .and_then(Value::as_str)
                .filter(|s| !s.is_empty())
                .unwrap_or("attachment.bin")
                .to_owned();
            platform_qq::attachments::validate_name(&name)?;
            let info = platform_qq::attachments::inspect_blob(&path)?;
            Some(AttachmentInput::File {
                resource: reference.into(),
                path,
                info,
                name,
            })
        } else if let Some(reference) = params.get("video_ref").and_then(Value::as_str) {
            Some(self.qq_prepare_video(
                &scope,
                reference,
                params.get("thumbnail_ref").and_then(Value::as_str),
            )?)
        } else {
            None
        };
        let image = params
            .get("image_ref")
            .map(|value| -> Result<ImageInput> {
                let reference = value
                    .as_str()
                    .ok_or("image_ref must be a media reference")?;
                let (_, path) = self.qq_resolve_send_resource(&scope, reference)?;
                let info = platform_qq::upload::inspect_image(&path)?;
                Ok(ImageInput {
                    resource: reference.into(),
                    path,
                    info,
                })
            })
            .transpose()?;
        let mut validation = content.clone();
        if (image.is_some() || attachment.is_some()) && validation.text.is_empty() {
            validation.text = "[附件]".into()
        }
        validation.validate()?;
        if rich_summary(&content).chars().count() > 1000 {
            return Err("QQ message exceeds1000 characters".into());
        }
        let id = required_str(params, "operation_id")?;
        self.with_task_scope(&scope, |rt| {
            let directory = account_directory(&rt.state, "qq", &rt.account)?;
            check_legacy(&directory, id)?;
            let outbox = Outbox::open(&rt.state)?;
            if outbox.exists(id)? {
                if let Some(input) = &attachment {
                    return replay_attachment(&outbox, &scope, &target, &content, input, id);
                }
                return if let Some(image) = &image {
                    replay_image(&outbox, &scope, &target, &content, image, id)
                } else {
                    replay_rich(&outbox, &scope, &target, &content, id)
                };
            }
            if target.destination.is_none() {
                return Err("QQ destination lacks a verified UID/group code".into());
            }
            rt.qq.start(&rt.state, &rt.account)?;
            if let Some(attachment) = attachment {
                return rt.qq.request(Command::SendAttachment {
                    target,
                    message: content,
                    attachment,
                    operation_id: id.into(),
                });
            }
            rt.qq.request(Command::Send {
                target,
                message: content,
                image,
                operation_id: id.into(),
            })
        })
    }
    pub(super) fn expand_qq_source(&mut self, params: &Value) -> Result<Value> {
        let reference = required_str(params, "reference")?;
        let scope = self.locators.scope_for(SOURCE_KIND, reference)?;
        tasks::check_account(params, &scope)?;
        if scope.platform != Platform::Qq {
            return Err("QQ source scope mismatch".into());
        }
        let source: Value = self.locators.get(&scope, SOURCE_KIND, reference)?;
        let store = ResourceStore::open(
            &account_directory(&self.state, "qq", &scope.account)?.join("sources"),
            source_budget(),
        )?;
        let id = source["resource"]
            .as_str()
            .ok_or("QQ source reference incomplete")?;
        let (_, path) = store
            .resolve(&scope, id)
            .map_err(|_| "QQ original message was evicted or is unavailable")?;
        self.with_task_scope(&scope,|rt|{
            let mode=params.get("mode").and_then(Value::as_str).unwrap_or("auto");
            if !matches!(mode,"auto"|"raw"|"json"){return Err("QQ original supports auto/json projection or raw binary expansion".into())}
            let resource=if mode=="raw"{
                rt.store.import(&scope,"qq-message-original","application/x-protobuf",&mut open_regular(&path,source_budget().max_resource_bytes)?)?
            }else{
                let bytes=read_bounded(&path,source_budget().max_resource_bytes)?;
                let projection=wire_projection(&bytes,0,&mut 0);
                let json=serde_json::to_vec(&projection).map_err(|e|e.to_string())?;
                rt.store.import(&scope,"qq-message-projection","application/json",&mut json.as_slice())?
            };
            let mut request=params.clone();request["resource"]=json!(resource.id);
            if mode=="raw"&&request.get("encoding").is_none(){request["encoding"]=json!("base64")}
            let content=rt.call(if mode=="raw"{"resource.read"}else{"content.json"},&request)?;
            Ok(json!({"status":"ok","scope":scope,"reference":reference,"resource":resource,"content":content,"scripts_executed":false,"semantic_fields_complete":false}))
        })
    }
}
#[cfg(test)]
#[path = "qq_tests.rs"]
mod tests;
