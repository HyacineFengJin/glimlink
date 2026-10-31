// SPDX-License-Identifier: MIT
//! Offline route, shutdown and raw-message projection regressions. No QQ/TCP.
use super::*;
use platform_qq::View;

const OWN: u32 = 12345;
const OWN_UID: &str = "u_own_fixture";
const PEER: u64 = 67890;
const PEER_UID: &str = "u_peer_fixture";

fn view() -> View {
    View {
        segments: Vec::new(),
        kind: 166,
        chat: true,
        sequence: Some(9),
        timestamp: Some(1_700_000_000),
        message_id: Some(17),
        new_id: None,
        from_uin: Some(PEER),
        to_uin: Some(u64::from(OWN)),
        group_uin: None,
        from_uid: Some(PEER_UID.into()),
        to_uid: Some(OWN_UID.into()),
        text: "fixture".into(),
        content: "fixture".into(),
        text_bytes: 7,
        truncated: false,
        unexpanded: false,
    }
}
struct RichLab(PathBuf);
impl RichLab {
    fn new() -> Self {
        let mut random = [0; 8];
        platform_qq::tea::random(&mut random).unwrap();
        Self(std::env::temp_dir().join(
            format!("qq-rich-host-{}",random.iter().map(|v|format!("{v:02x}")).collect::<String>()),
        ))
    }
    fn scope() -> Scope {
        Scope {
            platform: Platform::Qq,
            account: "fixture".into(),
        }
    }
}
impl Drop for RichLab {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}
fn group_target(code: u32) -> ChatTarget {
    let target = SendTarget::Group { code };
    ChatTarget {
        actor: OWN,
        key: chat_key(&target).unwrap(),
        destination: Some(target),
    }
}
#[test]
fn received_media_mentions_and_quote_are_account_scoped_opaque_references() {
    let lab = RichLab::new();
    let rt = Runtime::open(&lab.0, "public", None).unwrap();
    let scope = RichLab::scope();
    let target = group_target(111);
    let locator = platform_qq::rich::MediaLocator {
        kind: platform_qq::rich::MediaKind::Image,
        node: None,
        url: Some("https://gchat.qpic.cn/private-token-fixture".into()),
        file_uuid: None,
        file_hash: None,
        group: Some(111),
    };
    let mut event = json!({"message_sequence":9,"from_uin":PEER,"from_uid":PEER_UID,"to_uid":OWN_UID,"server_time":1700000000u32,"text":"hello","segments":[{"type":"mention","uid":PEER_UID,"display":"@fixture","all":false},{"type":"media","kind":"image","name":"animation.gif","locator":locator}]});
    rt.qq_project_rich(&scope, &target, &mut event).unwrap();
    let public = event.to_string();
    assert!(!public.contains(PEER_UID));
    assert!(!public.contains(OWN_UID));
    assert!(!public.contains("qpic.cn"));
    assert!(!public.contains("private-token"));
    assert!(event["message_ref"].is_string());
    let media = event["segments"][1]["media_ref"].as_str().unwrap();
    let saved: Value = rt.locators.get(&scope, MEDIA_KIND, media).unwrap();
    assert_eq!(
        saved["locator"]["url"],
        "https://gchat.qpic.cn/private-token-fixture"
    );
    let foreign = Scope {
        platform: Platform::Qq,
        account: "other".into(),
    };
    assert!(
        rt.locators
            .get::<Value>(&foreign, MEDIA_KIND, media)
            .is_err()
    );
    let quote: MessageTarget = rt
        .locators
        .get(&scope, MESSAGE_KIND, event["message_ref"].as_str().unwrap())
        .unwrap();
    assert_eq!(quote.chat, target);
    assert_eq!(quote.sender_uid, PEER_UID);
}
#[test]
fn rich_send_unknown_is_reused_and_cannot_be_changed_or_retargeted() {
    let lab = RichLab::new();
    let outbox = Outbox::open(&lab.0).unwrap();
    let scope = RichLab::scope();
    let target = group_target(111);
    let content = platform_qq::rich::Message {
        parts: vec![platform_qq::rich::Part::Face { id: 14 }],
        ..Default::default()
    };
    let mut record = Operation::new(
        "rich-once",
        scope.clone(),
        OWN.to_string(),
        serde_json::to_value(&target).unwrap(),
        "[QQ表情:14]",
    )
    .unwrap();
    record.request = Some(json!({"action":"qq_rich","rich_content":content}));
    outbox.insert(&record).unwrap();
    outbox.begin_attempt(&mut record).unwrap();
    let result = replay_rich(&outbox, &scope, &target, &content, "rich-once").unwrap();
    assert_eq!(result["status"], "unknown");
    assert_eq!(result["automatic_resend"], false);
    assert!(replay_rich(&outbox, &scope, &group_target(222), &content, "rich-once").is_err());
    let mut changed = content;
    changed.parts = vec![platform_qq::rich::Part::Face { id: 15 }];
    assert!(replay_rich(&outbox, &scope, &target, &changed, "rich-once").is_err());
}
#[test]
fn native_attachment_replay_freezes_bytes_filename_and_target() {
    let lab = RichLab::new();
    let outbox = Outbox::open(&lab.0).unwrap();
    let scope = RichLab::scope();
    let target = group_target(111);
    let content = platform_qq::rich::Message::default();
    let input = AttachmentInput::File {
        resource: "0123456789abcdef0123456789abcdef".into(),
        path: "/never-opened".into(),
        info: platform_qq::attachments::BlobInfo {
            bytes: 3,
            md5: "900150983cd24fb0d6963f7d28e17f72".into(),
            sha1: "a9993e364706816aba3e25717850c26c9cd0d89d".into(),
            md5_first_10m: "900150983cd24fb0d6963f7d28e17f72".into(),
        },
        name: "notes.txt".into(),
    };
    let mut record = Operation::new(
        "attachment-once",
        scope.clone(),
        OWN.to_string(),
        serde_json::to_value(&target).unwrap(),
        "[文件]",
    )
    .unwrap();
    record.request =
        Some(json!({"action":"qq_attachment","rich_content":content,"attachment":input.frozen()}));
    outbox.insert(&record).unwrap();
    outbox.begin_attempt(&mut record).unwrap();
    let result = replay_attachment(
        &outbox,
        &scope,
        &target,
        &content,
        &input,
        "attachment-once",
    )
    .unwrap();
    assert_eq!(result["status"], "unknown");
    let mut altered = input.clone();
    if let AttachmentInput::File { name, .. } = &mut altered {
        *name = "other.txt".into()
    }
    assert!(
        replay_attachment(
            &outbox,
            &scope,
            &target,
            &content,
            &altered,
            "attachment-once"
        )
        .is_err()
    );
    assert!(
        replay_attachment(
            &outbox,
            &scope,
            &group_target(222),
            &content,
            &input,
            "attachment-once"
        )
        .is_err()
    );
}
#[test]
fn foreign_chat_reply_fails_before_loading_any_login_state() {
    let lab = RichLab::new();
    let mut rt = Runtime::open(&lab.0, "public", None).unwrap();
    let scope = RichLab::scope();
    let chat = rt
        .locators
        .put(&scope, CHAT_KIND, &group_target(111))
        .unwrap();
    let quote = rt
        .locators
        .put(
            &scope,
            MESSAGE_KIND,
            &MessageTarget {
                chat: group_target(222),
                sequence: 1,
                sender_uin: OWN,
                sender_uid: OWN_UID.into(),
                timestamp: 1700000000,
                text: "fixture".into(),
            },
        )
        .unwrap();
    let error=rt.task_qq_send(&json!({"target_ref":chat,"reply_ref":quote,"operation_id":"foreign-quote","message":"hello"})).unwrap_err();
    assert!(error.contains("another QQ conversation"));
    assert!(
        !account_directory(&lab.0, "qq", "fixture")
            .unwrap()
            .join("session.json")
            .exists()
    );
}
#[test]
fn ordinary_wav_voice_is_encoded_to_native_amr_without_ai_media_steps() {
    let lab = RichLab::new();
    let mut rt = Runtime::open(&lab.0, "public", None).unwrap();
    let scope = RichLab::scope();
    let samples = 1600u32;
    let size = samples * 2;
    let mut wav = Vec::new();
    wav.extend_from_slice(b"RIFF");
    wav.extend_from_slice(&(36 + size).to_le_bytes());
    wav.extend_from_slice(b"WAVEfmt ");
    wav.extend_from_slice(&16u32.to_le_bytes());
    wav.extend_from_slice(&1u16.to_le_bytes());
    wav.extend_from_slice(&1u16.to_le_bytes());
    wav.extend_from_slice(&16000u32.to_le_bytes());
    wav.extend_from_slice(&32000u32.to_le_bytes());
    wav.extend_from_slice(&2u16.to_le_bytes());
    wav.extend_from_slice(&16u16.to_le_bytes());
    wav.extend_from_slice(b"data");
    wav.extend_from_slice(&size.to_le_bytes());
    wav.resize(44 + size as usize, 0);
    let input = rt
        .store
        .import(&scope, "fixture-voice", "audio/wav", &mut wav.as_slice())
        .unwrap();
    let native = rt.qq_prepare_voice(&scope, &input.id).unwrap();
    let AttachmentInput::Voice {
        resource,
        path,
        info,
    } = native
    else {
        panic!("voice input required")
    };
    assert_eq!(resource, input.id);
    assert_eq!(info.codec, "amr_nb");
    assert_eq!(info.duration_seconds, 1);
    assert!(std::fs::read(path).unwrap().starts_with(b"#!AMR\n"));
}
#[test]
fn send_media_resolver_reuses_same_alias_cross_platform_and_rejects_other_alias_before_fetch() {
    let lab = RichLab::new();
    let mut rt = Runtime::open(&lab.0, "public", None).unwrap();
    let target = RichLab::scope();
    let source = Scope {
        platform: Platform::Bilibili,
        account: target.account.clone(),
    };
    let resource = rt
        .store
        .import(
            &source,
            "cross-platform-fixture",
            "image/png",
            &mut b"synthetic-image-bytes".as_slice(),
        )
        .unwrap();
    let (actual, path) = rt.qq_resolve_send_resource(&target, &resource.id).unwrap();
    assert_eq!(actual, resource);
    assert_eq!(fs::read(path).unwrap(), b"synthetic-image-bytes");
    let foreign = Scope {
        platform: Platform::Qq,
        account: "other".into(),
    };
    let lazy = rt
        .locators
        .put(
            &foreign,
            MEDIA_KIND,
            &json!({"locator":{"kind":"image","url":"https://gchat.qpic.cn/never-fetch"}}),
        )
        .unwrap();
    let error = rt.qq_resolve_send_resource(&target, &lazy).unwrap_err();
    assert!(error.contains("another account alias"));
}
fn destination(view: &View) -> Option<SendTarget> {
    reply_destination(view, OWN, OWN_UID)
}
fn peer_target() -> Option<SendTarget> {
    Some(SendTarget::Private {
        uid: PEER_UID.into(),
    })
}

#[test]
fn private_incoming_and_outgoing_route_to_the_actual_peer_uid() {
    let incoming = view();
    assert_eq!(destination(&incoming), peer_target());
    let mut outgoing = incoming;
    std::mem::swap(&mut outgoing.from_uin, &mut outgoing.to_uin);
    std::mem::swap(&mut outgoing.from_uid, &mut outgoing.to_uid);
    assert_eq!(destination(&outgoing), peer_target());
}

#[test]
fn self_messages_bind_to_the_current_account() {
    let mut own = view();
    own.from_uin = Some(u64::from(OWN));
    own.from_uid = Some(OWN_UID.into());
    assert_eq!(destination(&own), Some(SendTarget::SelfAccount));
    // A documented UIN is sufficient to identify self; it is never converted
    // into a private UID string when UID fields are absent.
    own.from_uid = None;
    own.to_uid = None;
    assert_eq!(destination(&own), Some(SendTarget::SelfAccount));
}

#[test]
fn missing_peer_uid_and_unknown_or_foreign_recipients_stay_unresolved() {
    let mut missing_uid = view();
    missing_uid.from_uid = None;
    assert_eq!(destination(&missing_uid), None);

    let mut foreign = view();
    foreign.to_uin = Some(33333);
    foreign.to_uid = Some("u_other_fixture".into());
    assert_eq!(destination(&foreign), None);

    let mut unknown_recipient = view();
    unknown_recipient.to_uin = None;
    unknown_recipient.to_uid = None;
    assert_eq!(destination(&unknown_recipient), None);

    let mut outgoing_without_uid = view();
    outgoing_without_uid.from_uin = Some(u64::from(OWN));
    outgoing_without_uid.from_uid = Some(OWN_UID.into());
    outgoing_without_uid.to_uin = Some(PEER);
    outgoing_without_uid.to_uid = None;
    assert_eq!(destination(&outgoing_without_uid), None);
}

#[test]
fn numeric_and_uid_claims_about_self_must_agree_on_both_endpoints() {
    let mut variants = Vec::new();
    let mut sender_qq_claims_self = view();
    sender_qq_claims_self.from_uin = Some(u64::from(OWN));
    variants.push(sender_qq_claims_self);
    let mut sender_uid_claims_self = view();
    sender_uid_claims_self.from_uid = Some(OWN_UID.into());
    variants.push(sender_uid_claims_self);
    let mut recipient_uid_foreign = view();
    recipient_uid_foreign.to_uid = Some("u_other_fixture".into());
    variants.push(recipient_uid_foreign);
    let mut recipient_qq_foreign = view();
    recipient_qq_foreign.to_uin = Some(33333);
    variants.push(recipient_qq_foreign);
    for contradictory in variants {
        assert_eq!(destination(&contradictory), None);
    }
}

#[test]
fn explicit_uid_only_endpoints_work_without_guessing_numeric_identity() {
    let mut by_uid = view();
    by_uid.from_uin = None;
    by_uid.to_uin = None;
    assert_eq!(destination(&by_uid), peer_target());
    by_uid.to_uid = Some("u_other_fixture".into());
    assert_eq!(destination(&by_uid), None);
}

#[test]
fn group_routing_requires_chat_type_82_and_a_nonzero_u32_code() {
    let mut group = view();
    group.kind = 82;
    group.group_uin = Some(777);
    assert_eq!(destination(&group), Some(SendTarget::Group { code: 777 }));
    group.group_uin = Some(0);
    assert_eq!(destination(&group), None);
    group.group_uin = Some(u64::from(u32::MAX) + 1);
    assert_eq!(destination(&group), None);
    group.group_uin = Some(777);
    group.kind = 166;
    assert_eq!(destination(&group), None);
    group.kind = 82;
    group.chat = false;
    assert_eq!(destination(&group), None);
}

#[test]
fn full_command_queue_does_not_prevent_worker_drop_from_stopping_and_closing() {
    let (tx, rx) = mpsc::sync_channel(1);
    let (response, _) = mpsc::sync_channel(1);
    tx.try_send(Request {
        command: Command::Status,
        response,
    })
    .unwrap_or_else(|_| panic!("synthetic queue should accept exactly one command"));
    let (response, _) = mpsc::sync_channel(1);
    assert!(matches!(
        tx.try_send(Request {
            command: Command::Status,
            response,
        }),
        Err(mpsc::TrySendError::Full(_))
    ));
    let stop = Arc::new(AtomicBool::new(false));
    let thread_stop = Arc::clone(&stop);
    let (finished_tx, finished_rx) = mpsc::sync_channel(1);
    let handle = thread::spawn(move || {
        let deadline = Instant::now() + Duration::from_secs(1);
        while !thread_stop.load(Ordering::Relaxed) && Instant::now() < deadline {
            thread::yield_now();
        }
        let stopped = thread_stop.load(Ordering::Relaxed);
        // Drain only synthetic commands; no command implementation, Client or
        // socket exists. A timeout bounds even a broken sender-drop regression.
        let mut queued_status_seen = false;
        let disconnected = loop {
            match rx.recv_timeout(Duration::from_millis(500)) {
                Ok(Request {
                    command: Command::Status,
                    ..
                }) => queued_status_seen = true,
                Ok(_) => {}
                Err(mpsc::RecvTimeoutError::Disconnected) => break true,
                Err(mpsc::RecvTimeoutError::Timeout) => break false,
            }
        };
        let _ = finished_tx.send((stopped, queued_status_seen, disconnected));
    });
    let worker = Worker {
        alias: "fixture-only".into(),
        tx: Some(tx),
        stop,
        handle: Some(handle),
    };
    drop(worker);
    let result = finished_rx
        .recv_timeout(Duration::from_secs(2))
        .expect("synthetic worker must finish without a QQ connection");
    assert_eq!(result, (true, true, true));
}

#[test]
fn raw_wire_projection_bounds_nodes_text_and_binary_prefixes() {
    use platform_qq::wire;
    let mut raw = Vec::new();
    for value in 0..300 {
        wire::number(1, value, &mut raw);
    }
    let mut nodes = 0;
    let projected = wire_projection(&raw, 0, &mut nodes);
    assert_eq!(nodes, 256);
    assert_eq!(projected["fields"].as_array().unwrap().len(), 256);
    assert_eq!(projected["truncated"], true);

    let mut raw = Vec::new();
    wire::bytes(1, "喵".repeat(1025).as_bytes(), &mut raw);
    let projected = wire_projection(&raw, 0, &mut 0);
    assert_eq!(
        projected["fields"][0]["text"]
            .as_str()
            .unwrap()
            .chars()
            .count(),
        1024
    );
    assert_eq!(projected["fields"][0]["truncated"], true);

    let mut raw = Vec::new();
    wire::bytes(1, &[255; 300], &mut raw);
    let projected = wire_projection(&raw, 0, &mut 0);
    assert!(
        projected["fields"][0]["base64_prefix"]
            .as_str()
            .unwrap()
            .len()
            <= 344
    );
    assert_eq!(projected["fields"][0]["truncated"], true);
    assert_eq!(projected["semantic_fields_complete"], false);
}

#[test]
fn password_management_rejects_ambiguous_credentials_and_other_actors_without_connecting() {
    let lab = RichLab::new();
    let directory = account_directory(&lab.0, "qq", "fixture").unwrap();
    let mut session = platform_qq::Session::empty();
    session.uin = OWN;
    session.uid = OWN_UID.into();
    session
        .set_credentials(b"tgt", b"d2", &[7; 16], b"temp", Some(OWN_UID))
        .unwrap();
    session.save(&directory.join("session.json")).unwrap();
    platform_qq::session::write_private_json(&directory.join("device.json"),&json!({"guid":"000102030405060708090a0b0c0d0e0f","device_name":"synthetic-device","system_kernel":"Linux"})).unwrap();
    let original = fs::read(directory.join("session.json")).unwrap();
    let input = lab.0.join("password-input.json");
    let mut runtime = Runtime::open(&lab.0, "fixture", None).unwrap();
    for data in [
        json!({"uin":OWN,"password":"one","password_md5":"11111111111111111111111111111111"}),
        json!({"uin":OWN+1,"password":"synthetic"}),
        json!({"uin":OWN,"password":"synthetic","app_profile":"guess-latest"}),
        json!({"uin":OWN,"password":"synthetic","captcha":{"ticket":"t","randstr":"r","aid":"a"}}),
    ] {
        platform_qq::session::write_private_json(&input, &data).unwrap();
        assert!(runtime.qq_password_login(&input).is_err());
        assert!(runtime.qq.worker.is_none());
        assert_eq!(fs::read(directory.join("session.json")).unwrap(), original);
    }
    platform_qq::session::write_private_json(&input, &json!({"uin":OWN,"password":"synthetic"}))
        .unwrap();
    fs::set_permissions(&input, fs::Permissions::from_mode(0o644)).unwrap();
    assert!(
        runtime
            .qq_password_login(&input)
            .unwrap_err()
            .contains("0600")
    );
}
#[test]
fn imported_candidate_cannot_replace_an_existing_actor_or_device_before_validation() {
    let lab = RichLab::new();
    let directory = account_directory(&lab.0, "qq", "fixture").unwrap();
    let mut session = platform_qq::Session::empty();
    session.uin = OWN;
    session.uid = OWN_UID.into();
    session
        .set_credentials(b"tgt", b"d2", &[7; 16], b"temp", Some(OWN_UID))
        .unwrap();
    session.save(&directory.join("session.json")).unwrap();
    let device = json!({"guid":"000102030405060708090a0b0c0d0e0f","device_name":"synthetic-device","system_kernel":"Linux"});
    platform_qq::session::write_private_json(&directory.join("device.json"), &device).unwrap();
    let original = fs::read(directory.join("session.json")).unwrap();
    let input = lab.0.join("import-input.json");
    let mut runtime = Runtime::open(&lab.0, "fixture", None).unwrap();
    for (uin, guid) in [
        (OWN + 1, "000102030405060708090a0b0c0d0e0f"),
        (OWN, "01010101010101010101010101010101"),
    ] {
        let mut changed = session.clone();
        changed.uin = uin;
        let mut candidate = device.clone();
        candidate["guid"] = json!(guid);
        platform_qq::session::write_private_json(
            &input,
            &json!({"session":changed,"device":candidate}),
        )
        .unwrap();
        assert!(runtime.qq_import_login(&input).is_err());
        assert!(runtime.qq.worker.is_none());
        assert_eq!(fs::read(directory.join("session.json")).unwrap(), original);
    }
}

fn pending_password_lab(lab: &RichLab) -> (Runtime, std::net::TcpStream, PathBuf, Vec<u8>) {
    let mut runtime = Runtime::open(&lab.0, "fixture", None).unwrap();
    let directory = account_directory(&lab.0, "qq", "fixture").unwrap();
    let mut session = platform_qq::Session::empty();
    session.uin = OWN;
    session.uid = OWN_UID.into();
    session
        .set_credentials(b"old-tgt", b"old-d2", &[7; 16], b"old-temp", None)
        .unwrap();
    session.save(&directory.join("session.json")).unwrap();
    let old = fs::read(directory.join("session.json")).unwrap();
    let device:platform_qq::Device=serde_json::from_value(json!({"guid":"000102030405060708090a0b0c0d0e0f","device_name":"fixture","system_kernel":"Linux"})).unwrap();
    platform_qq::session::write_private_json(&directory.join("device.json"), &device).unwrap();
    let listener = std::net::TcpListener::bind((std::net::Ipv4Addr::LOCALHOST, 0)).unwrap();
    let stream = std::net::TcpStream::connect(listener.local_addr().unwrap()).unwrap();
    let (peer, _) = listener.accept().unwrap();
    peer.set_read_timeout(Some(Duration::from_millis(20)))
        .unwrap();
    let client = QqClient::from_tcp_stream(
        session,
        device,
        ClientConfig {
            session_file: directory.join("session.json"),
            device_file: directory.join("device.json"),
            signer_module: None,
        },
        stream,
    )
    .unwrap();
    let continuation:platform_qq::password::PasswordContinuation=serde_json::from_value(json!({"version":1,"uin":OWN,"uid":OWN_UID,"guid":(0..16).collect::<Vec<u8>>(),"profile":"linux31939038","expires_at":platform_qq::unix_seconds().unwrap()+300,"exchange_key":vec![1;16],"key_sig":vec![2;32],"exchange_expiration":300,"cookies":"synthetic-cookie","kind":"captcha","verify_url":"https://ti.qq.com/safe/tools/captcha/sms-verify-login?aid=2086582797&login_appid=1600001615&sid=synthetic%2Bsession&uin=12345","new_device":null})).unwrap();
    platform_qq::session::write_private_json(&directory.join("password-login.json"), &continuation)
        .unwrap();
    platform_qq::session::write_private_json(
        &directory.join("login-challenge.json"),
        &json!({"flow":"password"}),
    )
    .unwrap();
    runtime.qq.pending_password = Some(PendingPasswordLogin {
        alias: "fixture".into(),
        client,
        continuation,
        digest: PasswordDigest(platform_qq::auth::password_md5(b"synthetic")),
        deadline: Instant::now() + Duration::from_secs(300),
    });
    (runtime, peer, directory, old)
}
#[test]
fn password_cancel_closes_the_retained_tcp_and_cleans_only_pending_state() {
    use std::io::Read;
    let lab = RichLab::new();
    let (mut runtime, mut peer, directory, old) = pending_password_lab(&lab);
    assert!(runtime.qq_password_session_active());
    assert!(peer.read(&mut [0u8; 1]).is_err());
    assert_eq!(
        runtime.qq_password_cancel().unwrap()["same_connection_closed"],
        true
    );
    assert_eq!(peer.read(&mut [0u8; 1]).unwrap(), 0);
    assert!(!runtime.qq_password_session_active());
    assert!(!directory.join("password-login.json").exists());
    assert!(!directory.join("login-challenge.json").exists());
    assert_eq!(fs::read(directory.join("session.json")).unwrap(), old);
}
#[test]
fn password_expiry_closes_the_retained_tcp_without_replacing_saved_login() {
    use std::io::Read;
    let lab = RichLab::new();
    let (mut runtime, mut peer, directory, old) = pending_password_lab(&lab);
    assert!(!runtime.qq_password_expire().unwrap());
    runtime.qq.pending_password.as_mut().unwrap().deadline = Instant::now();
    assert!(runtime.qq_password_expire().unwrap());
    assert!(!runtime.qq_password_expire().unwrap());
    assert_eq!(peer.read(&mut [0u8; 1]).unwrap(), 0);
    assert_eq!(fs::read(directory.join("session.json")).unwrap(), old);
}
#[test]
fn proof_only_resume_is_bound_to_the_live_actor_and_retains_it_after_input_errors() {
    use std::io::Read;
    let lab = RichLab::new();
    let (mut runtime, mut peer, directory, old) = pending_password_lab(&lab);
    let input = lab.0.join("proof.json");
    // No uin or password is needed to identify the active operator connection.
    platform_qq::session::write_private_json(
        &input,
        &json!({"captcha":{"ticket":"synthetic","randstr":"synthetic","session_id":"wrong"}}),
    )
    .unwrap();
    assert!(
        runtime
            .qq_password_resume(&input)
            .unwrap_err()
            .contains("original challenge sid")
    );
    assert!(runtime.qq_password_session_active());
    assert!(
        runtime
            .qq_password_resume_from_disk(&input)
            .unwrap_err()
            .contains("live QQ password connection")
    );
    platform_qq::session::write_private_json(&input,&json!({"uin":OWN+1,"captcha":{"ticket":"synthetic","randstr":"synthetic","session_id":"synthetic%2Bsession"}})).unwrap();
    assert!(
        runtime
            .qq_password_resume(&input)
            .unwrap_err()
            .contains("another account")
    );
    assert!(peer.read(&mut [0u8; 1]).is_err());
    assert_eq!(fs::read(directory.join("session.json")).unwrap(), old);
    runtime.qq_password_cancel().unwrap();
}
#[test]
fn disk_continuation_never_silently_reconnects_for_default_resume() {
    let lab = RichLab::new();
    let (mut runtime, _peer, directory, _old) = pending_password_lab(&lab);
    drop(runtime.qq.pending_password.take());
    assert!(directory.join("password-login.json").exists());
    let input = lab.0.join("proof.json");
    platform_qq::session::write_private_json(&input,&json!({"captcha":{"ticket":"synthetic","randstr":"synthetic","session_id":"synthetic%2Bsession"}})).unwrap();
    assert!(
        runtime
            .qq_password_resume(&input)
            .unwrap_err()
            .contains("same_connection_unavailable")
    );
    assert!(directory.join("password-login.json").exists());
}
