use std::fs::File;
use std::io::Read;
use std::path::{Path, PathBuf};

use memmap2::Mmap;
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
    #[error("map_failed: {0}")]
    MapFailed(String),
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct MediaProbe {
    pub schema_version: u32,
    pub engine: ProbeEngine,
    pub source: MediaSource,
    pub duration_ms: Option<u64>,
    pub tracks: Vec<MediaTrack>,
    pub attachments: AttachmentSummary,
    pub capabilities: MediaCapabilities,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct ProbeEngine {
    pub name: String,
    pub version: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct MediaSource {
    pub path: PathBuf,
    pub size_bytes: u64,
    pub container: ContainerDescriptor,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct ContainerDescriptor {
    pub family: ContainerFamily,
    pub brand: String,
    pub extension_hint: Option<String>,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub enum ContainerFamily {
    IsoBmff,
    Matroska,
    Unknown,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct MediaTrack {
    pub id: String,
    pub index: u32,
    pub kind: TrackKind,
    pub codec: CodecDescriptor,
    pub duration_ms: Option<u64>,
    pub language: Option<String>,
    pub title: Option<String>,
    pub flags: TrackFlags,
    pub video: Option<VideoDescriptor>,
    pub audio: Option<AudioDescriptor>,
    pub subtitle: Option<SubtitleDescriptor>,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub enum TrackKind {
    Video,
    Audio,
    Subtitle,
    Unknown,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct CodecDescriptor {
    pub id: String,
    pub family: CodecFamily,
    pub profile: Option<String>,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub enum CodecFamily {
    H264,
    Hevc,
    Av1,
    Vp9,
    Aac,
    Ac3,
    Eac3,
    Dts,
    TrueHd,
    Flac,
    Alac,
    Opus,
    Mp3,
    TextSubtitle,
    BitmapSubtitle,
    Unknown,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct TrackFlags {
    pub default: bool,
    pub forced: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct VideoDescriptor {
    pub width: Option<u32>,
    pub height: Option<u32>,
    pub frame_rate: Option<f64>,
    pub pixel_format: Option<String>,
    pub dynamic_range: DynamicRange,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub enum DynamicRange {
    Sdr,
    Hdr10,
    Hdr10Plus,
    Hlg,
    DolbyVision,
    Unknown,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct AudioDescriptor {
    pub channels: Option<u32>,
    pub sample_rate: Option<u32>,
    pub atmos: bool,
    pub lossless: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct SubtitleDescriptor {
    pub format: SubtitleFormat,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub enum SubtitleFormat {
    Text,
    Bitmap,
    Unknown,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct AttachmentSummary {
    pub count: u32,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct MediaCapabilities {
    pub can_remux_without_decode: bool,
    pub can_segment_without_decode: bool,
    pub requires_video_decode: bool,
    pub requires_audio_decode: bool,
    pub unsupported_track_ids: Vec<String>,
}

pub fn probe_media_source(path: &Path) -> Result<MediaProbe, ProbeError> {
    if !path.exists() {
        return Err(ProbeError::FileNotFound);
    }

    let mut file = File::open(path).map_err(|err| ProbeError::OpenFailed(err.to_string()))?;
    let size_bytes = file
        .metadata()
        .map_err(|err| ProbeError::OpenFailed(err.to_string()))?
        .len();
    let mut head = [0_u8; 4096];
    let n = file
        .read(&mut head)
        .map_err(|err| ProbeError::ReadFailed(err.to_string()))?;

    let container = sniff_container(&head[..n]);
    let mapped =
        unsafe { Mmap::map(&file) }.map_err(|err| ProbeError::MapFailed(err.to_string()))?;

    let mut duration_ms = None;
    let mut tracks = Vec::new();
    let mut attachment_count = 0;

    if matches!(container, ContainerKind::Mp4 | ContainerKind::Mov) {
        let meta = mp4::parse_basic_metadata(&mapped);
        duration_ms = meta.duration_ms;
        tracks = tracks_from_mp4(&meta);
    } else if matches!(container, ContainerKind::Matroska | ContainerKind::Webm) {
        let meta = matroska::parse_basic_metadata(&mapped);
        duration_ms = meta.duration_ms;
        attachment_count = meta.attachment_count;
        tracks = tracks_from_matroska(&meta);
    }

    let capabilities = infer_capabilities(&tracks);
    Ok(MediaProbe {
        schema_version: 1,
        engine: ProbeEngine {
            name: "chroma-engine".to_string(),
            version: env!("CARGO_PKG_VERSION").to_string(),
        },
        source: MediaSource {
            path: path.to_path_buf(),
            size_bytes,
            container: container_descriptor(container, path),
        },
        duration_ms,
        tracks,
        attachments: AttachmentSummary {
            count: attachment_count,
        },
        capabilities,
    })
}

fn tracks_from_mp4(meta: &mp4::Mp4BasicMetadata) -> Vec<MediaTrack> {
    let mut video_n = 0;
    let mut audio_n = 0;
    let mut subtitle_n = 0;

    meta.tracks
        .iter()
        .map(|track| {
            let kind = match track.kind {
                mp4::Mp4TrackKind::Video => TrackKind::Video,
                mp4::Mp4TrackKind::Audio => TrackKind::Audio,
                mp4::Mp4TrackKind::Subtitle => TrackKind::Subtitle,
                mp4::Mp4TrackKind::Unknown => TrackKind::Unknown,
            };
            let id = next_track_id(
                kind,
                &mut video_n,
                &mut audio_n,
                &mut subtitle_n,
                track.index,
            );
            media_track(TrackInput {
                id,
                index: track.index,
                kind,
                codec: track.codec.clone(),
                duration_ms: track.duration_ms,
                language: None,
                title: None,
                flags: TrackFlags {
                    default: false,
                    forced: false,
                },
                shape: TrackShape {
                    width: track.width,
                    height: track.height,
                    channels: track.channels,
                    sample_rate: track.sample_rate,
                },
            })
        })
        .collect()
}

fn tracks_from_matroska(meta: &matroska::MatroskaBasicMetadata) -> Vec<MediaTrack> {
    let mut video_n = 0;
    let mut audio_n = 0;
    let mut subtitle_n = 0;

    meta.tracks
        .iter()
        .map(|track| {
            let kind = match track.kind {
                matroska::MatroskaTrackKind::Video => TrackKind::Video,
                matroska::MatroskaTrackKind::Audio => TrackKind::Audio,
                matroska::MatroskaTrackKind::Subtitle => TrackKind::Subtitle,
                matroska::MatroskaTrackKind::Unknown => TrackKind::Unknown,
            };
            let id = next_track_id(
                kind,
                &mut video_n,
                &mut audio_n,
                &mut subtitle_n,
                track.index,
            );
            media_track(TrackInput {
                id,
                index: track.index,
                kind,
                codec: track.codec.clone(),
                duration_ms: meta.duration_ms,
                language: track.language.clone(),
                title: track.name.clone(),
                flags: TrackFlags {
                    default: track.default,
                    forced: track.forced,
                },
                shape: TrackShape {
                    width: track.width,
                    height: track.height,
                    channels: track.channels,
                    sample_rate: track.sample_rate,
                },
            })
        })
        .collect()
}

#[derive(Debug, Clone, Copy)]
struct TrackShape {
    width: Option<u32>,
    height: Option<u32>,
    channels: Option<u32>,
    sample_rate: Option<u32>,
}

#[derive(Debug, Clone)]
struct TrackInput {
    id: String,
    index: u32,
    kind: TrackKind,
    codec: String,
    duration_ms: Option<u64>,
    language: Option<String>,
    title: Option<String>,
    flags: TrackFlags,
    shape: TrackShape,
}

fn media_track(input: TrackInput) -> MediaTrack {
    let family = codec_family(&input.codec);
    MediaTrack {
        id: input.id,
        index: input.index,
        kind: input.kind,
        codec: CodecDescriptor {
            id: input.codec.clone(),
            family,
            profile: None,
        },
        duration_ms: input.duration_ms,
        language: input.language,
        title: input.title,
        flags: input.flags,
        video: (input.kind == TrackKind::Video).then_some(VideoDescriptor {
            width: input.shape.width,
            height: input.shape.height,
            frame_rate: None,
            pixel_format: None,
            dynamic_range: DynamicRange::Unknown,
        }),
        audio: (input.kind == TrackKind::Audio).then_some(AudioDescriptor {
            channels: input.shape.channels,
            sample_rate: input.shape.sample_rate,
            atmos: family == CodecFamily::Eac3,
            lossless: matches!(
                family,
                CodecFamily::TrueHd | CodecFamily::Flac | CodecFamily::Alac
            ),
        }),
        subtitle: (input.kind == TrackKind::Subtitle).then_some(SubtitleDescriptor {
            format: subtitle_format_for_codec(&input.codec),
        }),
    }
}

fn next_track_id(
    kind: TrackKind,
    video_n: &mut u32,
    audio_n: &mut u32,
    subtitle_n: &mut u32,
    fallback: u32,
) -> String {
    match kind {
        TrackKind::Video => {
            let id = format!("v{video_n}");
            *video_n += 1;
            id
        }
        TrackKind::Audio => {
            let id = format!("a{audio_n}");
            *audio_n += 1;
            id
        }
        TrackKind::Subtitle => {
            let id = format!("s{subtitle_n}");
            *subtitle_n += 1;
            id
        }
        TrackKind::Unknown => format!("x{fallback}"),
    }
}

fn infer_capabilities(tracks: &[MediaTrack]) -> MediaCapabilities {
    let unsupported_track_ids = tracks
        .iter()
        .filter(|track| track.codec.family == CodecFamily::Unknown)
        .map(|track| track.id.clone())
        .collect::<Vec<_>>();
    let requires_video_decode = tracks
        .iter()
        .any(|track| track.kind == TrackKind::Video && !video_can_copy(track.codec.family));
    let requires_audio_decode = tracks
        .iter()
        .any(|track| track.kind == TrackKind::Audio && !audio_can_copy(track.codec.family));

    MediaCapabilities {
        can_remux_without_decode: !requires_video_decode && !requires_audio_decode,
        can_segment_without_decode: !requires_video_decode && !requires_audio_decode,
        requires_video_decode,
        requires_audio_decode,
        unsupported_track_ids,
    }
}

fn video_can_copy(family: CodecFamily) -> bool {
    matches!(
        family,
        CodecFamily::H264 | CodecFamily::Hevc | CodecFamily::Av1 | CodecFamily::Vp9
    )
}

fn audio_can_copy(family: CodecFamily) -> bool {
    matches!(
        family,
        CodecFamily::Aac
            | CodecFamily::Ac3
            | CodecFamily::Eac3
            | CodecFamily::Dts
            | CodecFamily::TrueHd
            | CodecFamily::Flac
            | CodecFamily::Alac
            | CodecFamily::Opus
            | CodecFamily::Mp3
    )
}

fn container_descriptor(container: ContainerKind, path: &Path) -> ContainerDescriptor {
    ContainerDescriptor {
        family: match container {
            ContainerKind::Mp4 | ContainerKind::Mov => ContainerFamily::IsoBmff,
            ContainerKind::Matroska | ContainerKind::Webm => ContainerFamily::Matroska,
            ContainerKind::Unknown => ContainerFamily::Unknown,
        },
        brand: match container {
            ContainerKind::Mp4 => "mp4",
            ContainerKind::Mov => "mov",
            ContainerKind::Matroska => "matroska",
            ContainerKind::Webm => "webm",
            ContainerKind::Unknown => "unknown",
        }
        .to_string(),
        extension_hint: path
            .extension()
            .and_then(|s| s.to_str())
            .map(|s| s.to_ascii_lowercase()),
    }
}

pub fn subtitle_format_for_codec(codec: &str) -> SubtitleFormat {
    match codec.trim().to_ascii_lowercase().as_str() {
        "subrip" | "ass" | "ssa" | "mov_text" | "webvtt" | "text" | "subviewer" | "microdvd" => {
            SubtitleFormat::Text
        }
        "hdmv_pgs_subtitle" | "dvd_subtitle" | "dvb_subtitle" | "dvb_teletext" | "xsub" => {
            SubtitleFormat::Bitmap
        }
        _ => SubtitleFormat::Unknown,
    }
}

pub fn normalize_audio_codec(codec: &str) -> String {
    match codec.trim().to_ascii_lowercase().as_str() {
        "dca" => "dts".to_string(),
        other => other.to_string(),
    }
}

pub fn codec_family(codec: &str) -> CodecFamily {
    match normalize_audio_codec(codec).as_str() {
        "h264" | "avc1" | "avc3" => CodecFamily::H264,
        "hevc" | "h265" | "hvc1" | "hev1" | "dvh1" | "dvhe" => CodecFamily::Hevc,
        "av1" | "av01" => CodecFamily::Av1,
        "vp9" | "vp09" => CodecFamily::Vp9,
        "aac" | "mp4a" => CodecFamily::Aac,
        "ac3" | "ac-3" => CodecFamily::Ac3,
        "eac3" | "ec-3" => CodecFamily::Eac3,
        "dts" => CodecFamily::Dts,
        "truehd" => CodecFamily::TrueHd,
        "flac" => CodecFamily::Flac,
        "alac" => CodecFamily::Alac,
        "opus" => CodecFamily::Opus,
        "mp3" | ".mp3" => CodecFamily::Mp3,
        "subrip" | "ass" | "ssa" | "mov_text" | "webvtt" | "text" | "subviewer" | "microdvd" => {
            CodecFamily::TextSubtitle
        }
        "hdmv_pgs_subtitle" | "dvd_subtitle" | "dvb_subtitle" | "dvb_teletext" | "xsub" => {
            CodecFamily::BitmapSubtitle
        }
        _ => CodecFamily::Unknown,
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
        assert_eq!(subtitle_format_for_codec("ass"), SubtitleFormat::Text);
        assert_eq!(
            subtitle_format_for_codec("hdmv_pgs_subtitle"),
            SubtitleFormat::Bitmap
        );
        assert_eq!(subtitle_format_for_codec("weird"), SubtitleFormat::Unknown);
    }

    #[test]
    fn assigns_stable_track_ids_by_kind() {
        let mut v = 0;
        let mut a = 0;
        let mut s = 0;
        assert_eq!(
            next_track_id(TrackKind::Audio, &mut v, &mut a, &mut s, 8),
            "a0"
        );
        assert_eq!(
            next_track_id(TrackKind::Video, &mut v, &mut a, &mut s, 9),
            "v0"
        );
        assert_eq!(
            next_track_id(TrackKind::Audio, &mut v, &mut a, &mut s, 10),
            "a1"
        );
    }
}
