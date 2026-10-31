// SPDX-License-Identifier: MIT
//! Independent local HTTP server exercises actual request encoding and cookies.
//! The HTTP rewrite exists only in cfg(test); production endpoints remain HTTPS-only.
use super::*;
use std::os::unix::fs::PermissionsExt;
use std::{
    fs,
    io::{Read, Write},
    net::{TcpListener, TcpStream},
    thread,
    time::{Duration, Instant},
};

fn read_request(stream: &mut TcpStream) -> String {
    stream
        .set_read_timeout(Some(Duration::from_secs(2)))
        .unwrap();
    let mut data = Vec::new();
    while !data.ends_with(b"\r\n\r\n") {
        let mut byte = [0];
        stream.read_exact(&mut byte).unwrap();
        data.push(byte[0]);
        assert!(data.len() < 32768);
    }
    let header = String::from_utf8(data.clone()).unwrap();
    let length = header
        .lines()
        .find_map(|l| {
            l.split_once(':')
                .filter(|(k, _)| k.eq_ignore_ascii_case("content-length"))
                .map(|(_, v)| v.trim().parse::<usize>().unwrap())
        })
        .unwrap_or(0);
    assert!(length < 32768);
    let mut body = vec![0; length];
    stream.read_exact(&mut body).unwrap();
    data.extend(body);
    String::from_utf8(data).unwrap()
}
fn reply(stream: &mut TcpStream, mime: &str, body: &[u8], headers: &str) {
    write!(stream,"HTTP/1.1 200 OK\r\nContent-Type: {mime}\r\nContent-Length: {}\r\nConnection: close\r\n{headers}\r\n",body.len()).unwrap();
    stream.write_all(body).unwrap();
}

fn run_profile_workflow(profile: LoginProfile) {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    listener.set_nonblocking(true).unwrap();
    let address = listener.local_addr().unwrap();
    let server = thread::spawn(move || {
        let deadline = Instant::now() + Duration::from_secs(15);
        let mut count = 0;
        while count < 11 {
            let (mut stream, _) = loop {
                match listener.accept() {
                    Ok(v) => break v,
                    Err(e) if e.kind() == std::io::ErrorKind::WouldBlock => {
                        assert!(
                            Instant::now() < deadline,
                            "server fixture deadline at request {count}"
                        );
                        thread::sleep(Duration::from_millis(1));
                    }
                    Err(e) => panic!("{e}"),
                }
            };
            let request = read_request(&mut stream);
            let lower = request.to_ascii_lowercase();
            if count != 4 {
                assert!(!lower.contains("\r\nclient-version:"));
                assert!(!lower.contains("\r\nextspam:"));
            }
            match count {
                0 => {
                    assert!(request.starts_with("GET /jslogin?"));
                    assert_eq!(
                        request.contains("%3Fmod%3Ddesktop"),
                        profile == LoginProfile::DesktopCompatibility
                    );
                    reply(
                        &mut stream,
                        "text/javascript",
                        br#"window.QRLogin.code = 200; window.QRLogin.uuid = "fixture_uuid==";"#,
                        "Set-Cookie: login=synthetic-login; Domain=.wx.qq.com; Path=/\r\n",
                    );
                }
                1 => {
                    assert!(request.starts_with("GET /qrcode/fixture_uuid== "));
                    assert!(!lower.contains("cookie: login="));
                    reply(&mut stream, "image/png", b"\x89PNG\r\n\x1a\nfixture", "");
                }
                2 => {
                    assert!(request.starts_with("GET /cgi-bin/mmwebwx-bin/login?"));
                    assert!(request.contains("tip=1"));
                    assert!(lower.contains("cookie: login=synthetic-login"));
                    reply(&mut stream, "text/javascript", b"window.code=201;", "");
                }
                3 => {
                    assert!(request.contains("tip=0"));
                    reply(&mut stream,"text/javascript",br#"window.code=200;window.redirect_uri="https://wx2.qq.com/cgi-bin/mmwebwx-bin/webwxnewloginpage?ticket=synthetic-ticket";"#,"");
                }
                4 => {
                    assert!(request.starts_with(
                        "GET /cgi-bin/mmwebwx-bin/webwxnewloginpage?ticket=synthetic-ticket&version=v2"
                    ));
                    assert!(!lower.contains("cookie: login="));
                    let header = |key: &str| {
                        request.lines().find_map(|line| {
                            line.split_once(':')
                                .filter(|(name, _)| name.eq_ignore_ascii_case(key))
                                .map(|(_, value)| value.trim())
                        })
                    };
                    if profile == LoginProfile::DesktopCompatibility {
                        assert_eq!(header("client-version"), Some("2.0.0"));
                        assert_eq!(header("extspam"), Some(crate::desktop::EXTSPAM));
                    } else {
                        assert!(header("client-version").is_none());
                        assert!(header("extspam").is_none());
                    }
                    let body=b"<error><ret>0</ret><skey>@synthetic-key</skey><wxsid>synthetic-sid</wxsid><wxuin>123</wxuin><pass_ticket>synthetic-pass</pass_ticket></error>";
                    write!(&mut stream,"HTTP/1.1 302 Found\r\nContent-Type: text/xml\r\nContent-Length: {}\r\nLocation: https://wx2.qq.com/\r\nSet-Cookie: sid=synthetic-cookie; Domain=.wx2.qq.com; Path=/\r\nConnection: close\r\n\r\n",body.len()).unwrap();
                    stream.write_all(body).unwrap();
                }
                5 => {
                    assert!(request.starts_with("POST /cgi-bin/mmwebwx-bin/webwxinit?"));
                    let body = request.split_once("\r\n\r\n").unwrap().1;
                    let parsed: serde_json::Value = serde_json::from_str(body).unwrap();
                    assert_eq!(parsed["BaseRequest"]["Uin"], 123);
                    assert_eq!(
                        parsed["BaseRequest"]["DeviceID"].as_str().unwrap().len(),
                        16
                    );
                    reply(&mut stream,"application/json",br#"{"BaseResponse":{"Ret":0},"User":{"UserName":"@self","NickName":"fixture"},"SyncKey":{"Count":1,"List":[{"Key":1,"Val":10}]}}"#,"");
                }
                6 => {
                    assert!(request.starts_with("GET /cgi-bin/mmwebwx-bin/synccheck?"));
                    assert!(request.contains("synckey=1_10"));
                    assert!(lower.contains("cookie: sid=synthetic-cookie"));
                    reply(
                        &mut stream,
                        "text/javascript",
                        br#"window.synccheck={retcode:"0",selector:"2"}"#,
                        "",
                    );
                }
                7 => {
                    assert!(request.starts_with("GET /cgi-bin/mmwebwx-bin/webwxgetcontact?"));
                    reply(&mut stream,"application/json",br#"{"BaseResponse":{"Ret":0},"MemberCount":1,"MemberList":[{"UserName":"@friend","NickName":"friend"}],"Seq":0}"#,"");
                }
                8 => {
                    assert!(request.starts_with("POST /cgi-bin/mmwebwx-bin/webwxsync?"));
                    let parsed: serde_json::Value =
                        serde_json::from_str(request.split_once("\r\n\r\n").unwrap().1).unwrap();
                    assert_eq!(parsed["SyncKey"]["List"][0]["Val"], 10);
                    reply(&mut stream,"application/json",br#"{"BaseResponse":{"Ret":0},"SyncKey":{"Count":1,"List":[{"Key":1,"Val":20}]},"SKey":"must-not-escape","AddMsgCount":1,"AddMsgList":[{"MsgId":"1234567890123","MsgType":34,"FromUserName":"@friend","ToUserName":"@self","Content":"","VoiceLength":1000}],"ContinueFlag":0}"#,"");
                }
                9 => {
                    assert!(request.starts_with(
                        "GET /cgi-bin/mmwebwx-bin/webwxgetvoice?msgid=1234567890123&"
                    ));
                    assert!(lower.contains("range: bytes=0-"));
                    reply(&mut stream, "audio/silk", b"#!SILK_V3fixture", "");
                }
                10 => {
                    assert!(request.starts_with("GET /cgi-bin/mmwebwx-bin/synccheck?"));
                    assert!(request.contains("synckey=1_20"));
                    reply(
                        &mut stream,
                        "text/javascript",
                        br#"window.synccheck={retcode:"0",selector:"0"}"#,
                        "",
                    );
                }
                _ => unreachable!(),
            }
            count += 1;
        }
        count
    });
    let mut config = Config::default();
    config.profile = profile;
    config.test_endpoint = Some(format!("http://{address}"));
    config.request_timeout = Duration::from_secs(2);
    let client = Client::new(config).unwrap();
    let mut attempt = client.begin_login().unwrap();
    assert_eq!(attempt.profile(), profile);
    assert!(matches!(
        client.finish_login(&mut attempt),
        Err(Error::InvalidState)
    ));
    let dir =
        std::env::temp_dir().join(format!("weixin-workflow-{}-{address}", std::process::id()));
    fs::create_dir(&dir).unwrap();
    fs::set_permissions(&dir, fs::Permissions::from_mode(0o700)).unwrap();
    let qr = client
        .download_qr(&mut attempt, &dir.join("qr.png"))
        .unwrap();
    assert_eq!(qr.mime, "image/png");
    assert_eq!(
        client.poll_login(&mut attempt).unwrap(),
        LoginStatus::Scanned
    );
    attempt.save(&dir.join("attempt.json")).unwrap();
    let mut attempt = LoginAttempt::load(&dir.join("attempt.json")).unwrap();
    assert_eq!(attempt.profile(), profile);
    assert!(matches!(
        client.finish_login(&mut attempt),
        Err(Error::InvalidState)
    ));
    let other = Client::new(Config::default()).unwrap();
    if profile != LoginProfile::Web {
        assert!(matches!(
            other.poll_login(&mut attempt),
            Err(Error::InvalidState)
        ));
    }
    assert_eq!(
        client.poll_login(&mut attempt).unwrap(),
        LoginStatus::Confirmed
    );
    let session = client.finish_login(&mut attempt).unwrap();
    assert_eq!(session.profile(), profile);
    assert_eq!(session.stable_identity(), "webwx:123");
    session.save(&dir.join("session.json")).unwrap();
    let mut session = Session::load(&dir.join("session.json")).unwrap();
    assert_eq!(session.profile(), profile);
    assert_eq!(client.resume(&mut session).unwrap().user.username, "@self");
    assert_eq!(
        client.contacts(&mut session, 0).unwrap().contacts[0].username,
        "@friend"
    );
    let batch = client.sync(&mut session).unwrap();
    assert_eq!(batch.messages[0].media, Some(MediaKind::Voice));
    assert!(
        !serde_json::to_string(&batch)
            .unwrap()
            .contains("must-not-escape")
    );
    let mut voice = Vec::new();
    let receipt = client
        .download_media(&mut session, &batch.messages[0], &mut voice, 1024)
        .unwrap();
    assert_eq!(voice, b"#!SILK_V3fixture");
    assert_eq!(receipt.kind, MediaKind::Voice);
    session.save(&dir.join("session.json")).unwrap();
    let mut session = Session::load(&dir.join("session.json")).unwrap();
    client.resume(&mut session).unwrap();
    assert_eq!(server.join().unwrap(), 11);
    fs::remove_dir_all(dir).unwrap();
}

#[test]
fn real_http_qr_ticket_cursor_restore_receive_and_voice() {
    run_profile_workflow(LoginProfile::Web);
}
#[test]
fn desktop_profile_is_explicit_confirmed_and_only_ticket_gets_headers() {
    run_profile_workflow(LoginProfile::DesktopCompatibility);
}

#[test]
fn malformed_sync_does_not_advance_the_in_memory_cursor() {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let address = listener.local_addr().unwrap();
    let server = thread::spawn(move || {
        let (mut stream, _) = listener.accept().unwrap();
        let request = read_request(&mut stream);
        assert!(request.starts_with("POST /cgi-bin/mmwebwx-bin/webwxsync?"));
        reply(&mut stream,"application/json",br#"{"BaseResponse":{"Ret":0},"SyncKey":{"Count":1,"List":[{"Key":1,"Val":99}]},"AddMsgCount":1,"AddMsgList":[{"MsgType":34,"FromUserName":"@friend","ToUserName":"@self","Content":"missing-stable-id"}]}"#,"");
    });
    let mut config = Config::default();
    config.test_endpoint = Some(format!("http://{address}"));
    config.request_timeout = Duration::from_secs(2);
    let client = Client::new(config).unwrap();
    let mut session = Session::new(
        "wx.qq.com".into(),
        model::Ticket {
            skey: "synthetic-skey".into(),
            sid: "synthetic-sid".into(),
            uin: 123,
            pass_ticket: "synthetic-pass".into(),
        },
        "e123456789012345".into(),
        Vec::new(),
    )
    .unwrap();
    session.user = Contact {
        username: "@self".into(),
        ..Default::default()
    };
    session.sync_key = vec![(1, 10)];
    assert!(client.sync(&mut session).is_err());
    assert_eq!(session.sync_string(), "1_10");
    server.join().unwrap();
}
