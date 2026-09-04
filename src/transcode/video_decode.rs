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

/// Retained VideoToolbox decoder for ordered H.264 or HEVC packet batches.
pub struct VideoToolboxBgraDecoderSession {
    codec: VideoCodec,
    output_format: RawVideoFormat,
    decoded_batches: u64,
    #[cfg(target_os = "macos")]
    video_description:
        objc2_core_foundation::CFRetained<objc2_core_media::CMVideoFormatDescription>,
    #[cfg(target_os = "macos")]
    session: objc2_core_foundation::CFRetained<objc2_video_toolbox::VTDecompressionSession>,
}

#[cfg(target_os = "macos")]
impl Drop for VideoToolboxBgraDecoderSession {
    fn drop(&mut self) {
        #[allow(unsafe_code)]
        // SAFETY: The retained session is valid until this owner is dropped.
        unsafe {
            self.session.invalidate();
        }
    }
}

impl std::fmt::Debug for VideoToolboxBgraDecoderSession {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("VideoToolboxBgraDecoderSession")
            .field("codec", &self.codec)
            .field("output_format", &self.output_format)
            .field("decoded_batches", &self.decoded_batches)
            .finish_non_exhaustive()
    }
}

impl VideoToolboxBgraDecoderSession {
    /// Creates one decoder that can serve multiple ordered packet batches.
    pub fn new(
        codec: VideoCodec,
        output_format: RawVideoFormat,
        decoder_config: &[u8],
    ) -> Result<Self, VideoDecodeError> {
        validate_decoded_video_format(output_format)?;
        if decoder_config.is_empty() {
            return Err(VideoDecodeError::MissingDecoderConfig);
        }
        platform_new_videotoolbox_bgra_decoder(codec, output_format, decoder_config)
    }

    /// Decodes one packet batch without recreating the native session.
    pub fn decode(
        &mut self,
        input: &VideoDecodeInput<'_>,
    ) -> Result<DecodedVideoOutput, VideoDecodeError> {
        if input.codec != self.codec {
            return Err(VideoDecodeError::InvalidInput {
                reason: "packet codec does not match retained decoder session".to_string(),
            });
        }
        let output = platform_decode_with_retained_session(self, input)?;
        self.decoded_batches = self.decoded_batches.saturating_add(1);
        Ok(output)
    }

    /// Returns the number of batches decoded by this native session.
    pub fn decoded_batches(&self) -> u64 {
        self.decoded_batches
    }
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
    VideoToolboxBgraDecoderSession::new(input.codec, output_format, decoder_config)?.decode(input)
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
    let description = videotoolbox_h264_format_description(decoder_config)?;
    new_decompression_session(&description, None, "H.264")?;

    Ok(VideoDecodeSessionInfo {
        codec: VideoCodec::H264,
        decoder: "chroma-videotoolbox-h264-decoder".to_string(),
        width: format.width,
        height: format.height,
        output_format: format,
        hardware_supported: hardware_decode_supported(objc2_core_media::kCMVideoCodecType_H264),
        session_created: true,
    })
}

#[cfg(target_os = "macos")]
fn platform_probe_videotoolbox_hevc_decoder_session(
    format: RawVideoFormat,
    decoder_config: &[u8],
) -> Result<VideoDecodeSessionInfo, VideoDecodeError> {
    let description = videotoolbox_hevc_format_description(decoder_config)?;
    new_decompression_session(&description, None, "HEVC")?;

    Ok(VideoDecodeSessionInfo {
        codec: VideoCodec::Hevc,
        decoder: "chroma-videotoolbox-hevc-decoder".to_string(),
        width: format.width,
        height: format.height,
        output_format: format,
        hardware_supported: hardware_decode_supported(objc2_core_media::kCMVideoCodecType_HEVC),
        session_created: true,
    })
}

#[cfg(target_os = "macos")]
fn hardware_decode_supported(codec: objc2_core_media::CMVideoCodecType) -> bool {
    #[allow(unsafe_code)]
    // SAFETY: The codec is one of CoreMedia's declared video codec constants.
    unsafe {
        objc2_video_toolbox::VTIsHardwareDecodeSupported(codec)
    }
}

#[cfg(target_os = "macos")]
fn new_decompression_session(
    description: &objc2_core_media::CMVideoFormatDescription,
    attributes: Option<&objc2_core_foundation::CFDictionary>,
    label: &str,
) -> Result<
    objc2_core_foundation::CFRetained<objc2_video_toolbox::VTDecompressionSession>,
    VideoDecodeError,
> {
    use std::ptr::{NonNull, null, null_mut};

    let mut raw = null_mut();
    #[allow(unsafe_code)]
    // SAFETY: `raw` is a valid out-parameter, the format and attributes stay
    // alive for the call, and a null callback selects per-frame handlers.
    let status = unsafe {
        objc2_video_toolbox::VTDecompressionSession::create(
            None,
            description,
            None,
            attributes,
            null(),
            NonNull::from(&mut raw),
        )
    };
    if status != 0 {
        return Err(VideoDecodeError::BackendFailed {
            reason: format!("VTDecompressionSessionCreate({label}) returned {status}"),
        });
    }
    let raw = NonNull::new(raw).ok_or_else(|| VideoDecodeError::BackendFailed {
        reason: format!("VTDecompressionSessionCreate({label}) returned no session"),
    })?;
    #[allow(unsafe_code)]
    // SAFETY: VideoToolbox returned this pointer at +1 under the Create rule.
    Ok(unsafe { objc2_core_foundation::CFRetained::from_raw(raw) })
}

#[cfg(target_os = "macos")]
fn platform_new_videotoolbox_bgra_decoder(
    codec: VideoCodec,
    output_format: RawVideoFormat,
    decoder_config: &[u8],
) -> Result<VideoToolboxBgraDecoderSession, VideoDecodeError> {
    let video_description = match codec {
        VideoCodec::H264 => videotoolbox_h264_format_description(decoder_config)?,
        VideoCodec::Hevc => videotoolbox_hevc_format_description(decoder_config)?,
    };
    let pixel_format = objc2_core_foundation::CFNumber::new_i32(
        objc2_core_video::kCVPixelFormatType_32BGRA as i32,
    );
    let width = objc2_core_foundation::CFNumber::new_i32(output_format.width as i32);
    let height = objc2_core_foundation::CFNumber::new_i32(output_format.height as i32);
    #[allow(unsafe_code)]
    // SAFETY: These framework keys are immutable process-lifetime CFStrings.
    let attrs = unsafe {
        objc2_core_foundation::CFDictionary::<
            objc2_core_foundation::CFString,
            objc2_core_foundation::CFType,
        >::from_slices(
            &[
                objc2_core_video::kCVPixelBufferPixelFormatTypeKey,
                objc2_core_video::kCVPixelBufferWidthKey,
                objc2_core_video::kCVPixelBufferHeightKey,
            ],
            &[pixel_format.as_ref(), width.as_ref(), height.as_ref()],
        )
    };
    #[allow(unsafe_code)]
    // SAFETY: Erasing generic markers preserves the dictionary representation.
    let untyped_attrs = unsafe { attrs.cast_unchecked() };
    let session = new_decompression_session(&video_description, Some(untyped_attrs), "BGRA")?;
    Ok(VideoToolboxBgraDecoderSession {
        codec,
        output_format,
        decoded_batches: 0,
        video_description,
        session,
    })
}

#[cfg(target_os = "macos")]
fn platform_decode_with_retained_session(
    retained: &VideoToolboxBgraDecoderSession,
    input: &VideoDecodeInput<'_>,
) -> Result<DecodedVideoOutput, VideoDecodeError> {
    use std::{
        collections::BTreeMap,
        sync::{Arc, Mutex},
    };

    use block2::RcBlock;
    use objc2_core_media::{CMSampleTimingInfo, CMTime};
    use objc2_core_video::CVImageBuffer;
    use objc2_video_toolbox::VTDecodeFrameFlags;

    let output_format = retained.output_format;
    #[allow(unsafe_code)]
    // SAFETY: CMVideoFormatDescription is the concrete video subtype of
    // CMFormatDescription and this borrow is bounded by the retained owner.
    let format_description = unsafe {
        &*(std::ptr::from_ref(&*retained.video_description)
            .cast::<objc2_core_media::CMFormatDescription>())
    };
    let session = &retained.session;

    let timing_by_pts = Arc::new(
        input
            .packets
            .iter()
            .map(|packet| {
                (
                    DecodeTimestampKey::from(packet.pts),
                    (packet.dts, packet.duration, packet.keyframe),
                )
            })
            .collect::<BTreeMap<_, _>>(),
    );
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
        let sample_buffer =
            sample_buffer_from_packet(&block_buffer, format_description, &timing, &sizes)?;
        let out_frames = Arc::clone(&frames);
        let out_error = Arc::clone(&callback_error);
        let timing_by_pts = Arc::clone(&timing_by_pts);
        let flags = VTDecodeFrameFlags::Frame_EnableAsynchronousDecompression
            | VTDecodeFrameFlags::Frame_EnableTemporalProcessing;

        let handler = RcBlock::new(
            move |status: i32, info, image: *mut CVImageBuffer, pts: CMTime, duration: CMTime| {
                if status != 0 {
                    set_callback_error(
                        &out_error,
                        VideoDecodeError::BackendFailed {
                            reason: format!("VT decode callback returned {status} ({info:?})"),
                        },
                    );
                    return;
                }
                let Some(image) = std::ptr::NonNull::new(image) else {
                    set_callback_error(
                        &out_error,
                        VideoDecodeError::BackendFailed {
                            reason: "VideoToolbox callback did not return a pixel buffer"
                                .to_string(),
                        },
                    );
                    return;
                };
                #[allow(unsafe_code)]
                // SAFETY: VideoToolbox guarantees the callback image remains
                // valid for the duration of this handler invocation.
                let Some(pixel_buffer) =
                    (unsafe { pixel_buffer_from_image_buffer(image.as_ref()) })
                else {
                    set_callback_error(
                        &out_error,
                        VideoDecodeError::BackendFailed {
                            reason: "VideoToolbox callback did not return a pixel buffer"
                                .to_string(),
                        },
                    );
                    return;
                };
                let pts_point = match time_point_from_cm_time(pts) {
                    Ok(pts) => pts,
                    Err(error) => {
                        set_callback_error(&out_error, error);
                        return;
                    }
                };
                let duration_delta = match time_delta_from_cm_time(duration) {
                    Ok(duration) => duration,
                    Err(error) => {
                        set_callback_error(&out_error, error);
                        return;
                    }
                };
                let timing = timing_by_pts
                    .get(&DecodeTimestampKey::from(pts_point))
                    .copied()
                    .unwrap_or((pts_point, duration_delta, false));
                match copy_bgra_pixel_buffer(pixel_buffer, callback_format) {
                    Ok(pixels) => match out_frames.lock() {
                        Ok(mut frames) => frames.push(DecodedVideoFrame {
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
                        }),
                        Err(_) => set_callback_error(
                            &out_error,
                            VideoDecodeError::BackendFailed {
                                reason: "VideoToolbox decoded frame output lock was poisoned"
                                    .to_string(),
                            },
                        ),
                    },
                    Err(error) => set_callback_error(&out_error, error),
                }
            },
        );
        #[allow(unsafe_code)]
        // SAFETY: The sample and handler remain live for submission, and
        // VideoToolbox copies the escaping block for asynchronous delivery.
        let status = unsafe {
            session.decode_frame_with_output_handler(
                &sample_buffer,
                flags,
                std::ptr::null_mut(),
                std::ptr::from_ref(&*handler).cast_mut(),
            )
        };
        if status != 0 {
            return Err(VideoDecodeError::BackendFailed {
                reason: format!("VTDecompressionSessionDecodeFrame returned {status}"),
            });
        }
    }

    if input.end_of_stream {
        #[allow(unsafe_code)]
        // SAFETY: The retained decoder session is live.
        let status = unsafe { session.finish_delayed_frames() };
        if status != 0 {
            return Err(VideoDecodeError::BackendFailed {
                reason: format!("VTDecompressionSessionFinishDelayedFrames returned {status}"),
            });
        }
    }
    #[allow(unsafe_code)]
    // SAFETY: The retained decoder session is live; this joins all callbacks.
    let status = unsafe { session.wait_for_asynchronous_frames() };
    if status != 0 {
        return Err(VideoDecodeError::BackendFailed {
            reason: format!("VTDecompressionSessionWaitForAsynchronousFrames returned {status}"),
        });
    }
    if let Some(error) = take_decode_callback_error(&callback_error)? {
        return Err(error);
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
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
struct DecodeTimestampKey {
    units: u64,
    units_per_second: u32,
}

#[cfg(target_os = "macos")]
impl From<TimePoint> for DecodeTimestampKey {
    fn from(point: TimePoint) -> Self {
        Self {
            units: point.units,
            units_per_second: point.scale.units_per_second,
        }
    }
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
fn take_decode_callback_error(
    error_slot: &std::sync::Arc<std::sync::Mutex<Option<VideoDecodeError>>>,
) -> Result<Option<VideoDecodeError>, VideoDecodeError> {
    error_slot
        .lock()
        .map_err(|_| VideoDecodeError::BackendFailed {
            reason: "VideoToolbox decode callback error lock was poisoned".to_string(),
        })
        .map(|mut slot| slot.take())
}

#[cfg(target_os = "macos")]
fn videotoolbox_h264_format_description(
    decoder_config: &[u8],
) -> Result<
    objc2_core_foundation::CFRetained<objc2_core_media::CMVideoFormatDescription>,
    VideoDecodeError,
> {
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

    create_video_format_description(&parameter_sets, config.nalu_length_size.into(), false)
}

#[cfg(target_os = "macos")]
fn videotoolbox_hevc_format_description(
    decoder_config: &[u8],
) -> Result<
    objc2_core_foundation::CFRetained<objc2_core_media::CMVideoFormatDescription>,
    VideoDecodeError,
> {
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

    create_video_format_description(&parameter_sets, config.nalu_length_size.into(), true)
}

#[cfg(target_os = "macos")]
fn create_video_format_description(
    parameter_sets: &[&[u8]],
    nal_length_size: i32,
    hevc: bool,
) -> Result<
    objc2_core_foundation::CFRetained<objc2_core_media::CMVideoFormatDescription>,
    VideoDecodeError,
> {
    use std::ptr::{NonNull, null};

    if parameter_sets.is_empty() || parameter_sets.iter().any(|set| set.is_empty()) {
        return Err(VideoDecodeError::InvalidDecoderConfig {
            reason: "video decoder parameter sets must not be empty".to_string(),
        });
    }
    let mut pointers = parameter_sets
        .iter()
        .filter_map(|set| NonNull::new(set.as_ptr().cast_mut()))
        .collect::<Vec<_>>();
    let mut sizes = parameter_sets
        .iter()
        .map(|set| set.len())
        .collect::<Vec<_>>();
    let mut raw: *const objc2_core_media::CMFormatDescription = null();
    #[allow(unsafe_code)]
    // SAFETY: The parameter-set pointers and sizes describe live slices for the
    // duration of the Create call, and `raw` is a valid out-parameter.
    let status = unsafe {
        if hevc {
            objc2_core_media::CMVideoFormatDescriptionCreateFromHEVCParameterSets(
                None,
                pointers.len(),
                NonNull::from(&mut pointers[0]),
                NonNull::from(&mut sizes[0]),
                nal_length_size,
                None,
                NonNull::from(&mut raw),
            )
        } else {
            objc2_core_media::CMVideoFormatDescriptionCreateFromH264ParameterSets(
                None,
                pointers.len(),
                NonNull::from(&mut pointers[0]),
                NonNull::from(&mut sizes[0]),
                nal_length_size,
                NonNull::from(&mut raw),
            )
        }
    };
    if status != 0 {
        return Err(VideoDecodeError::BackendFailed {
            reason: format!("CMVideoFormatDescriptionCreate returned {status}"),
        });
    }
    let raw = NonNull::new(raw.cast_mut()).ok_or_else(|| VideoDecodeError::BackendFailed {
        reason: "CMVideoFormatDescriptionCreate returned no description".to_string(),
    })?;
    #[allow(unsafe_code)]
    // SAFETY: Both Create functions return a video-format-description object
    // at +1, represented by the CMFormatDescription supertype in their ABI.
    Ok(unsafe {
        objc2_core_foundation::CFRetained::from_raw(
            raw.cast::<objc2_core_media::CMVideoFormatDescription>(),
        )
    })
}

#[cfg(target_os = "macos")]
fn block_buffer_from_packet(
    bytes: &[u8],
) -> Result<objc2_core_foundation::CFRetained<objc2_core_media::CMBlockBuffer>, VideoDecodeError> {
    use std::ptr::{NonNull, null, null_mut};

    let mut raw = null_mut();
    #[allow(unsafe_code)]
    // SAFETY: A null memory block asks CoreMedia to allocate `bytes.len()` bytes;
    // `raw` is a valid out-parameter and the custom source is absent.
    let status = unsafe {
        objc2_core_media::CMBlockBuffer::create_with_memory_block(
            None,
            null_mut(),
            bytes.len(),
            None,
            null(),
            0,
            bytes.len(),
            0,
            NonNull::from(&mut raw),
        )
    };
    if status != 0 {
        return Err(VideoDecodeError::BackendFailed {
            reason: format!("CMBlockBufferCreateWithMemoryBlock returned {status}"),
        });
    }
    let raw = NonNull::new(raw).ok_or_else(|| VideoDecodeError::BackendFailed {
        reason: "CMBlockBufferCreateWithMemoryBlock returned no buffer".to_string(),
    })?;
    #[allow(unsafe_code)]
    // SAFETY: CoreMedia returned this block buffer at +1 under the Create rule.
    let buffer = unsafe { objc2_core_foundation::CFRetained::from_raw(raw) };
    let source = NonNull::new(bytes.as_ptr().cast_mut().cast()).ok_or_else(|| {
        VideoDecodeError::InvalidInput {
            reason: "compressed packet is empty".to_string(),
        }
    })?;
    #[allow(unsafe_code)]
    // SAFETY: `source` spans `bytes.len()` readable bytes and the destination
    // buffer was allocated with exactly that writable capacity.
    let status = unsafe {
        objc2_core_media::CMBlockBuffer::replace_data_bytes(source, &buffer, 0, bytes.len())
    };
    if status != 0 {
        return Err(VideoDecodeError::BackendFailed {
            reason: format!("CMBlockBufferReplaceDataBytes returned {status}"),
        });
    }
    Ok(buffer)
}

#[cfg(target_os = "macos")]
fn sample_buffer_from_packet(
    block_buffer: &objc2_core_media::CMBlockBuffer,
    format_description: &objc2_core_media::CMFormatDescription,
    timing: &[objc2_core_media::CMSampleTimingInfo; 1],
    sizes: &[usize; 1],
) -> Result<objc2_core_foundation::CFRetained<objc2_core_media::CMSampleBuffer>, VideoDecodeError> {
    use std::ptr::{NonNull, null_mut};

    let mut raw = null_mut();
    #[allow(unsafe_code)]
    // SAFETY: The block buffer, format description, timing entry, and size entry
    // are valid for the call and `raw` is a valid out-parameter.
    let status = unsafe {
        objc2_core_media::CMSampleBuffer::create_ready(
            None,
            Some(block_buffer),
            Some(format_description),
            1,
            1,
            timing.as_ptr(),
            1,
            sizes.as_ptr(),
            NonNull::from(&mut raw),
        )
    };
    if status != 0 {
        return Err(VideoDecodeError::BackendFailed {
            reason: format!("CMSampleBufferCreateReady returned {status}"),
        });
    }
    let raw = NonNull::new(raw).ok_or_else(|| VideoDecodeError::BackendFailed {
        reason: "CMSampleBufferCreateReady returned no sample buffer".to_string(),
    })?;
    #[allow(unsafe_code)]
    // SAFETY: CoreMedia returned this sample buffer at +1 under the Create rule.
    Ok(unsafe { objc2_core_foundation::CFRetained::from_raw(raw) })
}

#[cfg(target_os = "macos")]
fn cm_time_point(point: TimePoint) -> Result<objc2_core_media::CMTime, VideoDecodeError> {
    cm_time(point.units, point.scale)
}

#[cfg(target_os = "macos")]
fn cm_time_delta(delta: TimeDelta) -> Result<objc2_core_media::CMTime, VideoDecodeError> {
    cm_time(delta.units, delta.scale)
}

#[cfg(target_os = "macos")]
fn cm_time(units: u64, scale: TimeScale) -> Result<objc2_core_media::CMTime, VideoDecodeError> {
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
    #[allow(unsafe_code)]
    // SAFETY: `timescale` was validated as positive.
    Ok(unsafe { objc2_core_media::CMTime::new(value, timescale) })
}

#[cfg(target_os = "macos")]
fn time_point_from_cm_time(time: objc2_core_media::CMTime) -> Result<TimePoint, VideoDecodeError> {
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
fn time_delta_from_cm_time(time: objc2_core_media::CMTime) -> Result<TimeDelta, VideoDecodeError> {
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
    image_buffer: &objc2_core_video::CVImageBuffer,
) -> Option<&objc2_core_video::CVPixelBuffer> {
    #[allow(unsafe_code)]
    // SAFETY: VideoToolbox decode output image buffers are CVPixelBuffers. The
    // dimension checks reject an invalid or incompatible callback object.
    let pixel_buffer =
        unsafe { &*(std::ptr::from_ref(image_buffer).cast::<objc2_core_video::CVPixelBuffer>()) };
    (objc2_core_video::CVPixelBufferGetWidth(pixel_buffer) > 0
        && objc2_core_video::CVPixelBufferGetHeight(pixel_buffer) > 0)
        .then_some(pixel_buffer)
}

#[cfg(target_os = "macos")]
fn copy_bgra_pixel_buffer(
    pixel_buffer: &objc2_core_video::CVPixelBuffer,
    format: RawVideoFormat,
) -> Result<Vec<u8>, VideoDecodeError> {
    let pixel_format = objc2_core_video::CVPixelBufferGetPixelFormatType(pixel_buffer);
    if pixel_format != objc2_core_video::kCVPixelFormatType_32BGRA {
        return Err(VideoDecodeError::BackendFailed {
            reason: format!(
                "VideoToolbox returned pixel format {}, expected BGRA",
                pixel_format
            ),
        });
    }
    let width = objc2_core_video::CVPixelBufferGetWidth(pixel_buffer);
    let height = objc2_core_video::CVPixelBufferGetHeight(pixel_buffer);
    if width != format.width as usize || height != format.height as usize {
        return Err(VideoDecodeError::BackendFailed {
            reason: format!(
                "VideoToolbox returned {}x{}, expected {}x{}",
                width, height, format.width, format.height
            ),
        });
    }

    let flags = objc2_core_video::CVPixelBufferLockFlags::ReadOnly;
    #[allow(unsafe_code)]
    // SAFETY: The callback pixel buffer is live and is locked only for reading.
    let status = unsafe { objc2_core_video::CVPixelBufferLockBaseAddress(pixel_buffer, flags) };
    if status != objc2_core_video::kCVReturnSuccess {
        return Err(VideoDecodeError::BackendFailed {
            reason: format!("CVPixelBufferLockBaseAddress returned {status}"),
        });
    }

    let row_bytes = format.width as usize * 4;
    let bytes_per_row = objc2_core_video::CVPixelBufferGetBytesPerRow(pixel_buffer);
    let mut pixels = vec![0_u8; row_bytes * format.height as usize];
    #[allow(unsafe_code)]
    // SAFETY: The pixel buffer is locked for read access, the base pointer is
    // valid for at least `bytes_per_row * height`, and the destination vector
    // is sized for tightly packed BGRA rows.
    unsafe {
        let base = objc2_core_video::CVPixelBufferGetBaseAddress(pixel_buffer).cast::<u8>();
        if base.is_null() || bytes_per_row < row_bytes {
            let _ = objc2_core_video::CVPixelBufferUnlockBaseAddress(pixel_buffer, flags);
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

    #[allow(unsafe_code)]
    // SAFETY: This balances the successful read lock above.
    let status = unsafe { objc2_core_video::CVPixelBufferUnlockBaseAddress(pixel_buffer, flags) };
    if status != objc2_core_video::kCVReturnSuccess {
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
fn platform_new_videotoolbox_bgra_decoder(
    _codec: VideoCodec,
    _output_format: RawVideoFormat,
    _decoder_config: &[u8],
) -> Result<VideoToolboxBgraDecoderSession, VideoDecodeError> {
    Err(VideoDecodeError::BackendUnavailable {
        reason: "VideoToolbox BGRA decode is only available on macOS".to_string(),
    })
}

#[cfg(not(target_os = "macos"))]
fn platform_decode_with_retained_session(
    _retained: &VideoToolboxBgraDecoderSession,
    _input: &VideoDecodeInput<'_>,
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
