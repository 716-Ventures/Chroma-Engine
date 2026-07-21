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
/// Keyframe-aware input seek anchor for compressed packet-copy playback.
pub struct SeekAnchor {
    /// Requested playback timestamp in milliseconds.
    pub target_ms: u64,
    /// Timestamp where source reads must begin.
    pub anchor_ms: u64,
    /// Packet index where source reads must begin.
    pub packet_index: u32,
    /// Media time between the read anchor and requested target.
    pub preroll_ms: u64,
    /// Whether the anchor packet is a random access point.
    pub key_aligned: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
/// Coarse packet-copy seek plan for one native source track.
pub struct CopySeekPlan {
    /// Track identifier covered by the seek plan.
    pub track_id: String,
    /// Requested playback timestamp in milliseconds.
    pub target_ms: u64,
    /// Keyframe-aware read anchor.
    pub anchor: SeekAnchor,
    /// Chunk of packets to copy after anchoring.
    pub chunk: NativeChunk,
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

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
/// Absolute media timestamp in a declared time scale.
pub struct TimePoint {
    /// Timestamp units in `scale`.
    pub units: u64,
    /// Units-per-second time scale.
    pub scale: TimeScale,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
/// Relative media duration in a declared time scale.
pub struct TimeDelta {
    /// Duration units in `scale`.
    pub units: u64,
    /// Units-per-second time scale.
    pub scale: TimeScale,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
/// Rational media time scale represented as units per second.
pub struct TimeScale {
    /// Number of timestamp units in one second.
    pub units_per_second: u32,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
/// Signed timestamp in a validated rational time base.
pub struct SignedTimePoint {
    /// Signed timestamp units in `scale`.
    pub units: i64,
    /// Units-per-second time scale.
    pub scale: TimeScale,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
/// Signed relative media duration in a validated rational time base.
pub struct SignedTimeDelta {
    /// Signed duration units in `scale`.
    pub units: i64,
    /// Units-per-second time scale.
    pub scale: TimeScale,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
/// Rounding policy for checked timestamp rescaling.
pub enum TimeRounding {
    /// Round toward negative infinity.
    Floor,
    /// Round toward zero.
    Truncate,
    /// Round to the nearest integer, half away from zero.
    Nearest,
    /// Round toward positive infinity.
    Ceil,
}

#[derive(Debug, Error, PartialEq, Eq)]
/// Error returned when timestamp construction or rescaling cannot preserve invariants.
pub enum TimeError {
    /// Time scale must be greater than zero.
    #[error("time scale must be greater than zero")]
    ZeroScale,
    /// Rescaled timestamp does not fit the requested integer type.
    #[error("rescaled timestamp overflowed")]
    Overflow,
}

impl TimeScale {
    /// Millisecond time scale.
    pub const MILLIS: Self = Self {
        units_per_second: 1000,
    };

    /// Builds a non-zero time scale.
    pub fn new(units_per_second: u32) -> Result<Self, TimeError> {
        if units_per_second == 0 {
            return Err(TimeError::ZeroScale);
        }
        Ok(Self { units_per_second })
    }

    /// Converts `units` in this scale to milliseconds.
    pub fn to_millis(self, units: u64) -> u64 {
        self.checked_rescale_u64(units, Self::MILLIS, TimeRounding::Truncate)
            .unwrap_or(0)
    }

    /// Converts unsigned `units` in this scale to `target` with checked arithmetic.
    pub fn checked_rescale_u64(
        self,
        units: u64,
        target: Self,
        rounding: TimeRounding,
    ) -> Result<u64, TimeError> {
        if self.units_per_second == 0 || target.units_per_second == 0 {
            return Err(TimeError::ZeroScale);
        }
        let value = rescale_signed(
            i128::from(units),
            i128::from(target.units_per_second),
            i128::from(self.units_per_second),
            rounding,
        )?;
        u64::try_from(value).map_err(|_| TimeError::Overflow)
    }

    /// Converts signed `units` in this scale to `target` with checked arithmetic.
    pub fn checked_rescale_i64(
        self,
        units: i64,
        target: Self,
        rounding: TimeRounding,
    ) -> Result<i64, TimeError> {
        if self.units_per_second == 0 || target.units_per_second == 0 {
            return Err(TimeError::ZeroScale);
        }
        let value = rescale_signed(
            i128::from(units),
            i128::from(target.units_per_second),
            i128::from(self.units_per_second),
            rounding,
        )?;
        i64::try_from(value).map_err(|_| TimeError::Overflow)
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

    /// Converts to a signed timestamp without changing the time scale.
    pub fn to_signed(self) -> Result<SignedTimePoint, TimeError> {
        Ok(SignedTimePoint {
            units: i64::try_from(self.units).map_err(|_| TimeError::Overflow)?,
            scale: self.scale,
        })
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

    /// Converts to a signed duration without changing the time scale.
    pub fn to_signed(self) -> Result<SignedTimeDelta, TimeError> {
        Ok(SignedTimeDelta {
            units: i64::try_from(self.units).map_err(|_| TimeError::Overflow)?,
            scale: self.scale,
        })
    }
}

impl SignedTimePoint {
    /// Builds a signed timestamp from native units and a non-zero time scale.
    pub fn new(units: i64, scale: TimeScale) -> Result<Self, TimeError> {
        TimeScale::new(scale.units_per_second)?;
        Ok(Self { units, scale })
    }

    /// Rescales this timestamp into a target time scale.
    pub fn rescale(self, target: TimeScale, rounding: TimeRounding) -> Result<Self, TimeError> {
        Ok(Self {
            units: self
                .scale
                .checked_rescale_i64(self.units, target, rounding)?,
            scale: target,
        })
    }
}

impl SignedTimeDelta {
    /// Builds a signed duration from native units and a non-zero time scale.
    pub fn new(units: i64, scale: TimeScale) -> Result<Self, TimeError> {
        TimeScale::new(scale.units_per_second)?;
        Ok(Self { units, scale })
    }

    /// Rescales this duration into a target time scale.
    pub fn rescale(self, target: TimeScale, rounding: TimeRounding) -> Result<Self, TimeError> {
        Ok(Self {
            units: self
                .scale
                .checked_rescale_i64(self.units, target, rounding)?,
            scale: target,
        })
    }
}

fn rescale_signed(
    units: i128,
    numerator: i128,
    denominator: i128,
    rounding: TimeRounding,
) -> Result<i128, TimeError> {
    if denominator == 0 {
        return Err(TimeError::ZeroScale);
    }
    let scaled = units.checked_mul(numerator).ok_or(TimeError::Overflow)?;
    let quotient = scaled / denominator;
    let remainder = scaled % denominator;
    if remainder == 0 {
        return Ok(quotient);
    }
    let same_sign = (scaled >= 0) == (denominator >= 0);
    let adjustment = match rounding {
        TimeRounding::Truncate => 0,
        TimeRounding::Floor => {
            if same_sign {
                0
            } else {
                -1
            }
        }
        TimeRounding::Ceil => {
            if same_sign {
                1
            } else {
                0
            }
        }
        TimeRounding::Nearest => {
            let doubled_remainder = remainder.abs().checked_mul(2).ok_or(TimeError::Overflow)?;
            if doubled_remainder >= denominator.abs() {
                if same_sign { 1 } else { -1 }
            } else {
                0
            }
        }
    };
    quotient.checked_add(adjustment).ok_or(TimeError::Overflow)
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

/// Finds the packet-copy read anchor at or before a requested timestamp.
pub fn seek_anchor_for_packets(packets: &[PacketRef], target_ms: u64) -> Option<SeekAnchor> {
    if packets.is_empty() {
        return None;
    }

    let mut fallback_idx = 0_usize;
    let mut keyframe_idx = None;
    for (idx, packet) in packets.iter().enumerate() {
        if packet.pts.as_millis() > target_ms {
            break;
        }
        fallback_idx = idx;
        if packet.keyframe {
            keyframe_idx = Some(idx);
        }
    }

    let packet_index = keyframe_idx.unwrap_or(fallback_idx);
    let packet = &packets[packet_index];
    let anchor_ms = packet.pts.as_millis();
    Some(SeekAnchor {
        target_ms,
        anchor_ms,
        packet_index: packet_index as u32,
        preroll_ms: target_ms.saturating_sub(anchor_ms),
        key_aligned: packet.keyframe,
    })
}

/// Builds a coarse packet-copy chunk that starts on the seek anchor.
pub fn plan_copy_seek(
    track_id: &str,
    packets: &[PacketRef],
    target_ms: u64,
    window_ms: u64,
) -> Option<CopySeekPlan> {
    let anchor = seek_anchor_for_packets(packets, target_ms)?;
    let start = anchor.packet_index as usize;
    let target_end_ms = target_ms.saturating_add(window_ms.max(1));
    let mut end = start.saturating_add(1).min(packets.len());

    for (idx, packet) in packets.iter().enumerate().skip(start + 1) {
        if packet.pts.as_millis() >= target_end_ms {
            end = idx;
            break;
        }
        end = idx.saturating_add(1);
    }

    let last_end_ms = packets[start..end]
        .iter()
        .map(|packet| {
            packet
                .pts
                .as_millis()
                .saturating_add(packet.duration.as_millis())
        })
        .max()
        .unwrap_or(anchor.anchor_ms);

    Some(CopySeekPlan {
        track_id: track_id.to_string(),
        target_ms,
        anchor: anchor.clone(),
        chunk: NativeChunk {
            index: 0,
            start: TimePoint::millis(anchor.anchor_ms),
            duration: TimeDelta::millis(last_end_ms.saturating_sub(anchor.anchor_ms)),
            packet_range: PacketRange {
                start: anchor.packet_index,
                end: end as u32,
            },
            key_aligned: anchor.key_aligned,
        },
    })
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
    fn seek_anchor_rolls_back_to_previous_keyframe() {
        let packets = vec![
            packet(0, true),
            packet(1000, false),
            packet(2000, false),
            packet(3000, true),
        ];

        let anchor = seek_anchor_for_packets(&packets, 2_500).unwrap();

        assert_eq!(
            anchor,
            SeekAnchor {
                target_ms: 2_500,
                anchor_ms: 0,
                packet_index: 0,
                preroll_ms: 2_500,
                key_aligned: true,
            }
        );
    }

    #[test]
    fn seek_anchor_uses_first_packet_when_no_prior_keyframe_exists() {
        let packets = vec![packet(0, false), packet(1000, true)];

        let anchor = seek_anchor_for_packets(&packets, 500).unwrap();

        assert_eq!(anchor.packet_index, 0);
        assert!(!anchor.key_aligned);
    }

    #[test]
    fn copy_seek_plan_starts_at_anchor_and_extends_to_requested_window() {
        let packets = vec![
            packet(0, true),
            packet(1000, false),
            packet(2000, false),
            packet(3000, true),
            packet(4000, false),
        ];

        let plan = plan_copy_seek("v0", &packets, 2_500, 1_500).unwrap();

        assert_eq!(plan.track_id, "v0");
        assert_eq!(plan.anchor.packet_index, 0);
        assert_eq!(plan.chunk.start, TimePoint::millis(0));
        assert_eq!(plan.chunk.packet_range, PacketRange { start: 0, end: 4 });
        assert!(plan.chunk.key_aligned);
    }

    #[test]
    fn seek_anchor_rejects_empty_packet_indexes() {
        assert_eq!(seek_anchor_for_packets(&[], 1_000), None);
        assert_eq!(plan_copy_seek("v0", &[], 1_000, 4_000), None);
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

    #[test]
    fn signed_time_preserves_negative_offsets() {
        let scale = TimeScale::new(90_000).unwrap();
        let point = SignedTimePoint::new(-45_000, scale).unwrap();

        assert_eq!(
            point.rescale(TimeScale::MILLIS, TimeRounding::Truncate),
            Ok(SignedTimePoint {
                units: -500,
                scale: TimeScale::MILLIS,
            })
        );
    }

    #[test]
    fn checked_rescale_supports_fractional_frame_rates() {
        let scale = TimeScale::new(24_000).unwrap();
        let target = TimeScale::new(1_001).unwrap();

        assert_eq!(
            scale.checked_rescale_u64(1, target, TimeRounding::Nearest),
            Ok(0)
        );
        assert_eq!(
            scale.checked_rescale_u64(24_000, target, TimeRounding::Nearest),
            Ok(1_001)
        );
    }

    #[test]
    fn checked_rescale_rejects_zero_scale_and_overflow() {
        assert_eq!(TimeScale::new(0), Err(TimeError::ZeroScale));
        assert_eq!(
            TimeScale::MILLIS.checked_rescale_u64(
                u64::MAX,
                TimeScale::new(u32::MAX).unwrap(),
                TimeRounding::Nearest,
            ),
            Err(TimeError::Overflow)
        );
    }

    #[test]
    fn signed_rescale_rounding_modes_are_explicit() {
        let scale = TimeScale::new(3).unwrap();
        let target = TimeScale::new(2).unwrap();

        assert_eq!(
            scale.checked_rescale_i64(-2, target, TimeRounding::Truncate),
            Ok(-1)
        );
        assert_eq!(
            scale.checked_rescale_i64(-2, target, TimeRounding::Floor),
            Ok(-2)
        );
        assert_eq!(
            scale.checked_rescale_i64(2, target, TimeRounding::Ceil),
            Ok(2)
        );
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
