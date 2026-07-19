use std::path::PathBuf;

use anyhow::Result;
use chroma_engine::probe::probe_media_source;
use chroma_engine::session::{plan_playback, AudioSelection, PlaybackConstraints, PlaybackTarget};
use clap::{Parser, Subcommand};

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

impl From<TargetArg> for PlaybackTarget {
    fn from(value: TargetArg) -> Self {
        match value {
            TargetArg::NativeChroma => Self::NativeChroma,
            TargetArg::Browser => Self::Browser,
            TargetArg::AppleNative => Self::AppleNative,
        }
    }
}
