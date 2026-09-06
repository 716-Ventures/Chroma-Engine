use std::collections::HashMap;

use mediaway_common::{Bytes, CodecKind, PixelFormat, Rational, VideoFrame, VideoFrameStorage};
use mediaway_encoder::{
    VideoEncoder,
    auto::{AutoVideoEncodeConfig, Backend, BackendSelection, EncodePathClass},
    windows::auto::AutoVideoEncoder,
};

use super::{
    EncodedVideoFrame, EncodedVideoOutput, EncodedVideoStream, RawVideoFormat, RawVideoFrameRef,
    VideoCodec, VideoEncodeError, build_avc_decoder_config, build_hevc_decoder_config,
    validate_raw_video_format, validate_raw_video_frames,
};

#[derive(Debug, Clone, Copy)]
struct PendingTiming {
    pts: crate::packet::TimePoint,
    dts: crate::packet::TimePoint,
    duration: crate::packet::TimeDelta,
}

/// Retained Windows native H.264 encoder using NVENC, Quick Sync, or Media Foundation.
pub struct WindowsH264EncoderSession {
    core: WindowsEncoderCore,
}

/// Retained Windows native HEVC Main encoder using NVENC, Quick Sync, or Media Foundation.
pub struct WindowsHevcEncoderSession {
    core: WindowsEncoderCore,
}

struct WindowsEncoderCore {
    format: RawVideoFormat,
    codec: VideoCodec,
    encoder: AutoVideoEncoder,
    backend_name: &'static str,
    pending: HashMap<i64, PendingTiming>,
    next_token: i64,
    decoder_config: Option<Vec<u8>>,
    encoded_batches: u64,
}

impl std::fmt::Debug for WindowsH264EncoderSession {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        debug_session("WindowsH264EncoderSession", &self.core, formatter)
    }
}

impl std::fmt::Debug for WindowsHevcEncoderSession {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        debug_session("WindowsHevcEncoderSession", &self.core, formatter)
    }
}

fn debug_session(
    name: &str,
    core: &WindowsEncoderCore,
    formatter: &mut std::fmt::Formatter<'_>,
) -> std::fmt::Result {
    formatter
        .debug_struct(name)
        .field("format", &core.format)
        .field("backend", &core.backend_name)
        .field("pending_frames", &core.pending.len())
        .field("encoded_batches", &core.encoded_batches)
        .finish_non_exhaustive()
}

impl WindowsH264EncoderSession {
    /// Opens the first usable native Windows H.264 encoder.
    pub fn new(format: RawVideoFormat, bitrate: u32) -> Result<Self, VideoEncodeError> {
        Ok(Self {
            core: WindowsEncoderCore::new(format, bitrate, VideoCodec::H264)?,
        })
    }

    /// Encodes one ordered BGRA frame batch without recreating the native session.
    pub fn encode(
        &mut self,
        frames: &[RawVideoFrameRef<'_>],
    ) -> Result<EncodedVideoOutput, VideoEncodeError> {
        self.core.encode(frames)
    }

    /// Returns the number of accepted frame batches.
    pub fn encoded_batches(&self) -> u64 {
        self.core.encoded_batches
    }

    /// Returns the selected native Windows backend identifier.
    pub fn backend_name(&self) -> &'static str {
        self.core.backend_name
    }
}

impl WindowsHevcEncoderSession {
    /// Opens the first usable native Windows HEVC Main encoder.
    pub fn new(format: RawVideoFormat, bitrate: u32) -> Result<Self, VideoEncodeError> {
        Ok(Self {
            core: WindowsEncoderCore::new(format, bitrate, VideoCodec::Hevc)?,
        })
    }

    /// Encodes one ordered BGRA frame batch without recreating the native session.
    pub fn encode(
        &mut self,
        frames: &[RawVideoFrameRef<'_>],
    ) -> Result<EncodedVideoOutput, VideoEncodeError> {
        self.core.encode(frames)
    }

    /// Returns the number of accepted frame batches.
    pub fn encoded_batches(&self) -> u64 {
        self.core.encoded_batches
    }

    /// Returns the selected native Windows backend identifier.
    pub fn backend_name(&self) -> &'static str {
        self.core.backend_name
    }
}

impl WindowsEncoderCore {
    fn new(
        format: RawVideoFormat,
        bitrate: u32,
        codec: VideoCodec,
    ) -> Result<Self, VideoEncodeError> {
        validate_raw_video_format(format)?;
        if !format.width.is_multiple_of(2) || !format.height.is_multiple_of(2) {
            return Err(VideoEncodeError::InvalidInput {
                reason: "Windows native encoding requires even width and height".to_string(),
            });
        }
        if bitrate == 0 {
            return Err(VideoEncodeError::InvalidInput {
                reason: "bitrate must be greater than zero".to_string(),
            });
        }
        let codec_kind = match codec {
            VideoCodec::H264 => CodecKind::H264,
            VideoCodec::Hevc => CodecKind::Hevc,
            VideoCodec::Av1 => {
                return Err(VideoEncodeError::BackendUnavailable {
                    reason: "Windows AV1 encode is not implemented".to_string(),
                });
            }
        };
        let mut config = AutoVideoEncodeConfig::new(
            codec_kind,
            format.width,
            format.height,
            Rational::new(u64::from(format.frame_rate_den), format.frame_rate_num),
        );
        config.bitrate_bps = bitrate;
        config.pixel_format = PixelFormat::Nv12;
        config.max_path_class = EncodePathClass::CpuUpload;
        config.gop_size = 1;

        let mut selected = None;
        for backend in [Backend::Nvenc, Backend::QuickSync, Backend::Os] {
            config.backend = BackendSelection::Explicit(backend);
            if let Ok(encoder) = AutoVideoEncoder::open(&config) {
                selected = Some((encoder, backend_name(backend, codec)));
                break;
            }
        }
        let (encoder, backend_name) = selected.ok_or_else(|| {
            VideoEncodeError::BackendUnavailable {
                reason: format!(
                    "Windows exposed no usable NVENC, Quick Sync, or Media Foundation {codec:?} encoder"
                ),
            }
        })?;
        Ok(Self {
            format,
            codec,
            encoder,
            backend_name,
            pending: HashMap::new(),
            next_token: 1,
            decoder_config: None,
            encoded_batches: 0,
        })
    }

    fn encode(
        &mut self,
        frames: &[RawVideoFrameRef<'_>],
    ) -> Result<EncodedVideoOutput, VideoEncodeError> {
        validate_raw_video_frames(self.format, frames)?;
        let mut output = Vec::with_capacity(frames.len());
        for frame in frames {
            let token = self.next_token;
            self.next_token =
                self.next_token
                    .checked_add(1)
                    .ok_or_else(|| VideoEncodeError::BackendFailed {
                        reason: "Windows encoder timestamp token space was exhausted".to_string(),
                    })?;
            self.pending.insert(
                token,
                PendingTiming {
                    pts: frame.pts,
                    dts: frame.dts,
                    duration: frame.duration,
                },
            );
            let input = VideoFrame {
                pts: token,
                duration: 1,
                width: self.format.width,
                height: self.format.height,
                format: PixelFormat::Nv12,
                storage: VideoFrameStorage::Cpu {
                    data: Bytes::from(bgra_to_nv12(self.format, frame.bytes)?),
                },
            };
            self.encoder
                .push_frame(&input)
                .map_err(map_runtime_error("frame submission"))?;
            self.receive_available(&mut output)?;
        }
        if output.is_empty() {
            return Err(VideoEncodeError::BackendFailed {
                reason: "Windows encoder buffered the entire input batch without output"
                    .to_string(),
            });
        }
        self.refresh_decoder_config();
        let decoder_config =
            self.decoder_config
                .clone()
                .ok_or_else(|| VideoEncodeError::BackendFailed {
                    reason: "Windows encoder emitted no decoder configuration".to_string(),
                })?;
        self.encoded_batches = self.encoded_batches.saturating_add(1);
        Ok(EncodedVideoOutput {
            stream: EncodedVideoStream {
                codec: self.codec,
                width: self.format.width,
                height: self.format.height,
                time_scale: frames[0].pts.scale,
                decoder_config: Some(decoder_config),
            },
            frames: output,
        })
    }

    fn receive_available(
        &mut self,
        output: &mut Vec<EncodedVideoFrame>,
    ) -> Result<(), VideoEncodeError> {
        while let Some(packet) = self
            .encoder
            .poll_packet()
            .map_err(map_runtime_error("output receive"))?
        {
            let timing = self.pending.remove(&packet.pts).ok_or_else(|| {
                VideoEncodeError::BackendFailed {
                    reason: format!(
                        "Windows encoder returned unknown timestamp token {}",
                        packet.pts
                    ),
                }
            })?;
            let unit = convert_access_unit(self.codec, &packet.payload)?;
            if !unit.parameter_sets.is_empty() {
                self.decoder_config = match self.codec {
                    VideoCodec::H264 => build_avc_decoder_config(&unit.parameter_sets, 4),
                    VideoCodec::Hevc => build_hevc_decoder_config(&unit.parameter_sets, 4),
                    VideoCodec::Av1 => None,
                };
            }
            output.push(EncodedVideoFrame {
                pts: timing.pts,
                dts: timing.dts,
                duration: timing.duration,
                payload: unit.payload,
                keyframe: packet.is_keyframe || unit.keyframe,
            });
        }
        Ok(())
    }

    fn refresh_decoder_config(&mut self) {
        if self.decoder_config.is_some() {
            return;
        }
        let extra = self.encoder.stream_info().extra_data();
        if extra.is_empty() {
            return;
        }
        if extra.first() == Some(&1) {
            self.decoder_config = Some(extra.to_vec());
        } else if let Ok(unit) = convert_access_unit(self.codec, extra) {
            self.decoder_config = match self.codec {
                VideoCodec::H264 => build_avc_decoder_config(&unit.parameter_sets, 4),
                VideoCodec::Hevc => build_hevc_decoder_config(&unit.parameter_sets, 4),
                VideoCodec::Av1 => None,
            };
        }
    }
}

fn backend_name(backend: Backend, codec: VideoCodec) -> &'static str {
    match (backend, codec) {
        (Backend::Nvenc, VideoCodec::H264) => "chroma-windows-nvenc-h264",
        (Backend::Nvenc, VideoCodec::Hevc) => "chroma-windows-nvenc-hevc",
        (Backend::QuickSync, VideoCodec::H264) => "chroma-windows-qsv-h264",
        (Backend::QuickSync, VideoCodec::Hevc) => "chroma-windows-qsv-hevc",
        (Backend::Os, VideoCodec::H264) => "chroma-windows-mf-h264",
        (Backend::Os, VideoCodec::Hevc) => "chroma-windows-mf-hevc",
        _ => "chroma-windows-video-encoder",
    }
}

fn map_runtime_error(
    operation: &'static str,
) -> impl FnOnce(mediaway_encoder::EncodeError) -> VideoEncodeError {
    move |error| VideoEncodeError::BackendFailed {
        reason: format!("Windows {operation} failed: {error}"),
    }
}

fn bgra_to_nv12(format: RawVideoFormat, bgra: &[u8]) -> Result<Vec<u8>, VideoEncodeError> {
    let width = format.width as usize;
    let height = format.height as usize;
    let mut output = vec![0_u8; width * height + width * height / 2];
    for row in 0..height {
        for column in 0..width {
            let source = (row * width + column) * 4;
            let b = i32::from(bgra[source]);
            let g = i32::from(bgra[source + 1]);
            let r = i32::from(bgra[source + 2]);
            output[row * width + column] = clamp(((66 * r + 129 * g + 25 * b + 128) >> 8) + 16);
        }
    }
    let uv_start = width * height;
    for row in 0..height / 2 {
        for column in 0..width / 2 {
            let mut r = 0_i32;
            let mut g = 0_i32;
            let mut b = 0_i32;
            for y_offset in 0..2 {
                for x_offset in 0..2 {
                    let source = (((row * 2 + y_offset) * width) + column * 2 + x_offset) * 4;
                    b += i32::from(bgra[source]);
                    g += i32::from(bgra[source + 1]);
                    r += i32::from(bgra[source + 2]);
                }
            }
            let r = (r + 2) / 4;
            let g = (g + 2) / 4;
            let b = (b + 2) / 4;
            let destination = uv_start + row * width + column * 2;
            output[destination] = clamp(((-38 * r - 74 * g + 112 * b + 128) >> 8) + 128);
            output[destination + 1] = clamp(((112 * r - 94 * g - 18 * b + 128) >> 8) + 128);
        }
    }
    Ok(output)
}

fn clamp(value: i32) -> u8 {
    value.clamp(0, 255) as u8
}

struct AccessUnit {
    payload: Vec<u8>,
    parameter_sets: Vec<Vec<u8>>,
    keyframe: bool,
}

fn convert_access_unit(codec: VideoCodec, bytes: &[u8]) -> Result<AccessUnit, VideoEncodeError> {
    let nals = annex_b_nals(bytes);
    if nals.is_empty() {
        return Ok(AccessUnit {
            payload: bytes.to_vec(),
            parameter_sets: Vec::new(),
            keyframe: false,
        });
    }
    let mut output = Vec::new();
    let mut parameter_sets = Vec::new();
    let mut keyframe = false;
    for nal in nals {
        let nal_type = match codec {
            VideoCodec::H264 => nal[0] & 0x1f,
            VideoCodec::Hevc => (nal[0] >> 1) & 0x3f,
            VideoCodec::Av1 => {
                return Err(VideoEncodeError::BackendUnavailable {
                    reason: "Windows AV1 encode is not implemented".to_string(),
                });
            }
        };
        let is_parameter_set = match codec {
            VideoCodec::H264 => matches!(nal_type, 7 | 8),
            VideoCodec::Hevc => matches!(nal_type, 32..=34),
            VideoCodec::Av1 => false,
        };
        if is_parameter_set {
            parameter_sets.push(nal.to_vec());
            continue;
        }
        if matches!(
            (codec, nal_type),
            (VideoCodec::H264, 9) | (VideoCodec::Hevc, 35)
        ) {
            continue;
        }
        keyframe |= match codec {
            VideoCodec::H264 => nal_type == 5,
            VideoCodec::Hevc => (16..=21).contains(&nal_type),
            VideoCodec::Av1 => false,
        };
        let length = u32::try_from(nal.len()).map_err(|_| VideoEncodeError::BackendFailed {
            reason: "Windows encoder NAL unit exceeds the length-prefix limit".to_string(),
        })?;
        output.extend_from_slice(&length.to_be_bytes());
        output.extend_from_slice(nal);
    }
    if output.is_empty() {
        return Err(VideoEncodeError::BackendFailed {
            reason: "Windows encoder emitted only parameter-set NAL units".to_string(),
        });
    }
    Ok(AccessUnit {
        payload: output,
        parameter_sets,
        keyframe,
    })
}

fn annex_b_nals(mut bytes: &[u8]) -> Vec<&[u8]> {
    let mut nals = Vec::new();
    while let Some((start, prefix)) = find_start_code(bytes) {
        bytes = &bytes[start + prefix..];
        let end = find_start_code(bytes).map_or(bytes.len(), |(offset, _)| offset);
        if end > 0 {
            nals.push(&bytes[..end]);
        }
        bytes = &bytes[end..];
    }
    nals
}

fn find_start_code(bytes: &[u8]) -> Option<(usize, usize)> {
    bytes.windows(3).enumerate().find_map(|(index, window)| {
        if window == [0, 0, 1] {
            Some((index, 3))
        } else if window == [0, 0, 0] && bytes.get(index + 3) == Some(&1) {
            Some((index, 4))
        } else {
            None
        }
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn format_2x2() -> RawVideoFormat {
        RawVideoFormat {
            width: 2,
            height: 2,
            frame_rate_num: 24,
            frame_rate_den: 1,
            pixel_format: super::super::RawVideoPixelFormat::Bgra,
        }
    }

    #[test]
    fn converts_black_bgra_to_limited_range_nv12() {
        let bgra = [0, 0, 0, 255, 0, 0, 0, 255, 0, 0, 0, 255, 0, 0, 0, 255];
        assert_eq!(
            bgra_to_nv12(format_2x2(), &bgra).unwrap(),
            [16, 16, 16, 16, 128, 128]
        );
    }

    #[test]
    fn converts_h264_annex_b_and_extracts_parameter_sets() {
        let bytes = [
            0, 0, 0, 1, 0x67, 0x64, 0, 0x1f, 0, 0, 1, 0x68, 0xee, 0x3c, 0x80, 0, 0, 0, 1, 0x65,
            0x88, 0x84,
        ];
        let unit = convert_access_unit(VideoCodec::H264, &bytes).unwrap();
        assert_eq!(
            unit.parameter_sets,
            [vec![0x67, 0x64, 0, 0x1f], vec![0x68, 0xee, 0x3c, 0x80]]
        );
        assert_eq!(unit.payload, [0, 0, 0, 3, 0x65, 0x88, 0x84]);
        assert!(unit.keyframe);
    }

    #[test]
    fn converts_hevc_annex_b_and_omits_aud() {
        let bytes = [
            0, 0, 1, 0x40, 1, 0xaa, 0, 0, 1, 0x42, 1, 0xbb, 0, 0, 1, 0x44, 1, 0xcc, 0, 0, 1, 0x46,
            1, 0xdd, 0, 0, 1, 0x26, 1, 0xee,
        ];
        let unit = convert_access_unit(VideoCodec::Hevc, &bytes).unwrap();
        assert_eq!(
            unit.parameter_sets,
            [
                vec![0x40, 1, 0xaa],
                vec![0x42, 1, 0xbb],
                vec![0x44, 1, 0xcc]
            ]
        );
        assert_eq!(unit.payload, [0, 0, 0, 3, 0x26, 1, 0xee]);
        assert!(unit.keyframe);
    }
}
