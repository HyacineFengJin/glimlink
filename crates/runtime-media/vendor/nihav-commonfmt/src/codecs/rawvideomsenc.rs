use nihav_core::codecs::*;
use nihav_core::io::byteio::*;

pub const BGR24_FORMAT: NAPixelFormaton = NAPixelFormaton { model: ColourModel::RGB(RGBSubmodel::RGB), components: 3,
                                        comp_info: [
                                            Some(NAPixelChromaton{ h_ss: 0, v_ss: 0, packed: true, depth: 8, shift: 0, comp_offs: 2, next_elem: 3 }),
                                            Some(NAPixelChromaton{ h_ss: 0, v_ss: 0, packed: true, depth: 8, shift: 0, comp_offs: 1, next_elem: 3 }),
                                            Some(NAPixelChromaton{ h_ss: 0, v_ss: 0, packed: true, depth: 8, shift: 0, comp_offs: 0, next_elem: 3 }),
                                            None, None],
                                        elem_size: 3, be: false, alpha: false, palette: false };

pub const BGR32_FORMAT: NAPixelFormaton = NAPixelFormaton { model: ColourModel::RGB(RGBSubmodel::RGB), components: 4,
                                        comp_info: [
                                            Some(NAPixelChromaton{ h_ss: 0, v_ss: 0, packed: true, depth: 8, shift: 0, comp_offs: 2, next_elem: 4 }),
                                            Some(NAPixelChromaton{ h_ss: 0, v_ss: 0, packed: true, depth: 8, shift: 0, comp_offs: 1, next_elem: 4 }),
                                            Some(NAPixelChromaton{ h_ss: 0, v_ss: 0, packed: true, depth: 8, shift: 0, comp_offs: 0, next_elem: 4 }),
                                            Some(NAPixelChromaton{ h_ss: 0, v_ss: 0, packed: true, depth: 8, shift: 0, comp_offs: 3, next_elem: 4 }),
                                            None],
                                        elem_size: 4, be: false, alpha: true, palette: false };

struct RawEncoder {
    stream: Option<NAStreamRef>,
    pkt:    Option<NAPacket>,
    pal:    [u8; 768],
    dpal:   Arc<[u8; 1024]>,
}

impl RawEncoder {
    fn new() -> Self {
        Self {
            stream: None,
            pkt:    None,
            pal:    [0; 768],
            dpal:   Arc::new([0; 1024]),
        }
    }
}

impl NAEncoder for RawEncoder {
    fn negotiate_format(&self, encinfo: &EncodeParameters) -> EncoderResult<EncodeParameters> {
        match encinfo.format {
            NACodecTypeInfo::None => {
                Ok(EncodeParameters {
                    format: NACodecTypeInfo::Video(NAVideoInfo::new(0, 0, false, YUV420_FORMAT)),
                    ..Default::default()
                })
            },
            NACodecTypeInfo::Video(_) => {
                let mut new_info = *encinfo;
                if let NACodecTypeInfo::Video(ref mut vinfo) = new_info.format {
                    if vinfo.format.model.is_rgb() {
                        let depth = if vinfo.format.is_paletted() { 8 } else { vinfo.format.get_total_depth() } as usize;
                        vinfo.format = match depth {
                                8 => PAL8_FORMAT,
                                15 | 16 => RGB555_FORMAT,
                                24 => BGR24_FORMAT,
                                _ if vinfo.format.has_alpha() => BGR32_FORMAT,
                                _ => BGR24_FORMAT,
                            };
                    } else {
                        vinfo.format = BGR24_FORMAT;
                    }
                }
                Ok(new_info)
            },
            NACodecTypeInfo::Audio(_) => Err(EncoderError::FormatError),
        }
    }
    fn get_capabilities(&self) -> u64 { ENC_CAPS_CBR }
    fn init(&mut self, stream_id: u32, encinfo: EncodeParameters) -> EncoderResult<NAStreamRef> {
        match encinfo.format {
            NACodecTypeInfo::None => Err(EncoderError::FormatError),
            NACodecTypeInfo::Audio(_) => Err(EncoderError::FormatError),
            NACodecTypeInfo::Video(_) => {
                let info = NACodecInfo::new("rawvideo-ms", encinfo.format, None);
                let mut stream = NAStream::new(StreamType::Video, stream_id, info, encinfo.tb_num, encinfo.tb_den, 0);
                stream.set_num(stream_id as usize);
                let stream = stream.into_ref();
                self.stream = Some(stream.clone());
                Ok(stream)
            }
        }
    }
    fn encode(&mut self, frm: &NAFrame) -> EncoderResult<()> {
        let buf = frm.get_buffer();
        let mut dbuf;
        let mut pal = None;
        match buf {
            NABufferType::Video(ref vbuf) | NABufferType::VideoPacked(ref vbuf) => {
                let vinfo = vbuf.get_info();
                let mut depth = if vinfo.format.is_paletted() { 8 } else { vinfo.format.get_total_depth() } as usize;
                if depth == 15 {
                    depth = 16;
                }
                if !vinfo.format.model.is_rgb() || !matches!(depth, 8 | 16 | 24 | 32) {
                    return Err(EncoderError::NotImplemented);
                }

                let (width, height) = vbuf.get_dimensions(0);
                let row_size = width * depth / 8;
                let pad_len = if (row_size & 3) != 0 { 4 - (row_size & 3) } else { 0 };
                let pad = &[0; 4][..pad_len];

                let src = vbuf.get_data();
                dbuf = Vec::with_capacity(height * (row_size + pad_len));
                let sstride = vbuf.get_stride(0).max(row_size);
                if vinfo.is_flipped() {
                    for line in src.chunks_exact(sstride).take(height) {
                        dbuf.extend_from_slice(&line[..row_size]);
                        dbuf.extend_from_slice(pad);
                    }
                } else {
                    for line in src.chunks_exact(sstride).take(height).rev() {
                        dbuf.extend_from_slice(&line[..row_size]);
                        dbuf.extend_from_slice(pad);
                    }
                }
                if vinfo.format.is_paletted() {
                    let pal_off = vbuf.get_offset(1);
                    let src_pal = &src[pal_off..][..768];
                    pal = if src_pal == self.pal {
                            Some(NASideData::Palette(false, self.dpal.clone()))
                        } else {
                            let mut pal32 = [0; 1024];
                            self.pal.copy_from_slice(src_pal);
                            for (dclr, sclr) in pal32.chunks_exact_mut(4)
                                    .zip(src_pal.chunks_exact(3)) {
                                dclr[..3].copy_from_slice(sclr);
                            }
                            self.dpal = Arc::new(pal32);
                            Some(NASideData::Palette(true, self.dpal.clone()))
                        };
                }
            },
            NABufferType::Video16(ref vbuf) => {
                let vinfo = vbuf.get_info();
                let (width, height) = vbuf.get_dimensions(0);
                let row_size = width * 2;
                let pad_len = if (row_size & 3) != 0 { 4 - (row_size & 3) } else { 0 };

                let src = vbuf.get_data();
                dbuf = vec![0; height * (row_size + pad_len)];
                let sstride = vbuf.get_stride(0);
                if vinfo.is_flipped() {
                    for (dline, sline) in dbuf.chunks_exact_mut(row_size + pad_len)
                            .zip(src.chunks_exact(sstride).take(height)) {
                        for (dst, &pix) in dline.chunks_exact_mut(2).zip(sline[..width].iter()) {
                            let _ = write_u16le(dst, pix);
                        }
                    }
                } else {
                    for (dline, sline) in dbuf.chunks_exact_mut(row_size + pad_len)
                            .zip(src.chunks_exact(sstride).take(height).rev()) {
                        for (dst, &pix) in dline.chunks_exact_mut(2).zip(sline[..width].iter()) {
                            let _ = write_u16le(dst, pix);
                        }
                    }
                }
            },
            NABufferType::Video32(ref _vbuf) => return Err(EncoderError::NotImplemented),
            NABufferType::None => {
                self.pkt = None;
                return Ok(());
            },
            _ => return Err(EncoderError::FormatError),
        };
        let mut pkt = NAPacket::new(self.stream.clone().unwrap(), frm.ts, true, dbuf);
        if let Some(pal_sd) = pal {
            pkt.add_side_data(pal_sd);
        }
        self.pkt = Some(pkt);
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

impl NAOptionHandler for RawEncoder {
    fn get_supported_options(&self) -> &[NAOptionDefinition] { &[] }
    fn set_options(&mut self, _options: &[NAOption]) { }
    fn query_option_value(&self, _name: &str) -> Option<NAValue> { None }
}

pub fn get_encoder() -> Box<dyn NAEncoder + Send> {
    Box::new(RawEncoder::new())
}
