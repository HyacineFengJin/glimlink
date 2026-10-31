use nihav_core::demuxers::*;
use nihav_registry::register::*;
use nihav_core::compr::deflate::*;

macro_rules! mktag {
    ($a:expr, $b:expr, $c:expr, $d:expr) => ({
        (($a as u32) << 24) | (($b as u32) << 16) | (($c as u32) << 8) | ($d as u32)
    });
    ($arr:expr) => ({
        (($arr[0] as u32) << 24) | (($arr[1] as u32) << 16) | (($arr[2] as u32) << 8) | ($arr[3] as u32)
    });
}

#[derive(Clone,Copy,Debug,PartialEq)]
enum DemuxMode {
    Normal,
    MacBin,
    ResFork,
}

trait Skip64 {
    fn skip64(&mut self, size: u64) -> ByteIOResult<()>;
}

impl<T: ?Sized + ByteIO> Skip64 for T {
    fn skip64(&mut self, size: u64) -> ByteIOResult<()> {
        if (size as usize as u64) != size {
            self.seek(SeekFrom::Current(size as i64))?;
        } else {
            self.read_skip(size as usize)?;
        }
        Ok(())
    }
}

fn read_chunk_header(br: &mut dyn ByteIO) -> DemuxerResult<(u32, u64)> {
    let size            = br.read_u32be()?;
    let ctype           = br.read_u32be()?;
    if size == 0 {
        let left = br.left();
        Ok((ctype, if left != -1 { left as u64 } else { u64::MAX }))
    } else if size == 1 {
        let size64      = br.read_u64be()?;
        validate!(size64 >= 16);
        Ok((ctype, size64 - 16))
    } else {
        validate!(size >= 8);
        Ok((ctype, (size as u64) - 8))
    }
}

fn read_palette(br: &mut dyn ByteIO, size: u64, pal: &mut [u8; 1024]) -> DemuxerResult<u64> {
    let _seed           = br.read_u32be()?;
    let flags           = br.read_u16be()?;
    let palsize         = (br.read_u16be()? as usize) + 1;
    validate!(palsize <= 256);
    validate!(flags == 0 || flags == 0x4000 || flags == 0x8000);
    let mut read_size = (palsize as u64) * 8 + 8;
    validate!(read_size <= size);
    for i in 0..palsize {
        let a           = br.read_u16be()?;
        let r           = br.read_u16be()?;
        let g           = br.read_u16be()?;
        let b           = br.read_u16be()?;
        pal[i * 4]     = (r >> 8) as u8;
        pal[i * 4 + 1] = (g >> 8) as u8;
        pal[i * 4 + 2] = (b >> 8) as u8;
        if flags == 0x8000 {
            pal[i * 4 + 3] = (a >> 8) as u8;
        }
    }
    if flags == 0x4000 {
        br.read_skip(8)?;
        read_size += 8;
    }
    Ok(read_size)
}

struct RootChunkHandler {
    ctype:  u32,
    parse:  fn(dmx: &mut MOVDemuxer, strmgr: &mut StreamManager, size: u64) -> DemuxerResult<u64>,
}

const IGNORED_CHUNKS: &[u32] = &[
    mktag!(b"free"), mktag!(b"skip"), mktag!(b"udta"), mktag!(b"uuid"), mktag!(b"wide")
];

const ROOT_CHUNK_HANDLERS: &[RootChunkHandler] = &[
    RootChunkHandler { ctype: mktag!(b"ftyp"), parse: read_ftyp },
    RootChunkHandler { ctype: mktag!(b"mdat"), parse: read_mdat },
    RootChunkHandler { ctype: mktag!(b"moov"), parse: read_moov },
    RootChunkHandler { ctype: mktag!(b"moof"), parse: read_moof },
    RootChunkHandler { ctype: mktag!(b"sidx"), parse: read_sidx },
    RootChunkHandler { ctype: mktag!(b"mfra"), parse: skip_chunk_mov },
];

fn print_cname(ctype: u32, size: u64, off: u64, depth: u8) {
    for _ in 0..depth { print!("    "); }
    let tag = [(ctype >> 24) as u8, (ctype >> 16) as u8, (ctype >> 8) as u8, ctype as u8];
    let mut printable = true;
    for ch in tag.iter() {
        if !(0x20..=0x7F).contains(ch) {
            printable = false;
            break;
        }
    }
    if printable {
        print!(" '{}{}{}{}'", tag[0] as char, tag[1] as char, tag[2] as char, tag[3] as char);
    } else {
        print!(" {:08X}", ctype);
    }
    println!(" size {} @ {:X}", size, off);
}

macro_rules! read_chunk_list {
    (root; $name: expr, $fname: ident, $handlers: ident) => {
        fn $fname(&mut self, strmgr: &mut StreamManager, size: u64) -> DemuxerResult<()> {
            self.depth += 1;
            validate!(self.depth < 32);
            let list_end = self.src.tell() + size;
            while self.src.tell() < list_end {
                let ret = read_chunk_header(self.src);
                if ret.is_err() { break; }
                let (ctype, size) = ret.unwrap();
                let size = if size == u64::MAX { list_end - self.src.tell() } else { size };
                if self.print_chunks {
                    print_cname(ctype, size, self.src.tell(), self.depth as u8);
                }
                if self.src.tell() + size > list_end {
                    break;
                }

                let handler = $handlers.iter().find(|x| x.ctype == ctype);
                let read_size;
                if let Some(ref handler) = handler {
                    read_size = (handler.parse)(self, strmgr, size)?;
                } else if IGNORED_CHUNKS.contains(&ctype) {
                    self.src.skip64(size)?;
                    continue;
                } else {
                    println!("skipping unknown chunk {:08X} size {}", ctype, size);
                    read_size = 0;
                }
                validate!(read_size <= size);
                self.src.skip64(size - read_size)?;
            }
            self.depth -= 1;
            validate!(self.src.tell() == list_end);
            Ok(())
        }
    };
    (track; $name: expr, $fname: ident, $handlers: ident) => {
        fn $fname(&mut self, br: &mut dyn ByteIO, size: u64) -> DemuxerResult<()> {
            self.depth += 1;
            validate!(self.depth < 32);
            let list_end = br.tell() + size;
            while br.tell() < list_end {
                let ret = read_chunk_header(br);
                if ret.is_err() { break; }
                let (ctype, size) = ret.unwrap();
                let size = if size == u64::MAX { list_end - br.tell() } else { size };
                if self.print_chunks {
                    print_cname(ctype, size, br.tell(), self.depth + 1);
                }
                if br.tell() + size > list_end {
                    break;
                }
                let handler = $handlers.iter().find(|x| x.ctype == ctype);
                let read_size;
                if let Some(ref handler) = handler {
                    read_size = (handler.parse)(self, br, size)?;
                } else if IGNORED_CHUNKS.contains(&ctype) {
                    br.skip64(size)?;
                    continue;
                } else {
                    read_size = 0;
                }
                validate!(read_size <= size);
                br.skip64(size - read_size)?;
            }
            self.depth -= 1;
            validate!(br.tell() == list_end);
            Ok(())
        }
    }
}

mod track;
use track::*;
mod pktread;

fn read_ftyp(dmx: &mut MOVDemuxer, _strmgr: &mut StreamManager, size: u64) -> DemuxerResult<u64> {
    dmx.src.skip64(size)?;
    Ok(size)
}

fn read_mdat(dmx: &mut MOVDemuxer, _strmgr: &mut StreamManager, size: u64) -> DemuxerResult<u64> {
    dmx.mdat_pos  = dmx.src.tell();
    dmx.mdat_size = size;
    dmx.src.skip64(size)?;
    Ok(size)
}

fn read_sidx(_dmx: &mut MOVDemuxer, _strmgr: &mut StreamManager, _size: u64) -> DemuxerResult<u64> {
    Ok(0)
}

fn parse_root_udta_payload(dmx: &mut MOVDemuxer, strmgr: &mut StreamManager, end: u64) -> DemuxerResult<()> {
    while dmx.src.tell() + 8 < end {
        let size            = dmx.src.read_u32be()?;
        let tag             = dmx.src.read_u32be()?;
        validate!(size >= 8 && dmx.src.tell() + u64::from(size - 8) <= end);
        if tag == mktag!(b"NAVG") {
            let dsize = size as usize - 8;
            let edata = if dsize > 0 {
                    let mut data = vec![0; dsize];
                              dmx.src.read_buf(&mut data)?;
                    Some(data)
                } else { None };
            let cinfo = NACodecInfo::new("qt-navigable", NACodecTypeInfo::None, edata);
            strmgr.add_stream(NAStream::new(StreamType::Data, 65535, cinfo, 1, 600, 0));
            break;
        } else {
                              dmx.src.read_skip((size - 8) as usize)?;
        }
    }
    Ok(())
}

fn read_root_udta(dmx: &mut MOVDemuxer, strmgr: &mut StreamManager, size: u64) -> DemuxerResult<u64> {
    let end = dmx.src.tell() + size;
    let _ = parse_root_udta_payload(dmx, strmgr, end); // its parsing should not hinder anything
    dmx.src.seek(SeekFrom::Start(end))?;
    Ok(size)
}

fn read_moov(dmx: &mut MOVDemuxer, strmgr: &mut StreamManager, size: u64) -> DemuxerResult<u64> {
    dmx.read_moov(strmgr, size)?;
    Ok(size)
}

const MOOV_CHUNK_HANDLERS: &[RootChunkHandler] = &[
    RootChunkHandler { ctype: mktag!(b"mvhd"), parse: read_mvhd },
    RootChunkHandler { ctype: mktag!(b"cmov"), parse: read_cmov },
    RootChunkHandler { ctype: mktag!(b"ctab"), parse: read_ctab },
    RootChunkHandler { ctype: mktag!(b"trak"), parse: read_trak },
    RootChunkHandler { ctype: mktag!(b"meta"), parse: read_meta },
    RootChunkHandler { ctype: mktag!(b"mvex"), parse: read_mvex },
    RootChunkHandler { ctype: mktag!(b"iods"), parse: skip_chunk_mov },
    RootChunkHandler { ctype: mktag!(b"udta"), parse: read_root_udta },
];

fn read_mvhd(dmx: &mut MOVDemuxer, _strmgr: &mut StreamManager, size: u64) -> DemuxerResult<u64> {
    const KNOWN_MVHD_SIZE: u64 = 100;
    let br = &mut dmx.src;
    validate!(size >= KNOWN_MVHD_SIZE);
    let version             = br.read_byte()?;
    validate!(version == 0 || version == 0xFF);
    dmx.ver_m1 = version == 0xFF;
    let _flags              = br.read_u24be()?;
    let _ctime              = br.read_u32be()?;
    let _mtime              = br.read_u32be()?;
    let tscale              = br.read_u32be()?;
    let duration            = br.read_u32be()?;
    let _pref_rate          = br.read_u32be()?;
    let _pref_volume        = br.read_u16be()?;
                              br.read_skip(10)?;
                              br.read_skip(36)?; // matrix
    let _preview_time       = br.read_u32be()?;
    let _preview_duration   = br.read_u32be()?;
    let _poster_time        = br.read_u32be()?;
    let _sel_time           = br.read_u32be()?;
    let _sel_duration       = br.read_u32be()?;
    let _cur_time           = br.read_u32be()?;
    let _next_track_id      = br.read_u32be()?;
    dmx.duration = duration;
    dmx.tb_den = tscale;

    Ok(KNOWN_MVHD_SIZE)
}

fn read_cmov(dmx: &mut MOVDemuxer, strmgr: &mut StreamManager, size: u64) -> DemuxerResult<u64> {
    let br = &mut dmx.src;
    validate!(size > 24);
    let dcom_size           = br.read_u32be()?;
    let dcom_tag            = br.read_tag()?;
    let compr_type          = br.read_tag()?;
    validate!(&dcom_tag == b"dcom" && dcom_size == 12);
    if &compr_type != b"zlib" {
        return Err(DemuxerError::NotImplemented);
    }
    let cmvd_size           = u64::from(br.read_u32be()?);
    let cmvd_tag            = br.read_tag()?;
    validate!(&cmvd_tag == b"cmvd" && cmvd_size > 14 && cmvd_size == size - 12);
    let comp_size = (cmvd_size - 12) as usize;
    let uncomp_size         = br.read_u32be()? as usize;
    validate!(uncomp_size > 8);
    let mut sbuf = vec![0; comp_size];
    let mut dbuf = vec![0; uncomp_size];
                              br.read_buf(sbuf.as_mut_slice())?;
    let ret = Inflate::uncompress(sbuf.as_slice(), dbuf.as_mut_slice());
    if ret.is_err() {
        return Err(DemuxerError::InvalidData);
    }
    let len = ret.unwrap();
    validate!(len == uncomp_size);
    let mut br = MemoryReader::new_read(dbuf.as_slice());
    let (ctype, csize) = read_chunk_header(&mut br)?;
    validate!(ctype == mktag!(b"moov"));
    let mut ddmx = MOVDemuxer::new(&mut br);
    ddmx.print_chunks = dmx.print_chunks;
    ddmx.read_moov(strmgr, csize)?;
    std::mem::swap(&mut dmx.tracks, &mut ddmx.tracks);
    dmx.duration = ddmx.duration;
    dmx.tb_den = ddmx.tb_den;
    std::mem::swap(&mut dmx.pal, &mut ddmx.pal);

    Ok(size)
}

fn read_ctab(dmx: &mut MOVDemuxer, _strmgr: &mut StreamManager, size: u64) -> DemuxerResult<u64> {
    let mut pal = [0; 1024];
    let size = read_palette(dmx.src, size, &mut pal)?;
    dmx.pal = Some(Arc::new(pal));
    Ok(size)
}

fn read_meta(dmx: &mut MOVDemuxer, _strmgr: &mut StreamManager, size: u64) -> DemuxerResult<u64> {
    dmx.src.skip64(size)?;
    Ok(size)
}

fn read_mvex(_dmx: &mut MOVDemuxer, _strmgr: &mut StreamManager, _size: u64) -> DemuxerResult<u64> {
    Ok(0)
}

fn skip_chunk_mov(_dmx: &mut MOVDemuxer, _strmgr: &mut StreamManager, _size: u64) -> DemuxerResult<u64> {
    Ok(0)
}

fn read_trak(dmx: &mut MOVDemuxer, strmgr: &mut StreamManager, size: u64) -> DemuxerResult<u64> {
    let mut track = Track::new(dmx.cur_track as u32, dmx.tb_den);
    track.print_chunks = dmx.print_chunks;
    track.ver_m1 = dmx.ver_m1;
    track.call_read_trak(dmx.src, size)?;
    validate!(track.tkhd_found);
    if !track.stsd_found {
        track.streams.push(NAStream::new(StreamType::Data, track.track_no, DUMMY_CODEC_INFO, track.tb_num, track.tb_den, 0));
    }
    // invent keyframes for video stream if none were reported
    if !track.stss_found && track.stream_type == StreamType::Video {
        let mut intraonly = false;
        if let Some(stream) = track.streams.first() {
            if let Some(desc) = get_codec_description(stream.get_info().get_name()) {
                intraonly = (desc.caps & CODEC_CAP_INTRAONLY) != 0;
            }
        }

        track.pkt_demux.invent_keyframes(intraonly);
    }
    validate!(strmgr.get_stream_by_id(track.track_id).is_none());
    dmx.cur_track += 1;
    dmx.tracks.push(track);
    Ok(size)
}

fn read_moof(dmx: &mut MOVDemuxer, strmgr: &mut StreamManager, size: u64) -> DemuxerResult<u64> {
    dmx.moof_off = dmx.src.tell() - 8;
    dmx.read_moof(strmgr, size)?;
    Ok(size)
}

const MOOF_CHUNK_HANDLERS: &[RootChunkHandler] = &[
    RootChunkHandler { ctype: mktag!(b"mfhd"), parse: read_mfhd },
    RootChunkHandler { ctype: mktag!(b"traf"), parse: read_traf },
    RootChunkHandler { ctype: mktag!(b"meta"), parse: read_meta },
];

fn read_mfhd(dmx: &mut MOVDemuxer, _strmgr: &mut StreamManager, size: u64) -> DemuxerResult<u64> {
    const KNOWN_MFHD_SIZE: u64 = 8;
    validate!(size >= KNOWN_MFHD_SIZE);
    let version             = dmx.src.read_byte()?;
    validate!(version == 0);
    let flags               = dmx.src.read_u24be()?;
    validate!(flags == 0);
    let _seq_no             = dmx.src.read_u32be()?;

    Ok(KNOWN_MFHD_SIZE)
}

fn read_traf(dmx: &mut MOVDemuxer, _strmgr: &mut StreamManager, size: u64) -> DemuxerResult<u64> {
    let mut buf = [0u8; 16];
                              dmx.src.peek_buf(&mut buf)?;
    validate!(&buf[4..8] == b"tfhd");
    let track_id = read_u32be(&buf[12..16])?;
    let mut track = None;
    for trk in dmx.tracks.iter_mut() {
        if trk.track_id == track_id {
            track = Some(trk);
            break;
        }
    }
    if let Some(track) = track {
        track.moof_off = dmx.moof_off;
        track.call_read_traf(dmx.src, size)?;
        Ok(size)
    } else {
        Ok(0)
    }
}

struct MOVDemuxer<'a> {
    src:            &'a mut dyn ByteIO,
    depth:          usize,
    mdat_pos:       u64,
    mdat_size:      u64,
    tracks:         Vec<Track>,
    cur_track:      usize,
    tb_den:         u32,
    duration:       u32,
    pal:            Option<Arc<[u8; 1024]>>,
    ver_m1:         bool,
    file_size:      u64,

    moof_off:       u64,

    print_chunks:   bool,

    demux_mode:     DemuxMode,

    force_seek:     bool,
}

impl<'a> DemuxCore<'a> for MOVDemuxer<'a> {
    fn open(&mut self, strmgr: &mut StreamManager, seek_index: &mut SeekIndex) -> DemuxerResult<()> {
        let mut data_start = 0;
        match self.demux_mode {
            DemuxMode::Normal => self.read_root(strmgr)?,
            DemuxMode::MacBin => {
                let ver                     = self.src.read_byte()?;
                validate!(ver == 0);
                                              self.src.read_skip(64)?;
                let tag                     = self.src.read_tag()?;
                validate!(&tag == b"MooV" || &tag == b"APPL");
                                              self.src.read_skip(14)?;
                let data_length             = self.src.read_u32be()?;
                validate!(data_length > 8);
                let rsrc_length             = self.src.read_u32be()?;
                validate!(rsrc_length > 0);
                                              self.src.read_skip(31)?;
                let ver                     = self.src.read_byte()?;
                validate!(ver == 0x81);
                let ver                     = self.src.read_byte()?;
                validate!(ver == 0x81);
                //xxx: maybe check header CRC

                let rsrc_start = 0x80 + ((data_length + 0x7F) & !0x7F);
                                              self.src.seek(SeekFrom::Start(rsrc_start.into()))?;
                let rsrc_off                = self.src.read_u32be()?;
                let rsrc_map_off            = self.src.read_u32be()?;
                let rsrc_size               = self.src.read_u32be()?;
                let _rsrc_map_size          = self.src.read_u32be()?;
                validate!(rsrc_off >= 0x10);
                validate!(rsrc_map_off >= rsrc_off + rsrc_size);
                                              self.src.seek(SeekFrom::Current(i64::from(rsrc_off - 16)))?;
                // I'm too lazy to parse resource map, so let's just iterate over resources for movie header
                let end_pos = u64::from(rsrc_start + rsrc_off + rsrc_size);
                let mut peek_buf = [0u8; 8];
                while self.src.tell() < end_pos {
                    let cur_size            = self.src.read_u32be()?;
                    validate!(self.src.tell() + u64::from(cur_size) <= end_pos);
                    if cur_size > 8 {
                        let rsize           = self.src.peek_u32be()?;
                        if rsize == cur_size {
                                              self.src.peek_buf(&mut peek_buf)?;
                            if &peek_buf[4..] == b"moov" {
                                              self.src.read_skip(8)?;
                                              self.src.peek_buf(&mut peek_buf)?;
                                if peek_buf == [0; 8] {
                                    println!("This looks like an alpha version of the format, try warhol demuxer instead");
                                    return Err(DemuxerError::InvalidData);
                                }
                                self.read_moov(strmgr, u64::from(rsize - 8))?;
                                self.mdat_pos = 8;
                                break;
                            }
                        }
                    }
                                              self.src.read_skip(cur_size as usize)?;
                }
                // if moov/mdat is not found, it may be in data part
                if self.tracks.is_empty() || self.mdat_pos == 0 {
                                              self.src.seek(SeekFrom::Start(0x80))?;
                    let end_pos = u64::from(data_length) + 0x80;
                    while self.src.tell() < end_pos && (self.mdat_pos == 0 || self.tracks.is_empty()) {
                        let size            = self.src.read_u32be()?;
                        let tag             = self.src.read_tag()?;
                        validate!(size >= 8);
                        match &tag {
                            b"moov" => {
                                              self.src.peek_buf(&mut peek_buf)?;
                                if peek_buf == [0; 8] {
                                    println!("This looks like an alpha version of the format, try warhol demuxer instead");
                                    return Err(DemuxerError::InvalidData);
                                }
                                self.read_moov(strmgr, u64::from(size - 8))?;
                            },
                            b"mdat" => {
                                self.mdat_pos = self.src.tell() - 0x80;
                                              self.src.read_skip(size as usize - 8)?;
                            },
                            _ => {
                                              self.src.read_skip(size as usize - 8)?;
                            },
                        }
                    }
                }
            },
            DemuxMode::ResFork => {
                let res_data_offset         = self.src.read_u32be()?;
                let res_map_offset          = self.src.read_u32be()?;
                let res_map_data_len        = self.src.read_u32be()?;
                let res_map_length          = self.src.read_u32be()?;
                validate!(res_data_offset == 0x100 && res_map_length > 16);
                                              self.src.seek(SeekFrom::Start(u64::from(res_map_offset)))?;
                let res_data_offset2        = self.src.read_u32be()?;
                let res_map_offset2         = self.src.read_u32be()?;
                let res_map_data_len2       = self.src.read_u32be()?;
                let res_map_length2         = self.src.read_u32be()?;
                validate!(res_data_offset == res_data_offset2
                    && res_map_offset == res_map_offset2
                    && res_map_data_len == res_map_data_len2
                    && res_map_length == res_map_length2);
                                              self.src.read_skip(res_map_length as usize - 16)?;
                data_start = self.src.tell();
                self.read_root(strmgr)?;
            },
        };
        validate!(self.mdat_pos > 0);
        validate!(!self.tracks.is_empty());
        for track in self.tracks.iter_mut() {
            for stream in track.streams.drain(..) {
                let str_id = strmgr.add_stream(stream).unwrap();
                track.track_str_ids.push(str_id);
            }
        }
        match self.demux_mode {
            DemuxMode::MacBin => {
                // patch data offsets
                for track in self.tracks.iter_mut() {
                    for offset in track.pkt_demux.chunk_offsets.iter_mut() {
                        *offset += 0x80;
                    }
                }
            },
            DemuxMode::ResFork => {
                // patch data offsets
                for track in self.tracks.iter_mut() {
                    for offset in track.pkt_demux.chunk_offsets.iter_mut() {
                        *offset += data_start;
                    }
                }
            },
            _ => {},
        }
        for track in self.tracks.iter_mut() {
            track.pkt_demux.fill_seek_index(track.track_no, seek_index);
            track.pkt_demux.decide_mode(track.stream_type);
        }
        self.src.seek(SeekFrom::Start(self.mdat_pos))?;
        self.cur_track = 0;
        Ok(())
    }

    fn get_frame(&mut self, strmgr: &mut StreamManager) -> DemuxerResult<NAPacket> {
        if self.tracks.is_empty() {
            return Err(DemuxerError::EOF);
        }
        let mut has_all_time = true;
        let mut min_ts = u64::MAX;
        for (trk_no, trk) in self.tracks.iter().enumerate() {
            if strmgr.is_ignored(trk_no) {
                continue;
            }
            if let Some(ts) = trk.pkt_demux.cur_ts {
                min_ts = min_ts.min(ts);
            } else {
                has_all_time = false;
                break;
            }
        }
        if has_all_time {
            for (trk_no, track) in self.tracks.iter_mut().enumerate() {
                if let Some(ts) = track.pkt_demux.cur_ts {
                    if ts == min_ts {
                        let first = track.pkt_demux.is_first();
                        if let Some((cts, offset, size, is_kf)) = track.pkt_demux.get_next_chunk() {
                            self.cur_track = trk_no + 1;
                            return process_packet(self.src, strmgr, track, cts, offset, size, first, is_kf);
                        }
                    }
                }
            }
        }

        for _ in 0..self.tracks.len() {
            if self.cur_track >= self.tracks.len() {
                self.cur_track = 0;
            }
            let track = &mut self.tracks[self.cur_track];
            self.cur_track += 1;
            if strmgr.is_ignored(self.cur_track - 1) {
                continue;
            }
            let first = track.pkt_demux.is_first();
            if let Some((cts, offset, size, is_kf)) = track.pkt_demux.get_next_chunk() {
                return process_packet(self.src, strmgr, track, cts, offset, size, first, is_kf);
            }
        }
        Err(DemuxerError::EOF)
    }

    fn seek(&mut self, time: NATimePoint, seek_index: &SeekIndex) -> DemuxerResult<()> {
        let ret = seek_index.find_pos(time);
        if ret.is_none() {
            if let NATimePoint::Milliseconds(_) = time {
                let mut aonly = true;
                for track in self.tracks.iter() {
                    if track.stream_type != StreamType::Audio {
                        aonly = false;
                        break;
                    }
                }
                if aonly {
                    for track in self.tracks.iter_mut() {
                        track.pkt_demux.seek(0, time, false)?;
                    }
                    return Ok(());
                }
            }
            if self.force_seek {
                let mut ret = Ok(());
                for track in self.tracks.iter_mut() {
                    if let Err(err) = track.pkt_demux.seek(0, time, true) {
                        ret = Err(err);
                        break;
                    }
                }
                if ret.is_err() {
                    for track in self.tracks.iter_mut() {
                        track.pkt_demux.reset();
                    }
                }
                return ret;
            }
            return Err(DemuxerError::SeekError);
        }
        let seek_info = ret.unwrap();
        let tbn = self.tracks[seek_info.str_id as usize].tb_num;
        let tbd = self.tracks[seek_info.str_id as usize].tb_den;
        let mut vpts = None;
        let mut apts = None;
        for track in self.tracks.iter_mut() {
            let cur_pts = if track.pkt_demux.seek_id as u32 == seek_info.str_id {
                    seek_info.pts
                } else if let Some(cur_f) = seek_index.seek_info[track.pkt_demux.seek_id].find_pos(time) {
                    cur_f.pts
                } else {
                    NATimeInfo::rescale_ts(seek_info.pts, tbn, tbd, track.tb_num, track.tb_den)
                };
            let actual_time = track.pkt_demux.seek(cur_pts, time, false)?;
            match track.stream_type {
                StreamType::Video => vpts = Some(actual_time),
                StreamType::Audio => apts = Some(actual_time),
                _ => {},
            };
        }
        /* For audio+video stream case when the post-seek actual times differ
           by more than half a second try to seek audio to a closer position
           to video.
        */
        if let (Some(vtime), Some(atime)) = (vpts, apts) {
            let acount = self.tracks.iter().filter(|trk| trk.stream_type == StreamType::Audio).count();
            let vcount = self.tracks.iter().filter(|trk| trk.stream_type == StreamType::Video).count();
            if (vcount == 1 && acount == 1) && (vtime.max(atime) - vtime.min(atime) > 500) && (atime != 0) {
                for track in self.tracks.iter_mut() {
                    if track.stream_type == StreamType::Audio {
                        let new_pts = NATimeInfo::rescale_ts(vtime, 1, 1000, track.tb_num, track.tb_den);
                        track.pkt_demux.seek(new_pts, NATimePoint::Milliseconds(vtime), false)?;
                    }
                }
            }
        }

        Ok(())
    }
    fn get_duration(&self) -> u64 {
        if self.tb_den != 0 {
            u64::from(self.duration) * 1000 / u64::from(self.tb_den)
        } else {
            0
        }
    }
}

const PRINT_CHUNKS: &str = "print_chunks";

const DEMUXER_OPTIONS: &[NAOptionDefinition] = &[
    NAOptionDefinition {
        name:           PRINT_CHUNKS,
        description:    "Print parsed file structure",
        opt_type:       NAOptionDefinitionType::Bool },
    NAOptionDefinition {
        name:           FORCE_SEEK_OPTION,
        description:    FORCE_SEEK_OPTION_DESC,
        opt_type:       NAOptionDefinitionType::Bool },
];

impl<'a> NAOptionHandler for MOVDemuxer<'a> {
    fn get_supported_options(&self) -> &[NAOptionDefinition] { DEMUXER_OPTIONS }
    #[allow(clippy::single_match)]
    fn set_options(&mut self, options: &[NAOption]) {
        for option in options.iter() {
            for opt_def in DEMUXER_OPTIONS.iter() {
                if opt_def.check(option).is_ok() {
                    match (option.name, &option.value) {
                        (PRINT_CHUNKS, NAValue::Bool(val)) => {
                            self.print_chunks = *val;
                        },
                        (FORCE_SEEK_OPTION, NAValue::Bool(val)) => {
                            self.force_seek = *val;
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
            FORCE_SEEK_OPTION => Some(NAValue::Bool(self.force_seek)),
            _ => None,
        }
    }
}

impl<'a> MOVDemuxer<'a> {
    fn new(io: &'a mut dyn ByteIO) -> Self {
        Self::new_common(io, DemuxMode::Normal)
    }
    fn new_macbinary(io: &'a mut dyn ByteIO) -> Self {
        Self::new_common(io, DemuxMode::MacBin)
    }
    fn new_resfork(io: &'a mut dyn ByteIO) -> Self {
        Self::new_common(io, DemuxMode::ResFork)
    }
    fn new_common(io: &'a mut dyn ByteIO, demux_mode: DemuxMode) -> Self {
        MOVDemuxer {
            src:            io,
            depth:          0,
            mdat_pos:       0,
            mdat_size:      0,
            tracks:         Vec::with_capacity(2),
            cur_track:      0,
            tb_den:         0,
            duration:       0,
            pal:            None,
            ver_m1:         false,
            file_size:      u64::MAX,

            moof_off:       0,

            print_chunks:   false,

            demux_mode,
            force_seek:     false,
        }
    }
    fn read_root(&mut self, strmgr: &mut StreamManager) -> DemuxerResult<()> {
        let mut old_flat_file = false;
        let mut moov_seen = false;
        let mut mdat_seen = false;

        self.depth = 0;
        let cur_pos = self.src.tell();
        self.src.seek(SeekFrom::End(0))?;
        self.file_size = self.src.tell();
        self.src.seek(SeekFrom::Start(cur_pos))?;
        while self.src.tell() < self.file_size {
            let ret = read_chunk_header(self.src);
            if ret.is_err() { break; }
            let (ctype, size) = ret.unwrap();
            if self.demux_mode == DemuxMode::Normal && ctype == mktag!(b"mdat") && size == u64::MAX && self.src.tell() == 8 {
                println!("mdat atom with no size at the very beginning of the file");
                println!("try nihav-movrender to extract streams from such file");
                return Err(DemuxerError::NotImplemented);
            }
            let size = if size == u64::MAX { self.file_size - self.src.tell() } else { size };
            if size == 256-8 && (ctype >> 24) == 0 {
                println!("Probably an old flattened file, skipping garbage in the middle...");
                self.src.read_skip(256 - 4)?;
                old_flat_file = true;
                continue;
            }
            if self.print_chunks {
                print_cname(ctype, size, self.src.tell(), 0);
            }
            if ctype == mktag!(b"moov") {
                moov_seen = true;
            }
            if ctype == mktag!(b"mdat") {
                mdat_seen = true;
            }
            if IGNORED_CHUNKS.contains(&ctype) {
                self.src.skip64(size)?;
                continue;
            }
            let handler = ROOT_CHUNK_HANDLERS.iter().find(|x| x.ctype == ctype);
            let read_size;
            if let Some(handler) = handler {
                read_size = (handler.parse)(self, strmgr, size)?;
            } else {
                println!("skipping unknown chunk {:08X} size {}", ctype, size);
                read_size = 0;
            }
            validate!(read_size <= size);
            self.src.skip64(size - read_size)?;
            if old_flat_file && moov_seen && mdat_seen {
                break;
            }
        }
//todo check if all needed chunks are found
        Ok(())
    }
    read_chunk_list!(root; "moov", read_moov, MOOV_CHUNK_HANDLERS);
    read_chunk_list!(root; "moof", read_moof, MOOF_CHUNK_HANDLERS);
}

pub struct MOVDemuxerCreator { }

impl DemuxerCreator for MOVDemuxerCreator {
    fn new_demuxer<'a>(&self, br: &'a mut dyn ByteIO) -> Box<dyn DemuxCore<'a> + 'a> {
        Box::new(MOVDemuxer::new(br))
    }
    fn get_name(&self) -> &'static str { "mov" }
}

pub struct MacBinaryMOVDemuxerCreator { }

impl DemuxerCreator for MacBinaryMOVDemuxerCreator {
    fn new_demuxer<'a>(&self, br: &'a mut dyn ByteIO) -> Box<dyn DemuxCore<'a> + 'a> {
        Box::new(MOVDemuxer::new_macbinary(br))
    }
    fn get_name(&self) -> &'static str { "mov-macbin" }
}

pub struct MacResForkMOVDemuxerCreator { }

impl DemuxerCreator for MacResForkMOVDemuxerCreator {
    fn new_demuxer<'a>(&self, br: &'a mut dyn ByteIO) -> Box<dyn DemuxCore<'a> + 'a> {
        Box::new(MOVDemuxer::new_resfork(br))
    }
    fn get_name(&self) -> &'static str { "mov-resfork" }
}

#[cfg(test)]
mod test {
    use super::*;
    use std::fs::File;
    use nihav_codec_support::test::dmx_test::*;

    fn test_core(name: &str, demux_mode: DemuxMode, nframes: usize, test_mode: DemuxerTestMode) {
        let mut file = File::open(name).unwrap();
        let mut br = FileReader::new_read(&mut file);
        let mut dmx = match demux_mode {
                DemuxMode::Normal => MOVDemuxer::new(&mut br),
                DemuxMode::MacBin => MOVDemuxer::new_macbinary(&mut br),
                DemuxMode::ResFork => MOVDemuxer::new_resfork(&mut br),
            };
        let mut sm = StreamManager::new();
        let mut si = SeekIndex::new();
        dmx.open(&mut sm, &mut si).unwrap();
        test_demuxer(&mut dmx, &mut sm, nframes, test_mode);
    }
    #[test]
    fn test_mov_demux() {
        // sample: https://samples.mplayerhq.hu/V-codecs/IV32/cubes.mov
        test_core("assets/Indeo/cubes.mov", DemuxMode::Normal, 10, DemuxerTestMode::Demux);
    }

    #[test]
    fn test_dash_demux() {
        // sample: a stream downloaded with youtube-dl
        test_core("assets/ITU/dash.m4a", DemuxMode::Normal, 10, DemuxerTestMode::Test(vec![
            DPacket::new(0, Some(0), Some(0), 371, true),
            DPacket::new(0, Some(1), Some(1), 372, false),
            DPacket::new(0, Some(2), Some(2), 371, false),
            DPacket::new(0, Some(3), Some(3), 372, false),
            DPacket::new(0, Some(4), Some(4), 371, false),
            DPacket::new(0, Some(5), Some(5), 372, false),
            DPacket::new(0, Some(6), Some(6), 371, false),
            DPacket::new(0, Some(7), Some(7), 372, false),
            DPacket::new(0, Some(8), Some(8), 371, false),
            DPacket::new(0, Some(9), Some(9), 372, false)]));
    }

    #[test]
    fn test_macbinary_demux() {
        // sample from King's Quest VI Macintosh edition
        test_core("assets/QT/Halfdome.bin", DemuxMode::MacBin, 10, DemuxerTestMode::Test(vec![
            DPacket::new(0, Some(0), Some(0), 11604, true),
            DPacket::new(0, Some(1), Some(1), 12344, false),
            DPacket::new(0, Some(2), Some(2), 8348, false),
            DPacket::new(0, Some(3), Some(3), 8344, false),
            DPacket::new(0, Some(4), Some(4), 8336, false),
            DPacket::new(0, Some(5), Some(5), 8348, false),
            DPacket::new(0, Some(6), Some(6), 8312, false),
            DPacket::new(0, Some(7), Some(7), 8344, false),
            DPacket::new(0, Some(8), Some(8), 12324, false),
            DPacket::new(0, Some(9), Some(9), 12276, false)]));
    }

    #[test]
    fn test_beta_qt() {
        // sample from Apple Reference & Presentation Library 8
        test_core("assets/QT/Smallwhale2", DemuxMode::MacBin, 10, DemuxerTestMode::Test(vec![
            DPacket::new(0, Some(0), Some(0), 9216, true),
            DPacket::new(1, Some(0), Some(0), 6144, true),
            DPacket::new(0, Some(17), Some(17), 9216, true),
            DPacket::new(1, Some(6144), Some(6144), 6144, true),
            DPacket::new(0, Some(33), Some(33), 9216, true),
            DPacket::new(0, Some(48), Some(48), 9216, true),
            DPacket::new(1, Some(12288), Some(12288), 6144, true),
            DPacket::new(0, Some(64), Some(64), 9216, true),
            DPacket::new(0, Some(81), Some(81), 9216, true),
            DPacket::new(1, Some(18432), Some(18432), 6144, true)]));
    }

    #[test]
    fn test_resfork_demux() {
        // sample from The Wonders of Electricity: An Adventure in Safety
        test_core("assets/QT/car.mov", DemuxMode::ResFork, 10, DemuxerTestMode::Test(vec![
            DPacket::new(0, Some(0), Some(0), 6144, true),
            DPacket::new(0, Some(6144), Some(6144), 4882, true),
            DPacket::new(0, Some(11026), Some(11026), 6144, true),
            DPacket::new(0, Some(17170), Some(17170), 4882, true),
            DPacket::new(0, Some(22052), Some(22052), 6144, true),
            DPacket::new(0, Some(28196), Some(28196), 4882, true),
            DPacket::new(0, Some(33078), Some(33078), 6144, true),
            DPacket::new(0, Some(39222), Some(39222), 4882, true),
            DPacket::new(0, Some(44104), Some(44104), 6144, true),
            DPacket::new(0, Some(50248), Some(50248), 4882, true)]));
    }

    #[test]
    fn test_demux_rle_qdmc() {
        // sample from https://samples.mplayerhq.hu/V-codecs/QTRLE/
        test_core("assets/QT/tidemo1-24bit-rle.mov", DemuxMode::Normal, 32, DemuxerTestMode::Test(vec![
            DPacket::new(0, Some(0), Some(0), 1486, true),
            DPacket::new(1, Some(0), Some(0), 309332, true),
            DPacket::new(0, Some(1), Some(1), 1486, false),
            DPacket::new(1, Some(240), Some(240), 10051, false),
            DPacket::new(0, Some(2), Some(2), 1486, false),
            DPacket::new(0, Some(3), Some(3), 1486, true),
            DPacket::new(1, Some(300), Some(300), 23679, false),
            DPacket::new(1, Some(360), Some(360), 51289, false),
            DPacket::new(0, Some(4), Some(4), 1486, false),
            DPacket::new(1, Some(420), Some(420), 41900, false),
            DPacket::new(1, Some(480), Some(480), 46795, false),
            DPacket::new(0, Some(5), Some(5), 1486, false),
            DPacket::new(1, Some(540), Some(540), 24484, false),
            DPacket::new(1, Some(600), Some(600), 27925, false),
            DPacket::new(0, Some(6), Some(6), 1486, true),
            DPacket::new(1, Some(660), Some(660), 19757, false),
            DPacket::new(1, Some(1680), Some(1680), 46203, false),
            DPacket::new(0, Some(7), Some(7), 1486, false),
            DPacket::new(0, Some(8), Some(8), 1486, false),
            DPacket::new(0, Some(9), Some(9), 1486, true),
            DPacket::new(0, Some(10), Some(10), 1486, false),
            DPacket::new(0, Some(11), Some(11), 1486, true),
            DPacket::new(0, Some(12), Some(12), 1486, false),
            DPacket::new(0, Some(13), Some(13), 1486, false),
            DPacket::new(0, Some(14), Some(14), 1486, true),
            DPacket::new(0, Some(15), Some(15), 1486, false),
            DPacket::new(0, Some(16), Some(16), 1486, false),
            DPacket::new(1, Some(1740), Some(1740), 42094, false),
            DPacket::new(1, Some(1800), Some(1800), 12453, false),
            DPacket::new(0, Some(17), Some(17), 1486, true),
            DPacket::new(1, Some(2040), Some(2040), 14418, false),
            DPacket::new(0, Some(18), Some(18), 1486, false)]));
    }
    #[test]
    fn test_demux_asp_adpcm() {
        // sample from https://samples.mplayerhq.hu/A-codecs/ima-adpcm/
        test_core("assets/QT/shuffle-ima41.mov", DemuxMode::Normal, 32, DemuxerTestMode::Test(vec![
            DPacket::new(0, Some(0), Some(0), 68, true),
            DPacket::new(1, Some(0), Some(0), 11494, true),
            DPacket::new(0, Some(1), Some(1), 68, false),
            DPacket::new(1, Some(1), Some(1), 3070, false),
            DPacket::new(0, Some(2), Some(2), 68, false),
            DPacket::new(0, Some(3), Some(3), 68, false),
            DPacket::new(0, Some(4), Some(4), 68, false),
            DPacket::new(0, Some(5), Some(5), 68, false),
            DPacket::new(0, Some(6), Some(6), 68, false),
            DPacket::new(0, Some(7), Some(7), 68, false),
            DPacket::new(0, Some(8), Some(8), 68, false),
            DPacket::new(0, Some(9), Some(9), 68, false),
            DPacket::new(0, Some(10), Some(10), 68, false),
            DPacket::new(0, Some(11), Some(11), 68, false),
            DPacket::new(0, Some(12), Some(12), 68, false),
            DPacket::new(0, Some(13), Some(13), 68, false),
            DPacket::new(0, Some(14), Some(14), 68, false),
            DPacket::new(0, Some(15), Some(15), 68, false),
            DPacket::new(0, Some(16), Some(16), 68, false),
            DPacket::new(0, Some(17), Some(17), 68, false),
            DPacket::new(0, Some(18), Some(18), 68, false),
            DPacket::new(0, Some(19), Some(19), 68, false),
            DPacket::new(0, Some(20), Some(20), 68, false),
            DPacket::new(0, Some(21), Some(21), 68, false),
            DPacket::new(0, Some(22), Some(22), 68, false),
            DPacket::new(0, Some(23), Some(23), 68, false),
            DPacket::new(0, Some(24), Some(24), 68, false),
            DPacket::new(0, Some(25), Some(25), 68, false),
            DPacket::new(0, Some(26), Some(26), 68, false),
            DPacket::new(0, Some(27), Some(27), 68, false),
            DPacket::new(0, Some(28), Some(28), 68, false),
            DPacket::new(0, Some(29), Some(29), 68, false)]));
    }
    #[test]
    fn test_demux_svq3_mp3() {
        // sample from https://samples.mplayerhq.hu/V-codecs/SVQ3/
        test_core("assets/QT/broken_sword_Large.mov", DemuxMode::Normal, 32, DemuxerTestMode::Test(vec![
            DPacket::new(0, Some(0), Some(0), 4620, true),
            DPacket::new(1, Some(0), Some(0), 834, true),
            DPacket::new(0, Some(24), Some(24), 1454, false),
            DPacket::new(1, Some(1), Some(1), 834, false),
            DPacket::new(0, Some(48), Some(48), 278, false),
            DPacket::new(1, Some(2), Some(2), 834, false),
            DPacket::new(0, Some(72), Some(72), 484, false),
            DPacket::new(1, Some(3), Some(3), 834, false),
            DPacket::new(0, Some(96), Some(96), 264, false),
            DPacket::new(1, Some(4), Some(4), 834, false),
            DPacket::new(0, Some(120), Some(120), 484, false),
            DPacket::new(0, Some(144), Some(144), 264, false),
            DPacket::new(1, Some(5), Some(5), 834, false),
            DPacket::new(0, Some(168), Some(168), 484, false),
            DPacket::new(1, Some(6), Some(6), 834, false),
            DPacket::new(0, Some(192), Some(192), 264, false),
            DPacket::new(1, Some(7), Some(7), 834, false),
            DPacket::new(0, Some(216), Some(216), 484, false),
            DPacket::new(0, Some(240), Some(240), 264, false),
            DPacket::new(1, Some(8), Some(8), 834, false),
            DPacket::new(0, Some(264), Some(264), 484, false),
            DPacket::new(1, Some(9), Some(9), 834, false),
            DPacket::new(0, Some(288), Some(288), 264, false),
            DPacket::new(1, Some(10), Some(10), 834, true),
            DPacket::new(0, Some(312), Some(312), 484, false),
            DPacket::new(0, Some(336), Some(336), 264, false),
            DPacket::new(1, Some(11), Some(11), 834, false),
            DPacket::new(0, Some(360), Some(360), 484, false),
            DPacket::new(1, Some(12), Some(12), 834, false),
            DPacket::new(0, Some(384), Some(384), 264, false),
            DPacket::new(1, Some(13), Some(13), 834, false),
            DPacket::new(0, Some(408), Some(408), 484, false)]));
    }
}
