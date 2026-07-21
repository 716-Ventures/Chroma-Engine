use std::path::{Path, PathBuf};

mod hls;

use crate::codec::aac::{aac_chunk_to_adts, parse_audio_specific_config};
use crate::codec::h264::{avc_chunk_to_annex_b, parse_avc_chunk_nalus};
use crate::codec::hevc::hevc_chunk_to_annex_b;
use crate::container::{
    matroska::{looks_like_ebml, parse_chunk_plan as parse_matroska_chunk_plan},
    mp4::{
        extract_chunk as extract_mp4_chunk, looks_like_mp4,
        parse_chunk_plan as parse_mp4_chunk_plan, parse_codec_config as parse_mp4_codec_config,
    },
};
use crate::output::publish_bytes;
use crate::packet::{ExtractedChunk, TimeDelta, TimePoint, TimeScale};
use crate::playback_manifest::{
    MatroskaManifestOptions, Mp4ManifestOptions, build_matroska_playback_manifest,
    build_mp4_playback_manifest,
};
use crate::probe::probe_media_source;
use crate::session::{AudioSelection, PlaybackConstraints, PlaybackTarget, plan_playback};
use crate::source::MappedMediaFile;
use crate::transcode::{
    AudioDecodeCodec, HlsTranscodeRequest, NativeFmp4TranscodeOptions, NativeFmp4VideoMode,
    RawVideoFormat, RawVideoPixelFormat, VideoCodec, build_audio_decode_input,
    build_video_decode_input, decode_videotoolbox_bgra_frames, plan_hls_transcode,
    probe_dts_audio_bridge, write_native_fmp4_transcode_init, write_native_fmp4_transcode_segment,
    write_native_fmp4_transcode_start,
};
use anyhow::{Result, bail};
use clap::{Parser, Subcommand};
use serde::Serialize;

#[derive(Debug, Parser)]
#[command(name = "chroma-engine")]
#[command(about = "Native Rust media engine for Chroma playback")]
struct Cli {
    #[command(subcommand)]
    command: Command,
}

#[derive(Debug, Subcommand)]
enum Command {
    /// Emit native Chroma media probe JSON.
    Probe { file: PathBuf },
    /// Emit a native Chroma playback pipeline plan.
    Plan {
        file: PathBuf,
        #[arg(long, value_enum, default_value_t = TargetArg::NativeChroma)]
        target: TargetArg,
        #[arg(long, default_value_t = false)]
        all_audio: bool,
        #[arg(long, default_value_t = true)]
        include_subtitles: bool,
    },
    /// Emit a native Chroma HLS transcode execution plan.
    TranscodePlan {
        file: PathBuf,
        #[arg(long, value_enum, default_value_t = TargetArg::AppleNative)]
        target: TargetArg,
        #[arg(long)]
        audio_track: Option<String>,
        #[arg(long, default_value_t = 4_000)]
        segment_ms: u64,
        #[arg(long, default_value_t = false)]
        force_video_transcode: bool,
    },
    /// Emit keyframe-aligned native chunk windows for a compressed packet track.
    Chunks {
        file: PathBuf,
        #[arg(long)]
        track: Option<String>,
        #[arg(long, default_value_t = 4_000)]
        target_ms: u64,
    },
    /// Emit decoder initialization facts for a compressed MP4/MOV track.
    CodecConfig {
        file: PathBuf,
        #[arg(long)]
        track: Option<String>,
    },
    /// Emit a Chroma-native playback manifest for a source.
    Manifest {
        file: PathBuf,
        #[arg(long, default_value_t = 4_000)]
        target_ms: u64,
        #[arg(long, default_value_t = true)]
        include_audio: bool,
    },
    /// Write a native compressed chunk payload and emit its manifest.
    ExtractChunk {
        input: PathBuf,
        output: PathBuf,
        #[arg(long)]
        track: Option<String>,
        #[arg(long, default_value_t = 0)]
        chunk_index: u32,
        #[arg(long, default_value_t = 4_000)]
        target_ms: u64,
    },
    /// Write a contiguous window of native compressed MP4/MOV chunks into a cache directory.
    ExtractWindow {
        input: PathBuf,
        output_dir: PathBuf,
        #[arg(long)]
        track: Option<String>,
        #[arg(long, default_value_t = 0)]
        start_chunk: u32,
        #[arg(long, default_value_t = 8)]
        chunk_count: u32,
        #[arg(long, default_value_t = 4_000)]
        target_ms: u64,
    },
    /// Emit AVC/H.264 NAL-unit layout for a native MP4/MOV chunk.
    H264Nalus {
        input: PathBuf,
        #[arg(long)]
        track: Option<String>,
        #[arg(long, default_value_t = 0)]
        chunk_index: u32,
        #[arg(long, default_value_t = 4_000)]
        target_ms: u64,
    },
    /// Write an Annex-B H.264 payload for a native MP4/MOV chunk.
    H264AnnexB {
        input: PathBuf,
        output: PathBuf,
        #[arg(long)]
        track: Option<String>,
        #[arg(long, default_value_t = 0)]
        chunk_index: u32,
        #[arg(long, default_value_t = 4_000)]
        target_ms: u64,
    },
    /// Write an Annex-B HEVC payload for a native MP4/MOV chunk.
    HevcAnnexB {
        input: PathBuf,
        output: PathBuf,
        #[arg(long)]
        track: Option<String>,
        #[arg(long, default_value_t = 0)]
        chunk_index: u32,
        #[arg(long, default_value_t = 4_000)]
        target_ms: u64,
    },
    /// Write an ADTS-framed AAC payload for a native MP4/MOV chunk.
    AacAdts {
        input: PathBuf,
        output: PathBuf,
        #[arg(long)]
        track: Option<String>,
        #[arg(long, default_value_t = 0)]
        chunk_index: u32,
        #[arg(long, default_value_t = 4_000)]
        target_ms: u64,
    },
    /// Emit platform encoder capabilities.
    EncoderProbe,
    /// Emit platform decoder backend capabilities.
    DecoderProbe,
    /// Decode one native packet chunk to BGRA and emit compact decoder health stats.
    DecodeChunk {
        input: PathBuf,
        #[arg(long)]
        track: Option<String>,
        #[arg(long, default_value_t = 0)]
        chunk_index: u32,
        #[arg(long, default_value_t = 4_000)]
        target_ms: u64,
    },
    /// Decode one native audio packet chunk to PCM and emit compact bridge health stats.
    DecodeAudioChunk {
        input: PathBuf,
        #[arg(long)]
        track: Option<String>,
        #[arg(long, default_value_t = 0)]
        chunk_index: u32,
        #[arg(long, default_value_t = 4_000)]
        target_ms: u64,
        #[arg(long, default_value_t = false)]
        probe_only: bool,
    },
    /// Decode one DTS audio chunk and encode it to bridge E-AC-3.
    BridgeAudioChunk {
        input: PathBuf,
        #[arg(long)]
        track: Option<String>,
        #[arg(long, default_value_t = 0)]
        chunk_index: u32,
        #[arg(long, default_value_t = 4_000)]
        target_ms: u64,
        #[arg(long, default_value_t = 640_000)]
        bitrate: u32,
    },
    /// Write a native transcoded fMP4 init segment for Matroska HLS playback.
    TranscodeFmp4Init {
        input: PathBuf,
        output: PathBuf,
        #[arg(long)]
        video_track: Option<String>,
        #[arg(long)]
        audio_track: Option<String>,
        #[arg(long, default_value_t = 4_000)]
        segment_ms: u64,
        #[arg(long, default_value_t = 16_000_000)]
        video_bitrate: u32,
        #[arg(long, default_value_t = 640_000)]
        audio_bitrate: u32,
        #[arg(long, value_enum, default_value_t = NativeFmp4VideoModeArg::Copy)]
        video_mode: NativeFmp4VideoModeArg,
    },
    /// Write one native transcoded fMP4 media segment for Matroska HLS playback.
    TranscodeFmp4Segment {
        input: PathBuf,
        output: PathBuf,
        #[arg(long)]
        index: u32,
        #[arg(long)]
        video_track: Option<String>,
        #[arg(long)]
        audio_track: Option<String>,
        #[arg(long, default_value_t = 4_000)]
        segment_ms: u64,
        #[arg(long, default_value_t = 16_000_000)]
        video_bitrate: u32,
        #[arg(long, default_value_t = 640_000)]
        audio_bitrate: u32,
        #[arg(long, value_enum, default_value_t = NativeFmp4VideoModeArg::Copy)]
        video_mode: NativeFmp4VideoModeArg,
    },
    /// Write native transcoded fMP4 init and media segment from one pass.
    TranscodeFmp4Start {
        input: PathBuf,
        init_output: PathBuf,
        segment_output: PathBuf,
        #[arg(long)]
        index: u32,
        #[arg(long)]
        video_track: Option<String>,
        #[arg(long)]
        audio_track: Option<String>,
        #[arg(long, default_value_t = 4_000)]
        segment_ms: u64,
        #[arg(long, default_value_t = 16_000_000)]
        video_bitrate: u32,
        #[arg(long, default_value_t = 640_000)]
        audio_bitrate: u32,
        #[arg(long, value_enum, default_value_t = NativeFmp4VideoModeArg::Copy)]
        video_mode: NativeFmp4VideoModeArg,
    },
    /// Warm the selected encoder backend.
    Warmup,
    /// Package a source into native HLS VOD output.
    Hls {
        input: PathBuf,
        output_dir: PathBuf,
        #[arg(long)]
        audio_track: Option<String>,
        #[arg(long, default_value_t = 4_000)]
        segment_ms: u64,
    },
    /// Package an MP4 source into native fMP4/CMAF HLS VOD output.
    HlsFmp4 {
        input: PathBuf,
        output_dir: PathBuf,
        #[arg(long)]
        audio_track: Option<String>,
        #[arg(long, default_value_t = 4_000)]
        segment_ms: u64,
    },
    /// Write a native fMP4/CMAF HLS init segment.
    HlsFmp4Init {
        input: PathBuf,
        output: PathBuf,
        #[arg(long)]
        audio_track: Option<String>,
        #[arg(long, default_value_t = 4_000)]
        segment_ms: u64,
    },
    /// Write a native fMP4/CMAF HLS init segment from a stored playback manifest.
    HlsFmp4InitFromManifest {
        manifest: PathBuf,
        output: PathBuf,
        #[arg(long)]
        audio_track: Option<String>,
        #[arg(long)]
        width: Option<u16>,
        #[arg(long)]
        height: Option<u16>,
    },
    /// Write one native fMP4/CMAF HLS media segment by index.
    HlsFmp4Segment {
        input: PathBuf,
        output: PathBuf,
        #[arg(long)]
        index: usize,
        #[arg(long)]
        audio_track: Option<String>,
        #[arg(long, default_value_t = 4_000)]
        segment_ms: u64,
    },
    /// Write one native fMP4/CMAF HLS media segment for an explicit source time window.
    HlsFmp4SegmentWindow {
        input: PathBuf,
        output: PathBuf,
        #[arg(long)]
        index: usize,
        #[arg(long)]
        start_ms: u64,
        #[arg(long)]
        end_ms: u64,
        #[arg(long)]
        audio_track: Option<String>,
    },
    /// Write a contiguous run of native fMP4/CMAF HLS segments by index.
    HlsFmp4Segments {
        input: PathBuf,
        output_dir: PathBuf,
        #[arg(long)]
        start: usize,
        #[arg(long)]
        count: usize,
        #[arg(long)]
        audio_track: Option<String>,
        #[arg(long, default_value_t = 4_000)]
        segment_ms: u64,
    },
    /// Emit native HLS VOD playlists and segment plan without writing segments.
    HlsPlan {
        input: PathBuf,
        #[arg(long)]
        audio_track: Option<String>,
        #[arg(long, default_value_t = 4_000)]
        segment_ms: u64,
    },
    /// Write one native HLS VOD segment by index.
    HlsSegment {
        input: PathBuf,
        output: PathBuf,
        #[arg(long)]
        index: usize,
        #[arg(long)]
        audio_track: Option<String>,
        #[arg(long, default_value_t = 4_000)]
        segment_ms: u64,
    },
    /// Write a contiguous run of native HLS VOD segments by index.
    HlsSegments {
        input: PathBuf,
        output_dir: PathBuf,
        #[arg(long)]
        start: usize,
        #[arg(long)]
        count: usize,
        #[arg(long)]
        audio_track: Option<String>,
        #[arg(long, default_value_t = 4_000)]
        segment_ms: u64,
    },
    /// Remux a source into faststart MP4.
    RemuxMp4 { input: PathBuf, output: PathBuf },
}

#[derive(Debug, Clone, Copy, clap::ValueEnum)]
enum NativeFmp4VideoModeArg {
    Copy,
    H264,
}

impl From<NativeFmp4VideoModeArg> for NativeFmp4VideoMode {
    fn from(value: NativeFmp4VideoModeArg) -> Self {
        match value {
            NativeFmp4VideoModeArg::Copy => NativeFmp4VideoMode::Copy,
            NativeFmp4VideoModeArg::H264 => NativeFmp4VideoMode::H264,
        }
    }
}

#[derive(Debug, Clone, Copy, clap::ValueEnum)]
enum TargetArg {
    NativeChroma,
    Browser,
    AppleNative,
}

#[derive(Debug, Serialize)]
struct ExtractedWindowChunk {
    track_id: String,
    chunk_index: u32,
    metadata_path: PathBuf,
    payload_path: PathBuf,
    byte_count: u64,
}

#[derive(Debug, Serialize)]
struct ExtractedWindow {
    track_id: Option<String>,
    start_chunk: u32,
    requested_chunk_count: u32,
    chunks: Vec<ExtractedWindowChunk>,
}

/// Runs the `chroma-engine` command-line interface.
pub fn run() -> Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(tracing_subscriber::EnvFilter::from_default_env())
        .with_writer(std::io::stderr)
        .init();

    let cli = Cli::parse();
    match cli.command {
        Command::Probe { file } => {
            let probe = probe_media_source(&file)?;
            println!("{}", serde_json::to_string_pretty(&probe)?);
        }
        Command::Plan {
            file,
            target,
            all_audio,
            include_subtitles,
        } => {
            let probe = probe_media_source(&file)?;
            let plan = plan_playback(
                &probe,
                PlaybackConstraints {
                    target: target.into(),
                    audio_selection: if all_audio {
                        AudioSelection::All
                    } else {
                        AudioSelection::Primary
                    },
                    include_subtitles,
                    ..PlaybackConstraints::default()
                },
            );
            println!("{}", serde_json::to_string_pretty(&plan)?);
        }
        Command::TranscodePlan {
            file,
            target,
            audio_track,
            segment_ms,
            force_video_transcode,
        } => {
            let probe = probe_media_source(&file)?;
            let plan = plan_hls_transcode(
                &probe,
                HlsTranscodeRequest {
                    target: target.into(),
                    audio_track_id: audio_track,
                    segment_target_ms: segment_ms,
                    force_video_transcode,
                },
            );
            println!("{}", serde_json::to_string_pretty(&plan)?);
        }
        Command::Chunks {
            file,
            track,
            target_ms,
        } => {
            let source = MappedMediaFile::open(&file)?;
            let bytes = source.as_ref();
            let plan = if looks_like_mp4(bytes) {
                parse_mp4_chunk_plan(bytes, track.as_deref(), target_ms)
            } else if looks_like_ebml(bytes) {
                parse_matroska_chunk_plan(bytes, track.as_deref(), target_ms)
            } else {
                bail!("native packet chunking currently supports MP4/MOV and Matroska/WebM");
            }
            .ok_or_else(|| anyhow::anyhow!("no matching packet-indexed track found"))?;
            println!("{}", serde_json::to_string_pretty(&plan)?);
        }
        Command::CodecConfig { file, track } => {
            let source = MappedMediaFile::open(&file)?;
            let bytes = source.as_ref();
            if !looks_like_mp4(bytes) {
                bail!("codec-config currently supports MP4/MOV sample descriptions");
            }
            let config = parse_mp4_codec_config(bytes, track.as_deref())
                .ok_or_else(|| anyhow::anyhow!("no matching MP4 codec config found"))?;
            println!("{}", serde_json::to_string_pretty(&config)?);
        }
        Command::Manifest {
            file,
            target_ms,
            include_audio,
        } => {
            let source = MappedMediaFile::open(&file)?;
            let bytes = source.as_ref();
            let manifest = if looks_like_mp4(bytes) {
                build_mp4_playback_manifest(
                    bytes,
                    &file,
                    Mp4ManifestOptions {
                        chunk_target_ms: target_ms,
                        include_audio,
                    },
                )
            } else if looks_like_ebml(bytes) {
                build_matroska_playback_manifest(
                    bytes,
                    &file,
                    MatroskaManifestOptions {
                        chunk_target_ms: target_ms,
                        include_audio,
                    },
                )
            } else {
                None
            }
            .ok_or_else(|| anyhow::anyhow!("could not build native playback manifest"))?;
            println!("{}", serde_json::to_string_pretty(&manifest)?);
        }
        Command::ExtractChunk {
            input,
            output,
            track,
            chunk_index,
            target_ms,
        } => {
            let source = MappedMediaFile::open(&input)?;
            let bytes = source.as_ref();
            let (manifest, payload) = if looks_like_mp4(bytes) {
                extract_mp4_chunk(bytes, track.as_deref(), target_ms, chunk_index)?
            } else if looks_like_ebml(bytes) {
                crate::container::matroska::extract_chunk(
                    bytes,
                    track.as_deref(),
                    target_ms,
                    chunk_index,
                )?
            } else {
                bail!(
                    "native chunk extraction currently supports MP4/MOV and Matroska/WebM packet tables"
                );
            };
            publish_bytes(&output, &payload)?;
            println!("{}", serde_json::to_string_pretty(&manifest)?);
        }
        Command::ExtractWindow {
            input,
            output_dir,
            track,
            start_chunk,
            chunk_count,
            target_ms,
        } => {
            let source = MappedMediaFile::open(&input)?;
            let bytes = source.as_ref();
            let written = if looks_like_mp4(bytes) {
                extract_mp4_window(
                    bytes,
                    &output_dir,
                    track.as_deref(),
                    start_chunk,
                    chunk_count,
                    target_ms,
                )?
            } else if looks_like_ebml(bytes) {
                extract_matroska_window(
                    bytes,
                    &output_dir,
                    track.as_deref(),
                    start_chunk,
                    chunk_count,
                    target_ms,
                )?
            } else {
                bail!(
                    "native chunk window extraction currently supports MP4/MOV and Matroska/WebM packet tables"
                );
            };
            println!("{}", serde_json::to_string_pretty(&written)?);
        }
        Command::H264Nalus {
            input,
            track,
            chunk_index,
            target_ms,
        } => {
            let source = MappedMediaFile::open(&input)?;
            let bytes = source.as_ref();
            if !looks_like_mp4(bytes) {
                bail!("h264-nalus currently supports MP4/MOV packet tables");
            }
            let config = parse_mp4_codec_config(bytes, track.as_deref())
                .ok_or_else(|| anyhow::anyhow!("no matching MP4 codec config found"))?;
            if config.codec != "h264" {
                bail!("selected track is {}, not h264", config.codec);
            }
            let nalu_length_size = config
                .nalu_length_size
                .ok_or_else(|| anyhow::anyhow!("missing AVC NAL length size"))?;
            let (manifest, payload) =
                extract_mp4_chunk(bytes, Some(&config.track_id), target_ms, chunk_index)?;
            let nalus = parse_avc_chunk_nalus(&payload, &manifest.samples, nalu_length_size)?;
            println!(
                "{}",
                serde_json::to_string_pretty(&H264NalusOutput {
                    track_id: config.track_id,
                    chunk_index,
                    nalu_length_size,
                    nalus,
                })?
            );
        }
        Command::H264AnnexB {
            input,
            output,
            track,
            chunk_index,
            target_ms,
        } => {
            let source = MappedMediaFile::open(&input)?;
            let bytes = source.as_ref();
            if !looks_like_mp4(bytes) {
                bail!("h264-annex-b currently supports MP4/MOV packet tables");
            }
            let config = parse_mp4_codec_config(bytes, track.as_deref())
                .ok_or_else(|| anyhow::anyhow!("no matching MP4 codec config found"))?;
            if config.codec != "h264" {
                bail!("selected track is {}, not h264", config.codec);
            }
            let nalu_length_size = config
                .nalu_length_size
                .ok_or_else(|| anyhow::anyhow!("missing AVC NAL length size"))?;
            let (manifest, payload) =
                extract_mp4_chunk(bytes, Some(&config.track_id), target_ms, chunk_index)?;
            let annex_b = avc_chunk_to_annex_b(&payload, &manifest.samples, nalu_length_size)?;
            let byte_count = annex_b.len() as u64;
            publish_bytes(&output, &annex_b)?;
            println!(
                "{}",
                serde_json::to_string_pretty(&H264AnnexBOutput {
                    track_id: config.track_id,
                    chunk_index,
                    nalu_length_size,
                    byte_count,
                })?
            );
        }
        Command::HevcAnnexB {
            input,
            output,
            track,
            chunk_index,
            target_ms,
        } => {
            let source = MappedMediaFile::open(&input)?;
            let bytes = source.as_ref();
            if !looks_like_mp4(bytes) {
                bail!("hevc-annex-b currently supports MP4/MOV packet tables");
            }
            let config = parse_mp4_codec_config(bytes, track.as_deref())
                .ok_or_else(|| anyhow::anyhow!("no matching MP4 codec config found"))?;
            if config.codec != "hevc" {
                bail!("selected track is {}, not hevc", config.codec);
            }
            let nalu_length_size = config
                .nalu_length_size
                .ok_or_else(|| anyhow::anyhow!("missing HEVC NAL length size"))?;
            let (manifest, payload) =
                extract_mp4_chunk(bytes, Some(&config.track_id), target_ms, chunk_index)?;
            let annex_b = hevc_chunk_to_annex_b(&payload, &manifest.samples, nalu_length_size)?;
            let byte_count = annex_b.len() as u64;
            publish_bytes(&output, &annex_b)?;
            println!(
                "{}",
                serde_json::to_string_pretty(&HevcAnnexBOutput {
                    track_id: config.track_id,
                    chunk_index,
                    nalu_length_size,
                    byte_count,
                })?
            );
        }
        Command::AacAdts {
            input,
            output,
            track,
            chunk_index,
            target_ms,
        } => {
            let source = MappedMediaFile::open(&input)?;
            let bytes = source.as_ref();
            if !looks_like_mp4(bytes) {
                bail!("aac-adts currently supports MP4/MOV packet tables");
            }
            let config = parse_mp4_codec_config(bytes, track.as_deref())
                .ok_or_else(|| anyhow::anyhow!("no matching MP4 codec config found"))?;
            if config.codec != "aac" {
                bail!("selected track is {}, not aac", config.codec);
            }
            let asc_hex = config
                .description_hex
                .as_deref()
                .ok_or_else(|| anyhow::anyhow!("missing AAC AudioSpecificConfig"))?;
            let asc = hex_to_bytes(asc_hex)?;
            let aac_config = parse_audio_specific_config(&asc)?;
            let (manifest, payload) =
                extract_mp4_chunk(bytes, Some(&config.track_id), target_ms, chunk_index)?;
            let adts = aac_chunk_to_adts(&payload, &manifest.samples, aac_config)?;
            let byte_count = adts.len() as u64;
            publish_bytes(&output, &adts)?;
            println!(
                "{}",
                serde_json::to_string_pretty(&AacAdtsOutput {
                    track_id: config.track_id,
                    chunk_index,
                    sample_rate: aac_config.sample_rate,
                    channel_config: aac_config.channel_config,
                    byte_count,
                })?
            );
        }
        Command::EncoderProbe => {
            let probe = crate::platform::encoder_probe();
            println!("{}", serde_json::to_string_pretty(&probe)?);
        }
        Command::DecoderProbe => {
            let probe = crate::platform::decoder_backend_plan();
            println!("{}", serde_json::to_string_pretty(&probe)?);
        }
        Command::DecodeChunk {
            input,
            track,
            chunk_index,
            target_ms,
        } => {
            let source = MappedMediaFile::open(&input)?;
            let decoded = decode_chunk(source.as_ref(), track.as_deref(), chunk_index, target_ms)?;
            println!("{}", serde_json::to_string_pretty(&decoded)?);
        }
        Command::DecodeAudioChunk {
            input,
            track,
            chunk_index,
            target_ms,
            probe_only,
        } => {
            let source = MappedMediaFile::open(&input)?;
            let decoded = decode_audio_chunk(
                source.as_ref(),
                track.as_deref(),
                chunk_index,
                target_ms,
                probe_only,
            )?;
            println!("{}", serde_json::to_string_pretty(&decoded)?);
        }
        Command::BridgeAudioChunk {
            input,
            track,
            chunk_index,
            target_ms,
            bitrate,
        } => {
            let source = MappedMediaFile::open(&input)?;
            let bridge = bridge_audio_chunk(
                source.as_ref(),
                track.as_deref(),
                chunk_index,
                target_ms,
                bitrate,
            )?;
            println!("{}", serde_json::to_string_pretty(&bridge)?);
        }
        Command::TranscodeFmp4Init {
            input,
            output,
            video_track,
            audio_track,
            segment_ms,
            video_bitrate,
            audio_bitrate,
            video_mode,
        } => {
            let written = write_native_fmp4_transcode_init(
                &input,
                &output,
                NativeFmp4TranscodeOptions {
                    video_track_id: video_track,
                    audio_track_id: audio_track,
                    segment_ms,
                    video_bitrate,
                    audio_bitrate,
                    video_mode: video_mode.into(),
                },
            )?;
            println!("{}", serde_json::to_string_pretty(&written)?);
        }
        Command::TranscodeFmp4Segment {
            input,
            output,
            index,
            video_track,
            audio_track,
            segment_ms,
            video_bitrate,
            audio_bitrate,
            video_mode,
        } => {
            let written = write_native_fmp4_transcode_segment(
                &input,
                &output,
                index,
                NativeFmp4TranscodeOptions {
                    video_track_id: video_track,
                    audio_track_id: audio_track,
                    segment_ms,
                    video_bitrate,
                    audio_bitrate,
                    video_mode: video_mode.into(),
                },
            )?;
            println!("{}", serde_json::to_string_pretty(&written)?);
        }
        Command::TranscodeFmp4Start {
            input,
            init_output,
            segment_output,
            index,
            video_track,
            audio_track,
            segment_ms,
            video_bitrate,
            audio_bitrate,
            video_mode,
        } => {
            let written = write_native_fmp4_transcode_start(
                &input,
                &init_output,
                &segment_output,
                index,
                NativeFmp4TranscodeOptions {
                    video_track_id: video_track,
                    audio_track_id: audio_track,
                    segment_ms,
                    video_bitrate,
                    audio_bitrate,
                    video_mode: video_mode.into(),
                },
            )?;
            println!("{}", serde_json::to_string_pretty(&written)?);
        }
        Command::Warmup => {
            crate::platform::warmup()?;
            println!("{}", serde_json::json!({ "ok": true }));
        }
        Command::Hls {
            input,
            output_dir,
            audio_track,
            segment_ms,
        } => hls::run_hls(input, output_dir, audio_track, segment_ms)?,
        Command::HlsFmp4 {
            input,
            output_dir,
            audio_track,
            segment_ms,
        } => hls::run_hls_fmp4(input, output_dir, audio_track, segment_ms)?,
        Command::HlsFmp4Init {
            input,
            output,
            audio_track,
            segment_ms,
        } => hls::run_hls_fmp4_init(input, output, audio_track, segment_ms)?,
        Command::HlsFmp4InitFromManifest {
            manifest,
            output,
            audio_track,
            width,
            height,
        } => hls::run_hls_fmp4_init_from_manifest(manifest, output, audio_track, width, height)?,
        Command::HlsFmp4Segment {
            input,
            output,
            index,
            audio_track,
            segment_ms,
        } => hls::run_hls_fmp4_segment(input, output, index, audio_track, segment_ms)?,
        Command::HlsFmp4SegmentWindow {
            input,
            output,
            index,
            start_ms,
            end_ms,
            audio_track,
        } => hls::run_hls_fmp4_segment_window(input, output, index, start_ms, end_ms, audio_track)?,
        Command::HlsFmp4Segments {
            input,
            output_dir,
            start,
            count,
            audio_track,
            segment_ms,
        } => hls::run_hls_fmp4_segments(input, output_dir, start, count, audio_track, segment_ms)?,
        Command::HlsPlan {
            input,
            audio_track,
            segment_ms,
        } => hls::run_hls_plan(input, audio_track, segment_ms)?,
        Command::HlsSegment {
            input,
            output,
            index,
            audio_track,
            segment_ms,
        } => hls::run_hls_segment(input, output, index, audio_track, segment_ms)?,
        Command::HlsSegments {
            input,
            output_dir,
            start,
            count,
            audio_track,
            segment_ms,
        } => hls::run_hls_segments(input, output_dir, start, count, audio_track, segment_ms)?,
        Command::RemuxMp4 { input, output } => {
            crate::remux::remux_mp4(&input, &output)?;
            println!("{}", serde_json::json!({ "ok": true }));
        }
    }

    Ok(())
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
struct H264NalusOutput {
    track_id: String,
    chunk_index: u32,
    nalu_length_size: u8,
    nalus: Vec<crate::codec::h264::AvcNalUnit>,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
struct H264AnnexBOutput {
    track_id: String,
    chunk_index: u32,
    nalu_length_size: u8,
    byte_count: u64,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
struct HevcAnnexBOutput {
    track_id: String,
    chunk_index: u32,
    nalu_length_size: u8,
    byte_count: u64,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
struct AacAdtsOutput {
    track_id: String,
    chunk_index: u32,
    sample_rate: u32,
    channel_config: u8,
    byte_count: u64,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
struct DecodeChunkOutput {
    track_id: String,
    chunk_index: u32,
    codec: VideoCodec,
    decoder: String,
    packet_count: u32,
    decoded_frame_count: usize,
    width: u32,
    height: u32,
    time_scale: TimeScale,
    first_pts: Option<TimePoint>,
    last_pts: Option<TimePoint>,
    first_duration: Option<TimeDelta>,
    keyframe_aligned: bool,
    decoded_bytes: u64,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
struct DecodeAudioChunkOutput {
    track_id: String,
    chunk_index: u32,
    codec: AudioDecodeCodec,
    decoder: String,
    packet_count: usize,
    dts_core_frame_count: usize,
    decoded_pcm_frame_count: usize,
    sample_rate: u32,
    channels: u32,
    time_scale: TimeScale,
    first_pts: Option<TimePoint>,
    last_pts: Option<TimePoint>,
    stable_format: bool,
    pcm_frames_per_channel: u64,
    decoded_samples: u64,
    decoded_bytes: u64,
    probe_only: bool,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
struct BridgeAudioChunkOutput {
    track_id: String,
    chunk_index: u32,
    source_codec: AudioDecodeCodec,
    decoder: String,
    encoder: String,
    packet_count: usize,
    decoded_pcm_frame_count: usize,
    encoded_frame_count: usize,
    sample_rate: u32,
    channels: u32,
    bitrate: u32,
    decoded_samples: u64,
    encoded_bytes: u64,
    first_pts: Option<TimePoint>,
    last_pts: Option<TimePoint>,
}

fn hex_to_bytes(hex: &str) -> Result<Vec<u8>> {
    let clean = hex.trim();
    if !clean.len().is_multiple_of(2) {
        bail!("hex string has odd length");
    }
    let mut out = Vec::with_capacity(clean.len() / 2);
    let bytes = clean.as_bytes();
    for idx in (0..bytes.len()).step_by(2) {
        let hi = hex_nibble(bytes[idx])?;
        let lo = hex_nibble(bytes[idx + 1])?;
        out.push((hi << 4) | lo);
    }
    Ok(out)
}

fn hex_nibble(byte: u8) -> Result<u8> {
    match byte {
        b'0'..=b'9' => Ok(byte - b'0'),
        b'a'..=b'f' => Ok(byte - b'a' + 10),
        b'A'..=b'F' => Ok(byte - b'A' + 10),
        _ => bail!("invalid hex byte"),
    }
}

fn decode_chunk(
    bytes: &[u8],
    requested_track_id: Option<&str>,
    chunk_index: u32,
    target_ms: u64,
) -> Result<DecodeChunkOutput> {
    if looks_like_mp4(bytes) {
        decode_mp4_chunk(bytes, requested_track_id, chunk_index, target_ms)
    } else if looks_like_ebml(bytes) {
        decode_matroska_chunk(bytes, requested_track_id, chunk_index, target_ms)
    } else {
        bail!("decode-chunk currently supports MP4/MOV and Matroska/WebM packet tables");
    }
}

fn decode_audio_chunk(
    bytes: &[u8],
    requested_track_id: Option<&str>,
    chunk_index: u32,
    target_ms: u64,
    probe_only: bool,
) -> Result<DecodeAudioChunkOutput> {
    if looks_like_ebml(bytes) {
        decode_matroska_audio_chunk(
            bytes,
            requested_track_id,
            chunk_index,
            target_ms,
            probe_only,
        )
    } else {
        bail!("decode-audio-chunk currently supports Matroska/WebM DTS packet tables");
    }
}

fn decode_matroska_audio_chunk(
    bytes: &[u8],
    requested_track_id: Option<&str>,
    chunk_index: u32,
    target_ms: u64,
    probe_only: bool,
) -> Result<DecodeAudioChunkOutput> {
    let meta = crate::container::matroska::parse_basic_metadata(bytes);
    let (track_id, track) = select_matroska_audio_track(&meta.tracks, requested_track_id)
        .ok_or_else(|| anyhow::anyhow!("no matching Matroska audio track found"))?;
    if track.codec != "dts" {
        bail!(
            "selected audio track {} uses {}, not DTS",
            track_id,
            track.codec
        );
    }
    let (manifest, payload) =
        crate::container::matroska::extract_chunk(bytes, Some(&track_id), target_ms, chunk_index)?;
    let time_scale = manifest
        .samples
        .first()
        .map(|sample| sample.pts.scale)
        .unwrap_or(TimeScale::MILLIS);
    let decode_input = build_audio_decode_input(
        AudioDecodeCodec::Dts,
        time_scale,
        &manifest.samples,
        &payload,
        true,
    )?;
    let probe = probe_dts_audio_bridge(&decode_input)?;
    if !probe_only {
        bail!("DTS decode is currently unsupported; use --probe-only for DTS packet inspection");
    }

    Ok(DecodeAudioChunkOutput {
        track_id,
        chunk_index: manifest.chunk.index,
        codec: AudioDecodeCodec::Dts,
        decoder: "dts-core-probe".to_string(),
        packet_count: probe.packet_count,
        dts_core_frame_count: probe.dts_core_frame_count,
        decoded_pcm_frame_count: 0,
        sample_rate: probe.pcm_format.sample_rate,
        channels: probe.pcm_format.channels,
        time_scale,
        first_pts: probe.first_pts,
        last_pts: probe.last_pts,
        stable_format: probe.stable_format,
        pcm_frames_per_channel: probe.pcm_frame_count,
        decoded_samples: 0,
        decoded_bytes: 0,
        probe_only,
    })
}

fn bridge_audio_chunk(
    bytes: &[u8],
    requested_track_id: Option<&str>,
    chunk_index: u32,
    target_ms: u64,
    _bitrate: u32,
) -> Result<BridgeAudioChunkOutput> {
    if !looks_like_ebml(bytes) {
        bail!("bridge-audio-chunk currently supports Matroska/WebM DTS packet tables");
    }
    let meta = crate::container::matroska::parse_basic_metadata(bytes);
    let (track_id, track) = select_matroska_audio_track(&meta.tracks, requested_track_id)
        .ok_or_else(|| anyhow::anyhow!("no matching Matroska audio track found"))?;
    if track.codec != "dts" {
        bail!(
            "selected audio track {} uses {}, not DTS",
            track_id,
            track.codec
        );
    }
    let (manifest, payload) =
        crate::container::matroska::extract_chunk(bytes, Some(&track_id), target_ms, chunk_index)?;
    let time_scale = manifest
        .samples
        .first()
        .map(|sample| sample.pts.scale)
        .unwrap_or(TimeScale::MILLIS);
    let decode_input = build_audio_decode_input(
        AudioDecodeCodec::Dts,
        time_scale,
        &manifest.samples,
        &payload,
        true,
    )?;
    let _probe = probe_dts_audio_bridge(&decode_input)?;
    bail!("DTS audio bridge is unsupported until Chroma Engine has a measured native DTS decoder")
}

fn decode_mp4_chunk(
    bytes: &[u8],
    requested_track_id: Option<&str>,
    chunk_index: u32,
    target_ms: u64,
) -> Result<DecodeChunkOutput> {
    let config = parse_mp4_codec_config(bytes, requested_track_id)
        .ok_or_else(|| anyhow::anyhow!("no matching MP4 video codec config found"))?;
    let codec = video_codec_from_label(&config.codec)?;
    let decoder_config = hex_to_bytes(
        config
            .description_hex
            .as_deref()
            .ok_or_else(|| anyhow::anyhow!("missing MP4 video decoder config"))?,
    )?;
    let (manifest, payload) =
        extract_mp4_chunk(bytes, Some(&config.track_id), target_ms, chunk_index)?;
    let meta = crate::container::mp4::parse_basic_metadata(bytes);
    let selected_video_index = config
        .track_id
        .strip_prefix('v')
        .and_then(|id| id.parse::<u32>().ok());
    let track = meta
        .tracks
        .iter()
        .find(|track| {
            track.kind == crate::container::mp4::Mp4TrackKind::Video
                && Some(track.index) == selected_video_index
        })
        .or_else(|| {
            meta.tracks
                .iter()
                .find(|track| track.kind == crate::container::mp4::Mp4TrackKind::Video)
        });
    let frame_rate = track.and_then(|track| track.frame_rate);
    decode_extracted_chunk(
        config.track_id,
        codec,
        decoder_config,
        manifest,
        &payload,
        RawVideoFormat {
            width: track.and_then(|track| track.width).unwrap_or(1920),
            height: track.and_then(|track| track.height).unwrap_or(1080),
            frame_rate_num: frame_rate
                .map(|rate| (rate * 1000.0).round() as u32)
                .filter(|rate| *rate > 0)
                .unwrap_or(24_000),
            frame_rate_den: 1000,
            pixel_format: RawVideoPixelFormat::Bgra,
        },
    )
}

fn decode_matroska_chunk(
    bytes: &[u8],
    requested_track_id: Option<&str>,
    chunk_index: u32,
    target_ms: u64,
) -> Result<DecodeChunkOutput> {
    let meta = crate::container::matroska::parse_basic_metadata(bytes);
    let (track_id, track) = select_matroska_video_track(&meta.tracks, requested_track_id)
        .ok_or_else(|| anyhow::anyhow!("no matching Matroska video track found"))?;
    let codec = video_codec_from_label(&track.codec)?;
    let decoder_config = track
        .codec_private
        .clone()
        .ok_or_else(|| anyhow::anyhow!("missing Matroska video decoder config"))?;
    let (manifest, payload) =
        crate::container::matroska::extract_chunk(bytes, Some(&track_id), target_ms, chunk_index)?;
    let frame_duration_ns = track.default_duration_ns.unwrap_or(41_666_667);
    decode_extracted_chunk(
        track_id,
        codec,
        decoder_config,
        manifest,
        &payload,
        RawVideoFormat {
            width: track.width.unwrap_or(1920),
            height: track.height.unwrap_or(1080),
            frame_rate_num: u32::try_from(1_000_000_000_u64 / frame_duration_ns.max(1))
                .unwrap_or(24)
                .max(1),
            frame_rate_den: 1,
            pixel_format: RawVideoPixelFormat::Bgra,
        },
    )
}

fn decode_extracted_chunk(
    track_id: String,
    codec: VideoCodec,
    decoder_config: Vec<u8>,
    manifest: ExtractedChunk,
    payload: &[u8],
    output_format: RawVideoFormat,
) -> Result<DecodeChunkOutput> {
    let decode_input = build_video_decode_input(
        codec,
        manifest
            .samples
            .first()
            .map(|sample| sample.pts.scale)
            .unwrap_or(TimeScale::MILLIS),
        Some(&decoder_config),
        &manifest.samples,
        payload,
        true,
    )?;
    let decoded = decode_videotoolbox_bgra_frames(&decode_input, output_format)?;
    Ok(DecodeChunkOutput {
        track_id,
        chunk_index: manifest.chunk.index,
        codec,
        decoder: decoded.stream.decoder,
        packet_count: manifest.packet_count,
        decoded_frame_count: decoded.frames.len(),
        width: decoded.stream.format.width,
        height: decoded.stream.format.height,
        time_scale: decode_input.time_scale,
        first_pts: decoded.frames.first().map(|frame| frame.pts),
        last_pts: decoded.frames.last().map(|frame| frame.pts),
        first_duration: decoded.frames.first().map(|frame| frame.duration),
        keyframe_aligned: manifest.chunk.key_aligned,
        decoded_bytes: decoded
            .frames
            .iter()
            .map(|frame| frame.pixels.len() as u64)
            .sum(),
    })
}

fn video_codec_from_label(codec: &str) -> Result<VideoCodec> {
    match codec {
        "h264" => Ok(VideoCodec::H264),
        "hevc" => Ok(VideoCodec::Hevc),
        _ => bail!("selected video track codec {codec} is not supported by native decode"),
    }
}

fn select_matroska_video_track<'a>(
    tracks: &'a [crate::container::matroska::MatroskaTrack],
    requested_track_id: Option<&str>,
) -> Option<(String, &'a crate::container::matroska::MatroskaTrack)> {
    let mut video_index = 0_u32;
    for track in tracks {
        if track.kind != crate::container::matroska::MatroskaTrackKind::Video {
            continue;
        }
        let track_id = format!("v{video_index}");
        video_index += 1;
        if requested_track_id
            .map(|requested| requested == track_id)
            .unwrap_or(true)
        {
            return Some((track_id, track));
        }
    }
    None
}

fn select_matroska_audio_track<'a>(
    tracks: &'a [crate::container::matroska::MatroskaTrack],
    requested_track_id: Option<&str>,
) -> Option<(String, &'a crate::container::matroska::MatroskaTrack)> {
    let mut fallback = None;
    let mut audio_index = 0_u32;
    for track in tracks {
        if track.kind != crate::container::matroska::MatroskaTrackKind::Audio {
            continue;
        }
        let track_id = format!("a{audio_index}");
        audio_index += 1;
        if fallback.is_none() {
            fallback = Some((track_id.clone(), track));
        }
        if requested_track_id
            .map(|requested| requested == track_id)
            .unwrap_or(track.default)
        {
            return Some((track_id, track));
        }
    }
    if requested_track_id.is_some() {
        None
    } else {
        fallback
    }
}

fn extract_mp4_window(
    bytes: &[u8],
    output_dir: &Path,
    requested_track_id: Option<&str>,
    start_chunk: u32,
    chunk_count: u32,
    target_ms: u64,
) -> Result<ExtractedWindow> {
    std::fs::create_dir_all(output_dir)?;
    let mut chunks = Vec::new();
    let end_chunk = start_chunk.saturating_add(chunk_count);
    for chunk_index in start_chunk..end_chunk {
        let (manifest, payload) =
            match extract_mp4_chunk(bytes, requested_track_id, target_ms, chunk_index) {
                Ok(chunk) => chunk,
                Err(_) => break,
            };
        let safe_track = safe_cache_component(&manifest.track_id);
        let payload_path = output_dir.join(format!("{safe_track}-{chunk_index}.bin"));
        let metadata_path = output_dir.join(format!("{safe_track}-{chunk_index}.json"));
        write_atomic(&payload_path, &payload)?;
        write_atomic(&metadata_path, serde_json::to_string(&manifest)?.as_bytes())?;
        chunks.push(ExtractedWindowChunk {
            track_id: manifest.track_id,
            chunk_index,
            metadata_path,
            payload_path,
            byte_count: payload.len() as u64,
        });
    }
    Ok(ExtractedWindow {
        track_id: requested_track_id.map(str::to_string),
        start_chunk,
        requested_chunk_count: chunk_count,
        chunks,
    })
}

fn extract_matroska_window(
    bytes: &[u8],
    output_dir: &Path,
    requested_track_id: Option<&str>,
    start_chunk: u32,
    chunk_count: u32,
    target_ms: u64,
) -> Result<ExtractedWindow> {
    std::fs::create_dir_all(output_dir)?;
    let mut chunks = Vec::new();
    for (manifest, payload) in crate::container::matroska::extract_window(
        bytes,
        requested_track_id,
        target_ms,
        start_chunk,
        chunk_count,
    )? {
        let chunk_index = manifest.chunk.index;
        let safe_track = safe_cache_component(&manifest.track_id);
        let payload_path = output_dir.join(format!("{safe_track}-{chunk_index}.bin"));
        let metadata_path = output_dir.join(format!("{safe_track}-{chunk_index}.json"));
        write_atomic(&payload_path, &payload)?;
        write_atomic(&metadata_path, serde_json::to_string(&manifest)?.as_bytes())?;
        chunks.push(ExtractedWindowChunk {
            track_id: manifest.track_id,
            chunk_index,
            metadata_path,
            payload_path,
            byte_count: payload.len() as u64,
        });
    }
    Ok(ExtractedWindow {
        track_id: requested_track_id.map(str::to_string),
        start_chunk,
        requested_chunk_count: chunk_count,
        chunks,
    })
}

fn safe_cache_component(value: &str) -> String {
    value
        .chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() || c == '_' || c == '-' {
                c
            } else {
                '_'
            }
        })
        .collect()
}

fn write_atomic(path: &Path, bytes: &[u8]) -> Result<()> {
    publish_bytes(path, bytes)?;
    Ok(())
}

impl From<TargetArg> for PlaybackTarget {
    fn from(value: TargetArg) -> Self {
        match value {
            TargetArg::NativeChroma => Self::NativeChroma,
            TargetArg::Browser => Self::Browser,
            TargetArg::AppleNative => Self::AppleNative,
        }
    }
}
