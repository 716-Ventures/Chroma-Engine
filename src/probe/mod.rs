use std::fs::File;
use std::io::Read;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};
use thiserror::Error;

use crate::{
    container::{ContainerKind, matroska, mp4, sniff_container},
    source::MappedMediaFile,
};

#[derive(Debug, Error)]
/// Error returned while probing a media source.
pub enum ProbeError {
    /// The requested path does not exist.
    #[error("file_not_found")]
    FileNotFound,
    /// The source file could not be opened.
    #[error("open_failed: {0}")]
    OpenFailed(String),
    /// The source header could not be read.
    #[error("read_failed: {0}")]
    ReadFailed(String),
    /// The source file could not be memory mapped.
    #[error("map_failed: {0}")]
    MapFailed(String),
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase")]
/// Media probe document used by Chroma analysis and playback planning.
pub struct MediaProbe {
    /// Probe schema version.
    pub schema_version: u32,
    /// Engine identity that produced the probe.
    pub engine: ProbeEngine,
    /// Source file and container descriptor.
    pub source: MediaSource,
    /// Media duration in milliseconds when known.
    pub duration_ms: Option<u64>,
    /// Tracks discovered in the source.
    pub tracks: Vec<MediaTrack>,
    /// Chapters discovered in the source.
    pub chapters: Vec<Chapter>,
    /// Attachment summary for container-level attachments.
    pub attachments: AttachmentSummary,
    /// Capability summary derived from the tracks.
    pub capabilities: MediaCapabilities,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
/// Engine identity embedded in probe output.
pub struct ProbeEngine {
    /// Engine name.
    pub name: String,
    /// Engine package version.
    pub version: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
/// Source-level file metadata.
pub struct MediaSource {
    /// Source path.
    pub path: PathBuf,
    /// Source size in bytes.
    pub size_bytes: u64,
    /// Detected container descriptor.
    pub container: ContainerDescriptor,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
/// Human-readable container descriptor.
pub struct ContainerDescriptor {
    /// Container family.
    pub family: ContainerFamily,
    /// Container brand or stable public name.
    pub brand: String,
    /// Extension hint derived from the source path.
    pub extension_hint: Option<String>,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
/// Supported high-level container families.
pub enum ContainerFamily {
    /// ISO Base Media File Format, including MP4 and MOV.
    IsoBmff,
    /// Matroska/WebM container family.
    Matroska,
    /// Unknown or unsupported container family.
    Unknown,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase")]
/// One media track discovered in a source.
pub struct MediaTrack {
    /// Stable semantic track identifier.
    pub id: String,
    /// Zero-based track index in the source.
    pub index: u32,
    /// Track media kind.
    pub kind: TrackKind,
    /// Normalized codec descriptor.
    pub codec: CodecDescriptor,
    /// Track duration in milliseconds when known.
    pub duration_ms: Option<u64>,
    /// Track language when available.
    pub language: Option<String>,
    /// Track title when available.
    pub title: Option<String>,
    /// Container-level track flags.
    pub flags: TrackFlags,
    /// Video metadata for video tracks.
    pub video: Option<VideoDescriptor>,
    /// Audio metadata for audio tracks.
    pub audio: Option<AudioDescriptor>,
    /// Subtitle metadata for subtitle tracks.
    pub subtitle: Option<SubtitleDescriptor>,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
/// Media kind for a track.
pub enum TrackKind {
    /// Video track.
    Video,
    /// Audio track.
    Audio,
    /// Subtitle track.
    Subtitle,
    /// Unknown or unsupported track kind.
    Unknown,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
/// Normalized codec information for a track.
pub struct CodecDescriptor {
    /// Container codec identifier.
    pub id: String,
    /// Normalized codec family.
    pub family: CodecFamily,
    /// Optional codec profile.
    pub profile: Option<String>,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
/// Codec families understood by Chroma Engine.
pub enum CodecFamily {
    /// H.264/AVC video.
    H264,
    /// H.265/HEVC video.
    Hevc,
    /// AV1 video.
    Av1,
    /// VP9 video.
    Vp9,
    /// AAC audio.
    Aac,
    /// Dolby Digital AC-3 audio.
    Ac3,
    /// Dolby Digital Plus E-AC-3 audio.
    Eac3,
    /// DTS audio.
    Dts,
    /// Dolby TrueHD audio.
    TrueHd,
    /// FLAC audio.
    Flac,
    /// ALAC audio.
    Alac,
    /// Opus audio.
    Opus,
    /// MPEG Layer III audio.
    Mp3,
    /// Text subtitle codec.
    TextSubtitle,
    /// Bitmap subtitle codec.
    BitmapSubtitle,
    /// Unknown or unsupported codec.
    Unknown,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
/// Container-level flags attached to a track.
pub struct TrackFlags {
    /// Whether this is the default track for its kind.
    pub default: bool,
    /// Whether this subtitle track is forced.
    pub forced: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase")]
/// Video-specific track metadata.
pub struct VideoDescriptor {
    /// Encoded width in pixels.
    pub width: Option<u32>,
    /// Encoded height in pixels.
    pub height: Option<u32>,
    /// Average or declared frame rate.
    pub frame_rate: Option<f64>,
    /// Pixel format when known.
    pub pixel_format: Option<String>,
    /// Dynamic range classification.
    pub dynamic_range: DynamicRange,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
/// Video dynamic range classification.
pub enum DynamicRange {
    /// Standard dynamic range.
    Sdr,
    /// HDR10.
    Hdr10,
    /// HDR10+.
    Hdr10Plus,
    /// Hybrid Log-Gamma.
    Hlg,
    /// Dolby Vision.
    DolbyVision,
    /// Unknown dynamic range.
    Unknown,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
/// Audio-specific track metadata.
pub struct AudioDescriptor {
    /// Channel count.
    pub channels: Option<u32>,
    /// Sample rate in hertz.
    pub sample_rate: Option<u32>,
    /// Whether the track carries Dolby Atmos metadata.
    pub atmos: bool,
    /// Whether the codec is lossless.
    pub lossless: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
/// Subtitle-specific track metadata.
pub struct SubtitleDescriptor {
    /// Subtitle representation family.
    pub format: SubtitleFormat,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
/// Subtitle representation family.
pub enum SubtitleFormat {
    /// Text subtitle format.
    Text,
    /// Bitmap subtitle format.
    Bitmap,
    /// Unknown subtitle format.
    Unknown,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
/// Summary of non-track attachments in the source.
pub struct AttachmentSummary {
    /// Number of attachments discovered.
    pub count: u32,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
/// Chapter marker normalized from the source container.
pub struct Chapter {
    /// Stable chapter identifier.
    pub id: String,
    /// Chapter start timestamp in milliseconds.
    pub start_ms: u64,
    /// Chapter end timestamp in milliseconds when available.
    pub end_ms: Option<u64>,
    /// Chapter title when available.
    pub title: Option<String>,
    /// Chapter language when available.
    pub language: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
/// Capability summary used to choose direct playback, remux, or transcode.
pub struct MediaCapabilities {
    /// Whether all selected tracks can be remuxed without decoding.
    pub can_remux_without_decode: bool,
    /// Whether the source can be segmented without decoding.
    pub can_segment_without_decode: bool,
    /// Whether video decode is required for compatible playback.
    pub requires_video_decode: bool,
    /// Whether audio decode is required for compatible playback.
    pub requires_audio_decode: bool,
    /// Track ids that Chroma Engine cannot currently carry.
    pub unsupported_track_ids: Vec<String>,
}

/// Probes a media source and returns normalized metadata for planning.
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
        MappedMediaFile::open(path).map_err(|err| ProbeError::MapFailed(err.to_string()))?;

    let mut duration_ms = None;
    let mut tracks = Vec::new();
    let mut chapters = Vec::new();
    let mut attachment_count = 0;

    if matches!(container, ContainerKind::Mp4 | ContainerKind::Mov) {
        let meta = mp4::parse_basic_metadata(mapped.as_ref());
        duration_ms = meta.duration_ms;
        tracks = tracks_from_mp4(&meta);
    } else if matches!(container, ContainerKind::Matroska | ContainerKind::Webm) {
        let meta = matroska::parse_basic_metadata(mapped.as_ref());
        duration_ms = meta.duration_ms;
        attachment_count = meta.attachment_count;
        chapters = chapters_from_matroska(&meta.chapters);
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
        chapters,
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
                language: track.language.clone(),
                title: None,
                flags: TrackFlags {
                    default: false,
                    forced: false,
                },
                shape: TrackShape {
                    width: track.width,
                    height: track.height,
                    frame_rate: track.frame_rate,
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
                    frame_rate: frame_rate_from_default_duration(track.default_duration_ns),
                    channels: track.channels,
                    sample_rate: track.sample_rate,
                },
            })
        })
        .collect()
}

fn chapters_from_matroska(chapters: &[matroska::MatroskaChapter]) -> Vec<Chapter> {
    chapters
        .iter()
        .map(|chapter| Chapter {
            id: chapter.id.clone(),
            start_ms: chapter.start_ms,
            end_ms: chapter.end_ms,
            title: chapter.title.clone(),
            language: chapter.language.clone(),
        })
        .collect()
}

#[derive(Debug, Clone, Copy)]
struct TrackShape {
    width: Option<u32>,
    height: Option<u32>,
    frame_rate: Option<f64>,
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
            frame_rate: input.shape.frame_rate,
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

fn frame_rate_from_default_duration(default_duration_ns: Option<u64>) -> Option<f64> {
    let duration_ns = default_duration_ns?;
    if duration_ns == 0 {
        return None;
    }
    Some(1_000_000_000_f64 / duration_ns as f64)
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

    #[test]
    fn derives_frame_rate_from_matroska_default_duration() {
        assert_eq!(
            frame_rate_from_default_duration(Some(41_666_667)),
            Some(23.999999808000002)
        );
        assert_eq!(frame_rate_from_default_duration(Some(0)), None);
        assert_eq!(frame_rate_from_default_duration(None), None);
    }
}
