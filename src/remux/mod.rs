use std::{io::Write, path::Path};

use serde::{Deserialize, Serialize};
use thiserror::Error;

use crate::{
    container::{ContainerKind, matroska, mp4, sniff_container},
    error::EngineErrorCode,
    hls::{HlsOptions, HlsVodPlan},
    output::publish_file,
    packet::{PacketExtractError, PacketPayloadSpan, PacketRange, packet_payload_spans},
};

/// Borrowed packet payloads ready for a stream-copy remux stage.
#[derive(Debug, PartialEq, Eq)]
pub struct RemuxPacketSpans<'a> {
    /// Source container family.
    pub container: ContainerKind,
    /// Stable semantic track identifier.
    pub track_id: String,
    /// Borrowed payload spans in packet order.
    pub spans: Vec<PacketPayloadSpan<'a>>,
}

/// Normalized source metadata that can be copied into remuxed outputs.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct RemuxMetadata {
    /// Source container family.
    pub container: String,
    /// Source duration in milliseconds when available.
    pub duration_ms: Option<u64>,
    /// Stable normalized track metadata.
    pub tracks: Vec<RemuxTrackMetadata>,
    /// Chapter markers copied from the source container.
    pub chapters: Vec<RemuxChapter>,
    /// Number of non-track attachments discovered in the source.
    pub attachment_count: u32,
}

/// Track metadata needed by a stream-copy remux plan.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct RemuxTrackMetadata {
    /// Stable semantic track identifier.
    pub id: String,
    /// Zero-based source track index.
    pub index: u32,
    /// Track media kind.
    pub kind: RemuxTrackKind,
    /// Normalized codec identifier.
    pub codec: String,
    /// Track duration in milliseconds when available.
    pub duration_ms: Option<u64>,
    /// ISO/BPC language tag when available.
    pub language: Option<String>,
    /// Track title or display name when available.
    pub title: Option<String>,
    /// Whether the source marks this as the default track.
    pub default: bool,
    /// Whether the source marks this track as forced.
    pub forced: bool,
}

/// Media kind for a remuxable source track.
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub enum RemuxTrackKind {
    /// Video track.
    Video,
    /// Audio track.
    Audio,
    /// Subtitle track.
    Subtitle,
    /// Unknown or unsupported track kind.
    Unknown,
}

/// Chapter marker copied from the source container.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct RemuxChapter {
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

/// Error returned while remuxing a source into MP4.
#[derive(Debug, Error)]
pub enum RemuxError {
    /// Source I/O failed.
    #[error(transparent)]
    Io(#[from] std::io::Error),
    /// Output I/O failed.
    #[error("output write failed: {0}")]
    OutputIo(String),
    /// The source container is not supported.
    #[error("unsupported container: {0}")]
    UnsupportedContainer(&'static str),
    /// The supplied MP4 boxes are not valid for faststart output.
    #[error("invalid faststart MP4 layout: {0}")]
    InvalidFaststartLayout(&'static str),
    /// The remux writer for this container is not implemented yet.
    #[error("remux writer is not implemented for {0}")]
    NotImplemented(&'static str),
    /// No matching stream-copy track exists.
    #[error("no matching track for stream-copy remux")]
    NoTrack,
    /// Packet payload extraction failed.
    #[error("packet extraction failed: {0}")]
    Packet(#[from] PacketExtractError),
    /// fMP4 remux planning or muxing failed.
    #[error("fragmented MP4 remux failed: {0}")]
    Fmp4(String),
}

impl RemuxError {
    /// Returns the stable Chroma Engine error code for this remux failure.
    pub fn code(&self) -> EngineErrorCode {
        match self {
            Self::Io(_) => EngineErrorCode::SourceReadFailed,
            Self::OutputIo(_) => EngineErrorCode::OutputIoFailed,
            Self::UnsupportedContainer(_) => EngineErrorCode::UnsupportedContainer,
            Self::InvalidFaststartLayout(_) => EngineErrorCode::UnsupportedContainer,
            Self::NotImplemented(_) => EngineErrorCode::OperationNotImplemented,
            Self::NoTrack => EngineErrorCode::NoMatchingTrack,
            Self::Packet(_) => EngineErrorCode::SourceReadFailed,
            Self::Fmp4(_) => EngineErrorCode::HlsUnsupported,
        }
    }
}

/// Returns normalized metadata that a remux writer can copy into its output.
pub fn copyable_metadata(bytes: &[u8]) -> Result<RemuxMetadata, RemuxError> {
    let kind = sniff_container(bytes);
    match kind {
        ContainerKind::Mp4 | ContainerKind::Mov => {
            let meta = mp4::parse_basic_metadata(bytes);
            Ok(metadata_from_mp4(kind, &meta))
        }
        ContainerKind::Matroska | ContainerKind::Webm => {
            let meta = matroska::parse_basic_metadata(bytes);
            Ok(metadata_from_matroska(kind, &meta))
        }
        ContainerKind::Unknown => Err(RemuxError::UnsupportedContainer(kind.public_name())),
    }
}

/// Returns borrowed stream-copy packet spans for a selected source track.
pub fn stream_copy_packet_spans<'a>(
    bytes: &'a [u8],
    requested_track_id: Option<&str>,
    range: PacketRange,
) -> Result<RemuxPacketSpans<'a>, RemuxError> {
    let kind = sniff_container(bytes);
    match kind {
        ContainerKind::Mp4 | ContainerKind::Mov => {
            let track =
                mp4::parse_packet_track(bytes, requested_track_id).ok_or(RemuxError::NoTrack)?;
            let spans = packet_payload_spans(bytes, &track.packets, range)?;
            Ok(RemuxPacketSpans {
                container: kind,
                track_id: track.track_id,
                spans,
            })
        }
        ContainerKind::Matroska | ContainerKind::Webm => {
            let track = matroska::parse_packet_track(bytes, requested_track_id)
                .ok_or(RemuxError::NoTrack)?;
            let spans = packet_payload_spans(bytes, &track.packets, range)?;
            Ok(RemuxPacketSpans {
                container: kind,
                track_id: track.id,
                spans,
            })
        }
        ContainerKind::Unknown => Err(RemuxError::UnsupportedContainer(kind.public_name())),
    }
}

/// Writes a faststart fragmented MP4 stream with `ftyp+moov` before media data.
pub fn write_faststart_mp4<W: Write>(
    mut output: W,
    init_segment: &[u8],
    media_fragments: &[&[u8]],
) -> Result<(), RemuxError> {
    validate_faststart_init(init_segment)?;
    for fragment in media_fragments {
        validate_media_fragment(fragment)?;
    }

    output
        .write_all(init_segment)
        .map_err(|err| RemuxError::OutputIo(err.to_string()))?;
    for fragment in media_fragments {
        output
            .write_all(fragment)
            .map_err(|err| RemuxError::OutputIo(err.to_string()))?;
    }
    Ok(())
}

/// Remuxes a supported source into fragmented MP4 without decoding.
pub fn remux_mp4(input: &Path, output: &Path) -> Result<(), RemuxError> {
    let mut file = std::fs::File::open(input)?;
    let mut head = [0_u8; 4096];
    let n = std::io::Read::read(&mut file, &mut head)?;
    let kind = sniff_container(&head[..n]);
    match kind {
        ContainerKind::Matroska | ContainerKind::Webm | ContainerKind::Mp4 | ContainerKind::Mov => {
            write_fragmented_mp4(input, output)
        }
        ContainerKind::Unknown => Err(RemuxError::UnsupportedContainer(kind.public_name())),
    }
}

fn write_fragmented_mp4(input: &Path, output: &Path) -> Result<(), RemuxError> {
    let plan = HlsVodPlan::open(input, HlsOptions::default())
        .map_err(|err| RemuxError::Fmp4(err.to_string()))?;
    let init = plan
        .fmp4_init_segment()
        .map_err(|err| RemuxError::Fmp4(err.to_string()))?;
    publish_file(output, |file| {
        let mut writer = std::io::BufWriter::new(file);
        writer.write_all(&init)?;
        for index in 0..plan.segment_count() {
            let segment = plan
                .mux_fmp4_segment(index)
                .map_err(|err| RemuxError::Fmp4(err.to_string()))?;
            writer.write_all(&segment)?;
        }
        writer.flush()?;
        Ok(())
    })
}

fn metadata_from_mp4(container: ContainerKind, meta: &mp4::Mp4BasicMetadata) -> RemuxMetadata {
    let mut video_n = 0;
    let mut audio_n = 0;
    let mut subtitle_n = 0;
    let tracks = meta
        .tracks
        .iter()
        .map(|track| {
            let kind = remux_kind_from_mp4(track.kind);
            RemuxTrackMetadata {
                id: next_track_id(
                    kind,
                    &mut video_n,
                    &mut audio_n,
                    &mut subtitle_n,
                    track.index,
                ),
                index: track.index,
                kind,
                codec: track.codec.clone(),
                duration_ms: track.duration_ms,
                language: track.language.clone(),
                title: track.title.clone(),
                default: track.default,
                forced: track.forced,
            }
        })
        .collect();

    RemuxMetadata {
        container: container.public_name().to_string(),
        duration_ms: meta.duration_ms,
        tracks,
        chapters: meta
            .chapters
            .iter()
            .map(|chapter| RemuxChapter {
                id: chapter.id.clone(),
                start_ms: chapter.start_ms,
                end_ms: chapter.end_ms,
                title: chapter.title.clone(),
                language: chapter.language.clone(),
            })
            .collect(),
        attachment_count: 0,
    }
}

fn metadata_from_matroska(
    container: ContainerKind,
    meta: &matroska::MatroskaBasicMetadata,
) -> RemuxMetadata {
    let mut video_n = 0;
    let mut audio_n = 0;
    let mut subtitle_n = 0;
    let tracks = meta
        .tracks
        .iter()
        .map(|track| {
            let kind = remux_kind_from_matroska(track.kind);
            RemuxTrackMetadata {
                id: next_track_id(
                    kind,
                    &mut video_n,
                    &mut audio_n,
                    &mut subtitle_n,
                    track.index,
                ),
                index: track.index,
                kind,
                codec: track.codec.clone(),
                duration_ms: meta.duration_ms,
                language: track.language.clone(),
                title: track.name.clone(),
                default: track.default,
                forced: track.forced,
            }
        })
        .collect();

    RemuxMetadata {
        container: container.public_name().to_string(),
        duration_ms: meta.duration_ms,
        tracks,
        chapters: meta
            .chapters
            .iter()
            .map(|chapter| RemuxChapter {
                id: chapter.id.clone(),
                start_ms: chapter.start_ms,
                end_ms: chapter.end_ms,
                title: chapter.title.clone(),
                language: chapter.language.clone(),
            })
            .collect(),
        attachment_count: meta.attachment_count,
    }
}

fn remux_kind_from_mp4(kind: mp4::Mp4TrackKind) -> RemuxTrackKind {
    match kind {
        mp4::Mp4TrackKind::Video => RemuxTrackKind::Video,
        mp4::Mp4TrackKind::Audio => RemuxTrackKind::Audio,
        mp4::Mp4TrackKind::Subtitle => RemuxTrackKind::Subtitle,
        mp4::Mp4TrackKind::Unknown => RemuxTrackKind::Unknown,
    }
}

fn remux_kind_from_matroska(kind: matroska::MatroskaTrackKind) -> RemuxTrackKind {
    match kind {
        matroska::MatroskaTrackKind::Video => RemuxTrackKind::Video,
        matroska::MatroskaTrackKind::Audio => RemuxTrackKind::Audio,
        matroska::MatroskaTrackKind::Subtitle => RemuxTrackKind::Subtitle,
        matroska::MatroskaTrackKind::Unknown => RemuxTrackKind::Unknown,
    }
}

fn next_track_id(
    kind: RemuxTrackKind,
    video_n: &mut u32,
    audio_n: &mut u32,
    subtitle_n: &mut u32,
    fallback: u32,
) -> String {
    match kind {
        RemuxTrackKind::Video => {
            let id = format!("v{video_n}");
            *video_n += 1;
            id
        }
        RemuxTrackKind::Audio => {
            let id = format!("a{audio_n}");
            *audio_n += 1;
            id
        }
        RemuxTrackKind::Subtitle => {
            let id = format!("s{subtitle_n}");
            *subtitle_n += 1;
            id
        }
        RemuxTrackKind::Unknown => format!("x{fallback}"),
    }
}

fn validate_faststart_init(init_segment: &[u8]) -> Result<(), RemuxError> {
    let boxes = top_level_boxes(init_segment);
    if boxes.len() < 2 {
        return Err(RemuxError::InvalidFaststartLayout(
            "init segment must contain ftyp and moov",
        ));
    }
    if boxes[0] != *b"ftyp" || boxes[1] != *b"moov" {
        return Err(RemuxError::InvalidFaststartLayout(
            "init segment must start with ftyp then moov",
        ));
    }
    if boxes.iter().take(2).any(|kind| kind == b"mdat") {
        return Err(RemuxError::InvalidFaststartLayout(
            "media data cannot precede moov",
        ));
    }
    Ok(())
}

fn validate_media_fragment(fragment: &[u8]) -> Result<(), RemuxError> {
    let boxes = top_level_boxes(fragment);
    if boxes.len() < 2 || boxes[0] != *b"moof" || boxes[1] != *b"mdat" {
        return Err(RemuxError::InvalidFaststartLayout(
            "media fragment must start with moof then mdat",
        ));
    }
    Ok(())
}

fn top_level_boxes(bytes: &[u8]) -> Vec<[u8; 4]> {
    let mut out = Vec::new();
    let mut cursor = 0_usize;
    while cursor.saturating_add(8) <= bytes.len() {
        let size = u32::from_be_bytes([
            bytes[cursor],
            bytes[cursor + 1],
            bytes[cursor + 2],
            bytes[cursor + 3],
        ]) as usize;
        if size < 8 || cursor.saturating_add(size) > bytes.len() {
            break;
        }
        out.push([
            bytes[cursor + 4],
            bytes[cursor + 5],
            bytes[cursor + 6],
            bytes[cursor + 7],
        ]);
        cursor += size;
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn remux_errors_expose_stable_codes() {
        assert_eq!(
            RemuxError::UnsupportedContainer("unknown").code(),
            EngineErrorCode::UnsupportedContainer
        );
        assert_eq!(
            RemuxError::NotImplemented("mp4").code(),
            EngineErrorCode::OperationNotImplemented
        );
        assert_eq!(
            RemuxError::OutputIo("closed".to_string()).code(),
            EngineErrorCode::OutputIoFailed
        );
        assert_eq!(RemuxError::NoTrack.code(), EngineErrorCode::NoMatchingTrack);
    }

    #[test]
    fn stream_copy_packet_spans_rejects_unknown_container() {
        let err = stream_copy_packet_spans(b"not media", None, PacketRange { start: 0, end: 0 })
            .unwrap_err();
        assert_eq!(err.code(), EngineErrorCode::UnsupportedContainer);
    }

    #[test]
    fn mp4_metadata_copy_keeps_tracks_and_chapters() {
        let metadata = metadata_from_mp4(
            ContainerKind::Mp4,
            &mp4::Mp4BasicMetadata {
                major_brand: Some("isom".to_string()),
                compatible_brands: vec!["iso6".to_string()],
                duration_ms: Some(120_000),
                tracks: vec![
                    mp4::Mp4Track {
                        index: 7,
                        kind: mp4::Mp4TrackKind::Video,
                        codec: "h264".to_string(),
                        duration_ms: Some(120_000),
                        language: None,
                        title: Some("Main".to_string()),
                        default: true,
                        forced: false,
                        frame_rate: Some(23.976),
                        bitrate_bps: Some(8_000_000),
                        dynamic_range: mp4::Mp4DynamicRange::Sdr,
                        pixel_format: Some("yuv420p".to_string()),
                        width: Some(1920),
                        height: Some(1080),
                        channels: None,
                        sample_rate: None,
                        atmos: false,
                    },
                    mp4::Mp4Track {
                        index: 8,
                        kind: mp4::Mp4TrackKind::Audio,
                        codec: "aac".to_string(),
                        duration_ms: Some(119_800),
                        language: Some("eng".to_string()),
                        title: Some("English".to_string()),
                        default: true,
                        forced: false,
                        frame_rate: None,
                        bitrate_bps: Some(384_000),
                        dynamic_range: mp4::Mp4DynamicRange::Unknown,
                        pixel_format: None,
                        width: None,
                        height: None,
                        channels: Some(6),
                        sample_rate: Some(48_000),
                        atmos: false,
                    },
                ],
                chapters: vec![mp4::Mp4Chapter {
                    id: "ch0".to_string(),
                    start_ms: 0,
                    end_ms: Some(60_000),
                    title: Some("Opening".to_string()),
                    language: Some("eng".to_string()),
                }],
            },
        );

        assert_eq!(metadata.container, "mp4");
        assert_eq!(metadata.duration_ms, Some(120_000));
        assert_eq!(metadata.attachment_count, 0);
        assert_eq!(metadata.tracks[0].id, "v0");
        assert_eq!(metadata.tracks[0].kind, RemuxTrackKind::Video);
        assert_eq!(metadata.tracks[1].id, "a0");
        assert_eq!(metadata.tracks[1].language.as_deref(), Some("eng"));
        assert_eq!(metadata.chapters[0].title.as_deref(), Some("Opening"));
    }

    #[test]
    fn matroska_metadata_copy_keeps_track_names_and_attachments() {
        let metadata = metadata_from_matroska(
            ContainerKind::Matroska,
            &matroska::MatroskaBasicMetadata {
                duration_ms: Some(90_000),
                tracks: vec![
                    matroska::MatroskaTrack {
                        index: 3,
                        number: 1,
                        kind: matroska::MatroskaTrackKind::Subtitle,
                        codec: "subrip".to_string(),
                        language: Some("spa".to_string()),
                        name: Some("Spanish Forced".to_string()),
                        default: false,
                        forced: true,
                        width: None,
                        height: None,
                        pixel_format: None,
                        transfer_characteristics: None,
                        channels: None,
                        sample_rate: None,
                        atmos: false,
                        object_audio_candidate: false,
                        default_duration_ns: None,
                        codec_private: None,
                    },
                    matroska::MatroskaTrack {
                        index: 4,
                        number: 2,
                        kind: matroska::MatroskaTrackKind::Audio,
                        codec: "eac3".to_string(),
                        language: Some("eng".to_string()),
                        name: Some("English 5.1".to_string()),
                        default: true,
                        forced: false,
                        width: None,
                        height: None,
                        pixel_format: None,
                        transfer_characteristics: None,
                        channels: Some(6),
                        sample_rate: Some(48_000),
                        atmos: false,
                        object_audio_candidate: true,
                        default_duration_ns: None,
                        codec_private: None,
                    },
                ],
                chapters: vec![matroska::MatroskaChapter {
                    id: "edition0_chapter0".to_string(),
                    start_ms: 1_000,
                    end_ms: None,
                    title: Some("Scene 1".to_string()),
                    language: Some("eng".to_string()),
                }],
                attachment_count: 2,
            },
        );

        assert_eq!(metadata.container, "mkv");
        assert_eq!(metadata.attachment_count, 2);
        assert_eq!(metadata.tracks[0].id, "s0");
        assert_eq!(metadata.tracks[0].title.as_deref(), Some("Spanish Forced"));
        assert!(metadata.tracks[0].forced);
        assert_eq!(metadata.tracks[1].id, "a0");
        assert_eq!(metadata.chapters[0].start_ms, 1_000);
    }

    #[test]
    fn writes_faststart_mp4_with_moov_before_media_data() {
        let init = [mp4_box(b"ftyp", b"isom"), mp4_box(b"moov", b"metadata")].concat();
        let fragment = [mp4_box(b"moof", b"traf"), mp4_box(b"mdat", b"payload")].concat();
        let mut out = Vec::new();

        write_faststart_mp4(&mut out, &init, &[&fragment]).unwrap();

        assert_eq!(
            top_level_boxes(&out),
            vec![*b"ftyp", *b"moov", *b"moof", *b"mdat"]
        );
    }

    #[test]
    fn rejects_faststart_init_with_mdat_before_moov() {
        let init = [mp4_box(b"ftyp", b"isom"), mp4_box(b"mdat", b"early")].concat();
        let err = write_faststart_mp4(Vec::new(), &init, &[]).unwrap_err();

        assert!(matches!(err, RemuxError::InvalidFaststartLayout(_)));
    }

    fn mp4_box(kind: &[u8; 4], payload: &[u8]) -> Vec<u8> {
        let mut out = Vec::new();
        out.extend_from_slice(&((payload.len() + 8) as u32).to_be_bytes());
        out.extend_from_slice(kind);
        out.extend_from_slice(payload);
        out
    }
}
