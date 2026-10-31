use nihav_core::demuxers::*;
use nihav_registry::register;
use nihav_core::demuxers::DemuxerError::*;
use nihav_codec_support::codecs::msstructs::*;
use std::str::FromStr;

struct PalInfo {
    pal:        Arc<[u8; 1024]>,
    changed:    bool,
}

#[derive(Clone,Copy,Debug,Default, PartialEq)]
enum DemuxingMode {
    #[default]
    Linear,
    NonInterleaved,
    SaturnAVI,
}

#[derive(Clone,Copy,Default)]
struct ChunkIndex {
    time:   u64,
    offset: u64,
}

#[derive(Clone,Copy,Default)]
struct RIFFSegment {
    pos:        u64,
    size:       usize,
    movi_pos:   u64,
    movi_size:  usize,
}

impl RIFFSegment {
    fn contains(&self, pos: u64) -> bool {
        pos >= self.movi_pos && pos < self.movi_pos + (self.movi_size as u64)
    }
    fn get_end(&self) -> u64 {
        self.pos + (self.size as u64)
    }
}

#[derive(Default)]
struct AVIStream {
    strm_no:        u8,
    got_strf:       bool,
    strm_type:      Option<StreamType>,
    pal:            Option<PalInfo>,
    cur_frame:      u64,
    odml_idx:       Vec<u64>,
    handler:        [u8; 4],
    strm_duration:  u32,
    tb_num:         u32,
    tb_den:         u32,
    aud_brate:      u32,
    ts:             Option<u64>,

    index:          Vec<ChunkIndex>,

    print_chunks:   bool,
    depth:          u8,
}

impl AVIStream {
    fn parse_strh(&mut self, src: &mut dyn ByteIO, _strmgr: &mut StreamManager, _seek_index: &mut SeekIndex, size: usize) -> DemuxerResult<()> {
        if size < 0x30 { return Err(InvalidData); }
        validate!(!self.got_strf);
        let tag                 = src.read_tag()?; //stream type
        self.handler            = src.read_tag()?; //handler(fourcc)
                                  src.read_u32le()?; //flags
                                  src.read_skip(2)?; //priority
                                  src.read_skip(2)?; //language
                                  src.read_skip(4)?; //initial frames
        self.tb_num             = src.read_u32le()?; //scale
        self.tb_den             = src.read_u32le()?; //rate
                                  src.read_skip(4)?; //start
        self.strm_duration      = src.read_u32le()?;
                                  src.read_skip(4)?; //buf size
                                  src.read_skip(4)?; //quality
                                  src.read_skip(4)?; //sample size
        /*let _a                  = src.read_u16le()?;
        let _b                  = src.read_u16le()?;
        let _c                  = src.read_u16le()?;
        let _d                  = src.read_u16le()?;*/

                                  src.read_skip(size - 0x30)?;

        // padding streams should be ignored
        if &tag == b"pads" {
            self.got_strf = true;
            return Ok(());
        }

        self.strm_type = match &tag {
                b"vids" => Some(StreamType::Video),
                b"auds" => Some(StreamType::Audio),
                _       => Some(StreamType::Data),
            };

        Ok(())
    }

    fn parse_strf(&mut self, src: &mut dyn ByteIO, strmgr: &mut StreamManager, _seek_index: &mut SeekIndex, size: usize) -> DemuxerResult<()> {
        validate!(self.strm_type.is_some() && !self.got_strf);
        match self.strm_type {
            Some(StreamType::Video) => self.parse_strf_vids(src, strmgr, size)?,
            Some(StreamType::Audio) => self.parse_strf_auds(src, strmgr, size)?,
            Some(_)                 => self.parse_strf_xxxx(src, strmgr, size)?,
            _ => unreachable!(),
        }
        self.got_strf = true;
        Ok(())
    }
    fn parse_strf_vids(&mut self, src: &mut dyn ByteIO, strmgr: &mut StreamManager, size: usize) -> DemuxerResult<()> {
        if size < 40 { return Err(InvalidData); }
        let strf_end = src.tell() + (size as u64);
        let mut header = MSBitmapInfo::read(src)?;
        if header.validate().is_err() { return Err(InvalidData); }
        let mut vhdr = header.get_video_info();
        let is_mvi = matches!(&self.handler, b"MVI1" | b"mvi1" | b"MVI2" | b"mvi2");
        let cname = if is_mvi {
                if self.handler[3] == b'1' { "mvi1" } else { "mvi2" }
            } else if find_raw_rgb_fmt(&header, &mut vhdr) {
                "rawvideo-ms"
            } else if find_raw_yuv_fmt(&header.compression, &mut vhdr) {
                "rawvideo"
            } else {
                match register::find_codec_from_avi_fourcc(&header.compression) {
                    None => "unknown",
                    Some(name) => name,
                }
            };
        let vci = NACodecTypeInfo::Video(vhdr);
        let mut edata: Option<Vec<u8>> = None;
        if src.tell() < strf_end {
            let edata_size = (strf_end - src.tell()) as usize;
            edata = Self::read_extradata(src, edata_size)?;
            if header.colors > 0 {
                if let Some(ref buf) = edata {
                    let mut pal = [0u8; 1024];
                    for (dpal, spal) in pal.chunks_mut(4).take(header.colors).zip(buf.chunks(4)) {
                        dpal[0] = spal[2];
                        dpal[1] = spal[1];
                        dpal[2] = spal[0];
                        dpal[3] = 0;
                    }
                    self.pal = Some(PalInfo { pal: Arc::new(pal), changed: true });
                }
            }
        } else if header.colors > 0 {
            if let Some(ref buf) = header.extradata {
                let mut pal = [0u8; 1024];
                for (dpal, spal) in pal.chunks_mut(4).take(header.colors).zip(buf.chunks(4)) {
                    dpal[0] = spal[2];
                    dpal[1] = spal[1];
                    dpal[2] = spal[0];
                    dpal[3] = 0;
                }
                self.pal = Some(PalInfo { pal: Arc::new(pal), changed: true });
            }
        }
        if is_mvi {
            if let Some(ref mut dta) = edata {
                for (i, &c) in header.compression.iter().enumerate() {
                    dta.insert(i, c);
                }
            } else {
                edata = Some(header.compression.to_vec());
            }
        } else if header.extradata.is_some() {
            edata = header.take_extradata();
        }
        validate!(src.tell() <= strf_end);
        src.seek(SeekFrom::Start(strf_end))?;
        let vinfo = NACodecInfo::new(cname, vci, edata);
        let res = strmgr.add_stream(NAStream::new(StreamType::Video, u32::from(self.strm_no), vinfo, self.tb_num, self.tb_den, u64::from(self.strm_duration)));
        if res.is_none() { return Err(MemoryError); }
        Ok(())
    }
    fn parse_strf_auds(&mut self, src: &mut dyn ByteIO, strmgr: &mut StreamManager, size: usize) -> DemuxerResult<()> {
        if size < 16 { return Err(InvalidData); }
        let mut hdr = MSWaveFormat::read(src, size)?;
        self.aud_brate = hdr.avg_bytes_per_sec;

        let ahdr = hdr.get_audio_info();
        let edata = hdr.take_extradata();
        let cname = match register::find_codec_from_wav_twocc(hdr.format_tag) {
                        None => "unknown",
                        Some(name) => name,
                    };
        let ainfo = NACodecInfo::new(cname, NACodecTypeInfo::Audio(ahdr), edata);
        let res = strmgr.add_stream(NAStream::new(StreamType::Audio, u32::from(self.strm_no), ainfo, self.tb_num, self.tb_den, u64::from(self.strm_duration)));
        if res.is_none() { return Err(MemoryError); }
        Ok(())
    }
    fn parse_strf_xxxx(&mut self, src: &mut dyn ByteIO, strmgr: &mut StreamManager, size: usize) -> DemuxerResult<()> {
        let edata = Self::read_extradata(src, size)?;
        let info = NACodecInfo::new("unknown", NACodecTypeInfo::None, edata);
        let res = strmgr.add_stream(NAStream::new(StreamType::Data, u32::from(self.strm_no), info, self.tb_num, self.tb_den, u64::from(self.strm_duration)));
        if res.is_none() { return Err(MemoryError); }
        Ok(())
    }
    fn read_extradata(src: &mut dyn ByteIO, size: usize) -> DemuxerResult<Option<Vec<u8>>> {
        if size == 0 { return Ok(None); }
        let mut edvec: Vec<u8> = vec![0; size];
        src.read_buf(&mut edvec)?;
        Ok(Some(edvec))
    }

    fn parse_indx(&mut self, src: &mut dyn ByteIO, _strmgr: &mut StreamManager, _seek_index: &mut SeekIndex, size: usize) -> DemuxerResult<()> {
        validate!(size >= 24);
        let entry_size              = src.read_u16le()? as usize;
        if entry_size != 4 {
            return Ok(());
        }
        let sub_type                = src.read_byte()?;
        let idx_type                = src.read_byte()?;
        validate!(sub_type == 0 && idx_type == 0);
        let entries                 = src.read_u32le()? as usize;
        validate!(entries > 0);
        validate!(size >= 24 + entries * 4 * entry_size);
                                      src.read_tag()?; //chunk id
                                      src.read_skip(12)?; // reserved
        for _ in 0..entries {
            let offset              = src.read_u64le()?;
            let _idx_len            = src.read_u32le()?;
            let _nframes            = src.read_u32le()?;
            self.odml_idx.push(offset);
        }
        Ok(())
    }

    fn parse_palette_change(&mut self, src: &mut dyn ByteIO, size: usize) -> DemuxerResult<()> {
        if let Some(ref mut pe) = self.pal {
            let mut newpal = *pe.pal;
            let mut data_left = size;
            while data_left > 0 {
                validate!(data_left >= 8);
                let start_clr       = src.read_byte()? as usize;
                let len             = src.read_byte()? as usize;
                let len = if len == 0 { 256 } else { len };
                let _flags          = src.read_u16le()?;
                validate!(start_clr + len <= 256);
                let change_size = len * 4 + 4;
                validate!(change_size <= data_left);
                for i in start_clr..(start_clr + len) {
                    newpal[i * 4]       = src.read_byte()?;
                    newpal[i * 4 + 1]   = src.read_byte()?;
                    newpal[i * 4 + 2]   = src.read_byte()?;
                    newpal[i * 4 + 3]   = 0;
                                      src.read_byte()?; // flags
                }
                data_left -= change_size;
            }
            pe.pal = Arc::new(newpal);
            pe.changed = true;
        } else {
                                      src.read_skip(size)?;
        }
        Ok(())
    }
}

#[derive(Default)]
struct AVIState {
    odml:           bool,
    odml_riff:      Vec<RIFFSegment>,
    key_offs:       Vec<u64>,
    size:           usize,
    movi_size:      usize,
    movi_pos:       u64,
    movi_orig:      usize,

    streams:        Vec<AVIStream>,
    nom_streams:    usize,

    demux_mode:     DemuxingMode,

    chunk_offs:     Vec<u64>,
    cur_chunk:      usize,
    iddx_pos:       u64,
    iddx_size:      usize,

    print_chunks:   bool,
    depth:          u8,
}

impl AVIState {
    fn new() -> Self {
        Self::default()
    }

    fn parse_hdrl(&mut self, src: &mut dyn ByteIO, strmgr: &mut StreamManager, seek_index: &mut SeekIndex, size: usize) -> DemuxerResult<()> {
        validate!(size > 20);
        let end = src.tell() + (size as u64) - 4;
        parse_chunks(self, src, strmgr, seek_index, HDRL_CHUNKS, end)
    }
    fn parse_movi(&mut self, src: &mut dyn ByteIO, _strmgr: &mut StreamManager, _seek_index: &mut SeekIndex, size: usize) -> DemuxerResult<()> {
        validate!(size > 10);
        self.movi_size = size - 4;
        self.movi_orig = self.movi_size;
        self.movi_pos  = src.tell();

        self.odml_riff[0].movi_pos  = self.movi_pos;
        self.odml_riff[0].movi_size = self.movi_size;
        Ok(())
    }
    fn parse_idx1(&mut self, src: &mut dyn ByteIO, strmgr: &mut StreamManager, seek_index: &mut SeekIndex, size: usize) -> DemuxerResult<()> {
        if seek_index.skip_index { return Ok(()); }
        if let Ok((_size, ni)) = parse_idx1(src, strmgr, seek_index, size, self.movi_pos, &mut self.key_offs, &mut self.streams) {
            if ni {
                self.demux_mode = DemuxingMode::NonInterleaved;
            }
        }
        Ok(())
    }
    fn parse_iddx(&mut self, src: &mut dyn ByteIO, _strmgr: &mut StreamManager, _seek_index: &mut SeekIndex, iddx_size: usize) -> DemuxerResult<()> {
        self.demux_mode = DemuxingMode::SaturnAVI;
        self.iddx_pos   = src.tell();
        self.iddx_size  = iddx_size;
        Ok(())
    }

    fn parse_avih(&mut self, src: &mut dyn ByteIO, _strmgr: &mut StreamManager, _seek_index: &mut SeekIndex, size: usize) -> DemuxerResult<()> {
        validate!(size >= 0x38);
        let _timebase = src.read_u32le()?; //microsec per frame
        src.read_skip(4)?; //max frame size
        src.read_skip(4)?; //padding
        src.read_u32le()?; //flags
        let _frames = src.read_u32le()?; //frames
        src.read_skip(4)?; //initial frames
        let streams = src.read_u32le()?; //streams
        if streams > 100 { return Err(InvalidData); }
        self.nom_streams = streams as usize;

        src.read_skip(4)?; //buf size
        let _width = src.read_u32le()?; //width
        let _height = src.read_u32le()? as i32; //height
        src.read_skip(16)?; //reserved

        Ok(())
    }
    fn parse_strl(&mut self, src: &mut dyn ByteIO, strmgr: &mut StreamManager, seek_index: &mut SeekIndex, size: usize) -> DemuxerResult<()> {
        validate!(size >= 20);
        let stream_no = self.streams.len();
        validate!(stream_no < 100);
        let mut stream = AVIStream { strm_no: stream_no as u8, ..Default::default() };
        let end = src.tell() + size as u64 - 4;
        stream.print_chunks = self.print_chunks;
        stream.depth = self.depth;
        parse_chunks(&mut stream, src, strmgr, seek_index, STRL_CHUNKS, end)?;
        validate!(stream.got_strf);
        if !stream.odml_idx.is_empty() {
            self.odml = true;
        }
        self.streams.push(stream);
        Ok(())
    }
}

struct AVIDemuxer<'a> {
    src:            &'a mut dyn ByteIO,
    state:          AVIState,
    print_chunks:   bool,
}

#[derive(Debug,Clone,Copy,PartialEq)]
enum RIFFTag {
    Chunk([u8; 4]),
    List([u8; 4]),
}

struct ChunkHandler<T> {
    tag:   RIFFTag,
    parse: fn(obj: &mut T, src: &mut dyn ByteIO, strmgr: &mut StreamManager, seek_index: &mut SeekIndex, size: usize) -> DemuxerResult<()>,
}

trait DebugChunks {
    fn inc_depth(&mut self);
    fn dec_depth(&mut self);
    fn get_depth(&self) -> u8;
    fn is_debug(&self) -> bool;
}

impl DebugChunks for AVIState {
    fn inc_depth(&mut self) { self.depth += 1; }
    fn dec_depth(&mut self) { self.depth -= 1; }
    fn get_depth(&self) -> u8 { self.depth }
    fn is_debug(&self) -> bool { self.print_chunks }
}

impl DebugChunks for AVIStream {
    fn inc_depth(&mut self) { self.depth += 1; }
    fn dec_depth(&mut self) { self.depth -= 1; }
    fn get_depth(&self) -> u8 { self.depth }
    fn is_debug(&self) -> bool { self.print_chunks }
}

fn parse_chunks<T: DebugChunks>(obj: &mut T, src: &mut dyn ByteIO, strmgr: &mut StreamManager, seek_index: &mut SeekIndex, handlers: &[ChunkHandler<T>], parse_end: u64) -> DemuxerResult<()> {
    obj.inc_depth();
    while src.tell() < parse_end {
        let tag                     = src.read_tag()?;
        let size                    = src.read_u32le()?;
        if obj.is_debug() {
            for _ in 0..obj.get_depth() {
                print!("    ");
            }
            print!("{}{}{}{}", tag[0] as char, tag[1] as char, tag[2] as char, tag[3] as char);
            if &tag == b"LIST" {
                if let Ok(tag2) = src.peek_tag() {
                    print!(" type {}{}{}{}", tag2[0] as char, tag2[1] as char, tag2[2] as char, tag2[3] as char);
                } else {
                    print!(" type ????");
                }
            }
            println!(" size {size:X} @ {:X}", src.tell() - 8);
        }
        let chunk_end = src.tell() + u64::from(size);
        validate!(chunk_end <= parse_end);
        if &tag == b"JUNK" {
                                      src.seek(SeekFrom::Start(chunk_end + (chunk_end & 1)))?;
            continue;
        }
        let ref_tag = if &tag == b"LIST" {
                validate!(size >= 4);
                RIFFTag::List(src.read_tag()?)
            } else { RIFFTag::Chunk(tag) };

        if let Some(handler) = handlers.iter().find(|hdl| hdl.tag == ref_tag) {
            (handler.parse)(obj, &mut *src, strmgr, seek_index, size as usize)?;
            validate!(src.tell() <= chunk_end);
        }
                                      src.seek(SeekFrom::Start(chunk_end))?;
        if (chunk_end & 1) != 0 {
            let b                   = src.peek_byte()?;
            if b == 0 {
                                      src.read_byte()?;
            } else {
                let mut peekbuf = [0; 9];
                if src.peek_buf(&mut peekbuf).is_ok() {
                    let size1 = read_u32le(&peekbuf[4..]).unwrap_or_default();
                    let size2 = read_u32le(&peekbuf[5..]).unwrap_or_default();
                    let end1 = src.tell() + u64::from(size1);
                    let end2 = src.tell() + u64::from(size2);
                    if end1 > parse_end && end2 <= parse_end {
                                      src.read_byte()?;
                    }
                }
            }
        }
    }
    obj.dec_depth();
    Ok(())
}

const AVI_ROOT_CHUNKS: &[ChunkHandler<AVIState>] = &[
    ChunkHandler{ tag: RIFFTag::List( *b"hdrl"), parse: AVIState::parse_hdrl },
    ChunkHandler{ tag: RIFFTag::List( *b"movi"), parse: AVIState::parse_movi },
    ChunkHandler{ tag: RIFFTag::Chunk(*b"idx1"), parse: AVIState::parse_idx1 },
    ChunkHandler{ tag: RIFFTag::Chunk(*b"iddx"), parse: AVIState::parse_iddx },
];

const HDRL_CHUNKS: &[ChunkHandler<AVIState>] = &[
    ChunkHandler{ tag: RIFFTag::Chunk(*b"avih"), parse: AVIState::parse_avih },
    ChunkHandler{ tag: RIFFTag::Chunk(*b"ON2h"), parse: AVIState::parse_avih },
    ChunkHandler{ tag: RIFFTag::List( *b"strl"), parse: AVIState::parse_strl },
];

const STRL_CHUNKS: &[ChunkHandler<AVIStream>] = &[
    ChunkHandler{ tag: RIFFTag::Chunk(*b"strh"), parse: AVIStream::parse_strh },
    ChunkHandler{ tag: RIFFTag::Chunk(*b"strf"), parse: AVIStream::parse_strf },
    ChunkHandler{ tag: RIFFTag::Chunk(*b"indx"), parse: AVIStream::parse_indx },
];

impl<'a> DemuxCore<'a> for AVIDemuxer<'a> {
    fn open(&mut self, strmgr: &mut StreamManager, seek_index: &mut SeekIndex) -> DemuxerResult<()> {
        let riff_tag = self.src.read_tag()?;
        let size     = self.src.read_u32le()? as usize;
        if self.print_chunks {
            println!("{}{}{}{} size {size:X} @ {:X}", riff_tag[0] as char, riff_tag[1] as char, riff_tag[2] as char, riff_tag[3] as char, self.src.tell() - 8);
        }
        self.state.odml_riff.push(RIFFSegment { pos: self.src.tell() - 8, size: size + 8, movi_pos: 0, movi_size: 0});
        let avi_tag  = self.src.read_tag()?;
        if !AVI_HEADER_TAGS.contains(&(&riff_tag, &avi_tag)) {
            return Err(InvalidData);
        }
        self.state.print_chunks = self.print_chunks;
        self.state.size = size;

        match parse_chunks(&mut self.state, &mut *self.src, strmgr, seek_index, AVI_ROOT_CHUNKS, size as u64 + 8) {
            Ok(()) => {},
            Err(DemuxerError::EOF) | Err(DemuxerError::IOError) => {},
            Err(err) => return Err(err),
        }

        validate!(self.state.movi_pos != 0);
        validate!(!self.state.streams.is_empty());

        if self.state.odml {
            let mut start = 0;
            let mut last_strm_no = 255;
            for stream in self.state.streams.iter_mut() {
                for &offset in stream.odml_idx.iter() {
                    if self.src.seek(SeekFrom::Start(offset)).is_err() {
                        break;
                    }
                    let ret = self.src.read_tag();
                    if ret.is_err() { break; }
                    let tag = ret.unwrap();
                    let ret = self.src.read_u32le();
                    if ret.is_err() { break; }
                    let size = ret.unwrap() as usize;
                    if &tag[..2] != b"ix" || tag[2] < b'0' || tag[2] > b'9' || tag[3] < b'0' || tag[3] > b'9'{
                        break;
                    }
                    let stream_no = ((tag[2] - b'0') * 10 + (tag[3] - b'0')) as usize;
                    validate!(stream_no == usize::from(stream.strm_no));

                    if last_strm_no != stream_no {
                        start = 0;
                        last_strm_no = stream_no;
                    }
                    let ret = parse_odml_ix(self.src, strmgr, seek_index, stream_no, size, start);
                    if let Ok(new_start) = ret {
                        start = new_start;
                    } else {
                        break;
                    }
                }
            }
        }

        if self.state.demux_mode == DemuxingMode::SaturnAVI {
            validate!(self.state.iddx_pos > 0);
            self.src.seek(SeekFrom::Start(self.state.iddx_pos))?;
            parse_iddx_data(self.src, strmgr, seek_index, self.state.iddx_size, self.state.movi_pos, &mut self.state.key_offs, &mut self.state.chunk_offs)?;
        }

        self.src.seek(SeekFrom::Start(self.state.movi_pos))?;

        Ok(())
    }

    fn get_frame(&mut self, strmgr: &mut StreamManager) -> DemuxerResult<NAPacket> {
        if self.state.movi_size == 0 && self.state.demux_mode != DemuxingMode::NonInterleaved {
            if !self.state.odml {
                return Err(EOF);
            }
            self.try_next_odml_chunk()?;
        }
        loop {
            match self.state.demux_mode {
                DemuxingMode::Linear => {
                    if (self.src.tell() & 1) == 1 {
                        self.src.read_skip(1)?;
                        self.state.movi_size -= 1;
                        if self.state.movi_size == 0 {
                            if !self.state.odml {
                                return Err(EOF);
                            }
                            self.try_next_odml_chunk()?;
                        }
                    }
                },
                DemuxingMode::NonInterleaved => {
                    let mut best_idx = usize::MAX;
                    let mut best_time = u64::MAX;
                    let mut offset = 0;
                    for (n, avi_str) in self.state.streams.iter().enumerate() {
                        if strmgr.is_ignored(n) {
                            continue;
                        }
                        let fno = avi_str.cur_frame as usize;
                        if fno < avi_str.index.len() && avi_str.index[fno].time < best_time {
                            best_idx  = n;
                            best_time = avi_str.index[fno].time;
                            offset    = avi_str.index[fno].offset;
                        }
                    }
                    if best_idx == usize::MAX {
                        return Err(EOF);
                    }
                    let mov_end = self.state.movi_pos + (self.state.movi_orig as u64);
                    validate!(offset >= self.state.movi_pos && offset < mov_end);
                    self.src.seek(SeekFrom::Start(offset))?;
                    self.state.movi_size = (mov_end - offset) as usize;
                },
                DemuxingMode::SaturnAVI => {
                    if self.state.cur_chunk >= self.state.chunk_offs.len() {
                        return Err(EOF);
                    }
                    let off = self.state.chunk_offs[self.state.cur_chunk];
                    let mov_end = self.state.movi_pos + (self.state.movi_orig as u64);
                    validate!(off >= self.state.movi_pos && off < mov_end);
                    self.src.seek(SeekFrom::Start(off))?;
                    self.state.movi_size = (mov_end - off) as usize;
                    self.state.cur_chunk += 1;
                },
            }
            let is_keyframe = self.state.key_offs.binary_search(&self.src.tell()).is_ok();
            let tag = self.src.read_tag()?;
            let size = self.src.read_u32le()? as usize;
            self.state.movi_size = self.state.movi_size.saturating_sub(8);
            if size > self.state.movi_size {
                self.state.movi_size = 0;
                return Err(InvalidData);
            }
            match &tag {
                b"JUNK" => {
                    self.state.movi_size -= size;
                    self.src.read_skip(size)?;
                    if self.state.movi_size == 0 {
                        if !self.state.odml {
                            return Err(EOF);
                        }
                        self.try_next_odml_chunk()?;
                    }
                    continue;
                },
                b"LIST" => {
                    self.state.movi_size -= 4;
                    self.src.read_skip(4)?;
                    if self.state.movi_size == 0 {
                        if !self.state.odml {
                            return Err(EOF);
                        }
                        self.try_next_odml_chunk()?;
                    }
                    continue;
                },
                b"idx1" | &[b'i', b'x', _, _] => {
                    let idx_pos = self.src.tell() - 8;
                    if !self.state.odml {
                        return Err(EOF);
                    }
                    self.src.read_skip(size)?;
                    if idx_pos > self.state.movi_pos {
                        self.try_next_odml_chunk()?;
                    } else {
                        self.state.movi_pos = self.src.tell();
                    }
                    continue;
                },
                _ => {},
            }
            if tag[0] < b'0' || tag[0] > b'9' || tag[1] < b'0' || tag[1] > b'9' {
                return Err(InvalidData);
            }
            let stream_no = usize::from(tag[0] - b'0') * 10 + usize::from(tag[1] - b'0');
            let stream = strmgr.get_stream(stream_no);
            if stream.is_none() || stream_no >= self.state.streams.len() {
                self.src.read_skip(size)?;
                self.state.movi_size -= size;
                continue;
            }
            let cur_stream = &mut self.state.streams[stream_no];
            if tag[2] == b'p' && tag[3] == b'c' {
                cur_stream.parse_palette_change(&mut *self.src, size)?;
                self.state.movi_size -= size;
                if self.state.movi_size == 0 {
                    if !self.state.odml {
                        return Err(EOF);
                    }
                    self.try_next_odml_chunk()?;
                }
                continue;
            }
            let stream = stream.unwrap();
            if size == 0 {
                if self.state.movi_size == 0 {
                    if !self.state.odml {
                        return Err(EOF);
                    }
                    self.try_next_odml_chunk()?;
                }
                continue;
            }
            let (tb_num, _) = stream.get_timebase();
            let mut ts = stream.make_ts(Some(cur_stream.cur_frame), None, None);
            if stream.get_media_type() == StreamType::Audio && tb_num == 1 && stream.get_info().get_name() == "pcm" {
                if cur_stream.cur_frame == 0 {
                    cur_stream.ts = Some(0);
                }
                ts.pts = cur_stream.ts;
                if let Some(ref mut ts) = cur_stream.ts {
                    let info = stream.get_info().get_properties().get_audio_info().unwrap();
                    *ts += (size / info.block_len) as u64;
                }
            }
            let mut pkt = self.src.read_packet(stream, ts, is_keyframe, size)?;
            if let Some(ref mut pe) = cur_stream.pal {
                pkt.add_side_data(NASideData::Palette(pe.changed, pe.pal.clone()));
                pe.changed = false;
            }
            cur_stream.cur_frame += 1;
            self.state.movi_size -= size;

            return Ok(pkt);
        }
    }

    fn seek(&mut self, time: NATimePoint, seek_index: &SeekIndex) -> DemuxerResult<()> {
        let ret = seek_index.find_pos(time);
        if ret.is_none() {
            return Err(DemuxerError::SeekError);
        }
        let seek_info = ret.unwrap();

        if self.state.odml && ((seek_info.pos < self.state.movi_pos) || (seek_info.pos > self.state.movi_pos + (self.state.movi_orig as u64))) {
            let mut found = false;
            for riff_seg in self.state.odml_riff.iter() {
                if riff_seg.contains(seek_info.pos) {
                    found = true;
                    self.state.movi_pos = riff_seg.movi_pos;
                    self.state.movi_orig = riff_seg.movi_size;
                    self.state.movi_size = riff_seg.movi_size;
                    break;
                }
            }
            if !found {
                let riff_seg = self.state.odml_riff.last().unwrap();
                self.src.seek(SeekFrom::Start(riff_seg.get_end()))?;
                loop {
                    let ret = self.try_next_odml_chunk();
                    if ret.is_err() {
                        return Err(DemuxerError::SeekError);
                    }
                    let riff_seg = self.state.odml_riff.last().unwrap();
                    if riff_seg.contains(seek_info.pos) {
                        self.state.movi_pos = riff_seg.movi_pos;
                        self.state.movi_orig = riff_seg.movi_size;
                        self.state.movi_size = riff_seg.movi_size;
                        break;
                    }
                    self.src.seek(SeekFrom::Start(riff_seg.get_end()))?;
                }
            }
        }
        if seek_info.pos < self.state.movi_pos { return Err(DemuxerError::SeekError); }
        let skip_size = (seek_info.pos - self.state.movi_pos) as usize;
        if skip_size > self.state.movi_orig { return Err(DemuxerError::SeekError); }
        self.state.movi_size = self.state.movi_orig - skip_size;

        self.state.streams[seek_info.str_id as usize].cur_frame = seek_info.pts;

        for ss in self.state.streams.iter_mut() {
            ss.ts = None;
        }
        match self.state.demux_mode {
            DemuxingMode::Linear => {
                self.src.seek(SeekFrom::Start(seek_info.pos))?;
            },
            DemuxingMode::NonInterleaved => {
                let mut real_time = 0;
                for avi_stream in self.state.streams.iter() {
                    if u32::from(avi_stream.strm_no) == seek_info.str_id {
                        real_time = avi_stream.index[seek_info.pts as usize].time;
                        break;
                    }
                }
                for avi_stream in self.state.streams.iter_mut() {
                    avi_stream.cur_frame = 0;
                    for frm in avi_stream.index.iter() {
                        if frm.time < real_time {
                            avi_stream.cur_frame += 1;
                        } else {
                            break;
                        }
                    }
                }
            },
            DemuxingMode::SaturnAVI => {
                for (n, &offset) in self.state.chunk_offs.iter().enumerate() {
                    if offset == seek_info.pos {
                        self.state.cur_chunk = n;
                        break;
                    }
                }
            },
        }

        Ok(())
    }
    fn get_duration(&self) -> u64 { 0 }
}

const PRINT_CHUNKS: &str = "print_chunks";

const DEMUXER_OPTIONS: &[NAOptionDefinition] = &[
    NAOptionDefinition {
        name:           PRINT_CHUNKS,
        description:    "Print parsed file structure",
        opt_type:       NAOptionDefinitionType::Bool },
];

#[allow(clippy::single_match)]
impl<'a> NAOptionHandler for AVIDemuxer<'a> {
    fn get_supported_options(&self) -> &[NAOptionDefinition] { DEMUXER_OPTIONS }
    fn set_options(&mut self, options: &[NAOption]) {
        for option in options.iter() {
            for opt_def in DEMUXER_OPTIONS.iter() {
                if opt_def.check(option).is_ok() {
                    match (option.name, &option.value) {
                        (PRINT_CHUNKS, NAValue::Bool(val)) => {
                            self.print_chunks = *val;
                        },
                        _ => {},
                    }
                }
            }
        }
    }
    fn query_option_value(&self, name: &str) -> Option<NAValue> {
        match name {
            PRINT_CHUNKS    => Some(NAValue::Bool(self.print_chunks)),
            _ => None,
        }
    }
}

impl<'a> AVIDemuxer<'a> {
    fn new(io: &'a mut dyn ByteIO) -> Self {
        AVIDemuxer {
            src: io,
            state: AVIState::new(),
            print_chunks: false,
        }
    }

    fn try_next_odml_chunk(&mut self) -> DemuxerResult<()> {
        let last_seg = self.state.odml_riff.last().unwrap();
        if self.src.tell() < last_seg.get_end() {
            for riff_seg in self.state.odml_riff.iter() {
                if riff_seg.pos >= self.src.tell() {
                    self.src.seek(SeekFrom::Start(riff_seg.movi_pos))?;
                    self.state.movi_pos = riff_seg.movi_pos;
                    self.state.movi_size = riff_seg.movi_size;
                    return Ok(());
                }
            }
        }
        self.src.seek(SeekFrom::Start(last_seg.get_end()))?;

        let riff_pos = self.src.tell();
        let ret = self.src.read_tag();
        if let Ok([b'R', b'I', b'F', b'F']) = ret {
        } else {
            return Err(DemuxerError::EOF);
        }
        let riff_size = self.src.read_u32le()? as usize;
        let tag = self.src.read_tag()?;
        if self.print_chunks {
            println!("RIFF extended {}{}{}{} size {riff_size:X} @ {:X}", tag[0] as char, tag[1] as char, tag[2] as char, tag[3] as char, self.src.tell() - 12);
        }
        validate!(&tag == b"AVIX");
        let tag = self.src.read_tag()?;
        validate!(&tag == b"LIST");
        let list_size = self.src.read_u32le()? as usize;
        validate!(list_size >= 4);
        let tag = self.src.read_tag()?;
        validate!(&tag == b"movi");
        self.state.odml_riff.push(RIFFSegment{ pos: riff_pos, size: riff_size + 8, movi_pos: riff_pos + 24, movi_size: list_size - 4});
        self.state.movi_pos = riff_pos + 24;
        self.state.movi_size = list_size - 4;
        self.state.movi_orig = self.state.movi_size;

        Ok(())
    }
}

const AVI_HEADER_TAGS: &[(&[u8; 4], &[u8; 4])] = &[
    ( b"RIFF", b"AVI " ),
    ( b"RIFF", b"AVIX" ),
    ( b"ON2 ", b"ON2f" ),
];

fn find_raw_rgb_fmt(hdr: &MSBitmapInfo, vhdr: &mut NAVideoInfo) -> bool {
    match &hdr.compression {
        &[0, 0, 0, 0] | b"DIB " => {
            if hdr.planes != 1 {
                return false;
            }
            let fmt_name = match hdr.bitcount {
                     8 => "pal8",
                    16 => "rgb555",
                    24 => "bgr24",
                    32 => "bgra24",
                    _ => return false,
                };
            if let Ok(fmt) = NAPixelFormaton::from_str(fmt_name) {
                vhdr.format = fmt;
                vhdr.flipped = !hdr.flipped;
                true
            } else {
                false
            }
        },
        _ => false,
    }
}

fn find_raw_yuv_fmt(compr: &[u8; 4], vhdr: &mut NAVideoInfo) -> bool {
    let (fmt_name, swapuv) = match compr {
            b"UYVY" | b"UYNY" | b"UYNV" | b"2vuy" => ("uyvy", false),
            b"I420" | b"IYUV" => ("yuv420p", false),
            b"YV12" => ("yuv420p", true),
            b"YVU9" => ("yuv410p", true),
            b"Y800" | b"Y8  " => ("grey", false),
            _ => return false,
        };
    if let Ok(fmt) = NAPixelFormaton::from_str(fmt_name) {
        vhdr.format = fmt;
        if swapuv {
            vhdr.format.comp_info.swap(1, 2);
        }
        true
    } else {
        false
    }
}

fn parse_idx1(src: &mut dyn ByteIO, strmgr: &mut StreamManager, seek_idx: &mut SeekIndex, size: usize, movi_pos: u64, key_offs: &mut Vec<u64>, avi_streams: &mut [AVIStream]) -> DemuxerResult<(usize, bool)> {
    validate!((size & 15) == 0);
    let mut tag = [0u8; 4];
    let num_entries = size >> 4;
    let mut counter = [0u64; 100];
    let mut tb_nums = [0u32; 100];
    let mut tb_dens = [0u32; 100];
    let mut add_offset = 0;
    let mut set_offset = false;
    let mut max_sizes = [0u32; 100];
    for _ in 0..num_entries {
                              src.read_buf(&mut tag)?;
        let flags           = src.read_u32le()?;
        let mut offset      = src.read_u32le()? as u64;
        let length          = src.read_u32le()?;

        if !set_offset && offset < movi_pos {
            add_offset = movi_pos - offset;
        }
        set_offset = true;

        offset += add_offset;

        if tag[0] < b'0' || tag[0] > b'9' || tag[1] < b'0' || tag[1] > b'9' {
            continue;
        }
        let stream_no = ((tag[0] - b'0') * 10 + (tag[1] - b'0')) as usize;

        max_sizes[stream_no] = max_sizes[stream_no].max(length);

        if let Some(stream) = strmgr.get_stream(stream_no) {
            let (stb_num, stb_den) = stream.get_timebase();
            let pts = counter[stream_no];
            if tb_nums[stream_no] == 0 {
                if stream.get_media_type() != StreamType::Audio {
                    tb_nums[stream_no] = stb_num;
                    tb_dens[stream_no] = stb_den;
                } else {
                    let info = stream.get_info();
                    let ainfo = info.get_properties().get_audio_info().unwrap();
                    if info.get_name() == "pcm" {
                        validate!(length > 0);
                        tb_nums[stream_no] = length / (ainfo.block_len as u32);
                        tb_dens[stream_no] = stb_den;
                    } else {
                        let mut abr = 0;
                        for avi_str in avi_streams.iter_mut() {
                            if avi_str.strm_no == (stream_no as u8) {
                                abr = avi_str.aud_brate;
                                break;
                            }
                        }
                        validate!(length > 0 && abr > 0);
                        tb_nums[stream_no] = length;
                        tb_dens[stream_no] = abr;
                    }
                }
            }
            let time = NATimeInfo::rescale_ts(pts, tb_nums[stream_no], tb_dens[stream_no], 1, 1000);

            for avi_str in avi_streams.iter_mut() {
                if avi_str.strm_no == (stream_no as u8) {
                    avi_str.index.push(ChunkIndex { time, offset });
                    break;
                }
            }

            if (flags & 0x10) != 0 {
                if stream.get_media_type() == StreamType::Video {
                    validate!(offset >= movi_pos);
                    seek_idx.add_entry(stream_no as u32, SeekEntry { time, pts, pos: offset });
                }
                key_offs.push(offset);
            }
        }
        if (flags & 0x100) == 0 && &tag[2..] != b"pc" {
            counter[stream_no] += 1;
        }
    }
    key_offs.sort_unstable();

    // check for non-interleaved AVIs
    let mut non_interleaved = false;
    let mut range0 = (0, 0);
    for avi_str in avi_streams.iter() {
        if avi_str.index.len() >= 10 {
            let cur_range = (avi_str.index[0].offset, avi_str.index[avi_str.index.len() - 1].offset);
            if cur_range.0 > cur_range.1 {
                non_interleaved = true;
                break;
            }
            if range0 == (0, 0) {
                range0 = cur_range;
            } else if cur_range.0 > range0.1 || cur_range.1 < range0.0 {
                non_interleaved = true;
                break;
            }
        } else if avi_str.index.len() == 1 {
            // check for a case when whole audio track is stored as one large frame (at the end of file)
            let max_frames = avi_streams.iter().fold(0usize, |acc, avistr| acc.max(avistr.index.len()));
            let strm = strmgr.get_stream(avi_str.strm_no.into()).unwrap();
            if strm.get_media_type() == StreamType::Audio && strm.get_info().get_name() == "pcm"
                    && max_frames > 100 && max_sizes[usize::from(avi_str.strm_no)] > 64000 {
                non_interleaved = true;
                break;
            }
        }
    }

    Ok((size, non_interleaved))
}

fn parse_iddx_data(src: &mut dyn ByteIO, strmgr: &mut StreamManager, seek_idx: &mut SeekIndex, size: usize, movi_pos: u64, key_offs: &mut Vec<u64>, chunk_offs: &mut Vec<u64>) -> DemuxerResult<usize> {
    validate!((size & 15) == 0);
    let mut tag = [0u8; 4];
    let num_entries = size >> 4;
    let mut counter = [0u64; 100];
    let mut add_offset = 0;
    let mut set_offset = false;
    for _ in 0..num_entries {
                              src.read_buf(&mut tag)?;
        let flags           = src.read_u32le()?;
        let mut offset      = src.read_u32le()? as u64;
        let _length         = src.read_u32le()?;

        if !set_offset && offset < movi_pos {
            add_offset = movi_pos - offset;
        }
        set_offset = true;

        offset += add_offset;

        if tag[0] < b'0' || tag[0] > b'9' || tag[1] < b'0' || tag[1] > b'9' {
            continue;
        }
        let stream_no = ((tag[0] - b'0') * 10 + (tag[1] - b'0')) as usize;
        chunk_offs.push(offset);

        if (flags & 0x10) != 0 {
            if let Some(stream) = strmgr.get_stream(stream_no) {
                if stream.get_media_type() == StreamType::Video {
                    let (tb_num, tb_den) = stream.get_timebase();
                    let pts = counter[stream_no];
                    let time = NATimeInfo::rescale_ts(pts, tb_num, tb_den, 1, 1000);
                    validate!(offset >= movi_pos);
                    seek_idx.add_entry(stream_no as u32, SeekEntry { time, pts, pos: offset });
                }
                if !seek_idx.skip_index {
                    key_offs.push(offset);
                }
            }
        }
        counter[stream_no] += 1;
    }
    key_offs.sort_unstable();
    Ok(size)
}

fn parse_odml_ix(src: &mut dyn ByteIO, strmgr: &mut StreamManager, seek_idx: &mut SeekIndex, stream_no: usize, size: usize, start: u64) -> DemuxerResult<u64> {
    validate!(size >= 24);
    let entry_size = src.read_u16le()? as usize;
    if entry_size != 2 {
        return Err(DemuxerError::NotImplemented);
    }
    let sub_type = src.read_byte()?;
    let idx_type = src.read_byte()?;
    validate!(sub_type == 0 && idx_type == 1);
    let entries = src.read_u32le()? as usize;
    validate!(size >= 24 + entries * 4 * entry_size);
    src.read_tag()?; //chunk id
    let base_offset = src.read_u64le()?;
    src.read_u32le()?; //reserved
    if let Some(stream) = strmgr.get_stream(stream_no) {
        if stream.get_media_type() == StreamType::Video {
            let (tb_num, tb_den) = stream.get_timebase();

            for i in 0..entries {
                let offset = src.read_u32le()?;
                validate!(offset >= 8);
                let _size  = src.read_u32le()?;

                let pts = start + (i as u64);
                let time = NATimeInfo::rescale_ts(pts, tb_num, tb_den, 1, 1000);
                seek_idx.add_entry(stream_no as u32, SeekEntry { time, pts, pos: base_offset + u64::from(offset - 8) });
            }

            Ok(start + (entries as u64))
        } else {
            Ok(0)
        }
    } else {
        Ok(0)
    }
}

pub struct AVIDemuxerCreator { }

impl DemuxerCreator for AVIDemuxerCreator {
    fn new_demuxer<'a>(&self, br: &'a mut dyn ByteIO) -> Box<dyn DemuxCore<'a> + 'a> {
        Box::new(AVIDemuxer::new(br))
    }
    fn get_name(&self) -> &'static str { "avi" }
}

#[cfg(test)]
mod test {
    use super::*;
    use nihav_codec_support::test::dmx_test::*;

    #[test]
    fn test_avi_demux() {
        //test sample: https://samples.mplayerhq.hu/V-codecs/RT21/320x240/laser05.avi
        test_file_demux("assets/Indeo/laser05.avi", &AVIDemuxerCreator{}, 0, DemuxerTestMode::Demux);
    }
}
