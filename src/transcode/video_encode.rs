use serde::{Deserialize, Serialize};
use thiserror::Error;

use crate::packet::{TimeDelta, TimePoint, TimeScale};
use crate::transcode::VideoCodec;

#[cfg(all(target_os = "linux", feature = "linux-vaapi"))]
mod vaapi_encode;

#[cfg(all(target_os = "linux", feature = "linux-vaapi"))]
pub use vaapi_encode::VaapiH264EncoderSession;
#[cfg(all(target_os = "linux", feature = "linux-vaapi"))]
pub use vaapi_encode::VaapiHevcEncoderSession;

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
    session: objc2_core_foundation::CFRetained<objc2_video_toolbox::VTCompressionSession>,
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

/// Retained cross-platform OpenH264 CPU encoder for a sequence of frame batches.
pub struct CpuH264EncoderSession {
    format: RawVideoFormat,
    bitrate: u32,
    encoded_batches: u64,
    encoded_frames: u64,
    decoder_config: Option<Vec<u8>>,
    encoder: openh264::encoder::Encoder,
}

impl std::fmt::Debug for CpuH264EncoderSession {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("CpuH264EncoderSession")
            .field("format", &self.format)
            .field("bitrate", &self.bitrate)
            .field("encoded_batches", &self.encoded_batches)
            .field("encoded_frames", &self.encoded_frames)
            .finish_non_exhaustive()
    }
}

impl CpuH264EncoderSession {
    /// Creates one portable H.264 software encoder that can serve multiple batches.
    pub fn new(format: RawVideoFormat, bitrate: u32) -> Result<Self, VideoEncodeError> {
        validate_raw_video_format(format)?;
        validate_openh264_format(format)?;
        if bitrate == 0 {
            return Err(VideoEncodeError::InvalidInput {
                reason: "bitrate must be greater than zero".to_string(),
            });
        }
        let frame_rate = format.frame_rate_num as f32 / format.frame_rate_den as f32;
        let config = openh264::encoder::EncoderConfig::new()
            .bitrate(openh264::encoder::BitRate::from_bps(bitrate))
            .max_frame_rate(openh264::encoder::FrameRate::from_hz(frame_rate))
            .rate_control_mode(openh264::encoder::RateControlMode::Bitrate)
            .profile(openh264::encoder::Profile::Main)
            .skip_frames(true)
            .vui(openh264::encoder::VuiConfig::bt709());
        let encoder = openh264::encoder::Encoder::with_api_config(
            openh264::OpenH264API::from_source(),
            config,
        )
        .map_err(|error| VideoEncodeError::BackendFailed {
            reason: format!("OpenH264 encoder initialization failed: {error}"),
        })?;
        Ok(Self {
            format,
            bitrate,
            encoded_batches: 0,
            encoded_frames: 0,
            decoder_config: None,
            encoder,
        })
    }

    /// Encodes one ordered BGRA frame batch without recreating the software encoder.
    pub fn encode(
        &mut self,
        frames: &[RawVideoFrameRef<'_>],
    ) -> Result<EncodedVideoOutput, VideoEncodeError> {
        validate_raw_video_frames(self.format, frames)?;
        let mut encoded_frames = Vec::with_capacity(frames.len());
        for frame in frames {
            if frame.keyframe && self.encoded_frames > 0 {
                self.encoder.force_intra_frame();
            }
            let bgra = openh264::formats::BgraSliceU8::new(
                frame.bytes,
                (self.format.width as usize, self.format.height as usize),
            );
            let yuv = openh264::formats::YUVBuffer::from_bgra8_source(bgra);
            let timestamp = openh264_timestamp(frame.pts);
            let bitstream = self.encoder.encode_at(&yuv, timestamp).map_err(|error| {
                VideoEncodeError::BackendFailed {
                    reason: format!("OpenH264 frame encode failed: {error}"),
                }
            })?;
            let access_unit = copy_openh264_access_unit(&bitstream)?;
            if !access_unit.parameter_sets.is_empty() {
                self.decoder_config = build_avc_decoder_config(&access_unit.parameter_sets, 4);
            }
            encoded_frames.push(EncodedVideoFrame {
                pts: frame.pts,
                dts: frame.dts,
                duration: frame.duration,
                payload: access_unit.payload,
                keyframe: access_unit.keyframe,
            });
            self.encoded_frames = self.encoded_frames.saturating_add(1);
        }
        let decoder_config =
            self.decoder_config
                .clone()
                .ok_or_else(|| VideoEncodeError::BackendFailed {
                    reason: "OpenH264 emitted no SPS/PPS decoder configuration".to_string(),
                })?;
        self.encoded_batches = self.encoded_batches.saturating_add(1);
        Ok(EncodedVideoOutput {
            stream: EncodedVideoStream {
                codec: VideoCodec::H264,
                width: self.format.width,
                height: self.format.height,
                time_scale: frames[0].pts.scale,
                decoder_config: Some(decoder_config),
            },
            frames: encoded_frames,
        })
    }

    /// Returns the number of batches encoded by this software session.
    pub fn encoded_batches(&self) -> u64 {
        self.encoded_batches
    }
}

/// Preferred retained H.264 encoder, with platform hardware first and CPU fallback.
#[derive(Debug)]
pub enum H264EncoderSession {
    /// Apple VideoToolbox encoder selected on macOS.
    VideoToolbox(VideoToolboxH264EncoderSession),
    /// Portable OpenH264 software encoder.
    Cpu(Box<CpuH264EncoderSession>),
    /// Linux VA-API H.264 encoder selected when a compatible render node is available.
    #[cfg(all(target_os = "linux", feature = "linux-vaapi"))]
    Vaapi(Box<VaapiH264EncoderSession>),
}

impl H264EncoderSession {
    /// Creates the preferred executable H.264 encoder for the current host.
    pub fn new(format: RawVideoFormat, bitrate: u32) -> Result<Self, VideoEncodeError> {
        #[cfg(target_os = "macos")]
        if let Ok(session) = VideoToolboxH264EncoderSession::new(format, bitrate) {
            return Ok(Self::VideoToolbox(session));
        }
        #[cfg(all(target_os = "linux", feature = "linux-vaapi"))]
        if let Ok(session) = VaapiH264EncoderSession::new(format, bitrate) {
            return Ok(Self::Vaapi(Box::new(session)));
        }
        CpuH264EncoderSession::new(format, bitrate).map(|session| Self::Cpu(Box::new(session)))
    }

    /// Encodes one ordered frame batch using the selected retained backend.
    pub fn encode(
        &mut self,
        frames: &[RawVideoFrameRef<'_>],
    ) -> Result<EncodedVideoOutput, VideoEncodeError> {
        match self {
            Self::VideoToolbox(session) => session.encode(frames),
            Self::Cpu(session) => session.encode(frames),
            #[cfg(all(target_os = "linux", feature = "linux-vaapi"))]
            Self::Vaapi(session) => session.encode(frames),
        }
    }

    /// Returns the number of batches encoded by the selected backend.
    pub fn encoded_batches(&self) -> u64 {
        match self {
            Self::VideoToolbox(session) => session.encoded_batches(),
            Self::Cpu(session) => session.encoded_batches(),
            #[cfg(all(target_os = "linux", feature = "linux-vaapi"))]
            Self::Vaapi(session) => session.encoded_batches(),
        }
    }

    /// Returns the stable backend identifier selected for this session.
    pub fn backend_name(&self) -> &'static str {
        match self {
            Self::VideoToolbox(_) => "chroma-videotoolbox-h264",
            Self::Cpu(_) => "chroma-cpu-h264",
            #[cfg(all(target_os = "linux", feature = "linux-vaapi"))]
            Self::Vaapi(_) => "chroma-vaapi-h264",
        }
    }
}

#[derive(Debug)]
struct OpenH264AccessUnit {
    payload: Vec<u8>,
    parameter_sets: Vec<Vec<u8>>,
    keyframe: bool,
}

fn copy_openh264_access_unit(
    bitstream: &openh264::encoder::EncodedBitStream<'_>,
) -> Result<OpenH264AccessUnit, VideoEncodeError> {
    let mut payload = Vec::new();
    let mut parameter_sets = Vec::new();
    let mut keyframe = false;
    for layer_index in 0..bitstream.num_layers() {
        let layer =
            bitstream
                .layer(layer_index)
                .ok_or_else(|| VideoEncodeError::BackendFailed {
                    reason: "OpenH264 returned an invalid layer index".to_string(),
                })?;
        for nal_index in 0..layer.nal_count() {
            let encoded_nal =
                layer
                    .nal_unit(nal_index)
                    .ok_or_else(|| VideoEncodeError::BackendFailed {
                        reason: "OpenH264 returned an invalid NAL index".to_string(),
                    })?;
            let nal = strip_annex_b_start_code(encoded_nal).ok_or_else(|| {
                VideoEncodeError::BackendFailed {
                    reason: "OpenH264 returned an empty or malformed Annex-B NAL unit".to_string(),
                }
            })?;
            match nal[0] & 0x1f {
                7 | 8 => parameter_sets.push(nal.to_vec()),
                9 => {}
                nal_type => {
                    keyframe |= nal_type == 5;
                    let nal_len =
                        u32::try_from(nal.len()).map_err(|_| VideoEncodeError::BackendFailed {
                            reason: "OpenH264 NAL unit exceeds the AVCC size limit".to_string(),
                        })?;
                    payload.extend_from_slice(&nal_len.to_be_bytes());
                    payload.extend_from_slice(nal);
                }
            }
        }
    }
    if payload.is_empty() {
        return Err(VideoEncodeError::BackendFailed {
            reason: format!(
                "OpenH264 emitted no video NAL units for {:?} frame",
                bitstream.frame_type()
            ),
        });
    }
    Ok(OpenH264AccessUnit {
        payload,
        parameter_sets,
        keyframe,
    })
}

fn strip_annex_b_start_code(encoded_nal: &[u8]) -> Option<&[u8]> {
    let nal = if encoded_nal.starts_with(&[0, 0, 0, 1]) {
        &encoded_nal[4..]
    } else if encoded_nal.starts_with(&[0, 0, 1]) {
        &encoded_nal[3..]
    } else {
        return None;
    };
    (!nal.is_empty()).then_some(nal)
}

fn openh264_timestamp(pts: TimePoint) -> openh264::Timestamp {
    let millis = u128::from(pts.units)
        .saturating_mul(1_000)
        .checked_div(u128::from(pts.scale.units_per_second))
        .unwrap_or(0)
        .min(i64::MAX as u128) as u64;
    openh264::Timestamp::from_millis(millis)
}

fn validate_openh264_format(format: RawVideoFormat) -> Result<(), VideoEncodeError> {
    if !format.width.is_multiple_of(2) || !format.height.is_multiple_of(2) {
        return Err(VideoEncodeError::InvalidInput {
            reason: "OpenH264 requires even width and height for I420 input".to_string(),
        });
    }
    let greater_dimension = format.width.max(format.height);
    let smaller_dimension = format.width.min(format.height);
    if greater_dimension > 3_840 || smaller_dimension > 2_160 {
        return Err(VideoEncodeError::InvalidInput {
            reason: "OpenH264 supports at most 3840x2160 landscape or 2160x3840 portrait"
                .to_string(),
        });
    }
    Ok(())
}

#[cfg(target_os = "macos")]
impl Drop for VideoToolboxH264EncoderSession {
    fn drop(&mut self) {
        #[allow(unsafe_code)]
        // SAFETY: The retained session is valid until this owner is dropped.
        unsafe {
            self.session.invalidate();
        }
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

/// Encodes a contiguous BGRA frame batch to H.264 using portable CPU software.
pub fn encode_h264_cpu_bgra_frames(
    format: RawVideoFormat,
    frames: &[RawVideoFrameRef<'_>],
    bitrate: u32,
) -> Result<EncodedVideoOutput, VideoEncodeError> {
    CpuH264EncoderSession::new(format, bitrate)?.encode(frames)
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
            reason: "dimensions exceed native encoder integer limits".to_string(),
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
    let session =
        new_compression_session(format, objc2_core_media::kCMVideoCodecType_H264, "H.264")?;
    prepare_compression_session(&session, "H.264")?;
    #[allow(unsafe_code)]
    // SAFETY: The session remains retained for the duration of this call.
    unsafe {
        session.invalidate();
    }

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
    let session =
        new_compression_session(format, objc2_core_media::kCMVideoCodecType_HEVC, "HEVC")?;
    prepare_compression_session(&session, "HEVC")?;
    #[allow(unsafe_code)]
    // SAFETY: The session remains retained for the duration of this call.
    unsafe {
        session.invalidate();
    }

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
fn new_compression_session(
    format: RawVideoFormat,
    codec: objc2_core_media::CMVideoCodecType,
    label: &str,
) -> Result<
    objc2_core_foundation::CFRetained<objc2_video_toolbox::VTCompressionSession>,
    VideoEncodeError,
> {
    use std::ptr::{NonNull, null_mut};

    let mut raw = null_mut();
    #[allow(unsafe_code)]
    // SAFETY: All optional configuration pointers are null and `raw` is a valid
    // out-parameter. A successful Create call returns a +1 retained session.
    let status = unsafe {
        objc2_video_toolbox::VTCompressionSession::create(
            None,
            format.width as i32,
            format.height as i32,
            codec,
            None,
            None,
            None,
            None,
            null_mut(),
            NonNull::from(&mut raw),
        )
    };
    if status != 0 {
        return Err(VideoEncodeError::BackendFailed {
            reason: format!("VTCompressionSessionCreate({label}) returned {status}"),
        });
    }
    let raw = NonNull::new(raw).ok_or_else(|| VideoEncodeError::BackendFailed {
        reason: format!("VTCompressionSessionCreate({label}) returned no session"),
    })?;
    #[allow(unsafe_code)]
    // SAFETY: VideoToolbox returned this pointer at +1 under the Create rule.
    Ok(unsafe { objc2_core_foundation::CFRetained::from_raw(raw) })
}

#[cfg(target_os = "macos")]
fn prepare_compression_session(
    session: &objc2_video_toolbox::VTCompressionSession,
    label: &str,
) -> Result<(), VideoEncodeError> {
    #[allow(unsafe_code)]
    // SAFETY: `session` is a live VideoToolbox compression session.
    let status = unsafe { session.prepare_to_encode_frames() };
    (status == 0)
        .then_some(())
        .ok_or_else(|| VideoEncodeError::BackendFailed {
            reason: format!("VTCompressionSessionPrepareToEncodeFrames({label}) returned {status}"),
        })
}

#[cfg(target_os = "macos")]
fn platform_encode_h264_videotoolbox_bgra_frame(
    format: RawVideoFormat,
    bgra: &[u8],
    bitrate: u32,
) -> Result<EncodedVideoOutput, VideoEncodeError> {
    let mut session = VideoToolboxH264EncoderSession::new(format, bitrate)?;
    let time_scale = TimeScale {
        units_per_second: format.frame_rate_num,
    };
    session.encode(&[RawVideoFrameRef {
        pts: TimePoint {
            units: 0,
            scale: time_scale,
        },
        dts: TimePoint {
            units: 0,
            scale: time_scale,
        },
        duration: TimeDelta {
            units: u64::from(format.frame_rate_den),
            scale: time_scale,
        },
        bytes: bgra,
        keyframe: true,
    }])
}

#[cfg(target_os = "macos")]
fn platform_new_h264_encoder_session(
    format: RawVideoFormat,
    bitrate: u32,
) -> Result<VideoToolboxH264EncoderSession, VideoEncodeError> {
    let session = new_compression_session(
        format,
        objc2_core_media::kCMVideoCodecType_H264,
        "H.264 batch",
    )?;
    #[allow(unsafe_code)]
    // SAFETY: These framework constants are immutable process-lifetime CFStrings.
    unsafe {
        set_vt_property_bool(
            &session,
            objc2_video_toolbox::kVTCompressionPropertyKey_RealTime,
            true,
        )?;
        set_vt_property_bool(
            &session,
            objc2_video_toolbox::kVTCompressionPropertyKey_AllowFrameReordering,
            false,
        )?;
        set_vt_property_i32(
            &session,
            objc2_video_toolbox::kVTCompressionPropertyKey_AverageBitRate,
            i32::try_from(bitrate).unwrap_or(i32::MAX),
        )?;
        set_vt_property_i32(
            &session,
            objc2_video_toolbox::kVTCompressionPropertyKey_MaxFrameDelayCount,
            0,
        )?;
        set_vt_property_i32(
            &session,
            objc2_video_toolbox::kVTCompressionPropertyKey_ExpectedFrameRate,
            i32::try_from(format.frame_rate_num / format.frame_rate_den.max(1)).unwrap_or(i32::MAX),
        )?;
    }
    prepare_compression_session(&session, "H.264 batch")?;
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

    use block2::RcBlock;
    use objc2_core_media::{CMSampleBuffer, CMTime};

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
        #[allow(unsafe_code)]
        // SAFETY: Erasing the dictionary's generic key/value markers does not
        // alter its Core Foundation representation.
        let frame_properties = frame_properties
            .as_deref()
            .map(|dictionary| unsafe { dictionary.cast_unchecked() });

        let handler = RcBlock::new(
            move |status: i32, _flags, sample_buffer_ref: *mut CMSampleBuffer| {
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
                let Some(sample_buffer) =
                    std::ptr::NonNull::new(sample_buffer_ref).map(|value| value.as_ptr())
                else {
                    set_encode_callback_error(
                        &error_out,
                        VideoEncodeError::BackendFailed {
                            reason:
                                "VideoToolbox H.264 batch encode callback returned no sample buffer"
                                    .to_string(),
                        },
                    );
                    return;
                };
                #[allow(unsafe_code)]
                // SAFETY: VideoToolbox owns the sample for the callback duration;
                // `copy_h264_sample_objc2` copies all referenced bytes immediately.
                let copied = unsafe { copy_h264_sample_objc2(&*sample_buffer) };
                let Some((payload, config)) = copied else {
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
                            reason: "VideoToolbox H.264 batch frame output lock was poisoned"
                                .to_string(),
                        },
                    ),
                }
            },
        );
        #[allow(unsafe_code)]
        // SAFETY: The image buffer and optional properties remain alive for the
        // submission, and VideoToolbox copies the escaping block as required.
        let status = unsafe {
            session.encode_frame_with_output_handler(
                image_buffer,
                CMTime::new(
                    i64::try_from(pts.units).unwrap_or(i64::MAX),
                    pts.scale.units_per_second as i32,
                ),
                CMTime::new(
                    i64::try_from(duration.units).unwrap_or(i64::MAX),
                    duration.scale.units_per_second as i32,
                ),
                frame_properties,
                std::ptr::null_mut(),
                std::ptr::from_ref(&*handler).cast_mut(),
            )
        };
        if status != 0 {
            return Err(VideoEncodeError::BackendFailed {
                reason: format!("VTCompressionSessionEncodeFrame(H.264 batch) returned {status}"),
            });
        }
    }
    #[allow(unsafe_code)]
    // SAFETY: The session is live and the timestamp uses a positive timescale.
    let status = unsafe {
        session.complete_frames(CMTime::new(
            i64::MAX,
            input_frames[0].pts.scale.units_per_second as i32,
        ))
    };
    if status != 0 {
        return Err(VideoEncodeError::BackendFailed {
            reason: format!("VTCompressionSessionCompleteFrames(H.264 batch) returned {status}"),
        });
    }
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
    session: &objc2_video_toolbox::VTCompressionSession,
    key: &objc2_core_foundation::CFString,
    value: bool,
) -> Result<(), VideoEncodeError> {
    let value = objc2_core_foundation::CFBoolean::new(value);
    set_vt_property(session, key, value.as_ref())
}

#[cfg(target_os = "macos")]
fn set_vt_property_i32(
    session: &objc2_video_toolbox::VTCompressionSession,
    key: &objc2_core_foundation::CFString,
    value: i32,
) -> Result<(), VideoEncodeError> {
    let value = objc2_core_foundation::CFNumber::new_i32(value);
    set_vt_property(session, key, value.as_ref())
}

#[cfg(target_os = "macos")]
fn set_vt_property(
    session: &objc2_video_toolbox::VTCompressionSession,
    key: &objc2_core_foundation::CFString,
    value: &objc2_core_foundation::CFType,
) -> Result<(), VideoEncodeError> {
    #[allow(unsafe_code)]
    // SAFETY: VTCompressionSession is one of the documented concrete VTSession
    // kinds, and the key/value pairs use the types required by VideoToolbox.
    let status = unsafe {
        let vt_session = &*(std::ptr::from_ref(session).cast::<objc2_video_toolbox::VTSession>());
        objc2_video_toolbox::VTSessionSetProperty(vt_session, key, Some(value))
    };
    if status == 0 || status == -12900 {
        Ok(())
    } else {
        Err(VideoEncodeError::BackendFailed {
            reason: format!("VTSessionSetProperty returned {status}"),
        })
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
fn force_keyframe_dictionary() -> objc2_core_foundation::CFRetained<
    objc2_core_foundation::CFDictionary<
        objc2_core_foundation::CFString,
        objc2_core_foundation::CFType,
    >,
> {
    #[allow(unsafe_code)]
    // SAFETY: The framework key is an immutable process-lifetime CFString.
    let key = unsafe { objc2_video_toolbox::kVTEncodeFrameOptionKey_ForceKeyFrame };
    let value = objc2_core_foundation::CFBoolean::new(true);
    objc2_core_foundation::CFDictionary::from_slices(&[key], &[value.as_ref()])
}

#[cfg(target_os = "macos")]
fn platform_encode_hevc_videotoolbox_bgra_frame(
    format: RawVideoFormat,
    bgra: &[u8],
    _bitrate: u32,
) -> Result<EncodedVideoOutput, VideoEncodeError> {
    encode_single_frame_objc2(format, bgra, VideoCodec::Hevc)
}

#[cfg(target_os = "macos")]
fn encode_single_frame_objc2(
    format: RawVideoFormat,
    bgra: &[u8],
    codec: VideoCodec,
) -> Result<EncodedVideoOutput, VideoEncodeError> {
    use std::sync::{Arc, Mutex};

    use block2::RcBlock;
    use objc2_core_media::{CMSampleBuffer, CMTime};

    let (codec_type, label) = match codec {
        VideoCodec::H264 => (objc2_core_media::kCMVideoCodecType_H264, "H.264"),
        VideoCodec::Hevc => (objc2_core_media::kCMVideoCodecType_HEVC, "HEVC"),
        VideoCodec::Av1 => {
            return Err(VideoEncodeError::InvalidInput {
                reason: "AV1 is a decode-only codec in this encoder".to_string(),
            });
        }
    };
    let session = new_compression_session(format, codec_type, label)?;
    prepare_compression_session(&session, label)?;
    let pixel_buffer = bgra_pixel_buffer(format, bgra)?;
    let image_buffer = image_buffer_from_pixel_buffer(&pixel_buffer);
    let result = Arc::new(Mutex::new(None));
    let callback_error = Arc::new(Mutex::new(None));
    let result_out = Arc::clone(&result);
    let error_out = Arc::clone(&callback_error);
    let handler = RcBlock::new(
        move |status: i32, _flags, sample_buffer_ref: *mut CMSampleBuffer| {
            if status != 0 {
                set_encode_callback_error(
                    &error_out,
                    VideoEncodeError::BackendFailed {
                        reason: format!("VideoToolbox {label} encode callback returned {status}"),
                    },
                );
                return;
            }
            let Some(sample_buffer) = std::ptr::NonNull::new(sample_buffer_ref) else {
                set_encode_callback_error(
                    &error_out,
                    VideoEncodeError::BackendFailed {
                        reason: format!(
                            "VideoToolbox {label} encode callback returned no sample buffer"
                        ),
                    },
                );
                return;
            };
            #[allow(unsafe_code)]
            // SAFETY: The callback owns a valid borrowed sample buffer for its
            // duration and the helper copies every referenced byte.
            let copied = unsafe {
                match codec {
                    VideoCodec::H264 => copy_h264_sample_objc2(sample_buffer.as_ref()),
                    VideoCodec::Hevc => copy_hevc_sample_objc2(sample_buffer.as_ref()),
                    VideoCodec::Av1 => None,
                }
            };
            match copied {
                Some(value) => {
                    if let Ok(mut slot) = result_out.lock() {
                        *slot = Some(value);
                    }
                }
                None => set_encode_callback_error(
                    &error_out,
                    VideoEncodeError::BackendFailed {
                        reason: format!("VideoToolbox {label} sample buffer copy failed"),
                    },
                ),
            }
        },
    );
    #[allow(unsafe_code)]
    // SAFETY: Inputs remain live through submission and VideoToolbox copies the
    // escaping handler. The timestamp timescale is validated as non-zero.
    let status = unsafe {
        session.encode_frame_with_output_handler(
            image_buffer,
            CMTime::new(0, format.frame_rate_num as i32),
            CMTime::new(
                i64::from(format.frame_rate_den),
                format.frame_rate_num as i32,
            ),
            None,
            std::ptr::null_mut(),
            std::ptr::from_ref(&*handler).cast_mut(),
        )
    };
    if status != 0 {
        return Err(VideoEncodeError::BackendFailed {
            reason: format!("VTCompressionSessionEncodeFrame({label}) returned {status}"),
        });
    }
    #[allow(unsafe_code)]
    // SAFETY: The session is live and the timestamp has a positive timescale.
    let status =
        unsafe { session.complete_frames(CMTime::new(i64::MAX, format.frame_rate_num as i32)) };
    #[allow(unsafe_code)]
    // SAFETY: This deterministically tears down the live session.
    unsafe {
        session.invalidate();
    }
    if status != 0 {
        return Err(VideoEncodeError::BackendFailed {
            reason: format!("VTCompressionSessionCompleteFrames({label}) returned {status}"),
        });
    }
    if let Some(error) = take_encode_callback_error(&callback_error)? {
        return Err(error);
    }
    let (payload, decoder_config) = result
        .lock()
        .map_err(|_| VideoEncodeError::BackendFailed {
            reason: "VideoToolbox frame output lock was poisoned".to_string(),
        })?
        .clone()
        .ok_or_else(|| VideoEncodeError::BackendFailed {
            reason: format!("VideoToolbox emitted no {label} sample buffers"),
        })?;
    let time_scale = TimeScale {
        units_per_second: format.frame_rate_num,
    };
    Ok(EncodedVideoOutput {
        stream: EncodedVideoStream {
            codec,
            width: format.width,
            height: format.height,
            time_scale,
            decoder_config,
        },
        frames: vec![EncodedVideoFrame {
            pts: TimePoint {
                units: 0,
                scale: time_scale,
            },
            dts: TimePoint {
                units: 0,
                scale: time_scale,
            },
            duration: TimeDelta {
                units: u64::from(format.frame_rate_den),
                scale: time_scale,
            },
            payload,
            keyframe: true,
        }],
    })
}

#[cfg(target_os = "macos")]
fn image_buffer_from_pixel_buffer(
    pixel_buffer: &objc2_core_video::CVPixelBuffer,
) -> &objc2_core_video::CVImageBuffer {
    #[allow(unsafe_code)]
    // SAFETY: CVPixelBuffer is a concrete CVImageBuffer subtype and the returned
    // borrow cannot outlive the source pixel buffer.
    unsafe {
        &*(std::ptr::from_ref(pixel_buffer).cast::<objc2_core_video::CVImageBuffer>())
    }
}

#[cfg(target_os = "macos")]
fn bgra_pixel_buffer(
    format: RawVideoFormat,
    bgra: &[u8],
) -> Result<objc2_core_foundation::CFRetained<objc2_core_video::CVPixelBuffer>, VideoEncodeError> {
    use std::ptr::{NonNull, null_mut};

    let mut raw = null_mut();
    #[allow(unsafe_code)]
    // SAFETY: `raw` is a valid out-parameter and the dimensions and pixel
    // format were validated before this helper is called.
    let status = unsafe {
        objc2_core_video::CVPixelBufferCreate(
            None,
            format.width as usize,
            format.height as usize,
            objc2_core_video::kCVPixelFormatType_32BGRA,
            None,
            NonNull::from(&mut raw),
        )
    };
    if status != objc2_core_video::kCVReturnSuccess {
        return Err(VideoEncodeError::BackendFailed {
            reason: format!("CVPixelBufferCreate(BGRA) returned {status}"),
        });
    }
    let raw = NonNull::new(raw).ok_or_else(|| VideoEncodeError::BackendFailed {
        reason: "CVPixelBufferCreate(BGRA) returned no pixel buffer".to_string(),
    })?;
    #[allow(unsafe_code)]
    // SAFETY: CoreVideo returned this object at +1 under the Create rule.
    let pixel_buffer = unsafe { objc2_core_foundation::CFRetained::from_raw(raw) };
    let flags = objc2_core_video::CVPixelBufferLockFlags::empty();
    #[allow(unsafe_code)]
    // SAFETY: The retained pixel buffer is valid and not otherwise CPU-locked.
    let status = unsafe { objc2_core_video::CVPixelBufferLockBaseAddress(&pixel_buffer, flags) };
    if status != objc2_core_video::kCVReturnSuccess {
        return Err(VideoEncodeError::BackendFailed {
            reason: format!("CVPixelBufferLockBaseAddress returned {status}"),
        });
    }
    let bytes_per_row = objc2_core_video::CVPixelBufferGetBytesPerRow(&pixel_buffer);
    let row_bytes = format.width as usize * 4;
    #[allow(unsafe_code)]
    // SAFETY: The pixel buffer is locked for CPU writes, `base` points to at
    // least `bytes_per_row * height` bytes owned by CoreVideo, and each source
    // row is exactly `row_bytes` bytes from the validated BGRA frame slice.
    unsafe {
        let base = objc2_core_video::CVPixelBufferGetBaseAddress(&pixel_buffer).cast::<u8>();
        if base.is_null() || bytes_per_row < row_bytes {
            let _ = objc2_core_video::CVPixelBufferUnlockBaseAddress(&pixel_buffer, flags);
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
    #[allow(unsafe_code)]
    // SAFETY: This balances the successful lock above.
    let status = unsafe { objc2_core_video::CVPixelBufferUnlockBaseAddress(&pixel_buffer, flags) };
    if status != objc2_core_video::kCVReturnSuccess {
        return Err(VideoEncodeError::BackendFailed {
            reason: format!("CVPixelBufferUnlockBaseAddress returned {status}"),
        });
    }
    Ok(pixel_buffer)
}

#[cfg(target_os = "macos")]
#[allow(unsafe_code)]
unsafe fn copy_h264_sample_objc2(
    sample_buffer: &objc2_core_media::CMSampleBuffer,
) -> Option<(Vec<u8>, Option<Vec<u8>>)> {
    let data_buffer = unsafe { sample_buffer.data_buffer() }?;
    let length = unsafe { data_buffer.data_length() };
    let mut payload = vec![0; length];
    let destination = std::ptr::NonNull::new(payload.as_mut_ptr().cast())?;
    if unsafe { data_buffer.copy_data_bytes(0, length, destination) } != 0 {
        return None;
    }
    let config = unsafe { sample_buffer.format_description() }
        .and_then(|description| unsafe { avc_decoder_config_from_objc2_format(&description) });
    Some((payload, config))
}

#[cfg(target_os = "macos")]
#[allow(unsafe_code)]
unsafe fn avc_decoder_config_from_objc2_format(
    description: &objc2_core_media::CMFormatDescription,
) -> Option<Vec<u8>> {
    let mut first_pointer = std::ptr::null();
    let mut first_size = 0;
    let mut count = 0;
    let mut nal_length = 0;
    if unsafe {
        objc2_core_media::CMVideoFormatDescriptionGetH264ParameterSetAtIndex(
            description,
            0,
            &mut first_pointer,
            &mut first_size,
            &mut count,
            &mut nal_length,
        )
    } != 0
    {
        return None;
    }
    let mut parameter_sets = Vec::with_capacity(count);
    for index in 0..count {
        let mut pointer = std::ptr::null();
        let mut size = 0;
        if unsafe {
            objc2_core_media::CMVideoFormatDescriptionGetH264ParameterSetAtIndex(
                description,
                index,
                &mut pointer,
                &mut size,
                std::ptr::null_mut(),
                std::ptr::null_mut(),
            )
        } != 0
            || pointer.is_null()
        {
            return None;
        }
        parameter_sets.push(unsafe { std::slice::from_raw_parts(pointer, size) }.to_vec());
    }
    build_avc_decoder_config(&parameter_sets, nal_length)
}

#[cfg(target_os = "macos")]
#[allow(unsafe_code)]
unsafe fn copy_hevc_sample_objc2(
    sample_buffer: &objc2_core_media::CMSampleBuffer,
) -> Option<(Vec<u8>, Option<Vec<u8>>)> {
    let data_buffer = unsafe { sample_buffer.data_buffer() }?;
    let length = unsafe { data_buffer.data_length() };
    let mut payload = vec![0; length];
    let destination = std::ptr::NonNull::new(payload.as_mut_ptr().cast())?;
    if unsafe { data_buffer.copy_data_bytes(0, length, destination) } != 0 {
        return None;
    }
    let config = unsafe { sample_buffer.format_description() }
        .and_then(|description| unsafe { hevc_decoder_config_from_objc2_format(&description) });
    Some((payload, config))
}

#[cfg(target_os = "macos")]
#[allow(unsafe_code)]
unsafe fn hevc_decoder_config_from_objc2_format(
    description: &objc2_core_media::CMFormatDescription,
) -> Option<Vec<u8>> {
    let mut first_pointer = std::ptr::null();
    let mut first_size = 0;
    let mut count = 0;
    let mut nal_length = 0;
    if unsafe {
        objc2_core_media::CMVideoFormatDescriptionGetHEVCParameterSetAtIndex(
            description,
            0,
            &mut first_pointer,
            &mut first_size,
            &mut count,
            &mut nal_length,
        )
    } != 0
    {
        return None;
    }
    let mut parameter_sets = Vec::with_capacity(count);
    for index in 0..count {
        let mut pointer = std::ptr::null();
        let mut size = 0;
        if unsafe {
            objc2_core_media::CMVideoFormatDescriptionGetHEVCParameterSetAtIndex(
                description,
                index,
                &mut pointer,
                &mut size,
                std::ptr::null_mut(),
                std::ptr::null_mut(),
            )
        } != 0
            || pointer.is_null()
        {
            return None;
        }
        parameter_sets.push(unsafe { std::slice::from_raw_parts(pointer, size) }.to_vec());
    }
    build_hevc_decoder_config(&parameter_sets, nal_length)
}

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

    #[test]
    fn cpu_h264_encodes_avcc_and_reuses_session() {
        let format = smoke_format();
        let bgra = vec![0; format.width as usize * format.height as usize * 4];
        let scale = TimeScale {
            units_per_second: 24,
        };
        let mut session =
            CpuH264EncoderSession::new(format, 500_000).expect("create retained CPU H.264 session");

        for index in 0..2 {
            let encoded = session
                .encode(&[RawVideoFrameRef {
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
                    keyframe: index == 0,
                }])
                .expect("encode CPU H.264 batch");

            assert_eq!(encoded.frames.len(), 1);
            assert!(!encoded.frames[0].payload.is_empty());
            let config = encoded.stream.decoder_config.as_deref().expect("avcC");
            let parsed = crate::codec::h264::parse_avc_decoder_config(config).expect("parse avcC");
            assert_eq!(parsed.nalu_length_size, 4);
            assert!(!parsed.sps.is_empty());
            assert!(!parsed.pps.is_empty());
            crate::codec::h264::parse_avc_sample_nalus(
                0,
                0,
                &encoded.frames[0].payload,
                parsed.nalu_length_size,
            )
            .expect("parse AVCC sample");
        }

        assert_eq!(session.encoded_batches(), 2);
    }

    #[test]
    fn cpu_h264_rejects_odd_dimensions() {
        let error = CpuH264EncoderSession::new(
            RawVideoFormat {
                width: 127,
                ..smoke_format()
            },
            500_000,
        )
        .expect_err("reject odd width");

        assert!(error.to_string().contains("even width and height"));
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
