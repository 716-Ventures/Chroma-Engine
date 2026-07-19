use std::path::PathBuf;

use anyhow::{bail, Result};
use chroma_engine::codec::h264::parse_avc_chunk_nalus;
use chroma_engine::container::{
    matroska::{looks_like_ebml, parse_chunk_plan as parse_matroska_chunk_plan},
    mp4::{
        extract_chunk as extract_mp4_chunk, looks_like_mp4,
        parse_chunk_plan as parse_mp4_chunk_plan, parse_codec_config as parse_mp4_codec_config,
    },
};
use chroma_engine::playback_manifest::{build_mp4_playback_manifest, Mp4ManifestOptions};
use chroma_engine::probe::probe_media_source;
use chroma_engine::session::{plan_playback, AudioSelection, PlaybackConstraints, PlaybackTarget};
use clap::{Parser, Subcommand};
use memmap2::Mmap;
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
    /// Emit platform encoder capabilities.
    EncoderProbe,
    /// Warm the selected encoder backend.
    Warmup,
    /// Remux a source into faststart MP4.
    RemuxMp4 { input: PathBuf, output: PathBuf },
}

#[derive(Debug, Clone, Copy, clap::ValueEnum)]
enum TargetArg {
    NativeChroma,
    Browser,
    AppleNative,
}

fn main() -> Result<()> {
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
        Command::Chunks {
            file,
            track,
            target_ms,
        } => {
            let source = std::fs::File::open(&file)?;
            let bytes = unsafe { Mmap::map(&source)? };
            let plan = if looks_like_mp4(&bytes) {
                parse_mp4_chunk_plan(&bytes, track.as_deref(), target_ms)
            } else if looks_like_ebml(&bytes) {
                parse_matroska_chunk_plan(&bytes, track.as_deref(), target_ms)
            } else {
                bail!("native packet chunking currently supports MP4/MOV and Matroska/WebM");
            }
            .ok_or_else(|| anyhow::anyhow!("no matching packet-indexed track found"))?;
            println!("{}", serde_json::to_string_pretty(&plan)?);
        }
        Command::CodecConfig { file, track } => {
            let source = std::fs::File::open(&file)?;
            let bytes = unsafe { Mmap::map(&source)? };
            if !looks_like_mp4(&bytes) {
                bail!("codec-config currently supports MP4/MOV sample descriptions");
            }
            let config = parse_mp4_codec_config(&bytes, track.as_deref())
                .ok_or_else(|| anyhow::anyhow!("no matching MP4 codec config found"))?;
            println!("{}", serde_json::to_string_pretty(&config)?);
        }
        Command::Manifest {
            file,
            target_ms,
            include_audio,
        } => {
            let source = std::fs::File::open(&file)?;
            let bytes = unsafe { Mmap::map(&source)? };
            if !looks_like_mp4(&bytes) {
                bail!("native playback manifest currently supports MP4/MOV");
            }
            let manifest = build_mp4_playback_manifest(
                &bytes,
                &file,
                Mp4ManifestOptions {
                    chunk_target_ms: target_ms,
                    include_primary_audio: include_audio,
                },
            )
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
            let source = std::fs::File::open(&input)?;
            let bytes = unsafe { Mmap::map(&source)? };
            if !looks_like_mp4(&bytes) {
                bail!("native chunk extraction currently supports MP4/MOV packet tables");
            }
            let (manifest, payload) =
                extract_mp4_chunk(&bytes, track.as_deref(), target_ms, chunk_index)?;
            std::fs::write(output, payload)?;
            println!("{}", serde_json::to_string_pretty(&manifest)?);
        }
        Command::H264Nalus {
            input,
            track,
            chunk_index,
            target_ms,
        } => {
            let source = std::fs::File::open(&input)?;
            let bytes = unsafe { Mmap::map(&source)? };
            if !looks_like_mp4(&bytes) {
                bail!("h264-nalus currently supports MP4/MOV packet tables");
            }
            let config = parse_mp4_codec_config(&bytes, track.as_deref())
                .ok_or_else(|| anyhow::anyhow!("no matching MP4 codec config found"))?;
            if config.codec != "h264" {
                bail!("selected track is {}, not h264", config.codec);
            }
            let nalu_length_size = config
                .nalu_length_size
                .ok_or_else(|| anyhow::anyhow!("missing AVC NAL length size"))?;
            let (manifest, payload) =
                extract_mp4_chunk(&bytes, Some(&config.track_id), target_ms, chunk_index)?;
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
        Command::EncoderProbe => {
            let probe = chroma_engine::platform::encoder_probe();
            println!("{}", serde_json::to_string_pretty(&probe)?);
        }
        Command::Warmup => {
            chroma_engine::platform::warmup()?;
            println!("{}", serde_json::json!({ "ok": true }));
        }
        Command::RemuxMp4 { input, output } => {
            chroma_engine::remux::remux_mp4(&input, &output)?;
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
    nalus: Vec<chroma_engine::codec::h264::AvcNalUnit>,
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
