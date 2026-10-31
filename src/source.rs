use std::borrow::Cow;
use std::{
    fs::{self, File},
    io,
    path::{Path, PathBuf},
    time::SystemTime,
};

use anyhow::{Context, Result};
use std::sync::Mutex;
use std::sync::atomic::{AtomicU64, Ordering};
pub(crate) mod probe_reader;

const METADATA_LOOKAHEAD_BYTES: usize = 1024 * 1024;
const METADATA_INITIAL_BYTES: usize = 8 * 1024;
const PACKET_GAP_BYTES: u64 = 256;
const PACKET_COALESCE_BYTES: usize = 64 * 1024;
const MAX_METADATA_PREFIX_BYTES: usize = 64;

/// A bounded diagnostic/tuning ceiling; default adapts up to one MiB. The
/// resource policy's remaining metadata budget always takes precedence.
fn metadata_lookahead_limit() -> usize {
    static LIMIT: std::sync::OnceLock<usize> = std::sync::OnceLock::new();
    *LIMIT.get_or_init(|| {
        std::env::var("CHROMA_METADATA_LOOKAHEAD_BYTES")
            .ok()
            .and_then(|value| value.parse::<usize>().ok())
            .unwrap_or(METADATA_LOOKAHEAD_BYTES)
            .clamp(METADATA_INITIAL_BYTES, METADATA_LOOKAHEAD_BYTES)
    })
}

/// One bounded lookahead window for neighbouring EBML headers. Packet payload
/// reads remain positional and never consult this window.
#[derive(Default)]
struct MetadataWindow {
    offset: u64,
    bytes: Vec<u8>,
}

impl MetadataWindow {
    fn read(
        &mut self,
        offset: u64,
        len: usize,
        source_len: u64,
        capacity_limit: usize,
        mut read: impl FnMut(&mut [u8], u64) -> io::Result<()>,
    ) -> io::Result<Vec<u8>> {
        if len > MAX_METADATA_PREFIX_BYTES
            || offset
                .checked_add(len as u64)
                .is_none_or(|end| end > source_len)
        {
            return Err(io::Error::new(
                io::ErrorKind::UnexpectedEof,
                "metadata prefix outside source",
            ));
        }
        if len == 0 {
            return Ok(Vec::new());
        }
        if self.bytes.capacity() > capacity_limit.min(METADATA_LOOKAHEAD_BYTES) {
            self.bytes = Vec::new();
        }
        if let Some(start) = offset
            .checked_sub(self.offset)
            .and_then(|relative| usize::try_from(relative).ok())
            && let Some(end) = start.checked_add(len)
            && let Some(bytes) = self.bytes.get(start..end)
        {
            return Ok(bytes.to_vec());
        }
        // Grow only for a forward, dense walk. Sparse cluster/seek lookups
        // keep the small first-read cost instead of pulling a MiB per jump.
        let dense = !self.bytes.is_empty()
            && offset >= self.offset
            && offset - self.offset <= (self.bytes.len() as u64).saturating_add(128 * 1024);
        let lookahead = if dense {
            self.bytes.len().saturating_mul(2)
        } else {
            METADATA_INITIAL_BYTES
        };
        let fill = capacity_limit
            .min(METADATA_LOOKAHEAD_BYTES)
            .min(lookahead)
            .min(usize::try_from(source_len - offset).unwrap_or(usize::MAX));
        if fill < len {
            // Metadata can occupy the complete custom budget. Keep the old
            // exact header read instead of retaining an over-budget cache.
            let mut bytes = bounded_buffer(len)?;
            read(&mut bytes, offset)?;
            return Ok(bytes);
        }
        self.bytes.clear();
        self.bytes
            .try_reserve_exact(fill)
            .map_err(io::Error::other)?;
        self.bytes.resize(fill, 0);
        if let Err(error) = read(&mut self.bytes, offset) {
            self.bytes.clear();
            return Err(error);
        }
        self.offset = offset;
        Ok(self.bytes[..len].to_vec())
    }
}

/// Packet reads are separate from container metadata, so parsers never need a
/// file-sized byte slice merely to copy a selected payload.
pub(crate) trait PacketSource {
    fn packet_window(&self, offset: u64, len: usize) -> io::Result<Vec<u8>>;
    fn packet_payload(
        &self,
        packets: &[crate::packet::PacketRef],
        range: crate::packet::PacketRange,
    ) -> Result<Vec<u8>>;
}

impl PacketSource for MediaSource {
    fn packet_window(&self, offset: u64, len: usize) -> io::Result<Vec<u8>> {
        self.read_at(offset, len)
    }
    fn packet_payload(
        &self,
        packets: &[crate::packet::PacketRef],
        range: crate::packet::PacketRange,
    ) -> Result<Vec<u8>> {
        let packets = packets
            .get(range.start as usize..range.end as usize)
            .ok_or_else(|| anyhow::anyhow!("invalid packet range"))?;
        Ok(self.read_packets(packets)?)
    }
}

#[cfg(test)]
impl PacketSource for Vec<u8> {
    fn packet_window(&self, offset: u64, len: usize) -> io::Result<Vec<u8>> {
        let start = usize::try_from(offset).map_err(io::Error::other)?;
        let end = start
            .checked_add(len)
            .ok_or_else(|| io::Error::other("range overflow"))?;
        self.get(start..end)
            .map(<[u8]>::to_vec)
            .ok_or_else(|| io::Error::new(io::ErrorKind::UnexpectedEof, "packet outside source"))
    }
    fn packet_payload(
        &self,
        packets: &[crate::packet::PacketRef],
        range: crate::packet::PacketRange,
    ) -> Result<Vec<u8>> {
        Ok(crate::packet::extract_packet_payload(self, packets, range)?)
    }
}

/// Immutable identity captured when a media source is opened.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MediaSourceIdentity {
    len: u64,
    modified: Option<SystemTime>,
    #[cfg(unix)]
    device: u64,
    #[cfg(unix)]
    inode: u64,
}

impl MediaSourceIdentity {
    fn from_metadata(metadata: &fs::Metadata) -> Self {
        #[cfg(unix)]
        use std::os::unix::fs::MetadataExt;

        Self {
            len: metadata.len(),
            modified: metadata.modified().ok(),
            #[cfg(unix)]
            device: metadata.dev(),
            #[cfg(unix)]
            inode: metadata.ino(),
        }
    }
}

/// File-backed media source view plus its original filesystem identity.
pub struct MediaSource {
    read_bytes: AtomicU64,
    read_operations: AtomicU64,
    clusters_visited: AtomicU64,
    metadata_window: Mutex<MetadataWindow>,
    runtime: std::sync::Arc<crate::EngineRuntime>,
    _lease: crate::resources::SessionLease,
    control: crate::WorkControl,
    matroska_index: std::sync::OnceLock<
        std::result::Result<crate::container::matroska::MatroskaFileIndex, String>,
    >,
    path: PathBuf,
    identity: MediaSourceIdentity,
    len: u64,
    bytes: Vec<u8>,
    compact_mp4: bool,
    file: File,
}

impl std::fmt::Debug for MediaSource {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("MediaSource")
            .field("path", &self.path)
            .field("identity", &self.identity)
            .field("len", &self.len)
            .finish_non_exhaustive()
    }
}

impl MediaSource {
    pub(crate) fn io_stats(&self) -> (u64, u64, u64) {
        (
            self.read_bytes.load(Ordering::Relaxed),
            self.read_operations.load(Ordering::Relaxed),
            self.clusters_visited.load(Ordering::Relaxed),
        )
    }
    pub(crate) fn visit_cluster(&self) {
        self.clusters_visited.fetch_add(1, Ordering::Relaxed);
    }
    fn read_exact_counted(&self, buffer: &mut [u8], offset: u64) -> io::Result<()> {
        self.read_operations.fetch_add(1, Ordering::Relaxed);
        read_exact_at(&self.file, buffer, offset)?;
        self.read_bytes
            .fetch_add(buffer.len() as u64, Ordering::Relaxed);
        Ok(())
    }
    pub(crate) fn policy(&self) -> &crate::ResourcePolicy {
        self.runtime.policy()
    }
    pub(crate) fn check_work(&self) -> Result<()> {
        Ok(self.control.check()?)
    }
    /// Oversized multi-track MP4s keep only headers resident; selected tables
    /// are loaded losslessly on demand under the same metadata budget.
    pub(crate) fn mp4_metadata_for_tracks(&self, tracks: &[&str]) -> Result<Cow<'_, [u8]>> {
        if !self.compact_mp4 {
            return Ok(Cow::Borrowed(self.as_ref()));
        }
        self.validate_current()?;
        let (_, bytes, _) =
            probe_reader::read_metadata_view(&self.path, self.policy(), &self.control, tracks)?;
        self.validate_current()?;
        Ok(Cow::Owned(bytes))
    }

    pub(crate) fn chunk_plan(
        &self,
        track: Option<&str>,
        target_ms: u64,
    ) -> Result<crate::packet::ChunkPlan> {
        if target_ms == 0 {
            anyhow::bail!("chunk target duration must be positive");
        }
        match crate::container::sniff_container(self.as_ref()) {
            crate::container::ContainerKind::Mp4 | crate::container::ContainerKind::Mov => {
                let metadata = self.mp4_metadata_for_tracks(&[track.unwrap_or("v0")])?;
                crate::container::mp4::parse_chunk_plan(&metadata, track, target_ms)
                    .ok_or_else(|| anyhow::anyhow!("missing MP4 chunk plan"))
            }
            crate::container::ContainerKind::Matroska | crate::container::ContainerKind::Webm => {
                self.matroska_index()?
                    .plan(self, track.unwrap_or("v0"), target_ms)
            }
            _ => anyhow::bail!("unsupported source container"),
        }
    }

    pub(crate) fn matroska_chunk_plans(
        &self,
        tracks: &[&str],
        target_ms: u64,
    ) -> Result<Vec<crate::packet::ChunkPlan>> {
        if tracks.is_empty() {
            return Ok(Vec::new());
        }
        if target_ms == 0 {
            anyhow::bail!("chunk target duration must be positive");
        }
        self.matroska_index()?.plans(self, tracks, target_ms)
    }

    pub(crate) fn extract_chunk(
        &self,
        track: Option<&str>,
        target_ms: u64,
        index: u32,
    ) -> Result<(crate::packet::ExtractedChunk, Vec<u8>)> {
        use crate::packet::{ExtractedChunk, PacketRange, packet_samples_for_range};
        self.validate_current()?;
        let mut chunk = self
            .chunk_plan(track, target_ms)?
            .chunks
            .into_iter()
            .find(|chunk| chunk.index == index)
            .ok_or_else(|| anyhow::anyhow!("chunk {index} is out of range"))?;
        let (id, packets) = if crate::container::mp4::looks_like_mp4(self.as_ref()) {
            let metadata = self.mp4_metadata_for_tracks(&[track.unwrap_or("v0")])?;
            let track = crate::container::mp4::parse_packet_track(&metadata, track)
                .ok_or_else(|| anyhow::anyhow!("missing MP4 packet track"))?;
            (track.track_id, track.packets)
        } else {
            let track = track.unwrap_or("v0");
            let track = self
                .matroska_index()?
                .packets(
                    self,
                    &[track],
                    chunk.start.as_millis(),
                    chunk
                        .start
                        .as_millis()
                        .saturating_add(chunk.duration.as_millis()),
                )?
                .remove(0);
            chunk.packet_range = PacketRange {
                start: 0,
                end: u32::try_from(track.packets.len())?,
            };
            (track.id, track.packets)
        };
        let payload = self.packet_payload(&packets, chunk.packet_range)?;
        let samples = packet_samples_for_range(&packets, chunk.packet_range)?;
        Ok((
            ExtractedChunk {
                track_id: id,
                packet_count: u32::try_from(samples.len())?,
                byte_count: payload.len() as u64,
                samples,
                chunk,
            },
            payload,
        ))
    }

    pub(crate) fn matroska_index(&self) -> Result<&crate::container::matroska::MatroskaFileIndex> {
        self.matroska_index
            .get_or_init(|| {
                crate::container::matroska::MatroskaFileIndex::open(self)
                    .map_err(|error| format!("{error:#}"))
            })
            .as_ref()
            .map_err(|error| anyhow::anyhow!(error.clone()))
    }
    pub(crate) fn element_at(&self, offset: u64, end: u64) -> Result<probe_reader::Element> {
        self.check_work()?;
        if offset >= end || end > self.len {
            anyhow::bail!("element outside source");
        }
        let header = self.cached_metadata_prefix(offset, (end - offset).min(12) as usize)?;
        probe_reader::element_from_header(&header, offset, end)
            .with_context(|| format!("Matroska element at byte {offset}, parent end {end}"))
    }

    /// Only EBML integer fields and lacing/block prefixes use metadata lookahead.
    /// Caller identity checks still surround the complete indexing/window scan.
    pub(crate) fn read_metadata_prefix(&self, offset: u64, len: usize) -> io::Result<Vec<u8>> {
        self.check_work().map_err(io::Error::other)?;
        self.policy()
            .check(
                "compressed window",
                len,
                self.policy().compressed_window_bytes,
            )
            .map_err(io::Error::other)?;
        self.cached_metadata_prefix(offset, len)
    }

    fn cached_metadata_prefix(&self, offset: u64, len: usize) -> io::Result<Vec<u8>> {
        self.check_work().map_err(io::Error::other)?;
        let capacity = self
            .policy()
            .metadata_bytes
            .saturating_sub(self.bytes.len())
            .min(self.policy().compressed_window_bytes)
            .min(metadata_lookahead_limit());
        let mut window = self
            .metadata_window
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        self.check_work().map_err(io::Error::other)?;
        let prefix = window.read(offset, len, self.len, capacity, |bytes, offset| {
            self.read_exact_counted(bytes, offset)
        })?;
        drop(window);
        self.check_work().map_err(io::Error::other)?;
        Ok(prefix)
    }

    /// Keep an indexed subtitle block's headers and payload in one bounded
    /// window, so validation and decoding do not reread it across the network.
    pub(crate) fn prefetch_metadata_span(&self, offset: u64, len: usize) -> io::Result<()> {
        self.check_work().map_err(io::Error::other)?;
        if offset
            .checked_add(len as u64)
            .is_none_or(|end| end > self.len)
        {
            return Err(io::Error::new(
                io::ErrorKind::UnexpectedEof,
                "metadata span outside source",
            ));
        }
        let capacity = self
            .policy()
            .metadata_bytes
            .saturating_sub(self.bytes.len())
            .min(self.policy().compressed_window_bytes)
            .min(metadata_lookahead_limit());
        if len == 0 || len > capacity {
            return Ok(());
        }
        let mut window = self
            .metadata_window
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        if offset
            .checked_sub(window.offset)
            .and_then(|at| usize::try_from(at).ok())
            .and_then(|at| at.checked_add(len))
            .is_some_and(|end| end <= window.bytes.len())
        {
            return Ok(());
        }
        if window.bytes.capacity() > capacity {
            window.bytes = Vec::new();
        }
        window.bytes.clear();
        window
            .bytes
            .try_reserve_exact(len)
            .map_err(io::Error::other)?;
        window.bytes.resize(len, 0);
        if let Err(error) = self.read_exact_counted(&mut window.bytes, offset) {
            window.bytes.clear();
            return Err(error);
        }
        window.offset = offset;
        self.check_work().map_err(io::Error::other)?;
        Ok(())
    }

    pub(crate) fn read_window_unvalidated(&self, offset: u64, len: usize) -> io::Result<Vec<u8>> {
        self.check_work().map_err(io::Error::other)?;
        self.policy()
            .check(
                "compressed window",
                len,
                self.policy().compressed_window_bytes,
            )
            .map_err(io::Error::other)?;
        if offset
            .checked_add(len as u64)
            .is_none_or(|end| end > self.len)
        {
            return Err(io::Error::new(
                io::ErrorKind::UnexpectedEof,
                "window outside source",
            ));
        }
        // Scanners validate the source around the complete pass. Reuse bytes
        // already fetched for neighbouring headers instead of issuing a second
        // network read for a lace table or subtitle payload inside that window.
        let cached = {
            let window = self
                .metadata_window
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner());
            offset
                .checked_sub(window.offset)
                .and_then(|relative| usize::try_from(relative).ok())
                .and_then(|start| {
                    start
                        .checked_add(len)
                        .and_then(|end| window.bytes.get(start..end))
                })
                .map(<[u8]>::to_vec)
        };
        if let Some(bytes) = cached {
            self.check_work().map_err(io::Error::other)?;
            return Ok(bytes);
        }
        let mut output = bounded_buffer(len)?;
        self.read_exact_counted(&mut output, offset)?;
        Ok(output)
    }

    /// Modern packet sessions retain owned container metadata and read payload
    /// offsets positionally. Matroska additionally indexes original cluster offsets.
    pub(crate) fn open_packet_copy(path: &Path) -> Result<Self> {
        Self::open_with_context(
            path,
            crate::EngineRuntime::global()?,
            crate::WorkControl::default(),
        )
    }

    pub(crate) fn open_with_context(
        path: &Path,
        runtime: std::sync::Arc<crate::EngineRuntime>,
        control: crate::WorkControl,
    ) -> Result<Self> {
        control.check()?;
        let lease = runtime.admit()?;
        let file = File::open(path)?;
        let identity = MediaSourceIdentity::from_metadata(&file.metadata()?);
        let (_, metadata, compact_mp4) =
            probe_reader::read_metadata_view(path, runtime.policy(), &control, &[])?;
        let source = Self {
            read_bytes: AtomicU64::new(0),
            read_operations: AtomicU64::new(0),
            clusters_visited: AtomicU64::new(0),
            metadata_window: Mutex::new(MetadataWindow::default()),
            runtime,
            _lease: lease,
            control,
            matroska_index: Default::default(),
            path: path.to_path_buf(),
            len: identity.len,
            identity,
            bytes: metadata,
            compact_mp4,
            file,
        };
        source.validate_current()?;
        Ok(source)
    }

    /// Returns the source length captured at open time.
    pub fn len(&self) -> u64 {
        self.len
    }

    /// Returns the bytes in the retained source view.
    pub fn as_bytes(&self) -> &[u8] {
        self.bytes.as_ref()
    }

    /// Reads a bounded owned positional window.
    pub fn read_at(&self, offset: u64, len: usize) -> io::Result<Vec<u8>> {
        self.validate_current().map_err(io::Error::other)?;
        if offset
            .checked_add(len as u64)
            .is_none_or(|end| end > self.len)
        {
            return Err(io::Error::new(
                io::ErrorKind::UnexpectedEof,
                "window outside source",
            ));
        }
        self.policy()
            .check(
                "compressed window",
                len,
                self.policy().compressed_window_bytes,
            )
            .map_err(io::Error::other)?;
        let mut output = bounded_buffer(len)?;
        self.read_exact_counted(&mut output, offset)?;
        self.validate_current().map_err(io::Error::other)?;
        Ok(output)
    }

    /// Reads selected packet spans with identity validation around the complete
    /// operation. Positional reads never depend on a shared file cursor.
    pub(crate) fn read_packets(&self, packets: &[crate::packet::PacketRef]) -> io::Result<Vec<u8>> {
        self.read_packets_mode(packets, false)
    }

    /// Background audio analysis traverses long interleaved windows. A bounded
    /// sequential span avoids a network round trip for each tiny audio chunk.
    /// Playback keeps the stricter low-amplification packet-copy policy.
    pub(crate) fn read_analysis_packets(
        &self,
        packets: &[crate::packet::PacketRef],
    ) -> io::Result<Vec<u8>> {
        self.read_packets_mode(packets, true)
    }

    fn read_packets_mode(
        &self,
        packets: &[crate::packet::PacketRef],
        analysis: bool,
    ) -> io::Result<Vec<u8>> {
        if let [packet] = packets {
            return self.read_at(packet.source_offset, packet.size as usize);
        }
        self.validate_current().map_err(io::Error::other)?;
        let size = packets.iter().try_fold(0_usize, |total, packet| {
            let end = packet
                .source_offset
                .checked_add(u64::from(packet.size))
                .ok_or_else(|| {
                    io::Error::new(io::ErrorKind::InvalidInput, "packet offset overflow")
                })?;
            if end > self.len {
                return Err(io::Error::new(
                    io::ErrorKind::UnexpectedEof,
                    "packet outside source",
                ));
            }
            total
                .checked_add(packet.size as usize)
                .ok_or_else(|| io::Error::new(io::ErrorKind::InvalidInput, "packet size overflow"))
        })?;
        self.policy()
            .check(
                "compressed window",
                size,
                self.policy().compressed_window_bytes,
            )
            .map_err(io::Error::other)?;
        let mut output = bounded_buffer(size)?;
        let mut cursor = 0;
        let mut index = 0;
        while index < packets.len() {
            self.check_work().map_err(io::Error::other)?;
            let first_index = index;
            let offset = packets[index].source_offset;
            let mut span = u64::from(packets[index].size);
            let mut payload = packets[index].size as usize;
            index += 1;
            while let Some(next) = packets.get(index) {
                let Some(gap) = next.source_offset.checked_sub(offset + span) else {
                    break;
                };
                let next_span = next.source_offset + u64::from(next.size) - offset;
                let next_payload = payload + next.size as usize;
                // Adjacent payloads read directly into the output. Analysis may
                // cross interleaved video within bounded I/O and memory limits;
                // out-of-order packets always start a separate positional read.
                let contiguous = gap == 0 && span == payload as u64;
                let (gap_limit, span_limit, amplification) = if analysis {
                    (256 * 1024, 1024 * 1024, 128)
                } else {
                    (PACKET_GAP_BYTES, PACKET_COALESCE_BYTES as u64, 2)
                };
                let bounded_gap = gap <= gap_limit
                    && next_span <= span_limit
                    && next_span <= (next_payload as u64).saturating_mul(amplification)
                    && next_span
                        <= self.policy().compressed_window_bytes.saturating_sub(size) as u64;
                if !contiguous && !bounded_gap {
                    break;
                }
                span = next_span;
                payload = next_payload;
                index += 1;
            }
            if span == payload as u64 {
                self.read_exact_counted(&mut output[cursor..cursor + payload], offset)?;
                cursor += payload;
            } else {
                let mut window = bounded_buffer(span as usize)?;
                self.read_exact_counted(&mut window, offset)?;
                for packet in &packets[first_index..index] {
                    let start = (packet.source_offset - offset) as usize;
                    let len = packet.size as usize;
                    output[cursor..cursor + len].copy_from_slice(&window[start..start + len]);
                    cursor += len;
                }
            }
        }
        self.validate_current().map_err(io::Error::other)?;
        Ok(output)
    }

    /// Verifies that the path still points at the same source identity.
    pub fn validate_current(&self) -> Result<()> {
        self.check_work()?;
        let current =
            fs::metadata(&self.path).with_context(|| format!("stat {}", self.path.display()))?;
        let current = MediaSourceIdentity::from_metadata(&current);
        if current != self.identity
            || MediaSourceIdentity::from_metadata(&self.file.metadata()?) != self.identity
        {
            anyhow::bail!("source changed: {}", self.path.display());
        }
        Ok(())
    }
}

fn bounded_buffer(len: usize) -> io::Result<Vec<u8>> {
    if len > 64 * 1024 * 1024 {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "source window exceeds 64 MiB budget",
        ));
    }
    let mut buffer = Vec::new();
    buffer.try_reserve_exact(len).map_err(io::Error::other)?;
    buffer.resize(len, 0);
    Ok(buffer)
}

fn read_exact_at(file: &File, mut buffer: &mut [u8], mut offset: u64) -> io::Result<()> {
    while !buffer.is_empty() {
        #[cfg(unix)]
        let result = {
            use std::os::unix::fs::FileExt;
            file.read_at(buffer, offset)
        };
        #[cfg(windows)]
        let result = {
            use std::os::windows::fs::FileExt;
            file.seek_read(buffer, offset)
        };
        #[cfg(not(any(unix, windows)))]
        let result: io::Result<usize> = Err(io::Error::new(
            io::ErrorKind::Unsupported,
            "positional reads unsupported",
        ));
        match result {
            Ok(0) => {
                return Err(io::Error::new(
                    io::ErrorKind::UnexpectedEof,
                    "source truncated during read",
                ));
            }
            Ok(read) => {
                offset = offset.checked_add(read as u64).ok_or_else(|| {
                    io::Error::new(io::ErrorKind::InvalidInput, "read offset overflow")
                })?;
                buffer = &mut buffer[read..];
            }
            Err(error) if error.kind() == io::ErrorKind::Interrupted => continue,
            Err(error) => return Err(error),
        }
    }
    Ok(())
}

impl AsRef<[u8]> for MediaSource {
    fn as_ref(&self) -> &[u8] {
        self.as_bytes()
    }
}

/// Compatibility wrapper for callers that still use the old mapped-source name.
pub type MappedMediaFile = MediaSource;

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn metadata_lookahead_coalesces_headers_and_reloads_only_outside_its_window() {
        let source = (0..20_000)
            .map(|index| (index % 251) as u8)
            .collect::<Vec<_>>();
        let mut cache = MetadataWindow::default();
        let mut reads = Vec::new();
        for (offset, len) in [(0, 12), (12, 2), (30, 16), (9_000, 12), (19_998, 2)] {
            let bytes = cache
                .read(
                    offset,
                    len,
                    source.len() as u64,
                    8 * 1024,
                    |output, offset| {
                        reads.push((offset, output.len()));
                        let start = offset as usize;
                        output.copy_from_slice(&source[start..start + output.len()]);
                        Ok(())
                    },
                )
                .unwrap();
            assert_eq!(bytes, source[offset as usize..offset as usize + len]);
        }
        assert_eq!(reads, [(0, 8_192), (9_000, 8_192), (19_998, 2)]);
        assert!(cache.bytes.capacity() <= 8 * 1024);
        assert!(
            cache
                .read(20_000, 1, 20_000, 8_192, |_, _| panic!(
                    "out-of-bounds read"
                ))
                .is_err()
        );
    }

    #[test]
    fn failed_metadata_fill_cannot_expose_partial_or_stale_bytes() {
        let mut cache = MetadataWindow::default();
        assert!(
            cache
                .read(0, 4, 16, 16, |output, _| {
                    output.fill(0xaa);
                    Err(io::Error::new(
                        io::ErrorKind::UnexpectedEof,
                        "truncated source",
                    ))
                })
                .is_err()
        );
        assert!(cache.bytes.is_empty());
        assert_eq!(
            cache
                .read(0, 4, 16, 16, |output, _| {
                    output.fill(0xbb);
                    Ok(())
                })
                .unwrap(),
            [0xbb; 4]
        );
    }

    fn metadata_fixture() -> (tempfile::TempDir, PathBuf) {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("headers.bin");
        let mut bytes = vec![0x81, 0x81, 2, 0x82, 0x81, 3];
        bytes.resize(128, 0);
        fs::write(&path, bytes).unwrap();
        (directory, path)
    }

    #[test]
    fn cached_headers_keep_parent_bounds_and_do_not_replace_payload_reads() {
        let (_directory, path) = metadata_fixture();
        let source = MediaSource::open_packet_copy(&path).unwrap();
        assert_eq!(source.element_at(0, source.len()).unwrap().end, 3);
        assert_eq!(source.read_metadata_prefix(2, 1).unwrap(), [2]);
        let initial = source.io_stats();
        assert_eq!((initial.0, initial.1), (128, 1));
        assert!(source.element_at(0, 1).is_err());
        assert!(source.element_at(0, 2).is_err());
        assert!(source.element_at(source.len(), source.len()).is_err());
        assert_eq!(source.io_stats().1, initial.1);
        // Public packet reads remain exact, even when their bytes are cached.
        assert_eq!(source.read_at(2, 1).unwrap(), [2]);
        assert_eq!(source.io_stats().1, initial.1 + 1);
        assert_eq!(source.io_stats().0, initial.0 + 1);
        fs::write(&path, b"replaced source").unwrap();
        assert!(source.validate_current().is_err());
        assert!(source.read_at(2, 1).is_err());
    }

    #[test]
    fn metadata_cache_respects_exact_and_tiny_remaining_budgets() {
        let (_directory, path) = metadata_fixture();
        for (metadata_bytes, expected_cache) in [(16, 0), (17, 1), (48, 32)] {
            let runtime = crate::EngineRuntime::new(crate::ResourcePolicy {
                metadata_bytes,
                compressed_window_bytes: 32,
                ..crate::ResourcePolicy::default()
            })
            .unwrap();
            let source =
                MediaSource::open_with_context(&path, runtime, crate::WorkControl::default())
                    .unwrap();
            assert_eq!(source.as_bytes().len(), 16);
            assert_eq!(source.element_at(0, source.len()).unwrap().end, 3);
            assert_eq!(source.read_metadata_prefix(2, 1).unwrap(), [2]);
            let window = source.metadata_window.lock().unwrap();
            assert_eq!(window.bytes.capacity(), expected_cache);
            assert!(source.bytes.len() + window.bytes.capacity() <= metadata_bytes);
        }
        let runtime = crate::EngineRuntime::new(crate::ResourcePolicy {
            compressed_window_bytes: 2,
            ..crate::ResourcePolicy::default()
        })
        .unwrap();
        let source =
            MediaSource::open_with_context(&path, runtime, crate::WorkControl::default()).unwrap();
        assert_eq!(source.element_at(0, source.len()).unwrap().end, 3);
        assert!(source.read_metadata_prefix(2, 3).is_err());
        assert!(source.metadata_window.lock().unwrap().bytes.is_empty());
    }

    #[test]
    fn cancellation_is_checked_before_a_metadata_cache_hit() {
        let (_directory, path) = metadata_fixture();
        let control = crate::WorkControl::default();
        let runtime = crate::EngineRuntime::new(crate::ResourcePolicy::default()).unwrap();
        let source = MediaSource::open_with_context(&path, runtime, control.clone()).unwrap();
        source.element_at(0, source.len()).unwrap();
        let reads = source.io_stats();
        control.cancel();
        assert!(source.element_at(0, source.len()).is_err());
        assert!(source.read_metadata_prefix(2, 1).is_err());
        assert_eq!(source.io_stats(), reads);
    }

    #[test]
    fn detects_same_path_replacement_and_preserves_owned_metadata() {
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join("sample.mp4");
        fs::write(&path, b"first").expect("write source");
        let source = MediaSource::open_packet_copy(&path).expect("open source");

        fs::write(&path, b"second").expect("replace source");

        assert!(source.validate_current().is_err());
        assert_eq!(source.as_bytes(), b"first");
    }

    #[test]
    fn positional_reads_reject_changed_sources_and_oversized_windows() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("source.bin");
        fs::write(&path, b"original").unwrap();
        let source = MediaSource::open_packet_copy(&path).unwrap();
        assert!(source.read_at(0, 65 * 1024 * 1024).is_err());
        fs::write(&path, b"x").unwrap();
        assert!(source.read_at(0, 8).is_err());
    }

    #[test]
    fn larger_metadata_window_reduces_sequential_header_reads_without_changing_bytes() {
        let data = vec![0x5a; 1024 * 1024];
        let scan = |capacity| {
            let mut window = MetadataWindow::default();
            let mut reads = 0;
            for offset in (0..data.len() - 12).step_by(300) {
                let got = window
                    .read(offset as u64, 12, data.len() as u64, capacity, |out, at| {
                        reads += 1;
                        out.copy_from_slice(&data[at as usize..at as usize + out.len()]);
                        Ok(())
                    })
                    .unwrap();
                assert_eq!(got, &data[offset..offset + 12]);
            }
            assert!(window.bytes.capacity() <= capacity);
            reads
        };
        assert!(scan(METADATA_LOOKAHEAD_BYTES) * 6 < scan(8 * 1024));
    }

    fn packet(offset: u64, size: u32) -> crate::packet::PacketRef {
        crate::packet::PacketRef {
            source_offset: offset,
            size,
            pts: crate::packet::TimePoint::millis(0),
            dts: crate::packet::TimePoint::millis(0),
            duration: crate::packet::TimeDelta::millis(1),
            keyframe: true,
        }
    }

    #[test]
    fn indexed_block_prefetch_serves_headers_and_payload_without_rereads() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("block.bin");
        let bytes = (0..100_000).map(|i| (i % 251) as u8).collect::<Vec<_>>();
        fs::write(&path, &bytes).unwrap();
        let source = MediaSource::open_packet_copy(&path).unwrap();
        source.read_metadata_prefix(0, 12).unwrap();
        source.prefetch_metadata_span(0, 32_000).unwrap();
        let before = source.io_stats();
        assert_eq!(
            source.read_metadata_prefix(31_000, 12).unwrap(),
            bytes[31_000..31_012]
        );
        assert_eq!(
            source.read_window_unvalidated(12, 30_000).unwrap(),
            bytes[12..30_012]
        );
        source.prefetch_metadata_span(100, 20_000).unwrap();
        assert_eq!(source.io_stats(), before);
        assert!(source.prefetch_metadata_span(90_000, 20_000).is_err());
        source.validate_current().unwrap();
    }

    #[test]
    fn packet_coalescing_skips_mux_headers_and_preserves_decode_order() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("fixture.bin");
        let data = (0..200_000).map(|i| (i % 251) as u8).collect::<Vec<_>>();
        fs::write(&path, &data).unwrap();
        let source = MediaSource::open_packet_copy(&path).unwrap();
        let packets = (0..100).map(|i| packet(i * 108, 100)).collect::<Vec<_>>();
        let before = source.io_stats();
        let got = source.read_packets(&packets).unwrap();
        let expected = packets
            .iter()
            .flat_map(|p| {
                data[p.source_offset as usize..p.source_offset as usize + p.size as usize].to_vec()
            })
            .collect::<Vec<_>>();
        assert_eq!(got, expected);
        assert_eq!(source.io_stats().1 - before.1, 1);
        assert!(source.io_stats().0 - before.0 <= 2 * got.len() as u64);
        let unordered = [
            packet(10000, 100),
            packet(100, 100),
            packet(200, 100),
            packet(100, 50),
        ];
        assert_eq!(
            source.read_packets(&unordered).unwrap(),
            unordered
                .iter()
                .flat_map(|p| data
                    [p.source_offset as usize..p.source_offset as usize + p.size as usize]
                    .to_vec())
                .collect::<Vec<_>>()
        );
    }

    #[test]
    fn packet_gap_reads_obey_amplification_gap_and_memory_limits() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("fixture.bin");
        fs::write(&path, vec![0x5a; 200_000]).unwrap();
        let source = MediaSource::open_packet_copy(&path).unwrap();
        for packets in [
            [packet(0, 1), packet(100, 1)],
            [packet(0, 100), packet(10000, 100)],
        ] {
            let before = source.io_stats();
            assert!(source.read_packets(&packets).is_ok());
            assert_eq!(source.io_stats().1 - before.1, 2);
            assert_eq!(
                source.io_stats().0 - before.0,
                packets.iter().map(|p| u64::from(p.size)).sum::<u64>()
            );
        }
        let runtime = crate::EngineRuntime::new(crate::ResourcePolicy {
            compressed_window_bytes: 200,
            ..crate::ResourcePolicy::default()
        })
        .unwrap();
        let bounded =
            MediaSource::open_with_context(&path, runtime, crate::WorkControl::default()).unwrap();
        let before = bounded.io_stats();
        assert_eq!(
            bounded
                .read_packets(&[packet(0, 100), packet(108, 100)])
                .unwrap()
                .len(),
            200
        );
        assert_eq!(bounded.io_stats().1 - before.1, 2);
    }

    #[test]
    fn analysis_packet_reads_coalesce_interleaving_without_changing_selected_bytes() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("interleaved.bin");
        let bytes = (0..4_000_000).map(|i| (i % 251) as u8).collect::<Vec<_>>();
        fs::write(&path, &bytes).unwrap();
        let source = MediaSource::open_packet_copy(&path).unwrap();
        let packets = (0..128)
            .map(|i| packet(i * 20_000, 256))
            .collect::<Vec<_>>();
        let expected = source.read_packets(&packets).unwrap();
        let before = source.io_stats();
        assert_eq!(source.read_analysis_packets(&packets).unwrap(), expected);
        let after = source.io_stats();
        assert_eq!(after.1 - before.1, 3);
        assert!(after.0 - before.0 <= expected.len() as u64 * 128);
        // Physical ordering, maximum gap, and source identity stay enforced.
        let packets = [packet(500_000, 256), packet(0, 256), packet(1_000_000, 256)];
        let before = source.io_stats();
        assert_eq!(
            source.read_analysis_packets(&packets).unwrap(),
            source.read_packets(&packets).unwrap()
        );
        assert_eq!(source.io_stats().1 - before.1, 6);
        let runtime = crate::EngineRuntime::new(crate::ResourcePolicy {
            compressed_window_bytes: 512,
            ..crate::ResourcePolicy::default()
        })
        .unwrap();
        let bounded =
            MediaSource::open_with_context(&path, runtime, crate::WorkControl::default()).unwrap();
        let before = bounded.io_stats();
        assert_eq!(
            bounded
                .read_analysis_packets(&[packet(0, 256), packet(1000, 256)])
                .unwrap()
                .len(),
            512
        );
        assert_eq!(bounded.io_stats().1 - before.1, 2);
        fs::write(&path, b"changed").unwrap();
        assert!(source.read_analysis_packets(&packets).is_err());
    }

    #[test]
    fn scan_windows_reuse_buffered_bytes_but_keep_bounds_and_public_identity_checks() {
        let (_dir, path) = metadata_fixture();
        let source = MediaSource::open_packet_copy(&path).unwrap();
        source.element_at(0, source.len()).unwrap();
        let before = source.io_stats();
        let got = source.read_window_unvalidated(2, 20).unwrap();
        assert_eq!(got, fs::read(&path).unwrap()[2..22]);
        assert_eq!(source.io_stats(), before);
        assert!(source.read_window_unvalidated(source.len(), 1).is_err());
        fs::write(&path, b"replaced source").unwrap();
        assert!(source.validate_current().is_err());
        assert!(source.read_at(0, 1).is_err());
    }

    #[test]
    fn read_at_bounds_positional_windows() {
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join("sample.mp4");
        fs::write(&path, b"abcdef").expect("write source");
        let source = MediaSource::open_packet_copy(&path).expect("open source");

        assert_eq!(source.read_at(2, 3).expect("read"), b"cde");
        assert_eq!(
            source.read_at(5, 2).expect_err("out of range").kind(),
            io::ErrorKind::UnexpectedEof
        );
    }
}
