// SPDX-License-Identifier: MIT
use crate::{Limits, MediaError, Result};
use serde::{Deserialize, Serialize};
#[cfg(unix)]
use std::os::unix::fs::OpenOptionsExt;
use std::{
    fs::{File, OpenOptions},
    io::{Read, Seek, SeekFrom, Write},
    path::{Path, PathBuf},
};
use symphonia::core::{
    audio::SampleBuffer,
    codecs::DecoderOptions,
    errors::Error,
    formats::{FormatOptions, FormatReader, SeekMode, SeekTo},
    io::{MediaSourceStream, MediaSourceStreamOptions},
    meta::MetadataOptions,
    probe::Hint,
    units::Time,
};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AudioProbe {
    pub codec: String,
    pub track_id: u32,
    pub sample_rate: u32,
    pub channels: usize,
    pub duration_ms: Option<u64>,
}
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AudioSegment {
    pub probe: AudioProbe,
    pub path: PathBuf,
    pub requested_start_ms: u64,
    pub requested_end_ms: u64,
    pub actual_start_ms: u64,
    pub actual_end_ms: u64,
    pub frames: u64,
    pub bytes: u64,
    pub complete: bool,
    pub gaps: Vec<String>,
}
fn error(e: Error) -> MediaError {
    match e {
        Error::Unsupported(s) => MediaError::Unsupported(s.into()),
        _ => MediaError::Decode(e.to_string()),
    }
}
fn open(path: &Path, limits: &Limits) -> Result<Box<dyn FormatReader>> {
    limits.check_file(path)?;
    let mut file = File::open(path)?;
    let mut magic = [0; 16];
    let n = file.read(&mut magic)?;
    file.seek(SeekFrom::Start(0))?;
    if magic[..n].starts_with(b"#!SILK")
        || magic[..n].starts_with(b"\x02#!SILK")
        || magic[..n].starts_with(b"#!AMR")
    {
        return Err(MediaError::Unsupported(
            "SILK/AMR decoder is not available".into(),
        ));
    }
    if n >= 8 && (&magic[4..8] == b"ftyp" || &magic[4..8] == b"styp") {
        crate::video::validate_container(path, limits)?;
    }
    let mut hint = Hint::new();
    if let Some(ext) = path.extension().and_then(|s| s.to_str()) {
        hint.with_extension(ext);
    }
    let stream = MediaSourceStream::new(
        Box::new(file),
        MediaSourceStreamOptions {
            buffer_len: 64 * 1024,
        },
    );
    let metadata = MetadataOptions {
        limit_metadata_bytes: symphonia::core::meta::Limit::Maximum(64 * 1024),
        limit_visual_bytes: symphonia::core::meta::Limit::Maximum(256 * 1024),
    };
    let probed = symphonia::default::get_probe()
        .format(&hint, stream, &FormatOptions::default(), &metadata)
        .map_err(error)?;
    Ok(probed.format)
}
fn select(
    format: &dyn FormatReader,
) -> Result<(AudioProbe, symphonia::core::codecs::CodecParameters)> {
    let t = format
        .tracks()
        .iter()
        .find(|t| {
            t.codec_params.sample_rate.is_some()
                && t.codec_params.codec != symphonia::core::codecs::CODEC_TYPE_NULL
        })
        .ok_or_else(|| MediaError::Unsupported("no supported audio track".into()))?;
    let p = &t.codec_params;
    // MP4 AAC AudioSpecificConfig supplies the channel layout at decoder initialization;
    // Symphonia 0.5 does not copy that layout into CodecParameters.
    let (rate, channels) = if let Some(ch) = p.channels {
        (p.sample_rate.unwrap(), ch.count())
    } else {
        let decoder = symphonia::default::get_codecs()
            .make(p, &DecoderOptions::default())
            .map_err(error)?;
        let audio = decoder.last_decoded();
        let spec = audio.spec();
        (spec.rate, spec.channels.count())
    };
    if rate == 0 || rate > 96_000 || channels == 0 || channels > 2 {
        return Err(MediaError::Limit(
            "audio rate/channels (max 96 kHz, stereo)".into(),
        ));
    }
    let duration_ms = p.n_frames.filter(|&n| n > 0).and_then(|n| {
        p.time_base.map(|tb| {
            let time = tb.calc_time(n);
            time.seconds.saturating_mul(1000) + (time.frac * 1000.0) as u64
        })
    });
    let name = symphonia::default::get_codecs()
        .get_codec(p.codec)
        .map(|d| d.short_name.to_string())
        .unwrap_or_else(|| format!("{:?}", p.codec));
    Ok((
        AudioProbe {
            codec: name,
            track_id: t.id,
            sample_rate: rate,
            channels,
            duration_ms,
        },
        p.clone(),
    ))
}
pub fn probe_audio(path: &Path, limits: &Limits) -> Result<AudioProbe> {
    if crate::silk::is_silk(path)? {
        return crate::silk::probe(path, limits);
    }
    if crate::amr::is_amr(path)? {
        return crate::amr::probe(path, limits);
    }
    let format = open(path, limits)?;
    let (mut probe, _) = select(format.as_ref())?;
    if is_mp4(path)? {
        let index = crate::mp4::index(path, limits)?;
        if index.fragmented && index.duration_ms > 0 {
            probe.duration_ms = Some(index.duration_ms);
        }
    }
    Ok(probe)
}
fn is_mp4(path: &Path) -> Result<bool> {
    let mut file = File::open(path)?;
    let mut head = [0; 8];
    let n = file.read(&mut head)?;
    Ok(n == 8 && (&head[4..] == b"ftyp" || &head[4..] == b"styp"))
}

pub(crate) fn wav_header(file: &mut File, rate: u32, channels: usize, bytes: u64) -> Result<()> {
    let n = u32::try_from(bytes).map_err(|_| MediaError::Limit("WAV RIFF length".into()))?;
    file.seek(SeekFrom::Start(0))?;
    file.write_all(b"RIFF")?;
    file.write_all(
        &n.checked_add(36)
            .ok_or_else(|| MediaError::Limit("WAV RIFF length".into()))?
            .to_le_bytes(),
    )?;
    file.write_all(b"WAVEfmt ")?;
    file.write_all(&16_u32.to_le_bytes())?;
    file.write_all(&1_u16.to_le_bytes())?;
    file.write_all(&(channels as u16).to_le_bytes())?;
    file.write_all(&rate.to_le_bytes())?;
    file.write_all(&(rate * channels as u32 * 2).to_le_bytes())?;
    file.write_all(&(channels as u16 * 2).to_le_bytes())?;
    file.write_all(&16_u16.to_le_bytes())?;
    file.write_all(b"data")?;
    file.write_all(&n.to_le_bytes())?;
    Ok(())
}

pub fn segment_audio_wav(
    input: &Path,
    output: &Path,
    start_ms: u64,
    duration_ms: u64,
    limits: &Limits,
) -> Result<AudioSegment> {
    if crate::silk::is_silk(input)? {
        return crate::silk::segment(input, output, start_ms, duration_ms, limits);
    }
    if crate::amr::is_amr(input)? {
        return crate::amr::segment(input, output, start_ms, duration_ms, limits);
    }
    let end_ms = limits.check_window(start_ms, duration_ms)?;
    let mut format = open(input, limits)?;
    let (mut probe, params) = select(format.as_ref())?;
    let mut decoder = symphonia::default::get_codecs()
        .make(&params, &DecoderOptions::default())
        .map_err(error)?;
    let mut time_base = params
        .time_base
        .ok_or_else(|| MediaError::Unsupported("audio track lacks time base".into()))?;
    let mut indexed = None;
    if is_mp4(input)? {
        let index = crate::mp4::index(input, limits)?;
        if index.fragmented {
            probe.duration_ms = Some(index.duration_ms);
            let track = index
                .tracks
                .into_values()
                .find(|t| t.audio)
                .ok_or_else(|| MediaError::Unsupported("no clear MP4 audio track".into()))?;
            let position = track
                .samples
                .iter()
                .position(|s| {
                    u128::from(s.pts.saturating_add(u64::from(s.duration))) * 1000
                        > u128::from(start_ms) * u128::from(track.timescale)
                })
                .unwrap_or(track.samples.len())
                .saturating_sub(2);
            time_base = symphonia::core::units::TimeBase::new(1, track.timescale);
            indexed = Some((track, position, File::open(input)?));
        }
    }
    if start_ms > 0 && indexed.is_none() {
        format
            .seek(
                SeekMode::Accurate,
                SeekTo::Time {
                    time: Time::new(start_ms / 1000, (start_ms % 1000) as f64 / 1000.0),
                    track_id: Some(probe.track_id),
                },
            )
            .map_err(error)?;
        decoder.reset();
    }
    let max_frames = duration_ms
        .checked_mul(u64::from(probe.sample_rate))
        .ok_or_else(|| MediaError::Limit("audio frames".into()))?
        / 1000;
    let max_bytes = max_frames
        .checked_mul(probe.channels as u64 * 2)
        .and_then(|n| n.checked_add(44))
        .ok_or_else(|| MediaError::Limit("audio bytes".into()))?;
    if max_bytes > limits.max_output_bytes {
        return Err(MediaError::Limit("requested WAV bytes".into()));
    }
    let mut options = OpenOptions::new();
    options.write(true).create_new(true);
    #[cfg(unix)]
    options.mode(0o600);
    let mut out = options.open(output)?;
    wav_header(&mut out, probe.sample_rate, probe.channels, 0)?;
    let processing = (|| -> Result<AudioSegment> {
        let mut frames = 0_u64;
        let mut actual_start = None;
        let mut actual_end = 0_u64;
        let mut gaps = Vec::new();
        let mut packets = 0;
        while frames < max_frames {
            let packet = if let Some((track, position, file)) = indexed.as_mut() {
                let Some(s) = track.samples.get(*position) else {
                    break;
                };
                *position += 1;
                file.seek(SeekFrom::Start(s.offset))?;
                let mut data = vec![0; s.size as usize];
                file.read_exact(&mut data)?;
                symphonia::core::formats::Packet::new_from_boxed_slice(
                    probe.track_id,
                    s.pts,
                    u64::from(s.duration),
                    data.into_boxed_slice(),
                )
            } else {
                match format.next_packet() {
                    Ok(p) => p,
                    Err(Error::IoError(e)) if e.kind() == std::io::ErrorKind::UnexpectedEof => {
                        break;
                    }
                    Err(e) => return Err(error(e)),
                }
            };
            packets += 1;
            if packets > limits.max_packets {
                return Err(MediaError::Limit("audio packet count".into()));
            }
            if packet.track_id() != probe.track_id {
                continue;
            }
            let time = time_base.calc_time(packet.ts());
            let packet_ms = time.seconds.saturating_mul(1000) + (time.frac * 1000.0) as u64;
            if packet_ms >= end_ms {
                break;
            }
            let audio = decoder.decode(&packet).map_err(error)?;
            let spec = *audio.spec();
            let channels = spec.channels.count();
            if spec.rate != probe.sample_rate || channels != probe.channels {
                return Err(MediaError::Unsupported(
                    "audio format changed mid-stream".into(),
                ));
            }
            let alloc = (audio.capacity() as u64)
                .checked_mul(channels as u64 * 2)
                .ok_or_else(|| MediaError::Limit("audio buffer overflow".into()))?;
            if alloc > limits.max_decoded_bytes || audio.frames() > limits.max_samples {
                return Err(MediaError::Limit("decoded audio packet".into()));
            }
            let mut samples = SampleBuffer::<i16>::new(audio.capacity() as u64, spec);
            samples.copy_interleaved_ref(audio);
            let skip = if packet_ms < start_ms {
                ((start_ms - packet_ms) * u64::from(spec.rate) / 1000) as usize
            } else {
                0
            };
            let available = samples.len() / channels;
            if skip >= available {
                continue;
            }
            let take = (available - skip).min((max_frames - frames) as usize);
            let from = packet_ms + skip as u64 * 1000 / u64::from(spec.rate);
            if let Some(_) = actual_start {
                if from > actual_end + 3 {
                    gaps.push(format!("audio timestamp gap {actual_end}..{from} ms"));
                }
            } else {
                actual_start = Some(from);
            }
            let mut pcm = Vec::with_capacity(take * channels * 2);
            for value in &samples.samples()[skip * channels..(skip + take) * channels] {
                pcm.extend_from_slice(&value.to_le_bytes());
            }
            out.write_all(&pcm)?;
            frames += take as u64;
            actual_end = packet_ms + (skip + take) as u64 * 1000 / u64::from(spec.rate);
        }
        if frames == 0 {
            return Err(MediaError::Decode(
                "no audio samples in requested window".into(),
            ));
        }
        let bytes = frames * probe.channels as u64 * 2;
        wav_header(&mut out, probe.sample_rate, probe.channels, bytes)?;
        out.flush()?;
        let complete =
            actual_start.unwrap() <= start_ms + 2 && actual_end + 2 >= end_ms && gaps.is_empty();
        if !complete {
            gaps.push(format!(
                "requested {start_ms}..{end_ms} ms, decoded {}..{actual_end} ms",
                actual_start.unwrap()
            ));
        }
        Ok(AudioSegment {
            probe,
            path: output.into(),
            requested_start_ms: start_ms,
            requested_end_ms: end_ms,
            actual_start_ms: actual_start.unwrap(),
            actual_end_ms: actual_end,
            frames,
            bytes: bytes + 44,
            complete,
            gaps,
        })
    })();
    if processing.is_err() {
        drop(out);
        let _ = std::fs::remove_file(output);
    }
    processing
}

/// A complete ordinary audio clip -> native QQ AMR-NB voice in one bounded pass.
#[derive(Debug, Serialize, Deserialize)]
pub struct EncodedVoice {
    pub duration_ms: u64,
    pub source_duration_ms: u64,
    pub bytes: u64,
    pub codec: String,
}
pub fn encode_qq_voice(input: &Path, output: &Path, limits: &Limits) -> Result<EncodedVoice> {
    use runtime_amr_codec::{
        codecs::amr::AmrCodec,
        types::{AudioCodec, CodecConfig, CodecType},
    };
    let probe = probe_audio(input, limits)?;
    if probe.duration_ms.is_some_and(|d| d > 120000) {
        return Err(MediaError::Limit(
            "QQ voice must be at most120 seconds".into(),
        ));
    }
    let mut opts = OpenOptions::new();
    opts.write(true).create_new(true);
    #[cfg(unix)]
    opts.mode(0o600);
    let mut out = opts.open(output)?;
    let work = output.with_extension("voice-work.wav");
    let processing = (|| {
        out.write_all(b"#!AMR\n")?;
        let mut encoder =
            AmrCodec::new(&CodecConfig::new(CodecType::AmrNb).with_amr_mode_set(&[7]))
                .map_err(|e| MediaError::Decode(e.to_string()))?;
        let mut total_source = 0u64;
        let mut pending = Vec::with_capacity(160);
        let mut encoded_frames = 0u64;
        let mut start = 0u64;
        loop {
            if start >= 120000 {
                break;
            }
            let duration = probe
                .duration_ms
                .map(|d| (d.saturating_sub(start)).min(60000))
                .unwrap_or(60000);
            if duration == 0 {
                break;
            }
            let segment = segment_audio_wav(input, &work, start, duration, limits)?;
            let source = std::fs::read(&work)?;
            std::fs::remove_file(&work)?;
            if source.len() < 44 || source.len() as u64 > limits.max_output_bytes {
                return Err(MediaError::Invalid("PCM conversion output".into()));
            }
            let rate = u64::from(segment.probe.sample_rate);
            let channels = segment.probe.channels;
            let frames = (source.len() - 44) / (channels * 2);
            let wanted = (frames as u64 * 8000 / rate) as usize;
            let mono = |frame: usize| -> f64 {
                let sum = (0..channels)
                    .map(|c| {
                        let at = 44 + (frame.min(frames - 1) * channels + c) * 2;
                        f64::from(i16::from_le_bytes([source[at], source[at + 1]]))
                    })
                    .sum::<f64>();
                sum / channels as f64
            };
            for index in 0..wanted {
                let position = index as f64 * rate as f64 / 8000.;
                let left = position as usize;
                let fraction = position - left as f64;
                let value = (mono(left) * (1. - fraction) + mono(left + 1) * fraction)
                    .round()
                    .clamp(-32768., 32767.) as i16;
                pending.push(value);
                if pending.len() == 160 {
                    let packet = encoder
                        .encode(&pending)
                        .map_err(|e| MediaError::Decode(e.to_string()))?;
                    out.write_all(&[0x3c])?;
                    out.write_all(&packet)?;
                    pending.clear();
                    encoded_frames += 1;
                }
            }
            total_source += frames as u64 * 1000 / rate;
            if segment.actual_end_ms < start + duration {
                break;
            }
            start += duration;
        }
        if total_source == 0 {
            return Err(MediaError::Decode("voice clip contains no audio".into()));
        }
        if !pending.is_empty() {
            pending.resize(160, 0);
            out.write_all(&[0x3c])?;
            out.write_all(
                &encoder
                    .encode(&pending)
                    .map_err(|e| MediaError::Decode(e.to_string()))?,
            )?;
            encoded_frames += 1;
        }
        if probe.duration_ms.is_none() && total_source >= 120000 {
            // A missing duration must not silently truncate a longer clip.
            match segment_audio_wav(input, &work, 120000, 20, limits) {
                Ok(extra) if extra.frames > 0 => {
                    return Err(MediaError::Limit("QQ voice exceeds120 seconds".into()));
                }
                _ => {}
            }
        }
        let bytes = out.stream_position()?;
        if bytes > limits.max_output_bytes {
            return Err(MediaError::Limit("AMR voice output bytes".into()));
        }
        out.sync_all()?;
        Ok(EncodedVoice {
            duration_ms: encoded_frames * 20,
            source_duration_ms: total_source,
            bytes,
            codec: "amr-nb-mr122".into(),
        })
    })();
    let _ = std::fs::remove_file(&work);
    if processing.is_err() {
        drop(out);
        let _ = std::fs::remove_file(output);
    }
    processing
}
#[cfg(test)]
mod voice_tests {
    use super::*;
    #[test]
    fn qq_voice_encoder_matches_independent_reference_bits() {
        let dir = std::env::temp_dir().join(format!("qq-voice-encode-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir(&dir).unwrap();
        let input = include_bytes!("../tests/vectors/amr-original/nb-input.pcm");
        let wav = dir.join("source.wav");
        let mut file = File::create(&wav).unwrap();
        wav_header(&mut file, 8000, 1, input.len() as u64).unwrap();
        file.write_all(input).unwrap();
        drop(file);
        let output = dir.join("voice.amr");
        let encoded = encode_qq_voice(&wav, &output, &Limits::default()).unwrap();
        assert!(
            std::fs::read(&output).unwrap().as_slice()
                == include_bytes!("../tests/vectors/amr-original/nb-mr122.amr")
        );
        assert_eq!(encoded.duration_ms, 1000);
        assert_eq!(encoded.codec, "amr-nb-mr122");
        std::fs::remove_dir_all(dir).unwrap();
    }
}
