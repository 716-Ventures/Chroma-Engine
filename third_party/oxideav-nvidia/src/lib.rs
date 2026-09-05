#![cfg(target_os = "linux")]
#![allow(unsafe_code)]
#![allow(clippy::upper_case_acronyms)]
//! Runtime-loaded NVIDIA NVDEC bridge, patched for checked P016/Main10 output.

pub mod decoder;
pub mod device;
pub mod encoder;
pub mod nvdec;
#[doc(hidden)]
pub mod sys;

pub use decoder::{H264NvDecoder, HevcNvDecoder};
pub use device::{Cuda, CudaContext, CudaDevice, NvError};
pub use encoder::{H264NvEncoder, HevcNvEncoder};
pub use nvdec::{nvdec_caps, NvdecCaps};
pub use sys::CudaVideoCodec;
