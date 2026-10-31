use nihav_core::demuxers::*;
use nihav_registry::register::*;
use nihav_codec_support::codecs::qt_pal::*;

use super::*;
use super::pktread::*;

macro_rules! mktag {
    ($a:expr, $b:expr, $c:expr, $d:expr) => ({
        (($a as u32) << 24) | (($b as u32) << 16) | (($c as u32) << 8) | ($d as u32)
    });
    ($arr:expr) => ({
        (($arr[0] as u32) << 24) | (($arr[1] as u32) << 16) | (($arr[2] as u32) << 8) | ($arr[3] as u32)
    });
}

const QT_RGB555_FORMAT: NAPixelFormaton = NAPixelFormaton::make_rgb16_fmt(5, 5, 5, true, true);

pub const ARGB_FORMAT: NAPixelFormaton = NAPixelFormaton {
        model: ColourModel::RGB(RGBSubmodel::RGB), components: 4,
        comp_info: [
            Some(NAPixelChromaton{ h_ss: 0, v_ss: 0, packed: true, depth: 8, shift: 0, comp_offs: 1, next_elem: 4 }),
            Some(NAPixelChromaton{ h_ss: 0, v_ss: 0, packed: true, depth: 8, shift: 0, comp_offs: 2, next_elem: 4 }),
            Some(NAPixelChromaton{ h_ss: 0, v_ss: 0, packed: true, depth: 8, shift: 0, comp_offs: 3, next_elem: 4 }),
            Some(NAPixelChromaton{ h_ss: 0, v_ss: 0, packed: true, depth: 8, shift: 0, comp_offs: 0, next_elem: 4 }),
            None ],
        elem_size: 4, be: false, alpha: true, palette: false };

struct TrackChunkHandler {
    ctype:  u32,
    parse:  fn(track: &mut Track, br: &mut dyn ByteIO, size: u64) -> DemuxerResult<u64>,
}

const IGNORED_CHUNKS: &[u32] = &[
    mktag!(b"free"), mktag!(b"skip"), mktag!(b"udta"), mktag!(b"wide")
];

fn skip_chunk(_track: &mut Track, br: &mut dyn ByteIO, size: u64) -> DemuxerResult<u64> {
    br.skip64(size)?;
    Ok(size)
}

const TRAK_CHUNK_HANDLERS: &[TrackChunkHandler] = &[
    TrackChunkHandler { ctype: mktag!(b"clip"), parse: skip_chunk },
    TrackChunkHandler { ctype: mktag!(b"matt"), parse: skip_chunk },
    TrackChunkHandler { ctype: mktag!(b"edts"), parse: skip_chunk },
    TrackChunkHandler { ctype: mktag!(b"tref"), parse: skip_chunk },
    TrackChunkHandler { ctype: mktag!(b"load"), parse: skip_chunk },
    TrackChunkHandler { ctype: mktag!(b"imap"), parse: skip_chunk },
    TrackChunkHandler { ctype: mktag!(b"tkhd"), parse: read_tkhd },
    TrackChunkHandler { ctype: mktag!(b"mdia"), parse: read_mdia },
];

fn read_tkhd(track: &mut Track, br: &mut dyn ByteIO, size: u64) -> DemuxerResult<u64> {
    const KNOWN_TKHD_SIZE: u64 = 84;
    validate!(size >= KNOWN_TKHD_SIZE);
    let version             = br.read_byte()?;
    validate!(version == 0);
    let _flags              = br.read_u24be()?;
    let _ctime              = br.read_u32be()?;
    let _mtime              = br.read_u32be()?;
    let track_id            = br.read_u32be()?;
                              br.read_skip(4)?;
    let duration            = br.read_u32be()?;
                              br.read_skip(8)?;
    let _layer              = br.read_u16be()?;
    let _alt_group          = br.read_u16be()?;
    let _volume             = br.read_u16be()?;
                              br.read_skip(2)?;
                              br.read_skip(36)?; // matrix
    let width               = br.read_u32be()? as usize;
    let height              = br.read_u32be()? as usize;
    track.width  = width  >> 16;
    track.height = height >> 16;
    track.track_id = track_id;
    track.pkt_demux.duration = duration;

    track.tkhd_found = true;
    Ok(KNOWN_TKHD_SIZE)
}

fn read_mdia(track: &mut Track, br: &mut dyn ByteIO, size: u64) -> DemuxerResult<u64> {
    track.stream_type = StreamType::None;
    track.read_mdia(br, size)?;
    Ok(size)
}

const MDIA_CHUNK_HANDLERS: &[TrackChunkHandler] = &[
    TrackChunkHandler { ctype: mktag!(b"mdhd"), parse: read_mdhd },
    TrackChunkHandler { ctype: mktag!(b"hdlr"), parse: read_hdlr },
    TrackChunkHandler { ctype: mktag!(b"minf"), parse: read_minf },
    TrackChunkHandler { ctype: mktag!(b"udta"), parse: read_udta_mdia },
];

fn read_mdhd(track: &mut Track, br: &mut dyn ByteIO, size: u64) -> DemuxerResult<u64> {
    const KNOWN_MDHD_SIZE: u64 = 24;
    validate!(size >= KNOWN_MDHD_SIZE);
    let version             = br.read_byte()?;
    validate!(version == 0);
    let flags               = br.read_u24be()?;
    validate!(flags == 0);
    let _ctime              = br.read_u32be()?;
    let _mtime              = br.read_u32be()?;
    track.tb_den            = br.read_u32be()?;
    validate!(track.tb_den != 0);
    track.pkt_demux.tb_den = track.tb_den;
    track.pkt_demux.duration = br.read_u32be()?;
    let _language           = br.read_u16be()?;
    let _quality            = br.read_u16be()?;

    Ok(KNOWN_MDHD_SIZE)
}

fn read_hdlr(track: &mut Track, br: &mut dyn ByteIO, size: u64) -> DemuxerResult<u64> {
    const KNOWN_HDLR_SIZE: u64 = 24;
    validate!(size >= KNOWN_HDLR_SIZE);
    let version             = br.read_byte()?;
    validate!(version == 0);
    let flags               = br.read_u24be()?;
    validate!(flags == 0);
    let comp_type           = br.read_u32be()?;
    let comp_subtype        = br.read_u32be()?;
    let _comp_manufacturer  = br.read_u32be()?;
    let _comp_flags         = br.read_u32be()?;
    let _comp_flags_mask    = br.read_u32be()?;

    if comp_type == mktag!(b"mhlr") || comp_type == 0 {
        if comp_subtype == mktag!(b"vide") {
            track.stream_type = StreamType::Video;
        } else if comp_subtype == mktag!(b"soun") {
            track.stream_type = StreamType::Audio;
        } else {
            track.stream_type = StreamType::Data;
            track.handler = comp_subtype;
        }
    } else if comp_type == mktag!(b"dhlr") {
        track.stream_type = StreamType::Data;
        track.handler = 0xFFFFFFF;
    } else {
        println!("Unknown stream type");
        track.stream_type = StreamType::Data;
        track.handler = 0xFFFFFFF;
    }

    Ok(KNOWN_HDLR_SIZE)
}

fn read_udta_mdia(track: &mut Track, br: &mut dyn ByteIO, size: u64) -> DemuxerResult<u64> {
    validate!(size > 4);
    let version             = br.read_byte()?;
    validate!(version == 0);
    let size2               = u64::from(br.read_u24be()?);
    if size2 == size {
        let tag             = br.peek_tag()?;
        if &tag == b"NC00" && track.streams.len() == 1 {
            let ref_strm = &track.streams[0];
            let ref_info = ref_strm.get_info();
            if ref_info.get_extradata().is_none() {
                validate!(size > 8);
                let mut edata = vec![0; size2 as usize - 4];
                              br.read_buf(&mut edata)?;
                let name = ref_info.get_name();
                let info = ref_info.get_properties();
                let cinfo = NACodecInfo::new(name, info, Some(edata));
                let strm_id = ref_strm.get_id();
                track.streams[0] = NAStream::new(track.stream_type, strm_id, cinfo, track.tb_num, track.tb_den, u64::from(track.pkt_demux.duration));
                return Ok(size);
            }
        }
    }
    Ok(4)
}

fn read_minf(track: &mut Track, br: &mut dyn ByteIO, size: u64) -> DemuxerResult<u64> {
    track.read_minf(br, size)?;
    Ok(size)
}

const MINF_CHUNK_HANDLERS: &[TrackChunkHandler] = &[
    TrackChunkHandler { ctype: mktag!(b"hdlr"), parse: skip_chunk },
    TrackChunkHandler { ctype: mktag!(b"dinf"), parse: skip_chunk },
    TrackChunkHandler { ctype: mktag!(b"vmhd"), parse: read_vmhd },
    TrackChunkHandler { ctype: mktag!(b"smhd"), parse: read_smhd },
    TrackChunkHandler { ctype: mktag!(b"gmhd"), parse: read_gmhd },
    TrackChunkHandler { ctype: mktag!(b"gmin"), parse: read_gmin },
    TrackChunkHandler { ctype: mktag!(b"stbl"), parse: read_stbl },
];

fn read_vmhd(track: &mut Track, br: &mut dyn ByteIO, size: u64) -> DemuxerResult<u64> {
    const KNOWN_VMHD_SIZE: u64 = 12;
    validate!(track.stream_type == StreamType::Video);
    validate!(size >= KNOWN_VMHD_SIZE);
    let version             = br.read_byte()?;
    validate!(version == 0);
    let _flags              = br.read_u24be()?;
                              br.read_skip(2)?; // graphics mode
                              br.read_skip(6)?; // opcolor
    Ok(KNOWN_VMHD_SIZE)
}

fn read_smhd(track: &mut Track, br: &mut dyn ByteIO, size: u64) -> DemuxerResult<u64> {
    const KNOWN_SMHD_SIZE: u64 = 8;
    validate!(track.stream_type == StreamType::Audio);
    validate!(size >= KNOWN_SMHD_SIZE);
    let version             = br.read_byte()?;
    validate!(version == 0);
    let _flags              = br.read_u24be()?;
                              br.read_skip(2)?; // balance
                              br.read_skip(2)?;
    Ok(KNOWN_SMHD_SIZE)
}

fn read_gmhd(track: &mut Track, _br: &mut dyn ByteIO, _size: u64) -> DemuxerResult<u64> {
    validate!(track.stream_type == StreamType::Data);
    Ok(0)
}

fn read_gmin(track: &mut Track, br: &mut dyn ByteIO, size: u64) -> DemuxerResult<u64> {
    validate!(track.stream_type == StreamType::Data);
    const KNOWN_GMIN_SIZE: u64 = 16;
    validate!(size >= KNOWN_GMIN_SIZE);
    let version             = br.read_byte()?;
    validate!(version == 0);
    let _flags              = br.read_u24be()?;
                              br.read_skip(2)?; // graphics mode
                              br.read_skip(6)?; // opcolor
                              br.read_skip(2)?; // balance
                              br.read_skip(2)?;
    Ok(KNOWN_GMIN_SIZE)
}

fn read_stbl(track: &mut Track, br: &mut dyn ByteIO, size: u64) -> DemuxerResult<u64> {
    track.read_stbl(br, size)?;
    Ok(size)
}

const STBL_CHUNK_HANDLERS: &[TrackChunkHandler] = &[
    TrackChunkHandler { ctype: mktag!(b"stsd"), parse: read_stsd },
    TrackChunkHandler { ctype: mktag!(b"stts"), parse: read_stts },
    TrackChunkHandler { ctype: mktag!(b"stgs"), parse: read_stgs },
    TrackChunkHandler { ctype: mktag!(b"stss"), parse: read_stss },
    TrackChunkHandler { ctype: mktag!(b"stsc"), parse: read_stsc },
    TrackChunkHandler { ctype: mktag!(b"stsz"), parse: read_stsz },
    TrackChunkHandler { ctype: mktag!(b"stco"), parse: read_stco },
    TrackChunkHandler { ctype: mktag!(b"co64"), parse: read_co64 },
    TrackChunkHandler { ctype: mktag!(b"stsh"), parse: skip_chunk },
    TrackChunkHandler { ctype: mktag!(b"ctts"), parse: read_ctts },
];

fn parse_audio_edata(br: &mut dyn ByteIO, start_pos: u64, size: u64) -> DemuxerResult<Option<Vec<u8>>> {
    let cur_start = br.tell();
    let end_pos = start_pos + size;
    while br.tell() + 8 < end_pos {
        let size            = br.read_u32be()?;
        if size < 8 {
            break;
        }
        let tag             = br.read_tag()?;
        match &tag {
            b"chan" => {
                              br.read_skip((size - 8) as usize)?;
            },
            b"wave" => {
                if size <= 16 {
                    return Ok(None);
                }
                let wave_end    = br.tell() - 8 + u64::from(size);
                while br.tell() + 8 < wave_end {
                    let size    = br.read_u32be()?;
                    let tag     = br.read_tag()?;
                    if size > 8 {
                        if matches!(&tag, b"frma" | b"mp4a") {
                                  br.read_skip((size - 8) as usize)?;
                        } else {
                                  br.seek(SeekFrom::Current(-8))?;
                            let mut buf = vec![0; (wave_end - br.tell()) as usize];
                                  br.read_buf(&mut buf)?;
                            return Ok(Some(buf));
                        }
                    } else {
                        return Ok(None);
                    }
                }
                return Ok(None);
            },
            _ => break,
        }
    }
                              br.seek(SeekFrom::Start(cur_start))?;
    let mut buf = vec![0; (size - (cur_start - start_pos)) as usize];
                              br.read_buf(&mut buf)?;
    Ok(Some(buf))
}

#[allow(clippy::neg_cmp_op_on_partial_ord)]
fn read_stsd(track: &mut Track, br: &mut dyn ByteIO, size: u64) -> DemuxerResult<u64> {
    const KNOWN_STSD_SIZE: u64 = 24;
    validate!(size >= KNOWN_STSD_SIZE);
    let start_pos = br.tell();
    let version             = br.read_byte()?;
    validate!(version == 0);
    let _flags              = br.read_u24be()?;
    let entries             = br.read_u32be()? as usize;
    validate!(entries > 0);
    track.pal.clear();
    track.pal.resize(entries, None);
    for desc_no in 0..entries {
        let estart = br.tell();
        let esize           = u64::from(br.read_u32be()?);
        let end_pos = br.tell() - 4 + esize;
        validate!(esize > 8 && end_pos <= start_pos + size);
        let mut fcc = [0u8; 4];
                              br.read_buf(&mut fcc)?;
                              br.read_skip(6)?;
        let _data_ref       = br.read_u16be()?;

        track.pkt_demux.fcc = fcc;

        let codec_info;
        match track.stream_type {
            StreamType::Video => {
                let _ver            = br.read_u16be()?;
                let _revision       = br.read_u16le()?;
                let _vendor         = br.read_u32be()?;
                let _temp_quality   = br.read_u32be()?;
                let _spat_quality   = br.read_u32be()?;
                let width           = br.read_u16be()? as usize;
                let height          = br.read_u16be()? as usize;
                let _hor_res        = br.read_u32be()?;
                let _vert_res       = br.read_u32be()?;
                let data_size       = br.read_u32be()?;
                if !track.ver_m1 {
                    validate!(data_size == 0);
                }
                let _frame_count    = br.read_u16be()? as usize;
                let _cname_len      = br.read_byte()? as usize;
                                      br.read_skip(31)?; // actual compressor name
                let depth           = br.read_u16be()?;
                let ctable_id       = br.read_u16be()?;
                let grayscale = depth > 0x20 || depth == 1;
                let depth = if grayscale { depth & 0x1F } else { depth };
                if ctable_id == 0 {
                    let max_pal_size = start_pos + size - br.tell();
                    if depth <= 8 {
                        let mut pal = [0; 1024];
                        read_palette(br, max_pal_size, &mut pal)?;
                        track.pal[desc_no] = Some(Arc::new(pal));
                    } else {
                                      br.read_skip(max_pal_size as usize)?;
                    }
                } else if (depth <= 8) && !grayscale {
                    match depth & 0x1F {
                        2 => {
                            let mut pal = [0; 1024];
                            pal[..4 * 4].copy_from_slice(&MOV_DEFAULT_PAL_2BIT);
                            track.pal[desc_no] = Some(Arc::new(pal));
                        },
                        4 => {
                            let mut pal = [0; 1024];
                            pal[..16 * 4].copy_from_slice(&MOV_DEFAULT_PAL_4BIT);
                            track.pal[desc_no] = Some(Arc::new(pal));
                        },
                        8 => {
                            track.pal[desc_no] = Some(Arc::new(MOV_DEFAULT_PAL_8BIT));
                        },
                        _ => {},
                    };
                } else if grayscale && ctable_id != 0xFFFF {
                    let mut pal = [0; 1024];
                    let cdepth = depth & 0x1F;
                    let size = 1 << cdepth;
                    for i in 0..size {
                        let mut clr = ((size - 1 - i) as u8) << (8 - cdepth);
                        let mut off = 8 - cdepth;
                        while off >= cdepth {
                            clr |= clr >> (8 - off);
                            off -= cdepth;
                        }
                        if off > 0 {
                            clr |= clr >> (8 - off);
                        }
                        pal[i * 4]     = clr;
                        pal[i * 4 + 1] = clr;
                        pal[i * 4 + 2] = clr;
                    }
                    track.pal[desc_no] = Some(Arc::new(pal));
                }
// todo other atoms, put as extradata
                let cname = if let Some(name) = find_codec_from_mov_video_fourcc(&fcc) {
                        name
                    } else if let Some(name) = find_codec_from_avi_fourcc(&fcc) {
                        name
                    } else if &fcc == b"pdf " {
                        "pdf"
                    } else {
                        "unknown"
                    };
                let format = if cname == "rawvideo" {
                        if &fcc == b"j420" {
                            validate!(depth == 12);
                            YUV420_FORMAT
                        } else {
                            match depth {
                                1..=8 | 33..=40 => PAL8_FORMAT,
                                15 | 16 => QT_RGB555_FORMAT,
                                24 => RGB24_FORMAT,
                                32 => ARGB_FORMAT,
                                _ => {
                                    println!("unknown depth {depth}");
                                    return Err(DemuxerError::NotImplemented);
                                }
                            }
                        }
                    } else if depth > 8 && depth <= 32 {
                        RGB24_FORMAT
                    } else {
                        PAL8_FORMAT
                    };
                let mut vhdr = NAVideoInfo::new(width, height, false, format);
                vhdr.bits = depth as u8;
                if track.ver_m1 { // skip the rest for the beta version of QT
                    br.seek(SeekFrom::Start(start_pos + size))?;
                }
                //skip various common atoms
                while br.tell() - start_pos + 8 < size {
                    let mut buf = [0u8; 8];
                    br.peek_buf(&mut buf)?;
                    let tsize = read_u32be(&buf).unwrap() as usize;
                    let tag = &buf[4..8];
                    validate!(tsize >= 8);
                    match tag {
                        b"pasp" | b"clap" | b"gama" => {
                            br.read_skip(tsize)?;
                        },
                        _ => break,
                    };
                }
                let edata = if br.tell() < end_pos {
                        let edata_size  = br.read_u32be()? as usize;
                        let left = (end_pos - br.tell()) as usize;
                        if edata_size > 0 || left > 8 {
                            validate!(edata_size >= 4);
                            let mut buf = vec![0; edata_size - 4];
                                          br.read_buf(buf.as_mut_slice())?;
                            Some(buf)
                        } else {
                            None
                        }
                    } else {
                        None
                    };
                codec_info = NACodecInfo::new(cname, NACodecTypeInfo::Video(vhdr), edata);
            },
            StreamType::Audio if track.ver_m1 => {
                                      br.read_skip(8)?;
                let nchannels       = br.read_u16be()?;
                validate!(nchannels == 1 || nchannels == 2);
                let sample_size     = br.read_u16be()?;
                validate!(sample_size == 8 || sample_size == 16);
                                      br.read_u32be()?;
                let sample_rate     = br.read_u32be()? >> 16;
                let cname = if matches!(&fcc, &[0, 0, 0, 0] | b"raw " | b"twos") { "pcm" } else { "unknown" };
                let mut soniton = NASoniton::new(sample_size as u8, SONITON_FLAG_SIGNED | SONITON_FLAG_BE);
                if sample_size == 8 && &fcc != b"twos" {
                    soniton.signed = false;
                }
                let ahdr = NAAudioInfo::new(sample_rate, nchannels as u8, soniton, 1);
                let edata = parse_audio_edata(br, estart, esize)?;
                codec_info = NACodecInfo::new(cname, NACodecTypeInfo::Audio(ahdr), edata);
                track.pkt_demux.mode = if cname == "pcm" { PacketMode::RawAudio } else { PacketMode::AudioCBR };
                track.pkt_demux.bsize     = (sample_size / 8) as usize;
                track.pkt_demux.channels  = nchannels as usize;
                track.pkt_demux.bits      = sample_size as usize;
                if track.tb_den <= 1 {
                    track.tb_den = sample_rate;
                }
            },
            StreamType::Audio => {
                let sver            = br.read_u16be()?;
                let _revision       = br.read_u16le()?;
                let _vendor         = br.read_u32be()?;
                let mut nchannels   = br.read_u16be()?;
                if sver != 2 {
                    validate!(nchannels <= 64);
                }
                let sample_size     = br.read_u16be()?;
                validate!(sample_size <= 128);
                let compr_id        = br.read_u16be()?;
                let packet_size     = br.read_u16be()? as usize;
                validate!(packet_size == 0);
                let mut sample_rate = br.read_u32be()? >> 16;
                if sver != 2 {
                    validate!(sample_rate > 0);
                }
                let cname = if let Some(name) = find_codec_from_mov_audio_fourcc(&fcc) {
                        name
                    } else if let (true, Some(name)) = ((fcc[0] == b'm' && fcc[1] == b's'),  find_codec_from_wav_twocc(u16::from(fcc[2]) * 256 + u16::from(fcc[3]))) {
                        name
                    } else if fcc == [0; 4] {
                        "pcm"
                    } else {
                        "unknown"
                    };
                let mut soniton = NASoniton::new(sample_size as u8, SONITON_FLAG_SIGNED | SONITON_FLAG_BE);
                if &fcc == b"raw " && sample_size == 8 {
                    soniton.signed = false;
                }
                if &fcc == b"sowt" {
                    soniton.be = false;
                }
                let mut block_align = 1;
                match sver {
                    1 => {
                        let samples_per_packet  = br.read_u32be()?;
                        let _bytes_per_packet   = br.read_u32be()?;
                        let bytes_per_frame     = br.read_u32be()?;
                        let _bytes_per_sample   = br.read_u32be()?;
                        track.pkt_demux.bsize = bytes_per_frame as usize;
                        track.pkt_demux.frame_samples = samples_per_packet as usize;
                        track.tb_num = samples_per_packet.max(1);
                        block_align = bytes_per_frame as usize;
                    },
                    2 => {
                                                  br.read_u32be()?; // some size
                        let srate               = br.read_f64be()?;
                        validate!(srate > 1.0);
                        sample_rate = srate as u32;
                        let channels            = br.read_u32be()?;
                        validate!(channels > 0 && channels < 255);
                        nchannels = channels as u16;
                                                  br.read_u32be()?; // always 0x7F000000
                        let _bits_per_csample   = br.read_u32be()?;
                        let _codec_flags        = br.read_u32be()?;
                        let bytes_per_frame     = br.read_u32be()?;
                        let samples_per_packet  = br.read_u32be()?;
                        track.pkt_demux.bsize = bytes_per_frame as usize;
                        track.pkt_demux.frame_samples = samples_per_packet as usize;
                        track.tb_num = samples_per_packet;
                    },
                    _ => {
                        track.pkt_demux.bsize = (sample_size / 8) as usize;
                    },
                };
                if track.tb_den <= 1 {
                    track.tb_den = sample_rate;
                    track.pkt_demux.tb_den = sample_rate;
                }
                track.pkt_demux.mode = if matches!(&fcc,
                        b"NONE" | b"raw " | b"twos" | b"sowt" |
                        b"in24" | b"in32" | b"fl32" | b"fl64" |
                        /*b"ima4" |*/ b"ms\x00\x02" | b"ms\x00\x11" |
                        b"alaw" | b"ulaw" |
                        b"MAC3" | b"MAC6") {
                    PacketMode::RawAudio
                } else if sver == 2 || compr_id == 0xFFFE {
                    PacketMode::AudioVBR
                } else {
                    PacketMode::AudioCBR
                };
                let ahdr = NAAudioInfo::new(sample_rate, nchannels as u8, soniton, block_align);
                let edata = parse_audio_edata(br, estart, esize)?;
                codec_info = NACodecInfo::new(cname, NACodecTypeInfo::Audio(ahdr), edata);
                track.pkt_demux.channels  = nchannels as usize;
                track.pkt_demux.bits      = sample_size as usize;
            },
            StreamType::None => {
                return Err(DemuxerError::InvalidData);
            },
            _ => {
                validate!(br.tell() <= end_pos);
                let edata_size = (end_pos - br.tell()) as usize;
                let edata = if edata_size > 0 {
                        let mut buf = vec![0; edata_size];
                        br.read_buf(&mut buf)?;
                        Some(buf)
                    } else { None };

                const HANDLER_FLASH: u32 = mktag!(b"flsh");
                const HANDLER_GENERIC: u32 = mktag!(b"gnrc");
                const HANDLER_MUSIC: u32 = mktag!(b"musi");
                const HANDLER_OBJECT: u32 = mktag!(b"obje");
                const HANDLER_PANO: u32 = mktag!(b"pano");
                const HANDLER_PANO2: u32 = mktag!(b"STpn");
                const HANDLER_QDRAW: u32 = mktag!(b"Qdrw");
                const HANDLER_QTVR: u32 = mktag!(b"qtvr");
                const HANDLER_QUARTZ: u32 = mktag!(b"qtz ");
                const HANDLER_SKIN: u32 = mktag!(b"skin");
                const HANDLER_SPRITE: u32 = mktag!(b"sprt");
                const HANDLER_TEXT: u32 = mktag!(b"text");
                const HANDLER_TIME: u32 = mktag!(b"tmcd");
                const HANDLER_TWEEN: u32 = mktag!(b"twen");
                let name = match track.handler {
                        HANDLER_FLASH => "flash",
                        HANDLER_GENERIC if &fcc == b"@ppt" => "powerpoint",
                        HANDLER_GENERIC => "generic-data",
                        HANDLER_MUSIC => "qt-music",
                        HANDLER_OBJECT => "qt-object-media",
                        HANDLER_PANO => "panorama",
                        HANDLER_PANO2 => "panorama2",
                        HANDLER_QDRAW => "quickdraw-stream",
                        HANDLER_QTVR => "qtvr",
                        HANDLER_QUARTZ => "quartz",
                        HANDLER_SKIN => "apple-skin",
                        HANDLER_SPRITE => "qt-sprite",
                        HANDLER_TEXT => "apple-text",
                        HANDLER_TIME => "timecode",
                        HANDLER_TWEEN => "apple-tween",
                        _ => "unknown"
                    };
                codec_info = NACodecInfo::new(name, NACodecTypeInfo::None, edata);
            },
        };
        track.pkt_demux.tb_num = track.tb_num;
        track.pkt_demux.tb_den = track.tb_den;
        validate!(br.tell() <= end_pos);
        br.seek(SeekFrom::Start(end_pos))?;
        track.streams.push(NAStream::new(track.stream_type, track.track_no | ((desc_no as u32) << 16), codec_info, track.tb_num, track.tb_den, u64::from(track.pkt_demux.duration)));
    }
    let read_size = br.tell() - start_pos;
    track.stsd_found = true;
    Ok(read_size)
}

fn read_stts(track: &mut Track, br: &mut dyn ByteIO, size: u64) -> DemuxerResult<u64> {
    validate!(size >= 8);
    let start_pos = br.tell();
    let version             = br.read_byte()?;
    validate!(version == 0);
    let _flags              = br.read_u24be()?;
    let entries             = br.read_u32be()? as usize;
    validate!(entries as u64 <= (size - 8) / 8);
    if entries == 0 {
    } else if entries == 1 {
        let _count          = br.read_u32be()?;
        let tb_num          = br.read_u32be()?;
        validate!(tb_num != 0);
        track.rescale(tb_num);
    } else {
        track.pkt_demux.time_to_sample.clear();
        track.pkt_demux.time_to_sample.reserve(entries);
        for _ in 0..entries {
            let count       = br.read_u32be()?;
            let mult        = br.read_u32be()?;
            track.pkt_demux.time_to_sample.push((count, mult));
        }
    }
    let read_size = br.tell() - start_pos;
    validate!(read_size <= size);
    Ok(read_size)
}

fn read_stgs(track: &mut Track, br: &mut dyn ByteIO, size: u64) -> DemuxerResult<u64> {
    validate!(size >= 8);
    validate!(track.ver_m1);
    let start_pos = br.tell();
    let version             = br.read_byte()?;
    validate!(version == 0);
    let _flags              = br.read_u24be()?;
    let entries             = br.read_u32be()? as usize;
    validate!(entries as u64 <= (size - 8) / 8);
    if entries == 0 {
    } else if entries == 1 {
        let _count          = br.read_u32be()?;
        let tb_num          = br.read_u32be()?;
        validate!(tb_num != 0);
        track.rescale(tb_num);
    } else {
        track.pkt_demux.time_to_sample.clear();
        track.pkt_demux.time_to_sample.reserve(entries);
        for _ in 0..entries {
            let count       = br.read_u32be()?;
            let mult        = br.read_u32be()?;
            track.pkt_demux.time_to_sample.push((count, mult));
        }
    }
    let read_size = br.tell() - start_pos;
    validate!(read_size <= size);
    Ok(read_size)
}

fn read_stss(track: &mut Track, br: &mut dyn ByteIO, size: u64) -> DemuxerResult<u64> {
    let version             = br.read_byte()?;
    validate!(version == 0);
    let _flags              = br.read_u24be()?;
    let entries             = br.read_u32be()? as usize;
    validate!(entries < ((u32::MAX >> 2) - 8) as usize);
    validate!((entries * 4 + 8) as u64 == size);
    track.pkt_demux.keyframes = Vec::with_capacity(entries);
    let mut last_chunk_no = 0;
    for _ in 0..entries {
        let chunk_no        = br.read_u32be()?;
        validate!(chunk_no > last_chunk_no);
        track.pkt_demux.keyframes.push(chunk_no);
        last_chunk_no = chunk_no;
    }
    track.stss_found = true;
    Ok(size)
}

fn read_stsc(track: &mut Track, br: &mut dyn ByteIO, size: u64) -> DemuxerResult<u64> {
    let version             = br.read_byte()?;
    validate!(version == 0);
    let _flags              = br.read_u24be()?;
    let entries             = br.read_u32be()? as usize;
    let mut last_sd = 0;
    if track.ver_m1 {
        validate!(entries < ((u32::MAX / 16) - 8) as usize);
        validate!((entries * 16 + 8) as u64 == size);
        for _i in 0..entries {
                              br.read_u32be()?;
            let chunk_no    = br.read_u32be()?;
            let nsamples    = br.read_u32be()?;
            let sampledesc  = br.read_u32be()?;
            track.pkt_demux.sample_map.push((chunk_no, nsamples));
            if sampledesc != last_sd {
                track.pkt_demux.desc_map.push((chunk_no, sampledesc));
                last_sd = sampledesc;
            }
        }
        return Ok(size);
    }
    validate!(entries < ((u32::MAX / 12) - 8) as usize);
    validate!((entries * 12 + 8) as u64 == size);
    track.pkt_demux.sample_map = Vec::with_capacity(entries);
    let mut last_chunk_no = 0;
    for _i in 0..entries {
        let mut chunk_no    = br.read_u32be()?;
        // QT2.0 bug?
        if chunk_no == last_chunk_no && last_chunk_no == 5 {
            println!("slightly buggy QT2.0 sample?");
            chunk_no = 6;
        }
        validate!(chunk_no > last_chunk_no);
        let nsamples        = br.read_u32be()?;
        let sample_desc     = br.read_u32be()?;
        track.pkt_demux.sample_map.push((chunk_no, nsamples));
        last_chunk_no = chunk_no;
        if sample_desc != last_sd {
            track.pkt_demux.desc_map.push((chunk_no, sample_desc));
            last_sd = sample_desc;
        }
    }
    Ok(size)
}

fn read_stsz(track: &mut Track, br: &mut dyn ByteIO, size: u64) -> DemuxerResult<u64> {
    let version             = br.read_byte()?;
    validate!(version == 0);
    let _flags              = br.read_u24be()?;
    let sample_size         = br.read_u32be()?;
    if sample_size != 0 {
        track.pkt_demux.sample_size = sample_size;
        if track.pkt_demux.sample_size != 1 || track.pkt_demux.bsize == 0 {
            track.pkt_demux.bsize = sample_size as usize;
        }
        Ok(8)
    } else {
        let entries             = br.read_u32be()? as usize;
        validate!((entries * 4 + 12) as u64 == size);
        track.pkt_demux.chunk_sizes = vec![0; entries];
                                  br.read_u32be_arr(&mut track.pkt_demux.chunk_sizes)?;
        Ok(size)
    }
}

fn read_stco(track: &mut Track, br: &mut dyn ByteIO, size: u64) -> DemuxerResult<u64> {
    let version             = br.read_byte()?;
    validate!(version == 0);
    let _flags              = br.read_u24be()?;
    let entries             = br.read_u32be()? as usize;
    validate!((entries * 4 + 8) as u64 == size);
    if entries < 1024 {
        track.pkt_demux.chunk_offsets = Vec::with_capacity(entries);
        for _i in 0..entries {
            let sample_offset   = br.read_u32be()?;
            track.pkt_demux.chunk_offsets.push(u64::from(sample_offset));
        }
    } else {
        // reading whole array at once and converting it later is much faster
        track.pkt_demux.chunk_offsets = vec![0; entries];
        let mut tmp = vec![0u32; entries];
        unsafe {
            let data = tmp.as_mut_ptr();
            let ptr = std::slice::from_raw_parts_mut(data as *mut u8, entries * 4);
            br.read_buf(ptr)?;
        }
        for (dst, &src) in track.pkt_demux.chunk_offsets.iter_mut().zip(tmp.iter()) {
            *dst = u64::from(u32::from_be(src));
        }
    }
    Ok(size)
}

fn read_co64(track: &mut Track, br: &mut dyn ByteIO, size: u64) -> DemuxerResult<u64> {
    let version             = br.read_byte()?;
    validate!(version == 0);
    let _flags              = br.read_u24be()?;
    let entries             = br.read_u32be()? as usize;
    validate!(entries > 0 && (entries * 8 + 8) as u64 == size);
    track.pkt_demux.chunk_offsets = vec![0; entries];
                              br.read_u64be_arr(&mut track.pkt_demux.chunk_offsets)?;
    Ok(size)
}

fn read_ctts(track: &mut Track, br: &mut dyn ByteIO, size: u64) -> DemuxerResult<u64> {
    validate!(size >= 8);
    let version             = br.read_byte()?;
    let _flags              = br.read_u24be()?;
    if version > 1 {
        return Err(DemuxerError::NotImplemented);
    }
    let entries             = br.read_u32be()? as usize;
    track.pkt_demux.ctts_version = version;
    track.pkt_demux.ctts_map.resize(entries);
    match version {
        0 | 1 => {
            validate!(size == (entries as u64) * 8 + 8);
            if entries < 1024 {
                for _ in 0..entries {
                    let samp_count  = br.read_u32be()?;
                    let samp_offset = br.read_u32be()?;
                    track.pkt_demux.ctts_map.add(samp_count, samp_offset / track.tb_div);
                }
            } else {
                // reading whole array at once and converting it later is much faster
                track.pkt_demux.ctts_map.array.resize(entries, RLEPair(0, 0));
                unsafe {
                    let data = track.pkt_demux.ctts_map.array.as_mut_ptr();
                    let ptr = std::slice::from_raw_parts_mut(data as *mut u8, entries * 8);
                    br.read_buf(ptr)?;
                }
                for RLEPair(count, offset) in track.pkt_demux.ctts_map.array.iter_mut() {
                    *count = u32::from_be(*count);
                    *offset = u32::from_be(*offset) / track.tb_div;
                }
            }
        },
        _ => unreachable!(),
    };
    track.pkt_demux.ctts_map.reset();

    Ok(size)
}

const TRAF_CHUNK_HANDLERS: &[TrackChunkHandler] = &[
    TrackChunkHandler { ctype: mktag!(b"tfhd"), parse: read_tfhd },
    TrackChunkHandler { ctype: mktag!(b"trun"), parse: read_trun },
    TrackChunkHandler { ctype: mktag!(b"sbgp"), parse: skip_chunk },
    TrackChunkHandler { ctype: mktag!(b"sgpd"), parse: skip_chunk },
    TrackChunkHandler { ctype: mktag!(b"subs"), parse: skip_chunk },
    TrackChunkHandler { ctype: mktag!(b"saiz"), parse: skip_chunk },
    TrackChunkHandler { ctype: mktag!(b"saio"), parse: skip_chunk },
    TrackChunkHandler { ctype: mktag!(b"tfdt"), parse: skip_chunk },
    TrackChunkHandler { ctype: mktag!(b"meta"), parse: skip_chunk },
];

fn read_tfhd(track: &mut Track, br: &mut dyn ByteIO, size: u64) -> DemuxerResult<u64> {
    validate!(size >= 8);
    let start = br.tell();
    let _version            = br.read_byte()?;
    let flags               = br.read_u24be()?;
    let _track_id           = br.read_u32be()?;
    if (flags & 0x000001) != 0 {
        let base_offset     = br.read_u64be()?;
        track.moof_off = base_offset;
    }
    if (flags & 0x000002) != 0 {
        let _sample_description_index = br.read_u32be()?;
    }
    if (flags & 0x000008) != 0 {
        let default_sample_duration = br.read_u32be()?;
        if track.tb_div == 1 {
            track.rescale(default_sample_duration);
        }
    }
    if (flags & 0x000010) != 0 {
        let _default_sample_size = br.read_u32be()?;
    }
    if (flags & 0x000020) != 0 {
        let _default_sample_flags = br.read_u32be()?;
    }
    /*if (flags & 0x010000) != 0 { // duration is empty
    }*/
    /*if (flags & 0x020000) != 0 { // base offset is moof start
    }*/
    Ok(br.tell() - start)
}

fn read_trun(track: &mut Track, br: &mut dyn ByteIO, size: u64) -> DemuxerResult<u64> {
    validate!(size >= 8);
    let version             = br.read_byte()?;
    let flags               = br.read_u24be()?;
    let data_off_present        = (flags & 0x000001) != 0;
    let first_sample_flags      = (flags & 0x000004) != 0;
    let sample_duration_present = (flags & 0x000100) != 0;
    let sample_size_present     = (flags & 0x000200) != 0;
    let sample_flags_present    = (flags & 0x000400) != 0;
    let sample_ct_off_present   = (flags & 0x000800) != 0;

    let sample_count            = br.read_u32be()? as usize;

    let mut hdr_size = 8;
    let mut arr_size = 0;
    if data_off_present {
        hdr_size += 4;
    }
    if first_sample_flags {
        hdr_size += 4;
    }
    if sample_duration_present {
        arr_size += 4;
    }
    if sample_size_present {
        arr_size += 4;
    }
    if sample_flags_present {
        arr_size += 4;
    }
    if sample_ct_off_present {
        arr_size += 4;
    }
    validate!(size == hdr_size + arr_size * (sample_count as u64));

    let mut data_off = if data_off_present {
            let off             = br.read_u32be()? as i32;
            let new_off = (track.moof_off as i64) + i64::from(off);
            validate!(new_off > 0);
            new_off as u64
        } else {
            track.moof_off
        };
    if first_sample_flags {
        let _flags              = br.read_u32be()?;
    }

    if sample_size_present {
        track.pkt_demux.chunk_sizes.reserve(sample_count);
        track.pkt_demux.chunk_offsets.reserve(sample_count);
    }

    if sample_ct_off_present {
        if track.pkt_demux.ctts_version != version {
            track.pkt_demux.ctts_version = version;
        }
        track.pkt_demux.ctts_map.reserve(sample_count);
    }

    if track.pkt_demux.chunk_offsets.len() < (u32::MAX as usize) {
        track.pkt_demux.keyframes.push((track.pkt_demux.chunk_offsets.len() + 1) as u32);
    }
    for _ in 0..sample_count {
        if sample_duration_present {
            let _duration       = br.read_u32be()?;
        }
        if sample_size_present {
            let ssize           = br.read_u32be()?;
            track.pkt_demux.chunk_sizes.push(ssize);
            track.pkt_demux.chunk_offsets.push(data_off);
            data_off += u64::from(ssize);
        }
        if sample_flags_present {
            let _flags          = br.read_u32be()?;
        }
        if sample_ct_off_present {
            let samp_offset     = br.read_u32be()?;
            if version == 0 {
                track.pkt_demux.ctts_map.add(1, samp_offset / track.tb_div);
            } else {
                track.pkt_demux.ctts_map.add(1, ((samp_offset as i32) / (track.tb_div as i32)) as u32);
            }
        }
    }
    track.pkt_demux.ctts_map.reset();

    Ok(size)
}

pub struct Track {
    pub ver_m1:         bool,
    pub track_id:       u32,
    pub track_str_ids:  Vec<usize>,
    pub track_no:       u32,
    pub tb_num:         u32,
    pub tb_den:         u32,
    pub tb_div:         u32,
    pub depth:          u8,
    pub tkhd_found:     bool,
    pub stsd_found:     bool,
    pub stss_found:     bool,
    pub stream_type:    StreamType,
    pub width:          usize,
    pub height:         usize,
    pub streams:        Vec<NAStream>,
    pub pal:            Vec<Option<Arc<[u8; 1024]>>>,
    pub pkt_demux:      QTPacketDemuxer,
    pub handler:        u32,

    pub moof_off:       u64,

    pub print_chunks:   bool,
}

impl Track {
    pub fn new(track_no: u32, tb_den: u32) -> Self {
        Self {
            tkhd_found:     false,
            stsd_found:     false,
            stss_found:     false,
            track_id:       0,
            track_str_ids:  Vec::new(),
            track_no,
            tb_num: 1,
            tb_den,
            tb_div:         1,
            stream_type:    StreamType::None,
            width:          0,
            height:         0,
            streams:        Vec::new(),
            handler:        0,
            depth:          0,
            pal:            Vec::with_capacity(1),
            ver_m1:         false,
            pkt_demux:      QTPacketDemuxer::new(),

            moof_off:       0,

            print_chunks:   false,
        }
    }
    read_chunk_list!(track; "trak", read_trak, TRAK_CHUNK_HANDLERS);
    read_chunk_list!(track; "mdia", read_mdia, MDIA_CHUNK_HANDLERS);
    read_chunk_list!(track; "minf", read_minf, MINF_CHUNK_HANDLERS);
    read_chunk_list!(track; "stbl", read_stbl, STBL_CHUNK_HANDLERS);
    read_chunk_list!(track; "traf", read_traf, TRAF_CHUNK_HANDLERS);
    pub fn call_read_trak(&mut self, br: &mut dyn ByteIO, size: u64) -> DemuxerResult<()> {
        self.read_trak(br, size)
    }
    pub fn call_read_traf(&mut self, br: &mut dyn ByteIO, size: u64) -> DemuxerResult<()> {
        self.read_traf(br, size)
    }
    fn rescale(&mut self, tb_num: u32) {
        self.tb_div = tb_num;
        for stream in self.streams.iter_mut() {
            let tb_den = stream.tb_den;
            let (tb_num, tb_den) = reduce_timebase(tb_num * stream.tb_num, tb_den);
            stream.duration /= u64::from(self.tb_div);
            stream.tb_num = tb_num;
            stream.tb_den = tb_den;
            self.tb_num = tb_num;
            self.tb_den = tb_den;
            self.pkt_demux.tb_num = tb_num;
            self.pkt_demux.tb_den = tb_den;
            self.pkt_demux.duration /= self.tb_div;
        }
    }
}

#[allow(clippy::too_many_arguments)]
pub fn process_packet(src: &mut dyn ByteIO, strmgr: &StreamManager, track: &mut Track, ts: NATimeInfo, offset: u64, size: usize, first: bool, is_kf: bool) -> DemuxerResult<NAPacket> {
    let cur_desc = if track.track_str_ids.len() > 1 {
            let cur_chunk_no = if track.pkt_demux.mode == PacketMode::OneToOne {
                    track.pkt_demux.cur_sample
                } else {
                    track.pkt_demux.cur_chunk
                };
            let mut cur_desc = 1;
            for &(chunk_no, desc_no) in track.pkt_demux.desc_map.iter() {
                if chunk_no as usize <= cur_chunk_no {
                    cur_desc = desc_no;
                }
                if chunk_no as usize >= cur_chunk_no {
                    break;
                }
            }
            cur_desc as usize - 1
        } else { 0 };
    if let Some(cpts) = ts.get_pts() {
        let cts = NATimeInfo::rescale_ts(cpts, ts.tb_num, ts.tb_den, 1, 1000);
        track.pkt_demux.cur_ts = Some(cts);
    } else {
        track.pkt_demux.cur_ts = None;
    }
    let str_idx = if cur_desc < track.track_str_ids.len() { cur_desc } else { 0 };
    let stream = strmgr.get_stream(track.track_str_ids[str_idx]);
    if stream.is_none() { return Err(DemuxerError::InvalidData); }
    let stream = stream.unwrap();
    src.seek(SeekFrom::Start(offset))?;
    let mut pkt = src.read_packet(stream, ts, first | is_kf, size)?;
    if let Some(ref pal) = track.pal[cur_desc] {
        let side_data = NASideData::Palette(first, pal.clone());
        pkt.add_side_data(side_data);
    }
    Ok(pkt)
}
