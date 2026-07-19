use std::path::PathBuf;

use anyhow::Result;
use chroma_engine::probe::probe_media_source;
use clap::{Parser, Subcommand};

#[derive(Debug, Parser)]
#[command(name = "chroma-engine")]
#[command(about = "Narrow Rust media engine for ChromaServer")]
struct Cli {
    #[command(subcommand)]
    command: Command,
}

#[derive(Debug, Subcommand)]
enum Command {
    /// Emit GenusServer-compatible source probe JSON.
    Probe {
        file: PathBuf,
    },
    /// Emit platform encoder capabilities.
    EncoderProbe,
    /// Warm the selected encoder backend.
    Warmup,
    /// Remux a source into faststart MP4.
    RemuxMp4 {
        input: PathBuf,
        output: PathBuf,
    },
    /// Run a live fMP4 HLS session.
    Hls {
        input: PathBuf,
        work_dir: PathBuf,
        #[arg(long, default_value_t = 0.0)]
        anchor_seconds: f64,
        #[arg(long, default_value_t = 6.0)]
        segment_seconds: f64,
    },
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
        Command::Hls {
            input,
            work_dir,
            anchor_seconds,
            segment_seconds,
        } => {
            chroma_engine::hls::run_hls_session(&input, &work_dir, anchor_seconds, segment_seconds)?;
            println!("{}", serde_json::json!({ "ok": true }));
        }
    }

    Ok(())
}
