// SPDX-License-Identifier: MIT
//! Generated C-reference vectors, not implementation-shaped expectations.
//! Source, model-free synthesis settings and hashes are pinned in vectors/provenance.json.
use runtime_silk::{Decoder, PacketReader};
#[test]
fn skype_v3_packets_match_the_reference_sample_for_sample() {
    let cases: &[(&str, &[u8], &[u8])] = &[
        (
            "silence8",
            include_bytes!("vectors/silence8.silk"),
            include_bytes!("vectors/silence8.pcm"),
        ),
        (
            "sine8",
            include_bytes!("vectors/sine8.silk"),
            include_bytes!("vectors/sine8.pcm"),
        ),
        (
            "mix12",
            include_bytes!("vectors/mix12.silk"),
            include_bytes!("vectors/mix12.pcm"),
        ),
        (
            "mix16",
            include_bytes!("vectors/mix16.silk"),
            include_bytes!("vectors/mix16.pcm"),
        ),
        (
            "chirp24",
            include_bytes!("vectors/chirp24.silk"),
            include_bytes!("vectors/chirp24.pcm"),
        ),
        (
            "noise16",
            include_bytes!("vectors/noise16.silk"),
            include_bytes!("vectors/noise16.pcm"),
        ),
        (
            "impulse24",
            include_bytes!("vectors/impulse24.silk"),
            include_bytes!("vectors/impulse24.pcm"),
        ),
    ];
    for &(name, input, reference) in cases {
        let mut packets = PacketReader::new(input).unwrap();
        let mut decoder = Decoder::new(24000).unwrap();
        let mut output = Vec::new();
        while let Some(packet) = packets.next_packet().unwrap() {
            for sample in decoder.decode_packet(&packet).unwrap() {
                output.extend_from_slice(&sample.to_le_bytes())
            }
        }
        assert_eq!(output.len(), reference.len(), "{name} sample count");
        assert!(
            output == reference,
            "{name}: first differing sample {:?}",
            output
                .chunks_exact(2)
                .zip(reference.chunks_exact(2))
                .position(|(a, b)| a != b)
        );
    }
}

#[test]
fn all_exposed_output_rates_match_the_reference_resampler() {
    let input = include_bytes!("vectors/mix16.silk").as_slice();
    let cases: &[(u32, &[u8])] = &[
        (8000, include_bytes!("vectors/mix16.rate8000.pcm")),
        (12000, include_bytes!("vectors/mix16.rate12000.pcm")),
        (16000, include_bytes!("vectors/mix16.rate16000.pcm")),
        (24000, include_bytes!("vectors/mix16.rate24000.pcm")),
        (32000, include_bytes!("vectors/mix16.rate32000.pcm")),
        (44100, include_bytes!("vectors/mix16.rate44100.pcm")),
        (48000, include_bytes!("vectors/mix16.rate48000.pcm")),
    ];
    for &(rate, want) in cases {
        let mut packets = PacketReader::new(input).unwrap();
        let mut decoder = Decoder::new(rate).unwrap();
        let mut got = Vec::new();
        assert_eq!(decoder.sample_rate(), rate);
        while let Some(packet) = packets.next_packet().unwrap() {
            for s in decoder.decode_packet(&packet).unwrap() {
                got.extend_from_slice(&s.to_le_bytes());
            }
        }
        assert!(
            got == want,
            "output rate {rate} differs from the C reference"
        );
    }
}
#[test]
fn dtx_empty_packets_keep_time_and_match_reference_comfort_noise() {
    for (name, input, want) in [
        (
            "dtx8",
            include_bytes!("vectors/dtx8.silk").as_slice(),
            include_bytes!("vectors/dtx8.pcm").as_slice(),
        ),
        (
            "dtx16",
            include_bytes!("vectors/dtx16.silk").as_slice(),
            include_bytes!("vectors/dtx16.pcm").as_slice(),
        ),
    ] {
        let mut packets = PacketReader::new(input).unwrap();
        let mut decoder = Decoder::new(24000).unwrap();
        let mut got = Vec::new();
        let mut empty = 0;
        while let Some(packet) = packets.next_packet().unwrap() {
            if packet.is_empty() {
                empty += 1
            };
            for s in decoder.decode_packet(&packet).unwrap() {
                got.extend_from_slice(&s.to_le_bytes());
            }
        }
        assert!(empty > 0, "{name} must exercise actual DTX gaps");
        assert_eq!(got.len(), 24000 * 4 * 2);
        assert!(got == want, "{name} differs from the C reference");
    }
}
#[test]
fn loss_concealment_retains_a_multi_frame_packet_duration() {
    let mut packets =
        PacketReader::new(include_bytes!("vectors/impulse24.silk").as_slice()).unwrap();
    let mut decoder = Decoder::new(24000).unwrap();
    let first = decoder
        .decode_packet(&packets.next_packet().unwrap().unwrap())
        .unwrap();
    assert_eq!(first.len(), 2400);
    let concealed = decoder.decode_packet(&[]).unwrap();
    assert_eq!(concealed.len(), first.len());
}
#[test]
fn malformed_packets_are_bounded_and_poison_a_failed_stream() {
    use runtime_silk::{Error, MAX_FRAMES_PER_PACKET};
    let mut random = 0x75a1u32;
    for length in [1, 2, 3, 4, 7, 15, 31, 63, 127, 255, 511, 1024] {
        for _ in 0..8 {
            let mut bytes = vec![0; length];
            for b in &mut bytes {
                random = random.wrapping_mul(1664525).wrapping_add(1013904223);
                *b = (random >> 24) as u8;
            }
            let mut decoder = Decoder::new(24000).unwrap();
            match decoder.decode_packet(&bytes) {
                Ok(pcm) => assert!(pcm.len() <= 480 * MAX_FRAMES_PER_PACKET && !pcm.is_empty()),
                Err(Error::Decode(_)) => {
                    assert!(matches!(decoder.decode_packet(&[]), Err(Error::Decode(_))))
                }
                Err(Error::FrameLimit) => {
                    assert!(matches!(decoder.decode_packet(&[]), Err(Error::Decode(_))));
                }
                Err(e) => panic!("unexpected error for an in-budget packet: {e}"),
            }
        }
    }
}
