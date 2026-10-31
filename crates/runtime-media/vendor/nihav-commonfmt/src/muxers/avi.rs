use nihav_core::muxers::*;
use nihav_registry::register::*;

const MAX_RIFF_BLOCKS: usize = 16; // ~16GB file should be enough
const MAX_RIFF_SIZE: u64 = 900 << 20; // 900MB of payload plus index data will hopefully fit in 1GB chunk limit

#[derive(Clone,Copy)]
struct ForcedTag {
    stream:     u32,
    tag:        [u8; 4],
}

#[derive(Clone,Copy)]
struct IdxEntry {
    tag:        [u8; 4],
    key:        bool,
    pos:        u64,
    len:        u32,
}

#[derive(Clone,Copy)]
struct AVIStream {
    strh_pos:   u64,
    indx_pos:   u64,
    nframes:    u32,
    lframes:    u32,
    is_video:   bool,
    max_size:   u32,
    pal_change: bool,
}

struct AVIMuxer<'a> {
    bw:             &'a mut dyn ByteIO,
    index:          Vec<IdxEntry>,
    video_str:      Option<usize>,
    video_id:       u32,
    data_pos:       u64,
    stream_info:    Vec<AVIStream>,
    pal_pos:        Vec<u32>,
    odml_pos:       u64,
    riff_blk:       usize,
    blk_start:      u64,
    wrote_hdr:      bool,
    forced_tags:    Vec<ForcedTag>,
}

impl<'a> AVIMuxer<'a> {
    fn new(bw: &'a mut dyn ByteIO) -> Self {
        Self {
            bw,
            index:          Vec::new(),
            video_str:      None,
            video_id:       0,
            data_pos:       0,
            stream_info:    Vec::with_capacity(2),
            pal_pos:        Vec::with_capacity(2),
            odml_pos:       0,
            riff_blk:       0,
            blk_start:      0,
            wrote_hdr:      true,
            forced_tags:    Vec::new(),
        }
    }
    fn patch_stream_stats(&mut self) -> MuxerResult<()> {
        let mut max_frames = 0;
        let mut max_size = 0;
        for sstat in self.stream_info.iter() {
            max_frames = max_frames.max(sstat.nframes);
            max_size = max_size.max(sstat.max_size);
            if sstat.pal_change {
                self.bw.seek(SeekFrom::Start(sstat.strh_pos))?;
                self.bw.write_u32le(0x00010000)?;
            }
            self.bw.seek(SeekFrom::Start(sstat.strh_pos + 0x18))?;
            self.bw.write_u32le(if sstat.is_video { sstat.nframes } else { 0 })?;
            self.bw.write_u32le(sstat.max_size)?;
        }
        self.bw.seek(SeekFrom::Start(0x30))?;
        self.bw.write_u32le(max_frames)?;
        self.bw.seek(SeekFrom::Current(8))?;
        self.bw.write_u32le(max_size)?;
        Ok(())
    }
    fn flush_riff_block(&mut self) -> MuxerResult<()> {
        for (sidx, stream) in self.stream_info.iter_mut().enumerate() {
            let idx_pos = self.bw.tell();

            let tag0 = b'0' + ((sidx / 10) as u8);
            let tag1 = b'0' + ((sidx % 10) as u8);

            let nentries = self.index.iter().filter(|idx| idx.tag[0] == tag0 && idx.tag[1] == tag1 && &idx.tag[2..] != b"pc").count();

            let tag = [b'i', b'x', tag0, tag1];
            self.bw.write_buf(&tag)?;
            self.bw.write_u32le((6 * 4 + nentries * 8) as u32)?;
            self.bw.write_u16le(2)?;
            self.bw.write_byte(0)?;
            self.bw.write_byte(1)?; // AVI_INDEX_OF_CHUNKS
            let mut chunk_id = [tag0, tag1, 0, 0];
            if stream.is_video {
                chunk_id[2] = b'd';
                chunk_id[3] = b'c';
            } else {
                chunk_id[2] = b'w';
                chunk_id[3] = b'b';
            }
            self.bw.write_u32le(nentries as u32)?;
            self.bw.write_buf(&chunk_id)?;
            self.bw.write_u64le(self.blk_start)?;
            self.bw.write_u32le(0)?;

            for entry in self.index.iter().filter(|idx| idx.tag[0] == tag0 && idx.tag[1] == tag1 && &idx.tag[2..] != b"pc") {
                self.bw.write_u32le((entry.pos - self.blk_start + 8) as u32)?;
                self.bw.write_u32le(entry.len)?;
            }

            let idx_end = self.bw.tell();

            self.bw.seek(SeekFrom::Start(stream.indx_pos + 8 * 4 + (self.riff_blk as u64) * 16))?;
            self.bw.write_u64le(idx_pos)?;
            self.bw.write_u32le((idx_end - idx_pos) as u32)?;
            self.bw.write_u32le(stream.lframes)?;
            stream.lframes = 0;

            self.bw.seek(SeekFrom::Start(idx_end))?;
        }

        patch_size(self.bw, self.blk_start + 8)?;
        patch_size(self.bw, self.data_pos)?;

        self.index.clear();

        self.riff_blk += 1;
        self.blk_start = self.bw.tell();
        self.wrote_hdr = false;

        // keep updating actual stream statistics and flags
        self.patch_stream_stats()?;

        Ok(())
    }
}

fn patch_size(bw: &mut dyn ByteIO, pos: u64) -> MuxerResult<()> {
    let size = bw.tell() - pos;
    if (bw.tell() & 1) != 0 {
        bw.write_byte(0)?;
    }
    bw.seek(SeekFrom::Current(-((((size + 1) & !1) + 4) as i64)))?;
    bw.write_u32le(size as u32)?;
    bw.seek(SeekFrom::End(0))?;
    Ok(())
}

fn gen_chunk_tag(stype: StreamType, str_no: u32) -> [u8; 4] {
    let c0 = b'0' + ((str_no / 10) as u8);
    let c1 = b'0' + ((str_no % 10) as u8);
    match stype {
        StreamType::Video => [c0, c1, b'd', b'c'],
        StreamType::Audio => [c0, c1, b'w', b'b'],
        StreamType::Subtitles => [c0, c1, b't', b'x'],
        _ => unimplemented!()
    }
}

impl<'a> MuxCore<'a> for AVIMuxer<'a> {
    fn create(&mut self, strmgr: &StreamManager) -> MuxerResult<()> {
        if strmgr.get_num_streams() == 0 {
            return Err(MuxerError::InvalidArgument);
        }
        if strmgr.get_num_streams() > 99 {
            return Err(MuxerError::UnsupportedFormat);
        }
        for (str_no, strm) in strmgr.iter().enumerate() {
            if strm.get_media_type() == StreamType::Video {
                self.video_str = Some(str_no);
                self.video_id  = strm.id;
                break;
            }
        }
        let (vinfo, tb_num, tb_den) = if let Some(str_id) = self.video_str {
                let vstr = strmgr.get_stream(str_id).unwrap();
                (vstr.get_info(), vstr.tb_num, vstr.tb_den)
            } else {
                (NACodecInfo::new_dummy(), 0, 1)
            };
        let hdrl_pos = self.bw.tell() + 20;
        self.bw.write_buf(b"RIFF\0\0\0\0AVI LIST\0\0\0\0hdrlavih")?;
        self.bw.write_u32le(56)?; // avih size
        let ms_per_frame = NATimeInfo::rescale_ts(1, tb_num, tb_den, 1, 1000000);
        self.bw.write_u32le(ms_per_frame as u32)?;
        self.bw.write_u32le(0)?; // max transfer rate
        self.bw.write_u32le(0)?; // padding granularity
        self.bw.write_u32le(0)?; // flags
        self.bw.write_u32le(0)?; // total frames
        self.bw.write_u32le(0)?; // initial frames
        self.bw.write_u32le(strmgr.get_num_streams() as u32)?;
        self.bw.write_u32le(0)?; // suggested buffer size
        if let NACodecTypeInfo::Video(ref vinfo) = vinfo.get_properties() {
            self.bw.write_u32le(vinfo.width as u32)?;
            self.bw.write_u32le(vinfo.height as u32)?;
        } else {
            self.bw.write_u32le(0)?;
            self.bw.write_u32le(0)?;
        }
        self.bw.write_u32le(0)?; // reserved
        self.bw.write_u32le(0)?; // reserved
        self.bw.write_u32le(0)?; // reserved
        self.bw.write_u32le(0)?; // reserved

        self.pal_pos.clear();
        self.pal_pos.resize(strmgr.get_num_streams(), 0);
        for (strno, strm) in strmgr.iter().enumerate() {
            let strl_pos = self.bw.tell() + 8;
            self.bw.write_buf(b"LIST\0\0\0\0strlstrh")?;
            self.bw.write_u32le(56)?; // strh size

            let forced_tag = self.forced_tags.iter().find(|tag| tag.stream == strno as u32);
            match strm.get_media_type() {
                StreamType::Video => {
                    self.bw.write_buf(b"vids")?;
                    let fcc = if strm.get_info().get_name() == "rawvideo-ms" {
                            Some(*b"DIB ")
                        } else { find_avi_fourcc(strm.get_info().get_name()) };
                    if fcc.is_none() {
                        return Err(MuxerError::UnsupportedFormat);
                    }
                    self.bw.write_buf(&fcc.unwrap_or([0; 4]))?;
                    let vinfo = strm.get_info().get_properties().get_video_info().unwrap();
                    if vinfo.width >= (1 << 16) || vinfo.height >= (1 << 16) {
                        return Err(MuxerError::UnsupportedFormat);
                    }
                },
                StreamType::Audio => {
                    self.bw.write_buf(b"auds")?;
                    self.bw.write_u32le(0)?;
                },
                StreamType::Subtitles => {
                    self.bw.write_buf(b"txts")?;
                    self.bw.write_u32le(0)?;
                },
                _ => return Err(MuxerError::UnsupportedFormat),
            };
            self.stream_info.push(AVIStream {
                    strh_pos:   self.bw.tell(),
                    indx_pos:   0,
                    is_video:   strm.get_media_type() == StreamType::Video,
                    nframes:    0,
                    lframes:    0,
                    max_size:   0,
                    pal_change: false,
                });

            self.bw.write_u32le(0)?; // flags
            self.bw.write_u16le(0)?; // priority
            self.bw.write_u16le(0)?; // language
            self.bw.write_u32le(0)?; // initial frames
            if strm.get_info().get_name() != "pcm" {
                self.bw.write_u32le(strm.tb_num)?;
                self.bw.write_u32le(strm.tb_den)?;
            } else {
                let ainfo = strm.get_info().get_properties().get_audio_info().unwrap();
                self.bw.write_u32le(1)?;
                self.bw.write_u32le(ainfo.sample_rate)?;
            }
            self.bw.write_u32le(0)?; // start
            self.bw.write_u32le(0)?; // length
            self.bw.write_u32le(0)?; // suggested buffer size
            self.bw.write_u32le(0)?; // quality
            self.bw.write_u32le(0)?; // sample_size
            self.bw.write_u16le(0)?; // x
            self.bw.write_u16le(0)?; // y
            self.bw.write_u16le(0)?; // w
            self.bw.write_u16le(0)?; // h

            self.bw.write_buf(b"strf")?;
            self.bw.write_u32le(0)?;
            let strf_pos = self.bw.tell();
            match strm.get_media_type() {
                StreamType::Video => {
                    let vinfo = strm.get_info().get_properties().get_video_info().unwrap();
                    self.bw.write_u32le(40)?;
                    self.bw.write_u32le(vinfo.width as u32)?;
                    self.bw.write_u32le(vinfo.height as u32)?;
                    if !vinfo.format.palette {
                        if strm.get_info().get_name() != "rawvideo-ms" {
                            self.bw.write_u16le(vinfo.format.components as u16)?;
                        } else {
                            self.bw.write_u16le(1)?;
                        }
                        let mut depth = vinfo.format.get_total_depth();
                        if depth == 15 {
                            depth = 16;
                        }
                        if depth == 8 {
                            depth = 24;
                        }
                        self.bw.write_u16le(depth.into())?;
                    } else {
                        self.bw.write_u16le(1)?;
                        self.bw.write_u16le(8)?;
                    }
                    let fcc = if let Some(ftag) = forced_tag {
                                Some(ftag.tag)
                            } else if strm.get_info().get_name() == "rawvideo-ms" {
                                Some([0; 4])
                            } else { find_avi_fourcc(strm.get_info().get_name()) };
                    if fcc.is_none() {
                        return Err(MuxerError::UnsupportedFormat);
                    }
                    self.bw.write_buf(&fcc.unwrap_or([0; 4]))?;
                    self.bw.write_u32le(0)?; // image size
                    self.bw.write_u32le(0)?; // x dpi
                    self.bw.write_u32le(0)?; // y dpi
                    if vinfo.format.palette {
                        let nclrs = 1 << vinfo.bits.min(8);
                        self.bw.write_u32le(nclrs)?; // total colors
                        self.bw.write_u32le(0)?; // important colors
                        self.pal_pos[strno] = self.bw.tell() as u32;
                        for _ in 0..(nclrs as usize) {
                            self.bw.write_u32le(0)?;
                        }
                    } else {
                        self.bw.write_u32le(0)?; // total colors
                        self.bw.write_u32le(0)?; // important colors
                    }
                    if let Some(ref edata) = strm.get_info().get_extradata() {
                        self.bw.write_buf(edata.as_slice())?;
                    }
                },
                StreamType::Audio => {
                    let ainfo = strm.get_info().get_properties().get_audio_info().unwrap();
                    let twocc = if let Some(ftag) = forced_tag {
                            Some(read_u16be(&ftag.tag).unwrap_or(0))
                        } else { find_wav_twocc(strm.get_info().get_name()) };
                    if twocc.is_none() {
                        return Err(MuxerError::UnsupportedFormat);
                    }
                    self.bw.write_u16le(twocc.unwrap_or(0))?;
                    self.bw.write_u16le(ainfo.channels as u16)?;
                    self.bw.write_u32le(ainfo.sample_rate)?;
                    if strm.get_info().get_name() != "pcm" {
                        self.bw.write_u32le(0)?; // avg bytes per second
                        self.bw.write_u16le(ainfo.block_len as u16)?;
                    } else {
                        let blk_size = u32::from(ainfo.channels) * u32::from(ainfo.format.bits) / 8;
                        let avg_bytes = ainfo.sample_rate * blk_size;
                        self.bw.write_u32le(avg_bytes)?;
                        self.bw.write_u16le(blk_size as u16)?;
                    }
                    self.bw.write_u16le(ainfo.format.bits as u16)?;
                    if let Some(ref edata) = strm.get_info().get_extradata() {
                        self.bw.write_buf(edata.as_slice())?;
                    }
                },
                StreamType::Subtitles => {
                    if let Some(ref edata) = strm.get_info().get_extradata() {
                        self.bw.write_buf(edata.as_slice())?;
                    }
                },
                _ => unreachable!(),
            };
            patch_size(self.bw, strf_pos)?;
            /* OpenDML */ {
                if let Some(sstat) = self.stream_info.last_mut() {
                    sstat.indx_pos = self.bw.tell();
                }
                self.bw.write_buf(b"indx")?;
                self.bw.write_u32le((6 * 4 + MAX_RIFF_BLOCKS * 16) as u32)?;
                self.bw.write_u16le(4)?;
                self.bw.write_byte(0)?;
                self.bw.write_byte(0)?; // AVI_INDEX_OF_INDEXES
                self.bw.write_u32le(0)?; // num entries
                let mut ix_tag = [b'0' + ((strno / 10) as u8), b'0' + ((strno % 10) as u8), 0, 0];
                match strm.get_media_type() {
                    StreamType::Video => {
                        ix_tag[2] = b'd';
                        ix_tag[3] = b'b';
                    },
                    StreamType::Audio => {
                        ix_tag[2] = b'w';
                        ix_tag[3] = b'b';
                    },
                    _ => return Err(MuxerError::NotImplemented),
                }
                self.bw.write_buf(&ix_tag)?;
                for _ in 0..3 {
                    self.bw.write_u32le(0)?;
                }
                for _ in 0..MAX_RIFF_BLOCKS {
                    self.bw.write_u64le(0)?;
                    self.bw.write_u32le(0)?;
                    self.bw.write_u32le(0)?;
                }

                /*if let StreamInfo::Video(ref vinfo) = stream {
                    self.bw.write_buf(b"vprp")?;
                    self.bw.write_u32le(0x44)?;
                    self.bw.write_u32le(0)?; // video format token
                    self.bw.write_u32le(0)?; // video standard
                    self.bw.write_u32le((vinfo.tb_den + vinfo.tb_num / 2) / vinfo.tb_num)?; // refresh rate
                    self.bw.write_u32le(vinfo.width as u32)?;
                    self.bw.write_u32le(vinfo.height as u32)?;
                    self.bw.write_u32le(0x00010001)?; // aspect ratio
                    self.bw.write_u32le(vinfo.width as u32)?;
                    self.bw.write_u32le(vinfo.height as u32)?;
                    self.bw.write_u32le(1)?; // single field
                    self.bw.write_u32le(vinfo.height as u32)?;
                    self.bw.write_u32le(vinfo.width as u32)?;
                    self.bw.write_u32le(vinfo.height as u32)?;
                    self.bw.write_u32le(vinfo.width as u32)?;
                    self.bw.write_u32le(0)?;
                    self.bw.write_u32le(0)?;
                    self.bw.write_u32le(0)?;
                    self.bw.write_u32le(0)?;
                }*/
            }
            patch_size(self.bw, strl_pos)?;
        }
        /* OpenDML */ {
            self.odml_pos = self.bw.tell();
            self.bw.write_buf(b"LIST")?;
            self.bw.write_u32le(16)?;
            self.bw.write_buf(b"odmldmlh")?;
            self.bw.write_u32le(4)?;
            self.bw.write_u32le(0)?;
        }
        patch_size(self.bw, hdrl_pos)?;

        self.data_pos = self.bw.tell() + 8;
        self.bw.write_buf(b"LIST\0\0\0\0movi")?;

        Ok(())
    }
    fn mux_frame(&mut self, _strmgr: &StreamManager, pkt: NAPacket) -> MuxerResult<()> {
        if self.data_pos == 0 {
            return Err(MuxerError::NotCreated);
        }
        let stream = pkt.get_stream();
        let str_num = stream.get_num();
        if str_num > 99 || str_num >= self.stream_info.len() {
            return Err(MuxerError::UnsupportedFormat);
        }

        if self.bw.tell() > self.blk_start + MAX_RIFF_SIZE {
            self.flush_riff_block()?;
        }

        if !self.wrote_hdr {
            if self.riff_blk >= MAX_RIFF_BLOCKS {
                return Err(MuxerError::NotPossible);
            }
            self.bw.seek(SeekFrom::Start(self.blk_start))?;

            self.bw.write_buf(b"RIFF")?;
            self.bw.write_u32le(0)?;
            self.bw.write_buf(b"AVIX")?;
            self.bw.write_buf(b"LIST")?;
            self.bw.write_u32le(0)?;
            self.data_pos = self.bw.tell();
            self.bw.write_buf(b"movi")?;

            self.wrote_hdr = true;
        }

        let chunk_len = pkt.get_buffer().len() as u32;

        if self.pal_pos[str_num] != 0 {
            for sdata in pkt.side_data.iter() {
                if let NASideData::Palette(_, ref pal) = sdata {
                    let cur_pos = self.bw.tell();
                    self.bw.seek(SeekFrom::Start(u64::from(self.pal_pos[str_num])))?;

                    let vinfo = stream.get_info().get_properties().get_video_info().unwrap();
                    let nclrs = 1 << vinfo.bits.min(8);
                    for quad in pal.chunks(4).take(nclrs) {
                        self.bw.write_byte(quad[2])?;
                        self.bw.write_byte(quad[1])?;
                        self.bw.write_byte(quad[0])?;
                        self.bw.write_byte(0)?;
                    }
                    self.bw.seek(SeekFrom::Start(cur_pos))?;
                    self.pal_pos[str_num] = 0;
                    break;
                }
            }
        } else {
            for sdata in pkt.side_data.iter() {
                if let NASideData::Palette(true, ref pal) = sdata {
                    //todo search for changed region
                    let start_clr = 0usize;
                    let end_clr = 256usize;
                    if start_clr < end_clr {
                        let chunk_len = ((end_clr - start_clr) as u32) * 4 + 4;
                        let mut tag = gen_chunk_tag(StreamType::Video, str_num as u32);
                        tag[2] = b'p';
                        tag[3] = b'c';
                        self.index.push(IdxEntry {
                                tag,
                                key:    false,
                                pos:    self.bw.tell(),
                                len:    chunk_len });
                        self.bw.write_buf(&tag)?;
                        self.bw.write_u32le(chunk_len)?;
                        self.bw.write_byte(start_clr as u8)?;
                        self.bw.write_byte((end_clr - start_clr) as u8)?;
                        self.bw.write_u16le(0)?; //flags
                        self.bw.write_buf(&pal[start_clr * 4..end_clr * 4])?;
                        self.stream_info[str_num].pal_change = true;
                    }
                }
            }
        }

        self.stream_info[str_num].nframes += 1;
        self.stream_info[str_num].lframes += 1;
        self.stream_info[str_num].max_size = self.stream_info[str_num].max_size.max(chunk_len);
        let tag = gen_chunk_tag(stream.get_media_type(), str_num as u32);
        self.index.push(IdxEntry {
                tag,
                key:    pkt.keyframe,
                pos:    self.bw.tell(),
                len:    chunk_len });
        self.bw.write_buf(&tag)?;
        self.bw.write_u32le(chunk_len)?;
        self.bw.write_buf(pkt.get_buffer().as_slice())?;
        if (self.bw.tell() & 1) != 0 {
            self.bw.write_byte(0)?;
        }
        Ok(())
    }
    fn flush(&mut self) -> MuxerResult<()> {
        Ok(())
    }
    fn end(&mut self) -> MuxerResult<()> {
        if self.riff_blk == 0 && self.index.is_empty() {
            return Err(MuxerError::NotCreated);
        }
        if self.riff_blk > 0 {
            self.flush_riff_block()?;
            let mut max_frames = 0;
            for sstat in self.stream_info.iter() {
                max_frames = max_frames.max(sstat.nframes);

                self.bw.seek(SeekFrom::Start(sstat.indx_pos + 12))?;
                self.bw.write_u32le(self.riff_blk as u32)?;
            }
            self.bw.seek(SeekFrom::Start(self.odml_pos + 20))?;
            self.bw.write_u32le(max_frames)?;

            self.bw.flush()?;
            Ok(())
        } else if !self.index.is_empty() {
            patch_size(self.bw, self.data_pos)?;
            if !self.index.is_empty() {
                self.bw.write_buf(b"idx1")?;
                self.bw.write_u32le((self.index.len() * 16) as u32)?;
                for item in self.index.iter() {
                    self.bw.write_buf(&item.tag)?;
                    let flags = if item.key { 0x10 }
                                else if &item.tag[2..] == b"pc" { 0x100 }
                                else { 0 };
                    self.bw.write_u32le(flags)?;
                    self.bw.write_u32le(item.pos as u32)?;
                    self.bw.write_u32le(item.len)?;
                }
            }
            patch_size(self.bw, 8)?;

            self.patch_stream_stats()?;

            // patch out useless OpenDML entries
            self.bw.seek(SeekFrom::Start(self.odml_pos))?;
            self.bw.write_buf(b"JUNK")?;
            for sstat in self.stream_info.iter() {
                self.bw.seek(SeekFrom::Start(sstat.indx_pos))?;
                self.bw.write_buf(b"JUNK")?;
            }

            self.bw.flush()?;

            Ok(())
        } else {
            Err(MuxerError::NotCreated)
        }
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
    if src.len() - pos < 3 + 2 || !src[pos..].starts_with(b"tag") {
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

impl<'a> NAOptionHandler for AVIMuxer<'a> {
    fn get_supported_options(&self) -> &[NAOptionDefinition] { MUXER_OPTS }
    fn set_options(&mut self, options: &[NAOption]) {
        for opt in options.iter() {
            if let NAOption { name: FORCE_STREAM_TAG_OPTION , value: NAValue::String(ref stag) } = opt {
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

pub struct AVIMuxerCreator {}

impl MuxerCreator for AVIMuxerCreator {
    fn new_muxer<'a>(&self, bw: &'a mut dyn ByteIO) -> Box<dyn MuxCore<'a> + 'a> {
        Box::new(AVIMuxer::new(bw))
    }
    fn get_name(&self) -> &'static str { "avi" }
    fn get_capabilities(&self) -> MuxerCapabilities { MuxerCapabilities::Universal }
    fn get_quirks(&self) -> MuxerQuirks { MuxerQuirks(MUX_QUIRK_FIXED_RATE) }
}

#[cfg(test)]
mod test {
    use nihav_core::codecs::*;
    use nihav_core::demuxers::*;
    use nihav_core::muxers::*;
    use nihav_codec_support::test::enc_video::*;
    use crate::*;

    #[test]
    fn test_avi_muxer() {
        let mut dmx_reg = RegisteredDemuxers::new();
        generic_register_all_demuxers(&mut dmx_reg);
        //test sample: https://samples.mplayerhq.hu/V-codecs/RT21/320x240/laser05.avi
        let dec_config = DecoderTestParams {
                demuxer:        "avi",
                in_name:        "assets/Indeo/laser05.avi",
                limit:          None,
                stream_type:    StreamType::None,
                dmx_reg, dec_reg: RegisteredDecoders::new(),
            };
        let mut mux_reg = RegisteredMuxers::new();
        generic_register_all_muxers(&mut mux_reg);
        /*let enc_config = EncoderTestParams {
                muxer:      "avi",
                enc_name:   "",
                out_name:   "muxed.avi",
                mux_reg, enc_reg: RegisteredEncoders::new(),
            };
        test_remuxing(&dec_config, &enc_config);*/
        test_remuxing_md5(&dec_config, "avi", &mux_reg,
                          [0xc5306965, 0xfe9ae8b8, 0x91c38644, 0x21ab366d]);
    }
}
