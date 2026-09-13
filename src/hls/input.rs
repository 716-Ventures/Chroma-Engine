//! Shared owned-payload access for file sessions and in-memory test fixtures.
use super::*;

pub(super) trait HlsInput {
    fn output_limit(&self) -> usize {
        128 * 1024 * 1024
    }
    fn compressed_limit(&self) -> usize {
        64 * 1024 * 1024
    }
    fn metadata(&self) -> &[u8];
    fn source_len(&self) -> u64;
    fn payload(&self, packets: &[PacketRef]) -> Result<Vec<u8>>;
    fn matroska_packets(
        &self,
        tracks: &[&str],
        start: u64,
        end: u64,
    ) -> Result<Vec<matroska::MatroskaPacketTrack>>;
    fn matroska_plan(&self, track: &str, target: u64) -> Result<ChunkPlan>;
}

impl HlsInput for MappedMediaFile {
    fn output_limit(&self) -> usize {
        self.policy().output_bytes
    }
    fn compressed_limit(&self) -> usize {
        self.policy().compressed_window_bytes
    }
    fn metadata(&self) -> &[u8] {
        self.as_ref()
    }
    fn source_len(&self) -> u64 {
        self.len()
    }
    fn payload(&self, packets: &[PacketRef]) -> Result<Vec<u8>> {
        Ok(self.read_packets(packets)?)
    }
    fn matroska_packets(
        &self,
        tracks: &[&str],
        start: u64,
        end: u64,
    ) -> Result<Vec<matroska::MatroskaPacketTrack>> {
        Ok(self.matroska_index()?.packets(self, tracks, start, end)?)
    }
    fn matroska_plan(&self, track: &str, target: u64) -> Result<ChunkPlan> {
        Ok(self.matroska_index()?.plan(self, track, target)?)
    }
}

#[cfg(test)]
impl HlsInput for [u8] {
    fn metadata(&self) -> &[u8] {
        self
    }
    fn source_len(&self) -> u64 {
        self.len() as u64
    }
    fn payload(&self, packets: &[PacketRef]) -> Result<Vec<u8>> {
        Ok(crate::packet::extract_packet_payload(
            self,
            packets,
            crate::packet::PacketRange {
                start: 0,
                end: u32::try_from(packets.len()).map_err(|_| anyhow!("packet count overflow"))?,
            },
        )
        .map_err(anyhow::Error::from)?)
    }
    fn matroska_packets(
        &self,
        tracks: &[&str],
        start: u64,
        end: u64,
    ) -> Result<Vec<matroska::MatroskaPacketTrack>> {
        matroska::parse_packet_tracks_in_time_window(self, tracks, start, end)
            .ok_or_else(|| anyhow!("missing Matroska packet window").into())
    }
    fn matroska_plan(&self, track: &str, target: u64) -> Result<ChunkPlan> {
        matroska::parse_chunk_plan(self, Some(track), target)
            .ok_or_else(|| anyhow!("missing Matroska chunk plan").into())
    }
}

#[cfg(test)]
macro_rules! fixture_input {
    ($type:ty $(, $generic:ident)?) => {
        impl $(<const $generic: usize>)? HlsInput for $type {
            fn metadata(&self) -> &[u8] { self.as_slice() }
            fn source_len(&self) -> u64 { self.len() as u64 }
            fn payload(&self, packets: &[PacketRef]) -> Result<Vec<u8>> { HlsInput::payload(self.as_slice(), packets) }
            fn matroska_packets(&self, tracks: &[&str], start: u64, end: u64) -> Result<Vec<matroska::MatroskaPacketTrack>> { HlsInput::matroska_packets(self.as_slice(), tracks, start, end) }
            fn matroska_plan(&self, track: &str, target: u64) -> Result<ChunkPlan> { HlsInput::matroska_plan(self.as_slice(), track, target) }
        }
    };
}
#[cfg(test)]
fixture_input!(Vec<u8>);
#[cfg(test)]
fixture_input!([u8; N], N);
