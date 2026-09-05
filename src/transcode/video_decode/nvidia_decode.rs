use std::collections::HashMap;

use oxideav_core::{
    CodecId, CodecParameters, Error as OxideError, Frame, Packet, PixelFormat, TimeBase, VideoFrame,
};

use super::{
    CompressedVideoPacket, CpuDecodeTiming, DecodedVideoFrame, DecodedVideoOutput,
    DecodedVideoStream, RawVideoFormat, VideoCodec, VideoDecodeError, VideoDecodeInput,
    validate_decoded_video_format, validate_packet_time_scale,
};

/// Retained Linux NVIDIA NVDEC H.264 decoder producing BGRA frames.
pub struct NvdecH264BgraDecoderSession {
    core: NvdecDecoderCore,
}

/// Retained Linux NVIDIA NVDEC HEVC decoder producing BGRA frames.
pub struct NvdecHevcBgraDecoderSession {
    core: NvdecDecoderCore,
}

struct NvdecDecoderCore {
    output_format: RawVideoFormat,
    codec: VideoCodec,
    decoder_name: &'static str,
    ten_bit: bool,
    decoder: Box<dyn oxideav_core::Decoder>,
    nalu_length_size: u8,
    parameter_sets: Option<Vec<u8>>,
    pending: HashMap<i64, CpuDecodeTiming>,
    next_token: i64,
    decoded_batches: u64,
}

impl std::fmt::Debug for NvdecH264BgraDecoderSession {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        debug_session("NvdecH264BgraDecoderSession", &self.core, formatter)
    }
}

impl std::fmt::Debug for NvdecHevcBgraDecoderSession {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        debug_session("NvdecHevcBgraDecoderSession", &self.core, formatter)
    }
}

fn debug_session(
    name: &str,
    core: &NvdecDecoderCore,
    formatter: &mut std::fmt::Formatter<'_>,
) -> std::fmt::Result {
    formatter
        .debug_struct(name)
        .field("output_format", &core.output_format)
        .field("pending_frames", &core.pending.len())
        .field("decoded_batches", &core.decoded_batches)
        .finish_non_exhaustive()
}

impl NvdecH264BgraDecoderSession {
    /// Opens an NVDEC H.264 decoder on the first usable CUDA device.
    pub fn new(
        output_format: RawVideoFormat,
        decoder_config: &[u8],
    ) -> Result<Self, VideoDecodeError> {
        validate_decoded_video_format(output_format)?;
        let config =
            crate::codec::h264::parse_avc_decoder_config(decoder_config).map_err(|error| {
                VideoDecodeError::InvalidDecoderConfig {
                    reason: error.to_string(),
                }
            })?;
        if crate::codec::pixel_format::pixel_format_from_avc_decoder_config(decoder_config)
            .as_deref()
            != Some("yuv420-8bit")
        {
            return Err(VideoDecodeError::BackendUnavailable {
                reason: "NVDEC H.264 currently supports 8-bit YUV420 input".to_string(),
            });
        }
        if config.sps.is_empty() || config.pps.is_empty() {
            return Err(VideoDecodeError::InvalidDecoderConfig {
                reason: "AVC decoder configuration does not contain SPS/PPS".to_string(),
            });
        }
        let mut parameter_sets = Vec::new();
        for set in config.sps.iter().chain(config.pps.iter()) {
            parameter_sets.extend_from_slice(&[0, 0, 0, 1]);
            parameter_sets.extend_from_slice(set);
        }
        Ok(Self {
            core: NvdecDecoderCore::new(
                output_format,
                VideoCodec::H264,
                "chroma-nvdec-h264-decoder",
                config.nalu_length_size,
                parameter_sets,
                false,
            )?,
        })
    }

    /// Decodes one ordered compressed packet batch without recreating NVDEC state.
    pub fn decode(
        &mut self,
        input: &VideoDecodeInput<'_>,
    ) -> Result<DecodedVideoOutput, VideoDecodeError> {
        self.core.decode(input)
    }

    /// Returns the number of accepted packet batches.
    pub fn decoded_batches(&self) -> u64 {
        self.core.decoded_batches
    }
}

impl NvdecHevcBgraDecoderSession {
    /// Opens an NVDEC HEVC decoder on the first usable CUDA device.
    pub fn new(
        output_format: RawVideoFormat,
        decoder_config: &[u8],
    ) -> Result<Self, VideoDecodeError> {
        validate_decoded_video_format(output_format)?;
        let config =
            crate::codec::hevc::parse_hevc_decoder_config(decoder_config).map_err(|error| {
                VideoDecodeError::InvalidDecoderConfig {
                    reason: error.to_string(),
                }
            })?;
        let pixel_format =
            crate::codec::pixel_format::pixel_format_from_hevc_decoder_config(decoder_config);
        let ten_bit = match pixel_format.as_deref() {
            Some("yuv420-8bit") => false,
            Some("yuv420-10bit") => true,
            _ => {
                return Err(VideoDecodeError::BackendUnavailable {
                    reason: "NVDEC HEVC supports 8-bit or 10-bit YUV420 input".to_string(),
                });
            }
        };
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
        Ok(Self {
            core: NvdecDecoderCore::new(
                output_format,
                VideoCodec::Hevc,
                "chroma-nvdec-hevc-decoder",
                config.nalu_length_size,
                parameter_sets,
                ten_bit,
            )?,
        })
    }

    /// Decodes one ordered compressed packet batch without recreating NVDEC state.
    pub fn decode(
        &mut self,
        input: &VideoDecodeInput<'_>,
    ) -> Result<DecodedVideoOutput, VideoDecodeError> {
        self.core.decode(input)
    }

    /// Returns the number of accepted packet batches.
    pub fn decoded_batches(&self) -> u64 {
        self.core.decoded_batches
    }
}

impl NvdecDecoderCore {
    fn new(
        output_format: RawVideoFormat,
        codec: VideoCodec,
        decoder_name: &'static str,
        nalu_length_size: u8,
        parameter_sets: Vec<u8>,
        ten_bit: bool,
    ) -> Result<Self, VideoDecodeError> {
        let mut parameters = CodecParameters::video(CodecId::new(match codec {
            VideoCodec::H264 => "h264",
            VideoCodec::Hevc => "hevc",
            VideoCodec::Av1 => return Err(VideoDecodeError::UnsupportedCodec),
        }));
        parameters.width = Some(output_format.width);
        parameters.height = Some(output_format.height);
        parameters.pixel_format = Some(if ten_bit {
            PixelFormat::Yuv420P10Le
        } else {
            PixelFormat::Yuv420P
        });
        let decoder = match codec {
            VideoCodec::H264 => oxideav_nvidia::H264NvDecoder::make(&parameters),
            VideoCodec::Hevc => oxideav_nvidia::HevcNvDecoder::make(&parameters),
            VideoCodec::Av1 => return Err(VideoDecodeError::UnsupportedCodec),
        }
        .map_err(map_init_error)?;
        Ok(Self {
            output_format,
            codec,
            decoder_name,
            ten_bit,
            decoder,
            nalu_length_size,
            parameter_sets: Some(parameter_sets),
            pending: HashMap::new(),
            next_token: 1,
            decoded_batches: 0,
        })
    }

    fn decode(
        &mut self,
        input: &VideoDecodeInput<'_>,
    ) -> Result<DecodedVideoOutput, VideoDecodeError> {
        if input.codec != self.codec {
            return Err(VideoDecodeError::UnsupportedCodec);
        }
        if input.time_scale.units_per_second == 0 {
            return Err(VideoDecodeError::InvalidInput {
                reason: "packet time scale must be greater than zero".to_string(),
            });
        }
        let mut frames = Vec::new();
        for compressed in &input.packets {
            validate_packet_time_scale(compressed, input.time_scale)?;
            let token = self.next_token;
            self.next_token =
                self.next_token
                    .checked_add(1)
                    .ok_or_else(|| VideoDecodeError::BackendFailed {
                        reason: "NVDEC timestamp token space was exhausted".to_string(),
                    })?;
            let mut annex_b = self.parameter_sets.take().unwrap_or_default();
            annex_b.extend_from_slice(&self.packet_to_annex_b(compressed)?);
            self.pending.insert(token, packet_timing(compressed));
            let packet = Packet::new(
                0,
                TimeBase::from_rate(input.time_scale.units_per_second),
                annex_b,
            )
            .with_pts(token)
            .with_dts(token)
            .with_keyframe(compressed.keyframe);
            self.decoder
                .send_packet(&packet)
                .map_err(map_runtime_error("packet submission"))?;
            self.receive_available(&mut frames)?;
        }
        if input.end_of_stream {
            self.decoder.flush().map_err(map_runtime_error("drain"))?;
            self.receive_available(&mut frames)?;
            if !self.pending.is_empty() {
                return Err(VideoDecodeError::BackendFailed {
                    reason: format!(
                        "NVDEC drained with {} packet timing record(s) unmatched",
                        self.pending.len()
                    ),
                });
            }
        }
        frames.sort_by_key(|frame| (frame.pts.units, frame.pts.scale.units_per_second));
        self.decoded_batches = self.decoded_batches.saturating_add(1);
        Ok(DecodedVideoOutput {
            stream: DecodedVideoStream {
                format: self.output_format,
                source_codec: self.codec,
                decoder: self.decoder_name.to_string(),
            },
            frames,
        })
    }

    fn packet_to_annex_b(
        &self,
        packet: &CompressedVideoPacket<'_>,
    ) -> Result<Vec<u8>, VideoDecodeError> {
        match self.codec {
            VideoCodec::H264 => {
                crate::codec::h264::avc_sample_to_annex_b(packet.bytes, self.nalu_length_size)
                    .map_err(|error| VideoDecodeError::InvalidInput {
                        reason: format!("invalid AVCC packet: {error}"),
                    })
            }
            VideoCodec::Hevc => {
                crate::codec::hevc::hevc_sample_to_annex_b(packet.bytes, self.nalu_length_size)
                    .map_err(|error| VideoDecodeError::InvalidInput {
                        reason: format!("invalid HEVC packet: {error}"),
                    })
            }
            VideoCodec::Av1 => Err(VideoDecodeError::UnsupportedCodec),
        }
    }

    fn receive_available(
        &mut self,
        frames: &mut Vec<DecodedVideoFrame>,
    ) -> Result<(), VideoDecodeError> {
        loop {
            let decoded = match self.decoder.receive_frame() {
                Ok(Frame::Video(frame)) => frame,
                Ok(_) => {
                    return Err(VideoDecodeError::BackendFailed {
                        reason: "NVDEC returned a non-video frame".to_string(),
                    });
                }
                Err(OxideError::NeedMore | OxideError::Eof) => return Ok(()),
                Err(error) => return Err(map_runtime_error("frame receive")(error)),
            };
            let token = decoded.pts.ok_or_else(|| VideoDecodeError::BackendFailed {
                reason: "NVDEC returned a frame without its timestamp token".to_string(),
            })?;
            let timing =
                self.pending
                    .remove(&token)
                    .ok_or_else(|| VideoDecodeError::BackendFailed {
                        reason: format!("NVDEC returned unknown timestamp token {token}"),
                    })?;
            frames.push(copy_planar_420_to_bgra(
                decoded,
                self.output_format,
                timing,
                self.ten_bit,
            )?);
        }
    }
}

fn packet_timing(packet: &CompressedVideoPacket<'_>) -> CpuDecodeTiming {
    CpuDecodeTiming {
        pts: packet.pts,
        dts: packet.dts,
        duration: packet.duration,
        keyframe: packet.keyframe,
    }
}

fn copy_planar_420_to_bgra(
    frame: VideoFrame,
    format: RawVideoFormat,
    timing: CpuDecodeTiming,
    ten_bit: bool,
) -> Result<DecodedVideoFrame, VideoDecodeError> {
    if frame.planes.len() < 3 {
        return Err(VideoDecodeError::BackendFailed {
            reason: format!(
                "NVDEC returned {} planes, expected I420",
                frame.planes.len()
            ),
        });
    }
    let width = format.width as usize;
    let height = format.height as usize;
    let chroma_width = width.div_ceil(2);
    let chroma_height = height.div_ceil(2);
    let y = &frame.planes[0];
    let u = &frame.planes[1];
    let v = &frame.planes[2];
    let bytes_per_sample = if ten_bit { 2 } else { 1 };
    validate_plane(
        "Y",
        y.stride,
        width * bytes_per_sample,
        height,
        y.data.len(),
    )?;
    validate_plane(
        "U",
        u.stride,
        chroma_width * bytes_per_sample,
        chroma_height,
        u.data.len(),
    )?;
    validate_plane(
        "V",
        v.stride,
        chroma_width * bytes_per_sample,
        chroma_height,
        v.data.len(),
    )?;
    let mut pixels = vec![0_u8; width * height * 4];
    for row in 0..height {
        for column in 0..width {
            let y_offset = row * y.stride + column * bytes_per_sample;
            let u_offset = (row / 2) * u.stride + (column / 2) * bytes_per_sample;
            let v_offset = (row / 2) * v.stride + (column / 2) * bytes_per_sample;
            let sample = |plane: &[u8], offset: usize| {
                if ten_bit {
                    (u16::from_le_bytes([plane[offset], plane[offset + 1]]) >> 8) as u8
                } else {
                    plane[offset]
                }
            };
            let components = super::limited_yuv_to_bgra(
                sample(&y.data, y_offset),
                sample(&u.data, u_offset),
                sample(&v.data, v_offset),
            );
            let offset = (row * width + column) * 4;
            pixels[offset..offset + 4].copy_from_slice(&components);
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

fn validate_plane(
    name: &str,
    stride: usize,
    row_bytes: usize,
    rows: usize,
    available: usize,
) -> Result<(), VideoDecodeError> {
    let required = stride
        .checked_mul(rows.saturating_sub(1))
        .and_then(|offset| offset.checked_add(row_bytes))
        .ok_or_else(|| VideoDecodeError::BackendFailed {
            reason: format!("NVDEC {name} plane geometry overflowed"),
        })?;
    if stride < row_bytes || available < required {
        return Err(VideoDecodeError::BackendFailed {
            reason: format!(
                "NVDEC {name} plane is too short: stride {stride}, row bytes {row_bytes}, rows {rows}, available {available}"
            ),
        });
    }
    Ok(())
}

fn map_init_error(error: OxideError) -> VideoDecodeError {
    match error {
        OxideError::Unsupported(reason) => VideoDecodeError::BackendUnavailable { reason },
        other => VideoDecodeError::BackendFailed {
            reason: format!("NVDEC initialization failed: {other}"),
        },
    }
}

fn map_runtime_error(operation: &'static str) -> impl FnOnce(OxideError) -> VideoDecodeError {
    move |error| VideoDecodeError::BackendFailed {
        reason: format!("NVDEC {operation} failed: {error}"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use oxideav_core::VideoPlane;

    #[test]
    fn converts_i420_black_to_bgra() {
        let format = RawVideoFormat {
            width: 2,
            height: 2,
            frame_rate_num: 24,
            frame_rate_den: 1,
            pixel_format: super::super::RawVideoPixelFormat::Bgra,
        };
        let frame = VideoFrame {
            pts: Some(1),
            planes: vec![
                VideoPlane {
                    stride: 2,
                    data: vec![16; 4],
                },
                VideoPlane {
                    stride: 1,
                    data: vec![128],
                },
                VideoPlane {
                    stride: 1,
                    data: vec![128],
                },
            ],
        };
        let scale = crate::packet::TimeScale {
            units_per_second: 24,
        };
        let decoded = copy_planar_420_to_bgra(
            frame,
            format,
            CpuDecodeTiming {
                pts: crate::packet::TimePoint { units: 0, scale },
                dts: crate::packet::TimePoint { units: 0, scale },
                duration: crate::packet::TimeDelta { units: 1, scale },
                keyframe: true,
            },
            false,
        )
        .expect("convert I420");

        assert_eq!(decoded.pixels, [0, 0, 0, 255].repeat(4));
    }

    #[test]
    fn rejects_short_i420_plane() {
        let error = validate_plane("Y", 2, 2, 2, 3).expect_err("short plane");
        assert!(error.to_string().contains("too short"));
    }

    #[test]
    fn converts_planar_p016_black_to_bgra() {
        let format = RawVideoFormat {
            width: 2,
            height: 2,
            frame_rate_num: 24,
            frame_rate_den: 1,
            pixel_format: super::super::RawVideoPixelFormat::Bgra,
        };
        let frame = VideoFrame {
            pts: Some(1),
            planes: vec![
                VideoPlane {
                    stride: 4,
                    data: [0x00, 0x10].repeat(4),
                },
                VideoPlane {
                    stride: 2,
                    data: vec![0x00, 0x80],
                },
                VideoPlane {
                    stride: 2,
                    data: vec![0x00, 0x80],
                },
            ],
        };
        let scale = crate::packet::TimeScale {
            units_per_second: 24,
        };
        let decoded = copy_planar_420_to_bgra(
            frame,
            format,
            CpuDecodeTiming {
                pts: crate::packet::TimePoint { units: 0, scale },
                dts: crate::packet::TimePoint { units: 0, scale },
                duration: crate::packet::TimeDelta { units: 1, scale },
                keyframe: true,
            },
            true,
        )
        .expect("convert P016");
        assert_eq!(decoded.pixels, [0, 0, 0, 255].repeat(4));
    }
}
