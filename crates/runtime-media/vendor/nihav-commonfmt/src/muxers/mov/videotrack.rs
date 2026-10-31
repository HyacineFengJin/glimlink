use nihav_core::muxers::*;
use nihav_registry::register::*;
use nihav_codec_support::codecs::qt_pal::*;

use super::*;

pub struct VideoTrackHandler {
    fcc:        [u8; 4],
    vinfo:      NAVideoInfo,
    frameno:    u32,
    tb_num:     u32,
    tb_den:     u32,
    pts:        u64,
    edata:      Option<Arc<Vec<u8>>>,
    pal:        Option<Arc<[u8; 1024]>>,
}

impl VideoTrackHandler {
    pub fn new(strm: NAStreamRef, ftag: Option<[u8; 4]>) -> MuxerResult<Self> {
        let cname = strm.get_info().get_name();
        let fcc = if let Some(tag) = ftag { tag } else { find_mov_video_fourcc(cname).ok_or(MuxerError::UnsupportedFormat)? };
        let vinfo = strm.get_info().get_properties().get_video_info().ok_or(MuxerError::UnsupportedFormat)?;
        if cname == "rawvideo" {
            match vinfo.format.to_short_string().ok_or(MuxerError::UnsupportedFormat)?.as_str() {
                "pal8" | "rgb24" | "rgb555be" => {},
                other => {
                    println!("Raw format {other} is not supported!");
                    return Err(MuxerError::UnsupportedFormat)?;
                }
            }
        }
        let (tb_num, tb_den) = strm.get_timebase();
        let edata = strm.get_info().get_extradata();

        Ok(Self{
            fcc, vinfo, tb_num, tb_den,
            frameno:    1,
            pts:        0,
            edata,
            pal:        None,
        })
    }
}

impl TrackHandler for VideoTrackHandler {
    fn write_chunk(&mut self, bw: &mut dyn ByteIO, ca: &mut ChunkAccount, pkt: NAPacket) -> MuxerResult<()> {
        let pts = pkt.ts.pts.ok_or(MuxerError::InvalidData)? * u64::from(self.tb_num);
        let src = pkt.get_buffer();
        let offset = bw.tell();
        ca.add_pts(pts);
        ca.add_offset(offset);
        ca.add_size(src.len());
        if pkt.is_keyframe() {
            ca.add_keyframe(self.frameno);
        }
        if self.pal.is_none() {
            for sd in pkt.side_data.iter() {
                if let NASideData::Palette(_, pal) = sd {
                    self.pal = Some(Arc::clone(pal));
                }
            }
        }
        self.frameno += 1;
        self.pts = pts + pkt.ts.duration.unwrap_or(1) * u64::from(self.tb_num);
        bw.write_buf(&src)?;
        Ok(())
    }
    fn flush_chunks(&mut self, _bw: &mut dyn ByteIO, _ca: &mut ChunkAccount) -> MuxerResult<()> { Ok(()) }
    fn get_time_info(&self) -> (u64, u32) { (self.pts, self.tb_den) }
    fn get_dimensions(&self) -> (usize, usize) {
        (self.vinfo.width, self.vinfo.height)
    }
    fn get_pkt_duration(&self) -> u64 { 1 }
    fn write_media_info(&mut self, bw: &mut dyn ByteIO) -> MuxerResult<()> {
        let vmhd = AtomMarker::start(bw, b"vmhd")?;
        bw.write_byte(0)?;  // version
        bw.write_u24be(1)?; // flags
        bw.write_u16be(0x40)?; // graphics mode - dither copy
        bw.write_u16be_arr(&[0x8000; 3])?; // opcolor
        vmhd.end(bw)?;
        Ok(())
    }
    fn write_descriptor(&mut self, bw: &mut dyn ByteIO) -> MuxerResult<()> {
        let codec = AtomMarker::start(bw, &self.fcc)?;
        bw.write_buf(&[0; 6])?;
        bw.write_u16be(1)?; // data reference index
        bw.write_u16be(1)?; // version
        bw.write_u16be(1)?; // revision
        bw.write_buf(b"appl")?; // vendor
        bw.write_u32be(0)?; // temporal quality
        bw.write_u32be(0)?; // spatial quality
        bw.write_u16be(self.vinfo.width as u16)?;
        bw.write_u16be(self.vinfo.height as u16)?;
        bw.write_u32be(72 << 16)?; // horizontal resolution
        bw.write_u32be(72 << 16)?; // vertical resolution
        bw.write_u32be(0)?; // data size
        bw.write_u16be(1)?; // frame count
        let name: &[u8] = match &self.fcc {
                b"raw " | b"j420" => b"Raw video",
                b"8BPS" => b"Planar RGB",
                b"jpeg" => b"Motion JPEG",
                b"mjpa" => b"Motion JPEG A",
                b"mjpb" => b"Motion JPEG B",
                b"cvid" => b"Cinepak",
                b"smc " => b"Apple Graphics",
                b"rpza" => b"Apple Video",
                b"rle " => b"Apple Animation",
                b"qdrw" => b"QuickDraw",
                b"SVQ1" | b"svqi" => b"Sorenson Video",
                b"SVQ3" | b"svq3" => b"Sorenson Video 3",
                b"rt21" => b"Indeo 2",
                b"IV31" | b"IV32" | b"iv31" | b"iv32" => b"Intel Indeo Video R3",
                b"MPAK" => b"MoviePak",
                b"pgvv" => b"Radius Studio",
                b"UCOD" => b"Clear Video",
                b"tmot" => b"TrueMotion-S",
                b"VP30" | b"VP31" => b"Truemotion VP3",
                b"arbc" => b"Gryphon ARBC Animation",
                b"ESCP" => b"Eidos Escape",
                b"kpcd" => b"Kodak Photo CD",
                b"yuv2" => b"Component Video",
                b"yuv4" => b"libquicktime component video",
                _ => b"Some unknown codec",
            };
        bw.write_pas_str(name)?;
        bw.write_buf(&[0; 32][..32 - name.len() - 1])?;
        let mut bpp = match self.vinfo.format.get_total_depth() {
                15 | 16 => 16,
                other_depth => other_depth
            };
        if self.vinfo.bits == 15 || self.vinfo.bits == 16 {
            bpp = 16;
        }
        if self.vinfo.format.is_paletted() { //xxx: handle 1-/2-/4-bit palettes too
            bpp = 8;
        }
        if &self.fcc == b"j420" {
            bpp = 12;
        }
        if bpp == 8 && self.pal.is_some() {
            bpp = self.vinfo.bits.min(8);
        }
        //xxx: detect greyscale?
        bw.write_u16be(u16::from(bpp))?;
        if bpp <= 8 {
            if let Some(ref pal) = self.pal {
                let pal_bpp = self.vinfo.bits.min(8);
                static DEFAULT_PALS: &[(u8, &[u8])] = &[(8, &MOV_DEFAULT_PAL_8BIT), (4, &MOV_DEFAULT_PAL_4BIT), (2, &MOV_DEFAULT_PAL_2BIT)];
                let mut def_pal = false;
                for &(dp_bits, dpal) in DEFAULT_PALS.iter() {
                    if pal_bpp == dp_bits && &pal[..4 << pal_bpp] == dpal {
                        def_pal = true;
                        break;
                    }
                }
                if def_pal {
                    bw.write_u16be(0xFFFF)?; // default color table ID
                } else {
                    bw.write_u16be(0)?; // color table ID
                    bw.write_u32be(0)?; // color table seed
                    bw.write_u16be(0x8000)?; // color table flags
                    bw.write_u16be((1 << pal_bpp) - 1)?; // color table size
                    for clr in pal.chunks_exact(4).take(1 << pal_bpp) {
                        bw.write_buf(&[0x00, 0x00, clr[0], clr[0], clr[1], clr[1], clr[2], clr[2]])?; // 16-bit ARGB
                    }
                }
            } else {
                bw.write_u16be(0xFFFF)?; // default color table ID
            }
        } else {
            bw.write_u16be(0xFFFF)?; // color table ID
        }
        if let Some(edata) = &self.edata {
            bw.write_u32be(edata.len() as u32 + 4)?;
            bw.write_buf(edata)?;
        }
        codec.end(bw)?;
        Ok(())
    }
}
