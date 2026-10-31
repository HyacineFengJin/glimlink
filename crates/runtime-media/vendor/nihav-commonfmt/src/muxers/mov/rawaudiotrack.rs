use nihav_core::muxers::*;

use super::*;

pub fn is_raw_audio(cname: &str) -> bool {
    matches!(cname,
                    "pcm" |
                    "mace-3" | "mace-6" |
                    "ima-adpcm-qt" |
                    "ulaw" | "alaw" |
                    "ms\x00\x02" | "ms\x00\x11")
}

pub struct RawAudioTrackHandler {
    fcc:        [u8; 4],
    rate:       u32,
    bits:       usize,
    channels:   usize,
    sample:     u64,
}

impl RawAudioTrackHandler {
    pub fn new(strm: NAStreamRef) -> MuxerResult<Self> {
        let cname = strm.get_info().get_name();
        let ainfo = strm.get_info().get_properties().get_audio_info().ok_or(MuxerError::UnsupportedFormat)?;
        let fcc = match cname {
                "pcm" => {
                    match ainfo.format {
                        NASoniton { bits: 8, be: _, packed: _, planar: _, float: false, signed: false } => *b"raw ",
                        NASoniton { bits: 8, be: _, packed: _, planar: _, float: false, signed: true } => *b"twos",
                        NASoniton { bits: 16, be: true, packed: _, planar: _, float: false, signed: true } => *b"twos",
                        NASoniton { bits: 16, be: false, packed: _, planar: _, float: false, signed: true } => *b"sowt",
                        NASoniton { bits: 24, be: true, packed: _, planar: _, float: false, signed: true } => *b"in24",
                        NASoniton { bits: 32, be: true, packed: _, planar: _, float: false, signed: true } => *b"in32",
                        NASoniton { bits: 32, be: true, packed: _, planar: _, float: true, signed: _ } => *b"fl32",
                        NASoniton { bits: 64, be: true, packed: _, planar: _, float: true, signed: _ } => *b"fl64",
                        _ => return Err(MuxerError::UnsupportedFormat),
                    }
                },
                "mace-3" => *b"MAC3",
                "mace-6" => *b"MAC6",
                "ima-adpcm-qt" => *b"ima4",
                "alaw" => *b"alaw",
                "ulaw" => *b"ulaw",

                "ms-adpcm" => return Err(MuxerError::UnsupportedFormat), //*b"ms\x00\x02",
                "ima-adpcm-ms" => *b"ms\x00\x11",
                _ => return Err(MuxerError::UnsupportedFormat),
            };
        let rate = ainfo.sample_rate;
        let bits = usize::from(ainfo.format.bits);
        let channels = usize::from(ainfo.channels);
        Ok(Self { fcc, rate, bits, channels, sample: 0 })
    }

    fn calculate_chunk_samples(&self, size: usize) -> usize {
        match &self.fcc {
            b"NONE" | b"raw " | b"twos" | b"sowt" | &[0, 0, 0, 0] => {
                size * 8 / (self.bits * self.channels)
            },
            b"ima4" => {
                let nblocks = size / (34 * self.channels);
                nblocks * 64
            },
            b"MAC3" => {
                size * 3 / self.channels
            },
            b"MAC6" => {
                size * 6 / self.channels
            },
            b"in24" => size / (3 * self.channels),
            b"in32" | b"fl32" => size / (4 * self.channels),
            b"fl64" => size / (8 * self.channels),
            b"ulaw" | b"alaw" => size,
            /*b"ms\x00\x02" => { //MS ADPCM
                size / block_size * ((block_size / self.channels - 7) * 2 + 2)
            },*/
            b"ms\x00\x11" => { //IMA ADPCM
                (size / self.channels - 4) * 2 + 1
            },
            _ => unreachable!(),
        }
    }
}

impl TrackHandler for RawAudioTrackHandler {
    fn write_chunk(&mut self, bw: &mut dyn ByteIO, ca: &mut ChunkAccount, pkt: NAPacket) -> MuxerResult<()> {
        let src = pkt.get_buffer();

        let chunk_samples = self.calculate_chunk_samples(src.len());

        let offset = bw.tell();
        ca.add_offset(offset);
        ca.add_chunk_samps(chunk_samples);
        self.sample += chunk_samples as u64;
        bw.write_buf(&src)?;
        Ok(())
    }
    fn flush_chunks(&mut self, _bw: &mut dyn ByteIO, ca: &mut ChunkAccount) -> MuxerResult<()> {
        ca.add_pts(self.sample);
        Ok(())
    }
    fn get_time_info(&self) -> (u64, u32) { (self.sample, self.rate) }
    fn get_dimensions(&self) -> (usize, usize) { (0, 0) }
    fn get_pkt_duration(&self) -> u64 { 1 }
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
        bw.write_u16be(0)?; // version
        bw.write_u16be(0)?; // revision
        bw.write_u32be(0)?; // vendor
        bw.write_u16be(self.channels as u16)?;
        bw.write_u16be(self.bits as u16)?;
        bw.write_u16be(0)?; // compression ID
        bw.write_u16be(0)?; // packet size
        bw.write_u32be(self.rate << 16)?;
        codec.end(bw)?;

        Ok(())
    }
}
