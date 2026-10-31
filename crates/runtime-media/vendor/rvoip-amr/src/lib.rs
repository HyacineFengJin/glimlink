//! AMR-only subset of MIT-licensed rvoip-codec-core 0.4.0.
#![allow(dead_code)]
pub mod error;
pub mod types;
mod fixed_point;
pub mod codecs { pub mod amr; }
