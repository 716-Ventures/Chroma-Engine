use serde::{Deserialize, Serialize};
use thiserror::Error;

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct PacketRef {
    pub source_offset: u64,
    pub size: u32,
    pub pts: TimePoint,
    pub dts: TimePoint,
    pub duration: TimeDelta,
    pub keyframe: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct ChunkPlan {
    pub track_ids: Vec<String>,
    pub chunks: Vec<NativeChunk>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct NativeChunk {
    pub index: u32,
    pub start: TimePoint,
    pub duration: TimeDelta,
    pub packet_range: PacketRange,
    pub key_aligned: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct ExtractedChunk {
    pub track_id: String,
    pub chunk: NativeChunk,
    pub packet_count: u32,
    pub byte_count: u64,
}

#[derive(Debug, Error, PartialEq, Eq)]
pub enum PacketExtractError {
    #[error("packet range start is after range end")]
    InvalidRange,
    #[error("packet range is outside the packet index")]
    RangeOutOfBounds,
    #[error("packet byte range is outside the source")]
    SourceOutOfBounds,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct PacketRange {
    pub start: u32,
    pub end: u32,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq, PartialOrd, Ord)]
#[serde(rename_all = "camelCase")]
pub struct TimePoint {
    pub units: u64,
    pub scale: TimeScale,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq, PartialOrd, Ord)]
#[serde(rename_all = "camelCase")]
pub struct TimeDelta {
    pub units: u64,
    pub scale: TimeScale,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq, PartialOrd, Ord)]
#[serde(rename_all = "camelCase")]
pub struct TimeScale {
    pub units_per_second: u32,
}

impl TimeScale {
    pub const MILLIS: Self = Self {
        units_per_second: 1000,
    };

    pub fn to_millis(self, units: u64) -> u64 {
        if self.units_per_second == 0 {
            return 0;
        }
        units.saturating_mul(1000) / u64::from(self.units_per_second)
    }
}

impl TimePoint {
    pub fn zero(scale: TimeScale) -> Self {
        Self { units: 0, scale }
    }

    pub fn millis(ms: u64) -> Self {
        Self {
            units: ms,
            scale: TimeScale::MILLIS,
        }
    }

    pub fn as_millis(self) -> u64 {
        self.scale.to_millis(self.units)
    }
}

impl TimeDelta {
    pub fn millis(ms: u64) -> Self {
        Self {
            units: ms,
            scale: TimeScale::MILLIS,
        }
    }

    pub fn as_millis(self) -> u64 {
        self.scale.to_millis(self.units)
    }
}

pub fn plan_fixed_chunks(packets: &[PacketRef], target_ms: u64) -> ChunkPlan {
    plan_track_chunks("", packets, target_ms)
}

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

pub fn extract_packet_payload(
    source: &[u8],
    packets: &[PacketRef],
    range: PacketRange,
) -> Result<Vec<u8>, PacketExtractError> {
    if range.start > range.end {
        return Err(PacketExtractError::InvalidRange);
    }
    let start = range.start as usize;
    let end = range.end as usize;
    if end > packets.len() {
        return Err(PacketExtractError::RangeOutOfBounds);
    }

    let byte_count = packets[start..end]
        .iter()
        .map(|packet| u64::from(packet.size))
        .sum::<u64>();
    let capacity = usize::try_from(byte_count).unwrap_or(usize::MAX);
    let mut out = Vec::with_capacity(capacity.min(source.len()));

    for packet in &packets[start..end] {
        let packet_start = packet.source_offset as usize;
        let packet_size = packet.size as usize;
        let packet_end = packet_start
            .checked_add(packet_size)
            .ok_or(PacketExtractError::SourceOutOfBounds)?;
        if packet_end > source.len() {
            return Err(PacketExtractError::SourceOutOfBounds);
        }
        out.extend_from_slice(&source[packet_start..packet_end]);
    }

    Ok(out)
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
