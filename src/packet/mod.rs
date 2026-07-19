use serde::{Deserialize, Serialize};

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
}
