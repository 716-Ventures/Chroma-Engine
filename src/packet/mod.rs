use serde::{Deserialize, Serialize};
use thiserror::Error;

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
/// A compressed packet reference inside the original source.
pub struct PacketRef {
    /// Byte offset where the packet payload starts in the source.
    pub source_offset: u64,
    /// Packet payload size in bytes.
    pub size: u32,
    /// Presentation timestamp.
    pub pts: TimePoint,
    /// Decode timestamp.
    pub dts: TimePoint,
    /// Packet presentation duration.
    pub duration: TimeDelta,
    /// Whether this packet starts at a random access point.
    pub keyframe: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
/// A keyframe-aligned chunking plan for one or more tracks.
pub struct ChunkPlan {
    /// Track identifiers covered by this plan.
    pub track_ids: Vec<String>,
    /// Ordered native chunks.
    pub chunks: Vec<NativeChunk>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
/// A native playback chunk backed by a packet range.
pub struct NativeChunk {
    /// Zero-based chunk index.
    pub index: u32,
    /// Chunk start timestamp.
    pub start: TimePoint,
    /// Chunk duration.
    pub duration: TimeDelta,
    /// Half-open packet range included in the chunk.
    pub packet_range: PacketRange,
    /// Whether the first packet in the chunk is keyframe aligned.
    pub key_aligned: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
/// Materialized packet payload and sample table for a chunk.
pub struct ExtractedChunk {
    /// Track identifier for the extracted chunk.
    pub track_id: String,
    /// Chunk timing and packet range metadata.
    pub chunk: NativeChunk,
    /// Number of packets included.
    pub packet_count: u32,
    /// Total payload bytes included.
    pub byte_count: u64,
    /// Per-packet sample metadata relative to the extracted payload.
    pub samples: Vec<ChunkSample>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
/// Sample metadata for one packet inside an extracted chunk.
pub struct ChunkSample {
    /// Source packet index.
    pub index: u32,
    /// Byte offset inside the extracted payload.
    pub payload_offset: u64,
    /// Sample payload size in bytes.
    pub byte_count: u32,
    /// Presentation timestamp.
    pub pts: TimePoint,
    /// Decode timestamp.
    pub dts: TimePoint,
    /// Sample duration.
    pub duration: TimeDelta,
    /// Whether the sample starts at a random access point.
    pub keyframe: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
/// Borrowed packet payload bytes inside the original source.
pub struct PacketPayloadSpan<'a> {
    /// Source packet index.
    pub packet_index: u32,
    /// Byte offset where the packet payload starts in the source.
    pub source_offset: u64,
    /// Borrowed packet payload bytes.
    pub bytes: &'a [u8],
}

#[derive(Debug, Error, PartialEq, Eq)]
/// Error returned when extracting packet payload bytes.
pub enum PacketExtractError {
    /// The requested range start is greater than its end.
    #[error("packet range start is after range end")]
    InvalidRange,
    /// The requested packet range is outside the packet index.
    #[error("packet range is outside the packet index")]
    RangeOutOfBounds,
    /// A packet byte range points outside the source buffer.
    #[error("packet byte range is outside the source")]
    SourceOutOfBounds,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
/// Half-open packet range `[start, end)`.
pub struct PacketRange {
    /// First packet index included.
    pub start: u32,
    /// First packet index excluded.
    pub end: u32,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq, PartialOrd, Ord)]
#[serde(rename_all = "camelCase")]
/// Absolute media timestamp in a declared time scale.
pub struct TimePoint {
    /// Timestamp units in `scale`.
    pub units: u64,
    /// Units-per-second time scale.
    pub scale: TimeScale,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq, PartialOrd, Ord)]
#[serde(rename_all = "camelCase")]
/// Relative media duration in a declared time scale.
pub struct TimeDelta {
    /// Duration units in `scale`.
    pub units: u64,
    /// Units-per-second time scale.
    pub scale: TimeScale,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq, PartialOrd, Ord)]
#[serde(rename_all = "camelCase")]
/// Rational media time scale represented as units per second.
pub struct TimeScale {
    /// Number of timestamp units in one second.
    pub units_per_second: u32,
}

impl TimeScale {
    /// Millisecond time scale.
    pub const MILLIS: Self = Self {
        units_per_second: 1000,
    };

    /// Converts `units` in this scale to milliseconds.
    pub fn to_millis(self, units: u64) -> u64 {
        if self.units_per_second == 0 {
            return 0;
        }
        units.saturating_mul(1000) / u64::from(self.units_per_second)
    }
}

impl TimePoint {
    /// Returns a zero timestamp in `scale`.
    pub fn zero(scale: TimeScale) -> Self {
        Self { units: 0, scale }
    }

    /// Builds a timestamp from milliseconds.
    pub fn millis(ms: u64) -> Self {
        Self {
            units: ms,
            scale: TimeScale::MILLIS,
        }
    }

    /// Converts the timestamp to milliseconds.
    pub fn as_millis(self) -> u64 {
        self.scale.to_millis(self.units)
    }
}

impl TimeDelta {
    /// Builds a duration from milliseconds.
    pub fn millis(ms: u64) -> Self {
        Self {
            units: ms,
            scale: TimeScale::MILLIS,
        }
    }

    /// Converts the duration to milliseconds.
    pub fn as_millis(self) -> u64 {
        self.scale.to_millis(self.units)
    }
}

/// Builds fixed-duration chunks for an anonymous track.
pub fn plan_fixed_chunks(packets: &[PacketRef], target_ms: u64) -> ChunkPlan {
    plan_track_chunks("", packets, target_ms)
}

/// Builds fixed-duration chunks for a named track.
pub fn plan_track_chunks(track_id: &str, packets: &[PacketRef], target_ms: u64) -> ChunkPlan {
    if packets.is_empty() || target_ms == 0 {
        return ChunkPlan {
            track_ids: Vec::new(),
            chunks: Vec::new(),
        };
    }

    let mut chunks = Vec::new();
    let mut start_idx = 0_u32;
    let mut chunk_start_ms = packets[0].pts.as_millis();
    let mut last_end_ms = chunk_start_ms;
    let mut key_aligned = packets[0].keyframe;

    for (idx, packet) in packets.iter().enumerate() {
        let packet_start_ms = packet.pts.as_millis();
        let packet_end_ms = packet_start_ms.saturating_add(packet.duration.as_millis());
        let should_cut = idx > 0
            && packet.keyframe
            && packet_start_ms.saturating_sub(chunk_start_ms) >= target_ms;
        if should_cut {
            chunks.push(NativeChunk {
                index: chunks.len() as u32,
                start: TimePoint::millis(chunk_start_ms),
                duration: TimeDelta::millis(last_end_ms.saturating_sub(chunk_start_ms)),
                packet_range: PacketRange {
                    start: start_idx,
                    end: idx as u32,
                },
                key_aligned,
            });
            start_idx = idx as u32;
            chunk_start_ms = packet_start_ms;
            key_aligned = packet.keyframe;
        }
        last_end_ms = packet_end_ms;
    }

    chunks.push(NativeChunk {
        index: chunks.len() as u32,
        start: TimePoint::millis(chunk_start_ms),
        duration: TimeDelta::millis(last_end_ms.saturating_sub(chunk_start_ms)),
        packet_range: PacketRange {
            start: start_idx,
            end: packets.len() as u32,
        },
        key_aligned,
    });

    let track_ids = if track_id.is_empty() {
        Vec::new()
    } else {
        vec![track_id.to_string()]
    };

    ChunkPlan { track_ids, chunks }
}

/// Copies packet payload bytes for a range into a contiguous buffer.
pub fn extract_packet_payload(
    source: &[u8],
    packets: &[PacketRef],
    range: PacketRange,
) -> Result<Vec<u8>, PacketExtractError> {
    let spans = packet_payload_spans(source, packets, range)?;
    let byte_count = spans
        .iter()
        .map(|span| span.bytes.len())
        .sum::<usize>()
        .min(source.len());
    let mut out = Vec::with_capacity(byte_count);

    for span in spans {
        out.extend_from_slice(span.bytes);
    }

    Ok(out)
}

/// Returns borrowed packet payload spans for a range without copying media bytes.
pub fn packet_payload_spans<'a>(
    source: &'a [u8],
    packets: &[PacketRef],
    range: PacketRange,
) -> Result<Vec<PacketPayloadSpan<'a>>, PacketExtractError> {
    if range.start > range.end {
        return Err(PacketExtractError::InvalidRange);
    }
    let start = range.start as usize;
    let end = range.end as usize;
    if end > packets.len() {
        return Err(PacketExtractError::RangeOutOfBounds);
    }

    let mut spans = Vec::with_capacity(end.saturating_sub(start));

    for (relative_idx, packet) in packets[start..end].iter().enumerate() {
        let packet_start = packet.source_offset as usize;
        let packet_size = packet.size as usize;
        let packet_end = packet_start
            .checked_add(packet_size)
            .ok_or(PacketExtractError::SourceOutOfBounds)?;
        if packet_end > source.len() {
            return Err(PacketExtractError::SourceOutOfBounds);
        }
        spans.push(PacketPayloadSpan {
            packet_index: range.start + relative_idx as u32,
            source_offset: packet.source_offset,
            bytes: &source[packet_start..packet_end],
        });
    }

    Ok(spans)
}

/// Builds sample metadata for packets in a range.
pub fn packet_samples_for_range(
    packets: &[PacketRef],
    range: PacketRange,
) -> Result<Vec<ChunkSample>, PacketExtractError> {
    if range.start > range.end {
        return Err(PacketExtractError::InvalidRange);
    }
    let start = range.start as usize;
    let end = range.end as usize;
    if end > packets.len() {
        return Err(PacketExtractError::RangeOutOfBounds);
    }

    let mut payload_offset = 0_u64;
    let mut samples = Vec::with_capacity(end.saturating_sub(start));
    for (relative_idx, packet) in packets[start..end].iter().enumerate() {
        samples.push(ChunkSample {
            index: range.start + relative_idx as u32,
            payload_offset,
            byte_count: packet.size,
            pts: packet.pts,
            dts: packet.dts,
            duration: packet.duration,
            keyframe: packet.keyframe,
        });
        payload_offset = payload_offset.saturating_add(u64::from(packet.size));
    }
    Ok(samples)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn fixed_chunks_cut_only_on_keyframes() {
        let packets = vec![
            packet(0, true),
            packet(1000, false),
            packet(2000, true),
            packet(3000, false),
            packet(4000, true),
        ];
        let plan = plan_track_chunks("v0", &packets, 2_000);
        assert_eq!(plan.track_ids, vec!["v0"]);
        assert_eq!(plan.chunks.len(), 3);
        assert_eq!(
            plan.chunks[0].packet_range,
            PacketRange { start: 0, end: 2 }
        );
        assert_eq!(
            plan.chunks[1].packet_range,
            PacketRange { start: 2, end: 4 }
        );
        assert_eq!(
            plan.chunks[2].packet_range,
            PacketRange { start: 4, end: 5 }
        );
    }

    #[test]
    fn extracts_packet_payload_ranges() {
        let source = b"00112233445566778899";
        let packets = vec![packet_at(2, 4), packet_at(10, 2), packet_at(16, 4)];
        let payload =
            extract_packet_payload(source, &packets, PacketRange { start: 0, end: 2 }).unwrap();
        assert_eq!(payload, b"112255");
    }

    #[test]
    fn borrows_packet_payload_spans_without_copying() {
        let source = b"00112233445566778899";
        let packets = vec![packet_at(2, 4), packet_at(10, 2), packet_at(16, 4)];
        let spans =
            packet_payload_spans(source, &packets, PacketRange { start: 1, end: 3 }).unwrap();

        assert_eq!(spans.len(), 2);
        assert_eq!(spans[0].packet_index, 1);
        assert_eq!(spans[0].source_offset, 10);
        assert_eq!(spans[0].bytes, b"55");
        assert_eq!(spans[1].packet_index, 2);
        assert_eq!(spans[1].source_offset, 16);
        assert_eq!(spans[1].bytes, b"8899");
    }

    #[test]
    fn builds_packet_samples_for_range() {
        let packets = vec![packet_at(2, 4), packet_at(10, 2), packet_at(16, 4)];
        let samples = packet_samples_for_range(&packets, PacketRange { start: 1, end: 3 }).unwrap();
        assert_eq!(samples.len(), 2);
        assert_eq!(samples[0].index, 1);
        assert_eq!(samples[0].payload_offset, 0);
        assert_eq!(samples[0].byte_count, 2);
        assert_eq!(samples[1].index, 2);
        assert_eq!(samples[1].payload_offset, 2);
        assert_eq!(samples[1].byte_count, 4);
    }

    fn packet(start_ms: u64, keyframe: bool) -> PacketRef {
        PacketRef {
            source_offset: start_ms,
            size: 10,
            pts: TimePoint::millis(start_ms),
            dts: TimePoint::millis(start_ms),
            duration: TimeDelta::millis(1000),
            keyframe,
        }
    }

    fn packet_at(source_offset: u64, size: u32) -> PacketRef {
        PacketRef {
            source_offset,
            size,
            pts: TimePoint::millis(0),
            dts: TimePoint::millis(0),
            duration: TimeDelta::millis(1),
            keyframe: true,
        }
    }
}
