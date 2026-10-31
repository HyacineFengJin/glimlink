// SPDX-License-Identifier: MIT
//! Disk-backed MP4/H.264 decoding using a pinned pure-Rust NihAV snapshot.
use crate::{
    Limits, MediaError, Result,
    pictures::{OutputFormat, write_image},
};
use nihav_core::{
    codecs::{NADecoderSupport, RegisteredDecoders},
    demuxers::{DemuxerError, RegisteredDemuxers, create_demuxer},
    formats::YUV420_FORMAT,
    frame::{
        NACodecInfo, NACodecTypeInfo, NAPacket, NAStream, NAStreamRef, NATimePoint, NAVideoInfo,
        StreamType,
    },
    io::byteio::FileReader,
};
use serde::{Deserialize, Serialize};
use std::{
    collections::BTreeMap,
    fs::File,
    io::{Read, Seek, SeekFrom},
    path::{Path, PathBuf},
};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct VideoTrack {
    pub id: u32,
    pub codec: String,
    pub width: Option<u32>,
    pub height: Option<u32>,
    pub audio: bool,
}
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct VideoProbe {
    pub duration_ms: u64,
    pub tracks: Vec<VideoTrack>,
    pub sample_table_entries: u64,
}
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FrameSample {
    pub target_ms: u64,
    pub actual_pts_ms: u64,
    pub path: PathBuf,
    pub width: u32,
    pub height: u32,
    pub bytes: u64,
}
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct VideoWindow {
    pub probe: VideoProbe,
    pub requested_start_ms: u64,
    pub requested_end_ms: u64,
    pub decoded_start_ms: Option<u64>,
    pub decoded_end_ms: Option<u64>,
    pub frames_decoded: usize,
    pub samples: Vec<FrameSample>,
    pub sampling_interval_ms: u64,
    pub visual_coverage: String,
    pub audio_processed: bool,
    pub gaps: Vec<String>,
}
#[derive(Default)]
struct OutputCleanup {
    paths: Vec<PathBuf>,
    committed: bool,
}
impl Drop for OutputCleanup {
    fn drop(&mut self) {
        if !self.committed {
            for path in &self.paths {
                let _ = std::fs::remove_file(path);
            }
        }
    }
}

#[derive(Default)]
struct Preflight {
    entries: u64,
    avcc: Vec<Vec<u8>>,
    boxes: usize,
}
fn be32(b: &[u8]) -> u32 {
    u32::from_be_bytes(b.try_into().unwrap())
}
fn at(file: &mut File, offset: u64, n: usize) -> Result<Vec<u8>> {
    file.seek(SeekFrom::Start(offset))?;
    let mut b = vec![0; n];
    file.read_exact(&mut b)?;
    Ok(b)
}
fn scan_boxes(
    file: &mut File,
    mut pos: u64,
    end: u64,
    depth: u8,
    p: &mut Preflight,
    l: &Limits,
) -> Result<()> {
    if depth > 12 {
        return Err(MediaError::Limit("MP4 nesting".into()));
    }
    while pos < end {
        if end - pos < 8 {
            return Err(MediaError::Invalid("truncated MP4 box".into()));
        }
        p.boxes += 1;
        if p.boxes > 20_000 {
            return Err(MediaError::Limit("MP4 box count".into()));
        }
        let head = at(file, pos, 8)?;
        let mut size = u64::from(be32(&head[..4]));
        let kind = &head[4..];
        let mut header = 8;
        if size == 1 {
            size = u64::from_be_bytes(at(file, pos + 8, 8)?.try_into().unwrap());
            header = 16;
        }
        if size == 0 {
            size = end - pos;
        }
        if size < header || size > end - pos {
            return Err(MediaError::Invalid("MP4 box length".into()));
        }
        let start = pos + header;
        let stop = pos + size;
        if kind != b"mdat" && size > 8 * 1024 * 1024 {
            return Err(MediaError::Limit("MP4 metadata box size".into()));
        }
        match kind {
            b"moov" | b"trak" | b"mdia" | b"minf" | b"stbl" | b"edts" | b"dinf" | b"moof"
            | b"mvex" | b"traf" => scan_boxes(file, start, stop, depth + 1, p, l)?,
            b"trun" => {
                if stop - start < 8 {
                    return Err(MediaError::Invalid("fragment sample header".into()));
                }
                let h = at(file, start, 8)?;
                let flags = be32(&h[..4]) & 0x00ff_ffff;
                let count = u64::from(be32(&h[4..]));
                p.entries = p
                    .entries
                    .checked_add(count)
                    .ok_or_else(|| MediaError::Limit("fragment sample count".into()))?;
                if count > l.max_samples as u64 || p.entries > l.max_samples as u64 * 8 {
                    return Err(MediaError::Limit("fragment sample entries".into()));
                }
                let prefix =
                    8 + if flags & 1 != 0 { 4 } else { 0 } + if flags & 4 != 0 { 4 } else { 0 };
                let record = (flags & 0xf00).count_ones() as u64 * 4;
                if prefix + count * record != stop - start {
                    return Err(MediaError::Invalid("fragment sample table length".into()));
                }
                if flags & 0x200 != 0 {
                    let offset = if flags & 0x100 != 0 { 4 } else { 0 };
                    for i in 0..count {
                        let n =
                            u64::from(be32(&at(file, start + prefix + i * record + offset, 4)?));
                        if n > l.max_decoded_bytes.min(8 * 1024 * 1024) {
                            return Err(MediaError::Limit("fragment encoded sample".into()));
                        }
                    }
                }
            }
            b"stsd" => {
                let data = at(file, start, 8)?;
                let count = be32(&data[4..]);
                if count > 16 {
                    return Err(MediaError::Limit("MP4 sample descriptions".into()));
                }
                let mut entry = start + 8;
                for _ in 0..count {
                    let h = at(file, entry, 8)?;
                    let n = u64::from(be32(&h[..4]));
                    if n < 8 || entry + n > stop {
                        return Err(MediaError::Invalid("MP4 sample entry".into()));
                    }
                    if &h[4..] == b"avc1" {
                        if n < 86 {
                            return Err(MediaError::Invalid("AVC sample entry".into()));
                        }
                        let dim = at(file, entry + 32, 4)?;
                        let w = u32::from(u16::from_be_bytes(dim[..2].try_into().unwrap()));
                        let h = u32::from(u16::from_be_bytes(dim[2..].try_into().unwrap()));
                        l.check_image(w, h, u64::from(w) * u64::from(h) * 4)?;
                        scan_boxes(file, entry + 86, entry + n, depth + 1, p, l)?;
                    }
                    entry += n;
                }
            }
            b"avcC" => {
                if size - header > 65536 {
                    return Err(MediaError::Limit("AVC configuration".into()));
                }
                let data = at(file, start, (size - header) as usize)?;
                validate_avcc(&data, l)?;
                p.avcc.push(data);
            }
            b"stsz" | b"stz2" | b"stts" | b"ctts" | b"stsc" | b"stco" | b"co64" | b"stss" => {
                let bytes = if kind == b"stsz" || kind == b"stz2" {
                    12
                } else {
                    8
                };
                if stop - start < bytes {
                    return Err(MediaError::Invalid("MP4 sample table header".into()));
                }
                let h = at(file, start, bytes as usize)?;
                let count = u64::from(be32(&h[h.len() - 4..]));
                p.entries = p
                    .entries
                    .checked_add(count)
                    .ok_or_else(|| MediaError::Limit("MP4 sample count overflow".into()))?;
                if count > l.max_samples as u64 || p.entries > l.max_samples as u64 * 8 {
                    return Err(MediaError::Limit("MP4 sample table entries".into()));
                }
                let stride = match kind {
                    b"stts" | b"ctts" | b"co64" => 8,
                    b"stsc" => 12,
                    b"stco" | b"stss" => 4,
                    _ => 0,
                };
                if count * stride > stop - start - bytes {
                    return Err(MediaError::Invalid("MP4 sample table truncated".into()));
                }
                if kind == b"stsz"
                    && u64::from(be32(&h[4..8])) > l.max_decoded_bytes.min(8 * 1024 * 1024)
                {
                    return Err(MediaError::Limit("fixed encoded packet size".into()));
                }
                if kind == b"stz2" {
                    let bits = h[7];
                    if ![4, 8, 16].contains(&bits)
                        || (count * u64::from(bits)).div_ceil(8) > stop - start - 12
                    {
                        return Err(MediaError::Invalid("MP4 compact size table".into()));
                    }
                }
                if kind == b"stts" {
                    if count * 8 > stop - start - 8 {
                        return Err(MediaError::Invalid("MP4 timing table truncated".into()));
                    }
                    let mut total = 0u64;
                    for i in 0..count {
                        total += u64::from(be32(&at(file, start + 8 + i * 8, 4)?));
                        if total > l.max_samples as u64 {
                            return Err(MediaError::Limit("expanded MP4 timing samples".into()));
                        }
                    }
                }
                if kind == b"stsz" && be32(&h[4..8]) == 0 {
                    if count * 4 > stop - start - 12 {
                        return Err(MediaError::Invalid("MP4 size table truncated".into()));
                    }
                    for i in 0..count {
                        if u64::from(be32(&at(file, start + 12 + i * 4, 4)?))
                            > l.max_decoded_bytes.min(8 * 1024 * 1024)
                        {
                            return Err(MediaError::Limit("encoded video packet".into()));
                        }
                    }
                }
            }
            b"elst" => {
                if stop - start < 8 {
                    return Err(MediaError::Invalid("MP4 edit header".into()));
                }
                let h = at(file, start, 8)?;
                let n = u64::from(be32(&h[4..]));
                let stride = match h[0] {
                    0 => 12,
                    1 => 20,
                    _ => return Err(MediaError::Invalid("MP4 edit version".into())),
                };
                if n > 2 || n * stride != stop - start - 8 {
                    return Err(MediaError::Unsupported("MP4 edit count/length".into()));
                }
            }
            _ => {}
        }
        pos = stop;
    }
    Ok(())
}
fn preflight(path: &Path, l: &Limits) -> Result<Preflight> {
    let len = l.check_file(path)?;
    let mut file = File::open(path)?;
    let mut p = Preflight::default();
    scan_boxes(&mut file, 0, len, 0, &mut p, l)?;
    Ok(p)
}
pub(crate) fn validate_container(path: &Path, l: &Limits) -> Result<()> {
    preflight(path, l).map(|_| ())
}

struct Bits {
    bytes: Vec<u8>,
    at: usize,
}
impl Bits {
    fn get(&mut self, n: usize) -> Result<u32> {
        if n > 32 || self.at + n > self.bytes.len() * 8 {
            return Err(MediaError::Invalid("truncated H264 SPS".into()));
        }
        let mut v = 0;
        for _ in 0..n {
            v = (v << 1) | u32::from((self.bytes[self.at / 8] >> (7 - self.at % 8)) & 1);
            self.at += 1;
        }
        Ok(v)
    }
    fn ue(&mut self) -> Result<u32> {
        let mut n = 0;
        while self.get(1)? == 0 {
            n += 1;
            if n > 24 {
                return Err(MediaError::Limit("H264 Exp-Golomb".into()));
            }
        }
        Ok(((1u32 << n) - 1) + self.get(n)?)
    }
    fn se(&mut self) -> Result<i32> {
        let v = self.ue()?;
        Ok(if v & 1 != 0 {
            ((v + 1) / 2) as i32
        } else {
            -((v / 2) as i32)
        })
    }
}
fn validate_sps(nal: &[u8], l: &Limits) -> Result<()> {
    if nal.len() < 4 || nal[0] & 31 != 7 {
        return Err(MediaError::Invalid("H264 SPS".into()));
    }
    let mut bytes = Vec::new();
    let mut zeros = 0;
    for &b in &nal[1..] {
        if zeros >= 2 && b == 3 {
            zeros = 0;
            continue;
        }
        bytes.push(b);
        zeros = if b == 0 { zeros + 1 } else { 0 };
    }
    let mut b = Bits { bytes, at: 0 };
    let profile = b.get(8)?;
    b.get(8)?;
    b.get(8)?;
    b.ue()?;
    if ![66, 77, 100].contains(&profile) {
        return Err(MediaError::Unsupported(format!(
            "H264 profile {profile}; first implementation supports 8-bit 4:2:0 Baseline/Main/High"
        )));
    }
    if profile == 100 {
        if b.ue()? != 1 || b.ue()? != 0 || b.ue()? != 0 {
            return Err(MediaError::Unsupported("H264 non-8-bit/4:2:0".into()));
        }
        b.get(1)?;
        if b.get(1)? != 0 {
            for i in 0..8 {
                if b.get(1)? != 0 {
                    let mut last = 8;
                    let mut next = 8;
                    for _ in 0..if i < 6 { 16 } else { 64 } {
                        if next != 0 {
                            next = (last + b.se()? + 256) % 256;
                        }
                        if next != 0 {
                            last = next;
                        }
                    }
                }
            }
        }
    }
    if b.ue()? > 12 {
        return Err(MediaError::Limit("H264 frame number bits".into()));
    }
    match b.ue()? {
        0 => {
            if b.ue()? > 12 {
                return Err(MediaError::Limit("H264 POC bits".into()));
            }
        }
        1 => {
            b.get(1)?;
            b.se()?;
            b.se()?;
            let n = b.ue()?;
            if n > 256 {
                return Err(MediaError::Limit("H264 POC cycle".into()));
            }
            for _ in 0..n {
                b.se()?;
            }
        }
        2 => {}
        _ => return Err(MediaError::Invalid("H264 POC type".into())),
    }
    let refs = b.ue()?;
    if refs > 16 {
        return Err(MediaError::Limit("H264 reference frames".into()));
    }
    b.get(1)?;
    let w = b
        .ue()?
        .checked_add(1)
        .and_then(|n| n.checked_mul(16))
        .ok_or_else(|| MediaError::Limit("H264 width overflow".into()))?;
    let map_h = b
        .ue()?
        .checked_add(1)
        .ok_or_else(|| MediaError::Limit("H264 height overflow".into()))?;
    let frame = b.get(1)?;
    if frame == 0 {
        return Err(MediaError::Unsupported(
            "interlaced H264 is outside first media decoder envelope".into(),
        ));
    }
    let h = map_h
        .checked_mul(16)
        .ok_or_else(|| MediaError::Limit("H264 height overflow".into()))?;
    let estimate = u64::from(w) * u64::from(h) * u64::from(refs + 6) * 4;
    l.check_image(w, h, estimate)?;
    Ok(())
}
fn validate_avcc(data: &[u8], l: &Limits) -> Result<usize> {
    if data.len() < 7 || data[0] != 1 {
        return Err(MediaError::Invalid("AVC configuration header".into()));
    }
    let count = data[5] & 31;
    if count == 0 {
        return Err(MediaError::Invalid("AVC configuration lacks SPS".into()));
    }
    let mut at = 6;
    for _ in 0..count {
        if at + 2 > data.len() {
            return Err(MediaError::Invalid("truncated AVC SPS length".into()));
        }
        let n = u16::from_be_bytes(data[at..at + 2].try_into().unwrap()) as usize;
        at += 2;
        if at + n > data.len() {
            return Err(MediaError::Invalid("truncated AVC SPS".into()));
        }
        validate_sps(&data[at..at + n], l)?;
        at += n;
    }
    Ok(usize::from((data[4] & 3) + 1))
}
fn check_packet(data: &[u8], nal_len: usize, l: &Limits) -> Result<()> {
    let mut at = 0;
    while at < data.len() {
        if data.len() - at < nal_len {
            return Err(MediaError::Invalid("H264 packet NAL length".into()));
        }
        let mut n = 0usize;
        for &b in &data[at..at + nal_len] {
            n = (n << 8) | usize::from(b);
        }
        at += nal_len;
        if n == 0 || n > data.len() - at {
            return Err(MediaError::Invalid("H264 packet NAL bounds".into()));
        }
        if data[at] & 31 == 7 {
            validate_sps(&data[at..at + n], l)?;
        }
        at += n;
    }
    Ok(())
}

fn probe_demux(dmx: &nihav_core::demuxers::Demuxer<'_>, entries: u64) -> VideoProbe {
    VideoProbe {
        duration_ms: dmx.get_duration(),
        sample_table_entries: entries,
        tracks: (0..dmx.get_num_streams())
            .filter_map(|i| dmx.get_stream(i))
            .map(|s| {
                let info = s.get_info();
                let video = info.get_properties().get_video_info();
                VideoTrack {
                    id: s.get_id(),
                    codec: info.get_name().into(),
                    width: video.map(|v| v.get_width() as u32),
                    height: video.map(|v| v.get_height() as u32),
                    audio: info.is_audio(),
                }
            })
            .collect(),
    }
}
fn probe_fragment(index: &crate::mp4::Index, entries: u64) -> VideoProbe {
    VideoProbe {
        duration_ms: index.duration_ms,
        sample_table_entries: entries,
        tracks: index
            .tracks
            .values()
            .map(|t| VideoTrack {
                id: t.id,
                codec: match t.codec.as_str() {
                    "avc1" => "h264",
                    "mp4a" => "aac",
                    name => name,
                }
                .into(),
                width: (t.width > 0).then_some(t.width),
                height: (t.height > 0).then_some(t.height),
                audio: t.audio,
            })
            .collect(),
    }
}
pub fn probe_video(path: &Path, l: &Limits) -> Result<VideoProbe> {
    let p = preflight(path, l)?;
    let index = crate::mp4::index(path, l)?;
    if index.fragmented {
        return Ok(probe_fragment(&index, p.entries));
    }
    let mut registry = RegisteredDemuxers::new();
    nihav_commonfmt::generic_register_all_demuxers(&mut registry);
    let mut file = File::open(path)?;
    let mut reader = FileReader::new_read(&mut file);
    let demux = create_demuxer(
        registry
            .find_demuxer("mov")
            .ok_or_else(|| MediaError::Unsupported("MOV demuxer".into()))?,
        &mut reader,
    )
    .map_err(|e| MediaError::Decode(format!("MP4 probe {e:?}")))?;
    Ok(probe_demux(&demux, p.entries))
}

pub fn sample_video_window(
    input: &Path,
    output_dir: &Path,
    start_ms: u64,
    duration_ms: u64,
    interval_ms: u64,
    max_edge: u32,
    l: &Limits,
) -> Result<VideoWindow> {
    let end_ms = l.check_window(start_ms, duration_ms)?;
    if interval_ms == 0
        || duration_ms.div_ceil(interval_ms) > l.max_frames as u64
        || max_edge == 0
        || max_edge > l.max_dimension
    {
        return Err(MediaError::Limit("video sampling density/size".into()));
    }
    let p = preflight(input, l)?;
    let mut index = crate::mp4::index(input, l)?;
    if index.fragmented {
        let probe = probe_fragment(&index, p.entries);
        let id = *index
            .tracks
            .iter()
            .find(|(_, t)| t.avcc.is_some())
            .ok_or_else(|| MediaError::Unsupported("no clear avc1 video track".into()))?
            .0;
        let track = index.tracks.remove(&id).unwrap();
        let mut extra = b"avcC".to_vec();
        extra.extend_from_slice(track.avcc.as_ref().unwrap());
        let info = NACodecInfo::new(
            "h264",
            NACodecTypeInfo::Video(NAVideoInfo::new(
                track.width as usize,
                track.height as usize,
                false,
                YUV420_FORMAT,
            )),
            Some(extra),
        );
        let stream = NAStream::new(
            StreamType::Video,
            id,
            info,
            1,
            track.timescale,
            track.next_duration(),
        )
        .into_ref();
        let mut position = track
            .samples
            .iter()
            .rposition(|s| {
                s.key
                    && u128::from(s.pts) * 1000
                        <= u128::from(start_ms) * u128::from(track.timescale)
            })
            .unwrap_or(0);
        let mut file = File::open(input)?;
        let read_stream = stream.clone();
        let source_gaps = track.coverage_gaps(start_ms, end_ms);
        let mut result = sample_packets(
            probe,
            stream,
            || {
                let Some(s) = track.samples.get(position) else {
                    return Ok(None);
                };
                position += 1;
                let data = at(&mut file, s.offset, s.size as usize)?;
                Ok(Some(NAPacket::new(
                    read_stream.clone(),
                    read_stream.make_ts(Some(s.pts), Some(s.dts), Some(u64::from(s.duration))),
                    s.key,
                    data,
                )))
            },
            output_dir,
            start_ms,
            end_ms,
            interval_ms,
            max_edge,
            l,
        )?;
        result.gaps.extend(source_gaps);
        return Ok(result);
    }
    let mut dreg = RegisteredDemuxers::new();
    nihav_commonfmt::generic_register_all_demuxers(&mut dreg);
    let mut file = File::open(input)?;
    let mut reader = FileReader::new_read(&mut file);
    let mut dmx = create_demuxer(
        dreg.find_demuxer("mov")
            .ok_or_else(|| MediaError::Unsupported("MOV demuxer".into()))?,
        &mut reader,
    )
    .map_err(|e| MediaError::Decode(format!("MP4 open {e:?}")))?;
    let probe = probe_demux(&dmx, p.entries);
    let stream = (0..dmx.get_num_streams())
        .filter_map(|i| dmx.get_stream(i))
        .find(|s| s.get_info().is_video())
        .ok_or_else(|| MediaError::Unsupported("no video stream".into()))?;
    if start_ms > 0 {
        dmx.seek(NATimePoint::Milliseconds(start_ms))
            .map_err(|e| MediaError::Unsupported(format!("MP4 seek {e:?}")))?;
    }
    sample_packets(
        probe,
        stream,
        || match dmx.get_frame() {
            Ok(p) => Ok(Some(p)),
            Err(DemuxerError::EOF) => Ok(None),
            Err(e) => Err(MediaError::Decode(format!("MP4 packet {e:?}"))),
        },
        output_dir,
        start_ms,
        end_ms,
        interval_ms,
        max_edge,
        l,
    )
}

#[allow(clippy::too_many_arguments)]
fn sample_packets(
    probe: VideoProbe,
    stream: NAStreamRef,
    mut next_packet: impl FnMut() -> Result<Option<NAPacket>>,
    output_dir: &Path,
    start_ms: u64,
    end_ms: u64,
    interval_ms: u64,
    max_edge: u32,
    l: &Limits,
) -> Result<VideoWindow> {
    let duration_ms = end_ms - start_ms;
    let info = stream.get_info();
    if info.get_name() != "h264" {
        return Err(MediaError::Unsupported(format!(
            "video codec {}",
            info.get_name()
        )));
    }
    let extra = info
        .get_extradata()
        .ok_or_else(|| MediaError::Invalid("missing AVC configuration".into()))?;
    let data = extra.strip_prefix(b"avcC").unwrap_or(&extra);
    let nal_len = validate_avcc(data, l)?;
    let mut registry = RegisteredDecoders::new();
    nihav_itu::itu_register_all_decoders(&mut registry);
    let mut decoder = registry
        .find_decoder("h264")
        .ok_or_else(|| MediaError::Unsupported("H264 decoder unavailable".into()))?(
    );
    let mut support = NADecoderSupport::new();
    decoder
        .init(&mut support, info)
        .map_err(|e| MediaError::Decode(format!("H264 init {e:?}")))?;
    std::fs::create_dir_all(output_dir)?;
    let (num, den) = stream.get_timebase();
    if num == 0 || den == 0 {
        return Err(MediaError::Invalid("video time base".into()));
    }
    let mut samples = BTreeMap::<u64, FrameSample>::new();
    let mut decoded = 0;
    let mut min_pts = None;
    let mut max_pts = None;
    let mut after_end = 0;
    let mut output_bytes = 0;
    let mut packets = 0;
    let mut cleanup = OutputCleanup::default();
    for _ in 0..l.max_packets {
        let Some(packet) = next_packet()? else { break };
        packets += 1;
        if packet.get_stream().get_id() != stream.get_id() {
            continue;
        }
        let bytes = packet.get_buffer();
        if bytes.len() as u64 > l.max_decoded_bytes.min(8 * 1024 * 1024) {
            return Err(MediaError::Limit("encoded H264 packet".into()));
        }
        check_packet(&bytes, nal_len, l)?;
        let frame = decoder
            .decode(&mut support, &packet)
            .map_err(|e| MediaError::Decode(format!("H264 frame {e:?}")))?;
        decoded += 1;
        let pts = frame
            .get_pts()
            .or(packet.get_pts())
            .ok_or_else(|| MediaError::Unsupported("decoded frame lacks PTS".into()))?;
        let ms = ((u128::from(pts) * u128::from(num) * 1000) / u128::from(den))
            .min(u128::from(u64::MAX)) as u64;
        min_pts = Some(min_pts.map_or(ms, |old: u64| old.min(ms)));
        max_pts = Some(max_pts.map_or(ms, |old: u64| old.max(ms)));
        if ms >= end_ms {
            after_end += 1;
            if after_end > 32 {
                break;
            }
            continue;
        }
        if ms < start_ms {
            continue;
        }
        let slot = (ms - start_ms) / interval_ms;
        let target = start_ms + slot * interval_ms;
        if samples.get(&slot).is_some_and(|s| s.actual_pts_ms <= ms) {
            continue;
        }
        let buffer = frame
            .get_buffer()
            .get_vbuf()
            .ok_or_else(|| MediaError::Unsupported("H264 output is not 8-bit pixels".into()))?;
        let vinfo = buffer.get_info();
        if vinfo.get_format() != YUV420_FORMAT {
            return Err(MediaError::Unsupported(
                "H264 pixel layout; first decoder supports YUV420p8".into(),
            ));
        }
        let (w, h) = (vinfo.get_width(), vinfo.get_height());
        l.check_image(w as u32, h as u32, (w as u64) * (h as u64) * 3)?;
        let pixels = buffer.get_data();
        let mut rgb = image::RgbImage::new(w as u32, h as u32);
        for y in 0..h {
            for x in 0..w {
                let yy =
                    i32::from(pixels[buffer.get_offset(0) + y * buffer.get_stride(0) + x]) - 16;
                let u = i32::from(
                    pixels[buffer.get_offset(1) + (y / 2) * buffer.get_stride(1) + x / 2],
                ) - 128;
                let v = i32::from(
                    pixels[buffer.get_offset(2) + (y / 2) * buffer.get_stride(2) + x / 2],
                ) - 128;
                let clip = |n: i32| ((n + 128) >> 8).clamp(0, 255) as u8;
                rgb.put_pixel(
                    x as u32,
                    y as u32,
                    image::Rgb([
                        clip(298 * yy + 409 * v),
                        clip(298 * yy - 100 * u - 208 * v),
                        clip(298 * yy + 516 * u),
                    ]),
                );
            }
        }
        let edge = max_edge.min((w.max(h)) as u32);
        let image = image::DynamicImage::ImageRgb8(rgb).thumbnail(edge, edge);
        let path = output_dir.join(format!("frame-{target:012}.jpg"));
        if let Some(old) = samples.remove(&slot) {
            output_bytes -= old.bytes;
            std::fs::remove_file(&old.path)?;
        }
        let mut frame_limits = l.clone();
        frame_limits.max_output_bytes = l.max_output_bytes.saturating_sub(output_bytes);
        let (bytes, _) = write_image(&image, &path, OutputFormat::Jpeg, &frame_limits)?;
        output_bytes += bytes;
        cleanup.paths.push(path.clone());
        samples.insert(
            slot,
            FrameSample {
                target_ms: target,
                actual_pts_ms: ms,
                path,
                width: image.width(),
                height: image.height(),
                bytes,
            },
        );
    }
    let mut gaps=vec!["audio has not been processed by the frame sampler; provide the aligned audio track separately".into(),"visual sampling may miss short events between actual PTS samples; request a denser window when needed".into()];
    for slot in 0..duration_ms.div_ceil(interval_ms) {
        if !samples.contains_key(&slot) {
            gaps.push(format!(
                "no visual sample for {}..{} ms",
                start_ms + slot * interval_ms,
                (start_ms + (slot + 1) * interval_ms).min(end_ms)
            ));
        }
    }
    if packets >= l.max_packets {
        gaps.push("decode packet budget exhausted".into());
    }
    if samples.is_empty() {
        return Err(MediaError::Decode(
            "no video frames in requested time window".into(),
        ));
    }
    cleanup.committed = true;
    Ok(VideoWindow {
        probe,
        requested_start_ms: start_ms,
        requested_end_ms: end_ms,
        decoded_start_ms: min_pts,
        decoded_end_ms: max_pts,
        frames_decoded: decoded,
        samples: samples.into_values().collect(),
        sampling_interval_ms: interval_ms,
        visual_coverage: "timestamped_samples_not_all_frames".into(),
        audio_processed: false,
        gaps,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    fn sps(width_mbs: u32, refs: u32) -> Vec<u8> {
        fn ue(bits: &mut Vec<bool>, v: u32) {
            let n = v + 1;
            let len = 32 - n.leading_zeros();
            bits.extend(std::iter::repeat_n(false, (len - 1) as usize));
            for shift in (0..len).rev() {
                bits.push(n & (1 << shift) != 0);
            }
        }
        let mut bits = Vec::new();
        for byte in [66u8, 0, 30] {
            for shift in (0..8).rev() {
                bits.push(byte & (1 << shift) != 0);
            }
        }
        ue(&mut bits, 0);
        ue(&mut bits, 0);
        ue(&mut bits, 0);
        ue(&mut bits, 0);
        ue(&mut bits, refs);
        bits.push(false);
        ue(&mut bits, width_mbs - 1);
        ue(&mut bits, 17);
        bits.push(true);
        let mut out = vec![0x67];
        for part in bits.chunks(8) {
            let mut value = 0;
            for (i, &set) in part.iter().enumerate() {
                if set {
                    value |= 1 << (7 - i);
                }
            }
            out.push(value);
        }
        out
    }
    #[test]
    fn sps_bounds_are_checked_before_decoder_allocation() {
        let limits = Limits::default();
        assert!(validate_sps(&sps(32, 4), &limits).is_ok());
        assert!(matches!(
            validate_sps(&sps(1000, 4), &limits),
            Err(MediaError::Limit(_))
        ));
        assert!(matches!(
            validate_sps(&sps(32, 17), &limits),
            Err(MediaError::Limit(_))
        ));
        assert!(validate_sps(&[0x67, 66, 0, 30], &limits).is_err());
    }
    #[test]
    fn nal_prefix_and_inband_sps_cannot_bypass_bounds() {
        let limits = Limits::default();
        assert!(check_packet(&[0, 0, 0, 99, 1], 4, &limits).is_err());
        let large = sps(1000, 4);
        let mut packet = (large.len() as u32).to_be_bytes().to_vec();
        packet.extend(large);
        assert!(matches!(
            check_packet(&packet, 4, &limits),
            Err(MediaError::Limit(_))
        ));
    }
}
