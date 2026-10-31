//! Independent pinned-Go protobuf fixtures. Generated in a network-disabled
//! Go container by referencegen.go; all identities and bytes are synthetic.
use platform_qq::{
    message::{self, Target},
    rich::{self, MediaKind, Message, Part, Quote, Segment},
    wire::{self, Fields},
};
fn hex(s: &str) -> Vec<u8> {
    (0..s.len())
        .step_by(2)
        .map(|n| u8::from_str_radix(&s[n..n + 2], 16).unwrap())
        .collect()
}
const RECEIVED: &str = "0aa201123caa0339083012330a310a2f0a1d08d209220d616e696d6174696f6e2e6769662a05080110d00f30203810120c757569645f666978747572651801181412299a01260a0d766964656f5f666978747572651a08636c69702e6d7034280330a912800180058801e80222371a0d766f6963655f666978747572652a0a766f6963652e73696c6b30801b920113766f6963655f67726f75705f66697874757265980102122d0a2b1a0c66696c655f666978747572652a096e6f7465732e74787430d723ca030c686173685f66697874757265";
#[test]
fn ordered_text_native_mention_quote_and_face_match_independent_go() {
    let message = Message {
        quote: Some(Quote {
            sequence: 42,
            sender_uin: 12345,
            sender_uid: "u_sender_fixture".into(),
            timestamp: 1700000000,
            text: "原文".into(),
        }),
        parts: vec![
            Part::Text {
                text: "hello ".into(),
            },
            Part::Mention {
                uid: "u_member_fixture".into(),
                display: "@成员".into(),
                all: false,
            },
            Part::Face { id: 14 },
            Part::Text { text: "end".into() },
        ],
        ..Default::default()
    };
    let raw = message::encode_rich(Target::Group(123456), &message, 2, 7, 1700000000).unwrap();
    let body = Fields::parse(&raw).unwrap().data(3).unwrap();
    assert_eq!(
        body,
        hex(
            "0a761232ea022f082a10b9601880e2cfaa062a0a0a080a06e58e9fe696874214182a3a10755f73656e6465725f666978747572655000120a0a080a0668656c6c6f2012250a230a0740e68890e5919862181802200028004a10755f6d656d6265725f6669787475726512041202080e12070a050a03656e64"
        )
    );
    let parsed =
        rich::parse_elements(Fields::parse(body).unwrap().data(1).unwrap(), Some(123456)).unwrap();
    assert!(matches!(&parsed[0],Segment::Reply{sequence:42,text,..} if text=="原文"));
    assert!(matches!(&parsed[2],Segment::Mention{uid,all:false,..} if uid=="u_member_fixture"));
    assert!(matches!(parsed[3], Segment::Face { id: 14 }));
    assert!(
        message::encode_rich(
            Target::Private("u_peer_fixture"),
            &message,
            2,
            7,
            1700000000
        )
        .is_err()
    );
}
#[test]
fn gif_voice_video_and_file_are_typed_without_fetching_bytes() {
    let raw = hex(RECEIVED);
    let body = Fields::parse(&raw).unwrap();
    let parsed = rich::parse_elements(body.data(1).unwrap(), Some(123456)).unwrap();
    assert_eq!(parsed.len(), 3);
    assert!(parsed.iter().any(|s|matches!(s,Segment::Media{kind:MediaKind::Image,name,width:Some(32),height:Some(16),locator,..}if name=="animation.gif"&&locator.node.is_some()&&locator.url.is_none())));
    assert!(parsed.iter().any(|s|matches!(s,Segment::Media{kind:MediaKind::Voice,name,duration_ms:Some(2000),locator,..}if name=="voice.silk"&&locator.group==Some(123456))));
    assert!(parsed.iter().any(|s|matches!(s,Segment::Media{kind:MediaKind::Video,name,duration_ms:Some(3000),width:Some(640),..}if name=="clip.mp4")));
    assert!(
        matches!(rich::parse_body_file(&raw,None).unwrap(),Some(Segment::Media{kind:MediaKind::File,size:Some(4567),locator,..})if locator.file_uuid.as_deref()==Some("file_fixture")&&locator.file_hash.as_deref()==Some("hash_fixture")&&locator.group.is_none())
    );
}
#[test]
fn packed_quote_overflow_and_excess_element_count_are_rejected() {
    let mut src = Vec::new();
    wire::bytes(1, &[255, 255, 255, 255, 127], &mut src);
    let mut e = Vec::new();
    wire::bytes(45, &src, &mut e);
    let mut rich = Vec::new();
    wire::bytes(2, &e, &mut rich);
    assert!(rich::parse_elements(&rich, None).is_err());
    let mut e = Vec::new();
    for _ in 0..65 {
        wire::bytes(2, &[8, 1], &mut e)
    }
    let mut rich = Vec::new();
    wire::bytes(2, &e, &mut rich);
    assert!(rich::parse_elements(&rich, None).is_err());
    let excessive = Message {
        parts: (0..65).map(|_| Part::Face { id: 1 }).collect(),
        ..Default::default()
    };
    assert!(excessive.validate().is_err());
}
#[test]
fn media_url_policy_rejects_arbitrary_hosts_credentials_ports_and_controls() {
    for url in [
        "http://127.0.0.1/x",
        "https://gchat.qpic.cn.evil.test/x",
        "https://user@gchat.qpic.cn/x",
        "https://gchat.qpic.cn:8080/x",
        "file:///etc/passwd",
        "https://gchat.qpic.cn/a\nb",
    ] {
        assert!(
            platform_qq::media::validate_media_url(url).is_err(),
            "{url}"
        )
    }
    for url in [
        "https://gchat.qpic.cn/gchatpic_new/a/0",
        "https://multimedia.nt.qq.com.cn/download?x=1",
    ] {
        platform_qq::media::validate_media_url(url).unwrap()
    }
}
#[test]
fn private_file_uses_native_transfer_route_and_exact_file_metadata() {
    let file = platform_qq::attachments::NativeFile {
        name: "notes.txt".into(),
        bytes: 3,
        md5: "900150983cd24fb0d6963f7d28e17f72".into(),
        uuid: "file_fixture".into(),
        hash: "hash_fixture".into(),
    };
    let message = Message {
        file: Some(file),
        ..Default::default()
    };
    let raw = message::encode_rich(
        Target::Private("u_peer_fixture"),
        &message,
        2,
        7,
        1700000000,
    )
    .unwrap();
    let req = Fields::parse(&raw).unwrap();
    let route = Fields::parse(req.data(1).unwrap()).unwrap();
    assert!(route.data(1).is_none());
    let trans = Fields::parse(route.data(15).unwrap()).unwrap();
    assert_eq!(trans.num(2), Some(4));
    assert_eq!(trans.data(8), Some(&b"u_peer_fixture"[..]));
    let body = Fields::parse(req.data(3).unwrap()).unwrap();
    let extra = Fields::parse(body.data(2).unwrap()).unwrap();
    let metadata = Fields::parse(extra.data(1).unwrap()).unwrap();
    assert_eq!(metadata.data(3), Some(&b"file_fixture"[..]));
    assert_eq!(metadata.num(6), Some(3));
    assert_eq!(metadata.num(9), Some(1));
    assert_eq!(metadata.data(57), Some(&b"hash_fixture"[..]));
    assert!(message::encode_rich(Target::Group(123456), &message, 2, 7, 1700000000).is_err());
}
