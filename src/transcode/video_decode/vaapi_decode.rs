#![allow(unsafe_code)]

use std::{
    alloc::{Layout, alloc_zeroed, dealloc},
    collections::HashMap,
    ptr::NonNull,
    sync::Arc,
    time::Duration,
};

use nuxodecs::{
    BlockingMode, DecodedFormat, Fourcc, Resolution,
    backend::vaapi::decoder::VaapiBackend,
    decoder::stateless::{
        DecodeError, DynStatelessVideoDecoder, StatelessDecoder, StatelessVideoDecoder, h264::H264,
        h265::H265,
    },
    decoder::{DecodedHandle, DecoderEvent},
    libva::{
        Display, ExternalBufferDescriptor, MemoryType, Surface, UsageHint,
        VASurfaceAttribExternalBuffers,
    },
    video_frame::{ReadMapping, VideoFrame, WriteMapping},
};

use super::{
    CompressedVideoPacket, CpuDecodeTiming, DecodedVideoFrame, DecodedVideoOutput,
    DecodedVideoStream, RawVideoFormat, VideoCodec, VideoDecodeError, VideoDecodeInput,
    validate_decoded_video_format, validate_packet_time_scale,
};

const FRAME_ALIGNMENT: usize = 4096;
const MAX_FRAME_BYTES: usize = 512 * 1024 * 1024;

/// Retained Linux VA-API H.264 decoder backed by directly readable NV12/P010 memory.
pub struct VaapiH264BgraDecoderSession {
    core: VaapiDecoderCore,
}

/// Retained Linux VA-API HEVC decoder backed by directly readable NV12/P010 memory.
pub struct VaapiHevcBgraDecoderSession {
    core: VaapiDecoderCore,
}

/// Retained Intel Quick Sync H.264 decoder exposed through Intel's VA-API driver.
pub struct QsvH264BgraDecoderSession {
    inner: VaapiH264BgraDecoderSession,
}

/// Retained Intel Quick Sync HEVC decoder exposed through Intel's VA-API driver.
pub struct QsvHevcBgraDecoderSession {
    inner: VaapiHevcBgraDecoderSession,
}

struct VaapiDecoderCore {
    output_format: RawVideoFormat,
    codec: VideoCodec,
    decoder_name: &'static str,
    decoded_batches: u64,
    nalu_length_size: u8,
    parameter_sets: Option<Vec<u8>>,
    next_token: u64,
    pending_timing: HashMap<u64, CpuDecodeTiming>,
    decoder: DynStatelessVideoDecoder<VaapiCpuFrame>,
}

impl std::fmt::Debug for VaapiH264BgraDecoderSession {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("VaapiH264BgraDecoderSession")
            .field("output_format", &self.core.output_format)
            .field("decoded_batches", &self.core.decoded_batches)
            .field("pending_frames", &self.core.pending_timing.len())
            .finish_non_exhaustive()
    }
}

impl VaapiH264BgraDecoderSession {
    /// Opens the first usable DRM render node and creates a VA-API H.264 decoder.
    pub fn new(
        output_format: RawVideoFormat,
        decoder_config: &[u8],
    ) -> Result<Self, VideoDecodeError> {
        Self::new_with_display(
            output_format,
            decoder_config,
            open_display()?,
            "chroma-vaapi-h264-decoder",
        )
    }

    fn new_with_display(
        output_format: RawVideoFormat,
        decoder_config: &[u8],
        display: Arc<Display>,
        decoder_name: &'static str,
    ) -> Result<Self, VideoDecodeError> {
        validate_decoded_video_format(output_format)?;
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
        let decoder = StatelessDecoder::<H264, VaapiBackend<VaapiCpuFrame>>::new_vaapi(
            display,
            BlockingMode::Blocking,
        )
        .map_err(|error| VideoDecodeError::BackendUnavailable {
            reason: format!("VA-API H.264 decoder initialization failed: {error}"),
        })?
        .into_trait_object();
        Ok(Self {
            core: VaapiDecoderCore {
                output_format,
                codec: VideoCodec::H264,
                decoder_name,
                decoded_batches: 0,
                nalu_length_size: config.nalu_length_size,
                parameter_sets: Some(parameter_sets),
                next_token: 1,
                pending_timing: HashMap::new(),
                decoder,
            },
        })
    }

    /// Decodes one ordered H.264 packet batch without recreating the VA context.
    pub fn decode(
        &mut self,
        input: &VideoDecodeInput<'_>,
    ) -> Result<DecodedVideoOutput, VideoDecodeError> {
        self.core.decode(input)
    }

    /// Returns the number of packet batches decoded by this session.
    pub fn decoded_batches(&self) -> u64 {
        self.core.decoded_batches
    }
}

impl std::fmt::Debug for VaapiHevcBgraDecoderSession {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("VaapiHevcBgraDecoderSession")
            .field("output_format", &self.core.output_format)
            .field("decoded_batches", &self.core.decoded_batches)
            .field("pending_frames", &self.core.pending_timing.len())
            .finish_non_exhaustive()
    }
}

impl VaapiHevcBgraDecoderSession {
    /// Opens the first usable DRM render node and creates a VA-API HEVC decoder.
    pub fn new(
        output_format: RawVideoFormat,
        decoder_config: &[u8],
    ) -> Result<Self, VideoDecodeError> {
        Self::new_with_display(
            output_format,
            decoder_config,
            open_display()?,
            "chroma-vaapi-hevc-decoder",
        )
    }

    fn new_with_display(
        output_format: RawVideoFormat,
        decoder_config: &[u8],
        display: Arc<Display>,
        decoder_name: &'static str,
    ) -> Result<Self, VideoDecodeError> {
        validate_decoded_video_format(output_format)?;
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
        let decoder = StatelessDecoder::<H265, VaapiBackend<VaapiCpuFrame>>::new_vaapi(
            display,
            BlockingMode::Blocking,
        )
        .map_err(|error| VideoDecodeError::BackendUnavailable {
            reason: format!("VA-API HEVC decoder initialization failed: {error}"),
        })?
        .into_trait_object();
        Ok(Self {
            core: VaapiDecoderCore {
                output_format,
                codec: VideoCodec::Hevc,
                decoder_name,
                decoded_batches: 0,
                nalu_length_size: config.nalu_length_size,
                parameter_sets: Some(parameter_sets),
                next_token: 1,
                pending_timing: HashMap::new(),
                decoder,
            },
        })
    }

    /// Decodes one ordered HEVC packet batch without recreating the VA context.
    pub fn decode(
        &mut self,
        input: &VideoDecodeInput<'_>,
    ) -> Result<DecodedVideoOutput, VideoDecodeError> {
        self.core.decode(input)
    }

    /// Returns the number of packet batches decoded by this session.
    pub fn decoded_batches(&self) -> u64 {
        self.core.decoded_batches
    }
}

impl std::fmt::Debug for QsvH264BgraDecoderSession {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("QsvH264BgraDecoderSession")
            .field("inner", &self.inner)
            .finish()
    }
}

impl QsvH264BgraDecoderSession {
    /// Opens an Intel VA-API render node and creates a Quick Sync H.264 decoder.
    pub fn new(
        output_format: RawVideoFormat,
        decoder_config: &[u8],
    ) -> Result<Self, VideoDecodeError> {
        Ok(Self {
            inner: VaapiH264BgraDecoderSession::new_with_display(
                output_format,
                decoder_config,
                open_intel_display()?,
                "chroma-qsv-h264-decoder",
            )?,
        })
    }

    /// Decodes one ordered H.264 packet batch without recreating the VA context.
    pub fn decode(
        &mut self,
        input: &VideoDecodeInput<'_>,
    ) -> Result<DecodedVideoOutput, VideoDecodeError> {
        self.inner.decode(input)
    }

    /// Returns the number of packet batches decoded by this session.
    pub fn decoded_batches(&self) -> u64 {
        self.inner.decoded_batches()
    }
}

impl std::fmt::Debug for QsvHevcBgraDecoderSession {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("QsvHevcBgraDecoderSession")
            .field("inner", &self.inner)
            .finish()
    }
}

impl QsvHevcBgraDecoderSession {
    /// Opens an Intel VA-API render node and creates a Quick Sync HEVC decoder.
    pub fn new(
        output_format: RawVideoFormat,
        decoder_config: &[u8],
    ) -> Result<Self, VideoDecodeError> {
        Ok(Self {
            inner: VaapiHevcBgraDecoderSession::new_with_display(
                output_format,
                decoder_config,
                open_intel_display()?,
                "chroma-qsv-hevc-decoder",
            )?,
        })
    }

    /// Decodes one ordered HEVC packet batch without recreating the VA context.
    pub fn decode(
        &mut self,
        input: &VideoDecodeInput<'_>,
    ) -> Result<DecodedVideoOutput, VideoDecodeError> {
        self.inner.decode(input)
    }

    /// Returns the number of packet batches decoded by this session.
    pub fn decoded_batches(&self) -> u64 {
        self.inner.decoded_batches()
    }
}

fn open_display() -> Result<Arc<Display>, VideoDecodeError> {
    Display::open().ok_or_else(|| VideoDecodeError::BackendUnavailable {
        reason: "libva could not open a DRM render node".to_string(),
    })
}

pub(in crate::transcode) fn open_intel_display() -> Result<Arc<Display>, VideoDecodeError> {
    for index in 128..192 {
        let path = format!("/dev/dri/renderD{index}");
        let Ok(display) = Display::open_drm_display(&path) else {
            continue;
        };
        let vendor = display.query_vendor_string().unwrap_or_default();
        let vendor_lower = vendor.to_ascii_lowercase();
        if vendor_lower.contains("intel")
            || vendor_lower.contains("ihd")
            || vendor_lower.contains("i965")
        {
            return Ok(display);
        }
    }
    Err(VideoDecodeError::BackendUnavailable {
        reason: "no Intel VA-API render node was available for Quick Sync".to_string(),
    })
}

impl VaapiDecoderCore {
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
        if let Some(parameter_sets) = self.parameter_sets.take() {
            self.submit_annex_b(0, &parameter_sets, &mut frames)?;
        }
        for packet in &input.packets {
            validate_packet_time_scale(packet, input.time_scale)?;
            let token = self.next_token;
            self.next_token =
                self.next_token
                    .checked_add(1)
                    .ok_or_else(|| VideoDecodeError::BackendFailed {
                        reason: "VA-API timestamp token space was exhausted".to_string(),
                    })?;
            let annex_b = match self.codec {
                VideoCodec::H264 => {
                    crate::codec::h264::avc_sample_to_annex_b(packet.bytes, self.nalu_length_size)
                        .map_err(|error| VideoDecodeError::InvalidInput {
                        reason: format!("invalid AVCC packet: {error}"),
                    })?
                }
                VideoCodec::Hevc => {
                    crate::codec::hevc::hevc_sample_to_annex_b(packet.bytes, self.nalu_length_size)
                        .map_err(|error| VideoDecodeError::InvalidInput {
                            reason: format!("invalid HEVC packet: {error}"),
                        })?
                }
                VideoCodec::Av1 => return Err(VideoDecodeError::UnsupportedCodec),
            };
            self.pending_timing.insert(token, packet_timing(packet));
            self.submit_annex_b(token, &annex_b, &mut frames)?;
        }
        if input.end_of_stream {
            self.decoder.flush().map_err(decode_error("drain"))?;
            self.collect_events(&mut frames)?;
            if !self.pending_timing.is_empty() {
                return Err(VideoDecodeError::BackendFailed {
                    reason: format!(
                        "VA-API drained with {} packet timing record(s) unmatched",
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
                source_codec: self.codec,
                decoder: self.decoder_name.to_string(),
            },
            frames,
        })
    }

    fn submit_annex_b(
        &mut self,
        token: u64,
        mut bytes: &[u8],
        frames: &mut Vec<DecodedVideoFrame>,
    ) -> Result<(), VideoDecodeError> {
        while !bytes.is_empty() {
            let allocation = self
                .decoder
                .stream_info()
                .map(|info| (info.coded_resolution, info.format));
            let mut allocate = || {
                allocation
                    .and_then(|(resolution, format)| VaapiCpuFrame::new(resolution, format).ok())
            };
            match self.decoder.decode(token, bytes, &mut allocate) {
                Ok(0) => {
                    return Err(VideoDecodeError::BackendFailed {
                        reason: "VA-API decoder accepted zero bytes".to_string(),
                    });
                }
                Ok(consumed) => bytes = &bytes[consumed..],
                Err(DecodeError::CheckEvents) | Err(DecodeError::NotEnoughOutputBuffers(_)) => {
                    self.decoder
                        .wait_for_next_event(Duration::from_secs(3))
                        .map_err(|error| VideoDecodeError::BackendFailed {
                            reason: format!("VA-API event wait failed: {error}"),
                        })?;
                    self.collect_events(frames)?;
                    continue;
                }
                Err(error) => return Err(decode_error("packet submission")(error)),
            }
            self.collect_events(frames)?;
        }
        Ok(())
    }

    fn collect_events(
        &mut self,
        frames: &mut Vec<DecodedVideoFrame>,
    ) -> Result<(), VideoDecodeError> {
        while let Some(event) = self.decoder.next_event() {
            match event {
                DecoderEvent::FormatChanged => {}
                DecoderEvent::FrameReady(handle) => {
                    handle
                        .sync()
                        .map_err(|error| VideoDecodeError::BackendFailed {
                            reason: format!("VA-API surface synchronization failed: {error}"),
                        })?;
                    let timing =
                        self.pending_timing
                            .remove(&handle.timestamp())
                            .ok_or_else(|| VideoDecodeError::BackendFailed {
                                reason: format!(
                                    "VA-API emitted timestamp token {} without packet timing",
                                    handle.timestamp()
                                ),
                            })?;
                    frames.push(copy_frame(&handle, self.output_format, timing)?);
                }
            }
        }
        Ok(())
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

fn decode_error(operation: &'static str) -> impl FnOnce(DecodeError) -> VideoDecodeError {
    move |error| VideoDecodeError::BackendFailed {
        reason: format!("VA-API {operation} failed: {error}"),
    }
}

fn copy_frame(
    handle: &impl DecodedHandle<Frame = VaapiCpuFrame>,
    format: RawVideoFormat,
    timing: CpuDecodeTiming,
) -> Result<DecodedVideoFrame, VideoDecodeError> {
    let visible = handle.display_resolution();
    if (visible.width, visible.height) != (format.width, format.height) {
        return Err(VideoDecodeError::InvalidOutputFormat {
            reason: format!(
                "VA-API decoded {}x{}, expected {}x{}",
                visible.width, visible.height, format.width, format.height
            ),
        });
    }
    let frame = handle.video_frame();
    let pitches = frame.get_plane_pitch();
    let mapping = frame
        .map()
        .map_err(|reason| VideoDecodeError::BackendFailed { reason })?;
    let planes = mapping.get();
    let pixels = match frame.decoded_format {
        DecodedFormat::NV12 => super::super::yuv::convert_nv12_to_bgra(
            planes[0],
            pitches[0],
            planes[1],
            pitches[1],
            format.width as usize,
            format.height as usize,
        )?,
        DecodedFormat::I010 => super::super::yuv::convert_p010_to_bgra(
            planes[0],
            pitches[0],
            planes[1],
            pitches[1],
            format.width as usize,
            format.height as usize,
        )?,
        _ => {
            return Err(VideoDecodeError::BackendFailed {
                reason: format!(
                    "VA-API returned unsupported decoded format {:?}",
                    frame.decoded_format
                ),
            });
        }
    };
    Ok(DecodedVideoFrame {
        pts: timing.pts,
        dts: timing.dts,
        duration: timing.duration,
        format,
        pixels,
        keyframe: timing.keyframe,
    })
}

#[derive(Debug)]
pub(in crate::transcode) struct VaapiCpuFrame {
    allocation: NonNull<u8>,
    allocation_layout: Layout,
    resolution: Resolution,
    decoded_format: DecodedFormat,
    stride: usize,
    uv_offset: usize,
}

impl VaapiCpuFrame {
    pub(in crate::transcode) fn new(
        resolution: Resolution,
        decoded_format: DecodedFormat,
    ) -> Result<Self, String> {
        let bytes_per_sample = match decoded_format {
            DecodedFormat::NV12 => 1,
            DecodedFormat::I010 => 2,
            _ => {
                return Err(format!(
                    "unsupported VA-API surface format {decoded_format:?}"
                ));
            }
        };
        let width = align_up(resolution.width as usize, 16)?;
        let height = align_up(resolution.height as usize, 4)?;
        let row_bytes = width
            .checked_mul(bytes_per_sample)
            .ok_or("VA-API surface row byte count overflowed")?;
        let stride = align_up(row_bytes, 64)?;
        let uv_offset = height
            .checked_mul(stride)
            .ok_or("VA-API luma size overflowed")?;
        let uv_size = height
            .div_ceil(2)
            .checked_mul(stride)
            .ok_or("VA-API chroma size overflowed")?;
        let allocation_size = uv_offset
            .checked_add(uv_size)
            .ok_or("VA-API surface size overflowed")?;
        if allocation_size > MAX_FRAME_BYTES {
            return Err("VA-API frame exceeds the 512 MiB safety limit".to_string());
        }
        let allocation_layout = Layout::from_size_align(allocation_size, FRAME_ALIGNMENT)
            .map_err(|error| format!("invalid VA-API allocation layout: {error}"))?;
        // SAFETY: allocation_layout is non-zero and valid. The pointer is retained until Drop.
        let allocation = NonNull::new(unsafe { alloc_zeroed(allocation_layout) })
            .ok_or_else(|| "VA-API frame allocation failed".to_string())?;
        Ok(Self {
            allocation,
            allocation_layout,
            resolution,
            decoded_format,
            stride,
            uv_offset,
        })
    }

    pub(in crate::transcode) fn frame_layout(&self) -> nuxodecs::FrameLayout {
        nuxodecs::FrameLayout {
            format: (self.fourcc(), 0),
            size: self.resolution,
            planes: vec![
                nuxodecs::PlaneLayout {
                    buffer_index: 0,
                    offset: 0,
                    stride: self.stride,
                },
                nuxodecs::PlaneLayout {
                    buffer_index: 0,
                    offset: self.uv_offset,
                    stride: self.stride,
                },
            ],
        }
    }

    pub(in crate::transcode) fn copy_bgra_to_nv12(&mut self, bgra: &[u8]) -> Result<(), String> {
        if self.decoded_format != DecodedFormat::NV12 {
            return Err("BGRA upload currently requires an NV12 VA-API surface".to_string());
        }
        let width = self.resolution.width as usize;
        let height = self.resolution.height as usize;
        let expected = width
            .checked_mul(height)
            .and_then(|pixels| pixels.checked_mul(4))
            .ok_or("BGRA input size overflowed")?;
        if bgra.len() != expected {
            return Err(format!(
                "BGRA input has {} bytes, expected {expected}",
                bgra.len()
            ));
        }
        // SAFETY: the allocation is exclusively borrowed and both plane ranges are in bounds.
        let bytes = unsafe {
            std::slice::from_raw_parts_mut(self.allocation.as_ptr(), self.allocation_layout.size())
        };
        for row in 0..height {
            for column in 0..width {
                let source = (row * width + column) * 4;
                let b = i32::from(bgra[source]);
                let g = i32::from(bgra[source + 1]);
                let r = i32::from(bgra[source + 2]);
                bytes[row * self.stride + column] =
                    (((47 * r + 157 * g + 16 * b + 128) >> 8) + 16).clamp(0, 255) as u8;
            }
        }
        for row in (0..height).step_by(2) {
            for column in (0..width).step_by(2) {
                let mut r = 0_i32;
                let mut g = 0_i32;
                let mut b = 0_i32;
                let mut count = 0_i32;
                for y in row..(row + 2).min(height) {
                    for x in column..(column + 2).min(width) {
                        let source = (y * width + x) * 4;
                        b += i32::from(bgra[source]);
                        g += i32::from(bgra[source + 1]);
                        r += i32::from(bgra[source + 2]);
                        count += 1;
                    }
                }
                r /= count;
                g /= count;
                b /= count;
                let destination = self.uv_offset + (row / 2) * self.stride + column;
                bytes[destination] =
                    (((-26 * r - 87 * g + 112 * b + 128) >> 8) + 128).clamp(0, 255) as u8;
                bytes[destination + 1] =
                    (((112 * r - 102 * g - 10 * b + 128) >> 8) + 128).clamp(0, 255) as u8;
            }
        }
        Ok(())
    }
}

impl Drop for VaapiCpuFrame {
    fn drop(&mut self) {
        // SAFETY: this pointer was allocated with this layout and is owned by this frame.
        unsafe { dealloc(self.allocation.as_ptr(), self.allocation_layout) };
    }
}

// SAFETY: the VA picture owning an Arc to this frame is synchronized before CPU mapping. The
// allocation address is stable and no mutable CPU mapping is exposed while VA owns the surface.
unsafe impl Send for VaapiCpuFrame {}
// SAFETY: see Send; all CPU access is read-only after DecodedHandle::sync.
unsafe impl Sync for VaapiCpuFrame {}

#[derive(Debug)]
pub(in crate::transcode) struct VaapiUserPtrDescriptor {
    allocation_size: usize,
    resolution: Resolution,
    decoded_format: DecodedFormat,
    stride: usize,
    uv_offset: usize,
    buffers: Vec<*mut u8>,
}

impl ExternalBufferDescriptor for VaapiUserPtrDescriptor {
    const MEMORY_TYPE: MemoryType = MemoryType::UserPtr;
    type DescriptorAttribute = VASurfaceAttribExternalBuffers;

    fn va_surface_attribute(&mut self) -> Self::DescriptorAttribute {
        VASurfaceAttribExternalBuffers {
            pixel_format: u32::from(fourcc_for_format(self.decoded_format)),
            width: self.resolution.width,
            height: self.resolution.height,
            data_size: self.allocation_size as u32,
            num_planes: 2,
            pitches: [self.stride as u32, self.stride as u32, 0, 0],
            offsets: [0, self.uv_offset as u32, 0, 0],
            buffers: self.buffers.as_mut_ptr().cast(),
            num_buffers: 1,
            flags: 0,
            private_data: std::ptr::null_mut(),
        }
    }
}

struct VaapiReadMapping<'a> {
    frame: &'a VaapiCpuFrame,
}

impl<'a> ReadMapping<'a> for VaapiReadMapping<'a> {
    fn get(&self) -> Vec<&[u8]> {
        let pointer = self.frame.allocation.as_ptr();
        let y_size = self.frame.uv_offset;
        let uv_size = self.frame.allocation_layout.size() - y_size;
        // SAFETY: both ranges are within the live allocation and the decoded handle was synced.
        unsafe {
            vec![
                std::slice::from_raw_parts(pointer, y_size),
                std::slice::from_raw_parts(pointer.add(y_size), uv_size),
            ]
        }
    }
}

impl VideoFrame for VaapiCpuFrame {
    type MemDescriptor = VaapiUserPtrDescriptor;
    type VaapiHandle = Surface<VaapiUserPtrDescriptor>;

    fn fourcc(&self) -> Fourcc {
        fourcc_for_format(self.decoded_format)
    }

    fn resolution(&self) -> Resolution {
        self.resolution
    }

    fn get_plane_size(&self) -> Vec<usize> {
        vec![
            self.uv_offset,
            self.allocation_layout.size() - self.uv_offset,
        ]
    }

    fn get_plane_pitch(&self) -> Vec<usize> {
        vec![self.stride, self.stride]
    }

    fn map<'a>(&'a self) -> Result<Box<dyn ReadMapping<'a> + 'a>, String> {
        Ok(Box::new(VaapiReadMapping { frame: self }))
    }

    fn map_mut<'a>(&'a mut self) -> Result<Box<dyn WriteMapping<'a> + 'a>, String> {
        Err("mutable CPU mapping is not supported for VA-API decode surfaces".to_string())
    }

    fn to_native_handle(&self, display: &Arc<Display>) -> Result<Self::VaapiHandle, String> {
        let descriptor = VaapiUserPtrDescriptor {
            allocation_size: self.allocation_layout.size(),
            resolution: self.resolution,
            decoded_format: self.decoded_format,
            stride: self.stride,
            uv_offset: self.uv_offset,
            buffers: vec![self.allocation.as_ptr()],
        };
        let mut surfaces = display
            .create_surfaces(
                rt_format_for_format(self.decoded_format),
                Some(u32::from(self.fourcc())),
                self.resolution.width,
                self.resolution.height,
                Some(UsageHint::USAGE_HINT_DECODER),
                vec![descriptor],
            )
            .map_err(|error| format!("VA-API user-pointer surface import failed: {error}"))?;
        surfaces
            .pop()
            .ok_or_else(|| "VA-API created no decode surface".to_string())
    }
}

fn fourcc_for_format(format: DecodedFormat) -> Fourcc {
    match format {
        DecodedFormat::I010 => Fourcc::from(b"P010"),
        _ => Fourcc::from(b"NV12"),
    }
}

fn rt_format_for_format(format: DecodedFormat) -> u32 {
    match format {
        DecodedFormat::I010 => nuxodecs::libva::VA_RT_FORMAT_YUV420_10,
        _ => nuxodecs::libva::VA_RT_FORMAT_YUV420,
    }
}

fn align_up(value: usize, alignment: usize) -> Result<usize, String> {
    value
        .checked_add(alignment - 1)
        .map(|rounded| rounded & !(alignment - 1))
        .ok_or_else(|| "frame alignment overflowed".to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn cpu_backed_nv12_surface_has_aligned_complete_planes() {
        let frame =
            VaapiCpuFrame::new(Resolution::from((1919, 1079)), DecodedFormat::NV12).unwrap();
        assert_eq!(frame.resolution(), Resolution::from((1919, 1079)));
        assert_eq!(frame.get_plane_pitch(), vec![1920, 1920]);
        assert_eq!(frame.get_plane_size(), vec![2_073_600, 1_036_800]);
        let mapping = frame.map().unwrap();
        assert_eq!(
            mapping
                .get()
                .iter()
                .map(|plane| plane.len())
                .collect::<Vec<_>>(),
            frame.get_plane_size()
        );
    }

    #[test]
    fn cpu_backed_p010_surface_uses_two_bytes_per_component() {
        let frame =
            VaapiCpuFrame::new(Resolution::from((1919, 1079)), DecodedFormat::I010).unwrap();
        assert_eq!(frame.fourcc(), Fourcc::from(b"P010"));
        assert_eq!(frame.get_plane_pitch(), vec![3840, 3840]);
        assert_eq!(frame.get_plane_size(), vec![4_147_200, 2_073_600]);
    }

    #[test]
    fn bgra_upload_produces_limited_range_nv12() {
        let mut black = VaapiCpuFrame::new(Resolution::from((2, 2)), DecodedFormat::NV12).unwrap();
        black.copy_bgra_to_nv12(&[0; 16]).unwrap();
        let mapping = black.map().unwrap();
        let planes = mapping.get();
        assert_eq!(&planes[0][..2], &[16, 16]);
        assert_eq!(&planes[1][..2], &[128, 128]);

        let mut white = VaapiCpuFrame::new(Resolution::from((2, 2)), DecodedFormat::NV12).unwrap();
        white.copy_bgra_to_nv12(&[255; 16]).unwrap();
        let mapping = white.map().unwrap();
        assert_eq!(&mapping.get()[0][..2], &[235, 235]);
    }

    #[test]
    fn cpu_backed_nv12_surface_rejects_excessive_dimensions() {
        let error = VaapiCpuFrame::new(Resolution::from((u32::MAX, u32::MAX)), DecodedFormat::NV12)
            .unwrap_err();
        assert!(error.contains("overflowed") || error.contains("safety limit"));
    }
}
