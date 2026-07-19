use std::fs::File;
use std::io::{Read, Seek, SeekFrom};
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};
use thiserror::Error;

use crate::container::{mp4, sniff_container, ContainerKind};

#[derive(Debug, Error)]
pub enum ProbeError {
    #[error("file_not_found")]
    FileNotFound,
    #[error("open_failed: {0}")]
    OpenFailed(String),
    #[error("read_failed: {0}")]
    ReadFailed(String),
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct SourceProbe {
    pub file_path: PathBuf,
    pub duration_ms: Option<u64>,
    pub container: String,
    pub container_direct_play: bool,
    pub video_streams: Vec<SourceVideoStream>,
    pub audio_streams: Vec<SourceAudioStream>,
    pub subtitle_streams: Vec<SourceSubtitleStream>,
    pub chapters: Vec<SourceChapter>,
    pub attachment_count: u32,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct SourceVideoStream {
    pub index: u32,
    pub codec: String,
    pub width: Option<u32>,
    pub height: Option<u32>,
    pub frame_rate: Option<f64>,
    pub profile: Option<String>,
    pub bitrate: Option<u64>,
    pub pixel_format: Option<String>,
    pub high_bit_depth: bool,
    pub color_primaries: Option<String>,
    pub color_transfer: Option<String>,
    pub color_space: Option<String>,
    pub color_range: Option<String>,
    pub dynamic_range: SourceVideoDynamicRange,
    pub dolby_vision: bool,
    pub hdr10_plus: bool,
    pub duration_ms: Option<u64>,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub enum SourceVideoDynamicRange {
    Sdr,
    Hdr10,
    Hdr10Plus,
    Hlg,
    DolbyVision,
    Unknown,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct SourceAudioStream {
    pub index: u32,
    pub codec: String,
    pub profile: Option<String>,
    pub channels: u32,
    pub bitrate: Option<u64>,
    pub language: Option<String>,
    pub title: Option<String>,
    pub default: bool,
    pub forced: bool,
    pub atmos: bool,
    pub atmos_joc: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct SourceSubtitleStream {
    pub index: u32,
    pub codec: String,
    pub kind: SourceSubtitleKind,
    pub language: Option<String>,
    pub title: Option<String>,
    pub default: bool,
    pub forced: bool,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub enum SourceSubtitleKind {
    Text,
    Bitmap,
    Unknown,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct SourceChapter {
    pub start_seconds: f64,
    pub end_seconds: f64,
    pub title: Option<String>,
}

pub fn probe_media_source(path: &Path) -> Result<SourceProbe, ProbeError> {
    if !path.exists() {
        return Err(ProbeError::FileNotFound);
    }

    let mut file = File::open(path).map_err(|err| ProbeError::OpenFailed(err.to_string()))?;
    let mut head = [0_u8; 4096];
    let n = file
        .read(&mut head)
        .map_err(|err| ProbeError::ReadFailed(err.to_string()))?;
    file.seek(SeekFrom::Start(0))
        .map_err(|err| ProbeError::ReadFailed(err.to_string()))?;

    let container = sniff_container(&head[..n]);
    let duration_ms = if matches!(container, ContainerKind::Mp4 | ContainerKind::Mov) {
        let bytes = std::fs::read(path).map_err(|err| ProbeError::ReadFailed(err.to_string()))?;
        mp4::parse_basic_metadata(&bytes).duration_ms
    } else {
        None
    };

    Ok(SourceProbe {
        file_path: path.to_path_buf(),
        duration_ms,
        container: container.public_name().to_string(),
        container_direct_play: container.direct_play(),
        video_streams: Vec::new(),
        audio_streams: Vec::new(),
        subtitle_streams: Vec::new(),
        chapters: Vec::new(),
        attachment_count: 0,
    })
}

pub fn classify_text_subtitle_codec(codec: &str) -> SourceSubtitleKind {
    match codec.trim().to_ascii_lowercase().as_str() {
        "subrip" | "ass" | "ssa" | "mov_text" | "webvtt" | "text" | "subviewer" | "microdvd" => {
            SourceSubtitleKind::Text
        }
        "hdmv_pgs_subtitle" | "dvd_subtitle" | "dvb_subtitle" | "dvb_teletext" | "xsub" => {
            SourceSubtitleKind::Bitmap
        }
        _ => SourceSubtitleKind::Unknown,
    }
}

pub fn normalize_audio_codec(codec: &str) -> String {
    match codec.trim().to_ascii_lowercase().as_str() {
        "dca" => "dts".to_string(),
        other => other.to_string(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn normalizes_dca_to_dts() {
        assert_eq!(normalize_audio_codec("DCA"), "dts");
    }

    #[test]
    fn classifies_subtitles() {
        assert_eq!(classify_text_subtitle_codec("ass"), SourceSubtitleKind::Text);
        assert_eq!(
            classify_text_subtitle_codec("hdmv_pgs_subtitle"),
            SourceSubtitleKind::Bitmap
        );
        assert_eq!(classify_text_subtitle_codec("weird"), SourceSubtitleKind::Unknown);
    }

    #[test]
    fn exposes_container_name_from_sniff() {
        assert_eq!(ContainerKind::Matroska.public_name(), "mkv");
    }
}
