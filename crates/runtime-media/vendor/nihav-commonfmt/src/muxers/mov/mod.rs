use std::convert::TryInto;

use nihav_core::muxers::*;

mod audiotrack;
use audiotrack::*;
mod rawaudiotrack;
use rawaudiotrack::*;
mod videotrack;
use videotrack::*;

const TIMESCALE: u32 = 1000;

#[derive(Clone,Copy)]
struct ForcedTag {
    stream:     u32,
    tag:        [u8; 4],
}

trait MovWrite {
    fn write_pas_str(&mut self, pas_str: &[u8]) -> ByteIOResult<()>;
}

impl<T:?Sized + ByteIO> MovWrite for T {
    fn write_pas_str(&mut self, pas_str: &[u8]) -> ByteIOResult<()> {
        self.write_byte(pas_str.len() as u8)?;
        self.write_buf(pas_str)?;
        Ok(())
    }
}

fn write_matrix_structure(bw: &mut dyn ByteIO) -> MuxerResult<()> {
    bw.write_u32be(0x00010000)?;
    bw.write_u32be(0)?;
    bw.write_u32be(0)?;
    bw.write_u32be(0)?;
    bw.write_u32be(0x00010000)?;
    bw.write_u32be(0)?;
    bw.write_u32be(0)?;
    bw.write_u32be(0)?;
    bw.write_u32be(0x40000000)?;
    Ok(())
}

struct AtomMarker {
    pos:    u64,
    name:   [u8; 4],
    done:   bool,
}

impl AtomMarker {
    fn start(bw: &mut dyn ByteIO, tag: &[u8; 4]) -> MuxerResult<Self> {
        let pos = bw.tell();
        bw.write_u32be(0)?;
        bw.write_buf(tag)?;
        Ok(Self{ pos, name: *tag, done: false })
    }
    fn end(mut self, bw: &mut dyn ByteIO) -> MuxerResult<()> {
        let cur_pos = bw.tell();
        if cur_pos < self.pos + 8 {
            return Err(MuxerError::InvalidData);
        }
        bw.seek(SeekFrom::Start(self.pos))?;
        bw.write_u32be((cur_pos - self.pos) as u32)?;
        bw.seek(SeekFrom::Start(cur_pos))?;
        self.done = true;
        Ok(())
    }
}

impl Drop for AtomMarker {
    fn drop(&mut self) {
        if !self.done {
            println!("Unclosed marker '{}{}{}{}'", self.name[0] as char, self.name[1] as char, self.name[2] as char, self.name[3] as char);
        }
    }
}

#[derive(Default)]
struct ChunkAccount {
    offsets:    Vec<u32>,
    offsets64:  Vec<u64>,
    sizes:      Vec<u32>,
    cmap:       Vec<u32>,
    pts:        Vec<u64>,
    keyframes:  Vec<u32>,
}

impl ChunkAccount {
    fn new() -> Self { Self::default() }
    fn is_empty(&self) -> bool { self.offsets.is_empty() && self.offsets64.is_empty() }

    fn add_size(&mut self, size: usize) {
        self.sizes.push(size as u32);
    }
    fn add_keyframe(&mut self, chunk: u32) {
        self.keyframes.push(chunk);
    }
    fn add_pts(&mut self, pts: u64) {
        self.pts.push(pts);
    }
    fn add_offset(&mut self, offset: u64) {
        if let Ok(off32) = offset.try_into() {
            self.offsets.push(off32);
        } else {
            if !self.offsets.is_empty() {
                self.offsets64.reserve(self.offsets.len() + 1);
                for &off32 in self.offsets.iter() {
                    self.offsets64.push(u64::from(off32));
                }
                self.offsets = Vec::new();
            }
            self.offsets64.push(offset);
        }
    }
    fn add_chunk_samps(&mut self, count: usize) {
        self.cmap.push(count as u32);
    }

    fn write_stts(&mut self, bw: &mut dyn ByteIO, raw_audio: bool, pktd: u64) -> MuxerResult<()> {
        if !raw_audio && self.pts.len() > 1 {
            let mut nentries = 1;
            let mut prev_delta = self.pts[1] - self.pts[0];
            for w in self.pts.windows(2).skip(1) {
                let delta = w[1] - w[0];
                if delta != prev_delta {
                    nentries += 1;
                    prev_delta = delta;
                }
            }
            bw.write_u32be(nentries as u32)?;
            let mut prev_delta = self.pts[1] - self.pts[0];
            let mut run = 1;
            for w in self.pts.windows(2).skip(1) {
                let delta = w[1] - w[0];
                if delta != prev_delta {
                    bw.write_u32be(run * (pktd as u32))?;
                    bw.write_u32be((prev_delta / pktd) as u32)?;
                    run = 1;
                    prev_delta = delta;
                } else {
                    run += 1;
                }
            }
            bw.write_u32be((run + 1) * (pktd as u32))?;
            bw.write_u32be((prev_delta / pktd) as u32)?;
        } else {
            bw.write_u32be(1)?;
            bw.write_u32be((*self.pts.last().unwrap_or(&0)) as u32)?;
            bw.write_u32be(1)?;
        }
        Ok(())
    }
    fn write_stsc(&mut self, bw: &mut dyn ByteIO) -> MuxerResult<()> {
        if self.cmap.is_empty() {
            bw.write_u32be(1)?; // number of entries
            bw.write_u32be(1)?; // first chunk
            bw.write_u32be(1)?; // samples per chunk
            bw.write_u32be(1)?; // sample description ID
        } else {
            let mut len = 0u32;
            let mut prev = 0;
            for &nsamps in self.cmap.iter() {
                if nsamps != prev {
                    prev = nsamps;
                    len += 1;
                }
            }
            bw.write_u32be(len)?;
            let mut prev = 0;
            for (i, &entry) in self.cmap.iter().enumerate() {
                if entry != prev {
                    bw.write_u32be(i as u32 + 1)?;
                    bw.write_u32be(entry)?; // samples per chunk
                    bw.write_u32be(1)?; // sample description ID
                    prev = entry;
                }
            }
        }
        Ok(())
    }
    fn write_stsz(&mut self, bw: &mut dyn ByteIO, raw_audio: bool) -> MuxerResult<()> {
        if !raw_audio {
            bw.write_u32be(0)?; // sample size
            bw.write_u32be(self.sizes.len() as u32)?;
            bw.write_u32be_arr(&self.sizes)?;
        } else {
            bw.write_u32be(1)?;
            let tot_size = self.sizes.iter().sum();
            bw.write_u32be(tot_size)?;
        }
        Ok(())
    }
    fn write_chunk_offsets(&mut self, bw: &mut dyn ByteIO) -> MuxerResult<()> {
        if self.offsets64.is_empty() {
            let stco = AtomMarker::start(bw, b"stco")?;
            bw.write_byte(0)?;  // version
            bw.write_u24be(0)?; // flags
            bw.write_u32be(self.offsets.len() as u32)?;
            bw.write_u32be_arr(&self.offsets)?;
            stco.end(bw)?;
        } else {
            let co64 = AtomMarker::start(bw, b"co64")?;
            bw.write_byte(0)?;  // version
            bw.write_u24be(0)?; // flags
            bw.write_u32be(self.offsets64.len() as u32)?;
            for &off in self.offsets64.iter() {
                bw.write_u64be(off)?;
            }
            co64.end(bw)?;
        }
        Ok(())
    }
    fn write_stss(&mut self, bw: &mut dyn ByteIO) -> MuxerResult<()> {
        bw.write_u32be(self.keyframes.len() as u32)?;
        bw.write_u32be_arr(&self.keyframes)?;
        Ok(())
    }
}

trait TrackHandler {
    fn write_chunk(&mut self, bw: &mut dyn ByteIO, ca: &mut ChunkAccount, pkt: NAPacket) -> MuxerResult<()>;
    fn flush_chunks(&mut self, bw: &mut dyn ByteIO, ca: &mut ChunkAccount) -> MuxerResult<()>;
    fn get_time_info(&self) -> (u64, u32);
    fn get_dimensions(&self) -> (usize, usize);
    fn get_pkt_duration(&self) -> u64;
    fn write_media_info(&mut self, bw: &mut dyn ByteIO) -> MuxerResult<()>;
    fn write_descriptor(&mut self, bw: &mut dyn ByteIO) -> MuxerResult<()>;
}

struct Track {
    id:         u32,
    mtype:      StreamType,
    handler:    Box<dyn TrackHandler>,
    ca:         ChunkAccount,
    raw_audio:  bool,
}

impl Track {
    fn write_trak(&mut self, bw: &mut dyn ByteIO) -> MuxerResult<()> {
        let trak = AtomMarker::start(bw, b"trak")?;

        let tkhd = AtomMarker::start(bw, b"tkhd")?;
        bw.write_byte(0)?;  // version
        bw.write_u24be(0xF)?; // flags (from LSB) - enabled, in movie, in preview, in poster
        bw.write_u32be(0)?; // creation time
        bw.write_u32be(0)?; // modification time
        bw.write_u32be(self.id)?;
        bw.write_u32be(0)?; // reserved
        let (nduration, tbase) = self.handler.get_time_info();
        let duration = NATimeInfo::rescale_ts(nduration, 1, tbase, 1, TIMESCALE) as u32;
        bw.write_u32be(duration)?; // duration
        bw.write_buf(&[0; 8])?; // reserved
        bw.write_u16be(0)?; // layer
        bw.write_u16be(0)?; // alternate group
        bw.write_u16be(if self.mtype != StreamType::Audio { 0 } else { 0x00FF })?; // volume
        bw.write_u16be(0)?; // reserved
        write_matrix_structure(bw)?;
        let (width, height) = self.handler.get_dimensions();
        bw.write_u32be(width as u32)?;
        bw.write_u32be(height as u32)?;
        tkhd.end(bw)?;

        let mdia = AtomMarker::start(bw, b"mdia")?;
        let mdhd = AtomMarker::start(bw, b"mdhd")?;
        bw.write_byte(0)?;  // version
        bw.write_u24be(0)?; // flags
        bw.write_u32be(0)?; // creation time
        bw.write_u32be(0)?; // modification time
        let (duration, time_scale) = self.handler.get_time_info();
        bw.write_u32be(time_scale)?;
        bw.write_u32be(duration as u32)?;
        bw.write_u16be(0)?; // language
        bw.write_u16be(0)?; // quality
        mdhd.end(bw)?;
        let hdlr = AtomMarker::start(bw, b"hdlr")?;
        bw.write_byte(0)?;  // version
        bw.write_u24be(0)?; // flags
        bw.write_buf(b"mhlr")?; // component type = media
        let hd = match self.mtype {
                StreamType::Video => b"vide",
                StreamType::Audio => b"soun",
                _ => unimplemented!(),
            };
        bw.write_buf(hd)?;
        bw.write_buf(b"appl")?; // component manufacturer
        bw.write_u32be(0)?; // component flags
        bw.write_u32be(0)?; // component flags mask
        let comp_string = match self.mtype {
                StreamType::Video => b"Video handler",
                StreamType::Audio => b"Audio handler",
                _ => unimplemented!(),
            };
        bw.write_pas_str(comp_string)?;
        hdlr.end(bw)?;

        let minf = AtomMarker::start(bw, b"minf")?;
        self.handler.write_media_info(bw)?;

        let hdlr = AtomMarker::start(bw, b"hdlr")?;
        bw.write_byte(0)?;  // version
        bw.write_u24be(0)?; // flags
        bw.write_buf(b"dhlr")?; // component type = data handler
        bw.write_buf(b"alis")?;
        bw.write_buf(b"appl")?; // component manufacturer
        bw.write_u32be(0)?; // component flags
        bw.write_u32be(0)?; // component flags mask
        bw.write_pas_str(b"Apple Alias Data Handler")?;
        hdlr.end(bw)?;

        let dinf = AtomMarker::start(bw, b"dinf")?;
        let dref = AtomMarker::start(bw, b"dref")?;
        bw.write_byte(0)?;  // version
        bw.write_u24be(0)?; // flags
        bw.write_u32be(1)?; // number of entries
        let alis = AtomMarker::start(bw, b"alis")?;
        bw.write_byte(0)?;  // version
        bw.write_u24be(1)?; // flags
        alis.end(bw)?;
        dref.end(bw)?;
        dinf.end(bw)?;

        let stbl = AtomMarker::start(bw, b"stbl")?;

        let stsd = AtomMarker::start(bw, b"stsd")?;
        bw.write_byte(0)?;  // version
        bw.write_u24be(0)?; // flags
        bw.write_u32be(1)?; // number of entries
        self.handler.write_descriptor(bw)?;
        stsd.end(bw)?;

        let pktd = self.handler.get_pkt_duration();
        let stts = AtomMarker::start(bw, b"stts")?;
        bw.write_byte(0)?;  // version
        bw.write_u24be(0)?; // flags
        self.ca.write_stts(bw, self.raw_audio, pktd)?;
        stts.end(bw)?;

        let stsc = AtomMarker::start(bw, b"stsc")?;
        bw.write_byte(0)?;  // version
        bw.write_u24be(0)?; // flags
        self.ca.write_stsc(bw)?;
        stsc.end(bw)?;

        let stsz = AtomMarker::start(bw, b"stsz")?;
        bw.write_byte(0)?;  // version
        bw.write_u24be(0)?; // flags
        self.ca.write_stsz(bw, self.raw_audio)?;
        stsz.end(bw)?;

        if !self.raw_audio && !self.ca.keyframes.is_empty() {
            let stss = AtomMarker::start(bw, b"stss")?;
            bw.write_byte(0)?;  // version
            bw.write_u24be(0)?; // flags
            self.ca.write_stss(bw)?;
            stss.end(bw)?;
        }

        self.ca.write_chunk_offsets(bw)?;

        stbl.end(bw)?;
        minf.end(bw)?;
        mdia.end(bw)?;
        trak.end(bw)?;
        Ok(())
    }
}

struct QTMuxer<'a> {
    bw:             &'a mut dyn ByteIO,
    tracks:         Vec<Track>,
    forced_tags:    Vec<ForcedTag>,
}

impl<'a> QTMuxer<'a> {
    fn new(bw: &'a mut dyn ByteIO) -> Self {
        Self {
            bw,
            tracks:         Vec::new(),
            forced_tags:    Vec::new(),
        }
    }
}

impl<'a> MuxCore<'a> for QTMuxer<'a> {
    fn create(&mut self, strmgr: &StreamManager) -> MuxerResult<()> {
        if strmgr.get_num_streams() == 0 {
            return Err(MuxerError::InvalidArgument);
        }
        if strmgr.get_num_streams() > 99 {
            return Err(MuxerError::UnsupportedFormat);
        }
        let mut trk_id = 1;
        for (strno, strm) in strmgr.iter().enumerate() {
            let forced_tag = self.forced_tags.iter().find(|tag| tag.stream == strno as u32).map(|tag| tag.tag);
            let mtype = strm.get_media_type();
            match mtype {
                StreamType::Video => {
                    self.tracks.push(Track {
                        id:         trk_id,
                        mtype,
                        handler:    Box::new(VideoTrackHandler::new(strm, forced_tag)?),
                        ca:         ChunkAccount::new(),
                        raw_audio:  false,
                    });
                    trk_id += 1;
                },
                StreamType::Audio => {
                    let raw_audio = is_raw_audio(strm.get_info().get_name());
                    let handler: Box<dyn TrackHandler> = if raw_audio {
                            Box::new(RawAudioTrackHandler::new(strm)?)
                        } else {
                            Box::new(AudioTrackHandler::new(strm, forced_tag)?)
                        };
                    self.tracks.push(Track {
                        id:         trk_id,
                        mtype,
                        handler,
                        ca:         ChunkAccount::new(),
                        raw_audio,
                    });
                    trk_id += 1;
                },
                _ => return Err(MuxerError::UnsupportedFormat),
            }
        }

        let marker = AtomMarker::start(self.bw, b"wide")?;
        marker.end(self.bw)?;
        let marker = AtomMarker::start(self.bw, b"mdat")?;
        marker.end(self.bw)?;

        Ok(())
    }
    fn mux_frame(&mut self, _strmgr: &StreamManager, pkt: NAPacket) -> MuxerResult<()> {
        if self.tracks.is_empty() {
            return Err(MuxerError::NotCreated);
        }
        let stream = pkt.get_stream();
        let str_num = stream.get_num();
        if str_num >= self.tracks.len() {
            return Err(MuxerError::UnsupportedFormat);
        }
        let trk = &mut self.tracks[str_num];

        trk.handler.write_chunk(self.bw, &mut trk.ca, pkt)?;

        Ok(())
    }
    fn flush(&mut self) -> MuxerResult<()> {
        for trk in self.tracks.iter_mut() {
            trk.handler.flush_chunks(self.bw, &mut trk.ca)?;
        }
        Ok(())
    }
    fn end(&mut self) -> MuxerResult<()> {
        if self.tracks.is_empty() || self.bw.tell() <= 16 {
            return Err(MuxerError::NotCreated);
        }

        self.flush()?;

        let mdat_size = self.bw.tell() - 8;
        if mdat_size < (1 << 32) {
            self.bw.seek(SeekFrom::Start(8))?;
            self.bw.write_u32be(mdat_size as u32)?;
        } else {
            self.bw.seek(SeekFrom::Start(0))?;
            self.bw.write_u32be(1)?;
            self.bw.write_buf(b"mdat")?;
            self.bw.write_u64be(mdat_size + 8)?;
        }
        self.bw.seek(SeekFrom::End(0))?;

        let moov = AtomMarker::start(self.bw, b"moov")?;
        let mvhd = AtomMarker::start(self.bw, b"mvhd")?;
        self.bw.write_byte(0)?;  // version
        self.bw.write_u24be(0)?; // flags
        self.bw.write_u32be(0)?; // creation time
        self.bw.write_u32be(0)?; // modification time
        self.bw.write_u32be(TIMESCALE)?;
        let mut duration = 0;
        for trk in self.tracks.iter() {
            let (nduration, tbase) = trk.handler.get_time_info();
            let dur = NATimeInfo::rescale_ts(nduration, 1, tbase, 1, TIMESCALE) as u32;
            duration = duration.max(dur);
        }
        self.bw.write_u32be(duration)?;
        self.bw.write_u32be(0x00010000)?; // preferred rate = 1.0
        self.bw.write_u16be(0x00FF)?; // preferred volume = 1.0
        self.bw.write_buf(&[0; 10])?; // reserved
        write_matrix_structure(self.bw)?;
        self.bw.write_u32be(0)?; // preview time
        self.bw.write_u32be(0)?; // preview duration
        self.bw.write_u32be(0)?; // poster time
        self.bw.write_u32be(0)?; // selection time
        self.bw.write_u32be(0)?; // selection duration
        self.bw.write_u32be(0)?; // current time
        self.bw.write_u32be(self.tracks.len() as u32 + 1)?; // next track ID
        mvhd.end(self.bw)?;

        for trk in self.tracks.iter_mut() {
            if !trk.ca.is_empty() {
                trk.write_trak(self.bw)?;
            }
        }

        let udta = AtomMarker::start(self.bw, b"udta")?;
        let writer = AtomMarker::start(self.bw, b"\xA9wrt")?;
        let wr_str = b"Generated by NihAV muxer. Sorry about that!";
        self.bw.write_u16be(wr_str.len() as u16)?;
        self.bw.write_u16be(0)?; // language code
        self.bw.write_buf(wr_str)?;
        writer.end(self.bw)?;
        self.bw.write_u32be(0)?; // for historical reasons
        udta.end(self.bw)?;

        moov.end(self.bw)?;

        self.bw.flush()?;

        Ok(())
    }
}

const MUXER_OPTS: &[NAOptionDefinition] = &[
    NAOptionDefinition {
        name: FORCE_STREAM_TAG_OPTION, description: FORCE_STREAM_TAG_OPTION_DESC,
        opt_type: NAOptionDefinitionType::String(None) },
];

fn parse_tag(src: &[u8]) -> Option<ForcedTag> {
    if !src.starts_with(b"stream") {
        return None;
    }
    let mut pos = 6;
    let mut stream = 0;
    while pos < src.len() && src[pos].is_ascii_digit() {
        stream = stream * 4 + u32::from(src[pos] - b'0');
        if stream > (1 << 24) {
            return None;
        }
        pos += 1;
    }
    if src.len() - pos != 3 + 2 * 4 || !src[pos..].starts_with(b"tag") {
        return None;
    }
    pos += 3;
    let mut tag = [0; 4];
    for (sym, pair) in tag.iter_mut().zip(src[pos..].chunks_exact(2)) {
        let c0 = match pair[0] {
                b'0'..=b'9' => pair[0] - b'0',
                b'a'..=b'f' => pair[0] - b'a' + 10,
                b'A'..=b'F' => pair[0] - b'A' + 10,
                _ => return None,
            };
        let c1 = match pair[1] {
                b'0'..=b'9' => pair[1] - b'0',
                b'a'..=b'f' => pair[1] - b'a' + 10,
                b'A'..=b'F' => pair[1] - b'A' + 10,
                _ => return None,
            };
        *sym = (c0 << 4) | c1;
    }
    Some(ForcedTag{ stream, tag })
}

impl<'a> NAOptionHandler for QTMuxer<'a> {
    fn get_supported_options(&self) -> &[NAOptionDefinition] { MUXER_OPTS }
    fn set_options(&mut self, options: &[NAOption]) {
        for opt in options.iter() {
            if let NAOption { name: "stream_tag" , value: NAValue::String(ref stag) } = opt {
                if let Some(new_tag) = parse_tag(stag.as_bytes()) {
                    let mut found = false;
                    for tag in self.forced_tags.iter_mut() {
                        if new_tag.stream == tag.stream {
                            tag.tag = new_tag.tag;
                            found = true;
                            break;
                        }
                    }
                    if !found {
                        self.forced_tags.push(new_tag);
                    }
                }
            }
        }
    }
    fn query_option_value(&self, _name: &str) -> Option<NAValue> { None }
}

pub struct MovMuxerCreator {}

impl MuxerCreator for MovMuxerCreator {
    fn new_muxer<'a>(&self, bw: &'a mut dyn ByteIO) -> Box<dyn MuxCore<'a> + 'a> {
        Box::new(QTMuxer::new(bw))
    }
    fn get_name(&self) -> &'static str { "mov" }
    fn get_capabilities(&self) -> MuxerCapabilities { MuxerCapabilities::Universal }
    fn get_quirks(&self) -> MuxerQuirks { MuxerQuirks(MUX_QUIRK_UNSYNC | MUX_QUIRK_GLOBAL_PAL) }
}

#[cfg(test)]
mod test {
    use nihav_core::codecs::*;
    use nihav_core::demuxers::*;
    use nihav_core::muxers::*;
    use nihav_codec_support::test::enc_video::*;
    use crate::*;

    #[test]
    fn test_mov_muxer_video_only() {
        let mut dmx_reg = RegisteredDemuxers::new();
        generic_register_all_demuxers(&mut dmx_reg);
        // sample from King's Quest VI Macintosh edition
        let dec_config = DecoderTestParams {
                demuxer:        "mov-macbin",
                in_name:        "assets/QT/Halfdome.bin",
                limit:          None,
                stream_type:    StreamType::None,
                dmx_reg, dec_reg: RegisteredDecoders::new(),
            };
        let mut mux_reg = RegisteredMuxers::new();
        generic_register_all_muxers(&mut mux_reg);
        test_remuxing_md5(&dec_config, "mov", &mux_reg,
                          [0x842382db, 0xa6d23d85, 0x4fa746c2, 0x5c8ede5b]);
    }

    #[test]
    fn test_mov_muxer_audio_only() {
        let mut dmx_reg = RegisteredDemuxers::new();
        generic_register_all_demuxers(&mut dmx_reg);
        // sample from The Wonders of Electricity: An Adventure in Safety
        let dec_config = DecoderTestParams {
                demuxer:        "mov-resfork",
                in_name:        "assets/QT/car.mov",
                limit:          None,
                stream_type:    StreamType::None,
                dmx_reg, dec_reg: RegisteredDecoders::new(),
            };
        let mut mux_reg = RegisteredMuxers::new();
        generic_register_all_muxers(&mut mux_reg);
        test_remuxing_md5(&dec_config, "mov", &mux_reg,
                          [0xe6bf92a2, 0x6a9871e5, 0x98fc2dcb, 0xe032c119]);
    }

    #[test]
    fn test_mov_muxer_video_and_audio() {
        let mut dmx_reg = RegisteredDemuxers::new();
        generic_register_all_demuxers(&mut dmx_reg);
        // samples from https://samples.mplayerhq.hu/V-codecs/QTRLE/tidemo1-24bit-rle.mov
        let dec_config = DecoderTestParams {
                demuxer:        "mov",
                in_name:        "assets/QT/tidemo1-24bit-rle.mov",
                limit:          None,
                stream_type:    StreamType::None,
                dmx_reg, dec_reg: RegisteredDecoders::new(),
            };
        let mut mux_reg = RegisteredMuxers::new();
        generic_register_all_muxers(&mut mux_reg);
        test_remuxing_md5(&dec_config, "mov", &mux_reg,
                          [0x4f91995d, 0x7ba8b830, 0x5efe9f8c, 0xd1779301]);
    }
}
