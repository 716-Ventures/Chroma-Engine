use serde::{Deserialize, Serialize};

use crate::packet::TimeScale;
use crate::platform::EncoderProfile;

mod audio_clock;
mod audio_decode;
mod audio_encode;
mod execution_plan;
mod video_decode;
mod video_encode;

pub use audio_clock::{AudioClockConfig, AudioFrameTiming, AudioSampleClock};
pub use audio_decode::{
    AudioDecodeCodec, AudioDecodeError, AudioDecodeInput, CompressedAudioPacket, DecodedPcmFrame,
    DecodedPcmOutput, DecodedPcmStream, DtsAudioBridgeProbe, DtsAudioPacketProbe,
    build_audio_decode_input, decode_dts_core_to_interleaved_i16, probe_dts_audio_bridge,
};
pub use audio_encode::{
    AudioEncodeError, EncodedAudioOutput, PcmAudioFormat, encode_aac_from_interleaved_i16,
    encode_ac3_from_interleaved_i16, encode_eac3_from_interleaved_i16,
};
pub use execution_plan::{
    HlsTranscodeRequest, TranscodeExecutionPlan, TranscodeOutputAudio, TranscodeOutputPlan,
    TranscodeOutputVideo, TranscodeStage, TranscodeStageKind, TranscodeStageStatus,
    plan_hls_transcode,
};
pub use video_decode::{
    CompressedVideoPacket, DecodedVideoFrame, DecodedVideoOutput, DecodedVideoStream,
    VideoDecodeError, VideoDecodeInput, VideoDecodeSessionInfo, VideoDecoderAction,
    VideoDecoderDrainState, build_video_decode_input, decode_videotoolbox_bgra_frames,
    decoder_actions_for_input, probe_videotoolbox_h264_decoder_session,
    probe_videotoolbox_hevc_decoder_session, validate_decoded_video_format,
};
pub use video_encode::{
    EncodedVideoFrame, EncodedVideoOutput, EncodedVideoStream, RawVideoFormat, RawVideoPixelFormat,
    VideoEncodeError, VideoEncodeSessionInfo, encode_h264_videotoolbox_bgra_frame,
    encode_hevc_videotoolbox_bgra_frame, probe_videotoolbox_h264_session,
    probe_videotoolbox_hevc_session,
};

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

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
/// Stable output description for an encoded audio stream.
pub struct EncodedAudioStream {
    /// Output codec carried by the stream.
    pub codec: AudioCodec,
    /// Output sample rate in Hz.
    pub sample_rate: u32,
    /// Output channel count.
    pub channels: u32,
    /// Codec-specific decoder configuration bytes, when required by the muxer.
    pub decoder_config: Option<Vec<u8>>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
/// One encoded audio access unit emitted by a native backend.
pub struct EncodedAudioFrame {
    /// Sample-clock timing for this access unit.
    pub timing: AudioFrameTiming,
    /// Compressed payload bytes.
    pub payload: Vec<u8>,
    /// True when this access unit starts after a real source discontinuity.
    pub discontinuity: bool,
}

impl EncodedAudioStream {
    /// Returns the time scale used by frames in this stream.
    pub fn time_scale(&self) -> TimeScale {
        TimeScale {
            units_per_second: self.sample_rate.max(1),
        }
    }
}

impl EncodedAudioFrame {
    /// Returns the frame presentation timestamp in milliseconds.
    pub fn pts_ms(&self) -> u64 {
        self.timing.pts.as_millis()
    }

    /// Returns the frame duration in milliseconds.
    pub fn duration_ms(&self) -> u64 {
        self.timing.duration.as_millis()
    }
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

fn rescale_units_rounded(units: u64, from: TimeScale, to: TimeScale) -> u64 {
    if from.units_per_second == 0 || to.units_per_second == 0 {
        return 0;
    }
    let numerator = u128::from(units) * u128::from(to.units_per_second);
    let denominator = u128::from(from.units_per_second);
    let rounded = (numerator + (denominator / 2)) / denominator;
    rounded.min(u128::from(u64::MAX)) as u64
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::packet::TimePoint;

    #[test]
    fn encoded_audio_stream_uses_sample_rate_time_scale() {
        let stream = EncodedAudioStream {
            codec: AudioCodec::Aac,
            sample_rate: 48_000,
            channels: 2,
            decoder_config: Some(vec![0x11, 0x90]),
        };

        assert_eq!(stream.time_scale().units_per_second, 48_000);
    }

    #[test]
    fn encoded_audio_frame_reports_millisecond_timing() {
        let mut clock = AudioSampleClock::new(AudioClockConfig {
            sample_rate: 48_000,
            discontinuity_threshold_ms: 100,
        });
        let frame = EncodedAudioFrame {
            timing: clock.stamp_frame(Some(TimePoint::millis(1_000)), 1_024),
            payload: vec![0xaa, 0xbb],
            discontinuity: false,
        };

        assert_eq!(frame.pts_ms(), 1_000);
        assert_eq!(frame.duration_ms(), 21);
    }
}
