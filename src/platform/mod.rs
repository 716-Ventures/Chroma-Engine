use anyhow::Result;
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct EncoderProbe {
    pub collected_at: String,
    pub profile: EncoderProfile,
    pub alternatives: Vec<EncoderProfile>,
    pub considered_encoders: Vec<String>,
    pub failure_notes: Vec<EncoderFailureNote>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct EncoderProfile {
    pub kind: HardwareKind,
    pub video_encoder: String,
    pub codec: VideoOutputCodec,
    pub hwaccel: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct EncoderFailureNote {
    pub encoder: String,
    pub reason: String,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub enum HardwareKind {
    VideoToolbox,
    Nvenc,
    Qsv,
    Amf,
    Vaapi,
    Cpu,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub enum VideoOutputCodec {
    H264,
    Hevc,
}

pub fn encoder_probe() -> EncoderProbe {
    EncoderProbe {
        collected_at: now_iso8601(),
        profile: default_cpu_profile(),
        alternatives: Vec::new(),
        considered_encoders: native_candidate_names(),
        failure_notes: vec![EncoderFailureNote {
            encoder: "native".to_string(),
            reason: "backend probing not implemented yet".to_string(),
        }],
    }
}

pub fn warmup() -> Result<()> {
    Ok(())
}

fn default_cpu_profile() -> EncoderProfile {
    EncoderProfile {
        kind: HardwareKind::Cpu,
        video_encoder: "cpu-h264-placeholder".to_string(),
        codec: VideoOutputCodec::H264,
        hwaccel: None,
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
