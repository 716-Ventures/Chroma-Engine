use std::{
    fs::{self, File},
    io,
    path::{Path, PathBuf},
    time::SystemTime,
};

use anyhow::{Context, Result};
use std::sync::atomic::{AtomicU64, Ordering};
pub(crate) mod probe_reader;

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
                crate::container::mp4::parse_chunk_plan(self.as_ref(), track, target_ms)
                    .ok_or_else(|| anyhow::anyhow!("missing MP4 chunk plan"))
            }
            crate::container::ContainerKind::Matroska | crate::container::ContainerKind::Webm => {
                self.matroska_index()?
                    .plan(self, track.unwrap_or("v0"), target_ms)
            }
            _ => anyhow::bail!("unsupported source container"),
        }
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
            let track = crate::container::mp4::parse_packet_track(self.as_ref(), track)
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
                    .map_err(|error| error.to_string())
            })
            .as_ref()
            .map_err(|error| anyhow::anyhow!(error.clone()))
    }
    pub(crate) fn element_at(&self, offset: u64, end: u64) -> Result<probe_reader::Element> {
        self.check_work()?;
        if end > self.len {
            anyhow::bail!("element outside source");
        }
        self.read_operations.fetch_add(1, Ordering::Relaxed);
        let element = probe_reader::element(&self.file, offset, end)?;
        self.read_bytes
            .fetch_add(end.saturating_sub(offset).min(12), Ordering::Relaxed);
        Ok(element)
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
        let (_, metadata) =
            probe_reader::read_metadata_with_context(path, runtime.policy(), &control)?;
        let source = Self {
            read_bytes: AtomicU64::new(0),
            read_operations: AtomicU64::new(0),
            clusters_visited: AtomicU64::new(0),
            runtime,
            _lease: lease,
            control,
            matroska_index: Default::default(),
            path: path.to_path_buf(),
            len: identity.len,
            identity,
            bytes: metadata,
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
            let first = &packets[index];
            let offset = first.source_offset;
            let mut length = first.size as usize;
            index += 1;
            // Coalesce only physically adjacent spans. Preserve decode order and
            // never read unselected interleaved payload or allocate a gap buffer.
            while let Some(next) = packets.get(index) {
                if offset.checked_add(length as u64) != Some(next.source_offset) {
                    break;
                }
                length += next.size as usize;
                index += 1;
            }
            let end = cursor + length;
            self.read_exact_counted(&mut output[cursor..end], offset)?;
            cursor = end;
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
