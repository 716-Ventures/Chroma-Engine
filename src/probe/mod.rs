use std::fs::File;
use std::io::{Read, Seek, SeekFrom};
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};
use thiserror::Error;

use crate::container::{matroska, mp4, sniff_container, ContainerKind};

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
    let mut duration_ms = None;
    let mut video_streams = Vec::new();
    let mut audio_streams = Vec::new();
    let mut subtitle_streams = Vec::new();
    let mut attachment_count = 0;

    if matches!(container, ContainerKind::Mp4 | ContainerKind::Mov) {
        let bytes = std::fs::read(path).map_err(|err| ProbeError::ReadFailed(err.to_string()))?;
        let meta = mp4::parse_basic_metadata(&bytes);
        duration_ms = meta.duration_ms;
        (video_streams, audio_streams, subtitle_streams) = source_streams_from_mp4(&meta);
    } else if matches!(container, ContainerKind::Matroska | ContainerKind::Webm) {
        let bytes = std::fs::read(path).map_err(|err| ProbeError::ReadFailed(err.to_string()))?;
        let meta = matroska::parse_basic_metadata(&bytes);
        duration_ms = meta.duration_ms;
        attachment_count = meta.attachment_count;
        (video_streams, audio_streams, subtitle_streams) = source_streams_from_matroska(&meta);
    }

    Ok(SourceProbe {
        file_path: path.to_path_buf(),
        duration_ms,
        container: container.public_name().to_string(),
        container_direct_play: container.direct_play(),
        video_streams,
        audio_streams,
        subtitle_streams,
        chapters: Vec::new(),
        attachment_count,
    })
}

fn source_streams_from_mp4(
    meta: &mp4::Mp4BasicMetadata,
) -> (
    Vec<SourceVideoStream>,
    Vec<SourceAudioStream>,
    Vec<SourceSubtitleStream>,
) {
    let mut videos = Vec::new();
    let mut audios = Vec::new();
    let mut subtitles = Vec::new();

    for track in &meta.tracks {
        match track.kind {
            mp4::Mp4TrackKind::Video => videos.push(SourceVideoStream {
                index: track.index,
                codec: track.codec.clone(),
                width: track.width,
                height: track.height,
                frame_rate: None,
                profile: None,
                bitrate: None,
                pixel_format: None,
                high_bit_depth: false,
                color_primaries: None,
                color_transfer: None,
                color_space: None,
                color_range: None,
                dynamic_range: SourceVideoDynamicRange::Unknown,
                dolby_vision: false,
                hdr10_plus: false,
                duration_ms: track.duration_ms,
            }),
            mp4::Mp4TrackKind::Audio => audios.push(SourceAudioStream {
                index: track.index,
                codec: normalize_audio_codec(&track.codec),
                profile: None,
                channels: track.channels.unwrap_or(2),
                bitrate: None,
                language: None,
                title: None,
                default: false,
                forced: false,
                atmos: false,
                atmos_joc: false,
            }),
            mp4::Mp4TrackKind::Subtitle => subtitles.push(SourceSubtitleStream {
                index: track.index,
                codec: track.codec.clone(),
                kind: classify_text_subtitle_codec(&track.codec),
                language: None,
                title: None,
                default: false,
                forced: false,
            }),
            mp4::Mp4TrackKind::Unknown => {}
        }
    }

    (videos, audios, subtitles)
}

fn source_streams_from_matroska(
    meta: &matroska::MatroskaBasicMetadata,
) -> (
    Vec<SourceVideoStream>,
    Vec<SourceAudioStream>,
    Vec<SourceSubtitleStream>,
) {
    let mut videos = Vec::new();
    let mut audios = Vec::new();
    let mut subtitles = Vec::new();

    for track in &meta.tracks {
        match track.kind {
            matroska::MatroskaTrackKind::Video => videos.push(SourceVideoStream {
                index: track.index,
                codec: track.codec.clone(),
                width: track.width,
                height: track.height,
                frame_rate: None,
                profile: None,
                bitrate: None,
                pixel_format: None,
                high_bit_depth: false,
                color_primaries: None,
                color_transfer: None,
                color_space: None,
                color_range: None,
                dynamic_range: SourceVideoDynamicRange::Unknown,
                dolby_vision: false,
                hdr10_plus: false,
                duration_ms: meta.duration_ms,
            }),
            matroska::MatroskaTrackKind::Audio => audios.push(SourceAudioStream {
                index: track.index,
                codec: normalize_audio_codec(&track.codec),
                profile: None,
                channels: track.channels.unwrap_or(2),
                bitrate: None,
                language: track.language.clone(),
                title: track.name.clone(),
                default: track.default,
                forced: track.forced,
                atmos: track.codec == "eac3"
                    && track
                        .name
                        .as_deref()
                        .is_some_and(|name| name.to_ascii_lowercase().contains("atmos")),
                atmos_joc: false,
            }),
            matroska::MatroskaTrackKind::Subtitle => subtitles.push(SourceSubtitleStream {
                index: track.index,
                codec: track.codec.clone(),
                kind: classify_text_subtitle_codec(&track.codec),
                language: track.language.clone(),
                title: track.name.clone(),
                default: track.default,
                forced: track.forced,
            }),
            matroska::MatroskaTrackKind::Unknown => {}
        }
    }

    (videos, audios, subtitles)
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
        assert_eq!(
            classify_text_subtitle_codec("ass"),
            SourceSubtitleKind::Text
        );
        assert_eq!(
            classify_text_subtitle_codec("hdmv_pgs_subtitle"),
            SourceSubtitleKind::Bitmap
        );
        assert_eq!(
            classify_text_subtitle_codec("weird"),
            SourceSubtitleKind::Unknown
        );
    }

    #[test]
    fn exposes_container_name_from_sniff() {
        assert_eq!(ContainerKind::Matroska.public_name(), "mkv");
    }
}
