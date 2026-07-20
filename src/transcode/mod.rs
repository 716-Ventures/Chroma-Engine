use serde::{Deserialize, Serialize};

use crate::platform::EncoderProfile;

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase")]
/// High-level operation graph for a transcode or remux job.
pub struct OperationPlan {
    /// Execution mode for the operation.
    pub mode: PlanMode,
    /// Encoder profile selected for output.
    pub encoder: EncoderProfile,
    /// Planned video operation.
    pub video_op: VideoOp,
    /// Planned audio operations, one per selected source track.
    pub audio_ops: Vec<AudioOp>,
    /// Planned subtitle operations, one per selected subtitle track.
    pub subtitle_ops: Vec<SubtitleOp>,
    /// Human-readable plan summary for diagnostics.
    pub summary: String,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
/// Supported execution modes for media operations.
pub enum PlanMode {
    /// Live segmented output for HLS playback.
    LiveHls,
    /// Offline MP4 output with transcoding as needed.
    OfflineMp4,
    /// Offline MP4 output using packet-copy remuxing only.
    OfflineMp4RemuxCopy,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase", tag = "kind")]
/// Planned video path.
pub enum VideoOp {
    /// Keep compressed video packets unchanged.
    Copy {
        /// Source video codec identifier.
        codec: String,
    },
    /// Decode and encode video to a target codec and bitrate.
    Transcode {
        /// Output video codec.
        codec: VideoCodec,
        /// Target video bitrate in bits per second.
        bitrate: u64,
    },
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
/// Video codecs Chroma Engine can target.
pub enum VideoCodec {
    /// H.264/AVC output.
    H264,
    /// H.265/HEVC output.
    Hevc,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase", tag = "kind")]
/// Planned audio path for one source track.
pub enum AudioOp {
    /// Keep compressed audio packets unchanged.
    Copy {
        /// Source track index.
        source_index: u32,
        /// Source audio codec identifier.
        codec: String,
    },
    /// Decode and encode audio to a target codec.
    Transcode {
        /// Source track index.
        source_index: u32,
        /// Output audio codec.
        codec: AudioCodec,
        /// Output channel count.
        channels: u32,
        /// Target audio bitrate in bits per second.
        bitrate: u64,
    },
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
/// Audio codecs Chroma Engine can target.
pub enum AudioCodec {
    /// AAC-LC output.
    Aac,
    /// Dolby Digital AC-3 output.
    Ac3,
    /// Dolby Digital Plus E-AC-3 output.
    Eac3,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase", tag = "kind")]
/// Planned subtitle path for one source track.
pub enum SubtitleOp {
    /// Keep MP4 timed text inline when the target supports it.
    MovTextInline {
        /// Source track index.
        source_index: u32,
    },
    /// Convert text subtitles to a WebVTT rendition.
    WebVttRendition {
        /// Source track index.
        source_index: u32,
    },
    /// Omit a subtitle track from output.
    Drop {
        /// Source track index.
        source_index: u32,
        /// Reason the subtitle cannot be carried.
        reason: String,
    },
}
