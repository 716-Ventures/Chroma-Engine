use serde::{Deserialize, Serialize};
use thiserror::Error;

use crate::{
    packet::{ChunkSample, TimeDelta, TimePoint, TimeScale},
    transcode::{RawVideoFormat, RawVideoPixelFormat, VideoCodec},
};

/// Borrowed compressed packet ready to feed a native video decoder.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct CompressedVideoPacket<'a> {
    /// Source packet index.
    pub index: u32,
    /// Presentation timestamp carried by the packet.
    pub pts: TimePoint,
    /// Decode timestamp carried by the packet.
    pub dts: TimePoint,
    /// Packet duration.
    pub duration: TimeDelta,
    /// True when the packet starts an independently decodable access unit.
    pub keyframe: bool,
    /// Borrowed compressed packet bytes.
    pub bytes: &'a [u8],
}

/// Packet batch and stream metadata sent into a native video decoder.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct VideoDecodeInput<'a> {
    /// Source codec carried by the compressed packets.
    pub codec: VideoCodec,
    /// Decoder time scale for packet timestamps.
    pub time_scale: TimeScale,
    /// Codec-specific decoder configuration bytes from the container.
    pub decoder_config: Option<&'a [u8]>,
    /// Ordered compressed packets.
    pub packets: Vec<CompressedVideoPacket<'a>>,
    /// True when this input is the final batch and the decoder must drain after it.
    pub end_of_stream: bool,
}

/// Stable output description for decoded video frames.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct DecodedVideoStream {
    /// Raw frame format emitted by the decoder.
    pub format: RawVideoFormat,
    /// Source codec decoded into the raw format.
    pub source_codec: VideoCodec,
    /// Backend that produced the frames.
    pub decoder: String,
}

/// One decoded video frame emitted by a native decoder.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct DecodedVideoFrame {
    /// Presentation timestamp.
    pub pts: TimePoint,
    /// Best-effort decode timestamp.
    pub dts: TimePoint,
    /// Frame duration.
    pub duration: TimeDelta,
    /// Raw frame format.
    pub format: RawVideoFormat,
    /// Raw pixel bytes in the declared format.
    pub pixels: Vec<u8>,
    /// True when this frame is independently decodable.
    pub keyframe: bool,
}

/// Decoded video stream plus frames emitted by a backend.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct DecodedVideoOutput {
    /// Output stream description.
    pub stream: DecodedVideoStream,
    /// Decoded frames.
    pub frames: Vec<DecodedVideoFrame>,
}

/// Successful native video decoder session initialization.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct VideoDecodeSessionInfo {
    /// Source codec accepted by the backend.
    pub codec: VideoCodec,
    /// Backend-specific decoder name.
    pub decoder: String,
    /// Session width in pixels.
    pub width: u32,
    /// Session height in pixels.
    pub height: u32,
    /// Raw format requested for decoded frames.
    pub output_format: RawVideoFormat,
    /// Whether the platform reports hardware decode support for the codec.
    pub hardware_supported: bool,
    /// Whether the native decompression session was created.
    pub session_created: bool,
}

/// Decoder pump action used by native video decoder implementations.
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase", tag = "kind")]
pub enum VideoDecoderAction {
    /// Send one compressed packet to the decoder.
    SendPacket {
        /// Source packet index.
        packet_index: u32,
    },
    /// Receive all frames currently available from the decoder.
    ReceiveAvailable,
    /// Signal end-of-stream to the decoder.
    SendDrain,
    /// Receive delayed frames after drain.
    ReceiveDrain,
}

/// Drain state for a decoder pump.
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub enum VideoDecoderDrainState {
    /// Normal packet feeding.
    Feeding,
    /// Drain packet has been sent.
    Draining,
    /// Decoder has no more delayed frames.
    Drained,
}

/// Error returned while preparing video decode input.
#[derive(Debug, Error, PartialEq, Eq)]
pub enum VideoDecodeError {
    /// Packet sample metadata points outside the payload.
    #[error("packet sample byte range is outside the decode payload")]
    PacketOutOfBounds,
    /// The caller provided an unsupported compressed source codec.
    #[error("unsupported video decode codec")]
    UnsupportedCodec,
    /// The caller provided invalid compressed decode input.
    #[error("invalid video decode input: {reason}")]
    InvalidInput {
        /// Diagnostic reason.
        reason: String,
    },
    /// The requested raw video output shape is invalid.
    #[error("invalid decoded video format: {reason}")]
    InvalidOutputFormat {
        /// Diagnostic reason.
        reason: String,
    },
    /// Required codec-specific decoder configuration was not supplied.
    #[error("missing video decoder configuration")]
    MissingDecoderConfig,
    /// Codec-specific decoder configuration could not be parsed.
    #[error("invalid video decoder configuration: {reason}")]
    InvalidDecoderConfig {
        /// Diagnostic reason.
        reason: String,
    },
    /// No native video decode backend is available on this platform.
    #[error("native video decode backend is unavailable: {reason}")]
    BackendUnavailable {
        /// Diagnostic reason.
        reason: String,
    },
    /// The native backend returned an error.
    #[error("native video decode failed: {reason}")]
    BackendFailed {
        /// Diagnostic reason.
        reason: String,
    },
}

/// Creates a native VideoToolbox H.264 decode session from an AVC decoder config.
pub fn probe_videotoolbox_h264_decoder_session(
    format: RawVideoFormat,
    decoder_config: &[u8],
) -> Result<VideoDecodeSessionInfo, VideoDecodeError> {
    validate_decoded_video_format(format)?;
    if decoder_config.is_empty() {
        return Err(VideoDecodeError::MissingDecoderConfig);
    }
    platform_probe_videotoolbox_h264_decoder_session(format, decoder_config)
}

/// Creates a native VideoToolbox HEVC decode session from an HEVC decoder config.
pub fn probe_videotoolbox_hevc_decoder_session(
    format: RawVideoFormat,
    decoder_config: &[u8],
) -> Result<VideoDecodeSessionInfo, VideoDecodeError> {
    validate_decoded_video_format(format)?;
    if decoder_config.is_empty() {
        return Err(VideoDecodeError::MissingDecoderConfig);
    }
    platform_probe_videotoolbox_hevc_decoder_session(format, decoder_config)
}

/// Decodes H.264 or HEVC compressed packets to tightly packed BGRA frames with VideoToolbox.
pub fn decode_videotoolbox_bgra_frames(
    input: &VideoDecodeInput<'_>,
    output_format: RawVideoFormat,
) -> Result<DecodedVideoOutput, VideoDecodeError> {
    validate_decoded_video_format(output_format)?;
    let decoder_config = input
        .decoder_config
        .ok_or(VideoDecodeError::MissingDecoderConfig)?;
    if decoder_config.is_empty() {
        return Err(VideoDecodeError::MissingDecoderConfig);
    }
    platform_decode_videotoolbox_bgra_frames(input, output_format)
}

/// Builds zero-copy compressed video packets from extracted chunk metadata.
pub fn build_video_decode_input<'a>(
    codec: VideoCodec,
    time_scale: TimeScale,
    decoder_config: Option<&'a [u8]>,
    samples: &[ChunkSample],
    payload: &'a [u8],
    end_of_stream: bool,
) -> Result<VideoDecodeInput<'a>, VideoDecodeError> {
    let packets = samples
        .iter()
        .map(|sample| {
            let start = usize::try_from(sample.payload_offset)
                .map_err(|_| VideoDecodeError::PacketOutOfBounds)?;
            let size = usize::try_from(sample.byte_count)
                .map_err(|_| VideoDecodeError::PacketOutOfBounds)?;
            let end = start
                .checked_add(size)
                .ok_or(VideoDecodeError::PacketOutOfBounds)?;
            let bytes = payload
                .get(start..end)
                .ok_or(VideoDecodeError::PacketOutOfBounds)?;
            Ok(CompressedVideoPacket {
                index: sample.index,
                pts: sample.pts,
                dts: sample.dts,
                duration: sample.duration,
                keyframe: sample.keyframe,
                bytes,
            })
        })
        .collect::<Result<Vec<_>, _>>()?;

    Ok(VideoDecodeInput {
        codec,
        time_scale,
        decoder_config,
        packets,
        end_of_stream,
    })
}

/// Returns the bounded send/receive/drain action sequence for a decode input batch.
pub fn decoder_actions_for_input(input: &VideoDecodeInput<'_>) -> Vec<VideoDecoderAction> {
    let mut out =
        Vec::with_capacity((input.packets.len() * 2) + usize::from(input.end_of_stream) * 2);
    for packet in &input.packets {
        out.push(VideoDecoderAction::SendPacket {
            packet_index: packet.index,
        });
        out.push(VideoDecoderAction::ReceiveAvailable);
    }
    if input.end_of_stream {
        out.push(VideoDecoderAction::SendDrain);
        out.push(VideoDecoderAction::ReceiveDrain);
    }
    out
}

/// Validates the raw video format a decoder backend plans to emit.
pub fn validate_decoded_video_format(format: RawVideoFormat) -> Result<(), VideoDecodeError> {
    if format.width == 0 || format.height == 0 {
        return Err(VideoDecodeError::InvalidOutputFormat {
            reason: "decoded video dimensions must be non-zero".to_string(),
        });
    }
    if format.frame_rate_num == 0 || format.frame_rate_den == 0 {
        return Err(VideoDecodeError::InvalidOutputFormat {
            reason: "decoded video frame rate must be non-zero".to_string(),
        });
    }
    if format.pixel_format != RawVideoPixelFormat::Bgra {
        return Err(VideoDecodeError::InvalidOutputFormat {
            reason: "only BGRA decoded output is currently accepted by native encoders".to_string(),
        });
    }
    Ok(())
}

#[cfg(target_os = "macos")]
fn platform_probe_videotoolbox_h264_decoder_session(
    format: RawVideoFormat,
    decoder_config: &[u8],
) -> Result<VideoDecodeSessionInfo, VideoDecodeError> {
    use core_media::format_description::kCMVideoCodecType_H264;
    use video_toolbox::decompression_session::VTDecompressionSession;

    let description = videotoolbox_h264_format_description(decoder_config)?;
    VTDecompressionSession::new(description, None, None).map_err(|status| {
        VideoDecodeError::BackendFailed {
            reason: format!("VTDecompressionSessionCreate(H.264) returned {status}"),
        }
    })?;

    Ok(VideoDecodeSessionInfo {
        codec: VideoCodec::H264,
        decoder: "chroma-videotoolbox-h264-decoder".to_string(),
        width: format.width,
        height: format.height,
        output_format: format,
        hardware_supported: VTDecompressionSession::is_hardware_decode_supported(
            kCMVideoCodecType_H264,
        ),
        session_created: true,
    })
}

#[cfg(target_os = "macos")]
fn platform_probe_videotoolbox_hevc_decoder_session(
    format: RawVideoFormat,
    decoder_config: &[u8],
) -> Result<VideoDecodeSessionInfo, VideoDecodeError> {
    use core_media::format_description::kCMVideoCodecType_HEVC;
    use video_toolbox::decompression_session::VTDecompressionSession;

    let description = videotoolbox_hevc_format_description(decoder_config)?;
    VTDecompressionSession::new(description, None, None).map_err(|status| {
        VideoDecodeError::BackendFailed {
            reason: format!("VTDecompressionSessionCreate(HEVC) returned {status}"),
        }
    })?;

    Ok(VideoDecodeSessionInfo {
        codec: VideoCodec::Hevc,
        decoder: "chroma-videotoolbox-hevc-decoder".to_string(),
        width: format.width,
        height: format.height,
        output_format: format,
        hardware_supported: VTDecompressionSession::is_hardware_decode_supported(
            kCMVideoCodecType_HEVC,
        ),
        session_created: true,
    })
}

#[cfg(target_os = "macos")]
fn platform_decode_videotoolbox_bgra_frames(
    input: &VideoDecodeInput<'_>,
    output_format: RawVideoFormat,
) -> Result<DecodedVideoOutput, VideoDecodeError> {
    use std::{
        collections::BTreeMap,
        sync::{Arc, Mutex},
    };

    use core_foundation::{
        base::TCFType, dictionary::CFDictionary, number::CFNumber, string::CFString,
    };
    use core_media::sample_buffer::{CMSampleBuffer, CMSampleTimingInfo};
    use core_video::pixel_buffer::{CVPixelBufferKeys, kCVPixelFormatType_32BGRA};
    use video_toolbox::{
        decompression_session::VTDecompressionSession, errors::VTDecodeFrameFlags,
    };

    let decoder_config = input
        .decoder_config
        .ok_or(VideoDecodeError::MissingDecoderConfig)?;
    let video_description = match input.codec {
        VideoCodec::H264 => videotoolbox_h264_format_description(decoder_config)?,
        VideoCodec::Hevc => videotoolbox_hevc_format_description(decoder_config)?,
    };
    let format_description = cm_format_description_from_video(&video_description);
    let attrs = CFDictionary::from_CFType_pairs(&[
        (
            CFString::from(CVPixelBufferKeys::PixelFormatType),
            CFNumber::from(kCVPixelFormatType_32BGRA as i32).as_CFType(),
        ),
        (
            CFString::from(CVPixelBufferKeys::Width),
            CFNumber::from(output_format.width as i32).as_CFType(),
        ),
        (
            CFString::from(CVPixelBufferKeys::Height),
            CFNumber::from(output_format.height as i32).as_CFType(),
        ),
    ]);
    let session =
        VTDecompressionSession::new(video_description, None, Some(&attrs)).map_err(|status| {
            VideoDecodeError::BackendFailed {
                reason: format!("VTDecompressionSessionCreate returned {status}"),
            }
        })?;

    let timing_by_pts = input
        .packets
        .iter()
        .map(|packet| {
            (
                packet.pts.as_millis(),
                (packet.dts, packet.duration, packet.keyframe),
            )
        })
        .collect::<BTreeMap<_, _>>();
    let frames = Arc::new(Mutex::new(Vec::<DecodedVideoFrame>::new()));
    let callback_error = Arc::new(Mutex::new(None::<VideoDecodeError>));
    let callback_format = output_format;

    for packet in &input.packets {
        let block_buffer = block_buffer_from_packet(packet.bytes)?;
        let timing = [CMSampleTimingInfo {
            duration: cm_time_delta(packet.duration)?,
            presentationTimeStamp: cm_time_point(packet.pts)?,
            decodeTimeStamp: cm_time_point(packet.dts)?,
        }];
        let sizes = [packet.bytes.len()];
        let sample_buffer = CMSampleBuffer::new_ready(
            &block_buffer,
            Some(&format_description),
            1,
            Some(&timing),
            Some(&sizes),
        )
        .map_err(|status| VideoDecodeError::BackendFailed {
            reason: format!("CMSampleBufferCreateReady returned {status}"),
        })?;
        let out_frames = Arc::clone(&frames);
        let out_error = Arc::clone(&callback_error);
        let timing_by_pts = timing_by_pts.clone();
        let flags = VTDecodeFrameFlags::Frame_EnableAsynchronousDecompression
            | VTDecodeFrameFlags::Frame_EnableTemporalProcessing;

        session
            .decode_frame_with_closure(
                sample_buffer,
                flags,
                move |status, info, image, pts, duration| {
                    if status != 0 {
                        set_callback_error(
                            &out_error,
                            VideoDecodeError::BackendFailed {
                                reason: format!("VT decode callback returned {status} ({info:?})"),
                            },
                        );
                        return;
                    }
                    let Some(pixel_buffer) = pixel_buffer_from_image_buffer(&image) else {
                        set_callback_error(
                            &out_error,
                            VideoDecodeError::BackendFailed {
                                reason: "VideoToolbox callback did not return a pixel buffer"
                                    .to_string(),
                            },
                        );
                        return;
                    };
                    let pts_point =
                        time_point_from_cm_time(pts).unwrap_or_else(|_| TimePoint::millis(0));
                    let duration_delta =
                        time_delta_from_cm_time(duration).unwrap_or_else(|_| TimeDelta::millis(0));
                    let timing = timing_by_pts
                        .get(&pts_point.as_millis())
                        .copied()
                        .unwrap_or((pts_point, duration_delta, false));
                    match copy_bgra_pixel_buffer(&pixel_buffer, callback_format) {
                        Ok(pixels) => {
                            if let Ok(mut frames) = out_frames.lock() {
                                frames.push(DecodedVideoFrame {
                                    pts: pts_point,
                                    dts: timing.0,
                                    duration: if duration_delta.units == 0 {
                                        timing.1
                                    } else {
                                        duration_delta
                                    },
                                    format: callback_format,
                                    pixels,
                                    keyframe: timing.2,
                                });
                            }
                        }
                        Err(error) => set_callback_error(&out_error, error),
                    }
                },
            )
            .map_err(|status| VideoDecodeError::BackendFailed {
                reason: format!("VTDecompressionSessionDecodeFrame returned {status}"),
            })?;
        session.wait_for_asynchronous_frames().map_err(|status| {
            VideoDecodeError::BackendFailed {
                reason: format!(
                    "VTDecompressionSessionWaitForAsynchronousFrames returned {status}"
                ),
            }
        })?;
        if let Some(error) = callback_error
            .lock()
            .map_err(|_| VideoDecodeError::BackendFailed {
                reason: "VideoToolbox decode callback error lock was poisoned".to_string(),
            })?
            .take()
        {
            return Err(error);
        }
    }

    if input.end_of_stream {
        session
            .finish_delayed_frames()
            .map_err(|status| VideoDecodeError::BackendFailed {
                reason: format!("VTDecompressionSessionFinishDelayedFrames returned {status}"),
            })?;
        session.wait_for_asynchronous_frames().map_err(|status| {
            VideoDecodeError::BackendFailed {
                reason: format!(
                    "VTDecompressionSessionWaitForAsynchronousFrames returned {status}"
                ),
            }
        })?;
    }

    let mut frames = Arc::try_unwrap(frames)
        .map_err(|_| VideoDecodeError::BackendFailed {
            reason: "VideoToolbox decoded frame buffer still has callback owners".to_string(),
        })?
        .into_inner()
        .map_err(|_| VideoDecodeError::BackendFailed {
            reason: "VideoToolbox decoded frame buffer lock was poisoned".to_string(),
        })?;
    frames.sort_by_key(|frame| (frame.pts.units, frame.pts.scale.units_per_second));

    Ok(DecodedVideoOutput {
        stream: DecodedVideoStream {
            format: output_format,
            source_codec: input.codec,
            decoder: match input.codec {
                VideoCodec::H264 => "chroma-videotoolbox-h264-decoder".to_string(),
                VideoCodec::Hevc => "chroma-videotoolbox-hevc-decoder".to_string(),
            },
        },
        frames,
    })
}

#[cfg(target_os = "macos")]
fn set_callback_error(
    error_slot: &std::sync::Arc<std::sync::Mutex<Option<VideoDecodeError>>>,
    error: VideoDecodeError,
) {
    if let Ok(mut slot) = error_slot.lock()
        && slot.is_none()
    {
        *slot = Some(error);
    }
}

#[cfg(target_os = "macos")]
fn videotoolbox_h264_format_description(
    decoder_config: &[u8],
) -> Result<core_media::format_description::CMVideoFormatDescription, VideoDecodeError> {
    use core_media::format_description::CMVideoFormatDescription;

    let config = crate::codec::h264::parse_avc_decoder_config(decoder_config).map_err(|error| {
        VideoDecodeError::InvalidDecoderConfig {
            reason: error.to_string(),
        }
    })?;
    let parameter_sets = config
        .sps
        .iter()
        .chain(config.pps.iter())
        .map(Vec::as_slice)
        .collect::<Vec<_>>();
    if parameter_sets.is_empty() {
        return Err(VideoDecodeError::InvalidDecoderConfig {
            reason: "AVC decoder configuration does not contain SPS/PPS parameter sets".to_string(),
        });
    }

    CMVideoFormatDescription::from_h264_parameter_sets(
        &parameter_sets,
        config.nalu_length_size.into(),
    )
    .map_err(|status| VideoDecodeError::BackendFailed {
        reason: format!("CMVideoFormatDescriptionCreateFromH264ParameterSets returned {status}"),
    })
}

#[cfg(target_os = "macos")]
fn videotoolbox_hevc_format_description(
    decoder_config: &[u8],
) -> Result<core_media::format_description::CMVideoFormatDescription, VideoDecodeError> {
    use core_media::format_description::CMVideoFormatDescription;

    let config =
        crate::codec::hevc::parse_hevc_decoder_config(decoder_config).map_err(|error| {
            VideoDecodeError::InvalidDecoderConfig {
                reason: error.to_string(),
            }
        })?;
    let parameter_sets = config
        .arrays
        .iter()
        .filter(|array| matches!(array.nal_unit_type, 32..=34))
        .flat_map(|array| array.units.iter().map(Vec::as_slice))
        .collect::<Vec<_>>();
    if parameter_sets.is_empty() {
        return Err(VideoDecodeError::InvalidDecoderConfig {
            reason: "HEVC decoder configuration does not contain VPS/SPS/PPS parameter sets"
                .to_string(),
        });
    }

    CMVideoFormatDescription::from_hevc_parameter_sets(
        &parameter_sets,
        config.nalu_length_size.into(),
        None,
    )
    .map_err(|status| VideoDecodeError::BackendFailed {
        reason: format!("CMVideoFormatDescriptionCreateFromHEVCParameterSets returned {status}"),
    })
}

#[cfg(target_os = "macos")]
fn cm_format_description_from_video(
    description: &core_media::format_description::CMVideoFormatDescription,
) -> core_media::format_description::CMFormatDescription {
    use core_foundation::base::TCFType;
    use core_media::format_description::{CMFormatDescription, CMFormatDescriptionRef};

    #[allow(unsafe_code)]
    // SAFETY: CMVideoFormatDescriptionRef is a concrete CMFormatDescriptionRef subtype.
    // `wrap_under_get_rule` retains the object for the short-lived CMSampleBuffer build.
    unsafe {
        CMFormatDescription::wrap_under_get_rule(
            description.as_concrete_TypeRef() as CMFormatDescriptionRef
        )
    }
}

#[cfg(target_os = "macos")]
fn block_buffer_from_packet(
    bytes: &[u8],
) -> Result<core_media::block_buffer::CMBlockBuffer, VideoDecodeError> {
    use core_media::block_buffer::CMBlockBuffer;

    #[allow(unsafe_code)]
    // SAFETY: The block buffer references the immutable packet slice only while
    // the caller synchronously builds and submits the CMSampleBuffer; the input
    // packet bytes outlive the decode wait in `decode_videotoolbox_bgra_frames`.
    unsafe {
        CMBlockBuffer::new_with_memory_block_from_slice(bytes, 0, bytes.len(), 0).map_err(
            |status| VideoDecodeError::BackendFailed {
                reason: format!("CMBlockBufferCreateWithMemoryBlock returned {status}"),
            },
        )
    }
}

#[cfg(target_os = "macos")]
fn cm_time_point(point: TimePoint) -> Result<core_media::time::CMTime, VideoDecodeError> {
    cm_time(point.units, point.scale)
}

#[cfg(target_os = "macos")]
fn cm_time_delta(delta: TimeDelta) -> Result<core_media::time::CMTime, VideoDecodeError> {
    cm_time(delta.units, delta.scale)
}

#[cfg(target_os = "macos")]
fn cm_time(units: u64, scale: TimeScale) -> Result<core_media::time::CMTime, VideoDecodeError> {
    let value = i64::try_from(units).map_err(|_| VideoDecodeError::InvalidInput {
        reason: "decode timestamp does not fit CoreMedia CMTime value".to_string(),
    })?;
    let timescale =
        i32::try_from(scale.units_per_second).map_err(|_| VideoDecodeError::InvalidInput {
            reason: "decode timestamp scale does not fit CoreMedia CMTime scale".to_string(),
        })?;
    if timescale <= 0 {
        return Err(VideoDecodeError::InvalidInput {
            reason: "decode timestamp scale must be positive".to_string(),
        });
    }
    Ok(core_media::time::CMTime::make(value, timescale))
}

#[cfg(target_os = "macos")]
fn time_point_from_cm_time(time: core_media::time::CMTime) -> Result<TimePoint, VideoDecodeError> {
    if time.timescale <= 0 || time.value < 0 {
        return Err(VideoDecodeError::BackendFailed {
            reason: "VideoToolbox returned invalid presentation timestamp".to_string(),
        });
    }
    Ok(TimePoint {
        units: time.value as u64,
        scale: TimeScale {
            units_per_second: time.timescale as u32,
        },
    })
}

#[cfg(target_os = "macos")]
fn time_delta_from_cm_time(time: core_media::time::CMTime) -> Result<TimeDelta, VideoDecodeError> {
    if time.timescale <= 0 || time.value < 0 {
        return Err(VideoDecodeError::BackendFailed {
            reason: "VideoToolbox returned invalid frame duration".to_string(),
        });
    }
    Ok(TimeDelta {
        units: time.value as u64,
        scale: TimeScale {
            units_per_second: time.timescale as u32,
        },
    })
}

#[cfg(target_os = "macos")]
fn pixel_buffer_from_image_buffer(
    image_buffer: &core_video::image_buffer::CVImageBuffer,
) -> Option<core_video::pixel_buffer::CVPixelBuffer> {
    use core_foundation::base::TCFType;
    use core_video::pixel_buffer::{CVPixelBuffer, CVPixelBufferRef};

    #[allow(unsafe_code)]
    // SAFETY: VideoToolbox's decompression callback supplies a CVImageBuffer
    // that is a CVPixelBuffer for video decode output. The type check below
    // verifies the wrapped CF object before pixel-buffer access.
    unsafe {
        let pixel_buffer = CVPixelBuffer::wrap_under_get_rule(
            image_buffer.as_concrete_TypeRef() as CVPixelBufferRef
        );
        (pixel_buffer.get_width() > 0 && pixel_buffer.get_height() > 0).then_some(pixel_buffer)
    }
}

#[cfg(target_os = "macos")]
fn copy_bgra_pixel_buffer(
    pixel_buffer: &core_video::pixel_buffer::CVPixelBuffer,
    format: RawVideoFormat,
) -> Result<Vec<u8>, VideoDecodeError> {
    use core_video::{
        pixel_buffer::{kCVPixelBufferLock_ReadOnly, kCVPixelFormatType_32BGRA},
        r#return::kCVReturnSuccess,
    };

    if pixel_buffer.get_pixel_format() != kCVPixelFormatType_32BGRA {
        return Err(VideoDecodeError::BackendFailed {
            reason: format!(
                "VideoToolbox returned pixel format {}, expected BGRA",
                pixel_buffer.get_pixel_format()
            ),
        });
    }
    if pixel_buffer.get_width() != format.width as usize
        || pixel_buffer.get_height() != format.height as usize
    {
        return Err(VideoDecodeError::BackendFailed {
            reason: format!(
                "VideoToolbox returned {}x{}, expected {}x{}",
                pixel_buffer.get_width(),
                pixel_buffer.get_height(),
                format.width,
                format.height
            ),
        });
    }

    let status = pixel_buffer.lock_base_address(kCVPixelBufferLock_ReadOnly);
    if status != kCVReturnSuccess {
        return Err(VideoDecodeError::BackendFailed {
            reason: format!("CVPixelBufferLockBaseAddress returned {status}"),
        });
    }

    let row_bytes = format.width as usize * 4;
    let bytes_per_row = pixel_buffer.get_bytes_per_row();
    let mut pixels = vec![0_u8; row_bytes * format.height as usize];
    #[allow(unsafe_code)]
    // SAFETY: The pixel buffer is locked for read access, the base pointer is
    // valid for at least `bytes_per_row * height`, and the destination vector
    // is sized for tightly packed BGRA rows.
    unsafe {
        let base = pixel_buffer.get_base_address() as *const u8;
        if base.is_null() || bytes_per_row < row_bytes {
            let _ = pixel_buffer.unlock_base_address(kCVPixelBufferLock_ReadOnly);
            return Err(VideoDecodeError::BackendFailed {
                reason: "CVPixelBuffer returned invalid BGRA storage".to_string(),
            });
        }
        for row in 0..format.height as usize {
            let src = base.add(row * bytes_per_row);
            let dst = pixels.as_mut_ptr().add(row * row_bytes);
            std::ptr::copy_nonoverlapping(src, dst, row_bytes);
        }
    }

    let status = pixel_buffer.unlock_base_address(kCVPixelBufferLock_ReadOnly);
    if status != kCVReturnSuccess {
        return Err(VideoDecodeError::BackendFailed {
            reason: format!("CVPixelBufferUnlockBaseAddress returned {status}"),
        });
    }
    Ok(pixels)
}

#[cfg(not(target_os = "macos"))]
fn platform_probe_videotoolbox_h264_decoder_session(
    _format: RawVideoFormat,
    _decoder_config: &[u8],
) -> Result<VideoDecodeSessionInfo, VideoDecodeError> {
    Err(VideoDecodeError::BackendUnavailable {
        reason: "VideoToolbox H.264 decode is only available on macOS".to_string(),
    })
}

#[cfg(not(target_os = "macos"))]
fn platform_probe_videotoolbox_hevc_decoder_session(
    _format: RawVideoFormat,
    _decoder_config: &[u8],
) -> Result<VideoDecodeSessionInfo, VideoDecodeError> {
    Err(VideoDecodeError::BackendUnavailable {
        reason: "VideoToolbox HEVC decode is only available on macOS".to_string(),
    })
}

#[cfg(not(target_os = "macos"))]
fn platform_decode_videotoolbox_bgra_frames(
    _input: &VideoDecodeInput<'_>,
    _output_format: RawVideoFormat,
) -> Result<DecodedVideoOutput, VideoDecodeError> {
    Err(VideoDecodeError::BackendUnavailable {
        reason: "VideoToolbox BGRA decode is only available on macOS".to_string(),
    })
}

#[cfg(test)]
mod tests {
    use crate::packet::{ChunkSample, TimeDelta, TimePoint};

    use super::*;

    #[test]
    fn build_decode_input_borrows_packet_payloads() {
        let samples = vec![sample(7, 0, 3, 1_000, true), sample(8, 3, 2, 1_033, false)];
        let payload = [0xaa, 0xbb, 0xcc, 0xdd, 0xee];

        let input = build_video_decode_input(
            VideoCodec::Hevc,
            TimeScale {
                units_per_second: 90_000,
            },
            Some(&[1, 2, 3]),
            &samples,
            &payload,
            false,
        )
        .unwrap();

        assert_eq!(input.packets.len(), 2);
        assert_eq!(input.packets[0].bytes, &[0xaa, 0xbb, 0xcc]);
        assert_eq!(input.packets[1].bytes, &[0xdd, 0xee]);
        assert_eq!(input.decoder_config, Some([1, 2, 3].as_slice()));
    }

    #[test]
    fn rejects_packet_payload_out_of_bounds() {
        let samples = vec![sample(0, 2, 8, 0, true)];
        let err = build_video_decode_input(
            VideoCodec::H264,
            TimeScale {
                units_per_second: 90_000,
            },
            None,
            &samples,
            &[0, 1, 2],
            false,
        )
        .unwrap_err();

        assert_eq!(err, VideoDecodeError::PacketOutOfBounds);
    }

    #[test]
    fn decoder_actions_receive_after_each_send_and_drain_once() {
        let samples = vec![sample(0, 0, 1, 0, true), sample(1, 1, 1, 33, false)];
        let payload = [0xaa, 0xbb];
        let input = build_video_decode_input(
            VideoCodec::H264,
            TimeScale {
                units_per_second: 90_000,
            },
            None,
            &samples,
            &payload,
            true,
        )
        .unwrap();

        let actions = decoder_actions_for_input(&input);

        assert_eq!(
            actions,
            vec![
                VideoDecoderAction::SendPacket { packet_index: 0 },
                VideoDecoderAction::ReceiveAvailable,
                VideoDecoderAction::SendPacket { packet_index: 1 },
                VideoDecoderAction::ReceiveAvailable,
                VideoDecoderAction::SendDrain,
                VideoDecoderAction::ReceiveDrain,
            ]
        );
    }

    #[test]
    fn validates_decoder_output_format() {
        let format = RawVideoFormat {
            width: 1_920,
            height: 1_080,
            frame_rate_num: 24_000,
            frame_rate_den: 1_001,
            pixel_format: RawVideoPixelFormat::Bgra,
        };

        assert!(validate_decoded_video_format(format).is_ok());
    }

    #[test]
    fn decoder_session_probe_rejects_missing_config_before_backend() {
        let err = probe_videotoolbox_h264_decoder_session(valid_format(), &[]).unwrap_err();

        assert_eq!(err, VideoDecodeError::MissingDecoderConfig);
    }

    #[test]
    fn bgra_decode_rejects_missing_config_before_backend() {
        let input = VideoDecodeInput {
            codec: VideoCodec::H264,
            time_scale: TimeScale {
                units_per_second: 90_000,
            },
            decoder_config: None,
            packets: Vec::new(),
            end_of_stream: true,
        };

        let err = decode_videotoolbox_bgra_frames(&input, valid_format()).unwrap_err();

        assert_eq!(err, VideoDecodeError::MissingDecoderConfig);
    }

    #[cfg(target_os = "macos")]
    #[test]
    fn decoder_session_probe_rejects_truncated_config() {
        let err = probe_videotoolbox_hevc_decoder_session(valid_format(), &[1, 2, 3])
            .expect_err("truncated hvcC must not reach VideoToolbox");

        assert!(matches!(err, VideoDecodeError::InvalidDecoderConfig { .. }));
    }

    fn valid_format() -> RawVideoFormat {
        RawVideoFormat {
            width: 128,
            height: 72,
            frame_rate_num: 24,
            frame_rate_den: 1,
            pixel_format: RawVideoPixelFormat::Bgra,
        }
    }

    fn sample(index: u32, offset: u64, size: u32, pts_ms: u64, keyframe: bool) -> ChunkSample {
        ChunkSample {
            index,
            payload_offset: offset,
            byte_count: size,
            pts: TimePoint::millis(pts_ms),
            dts: TimePoint::millis(pts_ms),
            duration: TimeDelta::millis(33),
            keyframe,
        }
    }
}
