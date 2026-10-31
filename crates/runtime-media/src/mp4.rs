// SPDX-License-Identifier: MIT
//! Small, bounded fragmented-MP4 sample index. Payload bytes stay in the source file.
//! Handles clear avc1/mp4a DASH files; encrypted sample entries are not decoded.
use crate::{Limits, MediaError, Result};
use std::{
    collections::BTreeMap,
    fs::File,
    io::{Read, Seek, SeekFrom},
    path::Path,
};

#[derive(Clone, Copy, Default)]
struct Defaults {
    duration: u32,
    size: u32,
    flags: u32,
}
#[derive(Clone, Copy, Debug)]
pub(crate) struct Sample {
    pub offset: u64,
    pub size: u32,
    pub dts: u64,
    pub pts: u64,
    pub duration: u32,
    pub key: bool,
}
#[derive(Default)]
pub(crate) struct Track {
    pub id: u32,
    pub timescale: u32,
    pub width: u32,
    pub height: u32,
    pub codec: String,
    pub audio: bool,
    pub avcc: Option<Vec<u8>>,
    pub samples: Vec<Sample>,
    defaults: Defaults,
    next_dts: u64,
    edit_media_time: u64,
    edit_lead: u64,
}
impl Track {
    pub(crate) fn next_duration(&self) -> u64 {
        self.next_dts
    }
    pub(crate) fn coverage_gaps(&self, start_ms: u64, end_ms: u64) -> Vec<String> {
        let mut intervals: Vec<_> = self
            .samples
            .iter()
            .map(|s| (s.pts, s.pts.saturating_add(u64::from(s.duration))))
            .collect();
        intervals.sort_unstable();
        let mut cursor = u128::from(start_ms) * u128::from(self.timescale) / 1000;
        let end = (u128::from(end_ms) * u128::from(self.timescale)).div_ceil(1000);
        let mut gaps = Vec::new();
        for (a, b) in intervals {
            let a = u128::from(a);
            let b = u128::from(b);
            if b <= cursor {
                continue;
            }
            if a >= end {
                break;
            }
            if a > cursor {
                if gaps.len() == 32 {
                    gaps.push("additional source timestamp gaps omitted after 32 entries".into());
                    return gaps;
                }
                gaps.push(format!(
                    "source video timeline has no frame for {}..{} ms",
                    cursor * 1000 / u128::from(self.timescale),
                    (a * 1000).div_ceil(u128::from(self.timescale))
                ));
            }
            cursor = cursor.max(b);
            if cursor >= end {
                break;
            }
        }
        if cursor < end {
            gaps.push(format!(
                "source video timeline has no frame for {}..{end_ms} ms",
                cursor * 1000 / u128::from(self.timescale)
            ));
        }
        gaps
    }
}
pub(crate) struct Index {
    pub tracks: BTreeMap<u32, Track>,
    pub fragmented: bool,
    pub duration_ms: u64,
}
#[derive(Clone, Copy)]
struct Atom {
    kind: [u8; 4],
    start: u64,
    data: u64,
    end: u64,
}
fn invalid(s: &str) -> MediaError {
    MediaError::Invalid(s.into())
}
fn read(file: &mut File, pos: u64, n: usize) -> Result<Vec<u8>> {
    file.seek(SeekFrom::Start(pos))?;
    let mut b = vec![0; n];
    file.read_exact(&mut b)?;
    Ok(b)
}
fn u32b(b: &[u8]) -> u32 {
    u32::from_be_bytes(b.try_into().unwrap())
}
fn atom(file: &mut File, pos: u64, end: u64) -> Result<Atom> {
    if end.saturating_sub(pos) < 8 {
        return Err(invalid("truncated MP4 atom"));
    }
    let h = read(file, pos, 8)?;
    let mut n = u64::from(u32b(&h[..4]));
    let mut header = 8;
    if n == 1 {
        if end - pos < 16 {
            return Err(invalid("MP4 extended header"));
        }
        n = u64::from_be_bytes(read(file, pos + 8, 8)?.try_into().unwrap());
        header = 16;
    }
    if n == 0 {
        n = end - pos;
    }
    if n < header || n > end - pos {
        return Err(invalid("MP4 atom bounds"));
    }
    Ok(Atom {
        kind: h[4..8].try_into().unwrap(),
        start: pos,
        data: pos + header,
        end: pos + n,
    })
}
fn children(file: &mut File, mut pos: u64, end: u64) -> Result<Vec<Atom>> {
    let mut out = Vec::new();
    while pos < end {
        if out.len() >= 20_000 {
            return Err(MediaError::Limit("MP4 children".into()));
        }
        let a = atom(file, pos, end)?;
        pos = a.end;
        out.push(a);
    }
    Ok(out)
}
fn payload(file: &mut File, a: Atom, minimum: usize) -> Result<Vec<u8>> {
    if a.end - a.data < minimum as u64 || a.end - a.data > 65536 {
        return Err(invalid("MP4 small metadata bounds"));
    }
    read(file, a.data, (a.end - a.data) as usize)
}
fn track_children(file: &mut File, start: u64, end: u64, t: &mut Track, depth: u8) -> Result<()> {
    if depth > 12 {
        return Err(MediaError::Limit("MP4 track nesting".into()));
    }
    for a in children(file, start, end)? {
        match &a.kind {
            b"tkhd" => {
                let d = payload(file, a, 20)?;
                let offset = if d[0] == 1 {
                    20
                } else if d[0] == 0 {
                    12
                } else {
                    return Err(invalid("tkhd version"));
                };
                if offset + 4 > d.len() {
                    return Err(invalid("tkhd track id"));
                }
                t.id = u32b(&d[offset..offset + 4]);
            }
            b"mdhd" => {
                let d = payload(file, a, 20)?;
                let offset = if d[0] == 1 {
                    20
                } else if d[0] == 0 {
                    12
                } else {
                    return Err(invalid("mdhd version"));
                };
                if offset + 4 > d.len() {
                    return Err(invalid("mdhd timescale"));
                }
                t.timescale = u32b(&d[offset..offset + 4]);
            }
            b"mdia" | b"minf" | b"stbl" | b"edts" => {
                track_children(file, a.data, a.end, t, depth + 1)?
            }
            b"elst" => {
                let d = payload(file, a, 8)?;
                let version = d[0];
                let count = u32b(&d[4..8]);
                let stride = if version == 0 {
                    12
                } else if version == 1 {
                    20
                } else {
                    return Err(invalid("edit list version"));
                };
                if count == 0 || count > 2 || d.len() != 8 + count as usize * stride {
                    return Err(MediaError::Unsupported("MP4 edit list requires one linear edit, optionally preceded by an empty edit".into()));
                }
                for i in 0..count as usize {
                    let row = &d[8 + i * stride..8 + (i + 1) * stride];
                    let (duration, media, rate) = if version == 0 {
                        (
                            u64::from(u32b(&row[..4])),
                            i64::from(u32b(&row[4..8]) as i32),
                            u32b(&row[8..12]),
                        )
                    } else {
                        (
                            u64::from_be_bytes(row[..8].try_into().unwrap()),
                            i64::from_be_bytes(row[8..16].try_into().unwrap()),
                            u32b(&row[16..20]),
                        )
                    };
                    if rate != 0x0001_0000 {
                        return Err(MediaError::Unsupported("MP4 non-unit edit rate".into()));
                    }
                    if media == -1 && i == 0 && count == 2 {
                        t.edit_lead = duration;
                    } else if media >= 0 && i + 1 == count as usize {
                        t.edit_media_time = media as u64;
                    } else {
                        return Err(MediaError::Unsupported("MP4 non-linear edit list".into()));
                    }
                }
            }
            b"stsd" => {
                if a.end - a.data < 8 {
                    return Err(invalid("stsd header"));
                }
                let h = read(file, a.data, 8)?;
                let count = u32b(&h[4..]);
                if count != 1 {
                    return Err(MediaError::Unsupported(
                        "one MP4 sample description per track is required".into(),
                    ));
                }
                let e = atom(file, a.data + 8, a.end)?;
                t.codec = String::from_utf8_lossy(&e.kind).into_owned();
                if &e.kind == b"avc1" {
                    if e.end - e.start < 86 {
                        return Err(invalid("avc1 entry"));
                    }
                    let dim = read(file, e.start + 32, 4)?;
                    t.width = u32::from(u16::from_be_bytes(dim[..2].try_into().unwrap()));
                    t.height = u32::from(u16::from_be_bytes(dim[2..].try_into().unwrap()));
                    for c in children(file, e.start + 86, e.end)? {
                        if &c.kind == b"avcC" {
                            t.avcc = Some(payload(file, c, 7)?);
                        }
                    }
                } else if &e.kind == b"mp4a" {
                    t.audio = true;
                } else if &e.kind == b"encv" || &e.kind == b"enca" {
                    return Err(MediaError::Unsupported(
                        "encrypted MP4 sample entries".into(),
                    ));
                }
            }
            _ => {}
        }
    }
    Ok(())
}
fn fragment(
    file: &mut File,
    moof: Atom,
    traf: Atom,
    tracks: &mut BTreeMap<u32, Track>,
    ranges: &[(u64, u64)],
    limits: &Limits,
    total: &mut usize,
) -> Result<()> {
    let atoms = children(file, traf.data, traf.end)?;
    let h = atoms
        .iter()
        .find(|a| &a.kind == b"tfhd")
        .ok_or_else(|| invalid("fragment lacks tfhd"))?;
    let d = payload(file, *h, 8)?;
    let flags = u32b(&d[..4]) & 0xffffff;
    let id = u32b(&d[4..8]);
    let t = tracks
        .get_mut(&id)
        .ok_or_else(|| invalid("fragment references missing track"))?;
    let mut p = 8;
    let mut base = moof.start;
    let mut defaults = t.defaults;
    let take = |p: &mut usize, n: usize| -> Result<&[u8]> {
        let end = p.checked_add(n).ok_or_else(|| invalid("tfhd length"))?;
        if end > d.len() {
            return Err(invalid("tfhd field bounds"));
        }
        let s = &d[*p..end];
        *p = end;
        Ok(s)
    };
    if flags & 1 != 0 {
        base = u64::from_be_bytes(take(&mut p, 8)?.try_into().unwrap());
    }
    if flags & 2 != 0 {
        take(&mut p, 4)?;
    }
    if flags & 8 != 0 {
        defaults.duration = u32b(take(&mut p, 4)?);
    }
    if flags & 16 != 0 {
        defaults.size = u32b(take(&mut p, 4)?);
    }
    if flags & 32 != 0 {
        defaults.flags = u32b(take(&mut p, 4)?);
    }
    if let Some(a) = atoms.iter().find(|a| &a.kind == b"tfdt") {
        let d = payload(file, *a, 8)?;
        t.next_dts = match d[0] {
            0 => u64::from(u32b(&d[4..8])),
            1 if d.len() >= 12 => u64::from_be_bytes(d[4..12].try_into().unwrap()),
            _ => return Err(invalid("tfdt version/length")),
        };
    }
    let mut previous_end = None;
    for a in atoms.into_iter().filter(|a| &a.kind == b"trun") {
        if a.end - a.data < 8 {
            return Err(invalid("trun header"));
        }
        let h = read(file, a.data, 8)?;
        let version = h[0];
        if version > 1 {
            return Err(invalid("trun version"));
        }
        let f = u32b(&h[..4]) & 0xffffff;
        let count = u32b(&h[4..]) as usize;
        *total = total
            .checked_add(count)
            .ok_or_else(|| MediaError::Limit("fragment sample overflow".into()))?;
        if *total > limits.max_samples {
            return Err(MediaError::Limit("fragment sample count".into()));
        }
        let mut cursor = a.data + 8;
        let mut offset = if f & 1 != 0 {
            let value = u32b(&read(file, cursor, 4)?) as i32;
            cursor += 4;
            let pos = i128::from(base) + i128::from(value);
            if pos < 0 || pos > i128::from(u64::MAX) {
                return Err(invalid("fragment data offset"));
            }
            pos as u64
        } else {
            previous_end.ok_or_else(|| {
                MediaError::Unsupported("first trun must provide data offset".into())
            })?
        };
        let first = if f & 4 != 0 {
            let v = u32b(&read(file, cursor, 4)?);
            cursor += 4;
            Some(v)
        } else {
            None
        };
        let record = u64::from((f & 0xf00).count_ones()) * 4;
        if cursor + record * count as u64 != a.end {
            return Err(invalid("trun entry lengths"));
        }
        for n in 0..count {
            let row = read(file, cursor, record as usize)?;
            cursor += record;
            let mut at = 0;
            let mut field = || {
                let value = u32b(&row[at..at + 4]);
                at += 4;
                value
            };
            let duration = if f & 0x100 != 0 {
                field()
            } else {
                defaults.duration
            };
            let size = if f & 0x200 != 0 {
                field()
            } else {
                defaults.size
            };
            let sf = if f & 0x400 != 0 {
                field()
            } else if n == 0 {
                first.unwrap_or(defaults.flags)
            } else {
                defaults.flags
            };
            let cto = if f & 0x800 != 0 {
                let value = field();
                if version == 1 {
                    i128::from(value as i32)
                } else {
                    i128::from(value)
                }
            } else {
                0
            };
            if duration == 0 || size == 0 {
                return Err(invalid("zero MP4 sample duration/size"));
            }
            if u64::from(size) > limits.max_decoded_bytes.min(8 * 1024 * 1024) {
                return Err(MediaError::Limit("fragment sample bytes".into()));
            }
            let end = offset
                .checked_add(u64::from(size))
                .ok_or_else(|| invalid("sample offset overflow"))?;
            if !ranges.iter().any(|&(a, b)| offset >= a && end <= b) {
                return Err(invalid("sample outside mdat payload"));
            }
            let pts = i128::from(t.next_dts) + cto;
            if pts < 0 || pts > i128::from(u64::MAX) {
                return Err(MediaError::Unsupported(
                    "negative/overflowing presentation timestamp".into(),
                ));
            }
            t.samples.push(Sample {
                offset,
                size,
                dts: t.next_dts,
                pts: pts as u64,
                duration,
                key: sf & 0x10000 == 0 && (sf >> 24) & 3 != 1,
            });
            t.next_dts = t
                .next_dts
                .checked_add(u64::from(duration))
                .ok_or_else(|| invalid("sample decode timestamp overflow"))?;
            offset = end;
        }
        previous_end = Some(offset);
    }
    Ok(())
}
pub(crate) fn index(path: &Path, limits: &Limits) -> Result<Index> {
    let len = limits.check_file(path)?;
    let mut file = File::open(path)?;
    let top = children(&mut file, 0, len)?;
    let fragmented = top.iter().any(|a| &a.kind == b"moof");
    let mut tracks = BTreeMap::new();
    let mut defaults = Vec::new();
    let mut movie_timescale = 0u32;
    for moov in top.iter().filter(|a| &a.kind == b"moov") {
        for a in children(&mut file, moov.data, moov.end)? {
            match &a.kind {
                b"mvhd" => {
                    let d = payload(&mut file, a, 20)?;
                    let off = if d[0] == 0 {
                        12
                    } else if d[0] == 1 {
                        20
                    } else {
                        return Err(invalid("movie timescale version"));
                    };
                    if off + 4 > d.len() {
                        return Err(invalid("movie timescale bounds"));
                    }
                    movie_timescale = u32b(&d[off..off + 4]);
                }
                b"trak" => {
                    let mut t = Track::default();
                    track_children(&mut file, a.data, a.end, &mut t, 0)?;
                    if t.id == 0 || t.timescale == 0 {
                        return Err(invalid("MP4 track id/timescale"));
                    }
                    if tracks.len() >= 16 {
                        return Err(MediaError::Limit("MP4 track count".into()));
                    }
                    if tracks.insert(t.id, t).is_some() {
                        return Err(invalid("duplicate MP4 track"));
                    }
                }
                b"mvex" => {
                    for child in children(&mut file, a.data, a.end)? {
                        if &child.kind == b"trex" {
                            let d = payload(&mut file, child, 24)?;
                            defaults.push((
                                u32b(&d[4..8]),
                                Defaults {
                                    duration: u32b(&d[12..16]),
                                    size: u32b(&d[16..20]),
                                    flags: u32b(&d[20..24]),
                                },
                            ));
                        }
                    }
                }
                _ => {}
            }
        }
    }
    for (id, d) in defaults {
        if let Some(t) = tracks.get_mut(&id) {
            t.defaults = d;
        }
    }
    let ranges: Vec<_> = top
        .iter()
        .filter(|a| &a.kind == b"mdat")
        .map(|a| (a.data, a.end))
        .collect();
    let mut total = 0;
    for moof in top.iter().filter(|a| &a.kind == b"moof") {
        for traf in children(&mut file, moof.data, moof.end)?
            .into_iter()
            .filter(|a| &a.kind == b"traf")
        {
            fragment(
                &mut file,
                *moof,
                traf,
                &mut tracks,
                &ranges,
                limits,
                &mut total,
            )?;
        }
    }
    for t in tracks.values_mut() {
        let lead = if t.edit_lead == 0 {
            0
        } else {
            if movie_timescale == 0 {
                return Err(invalid("missing edit movie timescale"));
            }
            u64::try_from(
                u128::from(t.edit_lead) * u128::from(t.timescale) / u128::from(movie_timescale),
            )
            .map_err(|_| invalid("edit lead overflow"))?
        };
        for sample in &mut t.samples {
            sample.pts = sample
                .pts
                .checked_sub(t.edit_media_time)
                .and_then(|v| v.checked_add(lead))
                .ok_or_else(|| {
                    MediaError::Unsupported("MP4 edit excludes encoded leading frames".into())
                })?;
        }
    }
    let duration_ms = tracks
        .values()
        .filter_map(|t| {
            t.samples
                .iter()
                .map(|s| s.pts.saturating_add(u64::from(s.duration)))
                .max()
                .map(|ts| {
                    ((u128::from(ts) * 1000) / u128::from(t.timescale)).min(u128::from(u64::MAX))
                        as u64
                })
        })
        .max()
        .unwrap_or(0);
    Ok(Index {
        tracks,
        fragmented,
        duration_ms,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    fn box_data(kind: &[u8; 4], data: &[u8]) -> Vec<u8> {
        let mut b = ((data.len() + 8) as u32).to_be_bytes().to_vec();
        b.extend(kind);
        b.extend(data);
        b
    }
    fn fixture() -> Vec<u8> {
        let mut tkhd = vec![0u8; 20];
        tkhd[12..16].copy_from_slice(&1u32.to_be_bytes());
        let mut mdhd = vec![0u8; 20];
        mdhd[12..16].copy_from_slice(&1000u32.to_be_bytes());
        let mut avc1 = vec![0u8; 78];
        avc1[24..26].copy_from_slice(&16u16.to_be_bytes());
        avc1[26..28].copy_from_slice(&16u16.to_be_bytes());
        avc1.extend(box_data(b"avcC", &[1, 66, 0, 30, 255, 225, 0]));
        let mut stsd = vec![0, 0, 0, 0, 0, 0, 0, 1];
        stsd.extend(box_data(b"avc1", &avc1));
        let mut track = box_data(b"tkhd", &tkhd);
        let mut mdia = box_data(b"mdhd", &mdhd);
        mdia.extend(box_data(
            b"minf",
            &box_data(b"stbl", &box_data(b"stsd", &stsd)),
        ));
        track.extend(box_data(b"mdia", &mdia));
        let moov = box_data(b"moov", &box_data(b"trak", &track));
        let tfhd = box_data(b"tfhd", &[0, 2, 0, 0, 0, 0, 0, 1]);
        let mut tfdt = vec![1, 0, 0, 0];
        tfdt.extend(10000u64.to_be_bytes());
        let tfdt = box_data(b"tfdt", &tfdt);
        let make_moof = |offset: u32| {
            let mut trun = vec![1, 0, 15, 1];
            trun.extend(2u32.to_be_bytes());
            trun.extend(offset.to_be_bytes());
            for (duration, size, flags, cto) in [
                (1000u32, 3u32, 0x0200_0000u32, 200i32),
                (500, 3, 0x0101_0000, -200),
            ] {
                trun.extend(duration.to_be_bytes());
                trun.extend(size.to_be_bytes());
                trun.extend(flags.to_be_bytes());
                trun.extend(cto.to_be_bytes());
            }
            let mut traf = tfhd.clone();
            traf.extend_from_slice(&tfdt);
            traf.extend(box_data(b"trun", &trun));
            box_data(b"moof", &box_data(b"traf", &traf))
        };
        let moof = make_moof(make_moof(0).len() as u32 + 8);
        let mut out = moov;
        out.extend(moof);
        out.extend(box_data(b"mdat", b"abcdef"));
        out
    }
    #[test]
    fn fragmented_index_preserves_dts_signed_cto_duration_and_offsets() {
        let path =
            std::env::temp_dir().join(format!("runtime-media-fmp4-{}.mp4", std::process::id()));
        let source = fixture();
        std::fs::write(&path, &source).unwrap();
        let index = index(&path, &Limits::default()).unwrap();
        assert!(index.fragmented);
        assert_eq!(index.duration_ms, 11300);
        let track = &index.tracks[&1];
        assert_eq!(track.samples.len(), 2);
        let a = track.samples[0];
        let b = track.samples[1];
        assert_eq!(
            (a.dts, a.pts, a.duration, a.key),
            (10000, 10200, 1000, true)
        );
        assert_eq!(
            (b.dts, b.pts, b.duration, b.key),
            (11000, 10800, 500, false)
        );
        assert_eq!(
            &source[a.offset as usize..a.offset as usize + a.size as usize],
            b"abc"
        );
        let mut limits = Limits::default();
        limits.max_samples = 1;
        assert!(matches!(
            super::index(&path, &limits),
            Err(MediaError::Limit(_))
        ));
        std::fs::remove_file(path).unwrap();
    }
}
