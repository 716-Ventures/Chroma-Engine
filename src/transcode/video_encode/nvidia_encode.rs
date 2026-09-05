use std::collections::HashMap;

use oxideav_core::{
    CodecId, CodecParameters, Error as OxideError, Frame, PixelFormat, Rational, VideoFrame,
    VideoPlane,
};

use super::{
    EncodedVideoFrame, EncodedVideoOutput, EncodedVideoStream, RawVideoFormat, RawVideoFrameRef,
    VideoCodec, VideoEncodeError, build_avc_decoder_config, validate_raw_video_format,
    validate_raw_video_frames,
};

#[derive(Debug, Clone, Copy)]
struct PendingTiming {
    pts: crate::packet::TimePoint,
    dts: crate::packet::TimePoint,
    duration: crate::packet::TimeDelta,
}

/// Retained Linux NVIDIA NVENC H.264 encoder accepting Chroma's BGRA frame boundary.
pub struct NvencH264EncoderSession {
    format: RawVideoFormat,
    encoder: Box<dyn oxideav_core::Encoder>,
    pending: HashMap<i64, PendingTiming>,
    next_token: i64,
    decoder_config: Option<Vec<u8>>,
    encoded_batches: u64,
}

impl std::fmt::Debug for NvencH264EncoderSession {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("NvencH264EncoderSession")
            .field("format", &self.format)
            .field("pending_frames", &self.pending.len())
            .field("encoded_batches", &self.encoded_batches)
            .finish_non_exhaustive()
    }
}

impl NvencH264EncoderSession {
    /// Opens an NVENC H.264 High-profile encoder on the first usable CUDA device.
    pub fn new(format: RawVideoFormat, bitrate: u32) -> Result<Self, VideoEncodeError> {
        validate_raw_video_format(format)?;
        if !format.width.is_multiple_of(2) || !format.height.is_multiple_of(2) {
            return Err(VideoEncodeError::InvalidInput {
                reason: "NVENC H.264 requires even width and height".to_string(),
            });
        }
        if bitrate == 0 {
            return Err(VideoEncodeError::InvalidInput {
                reason: "bitrate must be greater than zero".to_string(),
            });
        }
        let mut parameters = CodecParameters::video(CodecId::new("h264"));
        parameters.width = Some(format.width);
        parameters.height = Some(format.height);
        parameters.pixel_format = Some(PixelFormat::Yuv420P);
        parameters.frame_rate = Some(Rational::new(
            i64::from(format.frame_rate_num),
            i64::from(format.frame_rate_den),
        ));
        parameters.bit_rate = Some(u64::from(bitrate));
        let encoder = oxideav_nvidia::H264NvEncoder::make(&parameters).map_err(map_init_error)?;
        Ok(Self {
            format,
            encoder,
            pending: HashMap::new(),
            next_token: 1,
            decoder_config: None,
            encoded_batches: 0,
        })
    }

    /// Encodes one ordered BGRA batch without recreating the CUDA/NVENC session.
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
                        reason: "NVENC timestamp token space was exhausted".to_string(),
                    })?;
            self.pending.insert(
                token,
                PendingTiming {
                    pts: frame.pts,
                    dts: frame.dts,
                    duration: frame.duration,
                },
            );
            let video_frame = Frame::Video(bgra_to_i420(self.format, frame.bytes, token)?);
            self.encoder
                .send_frame(&video_frame)
                .map_err(map_runtime_error("frame submission"))?;
            self.receive_available(&mut output)?;
        }
        if output.is_empty() {
            return Err(VideoEncodeError::BackendFailed {
                reason: "NVENC buffered the entire input batch without emitting output".to_string(),
            });
        }
        let decoder_config =
            self.decoder_config
                .clone()
                .ok_or_else(|| VideoEncodeError::BackendFailed {
                    reason: "NVENC emitted no SPS/PPS decoder configuration".to_string(),
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

    fn receive_available(
        &mut self,
        output: &mut Vec<EncodedVideoFrame>,
    ) -> Result<(), VideoEncodeError> {
        loop {
            let packet = match self.encoder.receive_packet() {
                Ok(packet) => packet,
                Err(OxideError::NeedMore | OxideError::Eof) => return Ok(()),
                Err(error) => return Err(map_runtime_error("output receive")(error)),
            };
            let token = packet.pts.ok_or_else(|| VideoEncodeError::BackendFailed {
                reason: "NVENC returned a packet without its timestamp token".to_string(),
            })?;
            let timing =
                self.pending
                    .remove(&token)
                    .ok_or_else(|| VideoEncodeError::BackendFailed {
                        reason: format!("NVENC returned unknown timestamp token {token}"),
                    })?;
            let access_unit = annex_b_h264_to_avcc(&packet.data)?;
            if !access_unit.parameter_sets.is_empty() {
                self.decoder_config = build_avc_decoder_config(&access_unit.parameter_sets, 4);
            }
            output.push(EncodedVideoFrame {
                pts: timing.pts,
                dts: timing.dts,
                duration: timing.duration,
                payload: access_unit.payload,
                keyframe: access_unit.keyframe || packet.flags.keyframe,
            });
        }
    }

    /// Returns the number of frame batches accepted by this session.
    pub fn encoded_batches(&self) -> u64 {
        self.encoded_batches
    }
}

fn map_init_error(error: OxideError) -> VideoEncodeError {
    match error {
        OxideError::Unsupported(reason) => VideoEncodeError::BackendUnavailable { reason },
        other => VideoEncodeError::BackendFailed {
            reason: format!("NVENC initialization failed: {other}"),
        },
    }
}

fn map_runtime_error(operation: &'static str) -> impl FnOnce(OxideError) -> VideoEncodeError {
    move |error| VideoEncodeError::BackendFailed {
        reason: format!("NVENC {operation} failed: {error}"),
    }
}

fn bgra_to_i420(
    format: RawVideoFormat,
    bgra: &[u8],
    token: i64,
) -> Result<VideoFrame, VideoEncodeError> {
    let width = format.width as usize;
    let height = format.height as usize;
    let chroma_width = width / 2;
    let chroma_height = height / 2;
    let mut y = vec![0_u8; width * height];
    let mut u = vec![0_u8; chroma_width * chroma_height];
    let mut v = vec![0_u8; chroma_width * chroma_height];

    for row in 0..height {
        for column in 0..width {
            let pixel = (row * width + column) * 4;
            let b = i32::from(bgra[pixel]);
            let g = i32::from(bgra[pixel + 1]);
            let r = i32::from(bgra[pixel + 2]);
            y[row * width + column] =
                clamp_video_sample(((66 * r + 129 * g + 25 * b + 128) >> 8) + 16);
        }
    }
    for row in 0..chroma_height {
        for column in 0..chroma_width {
            let mut r_sum = 0_i32;
            let mut g_sum = 0_i32;
            let mut b_sum = 0_i32;
            for y_offset in 0..2 {
                for x_offset in 0..2 {
                    let pixel = (((row * 2 + y_offset) * width) + column * 2 + x_offset) * 4;
                    b_sum += i32::from(bgra[pixel]);
                    g_sum += i32::from(bgra[pixel + 1]);
                    r_sum += i32::from(bgra[pixel + 2]);
                }
            }
            let r = (r_sum + 2) / 4;
            let g = (g_sum + 2) / 4;
            let b = (b_sum + 2) / 4;
            let index = row * chroma_width + column;
            u[index] = clamp_video_sample(((-38 * r - 74 * g + 112 * b + 128) >> 8) + 128);
            v[index] = clamp_video_sample(((112 * r - 94 * g - 18 * b + 128) >> 8) + 128);
        }
    }
    Ok(VideoFrame {
        pts: Some(token),
        planes: vec![
            VideoPlane {
                stride: width,
                data: y,
            },
            VideoPlane {
                stride: chroma_width,
                data: u,
            },
            VideoPlane {
                stride: chroma_width,
                data: v,
            },
        ],
    })
}

fn clamp_video_sample(value: i32) -> u8 {
    value.clamp(0, 255) as u8
}

struct AvccAccessUnit {
    payload: Vec<u8>,
    parameter_sets: Vec<Vec<u8>>,
    keyframe: bool,
}

fn annex_b_h264_to_avcc(bytes: &[u8]) -> Result<AvccAccessUnit, VideoEncodeError> {
    let nals = annex_b_nals(bytes);
    if nals.is_empty() {
        return Err(VideoEncodeError::BackendFailed {
            reason: "NVENC returned no Annex-B NAL units".to_string(),
        });
    }
    let mut payload = Vec::new();
    let mut parameter_sets = Vec::new();
    let mut keyframe = false;
    for nal in nals {
        match nal[0] & 0x1f {
            7 | 8 => parameter_sets.push(nal.to_vec()),
            9 => {}
            nal_type => {
                keyframe |= nal_type == 5;
                let size =
                    u32::try_from(nal.len()).map_err(|_| VideoEncodeError::BackendFailed {
                        reason: "NVENC NAL unit exceeds the AVCC size limit".to_string(),
                    })?;
                payload.extend_from_slice(&size.to_be_bytes());
                payload.extend_from_slice(nal);
            }
        }
    }
    if payload.is_empty() {
        return Err(VideoEncodeError::BackendFailed {
            reason: "NVENC returned only parameter-set NAL units".to_string(),
        });
    }
    Ok(AvccAccessUnit {
        payload,
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

    #[test]
    fn converts_annex_b_to_avcc_and_extracts_parameter_sets() {
        let unit = annex_b_h264_to_avcc(&[
            0, 0, 0, 1, 0x67, 100, 0, 31, 0, 0, 1, 0x68, 1, 2, 0, 0, 1, 0x65, 3, 4,
        ])
        .expect("convert NVENC output");

        assert_eq!(unit.parameter_sets.len(), 2);
        assert!(unit.keyframe);
        assert_eq!(unit.payload, [0, 0, 0, 3, 0x65, 3, 4]);
    }

    #[test]
    fn converts_bgra_black_to_limited_range_i420() {
        let format = RawVideoFormat {
            width: 2,
            height: 2,
            frame_rate_num: 24,
            frame_rate_den: 1,
            pixel_format: super::super::RawVideoPixelFormat::Bgra,
        };
        let frame = bgra_to_i420(format, &[0; 16], 7).expect("convert BGRA");

        assert_eq!(frame.pts, Some(7));
        assert_eq!(frame.planes[0].data, [16; 4]);
        assert_eq!(frame.planes[1].data, [128]);
        assert_eq!(frame.planes[2].data, [128]);
    }
}
