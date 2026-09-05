use std::sync::Arc;

use nuxodecs::{
    BlockingMode, DecodedFormat, Fourcc, Resolution,
    backend::vaapi::encoder::VaapiBackend,
    codec::h264::parser::{Level, Profile},
    encoder::h264::EncoderConfig,
    encoder::stateless::h264::StatelessEncoder,
    encoder::{FrameMetadata, PredictionStructure, RateControl, Tunings, VideoEncoder},
    libva::{Display, Surface},
};

use crate::transcode::video_decode::vaapi_decode::{VaapiCpuFrame, VaapiUserPtrDescriptor};

use super::{
    EncodedVideoFrame, EncodedVideoOutput, EncodedVideoStream, RawVideoFormat, RawVideoFrameRef,
    VideoCodec, VideoEncodeError, build_avc_decoder_config, validate_raw_video_format,
    validate_raw_video_frames,
};

type Encoder = StatelessEncoder<
    VaapiCpuFrame,
    VaapiBackend<VaapiUserPtrDescriptor, Surface<VaapiUserPtrDescriptor>>,
>;

/// Retained Linux VA-API H.264 encoder accepting Chroma's BGRA frame boundary.
pub struct VaapiH264EncoderSession {
    format: RawVideoFormat,
    encoded_batches: u64,
    next_token: u64,
    decoder_config: Option<Vec<u8>>,
    encoder: Encoder,
}

impl std::fmt::Debug for VaapiH264EncoderSession {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("VaapiH264EncoderSession")
            .field("format", &self.format)
            .field("encoded_batches", &self.encoded_batches)
            .finish_non_exhaustive()
    }
}

impl VaapiH264EncoderSession {
    /// Opens a VA-API H.264 Main-profile encoder on the first usable DRM render node.
    pub fn new(format: RawVideoFormat, bitrate: u32) -> Result<Self, VideoEncodeError> {
        validate_raw_video_format(format)?;
        if !format.width.is_multiple_of(2) || !format.height.is_multiple_of(2) {
            return Err(VideoEncodeError::InvalidInput {
                reason: "VA-API H.264 requires even width and height".to_string(),
            });
        }
        if bitrate == 0 {
            return Err(VideoEncodeError::InvalidInput {
                reason: "bitrate must be greater than zero".to_string(),
            });
        }
        let display = Display::open().ok_or_else(|| VideoEncodeError::BackendUnavailable {
            reason: "libva could not open a DRM render node".to_string(),
        })?;
        let resolution = Resolution::from((format.width, format.height));
        let frame_rate = format
            .frame_rate_num
            .checked_add(format.frame_rate_den / 2)
            .and_then(|value| value.checked_div(format.frame_rate_den))
            .unwrap_or(0)
            .max(1);
        let config = EncoderConfig {
            resolution,
            profile: Profile::Main,
            level: Level::L4,
            pred_structure: PredictionStructure::LowDelay { limit: 240 },
            initial_tunings: Tunings {
                rate_control: RateControl::ConstantBitrate(u64::from(bitrate)),
                framerate: frame_rate,
                ..Default::default()
            },
        };
        let encoder = Encoder::new_vaapi(
            Arc::clone(&display),
            config,
            Fourcc::from(b"NV12"),
            resolution,
            false,
            BlockingMode::Blocking,
        )
        .map_err(|error| VideoEncodeError::BackendUnavailable {
            reason: format!("VA-API H.264 encoder initialization failed: {error}"),
        })?;
        Ok(Self {
            format,
            encoded_batches: 0,
            next_token: 1,
            decoder_config: None,
            encoder,
        })
    }

    /// Encodes one ordered BGRA frame batch without recreating the VA context.
    pub fn encode(
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
                        reason: "VA-API encoder timestamp token space was exhausted".to_string(),
                    })?;
            let mut surface = VaapiCpuFrame::new(
                Resolution::from((self.format.width, self.format.height)),
                DecodedFormat::NV12,
            )
            .map_err(|reason| VideoEncodeError::BackendFailed { reason })?;
            surface
                .copy_bgra_to_nv12(frame.bytes)
                .map_err(|reason| VideoEncodeError::BackendFailed { reason })?;
            let metadata = FrameMetadata {
                timestamp: token,
                layout: surface.frame_layout(),
                force_keyframe: frame.keyframe,
                force_idr: frame.keyframe,
            };
            self.encoder
                .encode(metadata, surface)
                .map_err(encode_error("frame submission"))?;
            let coded = self
                .encoder
                .poll()
                .map_err(encode_error("output polling"))?
                .ok_or_else(|| VideoEncodeError::BackendFailed {
                    reason: "VA-API encoder produced no output in blocking mode".to_string(),
                })?;
            if coded.metadata.timestamp != token {
                return Err(VideoEncodeError::BackendFailed {
                    reason: format!(
                        "VA-API encoder returned timestamp token {}, expected {token}",
                        coded.metadata.timestamp
                    ),
                });
            }
            let access_unit = annex_b_to_avcc(&coded.bitstream)?;
            if !access_unit.parameter_sets.is_empty() {
                self.decoder_config = build_avc_decoder_config(&access_unit.parameter_sets, 4);
            }
            output.push(EncodedVideoFrame {
                pts: frame.pts,
                dts: frame.dts,
                duration: frame.duration,
                payload: access_unit.payload,
                keyframe: access_unit.keyframe,
            });
        }
        let decoder_config =
            self.decoder_config
                .clone()
                .ok_or_else(|| VideoEncodeError::BackendFailed {
                    reason: "VA-API encoder emitted no SPS/PPS decoder configuration".to_string(),
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
            frames: output,
        })
    }

    /// Returns the number of frame batches encoded by this session.
    pub fn encoded_batches(&self) -> u64 {
        self.encoded_batches
    }
}

fn encode_error(
    operation: &'static str,
) -> impl FnOnce(nuxodecs::encoder::EncodeError) -> VideoEncodeError {
    move |error| VideoEncodeError::BackendFailed {
        reason: format!("VA-API {operation} failed: {error}"),
    }
}

struct AvccAccessUnit {
    payload: Vec<u8>,
    parameter_sets: Vec<Vec<u8>>,
    keyframe: bool,
}

fn annex_b_to_avcc(bytes: &[u8]) -> Result<AvccAccessUnit, VideoEncodeError> {
    let nal_units = annex_b_nal_units(bytes);
    if nal_units.is_empty() {
        return Err(VideoEncodeError::BackendFailed {
            reason: "VA-API encoder returned no Annex-B NAL units".to_string(),
        });
    }
    let mut payload = Vec::with_capacity(bytes.len());
    let mut parameter_sets = Vec::new();
    let mut keyframe = false;
    for nal in nal_units {
        match nal[0] & 0x1f {
            7 | 8 => parameter_sets.push(nal.to_vec()),
            9 => {}
            nal_type => {
                keyframe |= nal_type == 5;
                let size =
                    u32::try_from(nal.len()).map_err(|_| VideoEncodeError::BackendFailed {
                        reason: "VA-API NAL unit exceeds the AVCC size limit".to_string(),
                    })?;
                payload.extend_from_slice(&size.to_be_bytes());
                payload.extend_from_slice(nal);
            }
        }
    }
    if payload.is_empty() {
        return Err(VideoEncodeError::BackendFailed {
            reason: "VA-API encoder returned parameter sets without a coded slice".to_string(),
        });
    }
    Ok(AvccAccessUnit {
        payload,
        parameter_sets,
        keyframe,
    })
}

fn annex_b_nal_units(mut bytes: &[u8]) -> Vec<&[u8]> {
    let mut units = Vec::new();
    while let Some(start) = find_start_code(bytes, 0) {
        let nal_start = start.0 + start.1;
        let next = find_start_code(bytes, nal_start).map_or(bytes.len(), |value| value.0);
        if nal_start < next {
            units.push(&bytes[nal_start..next]);
        }
        bytes = &bytes[next..];
    }
    units
}

fn find_start_code(bytes: &[u8], from: usize) -> Option<(usize, usize)> {
    for index in from..bytes.len().saturating_sub(2) {
        if bytes[index..].starts_with(&[0, 0, 1]) {
            return Some((index, 3));
        }
        if bytes[index..].starts_with(&[0, 0, 0, 1]) {
            return Some((index, 4));
        }
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn converts_annex_b_output_to_avcc_and_extracts_parameter_sets() {
        let access_unit = annex_b_to_avcc(&[
            0, 0, 0, 1, 0x67, 1, 2, 3, 0, 0, 1, 0x68, 4, 0, 0, 0, 1, 0x65, 5, 6,
        ])
        .unwrap();
        assert_eq!(access_unit.parameter_sets.len(), 2);
        assert!(access_unit.keyframe);
        assert_eq!(access_unit.payload, [0, 0, 0, 3, 0x65, 5, 6]);
    }
}
