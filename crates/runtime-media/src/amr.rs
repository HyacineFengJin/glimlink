// SPDX-License-Identifier: MIT
//! Streaming AMR storage decoding, fixed memory and one stateful Rust decoder.
use crate::{
    Limits, MediaError, Result,
    audio::{AudioProbe, AudioSegment},
};
use runtime_amr_codec::{
    codecs::amr::{AmrCodec, AmrFrameType, AmrVariant},
    types::{CodecConfig, CodecType, CodedFrame, FrameKind, VariableRateCodec},
};
#[cfg(unix)]
use std::os::unix::fs::OpenOptionsExt;
use std::{
    fs::{File, OpenOptions},
    io::{Read, Seek, SeekFrom, Write},
    path::Path,
};
fn codec_error(e: impl std::fmt::Display) -> MediaError {
    MediaError::Decode(e.to_string())
}
fn header(file: &mut File) -> Result<(AmrVariant, u64)> {
    file.seek(SeekFrom::Start(0))?;
    let mut magic = Vec::new();
    for _ in 0..16 {
        let mut b = [0u8];
        if file.read(&mut b)? == 0 {
            break;
        }
        magic.push(b[0]);
        if b[0] == b'\n' {
            break;
        }
    }
    let variant = match magic.as_slice() {
        b"#!AMR\n" => AmrVariant::NarrowBand,
        b"#!AMR-WB\n" => AmrVariant::WideBand,
        _ => {
            return Err(MediaError::Unsupported(
                "AMR requires single-channel RFC4867 storage framing".into(),
            ));
        }
    };
    Ok((variant, magic.len() as u64))
}
fn frame(file: &mut File, variant: AmrVariant, last_mode: &mut u8) -> Result<Option<CodedFrame>> {
    let mut header = [0u8];
    if file.read(&mut header)? == 0 {
        return Ok(None);
    }
    if header[0] & 0x83 != 0 {
        return Err(MediaError::Invalid("AMR record padding".into()));
    }
    let kind = AmrFrameType::from_index(variant, header[0] >> 3 & 15).map_err(codec_error)?;
    let mut data = vec![0u8; kind.octet_aligned_bytes()];
    file.read_exact(&mut data)?;
    let kind = match kind {
        AmrFrameType::Speech(mode) => {
            *last_mode = mode.index();
            FrameKind::Speech
        }
        AmrFrameType::Sid(_) => FrameKind::ComfortNoise,
        AmrFrameType::NoData => FrameKind::NoData,
        AmrFrameType::SpeechLost => FrameKind::Lost,
    };
    Ok(Some(CodedFrame {
        kind,
        mode: *last_mode,
        quality_ok: header[0] & 4 != 0,
        data,
    }))
}
pub fn is_amr(path: &Path) -> Result<bool> {
    let mut file = File::open(path)?;
    let mut b = [0; 6];
    let n = file.read(&mut b)?;
    Ok(b[..n].starts_with(b"#!AMR"))
}
fn inspect(path: &Path, limits: &Limits) -> Result<(AudioProbe, AmrVariant, u64)> {
    limits.check_file(path)?;
    let mut file = File::open(path)?;
    let (variant, offset) = header(&mut file)?;
    let mut count = 0usize;
    let mut mode = 0;
    while frame(&mut file, variant, &mut mode)?.is_some() {
        count += 1;
        if count > limits.max_packets {
            return Err(MediaError::Limit("AMR record count".into()));
        }
    }
    Ok((
        AudioProbe {
            codec: if variant == AmrVariant::NarrowBand {
                "amr-nb"
            } else {
                "amr-wb"
            }
            .into(),
            track_id: 0,
            sample_rate: variant.sample_rate(),
            channels: 1,
            duration_ms: Some(count as u64 * 20),
        },
        variant,
        offset,
    ))
}
pub fn probe(path: &Path, limits: &Limits) -> Result<AudioProbe> {
    Ok(inspect(path, limits)?.0)
}
pub fn segment(
    input: &Path,
    output: &Path,
    start_ms: u64,
    duration_ms: u64,
    limits: &Limits,
) -> Result<AudioSegment> {
    let end_ms = limits.check_window(start_ms, duration_ms)?;
    let (probe, variant, offset) = inspect(input, limits)?;
    let rate = u64::from(probe.sample_rate);
    let begin = start_ms
        .checked_mul(rate)
        .ok_or_else(|| MediaError::Limit("AMR timestamp".into()))?
        / 1000;
    let end = end_ms
        .checked_mul(rate)
        .ok_or_else(|| MediaError::Limit("AMR timestamp".into()))?
        / 1000;
    let max_bytes = (end - begin)
        .checked_mul(2)
        .and_then(|n| n.checked_add(44))
        .ok_or_else(|| MediaError::Limit("AMR output bytes".into()))?;
    if max_bytes > limits.max_output_bytes {
        return Err(MediaError::Limit("AMR output bytes".into()));
    }
    let mut options = OpenOptions::new();
    options.write(true).create_new(true);
    #[cfg(unix)]
    options.mode(0o600);
    let mut out = options.open(output)?;
    let work = (|| {
        crate::audio::wav_header(&mut out, probe.sample_rate, 1, 0)?;
        let mut source = File::open(input)?;
        source.seek(SeekFrom::Start(offset))?;
        let config = CodecConfig::new(if variant == AmrVariant::NarrowBand {
            CodecType::AmrNb
        } else {
            CodecType::AmrWb
        });
        let mut decoder = AmrCodec::new(&config).map_err(codec_error)?;
        let mut last_mode = 0;
        let mut position = 0u64;
        let mut frames = 0u64;
        let mut gaps = Vec::new();
        let mut packets = 0;
        while position < end {
            let Some(packet) = frame(&mut source, variant, &mut last_mode)? else {
                break;
            };
            packets += 1;
            if packets > limits.max_packets {
                return Err(MediaError::Limit("AMR decoding packet budget".into()));
            }
            if !packet.quality_ok && gaps.len() < 128 {
                gaps.push(format!(
                    "damaged AMR frame at {}ms; codec concealment applied",
                    position * 1000 / rate
                ))
            }
            let pcm = decoder.decode_frame(&packet).map_err(codec_error)?;
            let left = begin.saturating_sub(position).min(pcm.len() as u64) as usize;
            let right = end.saturating_sub(position).min(pcm.len() as u64) as usize;
            for sample in &pcm[left..right] {
                // TS26.073/26.173 storage output is 13-/14-bit linear PCM.
                let sample = sample
                    & if variant == AmrVariant::NarrowBand {
                        !7
                    } else {
                        !3
                    };
                out.write_all(&sample.to_le_bytes())?;
                frames += 1;
            }
            position += pcm.len() as u64;
        }
        if frames == 0 {
            return Err(MediaError::Decode(
                "no AMR audio in requested window".into(),
            ));
        }
        let bytes = frames * 2;
        crate::audio::wav_header(&mut out, probe.sample_rate, 1, bytes)?;
        out.sync_all()?;
        let actual_end = start_ms + frames * 1000 / rate;
        Ok(AudioSegment {
            probe,
            path: output.into(),
            requested_start_ms: start_ms,
            requested_end_ms: end_ms,
            actual_start_ms: start_ms,
            actual_end_ms: actual_end,
            frames,
            bytes: bytes + 44,
            complete: actual_end >= end_ms,
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
    fn native_amr_windows_match_independent_reference_pcm_and_preserve_decoder_history() {
        let root = std::env::temp_dir().join(format!("amr-test-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        std::fs::create_dir(&root).unwrap();
        for (name, input, reference, rate) in [
            (
                "nb",
                include_bytes!("../tests/vectors/amr-original/nb-mr122.amr")
                    .as_slice(),
                include_bytes!("../tests/vectors/amr-original/nb-mr122.pcm")
                    .as_slice(),
                8000u64,
            ),
            (
                "wb",
                include_bytes!("../tests/vectors/amr-original/wb-mode4.amr")
                    .as_slice(),
                include_bytes!("../tests/vectors/amr-original/wb-mode4.pcm")
                    .as_slice(),
                16000u64,
            ),
        ] {
            let path = root.join(format!("{name}.amr"));
            std::fs::write(&path, input).unwrap();
            let output = root.join(format!("{name}.wav"));
            let result = segment(&path, &output, 27, 83, &Limits::default()).unwrap();
            let wav = std::fs::read(output).unwrap();
            let left = (27 * rate / 1000 * 2) as usize;
            let right = (110 * rate / 1000 * 2) as usize;
            assert!(
                wav[44..] == reference[left..right],
                "{name} PCM differs from reference"
            );
            assert_eq!(result.actual_start_ms, 27);
            assert_eq!(result.actual_end_ms, 110);
        }
        let truncated = root.join("bad.amr");
        std::fs::write(&truncated, b"#!AMR\n\x04\x01").unwrap();
        let output = root.join("bad.wav");
        assert!(segment(&truncated, &output, 0, 100, &Limits::default()).is_err());
        assert!(!output.exists());
        std::fs::remove_dir_all(root).unwrap();
    }
}
