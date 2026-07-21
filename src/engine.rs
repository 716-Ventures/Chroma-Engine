use std::path::Path;

use serde::{Deserialize, Serialize};
use thiserror::Error;

use crate::{
    container::{
        ContainerKind,
        matroska::{self, MatroskaPacketTrack},
        mp4::{self, Mp4TrackPacketIndex},
        sniff_container,
    },
    packet::{
        ChunkPlan, ExtractedChunk, PacketExtractError, PacketRef, extract_packet_payload,
        packet_samples_for_range, plan_track_chunks,
    },
    source::MediaSource,
};

/// Stateful Chroma Engine entrypoint.
#[derive(Debug, Default, Clone, Copy)]
pub struct Engine;

impl Engine {
    /// Creates an engine handle.
    pub fn new() -> Self {
        Self
    }

    /// Opens a packet-copy playback session and retains source/index state.
    pub fn open_playback_session(
        self,
        input: &Path,
        options: PlaybackSessionOptions,
    ) -> Result<PlaybackSession, EngineSessionError> {
        PlaybackSession::open(input, options)
    }
}

/// Options used to create a stateful playback session.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct PlaybackSessionOptions {
    /// Optional stable Chroma track ID, such as `v0` or `a1`.
    pub track_id: Option<String>,
    /// Preferred target segment duration in milliseconds.
    pub target_ms: u64,
}

impl Default for PlaybackSessionOptions {
    fn default() -> Self {
        Self {
            track_id: None,
            target_ms: 4_000,
        }
    }
}

/// Immutable plan created when a stateful playback session opens.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PlaybackSessionPlan {
    /// Source container family.
    pub container: ContainerKind,
    /// Selected stable track ID.
    pub track_id: String,
    /// Packet-copy chunk plan for the selected track.
    pub chunks: ChunkPlan,
    /// Number of indexed packets retained by the session.
    pub packet_count: usize,
}

/// Stateful packet-copy playback session with retained source bytes and packet index.
#[derive(Debug)]
pub struct PlaybackSession {
    source: MediaSource,
    plan: PlaybackSessionPlan,
    packets: Vec<PacketRef>,
}

impl PlaybackSession {
    /// Opens a source once and parses the selected packet index once.
    pub fn open(input: &Path, options: PlaybackSessionOptions) -> Result<Self, EngineSessionError> {
        if options.target_ms == 0 {
            return Err(EngineSessionError::InvalidTargetDuration);
        }
        let source = MediaSource::open(input).map_err(|error| EngineSessionError::Source {
            reason: error.to_string(),
        })?;
        let container = sniff_container(source.as_ref());
        let (track_id, packets) = match container {
            ContainerKind::Mp4 | ContainerKind::Mov => {
                let Mp4TrackPacketIndex {
                    track_id, packets, ..
                } = mp4::parse_packet_track(source.as_ref(), options.track_id.as_deref())
                    .ok_or(EngineSessionError::NoTrack)?;
                (track_id, packets)
            }
            ContainerKind::Matroska | ContainerKind::Webm => {
                let MatroskaPacketTrack { id, packets } =
                    matroska::parse_packet_track(source.as_ref(), options.track_id.as_deref())
                        .ok_or(EngineSessionError::NoTrack)?;
                (id, packets)
            }
            ContainerKind::Unknown => return Err(EngineSessionError::UnsupportedContainer),
        };
        let chunks = plan_track_chunks(&track_id, &packets, options.target_ms);
        let plan = PlaybackSessionPlan {
            container,
            track_id,
            packet_count: packets.len(),
            chunks,
        };
        Ok(Self {
            source,
            plan,
            packets,
        })
    }

    /// Returns the immutable session plan.
    pub fn plan(&self) -> &PlaybackSessionPlan {
        &self.plan
    }

    /// Extracts one packet-copy chunk from the retained source and packet index.
    pub fn segment(&self, index: u32) -> Result<(ExtractedChunk, Vec<u8>), EngineSessionError> {
        self.source
            .validate_current()
            .map_err(|error| EngineSessionError::SourceChanged {
                reason: error.to_string(),
            })?;
        let chunk = self
            .plan
            .chunks
            .chunks
            .iter()
            .find(|chunk| chunk.index == index)
            .cloned()
            .ok_or(EngineSessionError::NoChunk { index })?;
        let payload =
            extract_packet_payload(self.source.as_ref(), &self.packets, chunk.packet_range)?;
        let samples = packet_samples_for_range(&self.packets, chunk.packet_range)?;
        let packet_count = chunk
            .packet_range
            .end
            .saturating_sub(chunk.packet_range.start);
        let byte_count = payload.len() as u64;
        Ok((
            ExtractedChunk {
                track_id: self.plan.track_id.clone(),
                chunk,
                packet_count,
                byte_count,
                samples,
            },
            payload,
        ))
    }
}

/// Error returned by stateful engine session operations.
#[derive(Debug, Error, PartialEq, Eq)]
pub enum EngineSessionError {
    /// Source open/read failed.
    #[error("source failed: {reason}")]
    Source {
        /// Diagnostic reason.
        reason: String,
    },
    /// Source changed after the session was opened.
    #[error("source changed: {reason}")]
    SourceChanged {
        /// Diagnostic reason.
        reason: String,
    },
    /// Source container is unsupported.
    #[error("unsupported source container")]
    UnsupportedContainer,
    /// No matching packet-copy track exists.
    #[error("no matching track")]
    NoTrack,
    /// Target segment duration must be greater than zero.
    #[error("target segment duration must be greater than zero")]
    InvalidTargetDuration,
    /// Requested chunk index does not exist.
    #[error("chunk {index} does not exist")]
    NoChunk {
        /// Requested chunk index.
        index: u32,
    },
    /// Packet payload extraction failed.
    #[error("packet extraction failed: {0}")]
    Packet(#[from] PacketExtractError),
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn playback_session_reuses_retained_packet_index_for_segments() {
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join("indexed.mp4");
        std::fs::write(&path, indexed_mp4()).expect("write fixture");

        let session = Engine::new()
            .open_playback_session(
                &path,
                PlaybackSessionOptions {
                    track_id: Some("v0".to_string()),
                    target_ms: 1_000,
                },
            )
            .expect("open session");
        let (chunk, payload) = session.segment(0).expect("segment 0");

        assert_eq!(session.plan().track_id, "v0");
        assert_eq!(session.plan().packet_count, 2);
        assert_eq!(chunk.packet_count, 1);
        assert_eq!(payload, b"aaaa");
    }

    #[test]
    fn playback_session_detects_source_replacement_before_segmenting() {
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join("indexed.mp4");
        std::fs::write(&path, indexed_mp4()).expect("write fixture");
        let session = Engine::new()
            .open_playback_session(&path, PlaybackSessionOptions::default())
            .expect("open session");

        std::fs::write(&path, b"not the same source").expect("replace source");

        assert!(matches!(
            session.segment(0),
            Err(EngineSessionError::SourceChanged { .. })
        ));
    }

    fn indexed_mp4() -> Vec<u8> {
        let mut out = atom(
            b"ftyp",
            &[
                b"isom".as_slice(),
                &0_u32.to_be_bytes(),
                b"isom".as_slice(),
                b"mp42".as_slice(),
            ]
            .concat(),
        );
        out.extend_from_slice(&atom(b"moov", &trak_with_samples()));
        out.resize(800, 0);
        out[700..704].copy_from_slice(b"aaaa");
        out[704..708].copy_from_slice(b"bbbb");
        out
    }

    fn trak_with_samples() -> Vec<u8> {
        atom(
            b"trak",
            &[
                tkhd(),
                atom(
                    b"mdia",
                    &[
                        mdhd(1000, 2000),
                        hdlr(b"vide"),
                        atom(
                            b"minf",
                            &atom(
                                b"stbl",
                                &[
                                    stsd(),
                                    stts(&[1000, 1000]),
                                    stss(&[1, 2]),
                                    stsc(&[(1, 2)]),
                                    stsz(&[4, 4]),
                                    stco(&[700]),
                                ]
                                .concat(),
                            ),
                        ),
                    ]
                    .concat(),
                ),
            ]
            .concat(),
        )
    }

    fn tkhd() -> Vec<u8> {
        let mut payload = vec![0_u8; 84];
        payload[76..80].copy_from_slice(&(1920_u32 << 16).to_be_bytes());
        payload[80..84].copy_from_slice(&(1080_u32 << 16).to_be_bytes());
        atom(b"tkhd", &payload)
    }

    fn mdhd(timescale: u32, duration: u32) -> Vec<u8> {
        let mut payload = vec![0_u8; 20];
        payload[12..16].copy_from_slice(&timescale.to_be_bytes());
        payload[16..20].copy_from_slice(&duration.to_be_bytes());
        atom(b"mdhd", &payload)
    }

    fn hdlr(handler: &[u8; 4]) -> Vec<u8> {
        let mut payload = vec![0_u8; 12];
        payload[8..12].copy_from_slice(handler);
        atom(b"hdlr", &payload)
    }

    fn stsd() -> Vec<u8> {
        let mut entry_payload = vec![0_u8; 28];
        entry_payload[24..26].copy_from_slice(&1920_u16.to_be_bytes());
        entry_payload[26..28].copy_from_slice(&1080_u16.to_be_bytes());
        let entry = atom(b"avc1", &entry_payload);
        let mut payload = Vec::new();
        payload.extend_from_slice(&[0, 0, 0, 0]);
        payload.extend_from_slice(&1_u32.to_be_bytes());
        payload.extend_from_slice(&entry);
        atom(b"stsd", &payload)
    }

    fn stts(sample_durations: &[u32]) -> Vec<u8> {
        let mut payload = Vec::new();
        payload.extend_from_slice(&[0, 0, 0, 0]);
        payload.extend_from_slice(&(sample_durations.len() as u32).to_be_bytes());
        for duration in sample_durations {
            payload.extend_from_slice(&1_u32.to_be_bytes());
            payload.extend_from_slice(&duration.to_be_bytes());
        }
        atom(b"stts", &payload)
    }

    fn stss(sync_samples: &[u32]) -> Vec<u8> {
        let mut payload = Vec::new();
        payload.extend_from_slice(&[0, 0, 0, 0]);
        payload.extend_from_slice(&(sync_samples.len() as u32).to_be_bytes());
        for sample in sync_samples {
            payload.extend_from_slice(&sample.to_be_bytes());
        }
        atom(b"stss", &payload)
    }

    fn stsc(entries: &[(u32, u32)]) -> Vec<u8> {
        let mut payload = Vec::new();
        payload.extend_from_slice(&[0, 0, 0, 0]);
        payload.extend_from_slice(&(entries.len() as u32).to_be_bytes());
        for (first_chunk, samples_per_chunk) in entries {
            payload.extend_from_slice(&first_chunk.to_be_bytes());
            payload.extend_from_slice(&samples_per_chunk.to_be_bytes());
            payload.extend_from_slice(&1_u32.to_be_bytes());
        }
        atom(b"stsc", &payload)
    }

    fn stsz(sample_sizes: &[u32]) -> Vec<u8> {
        let mut payload = Vec::new();
        payload.extend_from_slice(&[0, 0, 0, 0]);
        payload.extend_from_slice(&0_u32.to_be_bytes());
        payload.extend_from_slice(&(sample_sizes.len() as u32).to_be_bytes());
        for size in sample_sizes {
            payload.extend_from_slice(&size.to_be_bytes());
        }
        atom(b"stsz", &payload)
    }

    fn stco(chunk_offsets: &[u32]) -> Vec<u8> {
        let mut payload = Vec::new();
        payload.extend_from_slice(&[0, 0, 0, 0]);
        payload.extend_from_slice(&(chunk_offsets.len() as u32).to_be_bytes());
        for offset in chunk_offsets {
            payload.extend_from_slice(&offset.to_be_bytes());
        }
        atom(b"stco", &payload)
    }

    fn atom(kind: &[u8; 4], payload: &[u8]) -> Vec<u8> {
        let mut out = Vec::with_capacity(payload.len() + 8);
        out.extend_from_slice(&((payload.len() + 8) as u32).to_be_bytes());
        out.extend_from_slice(kind);
        out.extend_from_slice(payload);
        out
    }
}
