use nihav_core::codecs::*;
use nihav_core::io::byteio::*;
use nihav_core::io::bitwriter::*;
use nihav_codec_support::codecs::ZIGZAG;
use nihav_codec_support::codecs::jpeg::*;

const C01: i32 =  2446;
const C02: i32 =  3196;
const C03: i32 =  4433;
const C04: i32 =  6270;
const C05: i32 =  7373;
const C06: i32 =  9633;
const C07: i32 = 12299;
const C08: i32 = 15137;
const C09: i32 = 16069;
const C10: i32 = 16819;
const C11: i32 = 20995;
const C12: i32 = 25172;
const PRECISION: u8 = 13;

// LLM FDCT
fn fdct_1d(coeffs: &mut [i32], shift: u8) {
    let bias = (1 << shift) >> 1;

    let in0 = coeffs[0];
    let in1 = coeffs[1];
    let in2 = coeffs[2];
    let in3 = coeffs[3];
    let in4 = coeffs[4];
    let in5 = coeffs[5];
    let in6 = coeffs[6];
    let in7 = coeffs[7];

    let tmp0 = in0 + in7;
    let tmp7 = in0 - in7;
    let tmp1 = in1 + in6;
    let tmp6 = in1 - in6;
    let tmp2 = in2 + in5;
    let tmp5 = in2 - in5;
    let tmp3 = in3 + in4;
    let tmp4 = in3 - in4;

    let tmp8 = tmp0 + tmp3;
    let tmp9 = tmp0 - tmp3;
    let tmpa = tmp1 + tmp2;
    let tmpb = tmp1 - tmp2;

    let t00 = (tmp4 + tmp5 + tmp6 + tmp7) * C06;
    let t01 = (tmp4 + tmp7) * -C05;
    let t02 = (tmp5 + tmp6) * -C11;
    let t03 = (tmp4 + tmp6) * -C09 + t00;
    let t04 = (tmp5 + tmp7) * -C02 + t00;
    let t05 = tmp4 * C01;
    let t06 = tmp5 * C10;
    let t07 = tmp6 * C12;
    let t08 = tmp7 * C07;

    if shift <= PRECISION {
        coeffs[0] = (tmp8 + tmpa) << (PRECISION - shift);
        coeffs[4] = (tmp8 - tmpa) << (PRECISION - shift);
    } else {
        let sh2 = shift - PRECISION;
        let bias2 = (1 << sh2) >> 1;
        coeffs[0] = (tmp8 + tmpa + bias2) >> sh2;
        coeffs[4] = (tmp8 - tmpa + bias2) >> sh2;
    }

    coeffs[1] = (t08 + t01 + t04 + bias) >> shift;
    coeffs[2] = ((tmp9 + tmpb) * C03 + tmp9 * C04 + bias) >> shift;
    coeffs[3] = (t07 + t02 + t03 + bias) >> shift;
    coeffs[5] = (t06 + t02 + t04 + bias) >> shift;
    coeffs[6] = ((tmp9 + tmpb) * C03 - tmpb * C08 + bias) >> shift;
    coeffs[7] = (t05 + t01 + t03 + bias) >> shift;
}

fn fdct(coeffs: &mut [i16; 64]) {
    let mut tmp: [i32; 64] = [0; 64];
    let mut row: [i32; 8] = [0; 8];
    for (dst, &src) in tmp.iter_mut().zip(coeffs.iter()) {
        *dst = i32::from(src);
    }
    for trow in tmp.chunks_exact_mut(8) {
        fdct_1d(trow, 12);
    }
    for i in 0..8 {
        for (dst, src) in row.iter_mut().zip(tmp.chunks_exact(8)) {
            *dst = src[i];
        }
        fdct_1d(&mut row, 17);
        for (dst, &src) in coeffs.chunks_exact_mut(8).zip(row.iter()) {
            dst[i] = src.clamp(-8191, 8191) as i16;
        }
    }
}

struct JEncCodebook {
    bits:   [u8; 256],
    codes:  [u16; 256],
    syms:   &'static [u8],
}

trait WriteSym {
    fn write_sym(&mut self, cb: &JEncCodebook, sym: u8) -> EncoderResult<()>;
}

impl WriteSym for BitWriter {
    fn write_sym(&mut self, cb: &JEncCodebook, sym: u8) -> EncoderResult<()> {
        if let Some(idx) = cb.syms.iter().position(|&s| s == sym) {
            self.write(u32::from(cb.codes[idx]), cb.bits[idx]);
            Ok(())
        } else {
            Err(EncoderError::Bug)
        }
    }
}

impl JEncCodebook {
    fn create_cb(lens: &[u8], syms: &'static [u8]) -> Self {
        let mut codes = [0; 256];
        let mut bits = [0; 256];

        let mut iter = bits.iter_mut();
        for (i, &len) in lens.iter().enumerate() {
            for _ in 0..len {
                *iter.next().unwrap() = (i + 1) as u8;
            }
        }
        let mut code = 0;
        let mut si = bits[0];
        let mut idx = 0;
        while idx < syms.len() {
            while idx < syms.len() && bits[idx] == si {
                codes[idx] = code;
                code += 1;
                idx  += 1;
            }
            while idx < syms.len() && bits[idx] != si {
                code <<= 1;
                si += 1;
            }
        }
        Self { bits, codes, syms }
    }
}

fn get_cat(val: i16) -> u8 {
    let mut cat = 0u8;
    while val.unsigned_abs() >= (1 << cat) {
        cat += 1;
    }
    cat
}

struct JPGEncoder {
    stream:     Option<NAStreamRef>,
    pkt:        Option<NAPacket>,
    quality:    u8,
    cur_q:      u8,
    qmat_y:     [u8; 64],
    qmat_c:     [u8; 64],
    tmp:        Vec<u8>,
    dc_cb:      [JEncCodebook; 2],
    ac_cb:      [JEncCodebook; 2],
}

impl JPGEncoder {
    fn new() -> Self {
        let dc_cb = std::array::from_fn(|idx| JEncCodebook::create_cb(&DC_LENS[idx], &DC_SYMS));
        let ac_cb = std::array::from_fn(|idx| JEncCodebook::create_cb(&AC_LENS[idx], AC_SYMS[idx]));
        Self {
            stream:     None,
            pkt:        None,
            quality:    0,
            cur_q:      255,
            qmat_y:     [0; 64],
            qmat_c:     [0; 64],
            tmp:        Vec::new(),
            dc_cb, ac_cb,
        }
    }
    fn get_blocks(blocks: &mut [[i16; 64]; 4], src: &[u8], stride: usize, hstep: usize, vstep: usize) -> usize {
        let mut blk_no = 0;
        for strip in src.chunks(stride * 8).take(vstep / 8) {
            for xoff in 0..(hstep / 8) {
                for (row, line) in blocks[blk_no].chunks_exact_mut(8).zip(strip[xoff * 8..].chunks(stride)) {
                    for (dst, &src) in row.iter_mut().zip(line.iter()) {
                        *dst = i16::from(src);
                    }
                }
                blk_no += 1;
            }
        }
        blk_no
    }
    fn write_block(bw: &mut BitWriter, blk: &[i16; 64], dc_cb: &JEncCodebook, ac_cb: &JEncCodebook) -> EncoderResult<()> {
        let dc = blk[0];
        let cat = get_cat(dc);
        bw.write_sym(dc_cb, cat)?;
        if cat > 0 {
            if dc > 0 {
                bw.write(dc as u32, cat);
            } else {
                bw.write(((1 << cat) - 1 + dc) as u32, cat);
            }
        }

        let mut run = 0u8;
        for &zz_idx in ZIGZAG.iter().skip(1) {
            let coef = blk[zz_idx];
            if coef != 0 {
                while run >= 16 {
                    bw.write_sym(ac_cb, 0xF0)?;
                    run -= 16;
                }
                let cat = get_cat(coef);
                bw.write_sym(ac_cb, (run << 4) | cat)?;
                if cat > 0 {
                    if coef > 0 {
                        bw.write(coef as u32, cat);
                    } else {
                        bw.write(((1 << cat) - 1 + coef) as u32, cat);
                    }
                }
                run = 0;
            } else {
                run += 1;
            }
        }
        if run > 0 {
            bw.write_sym(ac_cb, 0x00)?;
        }
        Ok(())
    }
    fn encode_data(&mut self, dst: Vec<u8>, vbuf: NAVideoBufferRef<u8>, no_subsampling: bool) -> EncoderResult<Vec<u8>> {
        let vinfo = vbuf.get_info();
        let src = vbuf.get_data();
        let mut bw = BitWriter::new(dst, BitWriterMode::BE);
        let mut blocks = [[[0i16; 64]; 4]; 4];
        let components = usize::from(vinfo.format.components);
        let mut offsets = [0; 4];
        let mut strides = [0; 4];
        for (c_no, (offset, stride)) in offsets.iter_mut().zip(strides.iter_mut()).enumerate() {
            *offset = vbuf.get_offset(c_no);
            *stride = vbuf.get_stride(c_no);
        }

        let mut dc_pred = [1024; 4];
        let qmats = [&self.qmat_y, &self.qmat_c];
        if !no_subsampling {
            let mut h_sizes = [0; 4];
            let mut v_sizes = [0; 4];
            for ((h_sz, v_sz), comp) in h_sizes.iter_mut().zip(v_sizes.iter_mut())
                    .zip(vinfo.format.comp_info.iter().flatten()) {
                *h_sz = 16 >> comp.h_ss;
                *v_sz = 16 >> comp.v_ss;
            }

            for _y in (0..vinfo.height).step_by(16) {
                for x in (0..vinfo.width).step_by(16) {
                    for (c_no, (&hstep, &vstep)) in h_sizes.iter().zip(v_sizes.iter())
                            .enumerate().take(components) {
                        let nblocks = Self::get_blocks(&mut blocks[c_no], &src[offsets[c_no] + x / 16 * hstep..], strides[c_no], hstep, vstep);
                        let cb_idx = if matches!(c_no, 1 | 2) { 1 } else { 0 };

                        for blk in blocks[c_no][..nblocks].iter_mut() {
                            fdct(blk);
                            // requant and clip DC before prediction
                            blk[0] = (blk[0] / i16::from(qmats[cb_idx][0])).clamp(-1023, 1023) * i16::from(qmats[cb_idx][0]);
                            let ldc = blk[0];
                            blk[0] -= dc_pred[c_no];
                            dc_pred[c_no] = ldc;
                            for (&idx, &quant) in ZIGZAG.iter().zip(qmats[cb_idx].iter()) {
                                blk[idx] = (blk[idx] / i16::from(quant)).clamp(-1023, 1023);
                            }
                            Self::write_block(&mut bw, blk, &self.dc_cb[cb_idx], &self.ac_cb[cb_idx])?;
                        }
                    }
                }
                for (offset, (&stride, &vsize)) in offsets.iter_mut()
                        .zip(strides.iter().zip(v_sizes.iter())) {
                    *offset += stride * vsize;
                }
            }
        } else {
            for _y in (0..vinfo.height).step_by(8) {
                for x in (0..vinfo.width).step_by(8) {
                    for (c_no, blocks) in blocks.iter_mut().take(components).enumerate() {
                        Self::get_blocks(blocks, &src[offsets[c_no] + x..], strides[c_no], 8, 8);
                        let cb_idx = if matches!(c_no, 1 | 2) { 1 } else { 0 };

                        let blk = &mut blocks[0];
                        fdct(blk);
                        // requant and clip DC before prediction
                        blk[0] = (blk[0] / i16::from(qmats[cb_idx][0])).clamp(-1023, 1023) * i16::from(qmats[cb_idx][0]);
                        let ldc = blk[0];
                        blk[0] -= dc_pred[c_no];
                        dc_pred[c_no] = ldc;
                        for (&idx, &quant) in ZIGZAG.iter().zip(qmats[cb_idx].iter()) {
                            blk[idx] = (blk[idx] / i16::from(quant)).clamp(-1023, 1023);
                        }
                        Self::write_block(&mut bw, blk, &self.dc_cb[cb_idx], &self.ac_cb[cb_idx])?;
                    }
                }
                for (offset, &stride) in offsets.iter_mut().zip(strides.iter()) {
                    *offset += stride * 8;
                }
            }
        }

        Ok(bw.end())
    }
}

impl NAEncoder for JPGEncoder {
    fn negotiate_format(&self, encinfo: &EncodeParameters) -> EncoderResult<EncodeParameters> {
        match encinfo.format {
            NACodecTypeInfo::None => {
                Ok(EncodeParameters {
                    format: NACodecTypeInfo::Video(NAVideoInfo::new(0, 0, true, YUV420_FORMAT)),
                    ..Default::default()
                })
            },
            NACodecTypeInfo::Audio(_) => Err(EncoderError::FormatError),
            NACodecTypeInfo::Video(vinfo) => {
                let format = if vinfo.format.model.is_yuv() { vinfo.format } else { YUV420_FORMAT };
                let width  = (vinfo.width  + 15) & !15;
                let height = (vinfo.height + 15) & !15;
                let outinfo = NAVideoInfo::new(width, height, false, format);
                let mut ofmt = *encinfo;
                ofmt.format = NACodecTypeInfo::Video(outinfo);
                Ok(ofmt)
            }
        }
    }
    fn get_capabilities(&self) -> u64 { ENC_CAPS_PARAMCHANGE }
    fn init(&mut self, stream_id: u32, encinfo: EncodeParameters) -> EncoderResult<NAStreamRef> {
        match encinfo.format {
            NACodecTypeInfo::None => Err(EncoderError::FormatError),
            NACodecTypeInfo::Audio(_) => Err(EncoderError::FormatError),
            NACodecTypeInfo::Video(vinfo) => {
                if vinfo.width > 65535 || vinfo.height > 65535 {
                    return Err(EncoderError::FormatError);
                }
                if (vinfo.width | vinfo.height) & 15 != 0 {
                    return Err(EncoderError::FormatError);
                }
                if !vinfo.format.model.is_yuv() {
                    return Err(EncoderError::FormatError);
                }

                let out_info = NAVideoInfo::new(vinfo.width, vinfo.height, false, vinfo.format);
                let info = NACodecInfo::new("jpeg", NACodecTypeInfo::Video(out_info), None);
                let mut stream = NAStream::new(StreamType::Video, stream_id, info, encinfo.tb_num, encinfo.tb_den, 0);
                stream.set_num(stream_id as usize);
                let stream = stream.into_ref();

                self.stream = Some(stream.clone());
                self.quality = encinfo.quality.min(100);

                Ok(stream)
            },
        }
    }
    fn encode(&mut self, frm: &NAFrame) -> EncoderResult<()> {
        let vbuf = frm.get_buffer().get_vbuf().ok_or(EncoderError::InvalidParameters)?;
        let vinfo = vbuf.get_info();

        if vinfo.width > 65535 || vinfo.height > 65535 {
            return Err(EncoderError::FormatError);
        }
        if (vinfo.width | vinfo.height) & 15 != 0 {
            return Err(EncoderError::FormatError);
        }
        if !vinfo.format.model.is_yuv() {
            return Err(EncoderError::FormatError);
        }
        if vinfo.format.get_max_subsampling() > 1 {
            return Err(EncoderError::NotImplemented);
        }

        let mut dbuf = Vec::with_capacity(4);
        let mut bw   = GrowableMemoryWriter::new_write(&mut dbuf);

        if self.quality != self.cur_q {
            if self.quality >= 50 || self.quality == 0 {
                let q = if self.quality > 0 { u16::from(200 - self.quality * 2) } else { 20 };
                for (dst, &src) in self.qmat_y.iter_mut().zip(DEF_LUMA_QUANT.iter()) {
                    *dst = ((q * u16::from(src) + 50) / 100).clamp(1, 255) as u8;
                }
                for (dst, &src) in self.qmat_c.iter_mut().zip(DEF_CHROMA_QUANT.iter()) {
                    *dst = ((q * u16::from(src) + 50) / 100).clamp(1, 255) as u8;
                }
            } else {
                let q = u32::from(self.quality);
                for (dst, &src) in self.qmat_y.iter_mut().zip(DEF_LUMA_QUANT.iter()) {
                    *dst = ((5000 * u32::from(src) / q + 50) / 100).clamp(1, 255) as u8;
                }
                for (dst, &src) in self.qmat_c.iter_mut().zip(DEF_CHROMA_QUANT.iter()) {
                    *dst = ((5000 * u32::from(src) / q + 50) / 100).clamp(1, 255) as u8;
                }
            }
            self.cur_q = self.quality;
        }

        // start of image
        bw.write_u16be(0xFFD8)?;
        // the usual marker
        bw.write_u16be(0xFFE0)?;
        bw.write_u16be(16)?;
        bw.write_buf(b"JFIF\x00")?;
        bw.write_byte(1)?;
        bw.write_byte(1)?;
        bw.write_byte(1)?;
        bw.write_u16be(72)?;
        bw.write_u16be(72)?;
        bw.write_byte(0)?;
        bw.write_byte(0)?;

        // luma quant matrix
        bw.write_u16be(0xFFDB)?;
        bw.write_u16be(64 + 3)?;
        bw.write_byte(0)?;
        bw.write_buf(&self.qmat_y)?;

        // chroma quant matrix
        bw.write_u16be(0xFFDB)?;
        bw.write_u16be(64 + 3)?;
        bw.write_byte(1)?;
        bw.write_buf(&self.qmat_c)?;

        // baseline image frame start
        bw.write_u16be(0xFFC0)?;
        let len = 8 + vinfo.format.components * 3;
        bw.write_u16be(u16::from(len))?;
        bw.write_byte(8)?;
        bw.write_u16be(vinfo.height as u16)?;
        bw.write_u16be(vinfo.width as u16)?;
        bw.write_byte(vinfo.format.components)?;

        let no_subsampling = vinfo.format.get_max_subsampling() == 0;

        if !no_subsampling {
            for (c_id, cinfo) in vinfo.format.comp_info.iter().flatten().enumerate() {
                bw.write_byte((c_id + 1) as u8)?;
                bw.write_byte(((2 >> cinfo.h_ss) << 4) | (2 >> cinfo.v_ss))?;
                bw.write_byte(if matches!(c_id, 1 | 2) { 1 } else { 0 })?;
            }
        } else {
            for c_id in 0..vinfo.format.components {
                bw.write_byte(c_id + 1)?;
                bw.write_byte(0x11)?;
                bw.write_byte(if matches!(c_id, 1 | 2) { 1 } else { 0 })?;
            }
        }

        // start of scan
        bw.write_u16be(0xFFDA)?;
        let len = 6 + vinfo.format.components * 2;
        bw.write_u16be(u16::from(len))?;
        bw.write_byte(vinfo.format.components)?;
        for c_id in 0..usize::from(vinfo.format.components) {
            bw.write_byte((c_id + 1) as u8)?;
            let is_chroma = matches!(c_id, 1 | 2);
            bw.write_byte(if !is_chroma { 0x00 } else { 0x11 })?;
        }
        bw.write_byte(0)?;
        bw.write_byte(63)?;
        bw.write_byte(0)?;

        self.tmp.clear();
        let mut tvec = Vec::new();
        std::mem::swap(&mut self.tmp, &mut tvec);
        let mut tvec = self.encode_data(tvec, vbuf, no_subsampling)?;
        std::mem::swap(&mut self.tmp, &mut tvec);

        for &b in self.tmp.iter() {
            bw.write_byte(b)?;
            if b == 0xFF {
                bw.write_byte(0)?;
            }
        }

        // end of image
        bw.write_u16be(0xFFD9)?;

        self.pkt = Some(NAPacket::new(self.stream.clone().unwrap(), frm.ts, true, dbuf));
        Ok(())
    }
    fn get_packet(&mut self) -> EncoderResult<Option<NAPacket>> {
        let mut npkt = None;
        std::mem::swap(&mut self.pkt, &mut npkt);
        Ok(npkt)
    }
    fn flush(&mut self) -> EncoderResult<()> {
        Ok(())
    }
}

impl NAOptionHandler for JPGEncoder {
    fn get_supported_options(&self) -> &[NAOptionDefinition] { &[] }
    fn set_options(&mut self, _options: &[NAOption]) {}
    fn query_option_value(&self, _name: &str) -> Option<NAValue> { None }
}

pub fn get_encoder() -> Box<dyn NAEncoder + Send> {
    Box::new(JPGEncoder::new())
}

#[cfg(test)]
mod test {
    use nihav_core::codecs::*;
    use nihav_core::demuxers::*;
    use nihav_core::muxers::*;
    use crate::*;
    use nihav_codec_support::test::enc_video::*;

    // sample: self-created with avconv
    fn test_jpeg(format: NAPixelFormaton, quality: u8, enc_options: &[NAOption], hash: &[u32; 4]) {
        let mut dmx_reg = RegisteredDemuxers::new();
        generic_register_all_demuxers(&mut dmx_reg);
        let mut dec_reg = RegisteredDecoders::new();
        generic_register_all_decoders(&mut dec_reg);
        let mut mux_reg = RegisteredMuxers::new();
        generic_register_all_muxers(&mut mux_reg);
        let mut enc_reg = RegisteredEncoders::new();
        generic_register_all_encoders(&mut enc_reg);

        let dec_config = DecoderTestParams {
                demuxer:        "yuv4mpeg",
                in_name:        "assets/Misc/test.y4m",
                stream_type:    StreamType::Video,
                limit:          None,
                dmx_reg, dec_reg,
            };
        let enc_config = EncoderTestParams {
                muxer:          "avi",
                enc_name:       "jpeg",
                out_name:       "mjpeg.avi",
                mux_reg, enc_reg,
            };
        let dst_vinfo = NAVideoInfo {
                width:   160,
                height:  128,
                format,
                flipped: false,
                bits:    24,
            };
        let enc_params = EncodeParameters {
                format:  NACodecTypeInfo::Video(dst_vinfo),
                quality,
                bitrate: 0,
                tb_num:  0,
                tb_den:  0,
                flags:   0,
            };
        //test_encoding_to_file(&dec_config, &enc_config, enc_params, enc_options);
        test_encoding_md5(&dec_config, &enc_config, enc_params, enc_options, hash);
    }
    #[test]
    fn test_jpeg_yuv_grey() {
        let enc_options = &[];
        test_jpeg(GREY_FORMAT, 90, enc_options, &[0x280d7a9e, 0xb18b9c91, 0x4d23946f, 0x3425b95d]);
    }
    #[test]
    fn test_jpeg_yuv420() {
        let enc_options = &[];
        test_jpeg(YUV420_FORMAT, 90, enc_options, &[0x54cc4a28, 0xdb8940fa, 0x1116a616, 0xb53b01a2]);
    }
    #[test]
    fn test_jpeg_quality_60() {
        let enc_options = &[];
        test_jpeg(YUV420_FORMAT, 60, enc_options, &[0x654be3de, 0xb61df243, 0x5a986aef, 0xc000e5a1]);
    }
    #[test]
    fn test_jpeg_yuv422() {
        let fmt = "yuv422p".parse::<NAPixelFormaton>().unwrap();
        let enc_options = &[];
        test_jpeg(fmt, 90, enc_options, &[0x2bfd432e, 0x0f49e204, 0xf6f350a8, 0x7f8b04c7]);
    }
    #[test]
    fn test_jpeg_yuv444() {
        let fmt = "yuv444p".parse::<NAPixelFormaton>().unwrap();
        let enc_options = &[];
        test_jpeg(fmt, 90, enc_options, &[0x8362d073, 0xd150c08a, 0xf736a473, 0x9384941d]);
    }
}
