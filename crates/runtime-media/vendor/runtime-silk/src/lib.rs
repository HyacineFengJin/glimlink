// SPDX-License-Identifier: MIT
//! Bounded Skype SILK v3 decoder, independent of Opus SILK. The fixed-point
//! kernel is a pinned C2Rust translation and is compiled entirely by rustc.
//! No build script, native codec, browser, subprocess or account access.
mod kernel;
use std::{
    fmt,
    io::{self, Read},
};
pub const MAX_PACKET_BYTES: usize = 1024;
pub const MAX_FRAMES_PER_PACKET: usize = 5;
pub type Result<T> = std::result::Result<T, Error>;
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Error {
    Header,
    Truncated,
    PacketTooLarge,
    SampleRate,
    Decode(i32),
    FrameLimit,
    Io,
}
impl fmt::Display for Error {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Decode(code) => write!(f, "SILK v3 decoder rejected packet ({code})"),
            _ => write!(f, "SILK v3 {self:?}"),
        }
    }
}
impl std::error::Error for Error {}
/// Owns one stream's state. Raw pointers in the private kernel reference its
/// immutable codebooks; state is never shared with another decoder or exposed.
pub struct Decoder {
    state: Box<kernel::SKP_Silk_decoder_state>,
    control: kernel::SKP_SILK_SDK_DecControlStruct,
    rate: u32,
    failed: bool,
}
impl Decoder {
    pub fn new(rate: u32) -> Result<Self> {
        if !matches!(rate, 8000 | 12000 | 16000 | 24000 | 32000 | 44100 | 48000) {
            return Err(Error::SampleRate);
        }
        // Every field has an all-zero valid representation: integer arrays,
        // nullable raw pointers and Option<extern fn>. SDK initialization sets
        // codec configuration before any packet is accepted.
        let mut state: Box<kernel::SKP_Silk_decoder_state> =
            Box::new(unsafe { std::mem::zeroed() });
        let result = unsafe {
            kernel::SKP_Silk_SDK_InitDecoder(
                (&mut *state as *mut kernel::SKP_Silk_decoder_state).cast(),
            )
        };
        if result != 0 {
            return Err(Error::Decode(result));
        }
        let control = kernel::SKP_SILK_SDK_DecControlStruct {
            API_sampleRate: rate as i32,
            frameSize: 0,
            framesPerPacket: 1,
            moreInternalDecoderFrames: 0,
            inBandFECOffset: 0,
        };
        Ok(Self {
            state,
            control,
            rate,
            failed: false,
        })
    }
    pub fn sample_rate(&self) -> u32 {
        self.rate
    }
    /// One packet contains at most five20ms frames. Empty packets request the
    /// reference decoder's loss concealment; concealed data is not received
    /// speech and the caller can identify it from the empty packet.
    pub fn decode_packet(&mut self, packet: &[u8]) -> Result<Vec<i16>> {
        if self.failed {
            return Err(Error::Decode(-12));
        }
        if packet.len() > MAX_PACKET_BYTES {
            return Err(Error::PacketTooLarge);
        }
        let mut output = Vec::with_capacity(self.rate as usize / 10);
        let loss = packet.is_empty();
        let expected_loss = usize::try_from(self.control.framesPerPacket)
            .ok()
            .filter(|n| (1..=MAX_FRAMES_PER_PACKET).contains(n))
            .unwrap_or(1);
        for frame in 0..MAX_FRAMES_PER_PACKET {
            // The SDK can synthesize one internal24kHz frame before resampling
            // to the API rate, so retain room for960 samples at every API rate.
            let mut buffer = [0i16; 960];
            let mut n = 960i16;
            let code = unsafe {
                kernel::SKP_Silk_SDK_Decode(
                    (&mut *self.state as *mut kernel::SKP_Silk_decoder_state).cast(),
                    &mut self.control,
                    i32::from(loss),
                    packet.as_ptr(),
                    packet.len() as i32,
                    buffer.as_mut_ptr(),
                    &mut n,
                )
            };
            if code != 0 {
                self.failed = true;
                return Err(Error::Decode(code));
            }
            let Some(n) = usize::try_from(n)
                .ok()
                .filter(|n| *n > 0 && *n <= buffer.len() && *n <= self.rate as usize / 50)
            else {
                self.failed = true;
                return Err(Error::FrameLimit);
            };
            output.extend_from_slice(&buffer[..n]);
            if (loss && frame + 1 >= expected_loss)
                || (!loss && self.control.moreInternalDecoderFrames == 0)
            {
                return Ok(output);
            }
        }
        self.failed = true;
        Err(Error::FrameLimit)
    }
}
/// Framing is length-prefixed little-endian SILK v3, optionally preceded by
/// Tencent's0x02 byte. Both EOF and the standard signed-16 negative terminator
/// end a stream; partial lengths/payloads fail instead of producing audio.
pub struct PacketReader<R> {
    reader: R,
    finished: bool,
    tencent: bool,
    packets: u64,
}
impl<R: Read> PacketReader<R> {
    pub fn new(mut reader: R) -> Result<Self> {
        let mut first = [0u8; 1];
        reader.read_exact(&mut first).map_err(read_error)?;
        let tencent = first[0] == 2;
        let mut header = [0u8; 9];
        if tencent {
            reader.read_exact(&mut header).map_err(read_error)?;
        } else {
            header[0] = first[0];
            reader.read_exact(&mut header[1..]).map_err(read_error)?;
        }
        if &header != b"#!SILK_V3" {
            return Err(Error::Header);
        }
        Ok(Self {
            reader,
            finished: false,
            tencent,
            packets: 0,
        })
    }
    pub fn is_tencent(&self) -> bool {
        self.tencent
    }
    pub fn packets_read(&self) -> u64 {
        self.packets
    }
    pub fn next_packet(&mut self) -> Result<Option<Vec<u8>>> {
        if self.finished {
            return Ok(None);
        }
        let mut length = [0u8; 2];
        loop {
            match self.reader.read(&mut length[..1]) {
                Ok(0) => {
                    self.finished = true;
                    return Ok(None);
                }
                Ok(_) => break,
                Err(e) if e.kind() == io::ErrorKind::Interrupted => continue,
                Err(_) => return Err(Error::Io),
            }
        }
        self.reader
            .read_exact(&mut length[1..])
            .map_err(read_error)?;
        let length = i16::from_le_bytes(length);
        if length < 0 {
            self.finished = true;
            return Ok(None);
        }
        let n = length as usize;
        if n > MAX_PACKET_BYTES {
            return Err(Error::PacketTooLarge);
        }
        let mut packet = vec![0u8; n];
        self.reader.read_exact(&mut packet).map_err(read_error)?;
        self.packets = self.packets.checked_add(1).ok_or(Error::FrameLimit)?;
        Ok(Some(packet))
    }
    pub fn into_inner(self) -> R {
        self.reader
    }
}
fn read_error(e: io::Error) -> Error {
    if e.kind() == io::ErrorKind::UnexpectedEof {
        Error::Truncated
    } else {
        Error::Io
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn framing_accepts_both_headers_and_never_accepts_partial_packet() {
        for tencent in [false, true] {
            let mut file = Vec::new();
            if tencent {
                file.push(2)
            }
            file.extend_from_slice(b"#!SILK_V3");
            file.extend_from_slice(&3i16.to_le_bytes());
            file.extend_from_slice(&[1, 2, 3]);
            file.extend_from_slice(&(-1i16).to_le_bytes());
            let mut reader = PacketReader::new(file.as_slice()).unwrap();
            assert_eq!(reader.is_tencent(), tencent);
            assert_eq!(reader.next_packet().unwrap(), Some(vec![1, 2, 3]));
            assert_eq!(reader.next_packet().unwrap(), None);
            assert_eq!(reader.next_packet().unwrap(), None);
            file.truncate(if tencent { 12 } else { 11 });
            let mut reader = PacketReader::new(file.as_slice()).unwrap();
            assert!(matches!(reader.next_packet(), Err(Error::Truncated)));
        }
    }
    #[test]
    fn size_rate_and_header_checks_precede_kernel_access() {
        assert!(matches!(Decoder::new(1000000), Err(Error::SampleRate)));
        assert!(matches!(
            PacketReader::new(b"#!AMR\n".as_slice()),
            Err(Error::Truncated) | Err(Error::Header)
        ));
        let mut d = Decoder::new(24000).unwrap();
        assert!(matches!(
            d.decode_packet(&[0; 1025]),
            Err(Error::PacketTooLarge)
        ));
        let mut r = PacketReader::new(b"#!SILK_V3\x01\x04".as_slice()).unwrap();
        assert!(matches!(r.next_packet(), Err(Error::PacketTooLarge)));
    }
}
