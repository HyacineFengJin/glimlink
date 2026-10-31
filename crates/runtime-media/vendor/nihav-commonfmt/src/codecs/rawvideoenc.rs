use nihav_core::codecs::*;
use nihav_core::io::byteio::*;

struct RawEncoder {
    stream: Option<NAStreamRef>,
    pkt:    Option<NAPacket>,
    vinfo:  NAVideoInfo,
    lpal:   Arc<[u8; 1024]>,
}

impl RawEncoder {
    fn new() -> Self {
        Self {
            stream: None,
            pkt:    None,
            vinfo:  NAVideoInfo::new(0, 0, false, YUV420_FORMAT),
            lpal:   Arc::new([0; 1024]),
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
                Ok(*encinfo)
            },
            NACodecTypeInfo::Audio(_) => Err(EncoderError::FormatError),
        }
    }
    fn get_capabilities(&self) -> u64 { ENC_CAPS_CBR }
    fn init(&mut self, stream_id: u32, encinfo: EncodeParameters) -> EncoderResult<NAStreamRef> {
        match encinfo.format {
            NACodecTypeInfo::None => Err(EncoderError::FormatError),
            NACodecTypeInfo::Audio(_) => Err(EncoderError::FormatError),
            NACodecTypeInfo::Video(vinfo) => {
                self.vinfo = vinfo;
                let info = NACodecInfo::new("rawvideo", encinfo.format, None);
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
        if let Some(vinfo) = buf.get_video_info() {
            if vinfo != self.vinfo {
                println!("Input format differs from the initial one");
                return Err(EncoderError::FormatError);
            }
        }
        match buf {
            NABufferType::Video(ref vbuf) | NABufferType::VideoPacked(ref vbuf) => {
                let vinfo = vbuf.get_info();
                let src = vbuf.get_data();
                match vinfo.format.model {
                    ColourModel::RGB(_) if vinfo.format.is_paletted() => {
                        let (width, height) = vbuf.get_dimensions(0);
                        let sstride = vbuf.get_stride(0);
                        let dstride = width;
                        dbuf = Vec::with_capacity(dstride * height);
                        match vinfo.bits.min(8) {
                            8 => {
                                if !vinfo.flipped {
                                    for line in src.chunks(sstride).take(height) {
                                        dbuf.extend_from_slice(&line[..dstride]);
                                    }
                                } else {
                                    for line in src.chunks(sstride).take(height).rev() {
                                        dbuf.extend_from_slice(&line[..dstride]);
                                    }
                                }
                            },
                            4 => {
                                if (vinfo.width & 1) != 0 {
                                    return Err(EncoderError::NotImplemented);
                                }
                                if !vinfo.flipped {
                                    for line in src.chunks(sstride).take(height) {
                                        for ppair in line.chunks_exact(2) {
                                            dbuf.push((ppair[0] << 4) | ppair[1]);
                                        }
                                    }
                                } else {
                                    for line in src.chunks(sstride).take(height).rev() {
                                        for ppair in line.chunks_exact(2) {
                                            dbuf.push((ppair[0] << 4) | ppair[1]);
                                        }
                                    }
                                }
                            },
                            2 => {
                                if (vinfo.width & 3) != 0 {
                                    return Err(EncoderError::NotImplemented);
                                }
                                if !vinfo.flipped {
                                    for line in src.chunks(sstride).take(height) {
                                        for quad in line.chunks_exact(4) {
                                            dbuf.push((quad[0] << 6) | (quad[1] << 4) | (quad[2] << 2) | quad[3]);
                                        }
                                    }
                                } else {
                                    for line in src.chunks(sstride).take(height).rev() {
                                        for quad in line.chunks_exact(4) {
                                            dbuf.push((quad[0] << 6) | (quad[1] << 4) | (quad[2] << 2) | quad[3]);
                                        }
                                    }
                                }
                            },
                            1 => {
                                if (vinfo.width & 8) != 0 {
                                    return Err(EncoderError::NotImplemented);
                                }
                                if !vinfo.flipped {
                                    for line in src.chunks(sstride).take(height) {
                                        for oct in line.chunks_exact(8) {
                                            let mut bb = 0;
                                            for &b in oct.iter() {
                                                bb = bb * 2 + b;
                                            }
                                            dbuf.push(bb);
                                        }
                                    }
                                } else {
                                    for line in src.chunks(sstride).take(height).rev() {
                                        for oct in line.chunks_exact(8) {
                                            let mut bb = 0;
                                            for &b in oct.iter() {
                                                bb = bb * 2 + b;
                                            }
                                            dbuf.push(bb);
                                        }
                                    }
                                }
                            },
                            _ => return Err(EncoderError::NotImplemented),
                        }
                        let mut pkt = NAPacket::new(self.stream.clone().unwrap(), frm.ts, true, dbuf);
                        let mut npal = [0; 1024];
                        for (dst, src) in npal.chunks_exact_mut(4).zip(src[vbuf.get_offset(1)..].chunks_exact(3)) {
                            dst[..3].copy_from_slice(src);
                        }
                        let new_pal = npal != *self.lpal;
                        if new_pal {
                            self.lpal = Arc::new(npal);
                        }
                        pkt.add_side_data(NASideData::Palette(new_pal, Arc::clone(&self.lpal)));

                        self.pkt = Some(pkt);

                        return Ok(());
                    },
                    ColourModel::RGB(_) => {
                        if vinfo.format.elem_size == 0 {
                            return Err(EncoderError::FormatError);
                        }
                        let (width, height) = vbuf.get_dimensions(0);
                        let sstride = vbuf.get_stride(0);
                        let dstride = width * usize::from(vinfo.format.elem_size);
                        dbuf = Vec::with_capacity(dstride * height);
                        for line in src.chunks(sstride).take(height) {
                            dbuf.extend_from_slice(&line[..dstride]);
                        }
                    },
                    ColourModel::YUV(_) if !vinfo.format.is_unpacked() => { // packed YUV should be fine as is
                        dbuf = src.clone();
                    },
                    ColourModel::YUV(_) => {
                        dbuf = Vec::with_capacity(src.len());
                        for (comp, cinfo) in vinfo.format.comp_info.iter().enumerate() {
                            if cinfo.is_none() {
                                continue;
                            }
                            let (width, height) = vbuf.get_dimensions(comp);
                            let off = vbuf.get_offset(comp);
                            let stride = vbuf.get_stride(comp);

                            for line in src[off..].chunks(stride).take(height) {
                                dbuf.extend_from_slice(&line[..width]);
                            }
                        }
                    },
                    _ => return Err(EncoderError::NotImplemented),
                }
            },
            NABufferType::Video16(ref vbuf16) => {
                let vinfo = vbuf16.get_info();
                let (width, height) = vbuf16.get_dimensions(0);
                let sstride = vbuf16.get_stride(0);
                let src = vbuf16.get_data();
                dbuf = vec![0; width * 2 * height];
                let mut mw = MemoryWriter::new_write(&mut dbuf);
                if vinfo.format.is_be() {
                    for sline in src.chunks(sstride).take(height) {
                        mw.write_u16be_arr(&sline[..width])?;
                    }
                } else {
                    for sline in src.chunks(sstride).take(height) {
                        mw.write_u16le_arr(&sline[..width])?;
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

impl NAOptionHandler for RawEncoder {
    fn get_supported_options(&self) -> &[NAOptionDefinition] { &[] }
    fn set_options(&mut self, _options: &[NAOption]) { }
    fn query_option_value(&self, _name: &str) -> Option<NAValue> { None }
}

pub fn get_encoder() -> Box<dyn NAEncoder + Send> {
    Box::new(RawEncoder::new())
}
