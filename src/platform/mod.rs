use serde::{Deserialize, Serialize};
use thiserror::Error;

use crate::{error::EngineErrorCode, transcode::AudioCodec};

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
    /// Output video codec warmed by this task.
    pub codec: VideoOutputCodec,
    /// Whether this warmup is required before first playback.
    pub required: bool,
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
/// Hardware encoder family.
pub enum HardwareKind {
    /// Apple VideoToolbox.
    VideoToolbox,
    /// NVIDIA NVENC.
    Nvenc,
    /// Intel Quick Sync Video.
    Qsv,
    /// AMD Advanced Media Framework.
    Amf,
    /// Linux VA-API.
    Vaapi,
    /// CPU encoder fallback.
    Cpu,
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
    let has_native_profile = !profiles.is_empty();
    let profile = profiles
        .first()
        .cloned()
        .unwrap_or_else(default_cpu_profile);
    let alternatives = if has_native_profile {
        profiles.drain(1..).chain([default_cpu_profile()]).collect()
    } else {
        Vec::new()
    };
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
    let warmup_tasks = video_backends
        .iter()
        .filter(|backend| backend.available && backend.kind != HardwareKind::Cpu)
        .map(|backend| EncoderWarmupTask {
            encoder: backend.video_encoder.clone(),
            codec: backend.codec,
            required: true,
        })
        .collect();

    EncoderBackendPlan {
        os,
        video_backends,
        audio_backends: audio_backend_matrix(),
        cpu_fallback: default_cpu_profile(),
        warmup_tasks,
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
    }
    Ok(())
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
    Vec::new()
}

fn video_backend_matrix() -> Vec<EncoderBackend> {
    match std::env::consts::OS {
        "macos" => vec![
            planned_video_backend(
                HardwareKind::VideoToolbox,
                VideoOutputCodec::H264,
                "chroma-videotoolbox-h264",
            ),
            planned_video_backend(
                HardwareKind::VideoToolbox,
                VideoOutputCodec::Hevc,
                "chroma-videotoolbox-hevc",
            ),
            planned_video_backend(HardwareKind::Cpu, VideoOutputCodec::H264, "chroma-cpu-h264"),
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
            planned_video_backend(HardwareKind::Cpu, VideoOutputCodec::H264, "chroma-cpu-h264"),
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
            planned_video_backend(HardwareKind::Cpu, VideoOutputCodec::H264, "chroma-cpu-h264"),
        ],
        _ => vec![planned_video_backend(
            HardwareKind::Cpu,
            VideoOutputCodec::H264,
            "chroma-cpu-h264",
        )],
    }
}

fn audio_backend_matrix() -> Vec<AudioEncoderBackend> {
    [
        (AudioCodec::Aac, "chroma-audiotoolbox-aac"),
        (AudioCodec::Ac3, "chroma-ac3-bridge"),
        (AudioCodec::Eac3, "chroma-eac3-bridge"),
    ]
    .into_iter()
    .map(|(codec, encoder)| {
        let available = audio_backend_available(codec);
        AudioEncoderBackend {
            codec,
            encoder: encoder.to_string(),
            available,
            unavailable_reason: (!available).then(|| {
                "native audio encode backend is planned but not executable in this build"
                    .to_string()
            }),
        }
    })
    .collect()
}

fn audio_backend_available(codec: AudioCodec) -> bool {
    matches!(codec, AudioCodec::Aac) && cfg!(target_os = "macos")
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
        assert_eq!(probe.profile.kind, HardwareKind::Cpu);
        assert_eq!(probe.profile.codec, VideoOutputCodec::H264);
        assert!(probe.alternatives.is_empty());
        assert_eq!(probe.failure_notes.len(), probe.considered_encoders.len());
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
        let aac = plan
            .audio_backends
            .iter()
            .find(|backend| backend.codec == AudioCodec::Aac)
            .expect("AAC backend");
        assert_eq!(aac.available, cfg!(target_os = "macos"));
        assert!(
            aac.available
                || aac
                    .unavailable_reason
                    .as_deref()
                    .is_some_and(|reason| reason.contains("planned"))
        );
        assert!(
            plan.audio_backends
                .iter()
                .any(|backend| backend.codec == AudioCodec::Ac3 && !backend.available)
        );
        assert!(
            plan.audio_backends
                .iter()
                .any(|backend| backend.codec == AudioCodec::Eac3 && !backend.available)
        );
    }

    #[test]
    fn backend_plan_models_platform_video_targets() {
        let plan = encoder_backend_plan();

        if std::env::consts::OS == "macos" {
            assert!(plan.video_backends.iter().any(|backend| {
                backend.kind == HardwareKind::VideoToolbox
                    && backend.codec == VideoOutputCodec::H264
                    && !backend.available
            }));
            assert!(plan.video_backends.iter().any(|backend| {
                backend.kind == HardwareKind::VideoToolbox
                    && backend.codec == VideoOutputCodec::Hevc
                    && !backend.available
            }));
            assert!(plan.warmup_tasks.is_empty());
        } else {
            assert!(
                plan.video_backends
                    .iter()
                    .any(|backend| backend.kind == HardwareKind::Cpu && !backend.available)
            );
        }
    }

    #[test]
    fn warmup_validates_planned_encoder_tasks() {
        warmup().unwrap();
    }
}
