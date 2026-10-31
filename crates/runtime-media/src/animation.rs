// SPDX-License-Identifier: MIT
//! Single-pass, bounded GIF inspection and composited-frame sampling.
//! No loops are played; zero-delay frames have an explicit sampling policy.
use crate::{BoundedWriter, Limits, MediaError, Result};
use image::{AnimationDecoder, ImageDecoder};
use serde::{Deserialize, Serialize};
#[cfg(unix)]
use std::os::unix::fs::OpenOptionsExt;
use std::{
    fs::{File, OpenOptions},
    io::{BufReader, Read, Seek, SeekFrom, Write},
    path::{Path, PathBuf},
};

const ZERO_DELAY_MS: u64 = 10;

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct GifProbe {
    pub width: u32,
    pub height: u32,
    pub input_bytes: u64,
    pub frame_count: u64,
    /// Sum of the encoded frame delays, without applying a viewer policy.
    pub declared_duration_ms: u64,
    pub sampling_duration_ms: u64,
    pub zero_delay_frames: u64,
    /// The raw Netscape repeat count; 0 denotes infinite repetition.
    pub loop_count: Option<u16>,
    pub loop_forever: bool,
    pub timing_policy: String,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct GifSample {
    pub frame_index: u64,
    pub target_ms: u64,
    pub frame_start_ms: u64,
    pub frame_end_ms: u64,
    pub frame_delay_ms: u64,
    pub sampling_delay_ms: u64,
    pub path: PathBuf,
    pub width: u32,
    pub height: u32,
    pub bytes: u64,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct GifWindow {
    pub source: GifProbe,
    pub requested_start_ms: u64,
    pub requested_end_ms: u64,
    pub frames_decoded: usize,
    pub decoded_until_ms: u64,
    pub samples: Vec<GifSample>,
    pub end_of_animation: bool,
    pub truncated: bool,
    pub truncation_reason: Option<String>,
    pub visual_coverage: String,
}

struct Scanner {
    file: File,
    bytes: u64,
    blocks: usize,
    block_limit: usize,
}
impl Scanner {
    fn byte(&mut self) -> Result<u8> {
        let mut byte = [0];
        self.file.read_exact(&mut byte)?;
        Ok(byte[0])
    }
    fn read<const N: usize>(&mut self) -> Result<[u8; N]> {
        let mut bytes = [0; N];
        self.file.read_exact(&mut bytes)?;
        Ok(bytes)
    }
    fn skip(&mut self, count: u64) -> Result<()> {
        let position = self.file.stream_position()?;
        if position
            .checked_add(count)
            .is_none_or(|end| end > self.bytes)
        {
            return Err(MediaError::Invalid("truncated GIF block".into()));
        }
        self.file.seek(SeekFrom::Start(position + count))?;
        Ok(())
    }
    fn block(&mut self) -> Result<()> {
        self.blocks += 1;
        if self.blocks > self.block_limit {
            return Err(MediaError::Limit("GIF block count".into()));
        }
        Ok(())
    }
    fn sub_blocks(&mut self) -> Result<()> {
        loop {
            self.block()?;
            let count = self.byte()?;
            if count == 0 {
                return Ok(());
            }
            self.skip(u64::from(count))?;
        }
    }
}
fn u16le(bytes: &[u8]) -> u16 {
    u16::from_le_bytes([bytes[0], bytes[1]])
}
fn canvas_bytes(width: u32, height: u32) -> u64 {
    u64::from(width) * u64::from(height) * 4
}

/// Scan frame descriptors/delays and loop metadata without decoding pixel data.
/// A complete trailer is required; no claims are based on a truncated prefix.
pub fn probe_gif(path: &Path, limits: &Limits) -> Result<GifProbe> {
    let bytes = limits.check_file(path)?;
    if bytes > limits.max_decoded_bytes || limits.max_packets == 0 {
        return Err(MediaError::Limit("GIF input or block budget".into()));
    }
    let mut scan = Scanner {
        file: File::open(path)?,
        bytes,
        blocks: 0,
        block_limit: limits.max_packets.min(100_000),
    };
    let header = scan.read::<13>()?;
    if &header[..6] != b"GIF87a" && &header[..6] != b"GIF89a" {
        return Err(MediaError::Unsupported("not a GIF signature".into()));
    }
    let width = u32::from(u16le(&header[6..8]));
    let height = u32::from(u16le(&header[8..10]));
    limits.check_image(width, height, canvas_bytes(width, height))?;
    if header[10] & 0x80 != 0 {
        scan.skip(3 * (1_u64 << ((header[10] & 7) + 1)))?;
    }
    let mut frames = 0_u64;
    let mut declared = 0_u64;
    let mut timeline = 0_u64;
    let mut zero_delays = 0_u64;
    let mut delay = 0_u64;
    let mut loop_count = None;
    loop {
        scan.block()?;
        match scan.byte()? {
            0x3b => break,
            0x2c => {
                let descriptor = scan.read::<9>()?;
                let left = u32::from(u16le(&descriptor[0..2]));
                let top = u32::from(u16le(&descriptor[2..4]));
                let frame_width = u32::from(u16le(&descriptor[4..6]));
                let frame_height = u32::from(u16le(&descriptor[6..8]));
                limits.check_image(
                    frame_width,
                    frame_height,
                    canvas_bytes(frame_width, frame_height),
                )?;
                if left + frame_width > width || top + frame_height > height {
                    return Err(MediaError::Invalid("GIF frame exceeds canvas".into()));
                }
                if descriptor[8] & 0x80 != 0 {
                    scan.skip(3 * (1_u64 << ((descriptor[8] & 7) + 1)))?;
                }
                if !(2..=8).contains(&scan.byte()?) {
                    return Err(MediaError::Invalid("GIF LZW code size".into()));
                }
                scan.sub_blocks()?;
                frames += 1;
                declared = declared
                    .checked_add(delay)
                    .ok_or_else(|| MediaError::Limit("GIF duration overflow".into()))?;
                timeline = timeline
                    .checked_add(if delay == 0 { ZERO_DELAY_MS } else { delay })
                    .ok_or_else(|| MediaError::Limit("GIF timeline overflow".into()))?;
                zero_delays += u64::from(delay == 0);
                delay = 0;
            }
            0x21 => match scan.byte()? {
                0xf9 => {
                    if scan.byte()? != 4 {
                        return Err(MediaError::Invalid("GIF control block size".into()));
                    }
                    let control = scan.read::<4>()?;
                    delay = u64::from(u16le(&control[1..3])) * 10;
                    if scan.byte()? != 0 {
                        return Err(MediaError::Invalid("GIF control terminator".into()));
                    }
                }
                0xff => {
                    let count = scan.byte()?;
                    let mut identifier = vec![0; usize::from(count)];
                    scan.file.read_exact(&mut identifier)?;
                    let repeat = identifier == b"NETSCAPE2.0" || identifier == b"ANIMEXTS1.0";
                    loop {
                        scan.block()?;
                        let count = scan.byte()?;
                        if count == 0 {
                            break;
                        }
                        if repeat && count == 3 {
                            let data = scan.read::<3>()?;
                            if data[0] == 1 {
                                loop_count = Some(u16le(&data[1..3]));
                            }
                        } else {
                            scan.skip(u64::from(count))?;
                        }
                    }
                }
                0x01 => {
                    return Err(MediaError::Unsupported(
                        "GIF plain-text rendering extension".into(),
                    ));
                }
                _ => scan.sub_blocks()?,
            },
            _ => return Err(MediaError::Invalid("unknown GIF block".into())),
        }
    }
    if frames == 0 {
        return Err(MediaError::Invalid("GIF has no image frames".into()));
    }
    Ok(GifProbe { width, height, input_bytes: bytes, frame_count: frames,
        declared_duration_ms: declared, sampling_duration_ms: timeline, zero_delay_frames: zero_delays,
        loop_count, loop_forever: loop_count == Some(0),
        timing_policy: "Encoded delays retained; zero delays use 10 ms only for this sampling timeline; single pass, no loop playback".into() })
}

#[derive(Default)]
struct Cleanup {
    paths: Vec<PathBuf>,
    committed: bool,
}
impl Drop for Cleanup {
    fn drop(&mut self) {
        if !self.committed {
            for path in &self.paths {
                let _ = std::fs::remove_file(path);
            }
        }
    }
}

fn write_sample(
    image: &image::DynamicImage,
    path: &Path,
    remaining: u64,
    cleanup: &mut Cleanup,
) -> Result<u64> {
    let mut options = OpenOptions::new();
    options.write(true).create_new(true);
    #[cfg(unix)]
    options.mode(0o600);
    let file = options.open(path)?;
    // Register only after create_new succeeded: failures preserve existing files,
    // while later encoder/flush failures remove our newly created output.
    cleanup.paths.push(path.to_owned());
    let mut writer = BoundedWriter {
        inner: file,
        bytes: 0,
        limit: remaining,
    };
    image::codecs::jpeg::JpegEncoder::new_with_quality(&mut writer, 82)
        .encode_image(image)
        .map_err(|e| MediaError::Decode(e.to_string()))?;
    writer.flush()?;
    Ok(writer.bytes)
}

/// Decode one GIF pass in order, keep only one emitted composite at a time, and
/// save only frames selected by the requested sample grid. Long displayed frames
/// are saved once even when several sample targets fall inside them.
pub fn prepare_gif(
    input: &Path,
    output_dir: &Path,
    start_ms: u64,
    duration_ms: u64,
    interval_ms: u64,
    max_edge: u32,
    limits: &Limits,
) -> Result<GifWindow> {
    let end_ms = limits.check_window(start_ms, duration_ms)?;
    if interval_ms == 0
        || duration_ms.div_ceil(interval_ms) > limits.max_frames as u64
        || max_edge == 0
        || max_edge > limits.max_dimension
    {
        return Err(MediaError::Limit("GIF sample count/interval/resize".into()));
    }
    if !std::fs::metadata(output_dir)?.is_dir() {
        return Err(MediaError::Invalid("GIF output directory".into()));
    }
    let source = probe_gif(input, limits)?;
    // The decoder can retain a disposal canvas and transient compositing buffers;
    // reserve four canvases, including our thumbnail/encoding work.
    let per_frame_bytes = canvas_bytes(source.width, source.height);
    if per_frame_bytes.saturating_mul(4) > limits.max_decoded_bytes {
        return Err(MediaError::Limit("GIF compositing memory".into()));
    }
    let mut result = GifWindow { source, requested_start_ms: start_ms, requested_end_ms: end_ms,
        frames_decoded: 0, decoded_until_ms: 0, samples: vec![], end_of_animation: false,
        truncated: false, truncation_reason: None,
        visual_coverage: "Selected composited GIF frames only; transient events can fall between samples; no loop playback".into() };
    if start_ms >= result.source.sampling_duration_ms {
        result.end_of_animation = true;
        return Ok(result);
    }
    let mut decoder = image::codecs::gif::GifDecoder::new(BufReader::new(File::open(input)?))
        .map_err(|e| MediaError::Decode(e.to_string()))?;
    if decoder.dimensions() != (result.source.width, result.source.height) {
        return Err(MediaError::Invalid(
            "GIF canvas changed after inspection".into(),
        ));
    }
    let mut image_limits = image::Limits::default();
    image_limits.max_image_width = Some(limits.max_dimension);
    image_limits.max_image_height = Some(limits.max_dimension);
    image_limits.max_alloc = Some(limits.max_decoded_bytes);
    decoder
        .set_limits(image_limits)
        .map_err(|e| MediaError::Limit(e.to_string()))?;
    let mut frames = decoder.into_frames();
    let mut next_target = start_ms;
    let mut written_bytes = 0_u64;
    let mut cleanup = Cleanup::default();
    while next_target < end_ms {
        if result.frames_decoded >= limits.max_packets.min(4096)
            || per_frame_bytes.saturating_mul(result.frames_decoded as u64 + 1)
                > limits.max_decoded_bytes.saturating_mul(8)
        {
            result.truncated = true;
            result.truncation_reason = Some("GIF decoded frame/work budget".into());
            break;
        }
        let Some(frame) = frames.next() else {
            result.end_of_animation = true;
            break;
        };
        let frame = frame.map_err(|e| MediaError::Decode(e.to_string()))?;
        let (numerator, denominator) = frame.delay().numer_denom_ms();
        if denominator == 0 {
            return Err(MediaError::Invalid("GIF delay denominator".into()));
        }
        let delay_ms = u64::from(numerator) / u64::from(denominator);
        let sampling_delay_ms = if delay_ms == 0 {
            ZERO_DELAY_MS
        } else {
            delay_ms
        };
        let frame_start = result.decoded_until_ms;
        let frame_end = frame_start
            .checked_add(sampling_delay_ms)
            .ok_or_else(|| MediaError::Limit("GIF decoded timeline overflow".into()))?;
        let index = result.frames_decoded as u64;
        result.frames_decoded += 1;
        result.decoded_until_ms = frame_end;
        if next_target < frame_end {
            let edge = max_edge.min(result.source.width.max(result.source.height));
            let image = image::DynamicImage::ImageRgba8(frame.into_buffer()).thumbnail(edge, edge);
            let path = output_dir.join(format!("gif-frame-{index:08}-{frame_start:012}.jpg"));
            let bytes = write_sample(
                &image,
                &path,
                limits.max_output_bytes.saturating_sub(written_bytes),
                &mut cleanup,
            )?;
            written_bytes = written_bytes
                .checked_add(bytes)
                .ok_or_else(|| MediaError::Limit("GIF output bytes".into()))?;
            result.samples.push(GifSample {
                frame_index: index,
                target_ms: next_target,
                frame_start_ms: frame_start,
                frame_end_ms: frame_end,
                frame_delay_ms: delay_ms,
                sampling_delay_ms,
                path,
                width: image.width(),
                height: image.height(),
                bytes,
            });
            // Advance over all grid points represented by this same composite.
            let skipped = (frame_end - 1 - next_target) / interval_ms + 1;
            next_target = next_target
                .checked_add(
                    skipped
                        .checked_mul(interval_ms)
                        .ok_or_else(|| MediaError::Limit("GIF sample timestamp overflow".into()))?,
                )
                .ok_or_else(|| MediaError::Limit("GIF sample timestamp overflow".into()))?;
        }
        if result.frames_decoded as u64 == result.source.frame_count {
            result.end_of_animation = true;
            break;
        }
    }
    cleanup.committed = true;
    Ok(result)
}

#[cfg(test)]
mod tests {
    use super::*;
    use image::{
        Delay, Frame, Rgba, RgbaImage,
        codecs::gif::{GifEncoder, Repeat},
    };
    use std::time::{SystemTime, UNIX_EPOCH};
    fn lab() -> PathBuf {
        let path = std::env::temp_dir().join(format!(
            "gif-test-{}-{}",
            std::process::id(),
            SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        std::fs::create_dir(&path).unwrap();
        path
    }
    fn fixture(path: &Path, delays: &[u32]) {
        fixture_size(path, delays, 8);
    }
    fn fixture_size(path: &Path, delays: &[u32], edge: u32) {
        let mut encoder = GifEncoder::new(File::create(path).unwrap());
        encoder.set_repeat(Repeat::Infinite).unwrap();
        for (i, delay) in delays.iter().enumerate() {
            let colour = [[255, 0, 0, 255], [0, 255, 0, 255], [0, 0, 255, 255]][i % 3];
            encoder
                .encode_frame(Frame::from_parts(
                    RgbaImage::from_pixel(edge, edge, Rgba(colour)),
                    0,
                    0,
                    Delay::from_numer_denom_ms(*delay, 1),
                ))
                .unwrap();
        }
    }
    #[test]
    fn gif_samples_follow_real_frame_delays_and_do_not_replay_infinite_loops() {
        let directory = lab();
        let path = directory.join("sample.gif");
        fixture(&path, &[100, 200, 300]);
        let limits = Limits::default();
        let probe = probe_gif(&path, &limits).unwrap();
        assert_eq!(probe.frame_count, 3);
        assert_eq!(probe.declared_duration_ms, 600);
        assert!(probe.loop_forever);
        assert_eq!(probe.loop_count, Some(0));
        let window = prepare_gif(&path, &directory, 50, 700, 100, 8, &limits).unwrap();
        assert_eq!(window.samples.len(), 3);
        assert_eq!(window.frames_decoded, 3);
        assert!(window.end_of_animation && !window.truncated);
        assert_eq!(
            window
                .samples
                .iter()
                .map(|s| s.target_ms)
                .collect::<Vec<_>>(),
            vec![50, 150, 350]
        );
        assert_eq!(
            window
                .samples
                .iter()
                .map(|s| s.frame_start_ms)
                .collect::<Vec<_>>(),
            vec![0, 100, 300]
        );
        for (index, sample) in window.samples.iter().enumerate() {
            let image = image::open(&sample.path).unwrap().to_rgb8();
            let pixel = image.get_pixel(3, 3);
            assert!(pixel[index] > 240);
            assert!(pixel[(index + 1) % 3] < 15);
        }
        std::fs::remove_dir_all(directory).unwrap();
    }
    #[test]
    fn gif_zero_delay_policy_work_limits_and_failure_cleanup_remain_explicit() {
        let directory = lab();
        let input = directory.join("zero.gif");
        fixture(&input, &[0, 100, 100]);
        let probe = probe_gif(&input, &Limits::default()).unwrap();
        assert_eq!(probe.declared_duration_ms, 200);
        assert_eq!(probe.sampling_duration_ms, 210);
        assert_eq!(probe.zero_delay_frames, 1);
        let output = directory.join("out");
        std::fs::create_dir(&output).unwrap();
        let mut limits = Limits::default();
        limits.max_output_bytes = 1;
        assert!(prepare_gif(&input, &output, 0, 200, 10, 8, &limits).is_err());
        assert_eq!(std::fs::read_dir(&output).unwrap().count(), 0);
        let mut limits = Limits::default();
        limits.max_decoded_bytes = 256;
        assert!(prepare_gif(&input, &output, 0, 200, 10, 8, &limits).is_err());
        let mut bytes = std::fs::read(&input).unwrap();
        bytes.pop();
        let broken = directory.join("truncated.gif");
        std::fs::write(&broken, bytes).unwrap();
        assert!(probe_gif(&broken, &Limits::default()).is_err());
        std::fs::remove_dir_all(directory).unwrap();
    }
    #[test]
    fn gif_decoding_work_cap_returns_a_truncated_window_and_preserves_existing_outputs() {
        let directory = lab();
        let input = directory.join("long.gif");
        fixture_size(&input, &vec![100; 50], 32);
        let output = directory.join("out");
        std::fs::create_dir(&output).unwrap();
        let mut limits = Limits::default();
        limits.max_decoded_bytes = 32 * 32 * 4 * 4;
        let window = prepare_gif(&input, &output, 0, 5000, 100, 32, &limits).unwrap();
        assert_eq!(window.source.frame_count, 50);
        assert_eq!(window.frames_decoded, 32);
        assert_eq!(window.decoded_until_ms, 3200);
        assert!(window.truncated && !window.end_of_animation);
        assert_eq!(window.samples.len(), 32);
        let before = std::fs::read(&window.samples[0].path).unwrap();
        assert!(prepare_gif(&input, &output, 0, 5000, 100, 32, &limits).is_err());
        assert_eq!(std::fs::read(&window.samples[0].path).unwrap(), before);
        std::fs::remove_dir_all(directory).unwrap();
    }
}
