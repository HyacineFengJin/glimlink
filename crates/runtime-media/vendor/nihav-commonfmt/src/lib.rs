//! Crate for providing support for various common formats.
extern crate nihav_core;
extern crate nihav_codec_support;
extern crate nihav_registry;

#[cfg(any(feature="decoders", feature="encoders"))]
#[allow(clippy::single_match)]
#[allow(clippy::upper_case_acronyms)]
mod codecs;

#[cfg(feature="decoders")]
pub use crate::codecs::generic_register_all_decoders;
#[cfg(feature="decoders")]
pub use crate::codecs::generic_register_all_packetisers;
#[cfg(feature="encoders")]
pub use crate::codecs::generic_register_all_encoders;

#[cfg(feature="demuxers")]
mod demuxers;
#[cfg(feature="demuxers")]
pub use crate::demuxers::generic_register_all_demuxers;

#[cfg(feature="muxers")]
mod muxers;
#[cfg(feature="muxers")]
pub use crate::muxers::generic_register_all_muxers;

#[cfg(test)]
extern crate nihav_realmedia;
