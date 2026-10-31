use nihav_core::muxers::*;
use nihav_registry::register::*;

use super::*;

pub struct AudioTrackHandler {
    fcc:        [u8; 4],
    ainfo:      NAAudioInfo,
    edata:      Option<Arc<Vec<u8>>>,
    pts:        u64,
    last_ts:    u64,
    frameno:    u32,
    frm_sz:     usize,
}

impl AudioTrackHandler {
    pub fn new(strm: NAStreamRef, ftag: Option<[u8; 4]>) -> MuxerResult<Self> {
        let cname = strm.get_info().get_name();
        let ainfo = strm.get_info().get_properties().get_audio_info().ok_or(MuxerError::UnsupportedFormat)?;
        let fcc = if let Some(tag) = ftag {
                tag
            } else if let Some(fcc) = find_mov_audio_fourcc(cname) {
                fcc
            } else if let Some(tcc) = find_wav_twocc(cname) {
                [b'm', b's', (tcc >> 8) as u8, tcc as u8]
            } else {
                return Err(MuxerError::UnsupportedFormat);
            };
        let edata = strm.get_info().get_extradata();
        Ok(Self{
            fcc, ainfo, edata,
            pts:        0,
            last_ts:    0,
            frameno:    1,
            frm_sz:     0,
        })
    }
}

impl TrackHandler for AudioTrackHandler {
    fn write_chunk(&mut self, bw: &mut dyn ByteIO, ca: &mut ChunkAccount, pkt: NAPacket) -> MuxerResult<()> {
        let src_pts = pkt.ts.pts.ok_or(MuxerError::InvalidData)?;
        let src = pkt.get_buffer();
        if self.frameno == 1 {
            self.frm_sz = src.len();
        }
        if self.frm_sz != src.len() {
            self.frm_sz = 0;
        }
        let offset = bw.tell();
        let pts = NATimeInfo::rescale_ts(src_pts, pkt.ts.tb_num, pkt.ts.tb_den, 1, self.ainfo.sample_rate);
        ca.add_pts(pts);
        ca.add_offset(offset);
        ca.add_size(src.len());
        if pkt.is_keyframe() {
            ca.add_keyframe(self.frameno);
        }
        self.frameno += 1;
        self.pts = pts + NATimeInfo::rescale_ts(pkt.ts.duration.unwrap_or(1), pkt.ts.tb_num, pkt.ts.tb_den, 1, self.ainfo.sample_rate);
        self.last_ts = pts;
        bw.write_buf(&src)?;
        Ok(())
    }
    fn flush_chunks(&mut self, _bw: &mut dyn ByteIO, _ca: &mut ChunkAccount) -> MuxerResult<()> { Ok(()) }
    fn get_time_info(&self) -> (u64, u32) { (self.pts, self.ainfo.sample_rate) }
    fn get_dimensions(&self) -> (usize, usize) { (0, 0) }
    fn get_pkt_duration(&self) -> u64 {
        if self.frm_sz != 0 && self.frameno > 2 {
            self.last_ts / u64::from(self.frameno - 2)
        } else {
            1
        }
    }
    fn write_media_info(&mut self, bw: &mut dyn ByteIO) -> MuxerResult<()> {
        let smhd = AtomMarker::start(bw, b"smhd")?;
        bw.write_byte(0)?;  // version
        bw.write_u24be(0)?; // flags
        bw.write_u16be(0)?; // balance
        bw.write_u16be(0)?; // reserved
        smhd.end(bw)?;
        Ok(())
    }
    fn write_descriptor(&mut self, bw: &mut dyn ByteIO) -> MuxerResult<()> {
        let codec = AtomMarker::start(bw, &self.fcc)?;
        bw.write_buf(&[0; 6])?;
        bw.write_u16be(1)?; // data reference index
        bw.write_u16be(1)?; // version
        bw.write_u16be(0)?; // revision
        bw.write_u32be(0)?; // vendor
        bw.write_u16be(u16::from(self.ainfo.channels))?;
        bw.write_u16be(u16::from(self.ainfo.format.bits))?;
        bw.write_u16be(if self.frm_sz != 0 { 0 } else { 0xFFFE })?; // compression ID
        bw.write_u16be(0)?; // packet size
        bw.write_u32be(self.ainfo.sample_rate << 16)?;
        if self.frm_sz != 0 { // CBR audio
            let samples_per_pkt = if self.frameno > 2 {
                    self.last_ts / u64::from(self.frameno - 2)
                } else { 0 };
            bw.write_u32be(samples_per_pkt as u32)?; // samples per packet
            bw.write_u32be(self.frm_sz as u32 / u32::from(self.ainfo.channels))?; // bytes per packet
            bw.write_u32be(self.frm_sz as u32)?; // bytes per frame
            bw.write_u32be(2)?; // bytes per sample
        } else {
            bw.write_u32be(0)?; // samples per packet
            bw.write_u32be(0)?; // bytes per packet
            bw.write_u32be(0)?; // bytes per frame
            bw.write_u32be(0)?; // bytes per sample
        }
        if let Some(edata) = &self.edata {
            bw.write_u32be(edata.len() as u32 + 20)?;
            bw.write_buf(b"wave")?;
            bw.write_u32be(12)?;
            bw.write_buf(b"frma")?;
            bw.write_buf(&self.fcc)?;
            bw.write_buf(edata)?;
        }
        codec.end(bw)?;
        Ok(())
    }
}
