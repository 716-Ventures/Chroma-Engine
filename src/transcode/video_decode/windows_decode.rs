#![allow(unsafe_code)]

use mediaway_common::{
    Bytes, CodecKind, GpuDeviceHandle, NativeHandle, Packet, PixelFormat, Rational,
};
use mediaway_decoder::{
    HardwareSurfaceFormat, HardwareVideoFrame, VideoDecoder, VideoDecoderConfig,
    VideoOutputPreference, windows::WindowsVideoDecoder,
};
use windows::{
    Win32::{
        Foundation::HMODULE,
        Graphics::{
            Direct3D::D3D_DRIVER_TYPE_HARDWARE,
            Direct3D11::{
                D3D11_CPU_ACCESS_READ, D3D11_CREATE_DEVICE_BGRA_SUPPORT,
                D3D11_CREATE_DEVICE_VIDEO_SUPPORT, D3D11_MAP_READ, D3D11_MAPPED_SUBRESOURCE,
                D3D11_SDK_VERSION, D3D11_TEXTURE2D_DESC, D3D11_USAGE_STAGING, D3D11CreateDevice,
                ID3D11Device, ID3D11DeviceContext, ID3D11Texture2D,
            },
            Dxgi::Common::{DXGI_FORMAT_NV12, DXGI_FORMAT_P010},
        },
    },
    core::Interface,
};

use super::{
    CompressedVideoPacket, DecodedVideoFrame, DecodedVideoOutput, DecodedVideoStream,
    RawVideoFormat, VideoCodec, VideoDecodeError, VideoDecodeInput, validate_decoded_video_format,
};

const MAX_FRAME_BYTES: usize = 512 * 1024 * 1024;

/// Retained Windows Media Foundation/D3D11 hardware H.264 decoder.
///
/// Media Foundation emits NV12 D3D11 textures. Chroma copies each completed texture to a
/// CPU-readable staging resource and converts the padded NV12 planes to its portable BGRA
/// boundary.
pub struct WindowsH264BgraDecoderSession {
    output_format: RawVideoFormat,
    codec: VideoCodec,
    hevc_nalu_length_size: Option<u8>,
    surface_format: HardwareSurfaceFormat,
    device: ID3D11Device,
    context: ID3D11DeviceContext,
    decoder: WindowsVideoDecoder,
    pending: Vec<PendingTiming>,
    next_token: i64,
    decoded_batches: u64,
}

/// Retained Windows Media Foundation/D3D11 hardware HEVC Main/Main10 decoder.
pub struct WindowsHevcBgraDecoderSession {
    inner: WindowsH264BgraDecoderSession,
}

#[derive(Debug, Clone, Copy)]
struct PendingTiming {
    token: i64,
    pts: crate::packet::TimePoint,
    dts: crate::packet::TimePoint,
    duration: crate::packet::TimeDelta,
    keyframe: bool,
}

impl std::fmt::Debug for WindowsH264BgraDecoderSession {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("WindowsH264BgraDecoderSession")
            .field("output_format", &self.output_format)
            .field("surface_format", &self.surface_format)
            .field("session_created", &true)
            .field("pending_frames", &self.pending.len())
            .field("decoded_batches", &self.decoded_batches)
            .finish_non_exhaustive()
    }
}

impl WindowsH264BgraDecoderSession {
    /// Creates the D3D11 device retained by the hardware decoder.
    pub fn new(
        output_format: RawVideoFormat,
        decoder_config: &[u8],
    ) -> Result<Self, VideoDecodeError> {
        Self::new_for_codec(VideoCodec::H264, output_format, decoder_config)
    }

    fn new_for_codec(
        codec: VideoCodec,
        output_format: RawVideoFormat,
        decoder_config: &[u8],
    ) -> Result<Self, VideoDecodeError> {
        validate_decoded_video_format(output_format)?;
        let output_bytes = (output_format.width as usize)
            .checked_mul(output_format.height as usize)
            .and_then(|pixels| pixels.checked_mul(4))
            .ok_or_else(|| VideoDecodeError::InvalidOutputFormat {
                reason: "Windows decoded BGRA byte count overflowed".to_string(),
            })?;
        if output_bytes > MAX_FRAME_BYTES {
            return Err(VideoDecodeError::InvalidOutputFormat {
                reason: "Windows decoded frame exceeds the 512 MiB safety limit".to_string(),
            });
        }
        if decoder_config.is_empty() {
            return Err(VideoDecodeError::MissingDecoderConfig);
        }
        let (device, context) = create_video_device()?;
        let (decoder, hevc_nalu_length_size, surface_format) =
            open_decoder(&device, codec, output_format, decoder_config)?;
        Ok(Self {
            output_format,
            codec,
            hevc_nalu_length_size,
            surface_format,
            device,
            context,
            decoder,
            pending: Vec::new(),
            next_token: 1,
            decoded_batches: 0,
        })
    }

    /// Decodes one ordered H.264 packet batch without recreating the native session.
    pub fn decode(
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
        for packet in &input.packets {
            let token = self.next_token;
            self.next_token =
                self.next_token
                    .checked_add(1)
                    .ok_or_else(|| VideoDecodeError::BackendFailed {
                        reason: "Windows decoder timestamp token space was exhausted".to_string(),
                    })?;
            let mediaway_packet = to_mediaway_packet(
                packet,
                input.time_scale,
                token,
                self.codec,
                self.hevc_nalu_length_size,
            )?;
            self.decoder
                .push_packet(&mediaway_packet)
                .map_err(map_decoder_error("packet submission"))?;
            self.pending.push(PendingTiming {
                token,
                pts: packet.pts,
                dts: packet.dts,
                duration: packet.duration,
                keyframe: packet.keyframe,
            });
            self.receive_available(&mut frames)?;
        }
        if input.end_of_stream {
            self.decoder.flush().map_err(map_decoder_error("drain"))?;
            self.receive_available(&mut frames)?;
            if !self.pending.is_empty() {
                return Err(VideoDecodeError::BackendFailed {
                    reason: format!(
                        "Windows {:?} decoder drained with {} unmatched timing record(s)",
                        self.codec,
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
                decoder: match self.codec {
                    VideoCodec::H264 => "chroma-d3d11va-h264-decoder",
                    VideoCodec::Hevc => "chroma-d3d11va-hevc-decoder",
                    VideoCodec::Av1 => return Err(VideoDecodeError::UnsupportedCodec),
                }
                .to_string(),
            },
            frames,
        })
    }

    /// Returns the number of accepted packet batches.
    pub fn decoded_batches(&self) -> u64 {
        self.decoded_batches
    }

    fn receive_available(
        &mut self,
        output: &mut Vec<DecodedVideoFrame>,
    ) -> Result<(), VideoDecodeError> {
        while let Some(frame) = self
            .decoder
            .poll_hardware_frame()
            .map_err(map_decoder_error("frame receive"))?
        {
            output.push(self.copy_frame(frame)?);
        }
        Ok(())
    }

    fn copy_frame(
        &mut self,
        frame: HardwareVideoFrame,
    ) -> Result<DecodedVideoFrame, VideoDecodeError> {
        if frame.format != self.surface_format
            || frame.width != self.output_format.width
            || frame.height != self.output_format.height
        {
            return Err(VideoDecodeError::InvalidOutputFormat {
                reason: format!(
                    "Windows decoder returned unexpected {:?} {}x{} frame",
                    frame.format, frame.width, frame.height
                ),
            });
        }
        let timing = take_timing(&mut self.pending, frame.pts)?;
        let pixels = copy_texture_to_bgra(
            &self.device,
            &self.context,
            frame.texture,
            frame.subresource,
            frame.width,
            frame.height,
            frame.format,
        )?;
        Ok(DecodedVideoFrame {
            pts: timing.pts,
            dts: timing.dts,
            duration: timing.duration,
            format: self.output_format,
            pixels,
            keyframe: timing.keyframe,
        })
    }
}

impl std::fmt::Debug for WindowsHevcBgraDecoderSession {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("WindowsHevcBgraDecoderSession")
            .field("inner", &self.inner)
            .finish()
    }
}

impl WindowsHevcBgraDecoderSession {
    /// Creates a D3D11-backed hardware HEVC Main/Main10 decoder.
    pub fn new(
        output_format: RawVideoFormat,
        decoder_config: &[u8],
    ) -> Result<Self, VideoDecodeError> {
        hevc_surface_format(decoder_config)?;
        Ok(Self {
            inner: WindowsH264BgraDecoderSession::new_for_codec(
                VideoCodec::Hevc,
                output_format,
                decoder_config,
            )?,
        })
    }

    /// Decodes one ordered HEVC packet batch without recreating the native session.
    pub fn decode(
        &mut self,
        input: &VideoDecodeInput<'_>,
    ) -> Result<DecodedVideoOutput, VideoDecodeError> {
        self.inner.decode(input)
    }

    /// Returns the number of accepted packet batches.
    pub fn decoded_batches(&self) -> u64 {
        self.inner.decoded_batches()
    }
}

fn create_video_device() -> Result<(ID3D11Device, ID3D11DeviceContext), VideoDecodeError> {
    let mut device = None;
    let mut context = None;
    // SAFETY: output pointers reference initialized Option slots and remain valid for the call.
    unsafe {
        D3D11CreateDevice(
            None,
            D3D_DRIVER_TYPE_HARDWARE,
            HMODULE::default(),
            D3D11_CREATE_DEVICE_VIDEO_SUPPORT | D3D11_CREATE_DEVICE_BGRA_SUPPORT,
            None,
            D3D11_SDK_VERSION,
            Some(&raw mut device),
            None,
            Some(&raw mut context),
        )
    }
    .map_err(|error| VideoDecodeError::BackendUnavailable {
        reason: format!("D3D11 video device creation failed: {error}"),
    })?;
    match (device, context) {
        (Some(device), Some(context)) => Ok((device, context)),
        _ => Err(VideoDecodeError::BackendUnavailable {
            reason: "D3D11 video device creation returned no device context".to_string(),
        }),
    }
}

fn open_decoder(
    device: &ID3D11Device,
    codec: VideoCodec,
    format: RawVideoFormat,
    decoder_config: &[u8],
) -> Result<(WindowsVideoDecoder, Option<u8>, HardwareSurfaceFormat), VideoDecodeError> {
    let device_handle = NativeHandle::new(Interface::as_raw(device) as usize).ok_or_else(|| {
        VideoDecodeError::BackendUnavailable {
            reason: "D3D11 returned a null device handle".to_string(),
        }
    })?;
    let (codec_kind, extra_data, hevc_nalu_length_size, surface_format) = match codec {
        VideoCodec::H264 => (
            CodecKind::H264,
            decoder_config.to_vec(),
            None,
            HardwareSurfaceFormat::Nv12,
        ),
        VideoCodec::Hevc => {
            let config =
                crate::codec::hevc::parse_hevc_decoder_config(decoder_config).map_err(|error| {
                    VideoDecodeError::InvalidDecoderConfig {
                        reason: error.to_string(),
                    }
                })?;
            let parameter_sets = crate::codec::hevc::hevc_parameter_sets_to_annex_b(&config);
            if parameter_sets.is_empty() {
                return Err(VideoDecodeError::InvalidDecoderConfig {
                    reason: "HEVC decoder configuration does not contain VPS/SPS/PPS".to_string(),
                });
            }
            (
                CodecKind::Hevc,
                parameter_sets,
                Some(config.nalu_length_size),
                hevc_surface_format(decoder_config)?,
            )
        }
        VideoCodec::Av1 => return Err(VideoDecodeError::UnsupportedCodec),
    };
    let config = VideoDecoderConfig {
        codec: codec_kind,
        width: format.width,
        height: format.height,
        time_base: Rational::new(u64::from(format.frame_rate_den), format.frame_rate_num),
        pixel_format: PixelFormat::Nv12,
        output: VideoOutputPreference::ZeroCopyGpu,
        hardware_surface_format: surface_format,
        gpu_device: Some(GpuDeviceHandle::DirectX11(device_handle)),
        extra_data: Bytes::from(extra_data),
    };
    WindowsVideoDecoder::open(&config)
        .map(|decoder| (decoder, hevc_nalu_length_size, surface_format))
        .map_err(|error| VideoDecodeError::BackendUnavailable {
            reason: format!("Windows hardware {codec:?} decoder initialization failed: {error}"),
        })
}

fn hevc_surface_format(decoder_config: &[u8]) -> Result<HardwareSurfaceFormat, VideoDecodeError> {
    match crate::codec::pixel_format::pixel_format_from_hevc_decoder_config(decoder_config)
        .as_deref()
    {
        Some("yuv420-8bit") => Ok(HardwareSurfaceFormat::Nv12),
        Some("yuv420-10bit") => Ok(HardwareSurfaceFormat::P010),
        _ => Err(VideoDecodeError::BackendUnavailable {
            reason: "Windows D3D11 HEVC supports Main/Main10 YUV420 input".to_string(),
        }),
    }
}

fn to_mediaway_packet(
    packet: &CompressedVideoPacket<'_>,
    expected_scale: crate::packet::TimeScale,
    token: i64,
    codec: VideoCodec,
    hevc_nalu_length_size: Option<u8>,
) -> Result<Packet, VideoDecodeError> {
    if packet.pts.scale != expected_scale
        || packet.dts.scale != expected_scale
        || packet.duration.scale != expected_scale
    {
        return Err(VideoDecodeError::InvalidInput {
            reason: "packet timestamp scale does not match decode input".to_string(),
        });
    }
    let payload = match (codec, hevc_nalu_length_size) {
        (VideoCodec::Hevc, Some(nalu_length_size)) => {
            crate::codec::hevc::hevc_sample_to_annex_b(packet.bytes, nalu_length_size).map_err(
                |error| VideoDecodeError::InvalidInput {
                    reason: format!("invalid HEVC packet: {error}"),
                },
            )?
        }
        _ => packet.bytes.to_vec(),
    };
    Ok(Packet {
        stream_id: 0,
        pts: token,
        dts: token,
        duration: 1,
        is_keyframe: packet.keyframe,
        is_discard: false,
        payload: Bytes::from(payload),
    })
}

fn take_timing(
    pending: &mut Vec<PendingTiming>,
    pts: i64,
) -> Result<PendingTiming, VideoDecodeError> {
    let index = pending
        .iter()
        .position(|timing| timing.token == pts)
        .ok_or_else(|| VideoDecodeError::BackendFailed {
            reason: format!("Windows decoder returned unknown PTS {pts}"),
        })?;
    Ok(pending.remove(index))
}

fn copy_texture_to_bgra(
    device: &ID3D11Device,
    context: &ID3D11DeviceContext,
    texture: NativeHandle,
    subresource: u32,
    width: u32,
    height: u32,
    format: HardwareSurfaceFormat,
) -> Result<Vec<u8>, VideoDecodeError> {
    let raw = texture.get() as *mut std::ffi::c_void;
    // SAFETY: Mediaway guarantees the handle is a live ID3D11Texture2D until the next decoder
    // operation. This function completes its copy and mapping before returning.
    let source = unsafe { ID3D11Texture2D::from_raw_borrowed(&raw) }.ok_or_else(|| {
        VideoDecodeError::InvalidOutputFormat {
            reason: "Windows decoder returned a null D3D11 texture".to_string(),
        }
    })?;
    let owning_device =
        unsafe { source.GetDevice() }.map_err(|error| VideoDecodeError::BackendFailed {
            reason: format!("D3D11 source-device query failed: {error}"),
        })?;
    if Interface::as_raw(&owning_device) != Interface::as_raw(device) {
        return Err(VideoDecodeError::InvalidOutputFormat {
            reason: "Windows decoder returned a texture from a different D3D11 device".to_string(),
        });
    }

    let mut source_desc = D3D11_TEXTURE2D_DESC::default();
    // SAFETY: source is a live texture and the descriptor output pointer is valid.
    unsafe { source.GetDesc(&raw mut source_desc) };
    let dxgi_format = match format {
        HardwareSurfaceFormat::Nv12 => DXGI_FORMAT_NV12,
        HardwareSurfaceFormat::P010 => DXGI_FORMAT_P010,
    };
    if source_desc.Format != dxgi_format || source_desc.Width < width || source_desc.Height < height
    {
        return Err(VideoDecodeError::InvalidOutputFormat {
            reason: "Windows decoder returned an incompatible D3D11 texture".to_string(),
        });
    }
    if source_desc.SampleDesc.Count != 1 {
        return Err(VideoDecodeError::InvalidOutputFormat {
            reason: "Windows decoder returned a multisampled video texture".to_string(),
        });
    }
    let bytes_per_sample = match format {
        HardwareSurfaceFormat::Nv12 => 1_usize,
        HardwareSurfaceFormat::P010 => 2_usize,
    };
    let surface_bytes = (source_desc.Width as usize)
        .checked_mul(bytes_per_sample)
        .and_then(|row_bytes| row_bytes.checked_mul(source_desc.Height as usize))
        .and_then(|luma_bytes| luma_bytes.checked_add(luma_bytes.div_ceil(2)))
        .ok_or_else(|| VideoDecodeError::InvalidOutputFormat {
            reason: "D3D11 decoded texture byte count overflowed".to_string(),
        })?;
    if surface_bytes > MAX_FRAME_BYTES {
        return Err(VideoDecodeError::InvalidOutputFormat {
            reason: "D3D11 decoded texture exceeds the 512 MiB safety limit".to_string(),
        });
    }
    let staging_desc = D3D11_TEXTURE2D_DESC {
        Width: source_desc.Width,
        Height: source_desc.Height,
        MipLevels: 1,
        ArraySize: 1,
        Format: dxgi_format,
        SampleDesc: source_desc.SampleDesc,
        Usage: D3D11_USAGE_STAGING,
        BindFlags: 0,
        CPUAccessFlags: D3D11_CPU_ACCESS_READ.0 as u32,
        MiscFlags: 0,
    };
    let mut staging = None;
    // SAFETY: the descriptor is fully initialized and no initial data is supplied.
    unsafe { device.CreateTexture2D(&raw const staging_desc, None, Some(&raw mut staging)) }
        .map_err(|error| VideoDecodeError::BackendFailed {
            reason: format!("D3D11 staging texture creation failed: {error}"),
        })?;
    let staging = staging.ok_or_else(|| VideoDecodeError::BackendFailed {
        reason: "D3D11 returned no staging texture".to_string(),
    })?;

    // SAFETY: both resources belong to the same device; destination is a matching single-slice
    // NV12 staging texture and source subresource was supplied by the decoder.
    unsafe {
        context.CopySubresourceRegion(&staging, 0, 0, 0, 0, source, subresource, None);
    }
    let mut mapped = D3D11_MAPPED_SUBRESOURCE::default();
    // SAFETY: staging was created for CPU reads and mapped resource output is valid.
    unsafe { context.Map(&staging, 0, D3D11_MAP_READ, 0, Some(&raw mut mapped)) }.map_err(
        |error| VideoDecodeError::BackendFailed {
            reason: format!("D3D11 decoded texture readback failed: {error}"),
        },
    )?;

    let result = mapped_texture_to_bgra(
        &mapped,
        width as usize,
        height as usize,
        source_desc.Height as usize,
        format,
    );
    // SAFETY: staging subresource zero was successfully mapped immediately above.
    unsafe { context.Unmap(&staging, 0) };
    result
}

fn mapped_texture_to_bgra(
    mapped: &D3D11_MAPPED_SUBRESOURCE,
    width: usize,
    height: usize,
    surface_height: usize,
    format: HardwareSurfaceFormat,
) -> Result<Vec<u8>, VideoDecodeError> {
    let stride = mapped.RowPitch as usize;
    let surface_chroma_height = surface_height.div_ceil(2);
    let total_rows = surface_height
        .checked_add(surface_chroma_height)
        .ok_or_else(|| VideoDecodeError::InvalidOutputFormat {
            reason: "D3D11 video-surface row count overflowed".to_string(),
        })?;
    let mapped_len =
        stride
            .checked_mul(total_rows)
            .ok_or_else(|| VideoDecodeError::InvalidOutputFormat {
                reason: "D3D11 mapped video-surface byte count overflowed".to_string(),
            })?;
    let minimum_stride = match format {
        HardwareSurfaceFormat::Nv12 => width,
        HardwareSurfaceFormat::P010 => {
            width
                .checked_mul(2)
                .ok_or_else(|| VideoDecodeError::InvalidOutputFormat {
                    reason: "D3D11 P010 row byte count overflowed".to_string(),
                })?
        }
    };
    if mapped.pData.is_null() || stride < minimum_stride {
        return Err(VideoDecodeError::InvalidOutputFormat {
            reason: "D3D11 returned an invalid mapped video-surface layout".to_string(),
        });
    }
    // SAFETY: D3D11 Map exposes RowPitch bytes for every luma and chroma row until Unmap.
    let bytes = unsafe { std::slice::from_raw_parts(mapped.pData.cast::<u8>(), mapped_len) };
    let uv_start = stride.checked_mul(surface_height).ok_or_else(|| {
        VideoDecodeError::InvalidOutputFormat {
            reason: "D3D11 video-surface chroma offset overflowed".to_string(),
        }
    })?;
    match format {
        HardwareSurfaceFormat::Nv12 => super::super::yuv::convert_nv12_to_bgra(
            &bytes[..uv_start],
            stride,
            &bytes[uv_start..],
            stride,
            width,
            height,
        ),
        HardwareSurfaceFormat::P010 => super::super::yuv::convert_p010_to_bgra(
            &bytes[..uv_start],
            stride,
            &bytes[uv_start..],
            stride,
            width,
            height,
        ),
    }
}

fn map_decoder_error(
    operation: &'static str,
) -> impl FnOnce(mediaway_decoder::DecodeError) -> VideoDecodeError {
    move |error| VideoDecodeError::BackendFailed {
        reason: format!("Windows hardware decoder {operation} failed: {error}"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn mediaway_packet_uses_unique_internal_timestamp_token() {
        let scale = crate::packet::TimeScale {
            units_per_second: 90_000,
        };
        let packet = CompressedVideoPacket {
            index: 4,
            pts: crate::packet::TimePoint { units: 9000, scale },
            dts: crate::packet::TimePoint { units: 6000, scale },
            duration: crate::packet::TimeDelta { units: 3000, scale },
            keyframe: true,
            bytes: &[1, 2, 3],
        };
        let converted = to_mediaway_packet(&packet, scale, 27, VideoCodec::H264, None).unwrap();
        assert_eq!(converted.pts, 27);
        assert_eq!(converted.dts, 27);
        assert_eq!(converted.duration, 1);
        assert_eq!(converted.payload.as_ref(), [1, 2, 3]);
    }

    #[test]
    fn output_timestamp_token_restores_source_timing() {
        let scale = crate::packet::TimeScale {
            units_per_second: 24,
        };
        let timing = PendingTiming {
            token: 9,
            pts: crate::packet::TimePoint { units: 2, scale },
            dts: crate::packet::TimePoint { units: 1, scale },
            duration: crate::packet::TimeDelta { units: 1, scale },
            keyframe: false,
        };
        assert_eq!(take_timing(&mut vec![timing], 9).unwrap().pts.units, 2);
    }

    #[test]
    fn hevc_main10_selects_p010_surface() {
        let mut config = [0_u8; 23];
        config[0] = 1;
        config[16] = 0xfd; // YUV420 (chroma_format_idc = 1).
        config[17] = 0xfa; // bit_depth_luma_minus8 = 2.
        config[18] = 0xfa; // bit_depth_chroma_minus8 = 2.

        assert_eq!(
            hevc_surface_format(&config).unwrap(),
            HardwareSurfaceFormat::P010
        );
    }
}
