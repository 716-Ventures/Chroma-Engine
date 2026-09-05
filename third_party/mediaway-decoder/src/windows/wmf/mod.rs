//! Media Foundation hardware decode helpers.

#![allow(unsafe_code)]

mod codec;
mod cpu;
mod dx11;
mod h264;
mod runtime;
mod shared;

pub(crate) use h264::WmfH264Decoder as WmfHardwareDecoder;
