pub mod cli;
pub(crate) mod codec;
pub(crate) mod container;
pub(crate) mod fmp4;
pub(crate) mod hls;
pub(crate) mod packet;
pub(crate) mod platform;
pub(crate) mod playback_manifest;
pub(crate) mod probe;
pub(crate) mod remux;
pub(crate) mod session;
pub(crate) mod source;
pub(crate) mod transcode;

pub use codec::subtitles::{
    TextSubtitleCue, WebVttSegment, parse_subrip, render_webvtt, segment_webvtt,
};
pub use container::{ContainerKind, sniff_container};
pub use hls::{
    HlsOptions, HlsOutput, HlsSegmentInfo, HlsVodPlan, HlsVodPlaylistPlan, write_hls_fmp4_init,
    write_hls_fmp4_segment, write_hls_fmp4_segments, write_hls_fmp4_vod, write_hls_segment,
    write_hls_segments, write_hls_vod,
};
pub use packet::{
    ChunkPlan, ChunkSample, ExtractedChunk, NativeChunk, PacketRange, TimeDelta, TimePoint,
    TimeScale, plan_fixed_chunks,
};
pub use platform::{
    EncoderFailureNote, EncoderProbe, EncoderProfile, HardwareKind, VideoOutputCodec,
    encoder_probe, warmup,
};
pub use playback_manifest::{
    ManifestTrack, MatroskaManifestOptions, Mp4ManifestOptions, NativePlaybackManifest,
    build_matroska_playback_manifest, build_mp4_playback_manifest,
};
pub use probe::{MediaProbe, ProbeError, probe_media_source};
pub use remux::remux_mp4;
pub use session::{
    AudioSelection, PipelineStage, PlaybackConstraints, PlaybackPlan, PlaybackTarget, StageKind,
    StageMode, TransportKind, TransportPlan, plan_playback,
};
pub use transcode::{
    AudioCodec, AudioOp, OperationPlan, PlanMode, SubtitleOp, VideoCodec, VideoOp,
};
