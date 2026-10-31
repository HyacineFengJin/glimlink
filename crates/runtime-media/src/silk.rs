// SPDX-License-Identifier: MIT
//! Bounded streaming SILK-v3 windows, including Tencent's framing prefix.
use crate::{
    Limits, MediaError, Result,
    audio::{AudioProbe, AudioSegment},
};
use runtime_silk::{Decoder, PacketReader};
#[cfg(unix)]
use std::os::unix::fs::OpenOptionsExt;
use std::{
    fs::{File, OpenOptions},
    io::{Read, Write},
    path::Path,
};
const RATE: u32 = 24000;
fn error(e: impl std::fmt::Display) -> MediaError {
    MediaError::Decode(e.to_string())
}
pub fn is_silk(path: &Path) -> Result<bool> {
    let mut file = File::open(path)?;
    let mut head = [0u8; 10];
    let n = file.read(&mut head)?;
    Ok(head[..n].starts_with(b"#!SILK") || head[..n].starts_with(b"\x02#!SILK"))
}
pub fn probe(path: &Path, limits: &Limits) -> Result<AudioProbe> {
    limits.check_file(path)?;
    let packets = PacketReader::new(File::open(path)?).map_err(error)?;
    Ok(AudioProbe {
        codec: if packets.is_tencent() {
            "silk-v3-tencent"
        } else {
            "silk-v3"
        }
        .into(),
        track_id: 0,
        sample_rate: RATE,
        channels: 1,
        duration_ms: None,
    })
}
pub fn segment(
    input: &Path,
    output: &Path,
    start_ms: u64,
    duration_ms: u64,
    limits: &Limits,
) -> Result<AudioSegment> {
    let end_ms = limits.check_window(start_ms, duration_ms)?;
    let mut probe = probe(input, limits)?;
    let begin = start_ms
        .checked_mul(u64::from(RATE))
        .ok_or_else(|| MediaError::Limit("SILK timestamp".into()))?
        / 1000;
    let end = end_ms
        .checked_mul(u64::from(RATE))
        .ok_or_else(|| MediaError::Limit("SILK timestamp".into()))?
        / 1000;
    let max_bytes = (end - begin)
        .checked_mul(2)
        .and_then(|b| b.checked_add(44))
        .ok_or_else(|| MediaError::Limit("SILK output bytes".into()))?;
    if max_bytes > limits.max_output_bytes {
        return Err(MediaError::Limit("SILK output bytes".into()));
    }
    let mut options = OpenOptions::new();
    options.write(true).create_new(true);
    #[cfg(unix)]
    options.mode(0o600);
    let mut out = options.open(output)?;
    let work = (|| {
        crate::audio::wav_header(&mut out, RATE, 1, 0)?;
        let mut packets = PacketReader::new(File::open(input)?).map_err(error)?;
        let mut decoder = Decoder::new(RATE).map_err(error)?;
        let mut position = 0u64;
        let mut frames = 0u64;
        let mut gaps = Vec::new();
        while position < end {
            let Some(packet) = packets.next_packet().map_err(error)? else {
                probe.duration_ms = Some(position * 1000 / u64::from(RATE));
                break;
            };
            if packets.packets_read() > limits.max_packets as u64 {
                return Err(MediaError::Limit("SILK packet count".into()));
            }
            if packet.is_empty() && gaps.len() < 128 {
                gaps.push(format!(
                    "SILK lost packet at {}ms; concealment synthesized",
                    position * 1000 / u64::from(RATE)
                ))
            }
            let pcm = decoder.decode_packet(&packet).map_err(error)?;
            let left = begin.saturating_sub(position).min(pcm.len() as u64) as usize;
            let right = end.saturating_sub(position).min(pcm.len() as u64) as usize;
            for sample in &pcm[left..right] {
                out.write_all(&sample.to_le_bytes())?;
                frames += 1;
            }
            position += pcm.len() as u64;
        }
        if frames == 0 {
            return Err(MediaError::Decode(
                "no SILK audio in requested window".into(),
            ));
        }
        let actual_end = start_ms + frames * 1000 / u64::from(RATE);
        let complete = actual_end >= end_ms && gaps.is_empty();
        if actual_end < end_ms {
            gaps.push(format!(
                "requested {start_ms}..{end_ms}ms, decoded {start_ms}..{actual_end}ms"
            ))
        }
        let bytes = frames * 2;
        crate::audio::wav_header(&mut out, RATE, 1, bytes)?;
        out.sync_all()?;
        Ok(AudioSegment {
            probe,
            path: output.into(),
            requested_start_ms: start_ms,
            requested_end_ms: end_ms,
            actual_start_ms: start_ms,
            actual_end_ms: actual_end,
            frames,
            bytes: bytes + 44,
            complete,
            gaps,
        })
    })();
    if work.is_err() {
        drop(out);
        let _ = std::fs::remove_file(output);
    }
    work
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn silk_windows_match_reference_pcm_across_multi_frame_packets_and_tencent_header() {
        let root = std::env::temp_dir().join(format!("silk-window-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        std::fs::create_dir(&root).unwrap();
        for (name, input, reference) in [
            (
                "standard",
                include_bytes!("../vendor/runtime-silk/tests/vectors/mix16.silk").as_slice(),
                include_bytes!("../vendor/runtime-silk/tests/vectors/mix16.pcm").as_slice(),
            ),
            (
                "tencent",
                include_bytes!("../vendor/runtime-silk/tests/vectors/chirp24.silk").as_slice(),
                include_bytes!("../vendor/runtime-silk/tests/vectors/chirp24.pcm").as_slice(),
            ),
        ] {
            let input_path = root.join(format!("{name}.silk"));
            let output = root.join(format!("{name}.wav"));
            std::fs::write(&input_path, input).unwrap();
            let segment = segment(&input_path, &output, 27, 83, &Limits::default()).unwrap();
            let actual = std::fs::read(&output).unwrap();
            let left = 27 * 24 * 2;
            let right = 110 * 24 * 2;
            assert!(
                actual[44..] == reference[left..right],
                "{name} window differs from C oracle"
            );
            assert_eq!(segment.frames, 83 * 24);
            assert_eq!(segment.actual_end_ms, 110);
            assert!(segment.complete);
        }
        let broken = root.join("broken.silk");
        std::fs::write(&broken, b"#!SILK_V3\x05\x00\x01").unwrap();
        let output = root.join("broken.wav");
        assert!(segment(&broken, &output, 0, 20, &Limits::default()).is_err());
        assert!(!output.exists());
        std::fs::remove_dir_all(root).unwrap();
    }
}
