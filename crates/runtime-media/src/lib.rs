// SPDX-License-Identifier: MIT
//! Bounded, file-backed media processing. No network, browser, or native codecs.
pub mod animation;
pub mod audio;
mod amr;
mod silk;
mod mp4;
pub mod pictures;
pub mod video;

use serde::{Deserialize, Serialize};
use std::{fmt, io, path::Path};

#[derive(Debug)]
pub enum MediaError {
    Unsupported(String),
    Limit(String),
    Invalid(String),
    Decode(String),
    Io(io::Error),
}
impl fmt::Display for MediaError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Unsupported(s) => write!(f, "unsupported media: {s}"),
            Self::Limit(s) => write!(f, "media limit: {s}"),
            Self::Invalid(s) => write!(f, "invalid media: {s}"),
            Self::Decode(s) => write!(f, "media decode: {s}"),
            Self::Io(e) => e.fmt(f),
        }
    }
}
impl std::error::Error for MediaError {}
impl From<io::Error> for MediaError {
    fn from(e: io::Error) -> Self {
        Self::Io(e)
    }
}
pub type Result<T> = std::result::Result<T, MediaError>;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Limits {
    pub max_input_bytes: u64,
    pub max_pixels: u64,
    pub max_decoded_bytes: u64,
    pub max_output_bytes: u64,
    pub max_dimension: u32,
    pub max_packets: usize,
    pub max_samples: usize,
    pub max_window_ms: u64,
    pub max_frames: usize,
}
impl Default for Limits {
    fn default() -> Self {
        Self {
            max_input_bytes: 256 * 1024 * 1024,
            max_pixels: 8_000_000,
            max_decoded_bytes: 96 * 1024 * 1024,
            max_output_bytes: 32 * 1024 * 1024,
            max_dimension: 4096,
            max_packets: 100_000,
            max_samples: 200_000,
            max_window_ms: 60_000,
            max_frames: 120,
        }
    }
}
impl Limits {
    pub fn check_file(&self, path: &Path) -> Result<u64> {
        let meta = std::fs::metadata(path)?;
        if !meta.is_file() {
            return Err(MediaError::Invalid(
                "input must be a regular disk file".into(),
            ));
        }
        if meta.len() > self.max_input_bytes {
            return Err(MediaError::Limit("input bytes".into()));
        }
        Ok(meta.len())
    }
    pub fn check_image(&self, width: u32, height: u32, bytes: u64) -> Result<()> {
        if width == 0 || height == 0 {
            return Err(MediaError::Invalid("zero image dimensions".into()));
        }
        if width > self.max_dimension
            || height > self.max_dimension
            || u64::from(width) * u64::from(height) > self.max_pixels
            || bytes > self.max_decoded_bytes
        {
            return Err(MediaError::Limit("decoded image dimensions/bytes".into()));
        }
        Ok(())
    }
    pub fn check_window(&self, start_ms: u64, duration_ms: u64) -> Result<u64> {
        if duration_ms == 0 || duration_ms > self.max_window_ms {
            return Err(MediaError::Limit("window duration".into()));
        }
        start_ms
            .checked_add(duration_ms)
            .ok_or_else(|| MediaError::Limit("timestamp overflow".into()))
    }
}

pub(crate) struct BoundedWriter<W> {
    pub inner: W,
    pub bytes: u64,
    pub limit: u64,
}
impl<W: io::Write> io::Write for BoundedWriter<W> {
    fn write(&mut self, b: &[u8]) -> io::Result<usize> {
        if b.len() as u64 > self.limit.saturating_sub(self.bytes) {
            return Err(io::Error::other("encoded output limit"));
        }
        let n = self.inner.write(b)?;
        self.bytes += n as u64;
        Ok(n)
    }
    fn flush(&mut self) -> io::Result<()> {
        self.inner.flush()
    }
}
