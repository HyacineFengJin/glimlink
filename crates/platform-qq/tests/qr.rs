// SPDX-License-Identifier: AGPL-3.0-only
//! Offline mock server + independently generated Go TLV golden values.
//! Golden values were generated with pinned LagrangeGo's exported tlv builders,
//! not by the Rust implementation. The mock server uses a synthetic P-256 key.
use md5::{Digest, Md5};
use p256::{PublicKey, SecretKey, ecdh::diffie_hellman, elliptic_curve::sec1::ToEncodedPoint};
use platform_qq::{
    qr::{self, QrLogin, QrState},
    session::Device,
    tea, wire,
    wire::Reader,
};

const FETCH: &str = "0000000000000000000000000700160043000000005f5e164f2006c296000102030405060708090a0b0c0d0e0f000e636f6d2e74656e63656e742e71710005322e302e30000e636f6d2e74656e63656e742e7171001b001e000000000000000000000003000000040000004800000002000000020000001d000a0100007ffc000000000000330010000102030405060708090a0b0c0d0e0f0035000400000013006600040000001300d1001f0a190a054c696e7578121073796e7468657469632d6465766963652202300103";
const PROTECTED: &str = "0004016e001073796e7468657469632d6465766963650147001b5f5e164f0005322e302e30000e636f6d2e74656e63656e742e71710128002400000001000000000000054c696e75780010000102030405060708090a0b0c0d0e0f00000124000c000000000000000000000000";
const LOGIN_WITHOUT_T144: &str = "0009000e0106000e73796e7468657469632d743130360116000a0000b7fffc0000000000014200120000000e636f6d2e74656e63656e742e717101450010000102030405060708090a0b0c0d0e0f00180016000000000005000000000000987e00bc614e000000000141000f00000007556e6b6e6f776e0000000001770017010000000000106e742e77746c6f67696e2e302e302e310191000100010000160000000000055f5e164f2006c29600001f410a1e10e00107000600010d00000103180000016a000e73796e7468657469632d7431366101660001050521000d0000001300076261736963696d";
const SYNTHETIC_UIN: u32 = 12_345_678;

fn hex(s: &str) -> Vec<u8> {
    (0..s.len())
        .step_by(2)
        .map(|i| u8::from_str_radix(&s[i..i + 2], 16).unwrap())
        .collect()
}
fn device() -> Device {
    serde_json::from_value(serde_json::json!({"guid":"000102030405060708090a0b0c0d0e0f", "device_name":"synthetic-device", "system_kernel":"Linux", "kernel_version":"synthetic-kernel"})).unwrap()
}
fn server_secret() -> SecretKey {
    let mut bytes = [0; 32];
    bytes[31] = 1;
    SecretKey::from_slice(&bytes).unwrap()
}
fn flow() -> QrLogin {
    QrLogin::with_server_key(
        &device(),
        9,
        server_secret()
            .public_key()
            .to_encoded_point(false)
            .as_bytes(),
    )
    .unwrap()
}
fn short(out: &mut Vec<u8>, bytes: &[u8]) {
    out.extend_from_slice(&(bytes.len() as u16).to_be_bytes());
    out.extend_from_slice(bytes);
}
fn read16(r: &mut Reader<'_>) -> u16 {
    u16::from_be_bytes(r.take(2).unwrap().try_into().unwrap())
}
fn readshort<'a>(r: &mut Reader<'a>) -> &'a [u8] {
    let n = read16(r) as usize;
    r.take(n).unwrap()
}
fn tlvs(values: &[(u16, &[u8])]) -> Vec<u8> {
    let mut out = (values.len() as u16).to_be_bytes().to_vec();
    for (tag, bytes) in values {
        out.extend_from_slice(&tag.to_be_bytes());
        short(&mut out, bytes);
    }
    out
}

fn inspect_request(packet: &[u8]) -> (u16, u32, Vec<u8>, [u8; 16]) {
    let mut r = Reader::new(packet);
    assert_eq!(r.byte().unwrap(), 2);
    assert_eq!(read16(&mut r) as usize, packet.len());
    assert_eq!(read16(&mut r), 8001);
    let command = read16(&mut r);
    assert_eq!(read16(&mut r), 1);
    let uin = r.u32().unwrap();
    assert_eq!(r.take(3).unwrap(), &[3, 0x87, 0]);
    assert_eq!(r.u32().unwrap(), 2);
    assert_eq!(r.take(8).unwrap(), &[0; 8]);
    assert_eq!(r.take(2).unwrap(), &[2, 1]);
    r.take(16).unwrap();
    assert_eq!(read16(&mut r), 0x131);
    assert_eq!(read16(&mut r), 9);
    let client = PublicKey::from_sec1_bytes(readshort(&mut r)).unwrap();
    let shared = diffie_hellman(server_secret().to_nonzero_scalar(), client.as_affine());
    let key: [u8; 16] = Md5::digest(&shared.raw_secret_bytes()[..16]).into();
    let tail = r.rest();
    assert_eq!(tail.last(), Some(&3));
    (
        command,
        uin,
        tea::decrypt(&tail[..tail.len() - 1], &key).unwrap(),
        key,
    )
}

fn response(command: u16, payload: &[u8], key: &[u8; 16]) -> Vec<u8> {
    let mut out = vec![2, 0, 0];
    out.extend_from_slice(&8001_u16.to_be_bytes());
    out.extend_from_slice(&command.to_be_bytes());
    out.extend_from_slice(&1_u16.to_be_bytes());
    out.extend_from_slice(&SYNTHETIC_UIN.to_be_bytes());
    out.extend_from_slice(&[3, 0, 0]);
    out.extend(tea::encrypt(payload, key).unwrap());
    out.push(3);
    let length = out.len() as u16;
    out[1..3].copy_from_slice(&length.to_be_bytes());
    out
}

fn fetch_response(key: &[u8; 16]) -> Vec<u8> {
    let mut out = vec![0; 55];
    short(&mut out, b"synthetic-qrsig");
    let mut url = vec![0];
    url.push(b"https://example.invalid/qr".len() as u8);
    url.extend_from_slice(b"https://example.invalid/qr");
    out.extend(tlvs(&[(0x17, b"synthetic-png"), (0xd1, &url)]));
    response(2066, &out, key)
}
fn poll_body(code: u8) -> Vec<u8> {
    let mut body = vec![0; 54];
    body[8..10].copy_from_slice(&0x12_u16.to_be_bytes());
    body.push(code);
    body
}
fn confirm(flow: &mut QrLogin, key: &[u8; 16]) -> [u8; 16] {
    let ticket_key = std::array::from_fn(|i| i as u8 + 16);
    let mut out = poll_body(0);
    out.extend_from_slice(&[0; 4]);
    out.extend_from_slice(&SYNTHETIC_UIN.to_be_bytes());
    out.extend_from_slice(&[0; 4]);
    out.extend(tlvs(&[
        (0x18, b"synthetic-t106"),
        (0x19, b"synthetic-t16a"),
        (0x1e, &ticket_key),
    ]));
    assert_eq!(
        flow.handle_poll(&response(2066, &out, key)).unwrap(),
        QrState::Confirmed { uin: SYNTHETIC_UIN }
    );
    ticket_key
}

#[test]
fn full_qr_handshake_matches_independent_go_tlvs() {
    let mut flow = flow();
    assert!(flow.poll_request(1_700_000_000).is_err());
    assert!(flow.login_request().is_err());
    let (cmd, uin, request, key) = inspect_request(&flow.fetch_request(1_700_000_000).unwrap());
    assert_eq!((cmd, uin), (2066, 0));
    assert_eq!(&request[67..], hex(FETCH));
    assert_eq!(&request[21..23], &0x31_u16.to_be_bytes());
    assert_eq!(&request[14..18], &1_700_000_000_u32.to_be_bytes());
    let qr = flow.handle_fetch(&fetch_response(&key)).unwrap();
    assert_eq!(qr.png, b"synthetic-png");
    assert_eq!(qr.url, "https://example.invalid/qr");
    let (cmd, uin, request, _) = inspect_request(&flow.poll_request(1_700_000_002).unwrap());
    assert_eq!((cmd, uin), (2066, 0));
    assert_eq!(&request[21..23], &0x12_u16.to_be_bytes());
    let mut expected = Vec::new();
    short(&mut expected, b"synthetic-qrsig");
    expected.extend_from_slice(&[0; 13]);
    expected.push(0x83);
    assert_eq!(&request[67..], expected);
    assert_eq!(
        flow.handle_poll(&response(2066, &poll_body(48), &key))
            .unwrap(),
        QrState::WaitingForScan
    );
    assert_eq!(
        flow.handle_poll(&response(2066, &poll_body(53), &key))
            .unwrap(),
        QrState::WaitingForConfirm
    );
    let ticket_key = confirm(&mut flow, &key);
    let (cmd, uin, request, _) = inspect_request(&flow.login_request().unwrap());
    assert_eq!((cmd, uin), (2064, SYNTHETIC_UIN));
    let mut r = Reader::new(&request);
    assert_eq!(read16(&mut r), 9);
    assert_eq!(read16(&mut r), 15);
    let mut plain = vec![0, 9, 0, 14];
    while !r.rest().is_empty() {
        let tag = read16(&mut r);
        let value = readshort(&mut r);
        if tag == 0x144 {
            assert_eq!(tea::decrypt(value, &ticket_key).unwrap(), hex(PROTECTED));
        } else {
            plain.extend_from_slice(&tag.to_be_bytes());
            short(&mut plain, value);
        }
    }
    assert_eq!(plain, hex(LOGIN_WITHOUT_T144));

    // Construct an independent server-side success response using the documented
    // 0x119 credential bundle and 0x543 / 9 / 11 / 1 UID field path.
    let mut uid = Vec::new();
    wire::bytes(1, b"synthetic-uid", &mut uid);
    let mut layer = Vec::new();
    wire::bytes(11, &uid, &mut layer);
    let mut envelope = Vec::new();
    wire::bytes(9, &layer, &mut envelope);
    let nickname = b"synthetic-user";
    let mut profile = vec![0, 0, 18, 1, nickname.len() as u8];
    profile.extend_from_slice(nickname);
    let new_d2_key = [0x42; 16];
    let credentials = tlvs(&[
        (0x10a, b"synthetic-tgt"),
        (0x143, b"synthetic-d2"),
        (0x305, &new_d2_key),
        (0x543, &envelope),
        (0x11a, &profile),
        (0x106, b"synthetic-password-ticket"),
    ]);
    let encrypted = tea::encrypt(&credentials, &ticket_key).unwrap();
    let mut body = vec![0, 9, 0];
    body.extend(tlvs(&[(0x119, &encrypted)]));
    let result = flow.handle_login(&response(2064, &body, &key)).unwrap();
    assert_eq!(result.uin, SYNTHETIC_UIN);
    assert_eq!(result.uid, "synthetic-uid");
    assert_eq!(result.tgt, b"synthetic-tgt");
    assert_eq!(result.d2, b"synthetic-d2");
    assert_eq!(result.d2_key, new_d2_key);
    assert_eq!(result.tgtgt, <[u8; 16]>::from(Md5::digest(new_d2_key)));
    assert_eq!(result.nickname, "synthetic-user");
    assert_eq!(result.temp_pwd, b"synthetic-password-ticket");
}

#[test]
fn qr_state_and_failure_paths_do_not_leave_reusable_tickets() {
    let mut flow = flow();
    let (_, _, _, key) = inspect_request(&flow.fetch_request(1).unwrap());
    flow.handle_fetch(&fetch_response(&key)).unwrap();
    confirm(&mut flow, &key);
    assert_eq!(
        flow.handle_poll(&response(2066, &poll_body(54), &key))
            .unwrap(),
        QrState::Canceled
    );
    assert!(flow.login_request().is_err());
    confirm(&mut flow, &key);
    assert_eq!(
        flow.handle_poll(&response(2066, &poll_body(17), &key))
            .unwrap(),
        QrState::Expired
    );
    assert!(flow.login_request().is_err());
    assert_eq!(
        flow.handle_poll(&response(2066, &poll_body(99), &key))
            .unwrap(),
        QrState::Unknown(99)
    );
    confirm(&mut flow, &key);
    assert_eq!(
        flow.handle_poll(&response(2066, &poll_body(48), &key))
            .unwrap(),
        QrState::WaitingForScan
    );
    assert!(flow.login_request().is_err());
    confirm(&mut flow, &key);
    let mut reason = vec![0; 4];
    short(&mut reason, b"verification needed");
    short(
        &mut reason,
        b"https://example.invalid/verify?token=synthetic-secret",
    );
    let mut body = vec![0, 9, 2];
    body.extend(tlvs(&[(0x146, &reason)]));
    let error = flow
        .handle_login(&response(2064, &body, &key))
        .err()
        .unwrap();
    assert!(error.contains("0x02"));
    assert!(error.contains("verification needed"));
    assert!(!error.contains("synthetic-secret"));
    assert!(!error.contains("https://"));
}

#[test]
fn malformed_qr_responses_and_server_keys_are_rejected() {
    let mut flow = flow();
    let (_, _, _, key) = inspect_request(&flow.fetch_request(1).unwrap());
    let valid = fetch_response(&key);
    for n in 0..valid.len() {
        assert!(
            flow.handle_fetch(&valid[..n]).is_err(),
            "truncated QR response {n}"
        );
    }
    let mut wrong = valid.clone();
    wrong[5..7].copy_from_slice(&2064_u16.to_be_bytes());
    assert!(flow.handle_fetch(&wrong).is_err());
    let mut wrong = valid.clone();
    wrong[14] = 3;
    assert!(flow.handle_fetch(&wrong).is_err());
    let mut wrong = valid;
    *wrong.last_mut().unwrap() = 0;
    assert!(flow.handle_fetch(&wrong).is_err());
    assert!(QrLogin::with_server_key(&device(), 1, &[0; 65]).is_err());
    assert!(qr::parse_server_key_json(br#"{"PubKeyMeta":{"KeyVer":0,"PubKey":"04"}}"#).is_err());
    assert!(qr::parse_server_key_json(&vec![0; 16_385]).is_err());
    let encoded = server_secret()
        .public_key()
        .to_encoded_point(false)
        .as_bytes()
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect::<String>();
    let json = serde_json::to_vec(&serde_json::json!({"PubKeyMeta":{"KeyVer":9,"PubKey":encoded}}))
        .unwrap();
    let (version, public) = qr::parse_server_key_json(&json).unwrap();
    assert_eq!(version, 9);
    assert_eq!(
        public,
        server_secret()
            .public_key()
            .to_encoded_point(false)
            .as_bytes()
    );
    assert!(QrLogin::new(&device()).is_ok());
}

#[test]
fn explicitly_selected_326_qr_login_uses_independent_go_app_profile_vector() {
    let remote = server_secret().public_key().to_encoded_point(false);
    let mut flow = QrLogin::with_server_key_profile(
        &device(),
        9,
        remote.as_bytes(),
        platform_qq::AppProfile::Linux32646494,
    )
    .unwrap();
    let (_, _, _, key) = inspect_request(&flow.fetch_request(1).unwrap());
    flow.handle_fetch(&fetch_response(&key)).unwrap();
    confirm(&mut flow, &key);
    let (_, _, request, _) = inspect_request(&flow.login_request().unwrap());
    let mut r = Reader::new(&request);
    assert_eq!(read16(&mut r), 9);
    let n = read16(&mut r);
    let fixture: serde_json::Value =
        serde_json::from_str(include_str!("auth-reference.json")).unwrap();
    let expected = hex(fixture["t100_326"].as_str().unwrap());
    let mut found = false;
    for _ in 0..n {
        let tag = read16(&mut r);
        let data = readshort(&mut r);
        if tag == 0x100 {
            let mut actual = tag.to_be_bytes().to_vec();
            short(&mut actual, data);
            assert_eq!(actual, expected);
            found = true;
        }
    }
    assert!(found);
    let profile = platform_qq::AppProfile::Linux32646494.info();
    let reference = &fixture["profile_326"];
    assert_eq!(
        profile.version,
        reference["current_version"].as_str().unwrap()
    );
    assert_eq!(
        u64::from(profile.sub_app_id),
        reference["sub_app_id"].as_u64().unwrap()
    );
    assert_eq!(
        profile.package_sign,
        reference["package_sign"].as_str().unwrap()
    );
}
