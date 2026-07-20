use std::path::Path;

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
    /// The source container is not supported.
    #[error("unsupported container: {0}")]
    UnsupportedContainer(&'static str),
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
            Self::UnsupportedContainer(_) => EngineErrorCode::UnsupportedContainer,
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
        assert_eq!(RemuxError::NoTrack.code(), EngineErrorCode::NoMatchingTrack);
    }

    #[test]
    fn stream_copy_packet_spans_rejects_unknown_container() {
        let err = stream_copy_packet_spans(b"not media", None, PacketRange { start: 0, end: 0 })
            .unwrap_err();
        assert_eq!(err.code(), EngineErrorCode::UnsupportedContainer);
    }
}
