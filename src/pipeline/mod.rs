use thiserror::Error;

use crate::{
    error::EngineErrorCode,
    packet::{ChunkSample, ExtractedChunk, NativeChunk},
    session::{PipelineStage, PlaybackPlan, StageKind, StageMode},
};

/// Error returned while routing chunks through a playback pipeline plan.
#[derive(Debug, Error, PartialEq, Eq)]
pub enum PipelineError {
    /// A chunk references a track that is not selected by the playback plan.
    #[error("track {0} is not selected by the playback plan")]
    UnselectedTrack(String),
    /// A selected chunk has no compatible packet, decode, or encode stage.
    #[error("track {0} has no compatible pipeline stage")]
    MissingStage(String),
}

impl PipelineError {
    /// Returns the stable Chroma Engine error code for this pipeline failure.
    pub fn code(&self) -> EngineErrorCode {
        EngineErrorCode::PipelineFailed
    }
}

/// A media chunk emitted from a concrete pipeline stage.
#[derive(Debug, PartialEq, Eq)]
pub struct PipelineStageChunk<'a> {
    /// Stable stage identifier from the playback plan.
    pub stage_id: String,
    /// Stage responsibility.
    pub stage_kind: StageKind,
    /// Stage execution mode.
    pub stage_mode: StageMode,
    /// Track id handled by this chunk.
    pub track_id: String,
    /// Timing and packet range for this chunk.
    pub chunk: NativeChunk,
    /// Stage-specific chunk payload.
    pub payload: PipelineStagePayload<'a>,
}

/// Payload emitted by a pipeline stage.
#[derive(Debug, PartialEq, Eq)]
pub enum PipelineStagePayload<'a> {
    /// Compressed packet bytes copied from the source.
    PacketCopy {
        /// Sample table matching the packet payload.
        samples: Vec<ChunkSample>,
        /// Borrowed compressed packet payload.
        bytes: &'a [u8],
    },
    /// Compressed packet bytes that need decode.
    DecodeInput {
        /// Sample table matching the packet payload.
        samples: Vec<ChunkSample>,
        /// Borrowed compressed packet payload.
        bytes: &'a [u8],
    },
    /// Encode work item produced after decode.
    EncodeInput {
        /// Number of decoded frames expected from the source packet window.
        expected_frame_count: u32,
    },
}

/// Emits packet/decode/encode stage chunks for extracted source chunks.
pub fn emit_stage_chunks<'a>(
    plan: &PlaybackPlan,
    chunks: &'a [(ExtractedChunk, Vec<u8>)],
) -> Result<Vec<PipelineStageChunk<'a>>, PipelineError> {
    let mut out = Vec::new();
    for (extracted, payload) in chunks {
        if !plan.selected_tracks.contains(&extracted.track_id) {
            return Err(PipelineError::UnselectedTrack(extracted.track_id.clone()));
        }

        let mut emitted = false;
        if let Some(stage) = stage_for_track(
            &plan.stages,
            &extracted.track_id,
            StageKind::PacketFilter,
            StageMode::Copy,
        ) {
            out.push(packet_copy_chunk(stage, extracted, payload));
            emitted = true;
        }

        if let Some(stage) = stage_for_track(
            &plan.stages,
            &extracted.track_id,
            StageKind::Decode,
            StageMode::Decode,
        ) {
            out.push(decode_input_chunk(stage, extracted, payload));
            emitted = true;
        }

        if let Some(stage) = stage_for_track(
            &plan.stages,
            &extracted.track_id,
            StageKind::Encode,
            StageMode::Encode,
        ) {
            out.push(encode_input_chunk(stage, extracted));
            emitted = true;
        }

        if !emitted {
            return Err(PipelineError::MissingStage(extracted.track_id.clone()));
        }
    }

    Ok(out)
}

fn stage_for_track<'a>(
    stages: &'a [PipelineStage],
    track_id: &str,
    kind: StageKind,
    mode: StageMode,
) -> Option<&'a PipelineStage> {
    stages.iter().find(|stage| {
        stage.kind == kind && stage.mode == mode && stage.track_ids.iter().any(|id| id == track_id)
    })
}

fn packet_copy_chunk<'a>(
    stage: &PipelineStage,
    extracted: &ExtractedChunk,
    payload: &'a [u8],
) -> PipelineStageChunk<'a> {
    PipelineStageChunk {
        stage_id: stage.id.clone(),
        stage_kind: stage.kind,
        stage_mode: stage.mode,
        track_id: extracted.track_id.clone(),
        chunk: extracted.chunk.clone(),
        payload: PipelineStagePayload::PacketCopy {
            samples: extracted.samples.clone(),
            bytes: payload,
        },
    }
}

fn decode_input_chunk<'a>(
    stage: &PipelineStage,
    extracted: &ExtractedChunk,
    payload: &'a [u8],
) -> PipelineStageChunk<'a> {
    PipelineStageChunk {
        stage_id: stage.id.clone(),
        stage_kind: stage.kind,
        stage_mode: stage.mode,
        track_id: extracted.track_id.clone(),
        chunk: extracted.chunk.clone(),
        payload: PipelineStagePayload::DecodeInput {
            samples: extracted.samples.clone(),
            bytes: payload,
        },
    }
}

fn encode_input_chunk<'a>(
    stage: &PipelineStage,
    extracted: &ExtractedChunk,
) -> PipelineStageChunk<'a> {
    PipelineStageChunk {
        stage_id: stage.id.clone(),
        stage_kind: stage.kind,
        stage_mode: stage.mode,
        track_id: extracted.track_id.clone(),
        chunk: extracted.chunk.clone(),
        payload: PipelineStagePayload::EncodeInput {
            expected_frame_count: extracted.samples.len() as u32,
        },
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{
        packet::{ChunkSample, PacketRange, TimeDelta, TimePoint},
        session::{PlaybackConstraints, PlaybackTarget, TransportKind, TransportPlan},
    };

    #[test]
    fn emits_copy_chunks_for_packet_copy_tracks() {
        let plan = playback_plan(vec![PipelineStage {
            id: "packet-copy0".to_string(),
            kind: StageKind::PacketFilter,
            track_ids: vec!["v0".to_string()],
            mode: StageMode::Copy,
            reason: "copy".to_string(),
        }]);
        let chunks = vec![(extracted_chunk("v0"), vec![1, 2, 3])];

        let emitted = emit_stage_chunks(&plan, &chunks).unwrap();

        assert_eq!(emitted.len(), 1);
        assert_eq!(emitted[0].stage_id, "packet-copy0");
        assert!(matches!(
            emitted[0].payload,
            PipelineStagePayload::PacketCopy {
                bytes: [1, 2, 3],
                ..
            }
        ));
    }

    #[test]
    fn emits_decode_and_encode_chunks_for_transformed_tracks() {
        let plan = playback_plan(vec![
            PipelineStage {
                id: "decode0".to_string(),
                kind: StageKind::Decode,
                track_ids: vec!["v0".to_string()],
                mode: StageMode::Decode,
                reason: "decode".to_string(),
            },
            PipelineStage {
                id: "encode0".to_string(),
                kind: StageKind::Encode,
                track_ids: vec!["v0".to_string()],
                mode: StageMode::Encode,
                reason: "encode".to_string(),
            },
        ]);
        let chunks = vec![(extracted_chunk("v0"), vec![9, 8, 7])];

        let emitted = emit_stage_chunks(&plan, &chunks).unwrap();

        assert_eq!(emitted.len(), 2);
        assert!(matches!(
            emitted[0].payload,
            PipelineStagePayload::DecodeInput {
                bytes: [9, 8, 7],
                ..
            }
        ));
        assert!(matches!(
            emitted[1].payload,
            PipelineStagePayload::EncodeInput {
                expected_frame_count: 1
            }
        ));
    }

    #[test]
    fn rejects_unselected_track_chunks() {
        let plan = playback_plan(Vec::new());
        let chunks = vec![(extracted_chunk("a0"), vec![1])];

        let err = emit_stage_chunks(&plan, &chunks).unwrap_err();

        assert_eq!(err, PipelineError::UnselectedTrack("a0".to_string()));
        assert_eq!(err.code(), EngineErrorCode::PipelineFailed);
    }

    fn playback_plan(stages: Vec<PipelineStage>) -> PlaybackPlan {
        PlaybackPlan {
            schema_version: 1,
            source_path: "/tmp/movie.mkv".to_string(),
            duration_ms: Some(1_000),
            selected_tracks: vec!["v0".to_string()],
            stages,
            transports: vec![TransportPlan {
                id: "segments0".to_string(),
                kind: TransportKind::ChromaSegments,
                source_stage_id: "mux0".to_string(),
                notes: Vec::new(),
            }],
            constraints: PlaybackConstraints {
                target: PlaybackTarget::NativeChroma,
                ..PlaybackConstraints::default()
            },
        }
    }

    fn extracted_chunk(track_id: &str) -> ExtractedChunk {
        ExtractedChunk {
            track_id: track_id.to_string(),
            chunk: NativeChunk {
                index: 0,
                start: TimePoint::millis(0),
                duration: TimeDelta::millis(1_000),
                packet_range: PacketRange { start: 0, end: 1 },
                key_aligned: true,
            },
            packet_count: 1,
            byte_count: 3,
            samples: vec![ChunkSample {
                index: 0,
                payload_offset: 0,
                byte_count: 3,
                pts: TimePoint::millis(0),
                dts: TimePoint::millis(0),
                duration: TimeDelta::millis(1_000),
                keyframe: true,
            }],
        }
    }
}
