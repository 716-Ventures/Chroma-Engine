//! Native media primitives for Chroma playback.
//!
//! Chroma Engine exposes a narrow Rust-first API for probing media sources,
//! planning playback pipelines, segmenting packet indexes, generating HLS
//! output, and describing native playback manifests. Internal parser and muxer
//! modules stay crate-private so the public contract can evolve deliberately.

#![warn(missing_docs)]

/// Command-line entrypoint support for the `chroma-engine` binary.
pub mod cli;
pub(crate) mod codec;
pub(crate) mod container;
pub(crate) mod error;
pub(crate) mod fmp4;
pub(crate) mod hls;
pub(crate) mod packet;
pub(crate) mod pipeline;
pub(crate) mod platform;
pub(crate) mod playback_manifest;
pub(crate) mod probe;
pub(crate) mod remux;
pub(crate) mod session;
pub(crate) mod source;
pub(crate) mod transcode;

pub use codec::subtitles::{
    MovTextSample, NativeTextSubtitleTrack, TextSubtitleCue, WebVttSegment, WebVttSidecarInput,
    WebVttSidecarSet, WebVttSidecarTrack, build_webvtt_sidecars,
    build_webvtt_sidecars_from_native_text_tracks, cues_to_mov_text_samples,
    encode_mov_text_sample, parse_native_text_subtitle_cues, parse_subrip, render_webvtt,
    segment_webvtt, write_webvtt_sidecars,
};
pub use container::{ContainerKind, sniff_container};
pub use error::EngineErrorCode;
pub use hls::{
    HlsAudioRendition, HlsAudioRenditionInput, HlsError, HlsMultiAudioOutputPlan, HlsOptions,
    HlsOutput, HlsSegmentInfo, HlsVodPlan, HlsVodPlaylistPlan, plan_multi_audio_hls_outputs,
    write_hls_fmp4_init, write_hls_fmp4_segment, write_hls_fmp4_segments, write_hls_fmp4_vod,
    write_hls_segment, write_hls_segments, write_hls_vod,
};
pub use packet::{
    ChunkPlan, ChunkSample, CopySeekPlan, ExtractedChunk, NativeChunk, PacketPayloadSpan,
    PacketRange, PacketRef, SeekAnchor, TimeDelta, TimePoint, TimeScale, packet_payload_spans,
    plan_copy_seek, plan_fixed_chunks, seek_anchor_for_packets,
};
pub use pipeline::{PipelineError, PipelineStageChunk, PipelineStagePayload, emit_stage_chunks};
pub use platform::{
    AudioEncoderBackend, EncoderBackend, EncoderBackendPlan, EncoderFailureNote, EncoderProbe,
    EncoderProfile, EncoderWarmupError, EncoderWarmupTask, HardwareKind, VideoOutputCodec,
    encoder_backend_plan, encoder_probe, warmup,
};
pub use playback_manifest::{
    ManifestTrack, MatroskaManifestOptions, Mp4ManifestOptions, NativePlaybackManifest,
    build_matroska_playback_manifest, build_mp4_playback_manifest,
};
pub use probe::{Chapter, MediaProbe, ProbeError, probe_media_source};
pub use remux::{
    RemuxChapter, RemuxError, RemuxMetadata, RemuxPacketSpans, RemuxTrackKind, RemuxTrackMetadata,
    copyable_metadata, remux_mp4, stream_copy_packet_spans, write_faststart_mp4,
};
pub use session::{
    AudioOutputPlan, AudioSelection, MultiAudioOutputPlan, PipelineStage, PlaybackConstraints,
    PlaybackPlan, PlaybackTarget, StageKind, StageMode, TransportKind, TransportPlan,
    plan_multi_audio_outputs, plan_playback,
};
pub use transcode::{
    AudioClockConfig, AudioCodec, AudioEncodeError, AudioFrameTiming, AudioOp, AudioSampleClock,
    EncodedAudioFrame, EncodedAudioOutput, EncodedAudioStream, OperationPlan, PcmAudioFormat,
    PlanMode, SubtitleOp, VideoCodec, VideoOp, encode_aac_from_interleaved_i16,
};
