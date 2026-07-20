use serde::{Deserialize, Serialize};
use thiserror::Error;

use crate::transcode::VideoCodec;

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
/// Raw video frame format accepted by native video encoders.
pub struct RawVideoFormat {
    /// Frame width in pixels.
    pub width: u32,
    /// Frame height in pixels.
    pub height: u32,
    /// Frame-rate numerator.
    pub frame_rate_num: u32,
    /// Frame-rate denominator.
    pub frame_rate_den: u32,
    /// Raw pixel memory layout.
    pub pixel_format: RawVideoPixelFormat,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
/// Raw video pixel formats Chroma Engine can feed to native encoders.
pub enum RawVideoPixelFormat {
    /// 8-bit BGRA, one packed 32-bit pixel per sample.
    Bgra,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
/// Successful native video encoder session initialization.
pub struct VideoEncodeSessionInfo {
    /// Output codec initialized by the backend.
    pub codec: VideoCodec,
    /// Backend-specific encoder name.
    pub encoder: String,
    /// Session width in pixels.
    pub width: u32,
    /// Session height in pixels.
    pub height: u32,
    /// Whether the session was requested as hardware-backed.
    pub hardware_required: bool,
    /// Whether the session reached the prepared-to-encode state.
    pub prepared: bool,
}

#[derive(Debug, Error)]
/// Error returned by native video encode backends.
pub enum VideoEncodeError {
    /// The requested raw video input shape is invalid or unsupported.
    #[error("invalid raw video input: {reason}")]
    InvalidInput {
        /// Diagnostic reason.
        reason: String,
    },
    /// No native video encode backend is available on this platform.
    #[error("native video encode backend is unavailable: {reason}")]
    BackendUnavailable {
        /// Diagnostic reason.
        reason: String,
    },
    /// The native backend returned an error.
    #[error("native video encode failed: {reason}")]
    BackendFailed {
        /// Diagnostic reason.
        reason: String,
    },
}

/// Creates and prepares a native VideoToolbox H.264 session for the requested raw format.
pub fn probe_videotoolbox_h264_session(
    format: RawVideoFormat,
) -> Result<VideoEncodeSessionInfo, VideoEncodeError> {
    validate_raw_video_format(format)?;
    platform_probe_videotoolbox_h264_session(format)
}

fn validate_raw_video_format(format: RawVideoFormat) -> Result<(), VideoEncodeError> {
    if format.width == 0 {
        return Err(VideoEncodeError::InvalidInput {
            reason: "width must be greater than zero".to_string(),
        });
    }
    if format.height == 0 {
        return Err(VideoEncodeError::InvalidInput {
            reason: "height must be greater than zero".to_string(),
        });
    }
    if format.frame_rate_num == 0 || format.frame_rate_den == 0 {
        return Err(VideoEncodeError::InvalidInput {
            reason: "frame rate numerator and denominator must be greater than zero".to_string(),
        });
    }
    if format.width > i32::MAX as u32 || format.height > i32::MAX as u32 {
        return Err(VideoEncodeError::InvalidInput {
            reason: "dimensions exceed VideoToolbox session limits".to_string(),
        });
    }
    Ok(())
}

#[cfg(target_os = "macos")]
fn platform_probe_videotoolbox_h264_session(
    format: RawVideoFormat,
) -> Result<VideoEncodeSessionInfo, VideoEncodeError> {
    use core_media::format_description::kCMVideoCodecType_H264;
    use video_toolbox::compression_session::VTCompressionSession;

    let session = VTCompressionSession::new(
        format.width as i32,
        format.height as i32,
        kCMVideoCodecType_H264,
        None,
        None,
        default_allocator(),
    )
    .map_err(|status| VideoEncodeError::BackendFailed {
        reason: format!("VTCompressionSessionCreate(H.264) returned {status}"),
    })?;
    session
        .prepare_to_encode_frames()
        .map_err(|status| VideoEncodeError::BackendFailed {
            reason: format!("VTCompressionSessionPrepareToEncodeFrames(H.264) returned {status}"),
        })?;
    session.invalidate();

    Ok(VideoEncodeSessionInfo {
        codec: VideoCodec::H264,
        encoder: "chroma-videotoolbox-h264".to_string(),
        width: format.width,
        height: format.height,
        hardware_required: false,
        prepared: true,
    })
}

#[cfg(target_os = "macos")]
fn default_allocator() -> core_foundation::base::CFAllocator {
    use core_foundation::base::TCFType;
    use core_foundation_sys::base::CFAllocatorGetDefault;

    #[allow(unsafe_code)]
    // SAFETY: CFAllocatorGetDefault returns the current process default allocator
    // under CoreFoundation's get rule. The wrapper retains it before use.
    unsafe {
        core_foundation::base::CFAllocator::wrap_under_get_rule(CFAllocatorGetDefault())
    }
}

#[cfg(not(target_os = "macos"))]
fn platform_probe_videotoolbox_h264_session(
    _format: RawVideoFormat,
) -> Result<VideoEncodeSessionInfo, VideoEncodeError> {
    Err(VideoEncodeError::BackendUnavailable {
        reason: "VideoToolbox H.264 encode is only available on macOS".to_string(),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn smoke_format() -> RawVideoFormat {
        RawVideoFormat {
            width: 128,
            height: 72,
            frame_rate_num: 24,
            frame_rate_den: 1,
            pixel_format: RawVideoPixelFormat::Bgra,
        }
    }

    #[test]
    fn rejects_zero_dimensions() {
        let err = probe_videotoolbox_h264_session(RawVideoFormat {
            width: 0,
            ..smoke_format()
        })
        .expect_err("reject zero width");

        assert!(err.to_string().contains("width"));
    }

    #[cfg(target_os = "macos")]
    #[test]
    fn macos_prepares_videotoolbox_h264_session() {
        let info =
            probe_videotoolbox_h264_session(smoke_format()).expect("prepare H.264 VT session");

        assert_eq!(info.codec, VideoCodec::H264);
        assert_eq!(info.encoder, "chroma-videotoolbox-h264");
        assert!(info.prepared);
    }

    #[cfg(not(target_os = "macos"))]
    #[test]
    fn non_macos_reports_backend_unavailable() {
        let err =
            probe_videotoolbox_h264_session(smoke_format()).expect_err("no VideoToolbox backend");

        assert!(matches!(err, VideoEncodeError::BackendUnavailable { .. }));
    }
}
