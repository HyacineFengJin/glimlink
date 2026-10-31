// SPDX-License-Identifier: AGPL-3.0-only
//! Offline fixed vectors from gofastTEA v0.1.3 tea_test.go, plus wire limits.
use platform_qq::{
    tea,
    wire::{self, Fields, MAX_FRAME, Reader},
};
fn unhex(s: &str) -> Vec<u8> {
    (0..s.len())
        .step_by(2)
        .map(|i| u8::from_str_radix(&s[i..i + 2], 16).unwrap())
        .collect()
}
#[test]
fn independent_upstream_tea_vectors_decode_without_native_signer() {
    for (plain, cipher) in [
        (
            "MiraiGO Here",
            "b7b2e52af7f5b1fbf37fc3d5546ac7569aecd01bbacf09bf",
        ),
        (
            "LXY Testing~",
            "9d0ab85aa14f5434ee83cd2a6b28bf306263cdf88e01264c",
        ),
        ("s", "528e8b5c48300b548e94262736ebb8b7"),
    ] {
        assert_eq!(
            tea::decrypt(&unhex(cipher), b"0123456789ABCDEF").unwrap(),
            plain.as_bytes()
        );
    }
    for size in [0, 1, 7, 8, 15, 128, 4096] {
        let p = vec![0x5a; size];
        let encoded = tea::encrypt(&p, &[7; 16]).unwrap();
        assert_eq!(tea::decrypt(&encoded, &[7; 16]).unwrap(), p);
    }
}
#[test]
fn truncated_packets_invalid_tea_keys_and_malformed_protobuf_are_rejected() {
    for n in 0..16 {
        assert!(tea::decrypt(&vec![0; n], &[0; 16]).is_err());
    }
    for n in [0, 1, 15, 17, 32] {
        assert!(tea::encrypt(b"fixture", &vec![0; n]).is_err());
    }
    for n in [0_u32, 1, 2, 3, MAX_FRAME as u32 + 1, u32::MAX] {
        assert!(Reader::new(&n.to_be_bytes()).packet().is_err());
    }
    for p in [&[0_u8][..], &[0x0a, 0x80][..], &[0x08, 0xff][..]] {
        assert!(Fields::parse(p).is_err());
    }
    let mut p = Vec::new();
    wire::number(1, u64::MAX, &mut p);
    wire::bytes(2, b"fixture", &mut p);
    let f = Fields::parse(&p).unwrap();
    assert_eq!(f.num(1), Some(u64::MAX));
    assert_eq!(f.data(2), Some(b"fixture".as_slice()));
}
