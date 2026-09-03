use serde::{Deserialize, Serialize};
use thiserror::Error;

use crate::packet::{TimeDelta, TimePoint, TimeScale};
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

/// Retained VideoToolbox H.264 encoder for a sequence of frame batches.
pub struct VideoToolboxH264EncoderSession {
    format: RawVideoFormat,
    bitrate: u32,
    encoded_batches: u64,
    #[cfg(target_os = "macos")]
    session: video_toolbox::compression_session::VTCompressionSession,
}

impl std::fmt::Debug for VideoToolboxH264EncoderSession {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("VideoToolboxH264EncoderSession")
            .field("format", &self.format)
            .field("bitrate", &self.bitrate)
            .field("encoded_batches", &self.encoded_batches)
            .finish_non_exhaustive()
    }
}

impl VideoToolboxH264EncoderSession {
    /// Creates and prepares one H.264 encoder that can serve multiple batches.
    pub fn new(format: RawVideoFormat, bitrate: u32) -> Result<Self, VideoEncodeError> {
        validate_raw_video_format(format)?;
        if bitrate == 0 {
            return Err(VideoEncodeError::InvalidInput {
                reason: "bitrate must be greater than zero".to_string(),
            });
        }
        platform_new_h264_encoder_session(format, bitrate)
    }

    /// Encodes one ordered frame batch without recreating the native session.
    pub fn encode(
        &mut self,
        frames: &[RawVideoFrameRef<'_>],
    ) -> Result<EncodedVideoOutput, VideoEncodeError> {
        validate_raw_video_frames(self.format, frames)?;
        let output = platform_encode_h264_with_retained_session(self, frames)?;
        self.encoded_batches = self.encoded_batches.saturating_add(1);
        Ok(output)
    }

    /// Returns the number of batches encoded by this native session.
    pub fn encoded_batches(&self) -> u64 {
        self.encoded_batches
    }
}

#[cfg(target_os = "macos")]
impl Drop for VideoToolboxH264EncoderSession {
    fn drop(&mut self) {
        self.session.invalidate();
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
/// Encoded video stream description plus emitted access units.
pub struct EncodedVideoOutput {
    /// Output stream description.
    pub stream: EncodedVideoStream,
    /// Encoded access units.
    pub frames: Vec<EncodedVideoFrame>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
/// One borrowed raw video frame accepted by native video encoders.
pub struct RawVideoFrameRef<'a> {
    /// Presentation timestamp.
    pub pts: TimePoint,
    /// Decode timestamp.
    pub dts: TimePoint,
    /// Frame duration.
    pub duration: TimeDelta,
    /// Raw frame bytes matching the selected raw format.
    pub bytes: &'a [u8],
    /// True when this source frame starts an independently decodable access unit.
    pub keyframe: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
/// Stable output description for an encoded video stream.
pub struct EncodedVideoStream {
    /// Output codec carried by the stream.
    pub codec: VideoCodec,
    /// Encoded width in pixels.
    pub width: u32,
    /// Encoded height in pixels.
    pub height: u32,
    /// Output time scale used by frame timing.
    pub time_scale: TimeScale,
    /// Codec-specific decoder configuration bytes, when required by the muxer.
    pub decoder_config: Option<Vec<u8>>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
/// One encoded video access unit emitted by a native backend.
pub struct EncodedVideoFrame {
    /// Presentation timestamp.
    pub pts: TimePoint,
    /// Decode timestamp.
    pub dts: TimePoint,
    /// Frame duration.
    pub duration: TimeDelta,
    /// Compressed payload bytes.
    pub payload: Vec<u8>,
    /// True when this access unit is independently decodable.
    pub keyframe: bool,
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

/// Creates and prepares a native VideoToolbox HEVC session for the requested raw format.
pub fn probe_videotoolbox_hevc_session(
    format: RawVideoFormat,
) -> Result<VideoEncodeSessionInfo, VideoEncodeError> {
    validate_raw_video_format(format)?;
    platform_probe_videotoolbox_hevc_session(format)
}

/// Encodes one BGRA frame to H.264 using the native VideoToolbox backend.
pub fn encode_h264_videotoolbox_bgra_frame(
    format: RawVideoFormat,
    bgra: &[u8],
    bitrate: u32,
) -> Result<EncodedVideoOutput, VideoEncodeError> {
    validate_raw_video_format(format)?;
    validate_bgra_frame(format, bgra)?;
    if bitrate == 0 {
        return Err(VideoEncodeError::InvalidInput {
            reason: "bitrate must be greater than zero".to_string(),
        });
    }
    platform_encode_h264_videotoolbox_bgra_frame(format, bgra, bitrate)
}

/// Encodes a contiguous BGRA frame batch to H.264 using one native VideoToolbox session.
pub fn encode_h264_videotoolbox_bgra_frames(
    format: RawVideoFormat,
    frames: &[RawVideoFrameRef<'_>],
    bitrate: u32,
) -> Result<EncodedVideoOutput, VideoEncodeError> {
    VideoToolboxH264EncoderSession::new(format, bitrate)?.encode(frames)
}

/// Encodes one BGRA frame to HEVC using the native VideoToolbox backend.
pub fn encode_hevc_videotoolbox_bgra_frame(
    format: RawVideoFormat,
    bgra: &[u8],
    bitrate: u32,
) -> Result<EncodedVideoOutput, VideoEncodeError> {
    validate_raw_video_format(format)?;
    validate_bgra_frame(format, bgra)?;
    if bitrate == 0 {
        return Err(VideoEncodeError::InvalidInput {
            reason: "bitrate must be greater than zero".to_string(),
        });
    }
    platform_encode_hevc_videotoolbox_bgra_frame(format, bgra, bitrate)
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

fn validate_bgra_frame(format: RawVideoFormat, bgra: &[u8]) -> Result<(), VideoEncodeError> {
    if format.pixel_format != RawVideoPixelFormat::Bgra {
        return Err(VideoEncodeError::InvalidInput {
            reason: "only BGRA frames can be encoded by this entrypoint".to_string(),
        });
    }
    let expected = usize::try_from(format.width)
        .ok()
        .and_then(|width| width.checked_mul(format.height as usize))
        .and_then(|pixels| pixels.checked_mul(4))
        .ok_or_else(|| VideoEncodeError::InvalidInput {
            reason: "BGRA frame byte count overflowed".to_string(),
        })?;
    if bgra.len() != expected {
        return Err(VideoEncodeError::InvalidInput {
            reason: format!("BGRA frame has {} byte(s), expected {expected}", bgra.len()),
        });
    }
    Ok(())
}

fn validate_raw_video_frames(
    format: RawVideoFormat,
    frames: &[RawVideoFrameRef<'_>],
) -> Result<(), VideoEncodeError> {
    if frames.is_empty() {
        return Err(VideoEncodeError::InvalidInput {
            reason: "video frame batch is empty".to_string(),
        });
    }
    let time_scale = frames[0].pts.scale;
    let mut previous_dts = None;
    for frame in frames {
        validate_bgra_frame(format, frame.bytes)?;
        if frame.pts.scale != time_scale
            || frame.dts.scale != time_scale
            || frame.duration.scale != time_scale
        {
            return Err(VideoEncodeError::InvalidInput {
                reason: "video frame batch timestamps must use one time scale".to_string(),
            });
        }
        if let Some(previous_dts) = previous_dts
            && frame.dts.units < previous_dts
        {
            return Err(VideoEncodeError::InvalidInput {
                reason: "video frame batch decode timestamps must be monotonic".to_string(),
            });
        }
        previous_dts = Some(frame.dts.units);
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
fn platform_probe_videotoolbox_hevc_session(
    format: RawVideoFormat,
) -> Result<VideoEncodeSessionInfo, VideoEncodeError> {
    use core_media::format_description::kCMVideoCodecType_HEVC;
    use video_toolbox::compression_session::VTCompressionSession;

    let session = VTCompressionSession::new(
        format.width as i32,
        format.height as i32,
        kCMVideoCodecType_HEVC,
        None,
        None,
        default_allocator(),
    )
    .map_err(|status| VideoEncodeError::BackendFailed {
        reason: format!("VTCompressionSessionCreate(HEVC) returned {status}"),
    })?;
    session
        .prepare_to_encode_frames()
        .map_err(|status| VideoEncodeError::BackendFailed {
            reason: format!("VTCompressionSessionPrepareToEncodeFrames(HEVC) returned {status}"),
        })?;
    session.invalidate();

    Ok(VideoEncodeSessionInfo {
        codec: VideoCodec::Hevc,
        encoder: "chroma-videotoolbox-hevc".to_string(),
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

#[cfg(target_os = "macos")]
fn platform_encode_h264_videotoolbox_bgra_frame(
    format: RawVideoFormat,
    bgra: &[u8],
    _bitrate: u32,
) -> Result<EncodedVideoOutput, VideoEncodeError> {
    use std::sync::{Arc, Mutex};

    use core_media::{format_description::kCMVideoCodecType_H264, time::CMTime};
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

    let pixel_buffer = bgra_pixel_buffer(format, bgra)?;
    let image_buffer = image_buffer_from_pixel_buffer(&pixel_buffer);
    let frames = Arc::new(Mutex::new(Vec::<EncodedVideoFrame>::new()));
    let decoder_config = Arc::new(Mutex::new(None::<Vec<u8>>));
    let callback_error = Arc::new(Mutex::new(None::<VideoEncodeError>));
    let frames_out = Arc::clone(&frames);
    let config_out = Arc::clone(&decoder_config);
    let error_out = Arc::clone(&callback_error);
    let time_scale = TimeScale {
        units_per_second: format.frame_rate_num,
    };
    let duration_units = format.frame_rate_den;
    let duration = TimeDelta {
        units: u64::from(duration_units),
        scale: time_scale,
    };

    session
        .encode_frame_with_closure(
            image_buffer,
            CMTime::make(0, format.frame_rate_num as i32),
            CMTime::make(format.frame_rate_den as i64, format.frame_rate_num as i32),
            None,
            move |status, _flags, sample_buffer_ref| {
                if status != 0 {
                    set_encode_callback_error(
                        &error_out,
                        VideoEncodeError::BackendFailed {
                            reason: format!("VideoToolbox H.264 encode callback returned {status}"),
                        },
                    );
                    return;
                }
                if sample_buffer_ref.is_null() {
                    set_encode_callback_error(
                        &error_out,
                        VideoEncodeError::BackendFailed {
                            reason: "VideoToolbox H.264 encode callback returned no sample buffer"
                                .to_string(),
                        },
                    );
                    return;
                }
                let Some((payload, config)) = copy_h264_sample(sample_buffer_ref) else {
                    set_encode_callback_error(
                        &error_out,
                        VideoEncodeError::BackendFailed {
                            reason: "VideoToolbox H.264 sample buffer copy failed".to_string(),
                        },
                    );
                    return;
                };
                if let Ok(mut guard) = config_out.lock()
                    && guard.is_none()
                {
                    *guard = config;
                }
                match frames_out.lock() {
                    Ok(mut guard) => guard.push(EncodedVideoFrame {
                        pts: TimePoint {
                            units: 0,
                            scale: time_scale,
                        },
                        dts: TimePoint {
                            units: 0,
                            scale: time_scale,
                        },
                        duration,
                        payload,
                        keyframe: true,
                    }),
                    Err(_) => set_encode_callback_error(
                        &error_out,
                        VideoEncodeError::BackendFailed {
                            reason: "VideoToolbox H.264 frame output lock was poisoned".to_string(),
                        },
                    ),
                }
            },
        )
        .map_err(|status| VideoEncodeError::BackendFailed {
            reason: format!("VTCompressionSessionEncodeFrame(H.264) returned {status}"),
        })?;
    session
        .complete_frames(CMTime::make(i64::MAX, format.frame_rate_num as i32))
        .map_err(|status| VideoEncodeError::BackendFailed {
            reason: format!("VTCompressionSessionCompleteFrames(H.264) returned {status}"),
        })?;
    session.invalidate();
    if let Some(error) = take_encode_callback_error(&callback_error)? {
        return Err(error);
    }

    let frames = frames
        .lock()
        .map_err(|_| VideoEncodeError::BackendFailed {
            reason: "VideoToolbox frame output lock was poisoned".to_string(),
        })?
        .clone();
    if frames.is_empty() {
        return Err(VideoEncodeError::BackendFailed {
            reason: "VideoToolbox emitted no H.264 sample buffers".to_string(),
        });
    }
    let decoder_config = decoder_config
        .lock()
        .map_err(|_| VideoEncodeError::BackendFailed {
            reason: "VideoToolbox decoder config lock was poisoned".to_string(),
        })?
        .clone();

    Ok(EncodedVideoOutput {
        stream: EncodedVideoStream {
            codec: VideoCodec::H264,
            width: format.width,
            height: format.height,
            time_scale,
            decoder_config,
        },
        frames,
    })
}

#[cfg(target_os = "macos")]
fn platform_new_h264_encoder_session(
    format: RawVideoFormat,
    bitrate: u32,
) -> Result<VideoToolboxH264EncoderSession, VideoEncodeError> {
    use core_media::format_description::kCMVideoCodecType_H264;
    use video_toolbox::{
        compression_properties::CompressionPropertyKey, compression_session::VTCompressionSession,
        session::TVTSession,
    };

    let session = VTCompressionSession::new(
        format.width as i32,
        format.height as i32,
        kCMVideoCodecType_H264,
        None,
        None,
        default_allocator(),
    )
    .map_err(|status| VideoEncodeError::BackendFailed {
        reason: format!("VTCompressionSessionCreate(H.264 batch) returned {status}"),
    })?;
    let vt_session = session.as_session();
    set_vt_property_bool(&vt_session, CompressionPropertyKey::RealTime, true)?;
    set_vt_property_bool(
        &vt_session,
        CompressionPropertyKey::AllowFrameReordering,
        false,
    )?;
    set_vt_property_i32(
        &vt_session,
        CompressionPropertyKey::AverageBitRate,
        i32::try_from(bitrate).unwrap_or(i32::MAX),
    )?;
    set_vt_property_i32(&vt_session, CompressionPropertyKey::MaxFrameDelayCount, 0)?;
    set_vt_property_i32(
        &vt_session,
        CompressionPropertyKey::ExpectedFrameRate,
        i32::try_from(format.frame_rate_num / format.frame_rate_den.max(1)).unwrap_or(i32::MAX),
    )?;
    session
        .prepare_to_encode_frames()
        .map_err(|status| VideoEncodeError::BackendFailed {
            reason: format!(
                "VTCompressionSessionPrepareToEncodeFrames(H.264 batch) returned {status}"
            ),
        })?;
    Ok(VideoToolboxH264EncoderSession {
        format,
        bitrate,
        encoded_batches: 0,
        session,
    })
}

#[cfg(target_os = "macos")]
fn platform_encode_h264_with_retained_session(
    retained: &VideoToolboxH264EncoderSession,
    input_frames: &[RawVideoFrameRef<'_>],
) -> Result<EncodedVideoOutput, VideoEncodeError> {
    use std::sync::{Arc, Mutex};

    use core_media::time::CMTime;

    let format = retained.format;
    let session = &retained.session;

    let frames = Arc::new(Mutex::new(Vec::<EncodedVideoFrame>::new()));
    let decoder_config = Arc::new(Mutex::new(None::<Vec<u8>>));
    let callback_error = Arc::new(Mutex::new(None::<VideoEncodeError>));
    for input in input_frames {
        let pixel_buffer = bgra_pixel_buffer(format, input.bytes)?;
        let image_buffer = image_buffer_from_pixel_buffer(&pixel_buffer);
        let frames_out = Arc::clone(&frames);
        let config_out = Arc::clone(&decoder_config);
        let error_out = Arc::clone(&callback_error);
        let pts = input.pts;
        let dts = input.dts;
        let duration = input.duration;
        let keyframe = input.keyframe;
        let frame_properties = keyframe.then(force_keyframe_dictionary);

        session
            .encode_frame_with_closure(
                image_buffer,
                CMTime::make(
                    i64::try_from(pts.units).unwrap_or(i64::MAX),
                    pts.scale.units_per_second as i32,
                ),
                CMTime::make(
                    i64::try_from(duration.units).unwrap_or(i64::MAX),
                    duration.scale.units_per_second as i32,
                ),
                frame_properties.as_ref(),
                move |status, _flags, sample_buffer_ref| {
                    if status != 0 {
                        set_encode_callback_error(
                            &error_out,
                            VideoEncodeError::BackendFailed {
                                reason: format!(
                                    "VideoToolbox H.264 batch encode callback returned {status}"
                                ),
                            },
                        );
                        return;
                    }
                    if sample_buffer_ref.is_null() {
                        set_encode_callback_error(
                            &error_out,
                            VideoEncodeError::BackendFailed {
                                reason:
                                    "VideoToolbox H.264 batch encode callback returned no sample buffer"
                                        .to_string(),
                            },
                        );
                        return;
                    }
                    let Some((payload, config)) = copy_h264_sample(sample_buffer_ref) else {
                        set_encode_callback_error(
                            &error_out,
                            VideoEncodeError::BackendFailed {
                                reason: "VideoToolbox H.264 batch sample buffer copy failed"
                                    .to_string(),
                            },
                        );
                        return;
                    };
                    if let Ok(mut guard) = config_out.lock()
                        && guard.is_none()
                    {
                        *guard = config;
                    }
                    match frames_out.lock() {
                        Ok(mut guard) => guard.push(EncodedVideoFrame {
                                pts,
                                dts,
                                duration,
                                payload,
                                keyframe,
                            }),
                        Err(_) => set_encode_callback_error(
                            &error_out,
                            VideoEncodeError::BackendFailed {
                                reason:
                                    "VideoToolbox H.264 batch frame output lock was poisoned"
                                        .to_string(),
                            },
                        ),
                    }
                },
            )
            .map_err(|status| VideoEncodeError::BackendFailed {
                reason: format!("VTCompressionSessionEncodeFrame(H.264 batch) returned {status}"),
            })?;
    }
    session
        .complete_frames(CMTime::make(
            i64::MAX,
            input_frames[0].pts.scale.units_per_second as i32,
        ))
        .map_err(|status| VideoEncodeError::BackendFailed {
            reason: format!("VTCompressionSessionCompleteFrames(H.264 batch) returned {status}"),
        })?;
    if let Some(error) = take_encode_callback_error(&callback_error)? {
        return Err(error);
    }

    let mut frames = frames
        .lock()
        .map_err(|_| VideoEncodeError::BackendFailed {
            reason: "VideoToolbox frame output lock was poisoned".to_string(),
        })?
        .clone();
    frames.sort_by_key(|frame| (frame.dts.units, frame.pts.units));
    if frames.is_empty() {
        return Err(VideoEncodeError::BackendFailed {
            reason: "VideoToolbox emitted no H.264 sample buffers".to_string(),
        });
    }
    let decoder_config = decoder_config
        .lock()
        .map_err(|_| VideoEncodeError::BackendFailed {
            reason: "VideoToolbox decoder config lock was poisoned".to_string(),
        })?
        .clone();

    Ok(EncodedVideoOutput {
        stream: EncodedVideoStream {
            codec: VideoCodec::H264,
            width: format.width,
            height: format.height,
            time_scale: input_frames[0].pts.scale,
            decoder_config,
        },
        frames,
    })
}

#[cfg(not(target_os = "macos"))]
fn platform_new_h264_encoder_session(
    _format: RawVideoFormat,
    _bitrate: u32,
) -> Result<VideoToolboxH264EncoderSession, VideoEncodeError> {
    Err(VideoEncodeError::BackendUnavailable {
        reason: "VideoToolbox H.264 encode is only available on macOS".to_string(),
    })
}

#[cfg(not(target_os = "macos"))]
fn platform_encode_h264_with_retained_session(
    _retained: &VideoToolboxH264EncoderSession,
    _input_frames: &[RawVideoFrameRef<'_>],
) -> Result<EncodedVideoOutput, VideoEncodeError> {
    Err(VideoEncodeError::BackendUnavailable {
        reason: "VideoToolbox H.264 encode is only available on macOS".to_string(),
    })
}

#[cfg(target_os = "macos")]
fn set_vt_property_bool(
    session: &video_toolbox::session::VTSession,
    key: video_toolbox::compression_properties::CompressionPropertyKey,
    value: bool,
) -> Result<(), VideoEncodeError> {
    use core_foundation::{base::TCFType, boolean::CFBoolean};
    session
        .set_property(key.into(), CFBoolean::from(value).as_CFType())
        .map_err(|status| VideoEncodeError::BackendFailed {
            reason: format!("VTSessionSetProperty({key:?}) returned {status}"),
        })
        .or_else(ignore_unsupported_vt_property)
}

#[cfg(target_os = "macos")]
fn set_vt_property_i32(
    session: &video_toolbox::session::VTSession,
    key: video_toolbox::compression_properties::CompressionPropertyKey,
    value: i32,
) -> Result<(), VideoEncodeError> {
    use core_foundation::{base::TCFType, number::CFNumber};
    session
        .set_property(key.into(), CFNumber::from(value).as_CFType())
        .map_err(|status| VideoEncodeError::BackendFailed {
            reason: format!("VTSessionSetProperty({key:?}) returned {status}"),
        })
        .or_else(ignore_unsupported_vt_property)
}

#[cfg(target_os = "macos")]
fn ignore_unsupported_vt_property(error: VideoEncodeError) -> Result<(), VideoEncodeError> {
    match &error {
        VideoEncodeError::BackendFailed { reason } if reason.contains("returned -12900") => Ok(()),
        _ => Err(error),
    }
}

#[cfg(target_os = "macos")]
fn set_encode_callback_error(
    error_slot: &std::sync::Arc<std::sync::Mutex<Option<VideoEncodeError>>>,
    error: VideoEncodeError,
) {
    if let Ok(mut slot) = error_slot.lock()
        && slot.is_none()
    {
        *slot = Some(error);
    }
}

#[cfg(target_os = "macos")]
fn take_encode_callback_error(
    error_slot: &std::sync::Arc<std::sync::Mutex<Option<VideoEncodeError>>>,
) -> Result<Option<VideoEncodeError>, VideoEncodeError> {
    error_slot
        .lock()
        .map_err(|_| VideoEncodeError::BackendFailed {
            reason: "VideoToolbox encode callback error lock was poisoned".to_string(),
        })
        .map(|mut slot| slot.take())
}

#[cfg(target_os = "macos")]
fn force_keyframe_dictionary() -> core_foundation::dictionary::CFDictionary<
    core_foundation::string::CFString,
    core_foundation::base::CFType,
> {
    use core_foundation::{
        base::TCFType, boolean::CFBoolean, dictionary::CFDictionary, string::CFString,
    };
    use video_toolbox::compression_properties::EncodeFrameOptionKey;

    let key = CFString::from(EncodeFrameOptionKey::ForceKeyFrame);
    let value = CFBoolean::true_value().as_CFType();
    CFDictionary::from_CFType_pairs(&[(key, value)])
}

#[cfg(target_os = "macos")]
fn platform_encode_hevc_videotoolbox_bgra_frame(
    format: RawVideoFormat,
    bgra: &[u8],
    _bitrate: u32,
) -> Result<EncodedVideoOutput, VideoEncodeError> {
    use std::sync::{Arc, Mutex};

    use core_media::{format_description::kCMVideoCodecType_HEVC, time::CMTime};
    use video_toolbox::compression_session::VTCompressionSession;

    let session = VTCompressionSession::new(
        format.width as i32,
        format.height as i32,
        kCMVideoCodecType_HEVC,
        None,
        None,
        default_allocator(),
    )
    .map_err(|status| VideoEncodeError::BackendFailed {
        reason: format!("VTCompressionSessionCreate(HEVC) returned {status}"),
    })?;
    session
        .prepare_to_encode_frames()
        .map_err(|status| VideoEncodeError::BackendFailed {
            reason: format!("VTCompressionSessionPrepareToEncodeFrames(HEVC) returned {status}"),
        })?;

    let pixel_buffer = bgra_pixel_buffer(format, bgra)?;
    let image_buffer = image_buffer_from_pixel_buffer(&pixel_buffer);
    let frames = Arc::new(Mutex::new(Vec::<EncodedVideoFrame>::new()));
    let decoder_config = Arc::new(Mutex::new(None::<Vec<u8>>));
    let callback_error = Arc::new(Mutex::new(None::<VideoEncodeError>));
    let frames_out = Arc::clone(&frames);
    let config_out = Arc::clone(&decoder_config);
    let error_out = Arc::clone(&callback_error);
    let time_scale = TimeScale {
        units_per_second: format.frame_rate_num,
    };
    let duration = TimeDelta {
        units: u64::from(format.frame_rate_den),
        scale: time_scale,
    };

    session
        .encode_frame_with_closure(
            image_buffer,
            CMTime::make(0, format.frame_rate_num as i32),
            CMTime::make(format.frame_rate_den as i64, format.frame_rate_num as i32),
            None,
            move |status, _flags, sample_buffer_ref| {
                if status != 0 {
                    set_encode_callback_error(
                        &error_out,
                        VideoEncodeError::BackendFailed {
                            reason: format!("VideoToolbox HEVC encode callback returned {status}"),
                        },
                    );
                    return;
                }
                if sample_buffer_ref.is_null() {
                    set_encode_callback_error(
                        &error_out,
                        VideoEncodeError::BackendFailed {
                            reason: "VideoToolbox HEVC encode callback returned no sample buffer"
                                .to_string(),
                        },
                    );
                    return;
                }
                let Some((payload, config)) = copy_hevc_sample(sample_buffer_ref) else {
                    set_encode_callback_error(
                        &error_out,
                        VideoEncodeError::BackendFailed {
                            reason: "VideoToolbox HEVC sample buffer copy failed".to_string(),
                        },
                    );
                    return;
                };
                if let Ok(mut guard) = config_out.lock()
                    && guard.is_none()
                {
                    *guard = config;
                }
                match frames_out.lock() {
                    Ok(mut guard) => guard.push(EncodedVideoFrame {
                        pts: TimePoint {
                            units: 0,
                            scale: time_scale,
                        },
                        dts: TimePoint {
                            units: 0,
                            scale: time_scale,
                        },
                        duration,
                        payload,
                        keyframe: true,
                    }),
                    Err(_) => set_encode_callback_error(
                        &error_out,
                        VideoEncodeError::BackendFailed {
                            reason: "VideoToolbox HEVC frame output lock was poisoned".to_string(),
                        },
                    ),
                }
            },
        )
        .map_err(|status| VideoEncodeError::BackendFailed {
            reason: format!("VTCompressionSessionEncodeFrame(HEVC) returned {status}"),
        })?;
    session
        .complete_frames(CMTime::make(i64::MAX, format.frame_rate_num as i32))
        .map_err(|status| VideoEncodeError::BackendFailed {
            reason: format!("VTCompressionSessionCompleteFrames(HEVC) returned {status}"),
        })?;
    session.invalidate();
    if let Some(error) = take_encode_callback_error(&callback_error)? {
        return Err(error);
    }

    let frames = frames
        .lock()
        .map_err(|_| VideoEncodeError::BackendFailed {
            reason: "VideoToolbox frame output lock was poisoned".to_string(),
        })?
        .clone();
    if frames.is_empty() {
        return Err(VideoEncodeError::BackendFailed {
            reason: "VideoToolbox emitted no HEVC sample buffers".to_string(),
        });
    }
    let decoder_config = decoder_config
        .lock()
        .map_err(|_| VideoEncodeError::BackendFailed {
            reason: "VideoToolbox decoder config lock was poisoned".to_string(),
        })?
        .clone();

    Ok(EncodedVideoOutput {
        stream: EncodedVideoStream {
            codec: VideoCodec::Hevc,
            width: format.width,
            height: format.height,
            time_scale,
            decoder_config,
        },
        frames,
    })
}

#[cfg(target_os = "macos")]
fn copy_h264_sample(
    sample_buffer_ref: core_media::sample_buffer::CMSampleBufferRef,
) -> Option<(Vec<u8>, Option<Vec<u8>>)> {
    use core_foundation::base::TCFType;
    use core_media::{block_buffer::CMBlockBuffer, sample_buffer::CMSampleBuffer};

    #[allow(unsafe_code)]
    // SAFETY: VideoToolbox owns the callback sample buffer for the duration of
    // this closure. Chroma copies all data and parameter sets before returning.
    let sample_buffer = unsafe { CMSampleBuffer::wrap_under_get_rule(sample_buffer_ref) };
    let data_buffer: CMBlockBuffer = sample_buffer.get_data_buffer()?;
    let mut payload = vec![0; data_buffer.get_data_length()];
    data_buffer.copy_data_bytes(0, &mut payload).ok()?;
    let config = sample_buffer
        .get_format_description()
        .and_then(|description| avc_decoder_config_from_format_description(&description));
    Some((payload, config))
}

#[cfg(target_os = "macos")]
fn copy_hevc_sample(
    sample_buffer_ref: core_media::sample_buffer::CMSampleBufferRef,
) -> Option<(Vec<u8>, Option<Vec<u8>>)> {
    use core_foundation::base::TCFType;
    use core_media::{block_buffer::CMBlockBuffer, sample_buffer::CMSampleBuffer};

    #[allow(unsafe_code)]
    // SAFETY: VideoToolbox owns the callback sample buffer for the duration of
    // this closure. Chroma copies all data and parameter sets before returning.
    let sample_buffer = unsafe { CMSampleBuffer::wrap_under_get_rule(sample_buffer_ref) };
    let data_buffer: CMBlockBuffer = sample_buffer.get_data_buffer()?;
    let mut payload = vec![0; data_buffer.get_data_length()];
    data_buffer.copy_data_bytes(0, &mut payload).ok()?;
    let config = sample_buffer
        .get_format_description()
        .and_then(|description| hevc_decoder_config_from_format_description(&description));
    Some((payload, config))
}

#[cfg(target_os = "macos")]
fn avc_decoder_config_from_format_description(
    description: &core_media::format_description::CMFormatDescription,
) -> Option<Vec<u8>> {
    use core_foundation::base::TCFType;
    use core_media::format_description::CMVideoFormatDescription;

    #[allow(unsafe_code)]
    // SAFETY: The sample buffer's format description is a video format description
    // for H.264 output. Parameter sets are copied before the description is dropped.
    let video_description =
        unsafe { CMVideoFormatDescription::wrap_under_get_rule(description.as_concrete_TypeRef()) };
    let mut parameter_sets = Vec::new();
    let (_, parameter_set_count, nal_length_size) =
        video_description.get_h264_parameter_set_at_index(0).ok()?;
    for index in 0..parameter_set_count {
        let (bytes, _, _) = video_description
            .get_h264_parameter_set_at_index(index)
            .ok()?;
        parameter_sets.push(bytes.to_vec());
    }
    build_avc_decoder_config(&parameter_sets, nal_length_size)
}

#[cfg(target_os = "macos")]
fn hevc_decoder_config_from_format_description(
    description: &core_media::format_description::CMFormatDescription,
) -> Option<Vec<u8>> {
    use core_foundation::base::TCFType;
    use core_media::format_description::CMVideoFormatDescription;

    #[allow(unsafe_code)]
    // SAFETY: The sample buffer's format description is a video format description
    // for HEVC output. Parameter sets are copied before the description is dropped.
    let video_description =
        unsafe { CMVideoFormatDescription::wrap_under_get_rule(description.as_concrete_TypeRef()) };
    let mut parameter_sets = Vec::new();
    let (_, parameter_set_count, nal_length_size) =
        video_description.get_hevc_parameter_set_at_index(0).ok()?;
    for index in 0..parameter_set_count {
        let (bytes, _, _) = video_description
            .get_hevc_parameter_set_at_index(index)
            .ok()?;
        parameter_sets.push(bytes.to_vec());
    }
    build_hevc_decoder_config(&parameter_sets, nal_length_size)
}

#[cfg(target_os = "macos")]
fn image_buffer_from_pixel_buffer(
    pixel_buffer: &core_video::pixel_buffer::CVPixelBuffer,
) -> core_video::image_buffer::CVImageBuffer {
    use core_foundation::base::TCFType;
    use core_video::image_buffer::CVImageBuffer;

    #[allow(unsafe_code)]
    // SAFETY: CVPixelBuffer is a CVImageBuffer subtype. The wrapper retains the
    // CoreVideo object while it is handed to VideoToolbox.
    unsafe {
        CVImageBuffer::wrap_under_get_rule(pixel_buffer.as_concrete_TypeRef())
    }
}

#[cfg(target_os = "macos")]
fn bgra_pixel_buffer(
    format: RawVideoFormat,
    bgra: &[u8],
) -> Result<core_video::pixel_buffer::CVPixelBuffer, VideoEncodeError> {
    use core_video::pixel_buffer::{CVPixelBuffer, kCVPixelFormatType_32BGRA};
    use core_video::r#return::kCVReturnSuccess;

    let pixel_buffer = CVPixelBuffer::new(
        kCVPixelFormatType_32BGRA,
        format.width as usize,
        format.height as usize,
        None,
    )
    .map_err(|status| VideoEncodeError::BackendFailed {
        reason: format!("CVPixelBufferCreate(BGRA) returned {status}"),
    })?;
    let status = pixel_buffer.lock_base_address(0);
    if status != kCVReturnSuccess {
        return Err(VideoEncodeError::BackendFailed {
            reason: format!("CVPixelBufferLockBaseAddress returned {status}"),
        });
    }
    let bytes_per_row = pixel_buffer.get_bytes_per_row();
    let row_bytes = format.width as usize * 4;
    #[allow(unsafe_code)]
    // SAFETY: The pixel buffer is locked for CPU writes, `base` points to at
    // least `bytes_per_row * height` bytes owned by CoreVideo, and each source
    // row is exactly `row_bytes` bytes from the validated BGRA frame slice.
    unsafe {
        let base = pixel_buffer.get_base_address() as *mut u8;
        if base.is_null() || bytes_per_row < row_bytes {
            let _ = pixel_buffer.unlock_base_address(0);
            return Err(VideoEncodeError::BackendFailed {
                reason: "CVPixelBuffer returned invalid BGRA storage".to_string(),
            });
        }
        for row in 0..format.height as usize {
            let src = bgra.as_ptr().add(row * row_bytes);
            let dst = base.add(row * bytes_per_row);
            std::ptr::copy_nonoverlapping(src, dst, row_bytes);
        }
    }
    let status = pixel_buffer.unlock_base_address(0);
    if status != kCVReturnSuccess {
        return Err(VideoEncodeError::BackendFailed {
            reason: format!("CVPixelBufferUnlockBaseAddress returned {status}"),
        });
    }
    Ok(pixel_buffer)
}

#[cfg(target_os = "macos")]
fn build_avc_decoder_config(parameter_sets: &[Vec<u8>], nal_length_size: i32) -> Option<Vec<u8>> {
    let mut sps = Vec::new();
    let mut pps = Vec::new();
    for set in parameter_sets {
        match set.first().map(|byte| byte & 0x1f) {
            Some(7) => sps.push(set.as_slice()),
            Some(8) => pps.push(set.as_slice()),
            _ => {}
        }
    }
    let first_sps = *sps.first()?;
    if first_sps.len() < 4 || sps.len() > 31 || pps.len() > u8::MAX as usize {
        return None;
    }
    let length_size_minus_one = u8::try_from(nal_length_size.checked_sub(1)?).ok()? & 0x03;
    let mut out = vec![
        1,
        first_sps[1],
        first_sps[2],
        first_sps[3],
        0xfc | length_size_minus_one,
        0xe0 | u8::try_from(sps.len()).ok()?,
    ];
    for set in sps {
        let len = u16::try_from(set.len()).ok()?;
        out.extend_from_slice(&len.to_be_bytes());
        out.extend_from_slice(set);
    }
    out.push(u8::try_from(pps.len()).ok()?);
    for set in pps {
        let len = u16::try_from(set.len()).ok()?;
        out.extend_from_slice(&len.to_be_bytes());
        out.extend_from_slice(set);
    }
    Some(out)
}

#[cfg(target_os = "macos")]
fn build_hevc_decoder_config(parameter_sets: &[Vec<u8>], nal_length_size: i32) -> Option<Vec<u8>> {
    let mut arrays: Vec<(u8, Vec<&[u8]>)> = Vec::new();
    let mut sps = None;
    for set in parameter_sets {
        let nal_unit_type = set.first().map(|byte| (byte >> 1) & 0x3f)?;
        if nal_unit_type == 33 {
            sps = Some(set.as_slice());
        }
        if let Some((_, units)) = arrays
            .iter_mut()
            .find(|(existing_type, _)| *existing_type == nal_unit_type)
        {
            units.push(set);
        } else {
            arrays.push((nal_unit_type, vec![set]));
        }
    }
    let sps = sps?;
    if sps.len() < 15 || arrays.len() > u8::MAX as usize {
        return None;
    }
    let length_size_minus_one = u8::try_from(nal_length_size.checked_sub(1)?).ok()? & 0x03;
    let mut out = Vec::new();
    out.push(1);
    out.push(sps[3]);
    out.extend_from_slice(&sps[4..8]);
    out.extend_from_slice(&sps[8..14]);
    out.push(sps[14]);
    out.extend_from_slice(&[0xf0, 0x00]);
    out.push(0xfc);
    out.push(0xfc);
    out.push(0xf8);
    out.push(0xf8);
    out.extend_from_slice(&[0x00, 0x00]);
    out.push(0x0c | length_size_minus_one);
    out.push(u8::try_from(arrays.len()).ok()?);
    for (nal_unit_type, units) in arrays {
        out.push(0x80 | nal_unit_type);
        out.extend_from_slice(&u16::try_from(units.len()).ok()?.to_be_bytes());
        for unit in units {
            out.extend_from_slice(&u16::try_from(unit.len()).ok()?.to_be_bytes());
            out.extend_from_slice(unit);
        }
    }
    Some(out)
}

#[cfg(not(target_os = "macos"))]
fn platform_encode_h264_videotoolbox_bgra_frame(
    _format: RawVideoFormat,
    _bgra: &[u8],
    _bitrate: u32,
) -> Result<EncodedVideoOutput, VideoEncodeError> {
    Err(VideoEncodeError::BackendUnavailable {
        reason: "VideoToolbox H.264 encode is only available on macOS".to_string(),
    })
}

#[cfg(not(target_os = "macos"))]
fn platform_encode_hevc_videotoolbox_bgra_frame(
    _format: RawVideoFormat,
    _bgra: &[u8],
    _bitrate: u32,
) -> Result<EncodedVideoOutput, VideoEncodeError> {
    Err(VideoEncodeError::BackendUnavailable {
        reason: "VideoToolbox HEVC encode is only available on macOS".to_string(),
    })
}

#[cfg(not(target_os = "macos"))]
fn platform_probe_videotoolbox_h264_session(
    _format: RawVideoFormat,
) -> Result<VideoEncodeSessionInfo, VideoEncodeError> {
    Err(VideoEncodeError::BackendUnavailable {
        reason: "VideoToolbox H.264 encode is only available on macOS".to_string(),
    })
}

#[cfg(not(target_os = "macos"))]
fn platform_probe_videotoolbox_hevc_session(
    _format: RawVideoFormat,
) -> Result<VideoEncodeSessionInfo, VideoEncodeError> {
    Err(VideoEncodeError::BackendUnavailable {
        reason: "VideoToolbox HEVC encode is only available on macOS".to_string(),
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

    #[test]
    fn rejects_wrong_bgra_byte_count() {
        let err = encode_h264_videotoolbox_bgra_frame(smoke_format(), &[0; 3], 500_000)
            .expect_err("reject bad BGRA frame size");

        assert!(err.to_string().contains("BGRA"));
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

    #[cfg(target_os = "macos")]
    #[test]
    fn macos_prepares_videotoolbox_hevc_session() {
        let info =
            probe_videotoolbox_hevc_session(smoke_format()).expect("prepare HEVC VT session");

        assert_eq!(info.codec, VideoCodec::Hevc);
        assert_eq!(info.encoder, "chroma-videotoolbox-hevc");
        assert!(info.prepared);
    }

    #[cfg(target_os = "macos")]
    #[test]
    fn macos_encodes_bgra_frame_to_h264() {
        let format = smoke_format();
        let bgra = vec![0; format.width as usize * format.height as usize * 4];

        let encoded = encode_h264_videotoolbox_bgra_frame(format, &bgra, 500_000)
            .expect("encode H.264 frame");

        assert_eq!(encoded.stream.codec, VideoCodec::H264);
        assert_eq!(encoded.stream.width, format.width);
        assert!(!encoded.frames.is_empty());
        assert!(encoded.frames.iter().all(|frame| !frame.payload.is_empty()));
        assert!(
            encoded
                .stream
                .decoder_config
                .as_deref()
                .is_some_and(|config| {
                    config.first() == Some(&1) && config.windows(2).any(|w| w == [0xe1, 0x00])
                })
        );
    }

    #[cfg(target_os = "macos")]
    #[test]
    fn macos_h264_session_encodes_multiple_batches() {
        let format = smoke_format();
        let bgra = vec![0; format.width as usize * format.height as usize * 4];
        let scale = TimeScale {
            units_per_second: 24,
        };
        let mut session = VideoToolboxH264EncoderSession::new(format, 500_000)
            .expect("create retained H.264 session");

        for index in 0..2 {
            let frames = [RawVideoFrameRef {
                pts: TimePoint {
                    units: index,
                    scale,
                },
                dts: TimePoint {
                    units: index,
                    scale,
                },
                duration: TimeDelta { units: 1, scale },
                bytes: &bgra,
                keyframe: true,
            }];
            let encoded = session.encode(&frames).expect("encode retained batch");
            assert!(!encoded.frames.is_empty());
        }

        assert_eq!(session.encoded_batches(), 2);
    }

    #[cfg(target_os = "macos")]
    #[test]
    fn macos_encodes_bgra_frame_to_hevc() {
        let format = smoke_format();
        let bgra = vec![0; format.width as usize * format.height as usize * 4];

        let encoded =
            encode_hevc_videotoolbox_bgra_frame(format, &bgra, 500_000).expect("encode HEVC frame");

        assert_eq!(encoded.stream.codec, VideoCodec::Hevc);
        assert_eq!(encoded.stream.width, format.width);
        assert!(!encoded.frames.is_empty());
        assert!(encoded.frames.iter().all(|frame| !frame.payload.is_empty()));
        let config = encoded.stream.decoder_config.as_deref().expect("hvcC");
        let parsed = crate::codec::hevc::parse_hevc_decoder_config(config).expect("parse hvcC");
        assert_eq!(parsed.nalu_length_size, 4);
        assert!(parsed.arrays.iter().any(|array| array.nal_unit_type == 32));
        assert!(parsed.arrays.iter().any(|array| array.nal_unit_type == 33));
        assert!(parsed.arrays.iter().any(|array| array.nal_unit_type == 34));
    }

    #[cfg(not(target_os = "macos"))]
    #[test]
    fn non_macos_reports_backend_unavailable() {
        let err =
            probe_videotoolbox_h264_session(smoke_format()).expect_err("no VideoToolbox backend");

        assert!(matches!(err, VideoEncodeError::BackendUnavailable { .. }));

        let err =
            probe_videotoolbox_hevc_session(smoke_format()).expect_err("no VideoToolbox backend");

        assert!(matches!(err, VideoEncodeError::BackendUnavailable { .. }));
    }
}
