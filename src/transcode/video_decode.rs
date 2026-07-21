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
    use core_media::format_description::{CMVideoFormatDescription, kCMVideoCodecType_H264};
    use video_toolbox::decompression_session::VTDecompressionSession;

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

    let description = CMVideoFormatDescription::from_h264_parameter_sets(
        &parameter_sets,
        config.nalu_length_size.into(),
    )
    .map_err(|status| VideoDecodeError::BackendFailed {
        reason: format!("CMVideoFormatDescriptionCreateFromH264ParameterSets returned {status}"),
    })?;
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
    use core_media::format_description::{CMVideoFormatDescription, kCMVideoCodecType_HEVC};
    use video_toolbox::decompression_session::VTDecompressionSession;

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

    let description = CMVideoFormatDescription::from_hevc_parameter_sets(
        &parameter_sets,
        config.nalu_length_size.into(),
        None,
    )
    .map_err(|status| VideoDecodeError::BackendFailed {
        reason: format!("CMVideoFormatDescriptionCreateFromHEVCParameterSets returned {status}"),
    })?;
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
