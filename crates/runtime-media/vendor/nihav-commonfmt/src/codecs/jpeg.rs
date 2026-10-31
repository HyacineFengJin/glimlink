use nihav_core::io::byteio::{ByteIO,MemoryReader};
use nihav_core::codecs::*;
use nihav_codec_support::codecs::jpeg::*;

struct DecoderWrapper {
    info:       NACodecInfoRef,
    jpeg:       JPEGDecoder,
    buf:        Vec<u8>,
}

impl DecoderWrapper {
    fn new() -> Self {
        Self {
            info:       NACodecInfo::new_dummy(),
            jpeg:       JPEGDecoder::new(),
            buf:        Vec::new(),
        }
    }
    fn reset(&mut self) {
        self.jpeg.quant      = [[0; 64]; 4];
        self.jpeg.cb         = JCodebooks::default();
        self.jpeg.width      = 0;
        self.jpeg.height     = 0;
        self.jpeg.depth      = 0;
    }

    fn parse_sof(&mut self, br: &mut dyn ByteIO) -> DecoderResult<NABufferType> {
        validate!(self.jpeg.width == 0);

        let len                         = br.read_u16be()? as usize;
        validate!(len >= 11);
        let p                           = br.read_byte()?;
        validate!(p > 2);
        if p != 8 {
            return Err(DecoderError::NotImplemented);
        }
        let y                           = br.read_u16be()? as usize;
        let x                           = br.read_u16be()? as usize;
        validate!(x > 0);
        if y == 0 {
            return Err(DecoderError::NotImplemented);
        }
        self.jpeg.depth = p;
        self.jpeg.width = x;
        self.jpeg.height = y;
        let nf                          = br.read_byte()? as usize;
        validate!(nf > 0);
        validate!(len == 8 + nf * 3);
        if nf > MAX_CHROMATONS {
            return Err(DecoderError::NotImplemented);
        }
        self.jpeg.max_h = 0;
        self.jpeg.max_v = 0;
        for i in 0..nf {
            let c                       = br.read_byte()?;
            self.jpeg.comp_id[i] = c;
            let hv                      = br.read_byte()?;
            let t                       = br.read_byte()?;
            validate!(t < 4);
            self.jpeg.qselect[i] = t;
            self.jpeg.subsamp[i] = hv;
            let hs = hv >> 4;

            validate!(hs == 1 || hs == 2 || (i == 0 && hs == 4));
            let vs = hv & 0xF;
            validate!(vs == 1 || vs == 2 || (i == 0 && vs == 4));
            self.jpeg.max_h = self.jpeg.max_h.max(hs);
            self.jpeg.max_v = self.jpeg.max_v.max(vs);
        }
        let mut chromatons = [None; MAX_CHROMATONS];
        for (i, chr) in chromatons[..nf].iter_mut().enumerate() {
            let h_ss = match self.jpeg.max_h / (self.jpeg.subsamp[i] >> 4) {
                    1 => 0,
                    2 => 1,
                    4 => 2,
                    _ => unreachable!(),
                };
            let v_ss = match self.jpeg.max_v / (self.jpeg.subsamp[i] & 0xF) {
                    1 => 0,
                    2 => 1,
                    4 => 2,
                    _ => return Err(DecoderError::InvalidData),
                };

            *chr = Some(NAPixelChromaton {
                    h_ss, v_ss,
                    packed:     false,
                    depth:      p,
                    shift:      0,
                    comp_offs:  i as u8,
                    next_elem:  (p + 7) >> 3,
                });
        }
        for i in 0..nf {
            for j in i + 1..nf {
                validate!(self.jpeg.comp_id[i] != self.jpeg.comp_id[j]);
            }
        }
        let formaton = NAPixelFormaton {
                model:      ColourModel::YUV(YUVSubmodel::YUVJ),
                components: nf as u8,
                comp_info:  chromatons,
                elem_size:  0,
                be:         false,
                alpha:      nf == 2 || nf == 4,
                palette:    false,
            };
        let vinfo = NAVideoInfo::new(x, y, false, formaton);
        Ok(alloc_video_buffer(vinfo, 4)?)
    }
}

impl NADecoder for DecoderWrapper {
    fn init(&mut self, _supp: &mut NADecoderSupport, info: NACodecInfoRef) -> DecoderResult<()> {
        if let NACodecTypeInfo::Video(vinfo) = info.get_properties() {
            let w = vinfo.get_width();
            let h = vinfo.get_height();
            let myinfo = NACodecTypeInfo::Video(NAVideoInfo::new(w, h, false, YUV420_FORMAT));
            self.info = NACodecInfo::new_ref(info.get_name(), myinfo, info.get_extradata()).into_ref();
            Ok(())
        } else {
            Err(DecoderError::InvalidData)
        }
    }
    fn decode(&mut self, _supp: &mut NADecoderSupport, pkt: &NAPacket) -> DecoderResult<NAFrameRef> {
        let src = pkt.get_buffer();
        if src.len() <= 4 { return Err(DecoderError::ShortData); }

        let mut bufinfo = NABufferType::None;
        let mut br = MemoryReader::new_read(&src);
        let start_tag                   = br.read_u16be()?;
        validate!(start_tag == 0xFFD8);

        let mut jtype = JPEGType::None;
        let mut arith = false;
        self.reset();
        loop {
            let tag                     = br.read_u16be()?;
            match tag {
                0xFFC0 => { //baseline DCT header
                    jtype = JPEGType::Baseline;
                    arith = false;
                    bufinfo = self.parse_sof(&mut br)?;
                },
                0xFFC1 => {
                    jtype = JPEGType::Extended;
                    arith = false;
                    bufinfo = self.parse_sof(&mut br)?;
                },
                0xFFC2 => {
                    jtype = JPEGType::Progressive;
                    arith = false;
                    bufinfo = self.parse_sof(&mut br)?;
                },
                0xFFC3 => {
                    jtype = JPEGType::Lossless;
                    arith = false;
                    bufinfo = self.parse_sof(&mut br)?;
                },
                0xFFC5 => {
                    jtype = JPEGType::Differential;
                    arith = false;
                    bufinfo = self.parse_sof(&mut br)?;
                },
                0xFFC6 => {
                    jtype = JPEGType::DiffProgressive;
                    arith = false;
                    bufinfo = self.parse_sof(&mut br)?;
                },
                0xFFC7 => {
                    jtype = JPEGType::DiffLossless;
                    arith = false;
                    bufinfo = self.parse_sof(&mut br)?;
                },
                0xFFC8 => return Err(DecoderError::NotImplemented),
                0xFFC9 => {
                    jtype = JPEGType::Extended;
                    arith = true;
                    bufinfo = self.parse_sof(&mut br)?;
                },
                0xFFCA => {
                    jtype = JPEGType::Progressive;
                    arith = true;
                    bufinfo = self.parse_sof(&mut br)?;
                },
                0xFFCB => {
                    jtype = JPEGType::Lossless;
                    arith = true;
                    bufinfo = self.parse_sof(&mut br)?;
                },
                0xFFCD => {
                    jtype = JPEGType::Differential;
                    arith = true;
                    bufinfo = self.parse_sof(&mut br)?;
                },
                0xFFCE => {
                    jtype = JPEGType::DiffProgressive;
                    arith = true;
                    bufinfo = self.parse_sof(&mut br)?;
                },
                0xFFCF => {
                    jtype = JPEGType::DiffLossless;
                    arith = true;
                    bufinfo = self.parse_sof(&mut br)?;
                },
                0xFFC4 => { //huff table
                    validate!(!arith);
                    let len             = u64::from(br.read_u16be()?);
                    validate!(len > 2);
                    let end = br.tell() + len - 2;
                    let mut lens = [0; 16];
                    let mut syms = [0; 256];
                    while br.tell() < end {
                        let tctn        = br.read_byte()? as usize;
                        let tclass = tctn >> 4;
                        validate!(tclass < 2);
                        let id = tctn & 0xF;
                        validate!(id < 4);
                                          br.read_buf(&mut lens)?;
                        let mut tot_len = 0usize;
                        for &el in lens.iter() {
                            tot_len += usize::from(el);
                        }
                        validate!(tot_len > 0 && tot_len <= 256);
                                          br.read_buf(&mut syms[..tot_len])?;
                        self.jpeg.cb.codebook[tclass][id] = Some(JCodebooks::generate_cb(&lens, &syms[..tot_len])?);
                    }
                    validate!(br.tell() == end);
                },
                0xFFCC => { // arith coding conditioning
                    return Err(DecoderError::NotImplemented);
                }
                0xFFD0..=0xFFD7 => return Err(DecoderError::NotImplemented),
                0xFFD9 => break,
                0xFFDA => { //start of scan
                    let len             = br.read_u16be()? as usize;
                    let ns              = br.read_byte()? as usize;
                    validate!(len == ns * 2 + 6);
                    let mut ci = [ComponentInfo::default(); MAX_CHROMATONS];
                    for info in ci[..ns].iter_mut() {
                        let id          = br.read_byte()?;
                        let mut found = false;
                        for (i, &c_id) in self.jpeg.comp_id.iter().enumerate() {
                            if c_id == id {
                                info.component_id = i;
                                found = true;
                                break;
                            }
                        }
                        validate!(found);
                        let tdta        = br.read_byte()? as usize;
                        let dc_id = tdta >> 4;
                        validate!(dc_id < 4);
                        if self.jpeg.cb.codebook[0][dc_id].is_none() {
                            validate!(dc_id < 2);
                            self.jpeg.cb.codebook[0][dc_id] = Some(JCodebooks::build_default_cb(true, dc_id)?);
                        }
                        let ac_id = tdta & 0xF;
                        validate!(ac_id < 4);
                        if self.jpeg.cb.codebook[1][ac_id].is_none() {
                            validate!(ac_id < 2);
                            self.jpeg.cb.codebook[1][ac_id] = Some(JCodebooks::build_default_cb(false, ac_id)?);
                        }
                        info.dc_table_id = dc_id;
                        info.ac_table_id = ac_id;
                    }
                    let ss              = br.read_byte()? as usize;
                    let se              = br.read_byte()? as usize;
                    let ahal            = br.read_byte()?;
                    let ah = ahal >> 4;
                    let al = ahal & 0xF;
                    match jtype {
                        JPEGType::Baseline | JPEGType::Extended => {
                            if arith {
                                println!("arithmetic coding!");
                                return Err(DecoderError::NotImplemented);
                            }
                            validate!(ss == 0 && se == 63);
                            validate!(ah == 0 && al == 0);
                            if let Some(buf) = bufinfo.get_vbuf() {
                                let max_size = src.len() - (br.tell() as usize);
                                self.buf.clear();
                                self.buf.reserve(max_size);
                                loop {
                                    let b   = br.read_byte()?;
                                    if b != 0xFF {
                                        self.buf.push(b);
                                    } else {
                                        let b2 = br.read_byte()?;
                                        if b2 == 0 {
                                            self.buf.push(b);
                                        } else {
                                                 br.seek(std::io::SeekFrom::Current(-2))?;
                                            break;
                                        }
                                    }
                                }

                                let mut data = Vec::new();
                                std::mem::swap(&mut self.buf, &mut data);
                                let ret = self.jpeg.decode_scan(jtype, &data, buf, &ci[..ns], ss, se, None);
                                std::mem::swap(&mut self.buf, &mut data);
                                ret?;
                            } else { unreachable!(); }
                        },
                        JPEGType::Progressive => {
                            validate!(ss < 64 && se < 64 && se >= ss);
                            validate!(ah < 14 && al < 14);
                            println!("Progressive JPEG");
                            return Err(DecoderError::NotImplemented);
                        },
                        JPEGType::Lossless => {
                            validate!(ss >= 1 && ss < 8 && se == 0);
                            validate!(ah == 0);
                            println!("LJPEG");
                            return Err(DecoderError::NotImplemented);
                        },
                        _ => return Err(DecoderError::NotImplemented),
                    };
                    let tag             = br.peek_u16be()?;
                    validate!((tag == 0xFFC4) || (tag >= 0xFFD0 && tag <= 0xFFD7) || (tag == 0xFFD9));
                },
                0xFFDB => { //quant tables
                    let mut len         = br.read_u16be()? as usize;
                    validate!(len >= 64 + 3);
                    len -= 2;
                    while len > 0 {
                        let pt          = br.read_byte()?;
                        let precision = pt >> 4;
                        validate!(precision < 2);
                        let id = (pt & 0xF) as usize;
                        validate!(id < 4);
                        let qsize = if precision == 0 { 64 } else { 64 * 2 } + 1;
                        validate!(len >= qsize);
                        if precision == 0 {
                            for el in self.jpeg.quant[id].iter_mut() {
                                *el     = i16::from(br.read_byte()?);
                            }
                        } else {
                            for el in self.jpeg.quant[id].iter_mut() {
                                *el     = br.read_u16be()? as i16;
                            }
                        }
                        len -= qsize;
                    }
                },
                0xFFDC => { //number of lines
                    return Err(DecoderError::NotImplemented);
                },
                0xFFDD => {
                    let len             = br.read_u16be()?;
                    validate!(len == 4);
                    let ri              = br.read_u16be()?;
                    if ri != 0 {
                        println!("restart interval {}", ri);
                        return Err(DecoderError::NotImplemented);
                    }
                },
                0xFFDE => return Err(DecoderError::NotImplemented),
                0xFFDF => return Err(DecoderError::NotImplemented),
                0xFFE0..=0xFFEF => { // application data
                    let len             = br.read_u16be()? as usize;
                    validate!(len >= 2);
                                          br.read_skip(len - 2)?;
                },
                0xFFF0..=0xFFF6 => return Err(DecoderError::NotImplemented),
                0xFFF7 => {
                    //jtype = JPEGType::JPEGLS;
                    //arith = false;
                    println!("JPEG-LS");
                    return Err(DecoderError::NotImplemented);
                },
                0xFFF8 => return Err(DecoderError::NotImplemented), //JPEG-LS parameters
                0xFFF9..=0xFFFD => return Err(DecoderError::NotImplemented),
                0xFFFE => { //comment
                    let len             = br.read_u16be()? as usize;
                    validate!(len >= 2);
                                          br.read_skip(len - 2)?;
                },
                0xFF01 => return Err(DecoderError::NotImplemented),
                0xFF02..=0xFFBF => return Err(DecoderError::NotImplemented),
                _ => return Err(DecoderError::InvalidData),
            };
        }
        validate!(jtype != JPEGType::None);

        if let NABufferType::None = bufinfo {
            return Err(DecoderError::InvalidData);
        }

        let mut frm = NAFrame::new_from_pkt(pkt, self.info.clone(), bufinfo);
        frm.set_keyframe(true);
        frm.set_frame_type(FrameType::I);
        Ok(frm.into_ref())
    }
    fn flush(&mut self) {
    }
}

impl NAOptionHandler for DecoderWrapper {
    fn get_supported_options(&self) -> &[NAOptionDefinition] { &[] }
    fn set_options(&mut self, _options: &[NAOption]) { }
    fn query_option_value(&self, _name: &str) -> Option<NAValue> { None }
}

pub fn get_decoder() -> Box<dyn NADecoder + Send> {
    Box::new(DecoderWrapper::new())
}

#[cfg(test)]
mod test {
    use nihav_core::codecs::RegisteredDecoders;
    use nihav_core::demuxers::RegisteredDemuxers;
    use nihav_codec_support::test::dec_video::*;
    use crate::generic_register_all_decoders;
    use crate::generic_register_all_demuxers;
    #[test]
    fn test_jpeg() {
        let mut dmx_reg = RegisteredDemuxers::new();
        generic_register_all_demuxers(&mut dmx_reg);
        let mut dec_reg = RegisteredDecoders::new();
        generic_register_all_decoders(&mut dec_reg);
        // sample: self-created with avconv
        test_decoding("avi", "jpeg", "assets/Misc/mjpeg.avi", Some(1), &dmx_reg,
                     &dec_reg, ExpectedTestResult::MD5Frames(vec![
                            [0xe07f7128, 0x8c55eb5d, 0x03bfdee5, 0x358b24a4],
                            [0xd3ec3f92, 0x1664c56d, 0xfc049754, 0xf65165b9]]));
    }
}
