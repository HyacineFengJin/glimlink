//! Golden vectors generated in the network-disabled Go lab container from
//! LagrangeGo f7f8abfaa7f1, using RomiChan/protobuf and PackElementsToBody.
//! All targets, text, timestamps and acknowledgements are synthetic.
use platform_qq::{
    message::{self, MAX_TEXT_BYTES, Target},
    wire,
};

fn unhex(value: &str) -> Vec<u8> {
    value
        .as_bytes()
        .chunks_exact(2)
        .map(|pair| {
            let nibble = |b: u8| match b {
                b'0'..=b'9' => b - b'0',
                b'a'..=b'f' => b - b'a' + 10,
                _ => panic!("invalid synthetic vector"),
            };
            nibble(pair[0]) * 16 + nibble(pair[1])
        })
        .collect()
}

#[test]
fn plain_text_requests_match_pinned_go_vectors() {
    let cases = [
        (
            Target::Private("u_fixture_only"),
            "hello 喵\n<&>",
            123,
            0x1234_5678,
            1_700_000_000,
            "0a120a10120e755f666978747572655f6f6e6c7912060801100018001a150a1312110a0f0a0d68656c6c6f20e596b50a3c263e207b28f8acd19101",
        ),
        (
            Target::Private("u_fixture_only"),
            "fixture",
            0,
            0,
            0,
            "0a120a10120e755f666978747572655f6f6e6c7912060801100018001a0f0a0d120b0a090a076669787475726520002800",
        ),
        (
            Target::Group(123_456_789),
            "群聊 喵",
            32767,
            u32::MAX,
            1_700_000_000,
            "0a07120508959aef3a12060801100018001a120a10120e0a0c0a0ae7bea4e8818a20e596b520ffff0128ffffffff0f62060880e2cfaa06",
        ),
        (
            Target::Group(u32::MAX),
            "2038 fixture",
            0,
            0,
            0x8000_0000,
            "0a08120608ffffffff0f12060801100018001a140a1212100a0e0a0c32303338206669787475726520002800620b0880808080f8ffffffff01",
        ),
        (
            Target::Group(1),
            "zero time fixture",
            0,
            0,
            0,
            "0a041202080112060801100018001a190a1712150a130a117a65726f2074696d652066697874757265200028006200",
        ),
    ];
    for (target, text, sequence, random, timestamp, expected) in cases {
        assert_eq!(
            message::encode_text(target, text, sequence, random, timestamp).unwrap(),
            unhex(expected)
        );
    }
}

#[test]
fn acknowledgements_match_pinned_go_vectors_and_require_correct_route() {
    let private = message::decode_response(&unhex("1880e2cfaa067095d6b0d10b")).unwrap();
    assert_eq!(private.result, 0); // proto3 default Result is omitted by Go.
    assert_eq!(private.timestamp, Some(1_700_000_000));
    assert_eq!(
        private
            .confirmed_sequence(Target::Private("u_fixture_only"))
            .unwrap(),
        3_123_456_789
    );
    assert!(private.confirmed_sequence(Target::Group(1)).is_err());

    let group = message::decode_response(&unhex("1881e2cfaa0658cd83066082e2cfaa06")).unwrap();
    assert_eq!(group.timestamp, Some(1_700_000_001));
    assert_eq!(group.timestamp2, Some(1_700_000_002));
    assert_eq!(group.confirmed_sequence(Target::Group(1)).unwrap(), 98_765);
    assert!(
        group
            .confirmed_sequence(Target::Private("u_fixture_only"))
            .is_err()
    );

    let rejected = message::decode_response(&unhex(
        "08ffffffffffffffffff01121173796e746865746963206661696c757265",
    ))
    .unwrap();
    assert_eq!(rejected.result, -1);
    assert!(
        rejected
            .confirmed_sequence(Target::Private("u_fixture_only"))
            .is_err()
    );
    assert!(
        !serde_json::to_string(&rejected)
            .unwrap()
            .contains("synthetic failure")
    );
}

#[test]
fn malformed_or_incomplete_acknowledgements_never_confirm_delivery() {
    for data in [
        vec![],
        vec![0x08],
        vec![0x0a, 0],
        vec![0x70, 0x80],
        vec![0x72, 0],
    ] {
        assert!(message::decode_response(&data).is_err());
    }
    for data in [vec![0x08, 0], vec![0x70, 0], vec![0x58, 0], vec![0x18, 1]] {
        let response = message::decode_response(&data).unwrap();
        assert!(
            response
                .confirmed_sequence(Target::Private("u_fixture_only"))
                .is_err()
        );
        assert!(response.confirmed_sequence(Target::Group(1)).is_err());
    }
    for field in [1, 3, 11, 12, 14] {
        let mut oversized = Vec::new();
        wire::number(field, u64::from(u32::MAX) + 1, &mut oversized);
        assert!(message::decode_response(&oversized).is_err());
    }
}

#[test]
fn unknown_response_fields_are_tolerated_and_last_singular_value_wins() {
    let mut encoded = Vec::new();
    wire::number(1, 77, &mut encoded);
    wire::number(14, 40, &mut encoded);
    wire::bytes(99, b"synthetic future field", &mut encoded);
    wire::number(1, 0, &mut encoded);
    wire::number(14, 41, &mut encoded);
    let response = message::decode_response(&encoded).unwrap();
    assert_eq!(
        response
            .confirmed_sequence(Target::Private("u_fixture_only"))
            .unwrap(),
        41
    );
}

#[test]
fn invalid_targets_and_oversize_utf8_fail_before_any_network_operation() {
    for target in [
        Target::Private(""),
        Target::Private("u_bad\nuid"),
        Target::Private("喵"),
        Target::Group(0),
    ] {
        assert!(message::encode_text(target, "fixture", 0, 0, 0).is_err());
    }
    assert!(message::encode_text(Target::Private("u_fixture_only"), "", 0, 0, 0).is_err());
    let oversized = "喵".repeat(MAX_TEXT_BYTES / 3 + 1);
    assert!(message::encode_text(Target::Private("u_fixture_only"), &oversized, 0, 0, 0).is_err());
    let boundary = "a".repeat(MAX_TEXT_BYTES);
    assert!(message::encode_text(Target::Private("u_fixture_only"), &boundary, 0, 0, 0).is_ok());
}
