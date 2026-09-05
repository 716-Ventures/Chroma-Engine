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
pub(crate) mod engine;
pub(crate) mod error;
pub(crate) mod fmp4;
#[cfg(feature = "fuzzing")]
pub mod fuzzing;
pub(crate) mod hls;
pub(crate) mod output;
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
pub use engine::{
    Engine, EngineSessionError, PlaybackSession, PlaybackSessionOptions, PlaybackSessionPlan,
    PlaybackSessionStats,
};
pub use error::{EngineError, EngineErrorCode, RetryAdvice};
pub use hls::{
    HlsAudioRendition, HlsAudioRenditionInput, HlsError, HlsMultiAudioOutputPlan, HlsOptions,
    HlsOutput, HlsSegmentInfo, HlsVodPlan, HlsVodPlaylistPlan, plan_multi_audio_hls_outputs,
    write_hls_fmp4_init, write_hls_fmp4_segment, write_hls_fmp4_segments, write_hls_fmp4_vod,
    write_hls_segment, write_hls_segments, write_hls_vod,
};
pub use packet::{
    ChunkPlan, ChunkSample, CopySeekPlan, ExtractedChunk, NativeChunk, PacketPayloadSpan,
    PacketRange, PacketRef, SeekAnchor, SignedTimeDelta, SignedTimePoint, TimeDelta, TimeError,
    TimePoint, TimeRounding, TimeScale, packet_payload_spans, plan_copy_seek, plan_fixed_chunks,
    seek_anchor_for_packets,
};
pub use pipeline::{PipelineError, PipelineStageChunk, PipelineStagePayload, emit_stage_chunks};
pub use platform::{
    AudioEncoderBackend, CapabilityState, DecoderBackendPlan, EncoderBackend, EncoderBackendPlan,
    EncoderFailureNote, EncoderProbe, EncoderProfile, EncoderWarmupError, EncoderWarmupKind,
    EncoderWarmupTask, HardwareKind, VideoDecodeSurfaceFormat, VideoDecoderBackend,
    VideoOutputCodec, decoder_backend_plan, encoder_backend_plan, encoder_probe, warmup,
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
    AacEncoderSession, AudioClockConfig, AudioCodec, AudioDecodeCodec, AudioDecodeError,
    AudioDecodeInput, AudioEncodeError, AudioFrameTiming, AudioOp, AudioSampleClock,
    AudioToolboxAacEncoderSession, BgraDecoderSession, CompressedAudioPacket,
    CompressedVideoPacket, CpuAacEncoderSession, CpuAc3EncoderSession, CpuAv1BgraDecoderSession,
    CpuEac3EncoderSession, CpuH264BgraDecoderSession, CpuH264EncoderSession,
    CpuHevcBgraDecoderSession, DecodedPcmAudioFrame, DecodedPcmAudioOutput, DecodedVideoFrame,
    DecodedVideoOutput, DecodedVideoStream, DtsAudioBridgeProbe, DtsAudioDecoderSession,
    DtsAudioPacketProbe, EncodedAudioFrame, EncodedAudioOutput, EncodedAudioStream,
    EncodedVideoFrame, EncodedVideoOutput, EncodedVideoStream, H264EncoderSession,
    HlsTranscodeRequest, NativeFmp4TranscodeInitOutput, NativeFmp4TranscodeOptions,
    NativeFmp4TranscodePlan, NativeFmp4TranscodeSegmentOutput, NativeFmp4TranscodeSession,
    NativeFmp4TranscodeSessionStats, NativeFmp4TranscodeStartOutput, OperationPlan, PcmAudioFormat,
    PlanMode, RawVideoFormat, RawVideoFrameRef, RawVideoPixelFormat, SubtitleOp,
    TranscodeExecutionPlan, TranscodeOutputAudio, TranscodeOutputPlan, TranscodeOutputVideo,
    TranscodeStage, TranscodeStageKind, TranscodeStageStatus, TrueHdAudioDecoderSession,
    VideoCodec, VideoDecodeError, VideoDecodeInput, VideoDecodeSessionInfo, VideoDecoderAction,
    VideoDecoderDrainState, VideoEncodeError, VideoEncodeSessionInfo, VideoOp,
    VideoToolboxBgraDecoderSession, VideoToolboxH264EncoderSession, build_audio_decode_input,
    build_video_decode_input, convert_nv12_to_bgra, convert_p010_to_bgra,
    decode_av1_cpu_bgra_frames, decode_dts_to_interleaved_i16, decode_h264_cpu_bgra_frames,
    decode_hevc_cpu_bgra_frames, decode_truehd_to_interleaved_i16, decode_videotoolbox_bgra_frames,
    decoder_actions_for_input, encode_aac_cpu_from_interleaved_i16,
    encode_aac_from_interleaved_i16, encode_ac3_cpu_from_interleaved_i16,
    encode_eac3_cpu_from_interleaved_i16, encode_h264_cpu_bgra_frames,
    encode_h264_videotoolbox_bgra_frame, encode_h264_videotoolbox_bgra_frames,
    encode_hevc_videotoolbox_bgra_frame, plan_hls_transcode, probe_dts_audio_bridge,
    probe_videotoolbox_h264_decoder_session, probe_videotoolbox_h264_session,
    probe_videotoolbox_hevc_decoder_session, probe_videotoolbox_hevc_session,
    validate_decoded_video_format, write_native_fmp4_transcode_init,
    write_native_fmp4_transcode_segment, write_native_fmp4_transcode_start,
};
#[cfg(all(target_os = "linux", feature = "linux-nvidia"))]
pub use transcode::{
    NvdecH264BgraDecoderSession, NvdecHevcBgraDecoderSession, NvencH264EncoderSession,
    NvencHevcEncoderSession,
};
#[cfg(all(target_os = "linux", feature = "linux-vaapi"))]
pub use transcode::{
    VaapiH264BgraDecoderSession, VaapiH264EncoderSession, VaapiHevcBgraDecoderSession,
    VaapiHevcEncoderSession,
};
