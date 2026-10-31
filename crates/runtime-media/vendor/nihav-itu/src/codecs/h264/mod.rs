/*
 known bugs and limitations:
  * wrong slice boundary filtering
  * not fully correct deblock strength selection for P/B-macroblocks
  * scaling lists for 4x4 blocks
*/
use std::sync::{Arc, RwLock};

use nihav_core::codecs::*;
use nihav_core::io::byteio::*;
use nihav_core::io::bitreader::*;
use nihav_core::io::intcode::*;
use nihav_codec_support::codecs::{MV, ZERO_MV};

mod baseline;
mod high;

mod cabac_coder;
mod common_types;
use common_types::*;
mod sets;
mod slice;
use slice::*;

pub type Shareable<T> = Arc<RwLock<T>>;

trait ReadUE {
    fn read_ue(&mut self) -> DecoderResult<u32>;
    fn read_te(&mut self, range: u32) -> DecoderResult<u32>;
    fn read_ue_lim(&mut self, max_val: u32) -> DecoderResult<u32> {
        let val = self.read_ue()?;
        validate!(val <= max_val);
        Ok(val)
    }
    fn read_se(&mut self) -> DecoderResult<i32> {
        let val = self.read_ue()?;
        if (val & 1) != 0 {
            Ok (((val >> 1) as i32) + 1)
        } else {
            Ok (-((val >> 1) as i32))
        }
    }
}

impl<'a> ReadUE for BitReader<'a> {
    fn read_ue(&mut self) -> DecoderResult<u32> {
        Ok(self.read_code(UintCodeType::GammaP)? - 1)
    }
    fn read_te(&mut self, range: u32) -> DecoderResult<u32> {
        if range == 1 {
            if self.read_bool()? {
                Ok(0)
            } else {
                Ok(1)
            }
        } else {
            let val = self.read_ue()?;
            validate!(val <= range);
            Ok(val)
        }
    }
}

#[derive(Clone,Copy,Default)]
pub struct CurrentMBInfo {
    pub mb_type:        MBType,
    pub sub_mb_type:    [SubMBType; 4],
    pub ipred:          [IntraPredMode; 16],
    pub chroma_ipred:   u8,
    pub mv_l0:          [MV; 16],
    pub ref_l0:         [PicRef; 4],
    pub mv_l1:          [MV; 16],
    pub ref_l1:         [PicRef; 4],
    pub qp_y:           u8,
    pub cbpy:           u8,
    pub cbpc:           u8,
    pub coeffs:         [[i16; 16]; 25],
    pub coeffs8x8:      [Coeff8x8; 4],
    pub chroma_dc:      [[i16; 4]; 2],
    pub coded:          [bool; 25],
    pub transform_size_8x8: bool,
}

impl CurrentMBInfo {
    fn clear_coeffs8x8(&mut self) {
        for c in self.coeffs8x8.iter_mut() {
            c.clear();
        }
    }
    fn can_have_8x8_tx(&self, inference_flag: bool) -> bool {
        match self.mb_type {
            MBType::Intra4x4 | MBType::Intra8x8 | MBType::Intra16x16(_, _, _) | MBType::PCM => false,
            MBType::P8x8 | MBType::P8x8Ref0 | MBType::B8x8 => {
                for &sub_id in self.sub_mb_type.iter() {
                    match sub_id {
                        SubMBType::P8x8 |
                        SubMBType::B8x8(_)
                            => {},
                        SubMBType::Direct8x8
                            => if !inference_flag { return false; },
                        _ => return false,
                    };
                }
                true
            },
            MBType::Direct => inference_flag,
            _ => true,
        }
    }
}

#[derive(Clone,Copy)]
pub struct Coeff8x8 {
    pub coeffs:     [i16; 64],
}

impl Coeff8x8 {
    fn clear(&mut self) {
        self.coeffs = [0; 64];
    }
}

impl Default for Coeff8x8 {
    fn default() -> Self {
        Self {
            coeffs: [0; 64],
        }
    }
}

fn get_long_term_id(is_idr: bool, slice_hdr: &SliceHeader) -> Option<usize> {
    if is_idr && !slice_hdr.long_term_reference {
        None
    } else {
        let marking = &slice_hdr.adaptive_ref_pic_marking;
        for (&op, &arg) in marking.memory_management_control_op.iter().zip(marking.operation_arg.iter()).take(marking.num_ops) {
            if op == 6 {
                return Some(arg as usize);
            }
        }
        None
    }
}

fn unescape_nal(src: &[u8], dst: &mut Vec<u8>) -> usize {
    let mut off = 0;
    let mut zrun = 0;
    dst.clear();
    dst.reserve(src.len());
    while off < src.len() {
        dst.push(src[off]);
        if src[off] != 0 {
            zrun = 0;
        } else {
            zrun += 1;
            if zrun == 2 && off + 1 < src.len() && src[off + 1] == 0x03 {
                zrun = 0;
                off += 1;
            }
            if zrun >= 3 && off + 1 < src.len() && src[off + 1] == 0x01 {
                off -= 3;
                dst.truncate(off);
                break;
            }
        }
        off += 1;
    }
    off
}

fn is_high_bitdepth(edata: &[u8]) -> DecoderResult<bool> {
    if edata.len() > 11 && &edata[0..4] == b"avcC" {
        let mut br = MemoryReader::new_read(edata);
        let mut nal_buf = Vec::new();

                                  br.read_skip(4)?;
        let version             = br.read_byte()?;
        validate!(version == 1);
        let profile             = br.read_byte()?;
        let _compatibility      = br.read_byte()?;
        let _level              = br.read_byte()?;
        let _b                  = br.read_byte()?;
        //validate!((b & 0xFC) == 0xFC);
        //self.nal_len            = (b & 3) + 1;
        let b                   = br.read_byte()?;
        //validate!((b & 0xE0) == 0xE0);
        let num_sps = (b & 0x1F) as usize;
        for _ in 0..num_sps {
            let len             = br.read_u16be()? as usize;
            let offset = br.tell() as usize;
            validate!((br.peek_byte()? & 0x1F) == 7);
            let _size = unescape_nal(&edata[offset..][..len], &mut nal_buf);
                                  br.read_skip(len)?;
            let sps = sets::parse_sps(&nal_buf[1..])?;
            if sps.bit_depth_luma > 8 {
                return Ok(true);
            }
        }
        let num_pps             = br.read_byte()? as usize;
        for _ in 0..num_pps {
            let len             = br.read_u16be()? as usize;
            let offset = br.tell() as usize;
            validate!((br.peek_byte()? & 0x1F) == 8);
            let _size = unescape_nal(&edata[offset..][..len], &mut nal_buf);
                                  br.read_skip(len)?;
        }
        if br.left() > 0 {
            match profile {
                100 | 110 | 122 | 144 => {
                    let b       = br.read_byte()?;
                    // some encoders put something different here
                    if (b & 0xFC) != 0xFC {
                        return Ok(false);
                    }
                    // b & 3 -> chroma format
                    let b       = br.read_byte()?;
                    validate!((b & 0xF8) == 0xF8);
                    // b & 7 -> luma depth minus 8
                    let b       = br.read_byte()?;
                    validate!((b & 0xF8) == 0xF8);
                    // b & 7 -> chroma depth minus 8
                    let num_spsext  = br.read_byte()? as usize;
                    for _ in 0..num_spsext {
                        let len = br.read_u16be()? as usize;
                        // parse spsext
                                  br.read_skip(len)?;
                    }
                },
                _ => {},
            };
        }
    }
    Ok(false)
}

struct STDecoderWrapper {
    h264:       Box<baseline::decoder_st::H264Decoder>,
    h264_hi:    Option<Box<high::decoder_st::H264Decoder>>,
}

impl NADecoder for STDecoderWrapper {
    fn init(&mut self, supp: &mut NADecoderSupport, info: NACodecInfoRef) -> DecoderResult<()> {
        let is_high_depth = if let Some(ref edata) = info.get_extradata() {
                is_high_bitdepth(edata).unwrap_or(false)
            } else {
                false
            };
        if !is_high_depth {
            self.h264.init(supp, info)
        } else {
            let mut h264_hi = Box::new(high::decoder_st::H264Decoder::new());
            h264_hi.init(supp, info)?;
            self.h264_hi = Some(h264_hi);
            Ok(())
        }
    }
    fn decode(&mut self, supp: &mut NADecoderSupport, pkt: &NAPacket) -> DecoderResult<NAFrameRef> {
        if let Some(ref mut h264_hi) = self.h264_hi {
            h264_hi.decode(supp, pkt)
        } else {
            self.h264.decode(supp, pkt)
        }
    }
    fn flush(&mut self) {
        if let Some(ref mut h264_hi) = self.h264_hi {
            h264_hi.flush()
        }
        self.h264.flush();
    }
}

impl NAOptionHandler for STDecoderWrapper {
    fn get_supported_options(&self) -> &[NAOptionDefinition] {
        if let Some(ref h264_hi) = self.h264_hi {
            h264_hi.get_supported_options()
        } else {
            self.h264.get_supported_options()
        }
    }
    fn set_options(&mut self, options: &[NAOption]) {
        if let Some(ref mut h264_hi) = self.h264_hi {
            h264_hi.set_options(options);
        } else {
            self.h264.set_options(options);
        }
    }
    fn query_option_value(&self, name: &str) -> Option<NAValue> {
        if let Some(ref h264_hi) = self.h264_hi {
            h264_hi.query_option_value(name)
        } else {
            self.h264.query_option_value(name)
        }
    }
}

pub fn get_decoder() -> Box<dyn NADecoder + Send> {
    Box::new(STDecoderWrapper {
        h264:       Box::new(baseline::decoder_st::H264Decoder::new()),
        h264_hi:    None,
    })
}

struct MTDecoderWrapper {
    h264:       Box<baseline::decoder_mt::H264MTDecoder>,
    h264_hi:    Option<Box<high::decoder_mt::H264MTDecoder>>,
}

impl NADecoderMT for MTDecoderWrapper {
    fn init(&mut self, supp: &mut NADecoderSupport, info: NACodecInfoRef, nthreads: usize) -> DecoderResult<()> {
        let is_high_depth = if let Some(ref edata) = info.get_extradata() {
                is_high_bitdepth(edata).unwrap_or(false)
            } else {
                false
            };
        if !is_high_depth {
            self.h264.init(supp, info, nthreads)
        } else {
            let mut h264_hi = Box::new(high::decoder_mt::H264MTDecoder::new());
            h264_hi.init(supp, info, nthreads)?;
            self.h264_hi = Some(h264_hi);
            Ok(())
        }
    }
    fn can_take_input(&mut self) -> bool {
        if let Some(ref mut h264_hi) = self.h264_hi {
            h264_hi.can_take_input()
        } else {
            self.h264.can_take_input()
        }
    }
    fn queue_pkt(&mut self, supp: &mut NADecoderSupport, pkt: &NAPacket, user_id: u32) -> DecoderResult<bool> {
        if let Some(ref mut h264_hi) = self.h264_hi {
            h264_hi.queue_pkt(supp, pkt, user_id)
        } else {
            self.h264.queue_pkt(supp, pkt, user_id)
        }
    }
    fn has_output(&mut self) -> bool {
        if let Some(ref mut h264_hi) = self.h264_hi {
            h264_hi.has_output()
        } else {
            self.h264.has_output()
        }
    }
    fn get_frame(&mut self) -> (DecoderResult<NAFrameRef>, u32) {
        if let Some(ref mut h264_hi) = self.h264_hi {
            h264_hi.get_frame()
        } else {
            self.h264.get_frame()
        }
    }
    fn flush(&mut self) {
        if let Some(ref mut h264_hi) = self.h264_hi {
            h264_hi.flush();
        }
        self.h264.flush();
    }
}

impl NAOptionHandler for MTDecoderWrapper {
    fn get_supported_options(&self) -> &[NAOptionDefinition] {
        if let Some(ref h264_hi) = self.h264_hi {
            h264_hi.get_supported_options()
        } else {
            self.h264.get_supported_options()
        }
    }
    fn set_options(&mut self, options: &[NAOption]) {
        if let Some(ref mut h264_hi) = self.h264_hi {
            h264_hi.set_options(options);
        } else {
            self.h264.set_options(options);
        }
    }
    fn query_option_value(&self, name: &str) -> Option<NAValue> {
        if let Some(ref h264_hi) = self.h264_hi {
            h264_hi.query_option_value(name)
        } else {
            self.h264.query_option_value(name)
        }
    }
}

pub fn get_decoder_mt() -> Box<dyn NADecoderMT + Send> {
    Box::new(MTDecoderWrapper {
        h264:       Box::new(baseline::decoder_mt::H264MTDecoder::new()),
        h264_hi:    None,
    })
}

const DEBLOCK_SKIP_OPTION: &str = "skip_deblock";

const DECODER_OPTIONS: &[NAOptionDefinition] = &[
    NAOptionDefinition {
        name: DEBLOCK_SKIP_OPTION, description: "Loop filter skipping mode",
        opt_type: NAOptionDefinitionType::Bool },
    NAOptionDefinition {
        name: FRAME_SKIP_OPTION, description: FRAME_SKIP_OPTION_DESC,
        opt_type: NAOptionDefinitionType::String(Some(&[
                FRAME_SKIP_OPTION_VAL_NONE,
                FRAME_SKIP_OPTION_VAL_KEYFRAME,
                FRAME_SKIP_OPTION_VAL_INTRA
            ])) },
];

#[cfg(test)]
mod test {
    use nihav_core::codecs::*;
    use nihav_core::demuxers::RegisteredDemuxers;
    use nihav_codec_support::test::dec_video::*;
    use crate::*;
    use nihav_commonfmt::generic_register_all_demuxers;

    // samples if not specified otherwise come from H.264 conformance suite
    mod raw_demux;
    mod conformance;
    mod conformance_mt;
    use self::raw_demux::RawH264DemuxerCreator;

    #[test]
    fn test_h264_perframe() {
        let mut dmx_reg = RegisteredDemuxers::new();
        dmx_reg.add_demuxer(&RawH264DemuxerCreator{});
        generic_register_all_demuxers(&mut dmx_reg);
        let mut dec_reg = RegisteredDecoders::new();
        itu_register_all_decoders(&mut dec_reg);

        test_decoding("rawh264", "h264",
                      "assets/ITU/h264-conformance/CABAST3_Sony_E.jsv",
                      None, &dmx_reg, &dec_reg, ExpectedTestResult::MD5Frames(vec![
                        [0xb5e5e368, 0x6ac59bfc, 0x82e35b7b, 0xbed17b81],
                        [0x8343b34d, 0x0de80ae9, 0xe9c08cc9, 0x05161d82],
                        [0x26e08b9b, 0x84949759, 0x71622124, 0x9bfff254],
                        [0x940c38bc, 0x559fb990, 0x2b82a7ca, 0x3543188a],
                        [0x60d7544d, 0x2fc8cc23, 0x4acac90f, 0x44c2a91c],
                        [0x68d86265, 0x15fc15b9, 0xe4946d83, 0x39d9584d],
                        [0xaed8e194, 0xa24b3a8a, 0xbed9085d, 0x05d68293],
                        [0x1cddffac, 0x0ce9d209, 0xc4090b8a, 0xc3008856],
                        [0x42ee0e5e, 0x4c1c3b64, 0xd91cc00b, 0x88be4b15],
                        [0x19a70aa8, 0xd8bc987d, 0x51c04849, 0x71191523],
                        [0x74532da6, 0xecb92919, 0xd39cb150, 0x9ca9933d],
                        [0x0444b315, 0x2ddfb91a, 0x1e21ce06, 0x0c8613e6],
                        [0xce209363, 0xf8d8331f, 0x72e0102f, 0x88de3a97],
                        [0xdbcfa40a, 0x7eed5940, 0xa5c53a66, 0xdfcd3cea],
                        [0x00796b14, 0x58f16117, 0xb6a5efd1, 0xfb129acd],
                        [0x7673f569, 0xfccfb96a, 0x1f614c82, 0xf62ea376],
                        [0x8669d98b, 0x9fdf4e7d, 0xa4083a7f, 0x9b66d296],
                        [0xf0537976, 0x924229ab, 0xd0f4612f, 0xad4b614e],
                        [0xbde82067, 0x6cf23a0c, 0xdd29e64d, 0xcaa72ff3],
                        [0xcfcb544a, 0x1f1a81b0, 0x2217108c, 0x4888d5ef],
                        [0x3369f874, 0x6a6dde75, 0x46d64780, 0xbf6ced32],
                        [0x253a1f45, 0x85954311, 0x983dbabe, 0x658f4ce3],
                        [0xec97b332, 0xa17b26d0, 0xbead22af, 0xa6bd7d8e],
                        [0x5673d973, 0x78528036, 0xabfe5e13, 0xdcedfb26],
                        [0xd6110fa9, 0x532d6a30, 0xb7f0aa7c, 0xae7b544b]]));
    }

    // mostly static music video downloaded with youtube-dl
    #[test]
    fn test_h264_real1() {
        let mut dmx_reg = RegisteredDemuxers::new();
        dmx_reg.add_demuxer(&RawH264DemuxerCreator{});
        generic_register_all_demuxers(&mut dmx_reg);
        let mut dec_reg = RegisteredDecoders::new();
        itu_register_all_decoders(&mut dec_reg);

        test_decoding("mov", "h264", "assets/ITU/1.mp4",
                      Some(60), &dmx_reg, &dec_reg,
                      ExpectedTestResult::MD5Frames(vec![
                            [0x9dbac04a, 0xc49ca8c1, 0x09bb9182, 0xc7928970],
                            [0x9dbac04a, 0xc49ca8c1, 0x09bb9182, 0xc7928970],
                            [0x9dbac04a, 0xc49ca8c1, 0x09bb9182, 0xc7928970],
                            [0x9dbac04a, 0xc49ca8c1, 0x09bb9182, 0xc7928970],
                            [0x9dbac04a, 0xc49ca8c1, 0x09bb9182, 0xc7928970],
                            [0x9dbac04a, 0xc49ca8c1, 0x09bb9182, 0xc7928970],
                            [0x9dbac04a, 0xc49ca8c1, 0x09bb9182, 0xc7928970],
                            [0x9dbac04a, 0xc49ca8c1, 0x09bb9182, 0xc7928970],
                            [0x9dbac04a, 0xc49ca8c1, 0x09bb9182, 0xc7928970],
                            [0x9dbac04a, 0xc49ca8c1, 0x09bb9182, 0xc7928970],
                            [0x9dbac04a, 0xc49ca8c1, 0x09bb9182, 0xc7928970],
                            [0x9dbac04a, 0xc49ca8c1, 0x09bb9182, 0xc7928970],
                            [0x9dbac04a, 0xc49ca8c1, 0x09bb9182, 0xc7928970],
                            [0x9dbac04a, 0xc49ca8c1, 0x09bb9182, 0xc7928970],
                            [0x9dbac04a, 0xc49ca8c1, 0x09bb9182, 0xc7928970],
                            [0x9dbac04a, 0xc49ca8c1, 0x09bb9182, 0xc7928970],
                            [0x9dbac04a, 0xc49ca8c1, 0x09bb9182, 0xc7928970],
                            [0x9dbac04a, 0xc49ca8c1, 0x09bb9182, 0xc7928970],
                            [0x9dbac04a, 0xc49ca8c1, 0x09bb9182, 0xc7928970],
                            [0x9dbac04a, 0xc49ca8c1, 0x09bb9182, 0xc7928970],
                            [0x9dbac04a, 0xc49ca8c1, 0x09bb9182, 0xc7928970],
                            [0x9dbac04a, 0xc49ca8c1, 0x09bb9182, 0xc7928970],
                            [0x9dbac04a, 0xc49ca8c1, 0x09bb9182, 0xc7928970],
                            [0x9dbac04a, 0xc49ca8c1, 0x09bb9182, 0xc7928970],
                            [0x9dbac04a, 0xc49ca8c1, 0x09bb9182, 0xc7928970],
                            [0x9dbac04a, 0xc49ca8c1, 0x09bb9182, 0xc7928970],
                            [0x9dbac04a, 0xc49ca8c1, 0x09bb9182, 0xc7928970],
                            [0x9dbac04a, 0xc49ca8c1, 0x09bb9182, 0xc7928970],
                            [0x9dbac04a, 0xc49ca8c1, 0x09bb9182, 0xc7928970],
                            [0x9dbac04a, 0xc49ca8c1, 0x09bb9182, 0xc7928970],
                            [0x9dbac04a, 0xc49ca8c1, 0x09bb9182, 0xc7928970],
                            [0x9dbac04a, 0xc49ca8c1, 0x09bb9182, 0xc7928970],
                            [0x9dbac04a, 0xc49ca8c1, 0x09bb9182, 0xc7928970],
                            [0x9dbac04a, 0xc49ca8c1, 0x09bb9182, 0xc7928970],
                            [0x9dbac04a, 0xc49ca8c1, 0x09bb9182, 0xc7928970],
                            [0x9dbac04a, 0xc49ca8c1, 0x09bb9182, 0xc7928970],
                            [0x9dbac04a, 0xc49ca8c1, 0x09bb9182, 0xc7928970],
                            [0xc54f1b6e, 0xaba56a71, 0x8b45132b, 0x3c8bde7f],
                            [0xe2742374, 0x7b9d6fa6, 0xd57eb3bb, 0x42986664],
                            [0xa5ebdc2e, 0x9753a46a, 0x631c6359, 0x861ae0e3],
                            [0x4d2c8769, 0xb9e15141, 0x03274d1f, 0xc15a3733],
                            [0x17ebec8f, 0xe417571e, 0x75eb2559, 0x2f9b882b],
                            [0x148e8c97, 0x778f92ba, 0x93646539, 0xeebe643a],
                            [0xc6770caa, 0x1ac11a57, 0x1388a550, 0x2347758e],
                            [0x91eb3ae4, 0xaf664462, 0x858d344a, 0xda3baa79],
                            [0x4de79514, 0x3597aff0, 0x53e1a22f, 0x7875aa4c],
                            [0xd5afcf7c, 0xa0f4ce82, 0x21a70eb2, 0x3911cde1],
                            [0x9efa2a08, 0x29019ca6, 0xaba90890, 0xfb982857],
                            [0xc5755e20, 0x4c66cb54, 0x1194812e, 0x11a9d940],
                            [0xfd131bbb, 0x0acefb02, 0x6c79b7ab, 0x35bcdd26],
                            [0xad159db0, 0xfa65ced2, 0xf77e2b22, 0x9e6283a8],
                            [0xba2059e3, 0xc9f1e5e7, 0x7ea5fbcb, 0xf48d4fc3],
                            [0xbe794078, 0x64d69f9b, 0x7b6355c5, 0x7dfb5b0f],
                            [0x6031b77b, 0x712f42fd, 0x30d423df, 0x740e488c],
                            [0xcc475484, 0x30a664fc, 0x227a9725, 0x4b2bfb18],
                            [0x44bef2ea, 0xaf1e69e8, 0x832d94a8, 0xffb22712],
                            [0xe9471e3d, 0x103de80f, 0xdc44136f, 0x67dacaa8],
                            [0x4df3823d, 0xf6486ca9, 0x016f3114, 0x1c2d0b42],
                            [0x1171666b, 0x08ca0ced, 0x98719757, 0xbd6b4a86],
                            [0x9d2fc556, 0x5569fbbd, 0x0ebf629f, 0xd4fdc3b5],
                            [0x27dbd3c3, 0x803f0230, 0x13f2ff1b, 0xb661b622]]));
    }
    // a sample downloaded from gfycat.com
    #[test]
    fn test_h264_real2() {
        let mut dmx_reg = RegisteredDemuxers::new();
        dmx_reg.add_demuxer(&RawH264DemuxerCreator{});
        generic_register_all_demuxers(&mut dmx_reg);
        let mut dec_reg = RegisteredDecoders::new();
        itu_register_all_decoders(&mut dec_reg);
        test_decoding("mov", "h264", "assets/ITU/DimpledSpanishCuckoo-mobile.mp4",
                      Some(10), &dmx_reg, &dec_reg,
                      ExpectedTestResult::MD5Frames(vec![
                            [0x674c6d60, 0xc7ab918d, 0x9db1beaf, 0xda9f2456],
                            [0x6a935350, 0x3d463ab2, 0xa3ab3c53, 0x97eb896b],
                            [0xf6c60411, 0x19ea2c49, 0x3512371a, 0xce6cb26a],
                            [0xc87afeaa, 0x79899908, 0x152e6320, 0xe689827f],
                            [0xa3d829e3, 0xb404dd32, 0x11983613, 0xbdf10ee6],
                            [0x2440ea01, 0x5b9d7fc7, 0x4fa5632b, 0xd2d76090],
                            [0xd80e8bf9, 0xe9190ab7, 0x2be8fa38, 0xb94182e8],
                            [0x50b9fd9a, 0x64393126, 0xd03162ec, 0xfb54172a],
                            [0x80d1f58f, 0x12e454c0, 0x2140ca5c, 0xe19350ba],
                            [0x26078d38, 0xf6a59d57, 0xcd14eaf8, 0x8eb08259],
                            [0x31494337, 0x6f8d3f52, 0x4bc9ff92, 0x0c601b1c]]));
    }
    #[test]
    fn test_h264_mt_perframe() {
        let mut dmx_reg = RegisteredDemuxers::new();
        dmx_reg.add_demuxer(&RawH264DemuxerCreator{});
        generic_register_all_demuxers(&mut dmx_reg);
        let mut dec_reg = RegisteredMTDecoders::new();
        itu_register_all_mt_decoders(&mut dec_reg);

        test_mt_decoding("rawh264", "h264",
                      "assets/ITU/h264-conformance/CABAST3_Sony_E.jsv",
                      None, &dmx_reg, &dec_reg, ExpectedTestResult::MD5Frames(vec![
                        [0xb5e5e368, 0x6ac59bfc, 0x82e35b7b, 0xbed17b81],
                        [0x8343b34d, 0x0de80ae9, 0xe9c08cc9, 0x05161d82],
                        [0x26e08b9b, 0x84949759, 0x71622124, 0x9bfff254],
                        [0x940c38bc, 0x559fb990, 0x2b82a7ca, 0x3543188a],
                        [0x60d7544d, 0x2fc8cc23, 0x4acac90f, 0x44c2a91c],
                        [0x68d86265, 0x15fc15b9, 0xe4946d83, 0x39d9584d],
                        [0xaed8e194, 0xa24b3a8a, 0xbed9085d, 0x05d68293],
                        [0x1cddffac, 0x0ce9d209, 0xc4090b8a, 0xc3008856],
                        [0x42ee0e5e, 0x4c1c3b64, 0xd91cc00b, 0x88be4b15],
                        [0x19a70aa8, 0xd8bc987d, 0x51c04849, 0x71191523],
                        [0x74532da6, 0xecb92919, 0xd39cb150, 0x9ca9933d],
                        [0x0444b315, 0x2ddfb91a, 0x1e21ce06, 0x0c8613e6],
                        [0xce209363, 0xf8d8331f, 0x72e0102f, 0x88de3a97],
                        [0xdbcfa40a, 0x7eed5940, 0xa5c53a66, 0xdfcd3cea],
                        [0x00796b14, 0x58f16117, 0xb6a5efd1, 0xfb129acd],
                        [0x7673f569, 0xfccfb96a, 0x1f614c82, 0xf62ea376],
                        [0x8669d98b, 0x9fdf4e7d, 0xa4083a7f, 0x9b66d296],
                        [0xf0537976, 0x924229ab, 0xd0f4612f, 0xad4b614e],
                        [0xbde82067, 0x6cf23a0c, 0xdd29e64d, 0xcaa72ff3],
                        [0xcfcb544a, 0x1f1a81b0, 0x2217108c, 0x4888d5ef],
                        [0x3369f874, 0x6a6dde75, 0x46d64780, 0xbf6ced32],
                        [0x253a1f45, 0x85954311, 0x983dbabe, 0x658f4ce3],
                        [0xec97b332, 0xa17b26d0, 0xbead22af, 0xa6bd7d8e],
                        [0x5673d973, 0x78528036, 0xabfe5e13, 0xdcedfb26],
                        [0xd6110fa9, 0x532d6a30, 0xb7f0aa7c, 0xae7b544b]]));
    }
    // a sample downloaded from gfycat.com
    #[test]
    fn test_h264_mt_real2() {
        let mut dmx_reg = RegisteredDemuxers::new();
        dmx_reg.add_demuxer(&RawH264DemuxerCreator{});
        generic_register_all_demuxers(&mut dmx_reg);
        let mut dec_reg = RegisteredMTDecoders::new();
        itu_register_all_mt_decoders(&mut dec_reg);
        test_mt_decoding("mov", "h264", "assets/ITU/DimpledSpanishCuckoo-mobile.mp4",
                      Some(10), &dmx_reg, &dec_reg,
                      ExpectedTestResult::MD5Frames(vec![
                            [0x674c6d60, 0xc7ab918d, 0x9db1beaf, 0xda9f2456],
                            [0x6a935350, 0x3d463ab2, 0xa3ab3c53, 0x97eb896b],
                            [0xa3d829e3, 0xb404dd32, 0x11983613, 0xbdf10ee6],
                            [0xc87afeaa, 0x79899908, 0x152e6320, 0xe689827f],
                            [0x2440ea01, 0x5b9d7fc7, 0x4fa5632b, 0xd2d76090],
                            [0xf6c60411, 0x19ea2c49, 0x3512371a, 0xce6cb26a],
                            [0x50b9fd9a, 0x64393126, 0xd03162ec, 0xfb54172a],
                            [0xd80e8bf9, 0xe9190ab7, 0x2be8fa38, 0xb94182e8],
                            [0x80d1f58f, 0x12e454c0, 0x2140ca5c, 0xe19350ba],
                            [0x26078d38, 0xf6a59d57, 0xcd14eaf8, 0x8eb08259],
                            [0x31494337, 0x6f8d3f52, 0x4bc9ff92, 0x0c601b1c]]));
    }
    // a sample from libav FATE suite
    #[test]
    fn test_h264_10_bit() {
        let mut dmx_reg = RegisteredDemuxers::new();
        dmx_reg.add_demuxer(&RawH264DemuxerCreator{});
        generic_register_all_demuxers(&mut dmx_reg);
        let mut dec_reg = RegisteredDecoders::new();
        itu_register_all_decoders(&mut dec_reg);
        test_decoding("rawh264", "h264", "assets/ITU/normal-10.h264",
                      None, &dmx_reg, &dec_reg,
                      ExpectedTestResult::MD5Frames(vec![
                          [0x50aa3b23, 0x62021b6d, 0x5a4ed176, 0x309bbeb2],
                          [0xdc632b62, 0x5a588c84, 0x7a5f8537, 0xc82e356b],
                          [0x0be7e536, 0xf54f8a6f, 0xe6ed1bb5, 0xf10ee116],
                          [0xcb69cc4d, 0xfe8cccab, 0x161c3687, 0x7bf5de8b],
                          [0x36667f83, 0x78468cab, 0xa378f86b, 0x9358a3f2],
                          [0x31a31140, 0xb8069e3e, 0x22d90a05, 0x0b3f8bff],
                          [0xb5f144a8, 0xb56ac2d6, 0x0bcae310, 0xebc3042e],
                          [0x324a14ce, 0x87bc9e89, 0x312bc02f, 0x9c6e2b0b],
                          [0xa8e10c16, 0xa85de23e, 0xd8e8e47e, 0xd00a9fd9],
                          [0x0b626c0f, 0x9ab5212d, 0x98728b97, 0xb8ce84a0]]));
    }
    #[test]
    fn test_h264_10bit_mt() {
        let mut dmx_reg = RegisteredDemuxers::new();
        dmx_reg.add_demuxer(&RawH264DemuxerCreator{});
        generic_register_all_demuxers(&mut dmx_reg);
        let mut dec_reg = RegisteredMTDecoders::new();
        itu_register_all_mt_decoders(&mut dec_reg);
        test_mt_decoding("rawh264", "h264", "assets/ITU/normal-10.h264",
                         None, &dmx_reg, &dec_reg,
                         ExpectedTestResult::MD5Frames(vec![
                             [0x50aa3b23, 0x62021b6d, 0x5a4ed176, 0x309bbeb2],
                             [0xdc632b62, 0x5a588c84, 0x7a5f8537, 0xc82e356b],
                             [0x0be7e536, 0xf54f8a6f, 0xe6ed1bb5, 0xf10ee116],
                             [0xcb69cc4d, 0xfe8cccab, 0x161c3687, 0x7bf5de8b],
                             [0x36667f83, 0x78468cab, 0xa378f86b, 0x9358a3f2],
                             [0x31a31140, 0xb8069e3e, 0x22d90a05, 0x0b3f8bff],
                             [0xb5f144a8, 0xb56ac2d6, 0x0bcae310, 0xebc3042e],
                             [0x324a14ce, 0x87bc9e89, 0x312bc02f, 0x9c6e2b0b],
                             [0xa8e10c16, 0xa85de23e, 0xd8e8e47e, 0xd00a9fd9],
                             [0x0b626c0f, 0x9ab5212d, 0x98728b97, 0xb8ce84a0]]));
    }
}

pub const I4X4_SCAN: [(u8, u8); 16] = [
    (0,0), (1,0), (0,1), (1,1), (2,0), (3,0), (2,1), (3,1),
    (0,2), (1,2), (0,3), (1,3), (2,2), (3,2), (2,3), (3,3)
];
