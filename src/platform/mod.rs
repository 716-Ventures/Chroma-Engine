use serde::{Deserialize, Serialize};
use thiserror::Error;

use crate::{
    error::EngineErrorCode,
    transcode::{
        AudioCodec, PcmAudioFormat, RawVideoFormat, RawVideoPixelFormat, VideoCodec,
        encode_aac_cpu_from_interleaved_i16, encode_h264_cpu_bgra_frames,
    },
};

#[cfg(target_os = "macos")]
use crate::transcode::{encode_h264_videotoolbox_bgra_frame, encode_hevc_videotoolbox_bgra_frame};

#[cfg(target_os = "linux")]
mod vaapi;

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
/// Result of probing the host for encoding capabilities.
pub struct EncoderProbe {
    /// RFC 3339 timestamp for when the probe was collected.
    pub collected_at: String,
    /// Preferred encoder profile for new work.
    pub profile: EncoderProfile,
    /// Additional compatible encoder profiles.
    pub alternatives: Vec<EncoderProfile>,
    /// Encoder candidates considered during probing.
    pub considered_encoders: Vec<String>,
    /// Reasons candidate encoders were rejected or unavailable.
    pub failure_notes: Vec<EncoderFailureNote>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
/// Concrete encoder profile selected for output.
pub struct EncoderProfile {
    /// Hardware acceleration family.
    pub kind: HardwareKind,
    /// Backend-specific encoder name.
    pub video_encoder: String,
    /// Output video codec.
    pub codec: VideoOutputCodec,
    /// Optional hardware acceleration device or mode.
    pub hwaccel: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
/// Full encoder backend matrix for the current host.
pub struct EncoderBackendPlan {
    /// Operating system used to choose native backends.
    pub os: String,
    /// Video encoder backends Chroma can target.
    pub video_backends: Vec<EncoderBackend>,
    /// Audio encoder backends Chroma can target.
    pub audio_backends: Vec<AudioEncoderBackend>,
    /// CPU fallback selected when hardware is unavailable or rejected.
    pub cpu_fallback: EncoderProfile,
    /// Warmup tasks that should run before serving transcode playback.
    pub warmup_tasks: Vec<EncoderWarmupTask>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
/// Full decoder backend matrix for the current host.
pub struct DecoderBackendPlan {
    /// Operating system used to choose native backends.
    pub os: String,
    /// Video decoder backends Chroma can target.
    pub video_backends: Vec<VideoDecoderBackend>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
/// One video encoder backend candidate.
pub struct EncoderBackend {
    /// Hardware acceleration family.
    pub kind: HardwareKind,
    /// Output video codec.
    pub codec: VideoOutputCodec,
    /// Backend-specific encoder name.
    pub video_encoder: String,
    /// Optional hardware acceleration device or mode.
    pub hwaccel: Option<String>,
    /// Whether this backend is available for the current OS target.
    pub available: bool,
    /// Diagnostic reason when the backend is not available.
    pub unavailable_reason: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
/// One video decoder backend candidate.
pub struct VideoDecoderBackend {
    /// Hardware acceleration family.
    pub kind: HardwareKind,
    /// Source video codec decoded by this backend.
    pub codec: VideoCodec,
    /// Backend-specific decoder name.
    pub decoder: String,
    /// Optional hardware acceleration device or mode.
    pub hwaccel: Option<String>,
    /// Raw pixel format emitted for downstream native encoders.
    pub output_pixel_format: RawVideoPixelFormat,
    /// Native hardware surface formats this backend is designed to receive before download/convert.
    pub native_surface_formats: Vec<VideoDecodeSurfaceFormat>,
    /// True when the backend can keep decoded surfaces on the device for a compatible encoder path.
    pub zero_copy_capable: bool,
    /// Runtime capability state. Planning may select only executable or verified backends.
    pub state: CapabilityState,
    /// Whether this backend is executable in the current build.
    pub available: bool,
    /// Diagnostic reason when the backend is not available.
    pub unavailable_reason: Option<String>,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
/// Truthful lifecycle state for a runtime media capability.
pub enum CapabilityState {
    /// Chroma has a model/roadmap entry, but no local signal or executable implementation.
    Modeled,
    /// Chroma detected hardware/runtime presence, but no executable backend is wired.
    Detected,
    /// Chroma opened the backend/device, but has not completed a codec smoke probe.
    Opened,
    /// Chroma has an implementation that can run for this codec/profile.
    Executable,
    /// Chroma completed a real warmup/smoke probe for this codec/profile.
    Verified,
}

impl CapabilityState {
    fn selectable(self) -> bool {
        matches!(self, Self::Executable | Self::Verified)
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
/// One audio encoder backend candidate.
pub struct AudioEncoderBackend {
    /// Output audio codec.
    pub codec: AudioCodec,
    /// Backend-specific encoder name.
    pub encoder: String,
    /// Whether this backend is available for the current build target.
    pub available: bool,
    /// Diagnostic reason when the backend is not available.
    pub unavailable_reason: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
/// One encoder warmup task.
pub struct EncoderWarmupTask {
    /// Encoder name to warm.
    pub encoder: String,
    /// Media domain warmed by this task.
    pub kind: EncoderWarmupKind,
    /// Output codec warmed by this task.
    pub codec: String,
    /// Whether this warmup is required before first playback.
    pub required: bool,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
/// Media domain for an encoder warmup task.
pub enum EncoderWarmupKind {
    /// Video encoder warmup.
    Video,
    /// Audio encoder warmup.
    Audio,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
/// Diagnostic note for an encoder candidate that could not be used.
pub struct EncoderFailureNote {
    /// Candidate encoder name.
    pub encoder: String,
    /// Rejection or failure reason.
    pub reason: String,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
/// Native hardware backend family.
pub enum HardwareKind {
    /// Apple VideoToolbox.
    VideoToolbox,
    /// NVIDIA NVENC.
    Nvenc,
    /// NVIDIA NVDEC/CUDA video decode.
    Nvdec,
    /// Intel Quick Sync Video.
    Qsv,
    /// AMD Advanced Media Framework.
    Amf,
    /// Linux VA-API.
    Vaapi,
    /// Windows Direct3D 11 Video Acceleration.
    D3d11Va,
    /// Windows Direct3D 12 Video Acceleration.
    D3d12Va,
    /// Windows DirectX Video Acceleration 2.
    Dxva2,
    /// CPU encoder fallback.
    Cpu,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
/// Native decoded video surface formats used by hardware decoder backends.
pub enum VideoDecodeSurfaceFormat {
    /// Apple VideoToolbox/CoreVideo pixel-buffer surface.
    VideoToolboxPixelBuffer,
    /// 8-bit BGRA system-memory frames.
    Bgra,
    /// 8-bit NV12 luma/chroma surfaces.
    Nv12,
    /// 10-bit P010 luma/chroma surfaces.
    P010,
    /// Linux VA-API device surface.
    VaapiSurface,
    /// NVIDIA CUDA/NVDEC device surface.
    CudaSurface,
    /// Intel Quick Sync device surface.
    QsvSurface,
    /// AMD AMF device surface.
    AmfSurface,
    /// Direct3D 11 texture surface.
    D3d11Texture,
    /// Direct3D 12 texture surface.
    D3d12Texture,
    /// DXVA2 decoder surface.
    Dxva2Surface,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
/// Video output codec family.
pub enum VideoOutputCodec {
    /// H.264/AVC output.
    H264,
    /// H.265/HEVC output.
    Hevc,
}

#[derive(Debug, Error)]
/// Error returned while warming an encoder backend.
#[error("encoder warmup failed: {reason}")]
pub struct EncoderWarmupError {
    /// Warmup failure reason.
    pub reason: String,
}

impl EncoderWarmupError {
    /// Returns the stable Chroma Engine error code for this warmup failure.
    pub fn code(&self) -> EngineErrorCode {
        EngineErrorCode::EncoderWarmupFailed
    }
}

/// Probes host encoder support and returns the preferred profile.
pub fn encoder_probe() -> EncoderProbe {
    let considered_encoders = native_candidate_names();
    let mut profiles = native_encoder_profiles();
    let profile = profiles
        .first()
        .cloned()
        .unwrap_or_else(default_cpu_profile);
    let alternatives = profiles.drain(1..).collect();
    let failure_notes = if profile.kind == HardwareKind::Cpu {
        considered_encoders
            .iter()
            .map(|encoder| EncoderFailureNote {
                encoder: encoder.clone(),
                reason: "native encode backend is planned but not executable in this build"
                    .to_string(),
            })
            .collect()
    } else {
        Vec::new()
    };

    EncoderProbe {
        collected_at: now_iso8601(),
        profile,
        alternatives,
        considered_encoders,
        failure_notes,
    }
}

/// Returns the host encoder backend matrix used by transcode planning.
pub fn encoder_backend_plan() -> EncoderBackendPlan {
    let os = std::env::consts::OS.to_string();
    let video_backends = video_backend_matrix();
    let audio_backends = audio_backend_matrix();
    let warmup_tasks = video_backends
        .iter()
        .filter(|backend| backend.available)
        .map(|backend| EncoderWarmupTask {
            encoder: backend.video_encoder.clone(),
            kind: EncoderWarmupKind::Video,
            codec: video_codec_label(backend.codec).to_string(),
            required: true,
        })
        .chain(
            audio_backends
                .iter()
                .filter(|backend| backend.available)
                .map(|backend| EncoderWarmupTask {
                    encoder: backend.encoder.clone(),
                    kind: EncoderWarmupKind::Audio,
                    codec: audio_codec_label(backend.codec).to_string(),
                    required: true,
                }),
        )
        .collect();

    EncoderBackendPlan {
        os,
        video_backends,
        audio_backends,
        cpu_fallback: default_cpu_profile(),
        warmup_tasks,
    }
}

/// Returns the host decoder backend matrix used by transcode planning.
pub fn decoder_backend_plan() -> DecoderBackendPlan {
    DecoderBackendPlan {
        os: std::env::consts::OS.to_string(),
        video_backends: video_decoder_backend_matrix(),
    }
}

/// Performs lightweight encoder startup work before serving playback.
pub fn warmup() -> Result<(), EncoderWarmupError> {
    let plan = encoder_backend_plan();
    for task in plan.warmup_tasks {
        if task.encoder.trim().is_empty() {
            return Err(EncoderWarmupError {
                reason: "encoder warmup task has an empty encoder name".to_string(),
            });
        }
        run_warmup_task(&task)?;
    }
    Ok(())
}

fn run_warmup_task(task: &EncoderWarmupTask) -> Result<(), EncoderWarmupError> {
    match (task.kind, task.encoder.as_str(), task.codec.as_str()) {
        (EncoderWarmupKind::Video, "chroma-videotoolbox-h264", "h264") => warm_h264_encoder(),
        (EncoderWarmupKind::Video, "chroma-videotoolbox-hevc", "hevc") => warm_hevc_encoder(),
        (EncoderWarmupKind::Video, "chroma-cpu-h264", "h264") => warm_cpu_h264_encoder(),
        (EncoderWarmupKind::Audio, "chroma-audiotoolbox-aac", "aac") => warm_aac_encoder(),
        (EncoderWarmupKind::Audio, "chroma-cpu-aac", "aac") => warm_cpu_aac_encoder(),
        _ => Ok(()),
    }
}

fn warm_hevc_encoder() -> Result<(), EncoderWarmupError> {
    #[cfg(target_os = "macos")]
    {
        let format = RawVideoFormat {
            width: 128,
            height: 72,
            frame_rate_num: 24,
            frame_rate_den: 1,
            pixel_format: RawVideoPixelFormat::Bgra,
        };
        let bgra = vec![0_u8; format.width as usize * format.height as usize * 4];
        encode_hevc_videotoolbox_bgra_frame(format, &bgra, 500_000).map_err(|error| {
            EncoderWarmupError {
                reason: format!("HEVC warmup failed: {error}"),
            }
        })?;
    }
    Ok(())
}

fn warm_h264_encoder() -> Result<(), EncoderWarmupError> {
    #[cfg(target_os = "macos")]
    {
        let format = RawVideoFormat {
            width: 128,
            height: 72,
            frame_rate_num: 24,
            frame_rate_den: 1,
            pixel_format: RawVideoPixelFormat::Bgra,
        };
        let bgra = vec![0_u8; format.width as usize * format.height as usize * 4];
        encode_h264_videotoolbox_bgra_frame(format, &bgra, 500_000).map_err(|error| {
            EncoderWarmupError {
                reason: format!("H.264 warmup failed: {error}"),
            }
        })?;
    }
    Ok(())
}

fn warm_cpu_h264_encoder() -> Result<(), EncoderWarmupError> {
    let format = RawVideoFormat {
        width: 128,
        height: 72,
        frame_rate_num: 24,
        frame_rate_den: 1,
        pixel_format: RawVideoPixelFormat::Bgra,
    };
    let bgra = vec![0_u8; format.width as usize * format.height as usize * 4];
    let scale = crate::packet::TimeScale {
        units_per_second: 24,
    };
    encode_h264_cpu_bgra_frames(
        format,
        &[crate::transcode::RawVideoFrameRef {
            pts: crate::packet::TimePoint { units: 0, scale },
            dts: crate::packet::TimePoint { units: 0, scale },
            duration: crate::packet::TimeDelta { units: 1, scale },
            bytes: &bgra,
            keyframe: true,
        }],
        500_000,
    )
    .map_err(|error| EncoderWarmupError {
        reason: format!("CPU H.264 warmup failed: {error}"),
    })?;
    Ok(())
}

fn warm_aac_encoder() -> Result<(), EncoderWarmupError> {
    #[cfg(target_os = "macos")]
    {
        use crate::transcode::AudioToolboxAacEncoderSession;

        let format = PcmAudioFormat {
            sample_rate: 48_000,
            channels: 2,
        };
        let pcm = vec![0_i16; 2048 * format.channels as usize];
        AudioToolboxAacEncoderSession::new(format, 128_000)
            .and_then(|mut session| session.encode(&pcm))
            .map_err(|error| EncoderWarmupError {
                reason: format!("AAC warmup failed: {error}"),
            })?;
    }
    Ok(())
}

fn warm_cpu_aac_encoder() -> Result<(), EncoderWarmupError> {
    let format = PcmAudioFormat {
        sample_rate: 48_000,
        channels: 2,
    };
    let pcm = vec![0_i16; 2048 * format.channels as usize];
    encode_aac_cpu_from_interleaved_i16(format, &pcm, 128_000).map_err(|error| {
        EncoderWarmupError {
            reason: format!("CPU AAC warmup failed: {error}"),
        }
    })?;
    Ok(())
}

fn video_decoder_backend_matrix() -> Vec<VideoDecoderBackend> {
    video_decoder_backend_matrix_for_os(std::env::consts::OS)
}

fn video_decoder_backend_matrix_for_os(os: &str) -> Vec<VideoDecoderBackend> {
    match os {
        "macos" => vec![
            video_decoder_backend(
                "macos",
                HardwareKind::VideoToolbox,
                VideoCodec::H264,
                "chroma-videotoolbox-h264-decoder",
                &[
                    VideoDecodeSurfaceFormat::VideoToolboxPixelBuffer,
                    VideoDecodeSurfaceFormat::Bgra,
                    VideoDecodeSurfaceFormat::Nv12,
                ],
            ),
            video_decoder_backend(
                "macos",
                HardwareKind::VideoToolbox,
                VideoCodec::Hevc,
                "chroma-videotoolbox-hevc-decoder",
                &[
                    VideoDecodeSurfaceFormat::VideoToolboxPixelBuffer,
                    VideoDecodeSurfaceFormat::Bgra,
                    VideoDecodeSurfaceFormat::Nv12,
                    VideoDecodeSurfaceFormat::P010,
                ],
            ),
            cpu_h264_decoder_backend("macos"),
            cpu_hevc_decoder_backend("macos"),
            cpu_av1_decoder_backend("macos"),
        ],
        "linux" => vec![
            video_decoder_backend(
                "linux",
                HardwareKind::Vaapi,
                VideoCodec::H264,
                "chroma-vaapi-h264-decoder",
                &[
                    VideoDecodeSurfaceFormat::VaapiSurface,
                    VideoDecodeSurfaceFormat::Nv12,
                ],
            ),
            video_decoder_backend(
                "linux",
                HardwareKind::Vaapi,
                VideoCodec::Hevc,
                "chroma-vaapi-hevc-decoder",
                &[
                    VideoDecodeSurfaceFormat::VaapiSurface,
                    VideoDecodeSurfaceFormat::Nv12,
                    VideoDecodeSurfaceFormat::P010,
                ],
            ),
            video_decoder_backend(
                "linux",
                HardwareKind::Nvdec,
                VideoCodec::H264,
                "chroma-nvdec-h264-decoder",
                &[
                    VideoDecodeSurfaceFormat::CudaSurface,
                    VideoDecodeSurfaceFormat::Nv12,
                ],
            ),
            video_decoder_backend(
                "linux",
                HardwareKind::Nvdec,
                VideoCodec::Hevc,
                "chroma-nvdec-hevc-decoder",
                &[
                    VideoDecodeSurfaceFormat::CudaSurface,
                    VideoDecodeSurfaceFormat::Nv12,
                    VideoDecodeSurfaceFormat::P010,
                ],
            ),
            video_decoder_backend(
                "linux",
                HardwareKind::Qsv,
                VideoCodec::H264,
                "chroma-qsv-h264-decoder",
                &[
                    VideoDecodeSurfaceFormat::QsvSurface,
                    VideoDecodeSurfaceFormat::Nv12,
                ],
            ),
            video_decoder_backend(
                "linux",
                HardwareKind::Qsv,
                VideoCodec::Hevc,
                "chroma-qsv-hevc-decoder",
                &[
                    VideoDecodeSurfaceFormat::QsvSurface,
                    VideoDecodeSurfaceFormat::Nv12,
                    VideoDecodeSurfaceFormat::P010,
                ],
            ),
            cpu_h264_decoder_backend("linux"),
            cpu_hevc_decoder_backend("linux"),
            cpu_av1_decoder_backend("linux"),
        ],
        "windows" => vec![
            video_decoder_backend(
                "windows",
                HardwareKind::D3d11Va,
                VideoCodec::H264,
                "chroma-d3d11va-h264-decoder",
                &[
                    VideoDecodeSurfaceFormat::D3d11Texture,
                    VideoDecodeSurfaceFormat::Nv12,
                ],
            ),
            video_decoder_backend(
                "windows",
                HardwareKind::D3d11Va,
                VideoCodec::Hevc,
                "chroma-d3d11va-hevc-decoder",
                &[
                    VideoDecodeSurfaceFormat::D3d11Texture,
                    VideoDecodeSurfaceFormat::Nv12,
                    VideoDecodeSurfaceFormat::P010,
                ],
            ),
            video_decoder_backend(
                "windows",
                HardwareKind::D3d12Va,
                VideoCodec::H264,
                "chroma-d3d12va-h264-decoder",
                &[
                    VideoDecodeSurfaceFormat::D3d12Texture,
                    VideoDecodeSurfaceFormat::Nv12,
                ],
            ),
            video_decoder_backend(
                "windows",
                HardwareKind::D3d12Va,
                VideoCodec::Hevc,
                "chroma-d3d12va-hevc-decoder",
                &[
                    VideoDecodeSurfaceFormat::D3d12Texture,
                    VideoDecodeSurfaceFormat::Nv12,
                    VideoDecodeSurfaceFormat::P010,
                ],
            ),
            video_decoder_backend(
                "windows",
                HardwareKind::Dxva2,
                VideoCodec::H264,
                "chroma-dxva2-h264-decoder",
                &[
                    VideoDecodeSurfaceFormat::Dxva2Surface,
                    VideoDecodeSurfaceFormat::Nv12,
                ],
            ),
            video_decoder_backend(
                "windows",
                HardwareKind::Dxva2,
                VideoCodec::Hevc,
                "chroma-dxva2-hevc-decoder",
                &[
                    VideoDecodeSurfaceFormat::Dxva2Surface,
                    VideoDecodeSurfaceFormat::Nv12,
                    VideoDecodeSurfaceFormat::P010,
                ],
            ),
            video_decoder_backend(
                "windows",
                HardwareKind::Qsv,
                VideoCodec::H264,
                "chroma-qsv-h264-decoder",
                &[
                    VideoDecodeSurfaceFormat::QsvSurface,
                    VideoDecodeSurfaceFormat::Nv12,
                ],
            ),
            video_decoder_backend(
                "windows",
                HardwareKind::Qsv,
                VideoCodec::Hevc,
                "chroma-qsv-hevc-decoder",
                &[
                    VideoDecodeSurfaceFormat::QsvSurface,
                    VideoDecodeSurfaceFormat::Nv12,
                    VideoDecodeSurfaceFormat::P010,
                ],
            ),
            video_decoder_backend(
                "windows",
                HardwareKind::Amf,
                VideoCodec::H264,
                "chroma-amf-h264-decoder",
                &[
                    VideoDecodeSurfaceFormat::AmfSurface,
                    VideoDecodeSurfaceFormat::Nv12,
                ],
            ),
            video_decoder_backend(
                "windows",
                HardwareKind::Amf,
                VideoCodec::Hevc,
                "chroma-amf-hevc-decoder",
                &[
                    VideoDecodeSurfaceFormat::AmfSurface,
                    VideoDecodeSurfaceFormat::Nv12,
                    VideoDecodeSurfaceFormat::P010,
                ],
            ),
            video_decoder_backend(
                "windows",
                HardwareKind::Nvdec,
                VideoCodec::H264,
                "chroma-nvdec-h264-decoder",
                &[
                    VideoDecodeSurfaceFormat::CudaSurface,
                    VideoDecodeSurfaceFormat::Nv12,
                ],
            ),
            video_decoder_backend(
                "windows",
                HardwareKind::Nvdec,
                VideoCodec::Hevc,
                "chroma-nvdec-hevc-decoder",
                &[
                    VideoDecodeSurfaceFormat::CudaSurface,
                    VideoDecodeSurfaceFormat::Nv12,
                    VideoDecodeSurfaceFormat::P010,
                ],
            ),
            cpu_h264_decoder_backend("windows"),
            cpu_hevc_decoder_backend("windows"),
            cpu_av1_decoder_backend("windows"),
        ],
        _ => vec![
            cpu_h264_decoder_backend(os),
            cpu_hevc_decoder_backend(os),
            cpu_av1_decoder_backend(os),
        ],
    }
}

fn cpu_h264_decoder_backend(os: &str) -> VideoDecoderBackend {
    video_decoder_backend(
        os,
        HardwareKind::Cpu,
        VideoCodec::H264,
        "chroma-cpu-h264-decoder",
        &[VideoDecodeSurfaceFormat::Bgra],
    )
}

fn cpu_hevc_decoder_backend(os: &str) -> VideoDecoderBackend {
    video_decoder_backend(
        os,
        HardwareKind::Cpu,
        VideoCodec::Hevc,
        "chroma-cpu-hevc-decoder",
        &[VideoDecodeSurfaceFormat::Bgra],
    )
}

fn cpu_av1_decoder_backend(os: &str) -> VideoDecoderBackend {
    video_decoder_backend(
        os,
        HardwareKind::Cpu,
        VideoCodec::Av1,
        "chroma-dav1d-av1-decoder",
        &[VideoDecodeSurfaceFormat::Bgra],
    )
}

fn video_decoder_backend(
    os: &str,
    kind: HardwareKind,
    codec: VideoCodec,
    decoder: &str,
    native_surface_formats: &[VideoDecodeSurfaceFormat],
) -> VideoDecoderBackend {
    let state = video_decode_backend_state(os, kind, codec);
    let available = state.selectable();
    VideoDecoderBackend {
        kind,
        codec,
        decoder: decoder.to_string(),
        hwaccel: (kind != HardwareKind::Cpu).then(|| format!("{kind:?}").to_lowercase()),
        output_pixel_format: RawVideoPixelFormat::Bgra,
        native_surface_formats: native_surface_formats.to_vec(),
        zero_copy_capable: native_surface_formats
            .iter()
            .any(|format| hardware_surface_format(*format)),
        state,
        available,
        unavailable_reason: (!available).then(|| video_decode_unavailable_reason(os, kind, codec)),
    }
}

fn hardware_surface_format(format: VideoDecodeSurfaceFormat) -> bool {
    matches!(
        format,
        VideoDecodeSurfaceFormat::VideoToolboxPixelBuffer
            | VideoDecodeSurfaceFormat::VaapiSurface
            | VideoDecodeSurfaceFormat::CudaSurface
            | VideoDecodeSurfaceFormat::QsvSurface
            | VideoDecodeSurfaceFormat::AmfSurface
            | VideoDecodeSurfaceFormat::D3d11Texture
            | VideoDecodeSurfaceFormat::D3d12Texture
            | VideoDecodeSurfaceFormat::Dxva2Surface
    )
}

fn video_decode_unavailable_reason(os: &str, kind: HardwareKind, codec: VideoCodec) -> String {
    if os != std::env::consts::OS {
        return format!(
            "{kind:?} {codec:?} decode is modeled for {os}, not the current {} runtime",
            std::env::consts::OS
        );
    }
    if kind == HardwareKind::VideoToolbox {
        format!("VideoToolbox hardware decode support was not reported for {codec:?}")
    } else if kind == HardwareKind::Vaapi && os == "linux" {
        linux_vaapi_decode_unavailable_reason(codec)
    } else if kind == HardwareKind::Qsv && os == "linux" {
        "Linux Quick Sync decode is detected through DRM but is not executable in this build"
            .to_string()
    } else if kind == HardwareKind::Nvdec && os == "linux" {
        "NVIDIA decode device was not detected under /dev/nvidiactl".to_string()
    } else if os == "windows" {
        "Windows hardware decode runtime probing is modeled but not executable in this build"
            .to_string()
    } else {
        "native video decode backend is planned but not executable in this build".to_string()
    }
}

fn video_decode_backend_state(os: &str, kind: HardwareKind, codec: VideoCodec) -> CapabilityState {
    if os != std::env::consts::OS {
        return CapabilityState::Modeled;
    }
    match (os, kind) {
        (_, HardwareKind::Cpu) => CapabilityState::Executable,
        ("macos", HardwareKind::VideoToolbox) if video_toolbox_decode_supported(codec) => {
            CapabilityState::Executable
        }
        ("linux", HardwareKind::Vaapi)
            if cfg!(feature = "linux-vaapi")
                && codec == VideoCodec::H264
                && linux_vaapi_decode_supported(codec) =>
        {
            CapabilityState::Executable
        }
        ("linux", HardwareKind::Vaapi) if linux_vaapi_decode_supported(codec) => {
            CapabilityState::Opened
        }
        ("linux", HardwareKind::Vaapi | HardwareKind::Qsv) if linux_dri_decode_device_present() => {
            CapabilityState::Detected
        }
        ("linux", HardwareKind::Nvdec) if linux_nvidia_decode_device_present() => {
            CapabilityState::Detected
        }
        ("windows", HardwareKind::D3d11Va | HardwareKind::D3d12Va | HardwareKind::Dxva2) => {
            if windows_directx_decode_runtime_present() {
                CapabilityState::Detected
            } else {
                CapabilityState::Modeled
            }
        }
        ("windows", HardwareKind::Qsv | HardwareKind::Amf | HardwareKind::Nvdec) => {
            if windows_vendor_decode_runtime_present(kind) {
                CapabilityState::Detected
            } else {
                CapabilityState::Modeled
            }
        }
        _ => CapabilityState::Modeled,
    }
}

#[cfg(target_os = "macos")]
fn video_toolbox_decode_supported(codec: VideoCodec) -> bool {
    let codec_type = match codec {
        VideoCodec::H264 => objc2_core_media::kCMVideoCodecType_H264,
        VideoCodec::Hevc => objc2_core_media::kCMVideoCodecType_HEVC,
        VideoCodec::Av1 => return false,
    };
    #[allow(unsafe_code)]
    // SAFETY: `codec_type` is one of CoreMedia's declared video codec constants.
    unsafe {
        objc2_video_toolbox::VTIsHardwareDecodeSupported(codec_type)
    }
}

#[cfg(not(target_os = "macos"))]
fn video_toolbox_decode_supported(_codec: VideoCodec) -> bool {
    false
}

#[cfg(target_os = "linux")]
fn linux_dri_decode_device_present() -> bool {
    std::fs::read_dir("/dev/dri")
        .ok()
        .into_iter()
        .flat_map(|entries| entries.flatten())
        .any(|entry| entry.file_name().to_string_lossy().starts_with("renderD"))
}

#[cfg(target_os = "linux")]
fn linux_vaapi_decode_supported(codec: VideoCodec) -> bool {
    vaapi::decode_probe().supports(codec)
}

#[cfg(not(target_os = "linux"))]
fn linux_vaapi_decode_supported(_codec: VideoCodec) -> bool {
    false
}

#[cfg(target_os = "linux")]
fn linux_vaapi_decode_unavailable_reason(codec: VideoCodec) -> String {
    vaapi::decode_probe().unavailable_reason(codec)
}

#[cfg(not(target_os = "linux"))]
fn linux_vaapi_decode_unavailable_reason(_codec: VideoCodec) -> String {
    "VA-API runtime probing is available only in Linux builds".to_string()
}

#[cfg(not(target_os = "linux"))]
fn linux_dri_decode_device_present() -> bool {
    false
}

#[cfg(target_os = "linux")]
fn linux_nvidia_decode_device_present() -> bool {
    std::path::Path::new("/dev/nvidiactl").exists()
}

#[cfg(not(target_os = "linux"))]
fn linux_nvidia_decode_device_present() -> bool {
    false
}

#[cfg(target_os = "windows")]
fn windows_directx_decode_runtime_present() -> bool {
    true
}

#[cfg(not(target_os = "windows"))]
fn windows_directx_decode_runtime_present() -> bool {
    false
}

#[cfg(target_os = "windows")]
fn windows_vendor_decode_runtime_present(_kind: HardwareKind) -> bool {
    false
}

#[cfg(not(target_os = "windows"))]
fn windows_vendor_decode_runtime_present(_kind: HardwareKind) -> bool {
    false
}

fn default_cpu_profile() -> EncoderProfile {
    EncoderProfile {
        kind: HardwareKind::Cpu,
        video_encoder: "chroma-cpu-h264".to_string(),
        codec: VideoOutputCodec::H264,
        hwaccel: None,
    }
}

fn native_encoder_profiles() -> Vec<EncoderProfile> {
    #[cfg(target_os = "macos")]
    {
        vec![
            EncoderProfile {
                kind: HardwareKind::VideoToolbox,
                video_encoder: "chroma-videotoolbox-h264".to_string(),
                codec: VideoOutputCodec::H264,
                hwaccel: Some("videotoolbox".to_string()),
            },
            EncoderProfile {
                kind: HardwareKind::VideoToolbox,
                video_encoder: "chroma-videotoolbox-hevc".to_string(),
                codec: VideoOutputCodec::Hevc,
                hwaccel: Some("videotoolbox".to_string()),
            },
            default_cpu_profile(),
        ]
    }
    #[cfg(not(target_os = "macos"))]
    {
        vec![default_cpu_profile()]
    }
}

fn video_backend_matrix() -> Vec<EncoderBackend> {
    match std::env::consts::OS {
        "macos" => vec![
            executable_video_backend(
                HardwareKind::VideoToolbox,
                VideoOutputCodec::H264,
                "chroma-videotoolbox-h264",
            ),
            executable_video_backend(
                HardwareKind::VideoToolbox,
                VideoOutputCodec::Hevc,
                "chroma-videotoolbox-hevc",
            ),
            executable_video_backend(HardwareKind::Cpu, VideoOutputCodec::H264, "chroma-cpu-h264"),
        ],
        "linux" => vec![
            planned_video_backend(
                HardwareKind::Nvenc,
                VideoOutputCodec::H264,
                "chroma-nvenc-h264",
            ),
            planned_video_backend(
                HardwareKind::Nvenc,
                VideoOutputCodec::Hevc,
                "chroma-nvenc-hevc",
            ),
            planned_video_backend(
                HardwareKind::Vaapi,
                VideoOutputCodec::H264,
                "chroma-vaapi-h264",
            ),
            planned_video_backend(
                HardwareKind::Vaapi,
                VideoOutputCodec::Hevc,
                "chroma-vaapi-hevc",
            ),
            planned_video_backend(HardwareKind::Qsv, VideoOutputCodec::H264, "chroma-qsv-h264"),
            planned_video_backend(HardwareKind::Qsv, VideoOutputCodec::Hevc, "chroma-qsv-hevc"),
            executable_video_backend(HardwareKind::Cpu, VideoOutputCodec::H264, "chroma-cpu-h264"),
        ],
        "windows" => vec![
            planned_video_backend(
                HardwareKind::Nvenc,
                VideoOutputCodec::H264,
                "chroma-nvenc-h264",
            ),
            planned_video_backend(
                HardwareKind::Nvenc,
                VideoOutputCodec::Hevc,
                "chroma-nvenc-hevc",
            ),
            planned_video_backend(HardwareKind::Qsv, VideoOutputCodec::H264, "chroma-qsv-h264"),
            planned_video_backend(HardwareKind::Qsv, VideoOutputCodec::Hevc, "chroma-qsv-hevc"),
            planned_video_backend(HardwareKind::Amf, VideoOutputCodec::H264, "chroma-amf-h264"),
            planned_video_backend(HardwareKind::Amf, VideoOutputCodec::Hevc, "chroma-amf-hevc"),
            executable_video_backend(HardwareKind::Cpu, VideoOutputCodec::H264, "chroma-cpu-h264"),
        ],
        _ => vec![executable_video_backend(
            HardwareKind::Cpu,
            VideoOutputCodec::H264,
            "chroma-cpu-h264",
        )],
    }
}

fn audio_backend_matrix() -> Vec<AudioEncoderBackend> {
    let mut backends = Vec::new();
    if cfg!(target_os = "macos") {
        backends.push(executable_audio_backend(
            AudioCodec::Aac,
            "chroma-audiotoolbox-aac",
        ));
    }
    backends.push(executable_audio_backend(AudioCodec::Aac, "chroma-cpu-aac"));
    backends.extend([
        planned_audio_backend(AudioCodec::Ac3, "chroma-ac3-copy"),
        planned_audio_backend(AudioCodec::Eac3, "chroma-eac3-copy"),
    ]);
    backends
}

fn executable_audio_backend(codec: AudioCodec, encoder: &str) -> AudioEncoderBackend {
    AudioEncoderBackend {
        codec,
        encoder: encoder.to_string(),
        available: true,
        unavailable_reason: None,
    }
}

fn planned_audio_backend(codec: AudioCodec, encoder: &str) -> AudioEncoderBackend {
    AudioEncoderBackend {
        codec,
        encoder: encoder.to_string(),
        available: false,
        unavailable_reason: Some(
            "native audio encode backend is planned but not executable in this build".to_string(),
        ),
    }
}

fn video_codec_label(codec: VideoOutputCodec) -> &'static str {
    match codec {
        VideoOutputCodec::H264 => "h264",
        VideoOutputCodec::Hevc => "hevc",
    }
}

fn audio_codec_label(codec: AudioCodec) -> &'static str {
    match codec {
        AudioCodec::Aac => "aac",
        AudioCodec::Ac3 => "ac3",
        AudioCodec::Eac3 => "eac3",
    }
}

fn planned_video_backend(
    kind: HardwareKind,
    codec: VideoOutputCodec,
    video_encoder: &str,
) -> EncoderBackend {
    EncoderBackend {
        kind,
        codec,
        video_encoder: video_encoder.to_string(),
        hwaccel: (kind != HardwareKind::Cpu).then(|| format!("{kind:?}").to_lowercase()),
        available: false,
        unavailable_reason: Some(
            "native video encode backend is planned but not executable in this build".to_string(),
        ),
    }
}

fn executable_video_backend(
    kind: HardwareKind,
    codec: VideoOutputCodec,
    video_encoder: &str,
) -> EncoderBackend {
    EncoderBackend {
        kind,
        codec,
        video_encoder: video_encoder.to_string(),
        hwaccel: (kind != HardwareKind::Cpu).then(|| format!("{kind:?}").to_lowercase()),
        available: true,
        unavailable_reason: None,
    }
}

fn native_candidate_names() -> Vec<String> {
    match std::env::consts::OS {
        "macos" => vec![
            "videotoolbox:h264".to_string(),
            "videotoolbox:hevc".to_string(),
        ],
        "windows" => vec![
            "nvenc:h264".to_string(),
            "nvenc:hevc".to_string(),
            "qsv:h264".to_string(),
            "qsv:hevc".to_string(),
            "amf:h264".to_string(),
            "amf:hevc".to_string(),
        ],
        "linux" => vec![
            "nvenc:h264".to_string(),
            "nvenc:hevc".to_string(),
            "vaapi:h264".to_string(),
            "vaapi:hevc".to_string(),
            "qsv:h264".to_string(),
            "qsv:hevc".to_string(),
        ],
        _ => Vec::new(),
    }
}

fn now_iso8601() -> String {
    time::OffsetDateTime::now_utc()
        .format(&time::format_description::well_known::Rfc3339)
        .unwrap_or_else(|_| "1970-01-01T00:00:00Z".to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn encoder_probe_reports_only_executable_profiles() {
        let probe = encoder_probe();
        assert_eq!(probe.considered_encoders, native_candidate_names());
        assert_eq!(
            probe.profile.kind,
            if cfg!(target_os = "macos") {
                HardwareKind::VideoToolbox
            } else {
                HardwareKind::Cpu
            }
        );
        assert_eq!(probe.profile.codec, VideoOutputCodec::H264);
        if cfg!(target_os = "macos") {
            assert!(probe.alternatives.iter().any(|profile| {
                profile.kind == HardwareKind::Cpu && profile.codec == VideoOutputCodec::H264
            }));
            assert!(probe.failure_notes.is_empty());
        } else {
            assert!(probe.alternatives.is_empty());
            assert_eq!(probe.failure_notes.len(), probe.considered_encoders.len());
        }
    }

    #[test]
    fn native_encoder_profiles_are_chroma_named() {
        for profile in native_encoder_profiles() {
            assert!(profile.video_encoder.starts_with("chroma-"));
        }
    }

    #[test]
    fn backend_plan_reports_audio_backend_availability_and_cpu_fallback() {
        let plan = encoder_backend_plan();

        assert_eq!(plan.cpu_fallback.kind, HardwareKind::Cpu);
        assert!(plan.audio_backends.iter().any(|backend| {
            backend.codec == AudioCodec::Aac
                && backend.encoder == "chroma-cpu-aac"
                && backend.available
                && backend.unavailable_reason.is_none()
        }));
        assert_eq!(
            plan.audio_backends.iter().any(|backend| {
                backend.codec == AudioCodec::Aac
                    && backend.encoder == "chroma-audiotoolbox-aac"
                    && backend.available
            }),
            cfg!(target_os = "macos")
        );
        assert!(plan.audio_backends.iter().any(|backend| {
            backend.codec == AudioCodec::Ac3
                && backend.encoder == "chroma-ac3-copy"
                && !backend.available
        }));
        assert!(plan.audio_backends.iter().any(|backend| {
            backend.codec == AudioCodec::Eac3
                && backend.encoder == "chroma-eac3-copy"
                && !backend.available
        }));
    }

    #[test]
    fn backend_plan_models_platform_video_targets() {
        let plan = encoder_backend_plan();

        if std::env::consts::OS == "macos" {
            assert!(plan.video_backends.iter().any(|backend| {
                backend.kind == HardwareKind::VideoToolbox
                    && backend.codec == VideoOutputCodec::H264
                    && backend.available
            }));
            assert!(plan.video_backends.iter().any(|backend| {
                backend.kind == HardwareKind::VideoToolbox
                    && backend.codec == VideoOutputCodec::Hevc
                    && backend.available
            }));
            assert!(plan.warmup_tasks.iter().any(|task| {
                task.kind == EncoderWarmupKind::Video
                    && task.encoder == "chroma-videotoolbox-h264"
                    && task.codec == "h264"
            }));
            assert!(plan.warmup_tasks.iter().any(|task| {
                task.kind == EncoderWarmupKind::Video
                    && task.encoder == "chroma-videotoolbox-hevc"
                    && task.codec == "hevc"
            }));
        }
        assert!(plan.video_backends.iter().any(|backend| {
            backend.kind == HardwareKind::Cpu
                && backend.codec == VideoOutputCodec::H264
                && backend.available
        }));
        assert!(plan.warmup_tasks.iter().any(|task| {
            task.kind == EncoderWarmupKind::Video
                && task.encoder == "chroma-cpu-h264"
                && task.codec == "h264"
        }));
    }

    #[test]
    fn decoder_backend_plan_models_platform_video_targets() {
        let plan = decoder_backend_plan();

        assert_eq!(plan.os, std::env::consts::OS);
        if std::env::consts::OS == "macos" {
            assert!(plan.video_backends.iter().any(|backend| {
                backend.kind == HardwareKind::VideoToolbox
                    && backend.codec == VideoCodec::H264
                    && backend.decoder == "chroma-videotoolbox-h264-decoder"
                    && backend.available == video_toolbox_decode_supported(VideoCodec::H264)
                    && backend.state
                        == if video_toolbox_decode_supported(VideoCodec::H264) {
                            CapabilityState::Executable
                        } else {
                            CapabilityState::Modeled
                        }
            }));
            assert!(plan.video_backends.iter().any(|backend| {
                backend.kind == HardwareKind::VideoToolbox
                    && backend.codec == VideoCodec::Hevc
                    && backend.decoder == "chroma-videotoolbox-hevc-decoder"
                    && backend.available == video_toolbox_decode_supported(VideoCodec::Hevc)
                    && backend.state
                        == if video_toolbox_decode_supported(VideoCodec::Hevc) {
                            CapabilityState::Executable
                        } else {
                            CapabilityState::Modeled
                        }
            }));
        }
        assert!(
            plan.video_backends
                .iter()
                .all(|backend| backend.output_pixel_format == RawVideoPixelFormat::Bgra)
        );
        assert!(plan.video_backends.iter().all(|backend| {
            backend
                .native_surface_formats
                .iter()
                .any(|format| matches!(format, VideoDecodeSurfaceFormat::Nv12))
                || matches!(backend.kind, HardwareKind::VideoToolbox | HardwareKind::Cpu)
        }));
        assert!(plan.video_backends.iter().any(|backend| {
            backend.kind == HardwareKind::Cpu
                && backend.codec == VideoCodec::H264
                && backend.decoder == "chroma-cpu-h264-decoder"
                && backend.state == CapabilityState::Executable
                && backend.available
                && backend.hwaccel.is_none()
        }));
        assert!(plan.video_backends.iter().any(|backend| {
            backend.kind == HardwareKind::Cpu
                && backend.codec == VideoCodec::Hevc
                && backend.decoder == "chroma-cpu-hevc-decoder"
                && backend.state == CapabilityState::Executable
                && backend.available
                && backend.hwaccel.is_none()
        }));
        assert!(plan.video_backends.iter().any(|backend| {
            backend.kind == HardwareKind::Cpu
                && backend.codec == VideoCodec::Av1
                && backend.decoder == "chroma-dav1d-av1-decoder"
                && backend.state == CapabilityState::Executable
                && backend.available
                && backend.hwaccel.is_none()
        }));
        assert!(
            plan.video_backends
                .iter()
                .all(|backend| { backend.available == backend.unavailable_reason.is_none() })
        );
    }

    #[test]
    fn decoder_backend_matrix_models_linux_hardware_decode_targets() {
        let backends = video_decoder_backend_matrix_for_os("linux");

        assert!(backends.iter().any(|backend| {
            backend.kind == HardwareKind::Vaapi
                && backend.codec == VideoCodec::H264
                && !backend.available
                && matches!(
                    backend.state,
                    CapabilityState::Modeled | CapabilityState::Detected
                )
                && backend
                    .native_surface_formats
                    .contains(&VideoDecodeSurfaceFormat::VaapiSurface)
        }));
        assert!(backends.iter().any(|backend| {
            backend.kind == HardwareKind::Nvdec
                && backend.codec == VideoCodec::Hevc
                && backend
                    .native_surface_formats
                    .contains(&VideoDecodeSurfaceFormat::CudaSurface)
                && backend
                    .native_surface_formats
                    .contains(&VideoDecodeSurfaceFormat::P010)
        }));
        assert!(backends.iter().any(|backend| {
            backend.kind == HardwareKind::Qsv
                && backend.codec == VideoCodec::Hevc
                && backend.zero_copy_capable
        }));
    }

    #[test]
    fn decoder_backend_matrix_models_windows_hardware_decode_targets() {
        let backends = video_decoder_backend_matrix_for_os("windows");

        assert!(backends.iter().any(|backend| {
            backend.kind == HardwareKind::D3d11Va
                && backend.codec == VideoCodec::H264
                && !backend.available
                && matches!(
                    backend.state,
                    CapabilityState::Modeled | CapabilityState::Detected
                )
                && backend
                    .native_surface_formats
                    .contains(&VideoDecodeSurfaceFormat::D3d11Texture)
        }));
        assert!(backends.iter().any(|backend| {
            backend.kind == HardwareKind::D3d12Va
                && backend.codec == VideoCodec::Hevc
                && backend
                    .native_surface_formats
                    .contains(&VideoDecodeSurfaceFormat::D3d12Texture)
                && backend
                    .native_surface_formats
                    .contains(&VideoDecodeSurfaceFormat::P010)
        }));
        assert!(backends.iter().any(|backend| {
            backend.kind == HardwareKind::Dxva2
                && backend.codec == VideoCodec::H264
                && backend
                    .native_surface_formats
                    .contains(&VideoDecodeSurfaceFormat::Dxva2Surface)
        }));
        assert!(backends.iter().any(|backend| {
            backend.kind == HardwareKind::Amf
                && backend.codec == VideoCodec::Hevc
                && backend
                    .native_surface_formats
                    .contains(&VideoDecodeSurfaceFormat::AmfSurface)
        }));
        assert!(backends.iter().any(|backend| {
            backend.kind == HardwareKind::Nvdec
                && backend.codec == VideoCodec::Hevc
                && backend
                    .native_surface_formats
                    .contains(&VideoDecodeSurfaceFormat::CudaSurface)
        }));
    }

    #[test]
    fn warmup_validates_planned_encoder_tasks() {
        warmup().unwrap();
    }

    #[test]
    fn backend_plan_warms_available_audio_backends() {
        let plan = encoder_backend_plan();
        let has_aac_warmup = plan.warmup_tasks.iter().any(|task| {
            task.kind == EncoderWarmupKind::Audio
                && task.codec == "aac"
                && task.encoder == "chroma-audiotoolbox-aac"
        });

        let has_cpu_aac_warmup = plan.warmup_tasks.iter().any(|task| {
            task.kind == EncoderWarmupKind::Audio
                && task.codec == "aac"
                && task.encoder == "chroma-cpu-aac"
        });

        assert_eq!(has_aac_warmup, cfg!(target_os = "macos"));
        assert!(has_cpu_aac_warmup);
        assert!(plan.warmup_tasks.iter().all(|task| {
            task.kind != EncoderWarmupKind::Audio
                || matches!(
                    task.encoder.as_str(),
                    "chroma-audiotoolbox-aac" | "chroma-cpu-aac"
                )
        }));
    }

    #[test]
    fn backend_plan_warms_available_video_backends() {
        let plan = encoder_backend_plan();
        let has_h264_warmup = plan.warmup_tasks.iter().any(|task| {
            task.kind == EncoderWarmupKind::Video
                && task.codec == "h264"
                && task.encoder == "chroma-videotoolbox-h264"
        });
        let has_hevc_warmup = plan.warmup_tasks.iter().any(|task| {
            task.kind == EncoderWarmupKind::Video
                && task.codec == "hevc"
                && task.encoder == "chroma-videotoolbox-hevc"
        });
        let has_cpu_h264_warmup = plan.warmup_tasks.iter().any(|task| {
            task.kind == EncoderWarmupKind::Video
                && task.codec == "h264"
                && task.encoder == "chroma-cpu-h264"
        });

        assert_eq!(has_h264_warmup, cfg!(target_os = "macos"));
        assert_eq!(has_hevc_warmup, cfg!(target_os = "macos"));
        assert!(has_cpu_h264_warmup);
    }
}
