use serde::{Deserialize, Serialize};

use crate::platform::EncoderProfile;

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct OperationPlan {
    pub mode: PlanMode,
    pub encoder: EncoderProfile,
    pub video_op: VideoOp,
    pub audio_ops: Vec<AudioOp>,
    pub subtitle_ops: Vec<SubtitleOp>,
    pub summary: String,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub enum PlanMode {
    LiveHls,
    OfflineMp4,
    OfflineMp4RemuxCopy,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase", tag = "kind")]
pub enum VideoOp {
    Copy { codec: String },
    Transcode { codec: VideoCodec, bitrate: u64 },
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub enum VideoCodec {
    H264,
    Hevc,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase", tag = "kind")]
pub enum AudioOp {
    Copy { source_index: u32, codec: String },
    Transcode {
        source_index: u32,
        codec: AudioCodec,
        channels: u32,
        bitrate: u64,
    },
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub enum AudioCodec {
    Aac,
    Ac3,
    Eac3,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase", tag = "kind")]
pub enum SubtitleOp {
    MovTextInline { source_index: u32 },
    WebVttRendition { source_index: u32 },
    Drop { source_index: u32, reason: String },
}
