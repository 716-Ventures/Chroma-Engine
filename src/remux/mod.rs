use std::{io::Write, path::Path};

use thiserror::Error;

use crate::{
    container::{ContainerKind, matroska, mp4, sniff_container},
    error::EngineErrorCode,
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
        }
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

/// Remuxes a supported source into MP4 without decoding.
pub fn remux_mp4(input: &Path, _output: &Path) -> Result<(), RemuxError> {
    let mut file = std::fs::File::open(input)?;
    let mut head = [0_u8; 4096];
    let n = std::io::Read::read(&mut file, &mut head)?;
    let kind = sniff_container(&head[..n]);
    match kind {
        ContainerKind::Matroska | ContainerKind::Webm | ContainerKind::Mp4 | ContainerKind::Mov => {
            Err(RemuxError::NotImplemented(kind.public_name()))
        }
        ContainerKind::Unknown => Err(RemuxError::UnsupportedContainer(kind.public_name())),
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
