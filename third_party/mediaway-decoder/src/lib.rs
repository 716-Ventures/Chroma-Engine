//! Narrow Mediaway decoder facade patched for Windows H.264/HEVC hardware output.

#![allow(unsafe_code)]

mod error;
mod video;

pub use error::DecodeError;
pub use video::{VideoDecoder, VideoDecoderConfig, VideoOutputPreference};

pub mod windows;
