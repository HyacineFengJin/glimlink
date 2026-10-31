use nihav_core::demuxers::*;

#[derive(Default)]
pub struct TimeSearcher {
    pub idx:        usize,
    pub base:       u64,
    pub sbase:      u32,
    pub cur_len:    u32,
    pub cur_mul:    u32,
}

impl TimeSearcher {
    pub fn new() -> Self { Self::default() }
    pub fn reset(&mut self) {
        *self = Self::default();
    }
    pub fn map_time(&mut self, sample: u32, tts: &[(u32, u32)]) -> u64 {
        if tts.is_empty() {
            u64::from(sample)
        } else if sample >= self.sbase {
            let mut sample = sample - self.sbase;
            if self.idx == 0 {
                let (cur_len, cur_mul) = tts[0];
                self.cur_len = cur_len;
                self.cur_mul = cur_mul;
                self.idx += 1;
            }
            while self.idx < tts.len() && sample > self.cur_len {
                sample -= self.cur_len;
                self.sbase += self.cur_len;
                self.base += u64::from(self.cur_len) * u64::from(self.cur_mul);
                self.cur_len = tts[self.idx].0;
                self.cur_mul = tts[self.idx].1;
                self.idx += 1;
            }
            self.base + u64::from(sample) * u64::from(self.cur_mul)
        } else {
            self.reset();
            self.map_time(sample, tts)
        }
    }
}

#[repr(C)]
#[derive(Clone,Copy,Default)]
pub struct RLEPair<T>(pub u32, pub T);

#[derive(Default)]
pub struct RLESearcher<T> {
    pub array:      Vec<RLEPair<T>>,
    pub idx:        usize,
    pub start:      u64,
    pub next:       u64,
}

impl<T:Default+Copy> RLESearcher<T> {
    //pub fn new() -> Self { Self::default() }
    pub fn resize(&mut self, size: usize) {
        self.array.clear();
        self.array.reserve(size);
    }
    pub fn reserve(&mut self, size: usize) {
        self.array.reserve(size);
    }
    pub fn add(&mut self, len: u32, val: T) {
        self.array.push(RLEPair(len, val));
    }
    pub fn reset(&mut self) {
        self.start = 0;
        if !self.array.is_empty() {
            self.idx = 0;
            self.next = u64::from(self.array[0].0);
        } else {
            self.idx = self.array.len();
            self.next = 0;
        }
    }
    pub fn map(&mut self, sample: u64) -> Option<T> {
        if sample < self.start {
            self.reset();
        }
        if self.idx < self.array.len() {
            if sample < self.next {
                Some(self.array[self.idx].1)
            } else {
                while (self.idx < self.array.len()) && (sample >= self.next) {
                    self.start = self.next;
                    self.idx += 1;
                    if self.idx < self.array.len() {
                        self.next += u64::from(self.array[self.idx].0);
                    }
                }
                if self.idx < self.array.len() {
                    Some(self.array[self.idx].1)
                } else {
                    None
                }
            }
        } else {
            None
        }
    }
}

#[derive(Default,Debug,Clone,Copy,PartialEq)]
pub enum PacketMode {
    #[default]
    Unknown,
    Complex,
    OneToOne,
    RawAudio,
    AudioCBR,
    AudioVBR,
}

impl PacketMode {
    pub fn is_audio(self) -> bool {
        matches!(self, PacketMode::RawAudio | PacketMode::AudioCBR | PacketMode::AudioVBR)
    }
}

#[derive(Default)]
pub struct QTPacketDemuxer {
    pub mode:           PacketMode,
    pub is_data_stream: bool,

    pub tb_num:         u32,
    pub tb_den:         u32,
    pub cur_chunk:      usize,
    pub cur_sample:     usize,
    pub cur_ts:         Option<u64>,
    pub samples_left:   usize,
    pub last_offset:    u64,
    pub duration:       u32,
    pub channels:       usize,
    pub bits:           usize,
    pub bsize:          usize,
    pub fcc:            [u8; 4],

    pub keyframes:      Vec<u32>,
    pub chunk_sizes:    Vec<u32>,
    pub chunk_offsets:  Vec<u64>,
    pub time_to_sample: Vec<(u32, u32)>,
    pub sample_map:     Vec<(u32, u32)>,
    pub desc_map:       Vec<(u32, u32)>,
    pub sample_size:    u32,
    pub frame_samples:  usize,
    pub ctts_map:       RLESearcher<u32>,
    pub ctts_version:   u8,
    pub timesearch:     TimeSearcher,

    pub seek_id:        usize,
}

impl QTPacketDemuxer {
    pub fn new() -> Self { Self::default() }
    pub fn is_first(&self) -> bool { self.cur_sample == 0 }
    pub fn fill_seek_index(&mut self, track_no: u32, seek_index: &mut SeekIndex) {
        if !self.keyframes.is_empty() {
            seek_index.mode = SeekIndexMode::Present;
        }
        self.seek_id = seek_index.add_stream(track_no);
        let mut tsearch = TimeSearcher::new();
        for kf_time in self.keyframes.iter() {
            let pts = tsearch.map_time(*kf_time - 1, &self.time_to_sample);
            let time = NATimeInfo::rescale_ts(pts, self.tb_num, self.tb_den, 1, 1000);
            seek_index.add_entry(track_no, SeekEntry { time, pts: u64::from(*kf_time - 1), pos: 0 });
        }
    }
    pub fn decide_mode(&mut self, stream_type: StreamType) {
        if self.mode == PacketMode::Unknown {
            self.mode = if self.chunk_offsets.len() == self.chunk_sizes.len() {
                    PacketMode::OneToOne
                } else {
                    PacketMode::Complex
                };
        }
        if self.mode == PacketMode::AudioCBR && self.chunk_offsets.len() == self.chunk_sizes.len() && self.sample_map.is_empty() {
            self.mode = PacketMode::OneToOne;
        }
        if self.mode == PacketMode::AudioCBR && &self.fcc == b"ima4" && self.bsize == 2 {
            self.mode = PacketMode::RawAudio;
            self.bsize = 34 * self.channels;
        }
        if &self.fcc == b".mp3" {
            self.mode = PacketMode::Complex;
        }
        if self.mode == PacketMode::AudioVBR && self.bsize <= 2 {
            self.mode = PacketMode::Complex;
        }
        self.is_data_stream = stream_type == StreamType::Data;
    }
    pub fn invent_keyframes(&mut self, intraonly: bool) {
        self.keyframes.reserve(self.time_to_sample.len());
        if intraonly && !self.chunk_sizes.is_empty() {
            for samp in 0..self.chunk_sizes.len() {
                self.keyframes.push((samp + 1) as u32);
            }
        } else if self.time_to_sample.len() > 2 {
            let mut abs_csamp = 1u64;
            for &(count, _scount) in self.time_to_sample.iter() {
                let count = u64::from(count);
                self.keyframes.push(abs_csamp as u32);
                abs_csamp += count;
            }
        } else {
            let max_nsamps = self.sample_map.iter().fold(0u32, |mns, &(_sno, ns)| mns.max(ns));
            if max_nsamps >= 3 {
                let nchunks = self.chunk_offsets.len() as u32;
                let mut sample_no = 1;
                let mut cur_chunk = 1;
                let mut cur_ns = 0;
                for &(chunk_no, nsamples) in self.sample_map.iter() {
                    while cur_chunk < chunk_no {
                        self.keyframes.push(sample_no);
                        sample_no += cur_ns;
                        cur_chunk += 1;
                    }
                    cur_ns = nsamples;
                }
                while cur_chunk < nchunks {
                    self.keyframes.push(sample_no);
                    sample_no += cur_ns;
                    cur_chunk += 1;
                }
            }
        }
    }
    fn calculate_chunk_size(&self, nsamp: usize) -> usize {
        if nsamp == 0 {
            self.bsize
        } else {
            match &self.fcc {
                b"NONE" | b"raw " | b"twos" | b"sowt" | &[0, 0, 0, 0] => {
                    (nsamp * self.bits * self.channels + 7) >> 3
                },
                b"ima4" => {
                    let nblocks = (nsamp + 63) >> 6;
                    nblocks * 34 * self.channels
                },
                b"MAC3" => {
                    (nsamp + 5) / 6 * 2 * self.channels
                },
                b"MAC6" => {
                    (nsamp + 5) / 6 * self.channels
                },
                b"in24" => nsamp * 3 * self.channels,
                b"in32" | b"fl32" => nsamp * 4 * self.channels,
                b"fl64" => nsamp * 8 * self.channels,
                b"ulaw" | b"alaw" => nsamp,
                b"ms\x00\x02" => { //MS ADPCM
                    ((nsamp - 1) / 2 + 7) * self.channels
                },
                b"ms\x00\x11" => { //IMA ADPCM
                    (nsamp / 2 + 4) * self.channels
                },
                _ => self.bsize,
            }
        }
    }
    pub fn reset(&mut self) {
        self.cur_chunk = self.chunk_offsets.len();
        self.samples_left = 0;
        self.cur_sample = self.chunk_sizes.len();
    }
    fn next_sample(&mut self) -> Option<bool> {
        let chunk_start = self.samples_left == 0;
        if self.samples_left == 0 {
            if self.cur_chunk >= self.chunk_offsets.len() {
                return None;
            }
            for (idx, samples) in self.sample_map.iter() {
                if *idx as usize <= self.cur_chunk + 1 {
                    self.samples_left = *samples as usize;
                } else {
                    break;
                }
            }
            self.last_offset = self.chunk_offsets[self.cur_chunk];
            self.cur_chunk += 1;
        }
        Some(chunk_start)
    }
    pub fn get_next_chunk(&mut self) -> Option<(NATimeInfo, u64, usize, bool)> {
        let dts_val = self.timesearch.map_time(self.cur_sample as u32, &self.time_to_sample);
        let pts = if let Some(dts_corr) = self.ctts_map.map(self.cur_sample as u64) {
                let pts = match self.ctts_version {
                        0 => dts_val.wrapping_add(u64::from(dts_corr)),
                        1 => dts_val.wrapping_add(i64::from(dts_corr as i32) as u64),
                        _ => unimplemented!(),
                    };
                if (pts as i64) < 0 {
                    None
                } else {
                    Some(pts)
                }
            } else {
                Some(dts_val)
            };
        let ts = NATimeInfo::new(pts, Some(dts_val), None, self.tb_num, self.tb_den);
        match self.mode {
            PacketMode::OneToOne => {
                if self.cur_sample >= self.chunk_sizes.len() {
                    return None;
                }
                let offset = self.chunk_offsets[self.cur_sample];
                let size   = self.chunk_sizes[self.cur_sample] as usize;
                self.cur_sample += 1;
                let is_kf = self.keyframes.contains(&(self.cur_sample as u32));
                Some((ts, offset, size, is_kf))
            },
            PacketMode::Complex => {
                let chunk_start = self.next_sample()?;
                let offset = self.last_offset;
                let size = self.get_size(self.cur_sample);
                self.last_offset += size as u64;
                self.cur_sample += 1;
                let is_kf = chunk_start && self.keyframes.contains(&(self.cur_sample as u32));
                self.samples_left -= 1;
                Some((ts, offset, size, is_kf))
            },
            PacketMode::AudioCBR | PacketMode::AudioVBR => {
                let chunk_start = self.next_sample()?;
                let offset = self.last_offset;
                let size = self.get_size(self.cur_sample);
                self.last_offset += size as u64;
                if self.frame_samples != 0 && self.bsize != 0 {
                    let nblocks = size / self.bsize;
                    if nblocks > 0 {
                        let consumed = (nblocks * self.frame_samples).min(self.samples_left);
                        self.cur_sample += 1;
                        self.samples_left -= consumed;
                    } else {
                        self.cur_sample += 1;
                        self.samples_left = 0;
                    }
                } else {
                    self.cur_sample += 1;
                    self.samples_left -= 1;
                }
                Some((ts, offset, size, chunk_start))
            },
            PacketMode::RawAudio => {
                self.next_sample()?;
                const BLOCK_SAMPLES: usize = 1024 * 6; // should be multiple of 64 and 6 to fit both IMA ADPCM and MACE 6:1 blocks
                let samples = self.samples_left.min(BLOCK_SAMPLES);
                let cur_size = self.calculate_chunk_size(samples);
                let offset = self.last_offset;
                self.cur_sample   += samples;
                self.samples_left -= samples;
                self.last_offset += cur_size as u64;
                Some((ts, offset, cur_size, true))
            },
            _ => unreachable!(),
        }
    }
    fn get_size(&self, sample_no: usize) -> usize {
        if !self.chunk_sizes.is_empty() {
            self.chunk_sizes[sample_no] as usize
        } else if !self.mode.is_audio() && self.sample_map.len() <= 1 && self.bsize > 0 {
            self.bsize
        } else if !self.sample_map.is_empty() && self.sample_size == 0 {
            let mut nsamp = 0;
            for (idx, samples) in self.sample_map.iter() {
                if *idx as usize <= self.cur_chunk {
                    nsamp = *samples;
                } else {
                    break;
                }
            }
            self.calculate_chunk_size(nsamp as usize)
        } else {
            self.bsize
        }
    }
    pub fn seek(&mut self, pts: u64, tpoint: NATimePoint, forced: bool) -> DemuxerResult<u64> {
        let tgt_pts = match tpoint {
            NATimePoint::Milliseconds(ms) => NATimeInfo::rescale_ts(ms, 1, 1000, self.tb_num, self.tb_den),
            NATimePoint::PTS(ts) => ts,
            _ => 0,
        };
        self.cur_sample = pts as usize;
        self.samples_left = 0;
        self.cur_ts = None;
        if forced && pts.abs_diff(tgt_pts) > 5 * u64::from(self.tb_den) {
            self.cur_sample = tgt_pts as usize;
        }
        match self.mode {
            PacketMode::RawAudio => {
                if self.frame_samples != 0 {
                    let mut apos = 0;
                    self.cur_sample = 0;
                    self.cur_chunk = 0;
                    let mut cmap = self.sample_map.iter();
                    let mut cur_samps = 0;
                    let (mut next_idx, mut next_samples) = cmap.next().unwrap();
                    loop {
                        if self.cur_chunk + 1 == next_idx as usize {
                            self.samples_left = cur_samps;
                            cur_samps = next_samples as usize;
                            if let Some((new_idx, new_samples)) = cmap.next() {
                                next_idx = *new_idx;
                                next_samples = *new_samples;
                            }
                        }
                        apos += (cur_samps / self.frame_samples) as u64;
                        if apos > tgt_pts && (cur_samps == self.frame_samples || apos > tgt_pts + 1) {
                            if self.cur_chunk >= self.chunk_offsets.len() {
                                return Err(DemuxerError::SeekError);
                            }
                            self.last_offset = self.chunk_offsets[self.cur_chunk];
                            break;
                        }
                        self.cur_chunk += 1;
                    }
                    self.samples_left = cur_samps;
                    self.cur_chunk += 1;
                } else {
                    self.cur_sample = tgt_pts as usize;
                    let mut csamp = 0;
                    self.cur_chunk = 0;
                    let mut cmap = self.sample_map.iter();
                    let mut cur_samps = 0;
                    let (mut next_idx, mut next_samples) = cmap.next().unwrap();
                    loop {
                        if self.cur_chunk + 1 == next_idx as usize {
                            self.samples_left = cur_samps;
                            cur_samps = next_samples as usize;
                            if let Some((new_idx, new_samples)) = cmap.next() {
                                next_idx = *new_idx;
                                next_samples = *new_samples;
                            }
                        }
                        csamp += cur_samps;
                        if csamp > self.cur_sample {
                            if self.cur_chunk >= self.chunk_offsets.len() {
                                return Err(DemuxerError::SeekError);
                            }
                            self.last_offset = self.chunk_offsets[self.cur_chunk];
                            break;
                        }
                        self.cur_chunk += 1;
                    }
                    self.samples_left = csamp - self.cur_sample;
                    self.cur_chunk += 1;
                }
            },
            PacketMode::AudioCBR | PacketMode::AudioVBR => {
                if self.chunk_offsets.len() == self.chunk_sizes.len() && self.duration != 0 {
                    let new_sample = (self.chunk_sizes.len() as u64 * tgt_pts / u64::from(self.duration)) as usize;
                    self.cur_sample = new_sample;
                    self.cur_chunk = self.cur_sample;
                } else {
                    if !self.time_to_sample.is_empty() {
                        let mut remaining = tgt_pts;
                        let mut abs_csamp = 0;
                        for &(count, scount) in self.time_to_sample.iter() {
                            let count = u64::from(count);
                            let scount = u64::from(scount);
                            let nblk = remaining / scount;
                            if nblk < count {
                                abs_csamp += nblk;
                                break;
                            }
                            remaining -= count * scount;
                            abs_csamp += count;
                        }
                        self.cur_sample = abs_csamp as usize;
                    } else {
                        self.cur_sample = tgt_pts as usize;
                    }
                    let tgt_sample = self.cur_sample;
                    let mut csamp = 0;
                    self.cur_chunk = 0;
                    let mut cmap = self.sample_map.iter();
                    let mut cur_samps = 0;
                    let (mut next_idx, mut next_samples) = cmap.next().unwrap();
                    next_samples /= self.frame_samples.max(1) as u32;
                    loop {
                        if self.cur_chunk + 1 == next_idx as usize {
                            self.samples_left = cur_samps;
                            cur_samps = next_samples as usize;
                            if let Some((new_idx, new_samples)) = cmap.next() {
                                next_idx = *new_idx;
                                next_samples = *new_samples;
                                next_samples /= self.frame_samples.max(1) as u32;
                            }
                        }
                        csamp += cur_samps;
                        if csamp > self.cur_sample {
                            if self.cur_chunk >= self.chunk_offsets.len() {
                                self.cur_sample = csamp - cur_samps;
                                self.samples_left = 0;
                                self.cur_sample = csamp;
                                return Err(DemuxerError::SeekError);
                            }
                            self.last_offset = self.chunk_offsets[self.cur_chunk];
                            break;
                        }
                        self.cur_chunk += 1;
                    }
                    self.cur_sample = csamp - cur_samps;
                    self.samples_left = cur_samps;
                    self.last_offset = self.chunk_offsets[self.cur_chunk];
                    self.cur_chunk += 1;

                    // try to refine sample position
                    if self.chunk_sizes.len() > self.chunk_offsets.len() {
                        for i in self.cur_sample..tgt_sample {
                            self.cur_sample   += 1;
                            self.samples_left -= 1;
                            self.last_offset  += u64::from(self.chunk_sizes[i]);
                        }
                    }
                }
            },
            PacketMode::OneToOne => {}, // nothing to do
            PacketMode::Complex => {
                if self.chunk_offsets.len() != self.chunk_sizes.len() && !self.sample_map.is_empty() {
                    let mut csamp = 0;
                    self.cur_chunk = 0;
                    let mut cmap = self.sample_map.iter();
                    let mut cur_samps = 0;
                    let (mut next_idx, mut next_samples) = cmap.next().unwrap();
                    loop {
                        if self.cur_chunk + 1 == next_idx as usize {
                            self.samples_left = cur_samps;
                            cur_samps = next_samples as usize;
                            if let Some((new_idx, new_samples)) = cmap.next() {
                                next_idx = *new_idx;
                                next_samples = *new_samples;
                            }
                        }
                        csamp += cur_samps;
                        if csamp >= self.cur_sample {
                            if self.cur_chunk >= self.chunk_offsets.len() {
                                if self.is_data_stream {
                                    self.cur_chunk = self.chunk_offsets.len();
                                    self.samples_left = 0;
                                    return Ok(NATimeInfo::rescale_ts(tgt_pts, self.tb_num, self.tb_den, 1, 1000));
                                }
                                return Err(DemuxerError::SeekError);
                            }
                            self.last_offset = self.chunk_offsets[self.cur_chunk];
                            break;
                        }
                        self.cur_chunk += 1;
                    }
                    csamp -= cur_samps;
                    for sample_no in csamp..self.cur_sample {
                        self.last_offset += self.get_size(sample_no) as u64;
                    }
                    self.samples_left = csamp + cur_samps - self.cur_sample;
                    self.cur_chunk += 1;
                }
            },
            _ => unreachable!(),
        }
        let cur_pts = self.timesearch.map_time(self.cur_sample as u32, &self.time_to_sample);
        let cur_time = NATimeInfo::rescale_ts(cur_pts, self.tb_num, self.tb_den, 1, 1000);
        Ok(cur_time)
    }
}
