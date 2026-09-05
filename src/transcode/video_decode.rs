use serde::{Deserialize, Serialize};
use thiserror::Error;

use crate::{
    packet::{ChunkSample, TimeDelta, TimePoint, TimeScale},
    transcode::{RawVideoFormat, RawVideoPixelFormat, VideoCodec},
};

use super::yuv::limited_yuv_to_bgra;

#[cfg(all(target_os = "linux", feature = "linux-vaapi"))]
mod vaapi_decode;

#[cfg(all(target_os = "linux", feature = "linux-vaapi"))]
pub use vaapi_decode::{VaapiH264BgraDecoderSession, VaapiHevcBgraDecoderSession};

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

/// Retained cross-platform OpenH264 decoder that emits tightly packed BGRA frames.
pub struct CpuH264BgraDecoderSession {
    output_format: RawVideoFormat,
    decoded_batches: u64,
    nalu_length_size: u8,
    parameter_sets: Option<Vec<u8>>,
    pending_timing: Vec<CpuDecodeTiming>,
    decoder: openh264::decoder::Decoder,
}

/// Retained, portable HEVC Main/Main10 decoder that emits tightly packed BGRA frames.
pub struct CpuHevcBgraDecoderSession {
    output_format: RawVideoFormat,
    decoded_batches: u64,
    nalu_length_size: u8,
    pending_timing: Vec<CpuDecodeTiming>,
    decoder: rust_h265::Decoder,
}

/// Retained cross-platform dav1d AV1 decoder that emits tightly packed BGRA frames.
pub struct CpuAv1BgraDecoderSession {
    output_format: RawVideoFormat,
    decoded_batches: u64,
    pending_timing: Vec<CpuDecodeTiming>,
    decoder: dav1d::Decoder,
}

impl std::fmt::Debug for CpuAv1BgraDecoderSession {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("CpuAv1BgraDecoderSession")
            .field("output_format", &self.output_format)
            .field("decoded_batches", &self.decoded_batches)
            .field("pending_frames", &self.pending_timing.len())
            .finish_non_exhaustive()
    }
}

impl CpuAv1BgraDecoderSession {
    /// Creates a portable AV1 decoder. AV1 packets carry their sequence headers in-band.
    pub fn new(output_format: RawVideoFormat) -> Result<Self, VideoDecodeError> {
        validate_decoded_video_format(output_format)?;
        let mut settings = dav1d::Settings::default();
        // Keep streaming latency and retained-session memory bounded. Throughput still
        // benefits from dav1d's internal threading, but pictures cannot accumulate
        // behind an unbounded frame-delay window between HLS packet batches.
        settings.set_max_frame_delay(1);
        let decoder = dav1d::Decoder::with_settings(&settings).map_err(|error| {
            VideoDecodeError::BackendFailed {
                reason: format!("dav1d decoder initialization failed: {error}"),
            }
        })?;
        Ok(Self {
            output_format,
            decoded_batches: 0,
            pending_timing: Vec::new(),
            decoder,
        })
    }

    /// Decodes one ordered AV1 packet batch without recreating the software decoder.
    pub fn decode(
        &mut self,
        input: &VideoDecodeInput<'_>,
    ) -> Result<DecodedVideoOutput, VideoDecodeError> {
        if input.codec != VideoCodec::Av1 {
            return Err(VideoDecodeError::UnsupportedCodec);
        }
        if input.time_scale.units_per_second == 0 {
            return Err(VideoDecodeError::InvalidInput {
                reason: "packet time scale must be greater than zero".to_string(),
            });
        }
        let mut frames = Vec::new();
        for packet in &input.packets {
            validate_packet_time_scale(packet, input.time_scale)?;
            insert_cpu_decode_timing(
                &mut self.pending_timing,
                CpuDecodeTiming {
                    pts: packet.pts,
                    dts: packet.dts,
                    duration: packet.duration,
                    keyframe: packet.keyframe,
                },
            );
            let timestamp =
                i64::try_from(packet.pts.units).map_err(|_| VideoDecodeError::InvalidInput {
                    reason: "AV1 packet timestamp exceeds dav1d's signed timestamp range"
                        .to_string(),
                })?;
            let duration = i64::try_from(packet.duration.units).map_err(|_| {
                VideoDecodeError::InvalidInput {
                    reason: "AV1 packet duration exceeds dav1d's signed duration range".to_string(),
                }
            })?;
            let offset = i64::from(packet.index);
            let mut sent = self.decoder.send_data(
                packet.bytes.to_vec(),
                Some(offset),
                Some(timestamp),
                Some(duration),
            );
            loop {
                match sent {
                    Ok(()) => break,
                    Err(dav1d::Error::Again) => {
                        self.receive_available(&mut frames)?;
                        sent = self.decoder.send_pending_data();
                    }
                    Err(error) => return Err(dav1d_decode_error("packet submission", error)),
                }
            }
            self.receive_one(&mut frames)?;
        }
        if input.end_of_stream {
            self.receive_available(&mut frames)?;
            if !self.pending_timing.is_empty() {
                return Err(VideoDecodeError::BackendFailed {
                    reason: format!(
                        "dav1d drained with {} packet timing record(s) unmatched",
                        self.pending_timing.len()
                    ),
                });
            }
        }
        frames.sort_by_key(|frame| (frame.pts.units, frame.pts.scale.units_per_second));
        self.decoded_batches = self.decoded_batches.saturating_add(1);
        Ok(DecodedVideoOutput {
            stream: DecodedVideoStream {
                format: self.output_format,
                source_codec: VideoCodec::Av1,
                decoder: "chroma-dav1d-av1-decoder".to_string(),
            },
            frames,
        })
    }

    fn receive_one(&mut self, frames: &mut Vec<DecodedVideoFrame>) -> Result<(), VideoDecodeError> {
        match self.decoder.get_picture() {
            Ok(picture) => {
                let timing = take_dav1d_timing(&mut self.pending_timing, &picture)?;
                frames.push(copy_dav1d_bgra_frame(&picture, self.output_format, timing)?);
                Ok(())
            }
            Err(dav1d::Error::Again) => Ok(()),
            Err(error) => Err(dav1d_decode_error("frame receive", error)),
        }
    }

    fn receive_available(
        &mut self,
        frames: &mut Vec<DecodedVideoFrame>,
    ) -> Result<(), VideoDecodeError> {
        loop {
            match self.decoder.get_picture() {
                Ok(picture) => {
                    let timing = take_dav1d_timing(&mut self.pending_timing, &picture)?;
                    frames.push(copy_dav1d_bgra_frame(&picture, self.output_format, timing)?);
                }
                Err(dav1d::Error::Again) => return Ok(()),
                Err(error) => return Err(dav1d_decode_error("decoder drain", error)),
            }
        }
    }

    /// Returns the number of batches decoded by this software session.
    pub fn decoded_batches(&self) -> u64 {
        self.decoded_batches
    }
}

impl std::fmt::Debug for CpuHevcBgraDecoderSession {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("CpuHevcBgraDecoderSession")
            .field("output_format", &self.output_format)
            .field("decoded_batches", &self.decoded_batches)
            .field("pending_frames", &self.pending_timing.len())
            .finish_non_exhaustive()
    }
}

impl CpuHevcBgraDecoderSession {
    /// Creates a portable HEVC decoder from an HEVCDecoderConfigurationRecord.
    pub fn new(
        output_format: RawVideoFormat,
        decoder_config: &[u8],
    ) -> Result<Self, VideoDecodeError> {
        validate_decoded_video_format(output_format)?;
        if decoder_config.is_empty() {
            return Err(VideoDecodeError::MissingDecoderConfig);
        }
        let config =
            crate::codec::hevc::parse_hevc_decoder_config(decoder_config).map_err(|error| {
                VideoDecodeError::InvalidDecoderConfig {
                    reason: error.to_string(),
                }
            })?;
        if !(32..=34).all(|nal_type| {
            config
                .arrays
                .iter()
                .any(|array| array.nal_unit_type == nal_type && !array.units.is_empty())
        }) {
            return Err(VideoDecodeError::InvalidDecoderConfig {
                reason: "HEVC decoder configuration does not contain VPS/SPS/PPS".to_string(),
            });
        }
        let parameter_sets = crate::codec::hevc::hevc_parameter_sets_to_annex_b(&config);
        let mut decoder = rust_h265::Decoder::new();
        feed_hevc_nals(&mut decoder, &parameter_sets)?;
        Ok(Self {
            output_format,
            decoded_batches: 0,
            nalu_length_size: config.nalu_length_size,
            pending_timing: Vec::new(),
            decoder,
        })
    }

    /// Decodes one ordered HEVC packet batch without recreating the software decoder.
    pub fn decode(
        &mut self,
        input: &VideoDecodeInput<'_>,
    ) -> Result<DecodedVideoOutput, VideoDecodeError> {
        if input.codec != VideoCodec::Hevc {
            return Err(VideoDecodeError::UnsupportedCodec);
        }
        if input.time_scale.units_per_second == 0 {
            return Err(VideoDecodeError::InvalidInput {
                reason: "packet time scale must be greater than zero".to_string(),
            });
        }
        let mut output_frames = Vec::new();
        for packet in &input.packets {
            validate_packet_time_scale(packet, input.time_scale)?;
            let annex_b =
                crate::codec::hevc::hevc_sample_to_annex_b(packet.bytes, self.nalu_length_size)
                    .map_err(|error| VideoDecodeError::InvalidInput {
                        reason: format!("invalid HEVC packet: {error}"),
                    })?;
            insert_cpu_decode_timing(
                &mut self.pending_timing,
                CpuDecodeTiming {
                    pts: packet.pts,
                    dts: packet.dts,
                    duration: packet.duration,
                    keyframe: packet.keyframe,
                },
            );
            for decoded in feed_hevc_nals(&mut self.decoder, &annex_b)? {
                let timing = take_cpu_decode_timing(&mut self.pending_timing, "HEVC")?;
                output_frames.push(copy_hevc_bgra_frame(decoded, self.output_format, timing)?);
            }
        }
        if input.end_of_stream {
            if let Some(decoded) = self.decoder.flush() {
                let timing = take_cpu_decode_timing(&mut self.pending_timing, "HEVC")?;
                output_frames.push(copy_hevc_bgra_frame(decoded, self.output_format, timing)?);
            }
            if !self.pending_timing.is_empty() {
                return Err(VideoDecodeError::BackendFailed {
                    reason: format!(
                        "HEVC decoder drained with {} packet timing record(s) unmatched",
                        self.pending_timing.len()
                    ),
                });
            }
        }
        output_frames.sort_by_key(|frame| (frame.pts.units, frame.pts.scale.units_per_second));
        self.decoded_batches = self.decoded_batches.saturating_add(1);
        Ok(DecodedVideoOutput {
            stream: DecodedVideoStream {
                format: self.output_format,
                source_codec: VideoCodec::Hevc,
                decoder: "chroma-cpu-hevc-decoder".to_string(),
            },
            frames: output_frames,
        })
    }

    /// Returns the number of batches decoded by this software session.
    pub fn decoded_batches(&self) -> u64 {
        self.decoded_batches
    }
}

impl std::fmt::Debug for CpuH264BgraDecoderSession {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("CpuH264BgraDecoderSession")
            .field("output_format", &self.output_format)
            .field("decoded_batches", &self.decoded_batches)
            .field("pending_frames", &self.pending_timing.len())
            .finish_non_exhaustive()
    }
}

impl CpuH264BgraDecoderSession {
    /// Creates one portable H.264 decoder that can serve multiple ordered packet batches.
    pub fn new(
        output_format: RawVideoFormat,
        decoder_config: &[u8],
    ) -> Result<Self, VideoDecodeError> {
        validate_decoded_video_format(output_format)?;
        if decoder_config.is_empty() {
            return Err(VideoDecodeError::MissingDecoderConfig);
        }
        let config =
            crate::codec::h264::parse_avc_decoder_config(decoder_config).map_err(|error| {
                VideoDecodeError::InvalidDecoderConfig {
                    reason: error.to_string(),
                }
            })?;
        if config.sps.is_empty() || config.pps.is_empty() {
            return Err(VideoDecodeError::InvalidDecoderConfig {
                reason: "AVC decoder configuration does not contain SPS and PPS".to_string(),
            });
        }
        let mut parameter_sets = Vec::new();
        for parameter_set in config.sps.iter().chain(config.pps.iter()) {
            parameter_sets.extend_from_slice(&[0, 0, 0, 1]);
            parameter_sets.extend_from_slice(parameter_set);
        }
        let decoder_config = openh264::decoder::DecoderConfig::new()
            .flush_after_decode(openh264::decoder::Flush::NoFlush);
        let decoder = openh264::decoder::Decoder::with_api_config(
            openh264::OpenH264API::from_source(),
            decoder_config,
        )
        .map_err(|error| VideoDecodeError::BackendFailed {
            reason: format!("OpenH264 decoder initialization failed: {error}"),
        })?;
        Ok(Self {
            output_format,
            decoded_batches: 0,
            nalu_length_size: config.nalu_length_size,
            parameter_sets: Some(parameter_sets),
            pending_timing: Vec::new(),
            decoder,
        })
    }

    /// Decodes one ordered H.264 packet batch without recreating the software decoder.
    pub fn decode(
        &mut self,
        input: &VideoDecodeInput<'_>,
    ) -> Result<DecodedVideoOutput, VideoDecodeError> {
        if input.codec != VideoCodec::H264 {
            return Err(VideoDecodeError::UnsupportedCodec);
        }
        if input.time_scale.units_per_second == 0 {
            return Err(VideoDecodeError::InvalidInput {
                reason: "packet time scale must be greater than zero".to_string(),
            });
        }
        let mut output_frames = Vec::new();
        for packet in &input.packets {
            if packet.pts.scale != input.time_scale
                || packet.dts.scale != input.time_scale
                || packet.duration.scale != input.time_scale
            {
                return Err(VideoDecodeError::InvalidInput {
                    reason: "packet timestamps must match the decode input time scale".to_string(),
                });
            }
            let sample_annex_b =
                crate::codec::h264::avc_sample_to_annex_b(packet.bytes, self.nalu_length_size)
                    .map_err(|error| VideoDecodeError::InvalidInput {
                        reason: format!("invalid AVCC packet: {error}"),
                    })?;
            let mut annex_b = self.parameter_sets.take().unwrap_or_default();
            annex_b.extend_from_slice(&sample_annex_b);
            insert_cpu_decode_timing(
                &mut self.pending_timing,
                CpuDecodeTiming {
                    pts: packet.pts,
                    dts: packet.dts,
                    duration: packet.duration,
                    keyframe: packet.keyframe,
                },
            );
            let decoded =
                self.decoder
                    .decode(&annex_b)
                    .map_err(|error| VideoDecodeError::BackendFailed {
                        reason: format!("OpenH264 packet decode failed: {error}"),
                    })?;
            if let Some(decoded) = decoded {
                let timing = take_cpu_decode_timing(&mut self.pending_timing, "OpenH264")?;
                output_frames.push(copy_openh264_bgra_frame(
                    &decoded,
                    self.output_format,
                    timing,
                )?);
            }
        }
        if input.end_of_stream {
            let remaining = self.decoder.flush_remaining().map_err(|error| {
                VideoDecodeError::BackendFailed {
                    reason: format!("OpenH264 decoder drain failed: {error}"),
                }
            })?;
            for decoded in remaining {
                let timing = take_cpu_decode_timing(&mut self.pending_timing, "OpenH264")?;
                output_frames.push(copy_openh264_bgra_frame(
                    &decoded,
                    self.output_format,
                    timing,
                )?);
            }
            if !self.pending_timing.is_empty() {
                return Err(VideoDecodeError::BackendFailed {
                    reason: format!(
                        "OpenH264 drained with {} packet timing record(s) unmatched",
                        self.pending_timing.len()
                    ),
                });
            }
        }
        output_frames.sort_by_key(|frame| (frame.pts.units, frame.pts.scale.units_per_second));
        self.decoded_batches = self.decoded_batches.saturating_add(1);
        Ok(DecodedVideoOutput {
            stream: DecodedVideoStream {
                format: self.output_format,
                source_codec: VideoCodec::H264,
                decoder: "chroma-cpu-h264-decoder".to_string(),
            },
            frames: output_frames,
        })
    }

    /// Returns the number of batches decoded by this software session.
    pub fn decoded_batches(&self) -> u64 {
        self.decoded_batches
    }
}

/// Preferred retained BGRA decoder, with platform hardware first and CPU fallback for H.264.
#[derive(Debug)]
pub enum BgraDecoderSession {
    /// Apple VideoToolbox decoder selected on macOS.
    VideoToolbox(VideoToolboxBgraDecoderSession),
    /// Portable OpenH264 software decoder.
    CpuH264(Box<CpuH264BgraDecoderSession>),
    /// Portable safe-Rust HEVC software decoder.
    CpuHevc(Box<CpuHevcBgraDecoderSession>),
    /// Portable dav1d AV1 software decoder.
    CpuAv1(Box<CpuAv1BgraDecoderSession>),
    /// Linux VA-API H.264 decoder using runtime-loaded libva.
    #[cfg(all(target_os = "linux", feature = "linux-vaapi"))]
    VaapiH264(Box<VaapiH264BgraDecoderSession>),
    /// Linux VA-API HEVC decoder using runtime-loaded libva.
    #[cfg(all(target_os = "linux", feature = "linux-vaapi"))]
    VaapiHevc(Box<VaapiHevcBgraDecoderSession>),
}

impl BgraDecoderSession {
    /// Creates the preferred executable decoder for the current host and codec.
    pub fn new(
        codec: VideoCodec,
        output_format: RawVideoFormat,
        decoder_config: &[u8],
    ) -> Result<Self, VideoDecodeError> {
        #[cfg(target_os = "macos")]
        if let Ok(session) =
            VideoToolboxBgraDecoderSession::new(codec, output_format, decoder_config)
        {
            return Ok(Self::VideoToolbox(session));
        }
        #[cfg(all(target_os = "linux", feature = "linux-vaapi"))]
        if codec == VideoCodec::H264
            && let Ok(session) = VaapiH264BgraDecoderSession::new(output_format, decoder_config)
        {
            return Ok(Self::VaapiH264(Box::new(session)));
        }
        #[cfg(all(target_os = "linux", feature = "linux-vaapi"))]
        if codec == VideoCodec::Hevc
            && let Ok(session) = VaapiHevcBgraDecoderSession::new(output_format, decoder_config)
        {
            return Ok(Self::VaapiHevc(Box::new(session)));
        }
        match codec {
            VideoCodec::H264 => CpuH264BgraDecoderSession::new(output_format, decoder_config)
                .map(|session| Self::CpuH264(Box::new(session))),
            VideoCodec::Hevc => CpuHevcBgraDecoderSession::new(output_format, decoder_config)
                .map(|session| Self::CpuHevc(Box::new(session))),
            VideoCodec::Av1 => CpuAv1BgraDecoderSession::new(output_format)
                .map(|session| Self::CpuAv1(Box::new(session))),
        }
    }

    /// Decodes one packet batch using the selected retained backend.
    pub fn decode(
        &mut self,
        input: &VideoDecodeInput<'_>,
    ) -> Result<DecodedVideoOutput, VideoDecodeError> {
        match self {
            Self::VideoToolbox(session) => session.decode(input),
            Self::CpuH264(session) => session.decode(input),
            Self::CpuHevc(session) => session.decode(input),
            Self::CpuAv1(session) => session.decode(input),
            #[cfg(all(target_os = "linux", feature = "linux-vaapi"))]
            Self::VaapiH264(session) => session.decode(input),
            #[cfg(all(target_os = "linux", feature = "linux-vaapi"))]
            Self::VaapiHevc(session) => session.decode(input),
        }
    }

    /// Returns the number of batches decoded by the selected backend.
    pub fn decoded_batches(&self) -> u64 {
        match self {
            Self::VideoToolbox(session) => session.decoded_batches(),
            Self::CpuH264(session) => session.decoded_batches(),
            Self::CpuHevc(session) => session.decoded_batches(),
            Self::CpuAv1(session) => session.decoded_batches(),
            #[cfg(all(target_os = "linux", feature = "linux-vaapi"))]
            Self::VaapiH264(session) => session.decoded_batches(),
            #[cfg(all(target_os = "linux", feature = "linux-vaapi"))]
            Self::VaapiHevc(session) => session.decoded_batches(),
        }
    }

    /// Returns the stable backend identifier selected for this session.
    pub fn backend_name(&self) -> &'static str {
        match self {
            Self::VideoToolbox(session) => match session.codec {
                VideoCodec::H264 => "chroma-videotoolbox-h264-decoder",
                VideoCodec::Hevc => "chroma-videotoolbox-hevc-decoder",
                VideoCodec::Av1 => "chroma-videotoolbox-av1-decoder",
            },
            Self::CpuH264(_) => "chroma-cpu-h264-decoder",
            Self::CpuHevc(_) => "chroma-cpu-hevc-decoder",
            Self::CpuAv1(_) => "chroma-dav1d-av1-decoder",
            #[cfg(all(target_os = "linux", feature = "linux-vaapi"))]
            Self::VaapiH264(_) => "chroma-vaapi-h264-decoder",
            #[cfg(all(target_os = "linux", feature = "linux-vaapi"))]
            Self::VaapiHevc(_) => "chroma-vaapi-hevc-decoder",
        }
    }
}

#[derive(Debug, Clone, Copy)]
struct CpuDecodeTiming {
    pts: TimePoint,
    dts: TimePoint,
    duration: TimeDelta,
    keyframe: bool,
}

fn insert_cpu_decode_timing(pending: &mut Vec<CpuDecodeTiming>, timing: CpuDecodeTiming) {
    let index = pending.partition_point(|candidate| {
        (candidate.pts.units, candidate.pts.scale.units_per_second)
            <= (timing.pts.units, timing.pts.scale.units_per_second)
    });
    pending.insert(index, timing);
}

fn take_cpu_decode_timing(
    pending: &mut Vec<CpuDecodeTiming>,
    backend: &str,
) -> Result<CpuDecodeTiming, VideoDecodeError> {
    if pending.is_empty() {
        return Err(VideoDecodeError::BackendFailed {
            reason: format!("{backend} emitted a frame without matching packet timing"),
        });
    }
    Ok(pending.remove(0))
}

fn take_dav1d_timing(
    pending: &mut Vec<CpuDecodeTiming>,
    picture: &dav1d::Picture,
) -> Result<CpuDecodeTiming, VideoDecodeError> {
    if let Some(timestamp) = picture.timestamp()
        && let Ok(timestamp) = u64::try_from(timestamp)
        && let Some(index) = pending
            .iter()
            .position(|timing| timing.pts.units == timestamp)
    {
        return Ok(pending.remove(index));
    }
    take_cpu_decode_timing(pending, "dav1d")
}

fn dav1d_decode_error(operation: &str, error: dav1d::Error) -> VideoDecodeError {
    VideoDecodeError::BackendFailed {
        reason: format!("dav1d {operation} failed: {error}"),
    }
}

fn validate_packet_time_scale(
    packet: &CompressedVideoPacket<'_>,
    time_scale: TimeScale,
) -> Result<(), VideoDecodeError> {
    if packet.pts.scale != time_scale
        || packet.dts.scale != time_scale
        || packet.duration.scale != time_scale
    {
        return Err(VideoDecodeError::InvalidInput {
            reason: "packet timestamps must match the decode input time scale".to_string(),
        });
    }
    Ok(())
}

fn feed_hevc_nals(
    decoder: &mut rust_h265::Decoder,
    annex_b: &[u8],
) -> Result<Vec<rust_h265::Frame>, VideoDecodeError> {
    let nals = rust_h265::parse_annex_b(annex_b);
    if nals.is_empty() {
        return Err(VideoDecodeError::InvalidInput {
            reason: "HEVC Annex-B packet contains no NAL units".to_string(),
        });
    }
    let mut frames = Vec::new();
    for nal in &nals {
        if let Some(frame) =
            decoder
                .decode_nal(nal)
                .map_err(|error| VideoDecodeError::BackendFailed {
                    reason: format!("HEVC packet decode failed: {error}"),
                })?
        {
            frames.push(frame);
        }
    }
    Ok(frames)
}

fn copy_hevc_bgra_frame(
    decoded: rust_h265::Frame,
    format: RawVideoFormat,
    timing: CpuDecodeTiming,
) -> Result<DecodedVideoFrame, VideoDecodeError> {
    if (decoded.width, decoded.height) != (format.width, format.height) {
        return Err(VideoDecodeError::InvalidOutputFormat {
            reason: format!(
                "HEVC decoded {}x{}, expected {}x{}",
                decoded.width, decoded.height, format.width, format.height
            ),
        });
    }
    let width = format.width as usize;
    let height = format.height as usize;
    let chroma_width = width.div_ceil(2);
    let chroma_height = height.div_ceil(2);
    let luma_pixels =
        width
            .checked_mul(height)
            .ok_or_else(|| VideoDecodeError::InvalidOutputFormat {
                reason: "HEVC decoded luma dimensions overflowed".to_string(),
            })?;
    let chroma_pixels = chroma_width.checked_mul(chroma_height).ok_or_else(|| {
        VideoDecodeError::InvalidOutputFormat {
            reason: "HEVC decoded chroma dimensions overflowed".to_string(),
        }
    })?;
    if decoded.y.len() != luma_pixels
        || decoded.u.len() != chroma_pixels
        || decoded.v.len() != chroma_pixels
    {
        return Err(VideoDecodeError::InvalidOutputFormat {
            reason: "HEVC decoder returned invalid YUV420 plane sizes".to_string(),
        });
    }
    let byte_count =
        luma_pixels
            .checked_mul(4)
            .ok_or_else(|| VideoDecodeError::InvalidOutputFormat {
                reason: "HEVC decoded BGRA byte count overflowed".to_string(),
            })?;
    let mut pixels = vec![0_u8; byte_count];
    for row in 0..height {
        for column in 0..width {
            let y = hevc_pixel_to_u8(&decoded.y, row * width + column, decoded.bit_depth)?;
            let chroma_index = (row / 2) * chroma_width + column / 2;
            let u = hevc_pixel_to_u8(&decoded.u, chroma_index, decoded.bit_depth)?;
            let v = hevc_pixel_to_u8(&decoded.v, chroma_index, decoded.bit_depth)?;
            let offset = (row * width + column) * 4;
            pixels[offset..offset + 4].copy_from_slice(&limited_yuv_to_bgra(y, u, v));
        }
    }
    Ok(DecodedVideoFrame {
        pts: timing.pts,
        dts: timing.dts,
        duration: timing.duration,
        format,
        pixels,
        keyframe: timing.keyframe,
    })
}

fn copy_dav1d_bgra_frame(
    picture: &dav1d::Picture,
    format: RawVideoFormat,
    timing: CpuDecodeTiming,
) -> Result<DecodedVideoFrame, VideoDecodeError> {
    use dav1d::{PixelLayout, PlanarImageComponent};

    if (picture.width(), picture.height()) != (format.width, format.height) {
        return Err(VideoDecodeError::InvalidOutputFormat {
            reason: format!(
                "dav1d decoded {}x{}, expected {}x{}",
                picture.width(),
                picture.height(),
                format.width,
                format.height
            ),
        });
    }
    let width = format.width as usize;
    let height = format.height as usize;
    let byte_count = width
        .checked_mul(height)
        .and_then(|pixels| pixels.checked_mul(4))
        .ok_or_else(|| VideoDecodeError::InvalidOutputFormat {
            reason: "AV1 decoded BGRA byte count overflowed".to_string(),
        })?;
    let layout = picture.pixel_layout();
    let component_bits = picture
        .bits_per_component()
        .map(|bits| bits.0)
        .ok_or_else(|| VideoDecodeError::InvalidOutputFormat {
            reason: "dav1d returned an unknown component depth".to_string(),
        })?;
    let reported_bits = picture.bit_depth();
    if !(8..=12).contains(&component_bits) || reported_bits != component_bits {
        return Err(VideoDecodeError::InvalidOutputFormat {
            reason: format!(
                "dav1d returned unsupported component depth {component_bits} (reported {reported_bits})"
            ),
        });
    }
    let y_plane = picture.plane(PlanarImageComponent::Y);
    let u_plane = picture.plane(PlanarImageComponent::U);
    let v_plane = picture.plane(PlanarImageComponent::V);
    let y_stride = picture.stride(PlanarImageComponent::Y) as usize;
    let uv_stride = picture.stride(PlanarImageComponent::U) as usize;
    let bytes_per_component = if component_bits == 8 { 1 } else { 2 };
    let chroma_width = match layout {
        PixelLayout::I400 => 0,
        PixelLayout::I420 | PixelLayout::I422 => width.div_ceil(2),
        PixelLayout::I444 => width,
    };
    let chroma_height = match layout {
        PixelLayout::I420 => height.div_ceil(2),
        PixelLayout::I400 => 0,
        PixelLayout::I422 | PixelLayout::I444 => height,
    };
    validate_dav1d_plane(
        y_plane.as_ref(),
        y_stride,
        width,
        height,
        bytes_per_component,
        "Y",
    )?;
    if layout != PixelLayout::I400 {
        validate_dav1d_plane(
            u_plane.as_ref(),
            uv_stride,
            chroma_width,
            chroma_height,
            bytes_per_component,
            "U",
        )?;
        validate_dav1d_plane(
            v_plane.as_ref(),
            uv_stride,
            chroma_width,
            chroma_height,
            bytes_per_component,
            "V",
        )?;
    }

    let mut pixels = vec![0_u8; byte_count];
    for row in 0..height {
        for column in 0..width {
            let y = dav1d_sample_u8(y_plane.as_ref(), y_stride, row, column, component_bits)?;
            let (u, v) = if layout == PixelLayout::I400 {
                (128, 128)
            } else {
                let chroma_row = if layout == PixelLayout::I420 {
                    row / 2
                } else {
                    row
                };
                let chroma_column = if matches!(layout, PixelLayout::I420 | PixelLayout::I422) {
                    column / 2
                } else {
                    column
                };
                (
                    dav1d_sample_u8(
                        u_plane.as_ref(),
                        uv_stride,
                        chroma_row,
                        chroma_column,
                        component_bits,
                    )?,
                    dav1d_sample_u8(
                        v_plane.as_ref(),
                        uv_stride,
                        chroma_row,
                        chroma_column,
                        component_bits,
                    )?,
                )
            };
            let offset = (row * width + column) * 4;
            pixels[offset..offset + 4].copy_from_slice(&limited_yuv_to_bgra(y, u, v));
        }
    }
    Ok(DecodedVideoFrame {
        pts: timing.pts,
        dts: timing.dts,
        duration: timing.duration,
        format,
        pixels,
        keyframe: timing.keyframe,
    })
}

fn validate_dav1d_plane(
    plane: &[u8],
    stride: usize,
    width: usize,
    height: usize,
    bytes_per_component: usize,
    name: &str,
) -> Result<(), VideoDecodeError> {
    let row_bytes = width.checked_mul(bytes_per_component).ok_or_else(|| {
        VideoDecodeError::InvalidOutputFormat {
            reason: format!("dav1d {name} plane width overflowed"),
        }
    })?;
    let required =
        stride
            .checked_mul(height)
            .ok_or_else(|| VideoDecodeError::InvalidOutputFormat {
                reason: format!("dav1d {name} plane height overflowed"),
            })?;
    if stride < row_bytes || plane.len() < required {
        return Err(VideoDecodeError::InvalidOutputFormat {
            reason: format!("dav1d returned a truncated {name} plane"),
        });
    }
    Ok(())
}

fn dav1d_sample_u8(
    plane: &[u8],
    stride: usize,
    row: usize,
    column: usize,
    component_bits: usize,
) -> Result<u8, VideoDecodeError> {
    let bytes_per_component = if component_bits == 8 { 1 } else { 2 };
    let offset = row
        .checked_mul(stride)
        .and_then(|value| value.checked_add(column.checked_mul(bytes_per_component)?))
        .ok_or_else(|| VideoDecodeError::InvalidOutputFormat {
            reason: "dav1d plane offset overflowed".to_string(),
        })?;
    let value = if component_bits == 8 {
        u16::from(
            *plane
                .get(offset)
                .ok_or_else(|| VideoDecodeError::InvalidOutputFormat {
                    reason: "dav1d returned a truncated pixel plane".to_string(),
                })?,
        )
    } else {
        let bytes =
            plane
                .get(offset..offset + 2)
                .ok_or_else(|| VideoDecodeError::InvalidOutputFormat {
                    reason: "dav1d returned a truncated pixel plane".to_string(),
                })?;
        u16::from_ne_bytes([bytes[0], bytes[1]])
    };
    Ok((value >> component_bits.saturating_sub(8)).min(255) as u8)
}

fn hevc_pixel_to_u8(
    plane: &rust_h265::PixelData,
    index: usize,
    bit_depth: u8,
) -> Result<u8, VideoDecodeError> {
    if !(8..=16).contains(&bit_depth) {
        return Err(VideoDecodeError::InvalidOutputFormat {
            reason: format!("HEVC decoder returned unsupported {bit_depth}-bit pixels"),
        });
    }
    match plane {
        rust_h265::PixelData::U8(values) => values.get(index).copied(),
        rust_h265::PixelData::U16(values) => values.get(index).map(|value| {
            let shift = bit_depth.saturating_sub(8);
            (value >> shift).min(255) as u8
        }),
    }
    .ok_or_else(|| VideoDecodeError::InvalidOutputFormat {
        reason: "HEVC decoder returned a truncated pixel plane".to_string(),
    })
}

fn copy_openh264_bgra_frame(
    decoded: &openh264::decoder::DecodedYUV<'_>,
    format: RawVideoFormat,
    timing: CpuDecodeTiming,
) -> Result<DecodedVideoFrame, VideoDecodeError> {
    use openh264::formats::YUVSource as _;

    let dimensions = decoded.dimensions();
    if dimensions != (format.width as usize, format.height as usize) {
        return Err(VideoDecodeError::InvalidOutputFormat {
            reason: format!(
                "OpenH264 decoded {}x{}, expected {}x{}",
                dimensions.0, dimensions.1, format.width, format.height
            ),
        });
    }
    let byte_count = dimensions
        .0
        .checked_mul(dimensions.1)
        .and_then(|pixels| pixels.checked_mul(4))
        .ok_or_else(|| VideoDecodeError::InvalidOutputFormat {
            reason: "decoded BGRA frame byte count overflowed".to_string(),
        })?;
    let mut pixels = vec![0; byte_count];
    decoded.write_rgba8(&mut pixels);
    for pixel in pixels.chunks_exact_mut(4) {
        pixel.swap(0, 2);
    }
    Ok(DecodedVideoFrame {
        pts: timing.pts,
        dts: timing.dts,
        duration: timing.duration,
        format,
        pixels,
        keyframe: timing.keyframe,
    })
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

/// Decodes H.264 AVCC packets to tightly packed BGRA frames using portable CPU software.
pub fn decode_h264_cpu_bgra_frames(
    input: &VideoDecodeInput<'_>,
    output_format: RawVideoFormat,
) -> Result<DecodedVideoOutput, VideoDecodeError> {
    if input.codec != VideoCodec::H264 {
        return Err(VideoDecodeError::UnsupportedCodec);
    }
    let decoder_config = input
        .decoder_config
        .ok_or(VideoDecodeError::MissingDecoderConfig)?;
    CpuH264BgraDecoderSession::new(output_format, decoder_config)?.decode(input)
}

/// Decodes HEVC length-prefixed packets to tightly packed BGRA using portable CPU software.
pub fn decode_hevc_cpu_bgra_frames(
    input: &VideoDecodeInput<'_>,
    output_format: RawVideoFormat,
) -> Result<DecodedVideoOutput, VideoDecodeError> {
    if input.codec != VideoCodec::Hevc {
        return Err(VideoDecodeError::UnsupportedCodec);
    }
    let decoder_config = input
        .decoder_config
        .ok_or(VideoDecodeError::MissingDecoderConfig)?;
    CpuHevcBgraDecoderSession::new(output_format, decoder_config)?.decode(input)
}

/// Decodes AV1 low-overhead bitstream packets to tightly packed BGRA using dav1d.
pub fn decode_av1_cpu_bgra_frames(
    input: &VideoDecodeInput<'_>,
    output_format: RawVideoFormat,
) -> Result<DecodedVideoOutput, VideoDecodeError> {
    if input.codec != VideoCodec::Av1 {
        return Err(VideoDecodeError::UnsupportedCodec);
    }
    CpuAv1BgraDecoderSession::new(output_format)?.decode(input)
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
        VideoCodec::Av1 => return Err(VideoDecodeError::UnsupportedCodec),
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
                VideoCodec::Av1 => return Err(VideoDecodeError::UnsupportedCodec),
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

    #[test]
    fn cpu_h264_round_trip_emits_bgra_with_source_timing() {
        use crate::transcode::{CpuH264EncoderSession, RawVideoFrameRef};

        let format = valid_format();
        let frame_bytes = format.width as usize * format.height as usize * 4;
        let black = vec![0; frame_bytes];
        let white = vec![255; frame_bytes];
        let scale = TimeScale {
            units_per_second: 24,
        };
        let source_frames = [
            RawVideoFrameRef {
                pts: TimePoint { units: 0, scale },
                dts: TimePoint { units: 0, scale },
                duration: TimeDelta { units: 1, scale },
                bytes: &black,
                keyframe: true,
            },
            RawVideoFrameRef {
                pts: TimePoint { units: 1, scale },
                dts: TimePoint { units: 1, scale },
                duration: TimeDelta { units: 1, scale },
                bytes: &white,
                keyframe: false,
            },
        ];
        let encoded = CpuH264EncoderSession::new(format, 500_000)
            .expect("CPU encoder")
            .encode(&source_frames)
            .expect("encode test frames");
        let packets = encoded
            .frames
            .iter()
            .enumerate()
            .map(|(index, frame)| CompressedVideoPacket {
                index: index as u32,
                pts: frame.pts,
                dts: frame.dts,
                duration: frame.duration,
                keyframe: frame.keyframe,
                bytes: &frame.payload,
            })
            .collect();
        let decoder_config = encoded.stream.decoder_config.as_deref().expect("avcC");
        let input = VideoDecodeInput {
            codec: VideoCodec::H264,
            time_scale: scale,
            decoder_config: Some(decoder_config),
            packets,
            end_of_stream: true,
        };

        let decoded = decode_h264_cpu_bgra_frames(&input, format).expect("decode test frames");

        assert_eq!(decoded.stream.decoder, "chroma-cpu-h264-decoder");
        assert_eq!(decoded.frames.len(), 2);
        assert_eq!(decoded.frames[0].pts.units, 0);
        assert_eq!(decoded.frames[1].pts.units, 1);
        assert!(
            decoded
                .frames
                .iter()
                .all(|frame| { frame.format == format && frame.pixels.len() == frame_bytes })
        );
    }

    #[test]
    fn dav1d_decodes_redistributable_8_bit_fixture() {
        let fixture = parse_av1_fixture(include_str!(concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/tests/fixtures/av1/testsrc2-32x32-8bit.txt"
        )));
        let input = build_video_decode_input(
            VideoCodec::Av1,
            fixture.time_scale,
            None,
            &fixture.samples,
            &fixture.payload,
            true,
        )
        .unwrap();

        let output = decode_av1_cpu_bgra_frames(&input, fixture.format).unwrap();

        assert_eq!(output.stream.decoder, "chroma-dav1d-av1-decoder");
        assert_eq!(output.frames.len(), 4);
        assert_av1_fixture_frames(&output.frames, fixture.format, fixture.time_scale);
    }

    #[test]
    fn dav1d_retains_state_across_10_bit_packet_batches() {
        use crate::transcode::CpuH264EncoderSession;

        let fixture = parse_av1_fixture(include_str!(concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/tests/fixtures/av1/testsrc2-32x32-10bit.txt"
        )));
        let mut decoder = CpuAv1BgraDecoderSession::new(fixture.format).unwrap();
        let first = build_video_decode_input(
            VideoCodec::Av1,
            fixture.time_scale,
            None,
            &fixture.samples[..2],
            &fixture.payload,
            false,
        )
        .unwrap();
        let second = build_video_decode_input(
            VideoCodec::Av1,
            fixture.time_scale,
            None,
            &fixture.samples[2..],
            &fixture.payload,
            true,
        )
        .unwrap();

        let first_frames = decoder.decode(&first).unwrap().frames;
        let second_frames = decoder.decode(&second).unwrap().frames;
        assert_eq!(first_frames.len(), 2);
        assert_eq!(second_frames.len(), 2);

        let mut encoder = CpuH264EncoderSession::new(fixture.format, 250_000).unwrap();
        let first_refs = raw_frame_refs(&first_frames);
        let second_refs = raw_frame_refs(&second_frames);
        let first_encoded = encoder.encode(&first_refs).unwrap();
        let second_encoded = encoder.encode(&second_refs).unwrap();
        assert_eq!(first_encoded.frames.len() + second_encoded.frames.len(), 4);
        assert_eq!(encoder.encoded_batches(), 2);

        let mut frames = first_frames;
        frames.extend(second_frames);

        assert_eq!(decoder.decoded_batches(), 2);
        assert_eq!(frames.len(), 4);
        frames.sort_by_key(|frame| frame.pts.units);
        assert_av1_fixture_frames(&frames, fixture.format, fixture.time_scale);
    }

    struct Av1Fixture {
        format: RawVideoFormat,
        time_scale: TimeScale,
        samples: Vec<ChunkSample>,
        payload: Vec<u8>,
    }

    fn parse_av1_fixture(source: &str) -> Av1Fixture {
        let mut lines = source
            .lines()
            .map(str::trim)
            .filter(|line| !line.is_empty() && !line.starts_with('#'));
        let header = lines.next().unwrap().split_whitespace().collect::<Vec<_>>();
        let width = header[0].parse().unwrap();
        let height = header[1].parse().unwrap();
        let time_scale = TimeScale {
            units_per_second: header[2].parse().unwrap(),
        };
        let mut samples = Vec::new();
        let mut payload = Vec::new();
        for line in lines {
            let fields = line.split_whitespace().collect::<Vec<_>>();
            let bytes = decode_hex(fields[4]);
            let payload_offset = payload.len() as u64;
            payload.extend_from_slice(&bytes);
            let pts = TimePoint {
                units: fields[1].parse().unwrap(),
                scale: time_scale,
            };
            samples.push(ChunkSample {
                index: fields[0].parse().unwrap(),
                payload_offset,
                byte_count: bytes.len() as u32,
                pts,
                dts: pts,
                duration: TimeDelta {
                    units: fields[2].parse().unwrap(),
                    scale: time_scale,
                },
                keyframe: fields[3] == "1",
            });
        }
        Av1Fixture {
            format: RawVideoFormat {
                width,
                height,
                frame_rate_num: 4,
                frame_rate_den: 1,
                pixel_format: RawVideoPixelFormat::Bgra,
            },
            time_scale,
            samples,
            payload,
        }
    }

    fn decode_hex(hex: &str) -> Vec<u8> {
        assert_eq!(hex.len() % 2, 0);
        hex.as_bytes()
            .chunks_exact(2)
            .map(|pair| {
                let pair = std::str::from_utf8(pair).unwrap();
                u8::from_str_radix(pair, 16).unwrap()
            })
            .collect()
    }

    fn assert_av1_fixture_frames(
        frames: &[DecodedVideoFrame],
        format: RawVideoFormat,
        time_scale: TimeScale,
    ) {
        let expected_pts = [0, 250_000_000, 500_000_000, 750_000_000];
        for (frame, expected_pts) in frames.iter().zip(expected_pts) {
            assert_eq!(frame.format, format);
            assert_eq!(
                frame.pts,
                TimePoint {
                    units: expected_pts,
                    scale: time_scale
                }
            );
            assert_eq!(
                frame.duration,
                TimeDelta {
                    units: 250_000_000,
                    scale: time_scale
                }
            );
            assert_eq!(frame.pixels.len(), 32 * 32 * 4);
            assert!(frame.pixels.chunks_exact(4).all(|pixel| pixel[3] == 255));
            assert!(frame.pixels.windows(2).any(|pair| pair[0] != pair[1]));
        }
    }

    fn raw_frame_refs(frames: &[DecodedVideoFrame]) -> Vec<crate::transcode::RawVideoFrameRef<'_>> {
        frames
            .iter()
            .map(|frame| crate::transcode::RawVideoFrameRef {
                pts: frame.pts,
                dts: frame.pts,
                duration: frame.duration,
                bytes: &frame.pixels,
                keyframe: frame.keyframe,
            })
            .collect()
    }

    #[test]
    fn cpu_hevc_decodes_real_access_unit_to_bgra() {
        const VPS: &[u8] = &[
            0x40, 0x01, 0x0c, 0x01, 0xff, 0xff, 0x03, 0x70, 0x00, 0x00, 0x03, 0x00, 0x90, 0x00,
            0x00, 0x03, 0x00, 0x00, 0x03, 0x00, 0x1e, 0xba, 0x02, 0x40,
        ];
        const SPS: &[u8] = &[
            0x42, 0x01, 0x01, 0x03, 0x70, 0x00, 0x00, 0x03, 0x00, 0x90, 0x00, 0x00, 0x03, 0x00,
            0x00, 0x03, 0x00, 0x1e, 0xa0, 0x88, 0x45, 0x96, 0xe9, 0x7c, 0x2e, 0x01, 0x00, 0x00,
            0x03, 0x03, 0xe8, 0x00, 0x00, 0x03, 0x03, 0xe8, 0x08,
        ];
        const PPS: &[u8] = &[0x44, 0x01, 0xc0, 0x71, 0x81, 0xa4, 0x80];
        const IDR: &[u8] = &[0x28, 0x01, 0xac, 0x4c, 0xed, 0xdb, 0xaf, 0xfc, 0x42, 0x40];

        let decoder_config = hevc_test_config(&[(32, VPS), (33, SPS), (34, PPS)]);
        let mut packet = Vec::with_capacity(IDR.len() + 4);
        packet.extend_from_slice(&(IDR.len() as u32).to_be_bytes());
        packet.extend_from_slice(IDR);
        let scale = TimeScale {
            units_per_second: 24,
        };
        let input = VideoDecodeInput {
            codec: VideoCodec::Hevc,
            time_scale: scale,
            decoder_config: Some(&decoder_config),
            packets: vec![CompressedVideoPacket {
                index: 0,
                pts: TimePoint { units: 0, scale },
                dts: TimePoint { units: 0, scale },
                duration: TimeDelta { units: 1, scale },
                keyframe: true,
                bytes: &packet,
            }],
            end_of_stream: true,
        };
        let format = RawVideoFormat {
            width: 16,
            height: 16,
            frame_rate_num: 24,
            frame_rate_den: 1,
            pixel_format: RawVideoPixelFormat::Bgra,
        };

        let decoded = decode_hevc_cpu_bgra_frames(&input, format).expect("decode HEVC fixture");

        assert_eq!(decoded.stream.decoder, "chroma-cpu-hevc-decoder");
        assert_eq!(decoded.frames.len(), 1);
        assert_eq!(decoded.frames[0].pixels.len(), 16 * 16 * 4);
        assert!(
            decoded.frames[0]
                .pixels
                .chunks_exact(4)
                .all(|pixel| pixel[3] == 255)
        );
    }

    #[test]
    fn cpu_hevc_converts_main10_planes_to_bgra() {
        let format = RawVideoFormat {
            width: 2,
            height: 2,
            frame_rate_num: 24,
            frame_rate_den: 1,
            pixel_format: RawVideoPixelFormat::Bgra,
        };
        let scale = TimeScale {
            units_per_second: 24,
        };
        let frame = rust_h265::Frame {
            y: rust_h265::PixelData::U16(vec![256; 4]),
            u: rust_h265::PixelData::U16(vec![512]),
            v: rust_h265::PixelData::U16(vec![512]),
            width: 2,
            height: 2,
            pic_order_cnt: 0,
            bit_depth: 10,
        };
        let converted = copy_hevc_bgra_frame(
            frame,
            format,
            CpuDecodeTiming {
                pts: TimePoint { units: 0, scale },
                dts: TimePoint { units: 0, scale },
                duration: TimeDelta { units: 1, scale },
                keyframe: true,
            },
        )
        .expect("convert Main10 frame");

        assert_eq!(converted.pixels.len(), 16);
        assert!(
            converted
                .pixels
                .chunks_exact(4)
                .all(|pixel| { pixel[0] == pixel[1] && pixel[1] == pixel[2] && pixel[3] == 255 })
        );
    }

    fn hevc_test_config(arrays: &[(u8, &[u8])]) -> Vec<u8> {
        let mut config = vec![
            1,
            1,
            0,
            0,
            0,
            0,
            0,
            0,
            0,
            0,
            0,
            0,
            120,
            0,
            0,
            0,
            0,
            0,
            0,
            0,
            0,
            3,
            arrays.len() as u8,
        ];
        for (nal_type, unit) in arrays {
            config.push(0x80 | nal_type);
            config.extend_from_slice(&1_u16.to_be_bytes());
            config.extend_from_slice(&(unit.len() as u16).to_be_bytes());
            config.extend_from_slice(unit);
        }
        config
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
