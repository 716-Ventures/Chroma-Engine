//! Windows Media Foundation/D3D11 hardware video decoder.

#![cfg_attr(windows, allow(unsafe_code))]
#![cfg_attr(not(windows), deny(unsafe_code))]

use crate::{DecodeError, VideoDecoder, VideoDecoderConfig};
use mediaway_common::{Bytes, Packet, StreamInfo, VideoFrame};

#[cfg(windows)]
mod wmf;

/// Windows hardware H.264/HEVC decoder session.
pub struct WindowsVideoDecoder {
    #[cfg(windows)]
    inner: Option<wmf::WmfHardwareDecoder>,
    #[cfg(not(windows))]
    _private: (),
}

impl WindowsVideoDecoder {
    /// Opens a hardware-only Media Foundation transform with D3D11 output.
    #[cfg(windows)]
    pub fn open(config: &VideoDecoderConfig) -> Result<Self, DecodeError> {
        use mediaway_common::CodecKind;
        if !matches!(config.codec, CodecKind::H264 | CodecKind::Hevc) {
            return Err(DecodeError::Unsupported);
        }
        Ok(Self {
            inner: Some(wmf::WmfHardwareDecoder::open(config)?),
        })
    }

    /// Reports the native decoder as unavailable outside Windows.
    #[cfg(not(windows))]
    pub fn open(_config: &VideoDecoderConfig) -> Result<Self, DecodeError> {
        Err(DecodeError::Unsupported)
    }
}

#[cfg(windows)]
impl VideoDecoder for WindowsVideoDecoder {
    fn stream_info(&self) -> &StreamInfo {
        if let Some(decoder) = self.inner.as_ref() {
            decoder.stream_info()
        } else {
            closed_stream_info()
        }
    }

    fn push_packet(&mut self, packet: &Packet) -> Result<(), DecodeError> {
        self.inner
            .as_mut()
            .ok_or(DecodeError::Closed)?
            .push_packet(packet)
    }

    fn poll_frame(&mut self) -> Result<Option<VideoFrame>, DecodeError> {
        self.inner.as_mut().ok_or(DecodeError::Closed)?.poll_frame()
    }

    fn flush(&mut self) -> Result<(), DecodeError> {
        self.inner.as_mut().ok_or(DecodeError::Closed)?.flush()
    }
}

#[cfg(not(windows))]
impl VideoDecoder for WindowsVideoDecoder {
    fn stream_info(&self) -> &StreamInfo {
        closed_stream_info()
    }

    fn push_packet(&mut self, _packet: &Packet) -> Result<(), DecodeError> {
        Err(DecodeError::Unsupported)
    }

    fn poll_frame(&mut self) -> Result<Option<VideoFrame>, DecodeError> {
        Ok(None)
    }

    fn flush(&mut self) -> Result<(), DecodeError> {
        Err(DecodeError::Unsupported)
    }
}

fn closed_stream_info() -> &'static StreamInfo {
    use std::sync::OnceLock;
    static INFO: OnceLock<StreamInfo> = OnceLock::new();
    INFO.get_or_init(|| StreamInfo::Video {
        id: 0,
        codec: mediaway_common::CodecKind::H264,
        time_base: mediaway_common::Rational::new(1, 30),
        geometry: mediaway_common::VideoGeometry {
            width: 0,
            height: 0,
        },
        extra_data: Bytes::new(),
    })
}
