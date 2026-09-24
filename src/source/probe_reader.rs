//! Bounded positional metadata reads. Media payload is never mapped or copied.
use std::{fs::File, io, path::Path, time::Instant};

use anyhow::{Context, Result, bail};

use super::{MediaSourceIdentity, read_exact_at};

const MAX_METADATA: usize = 64 * 1024 * 1024;
const MAX_ELEMENTS: usize = 100_000;

#[derive(Clone, Copy, Default)]
struct ReadCounters {
    operations: u64,
    bytes: u64,
    elements: u64,
}

thread_local! {
    static READ_COUNTERS: std::cell::Cell<ReadCounters> = const { std::cell::Cell::new(ReadCounters { operations: 0, bytes: 0, elements: 0 }) };
}

fn count_read(bytes: usize) {
    READ_COUNTERS.with(|cell| {
        let mut value = cell.get();
        value.operations += 1;
        value.bytes += bytes as u64;
        cell.set(value);
    });
}

pub(crate) fn read_metadata(path: &Path) -> Result<(u64, Vec<u8>)> {
    let runtime = crate::EngineRuntime::global()?;
    let _lease = runtime.admit()?;
    read_metadata_with_context(path, runtime.policy(), &crate::WorkControl::default())
}

pub(crate) fn read_metadata_with_context(
    path: &Path,
    policy: &crate::ResourcePolicy,
    control: &crate::WorkControl,
) -> Result<(u64, Vec<u8>)> {
    let timed = std::env::var_os("CHROMA_PROBE_DIAGNOSTICS").is_some();
    let started = Instant::now();
    if timed {
        READ_COUNTERS.with(|cell| cell.set(ReadCounters::default()));
    }
    control.check()?;
    let file = File::open(path).with_context(|| format!("open {}", path.display()))?;
    let identity = MediaSourceIdentity::from_metadata(&file.metadata()?);
    let len = identity.len;
    let head = read(&file, 0, len.min(16) as usize)?;
    let bytes = match crate::container::sniff_container(&head) {
        crate::container::ContainerKind::Mp4 | crate::container::ContainerKind::Mov => {
            mp4_metadata(&file, len, policy, control)?
        }
        crate::container::ContainerKind::Matroska | crate::container::ContainerKind::Webm => {
            mkv_metadata(&file, len, policy, control)?
        }
        crate::container::ContainerKind::Unknown => head,
    };
    if MediaSourceIdentity::from_metadata(&file.metadata()?) != identity
        || MediaSourceIdentity::from_metadata(&std::fs::metadata(path)?) != identity
    {
        bail!("source changed during metadata read");
    }
    control.check()?;
    policy.check("container metadata", bytes.len(), policy.metadata_bytes)?;
    crate::container::validate_metadata_budget(&bytes, policy.parse_limits())?;
    if timed {
        let counters = READ_COUNTERS.with(std::cell::Cell::get);
        eprintln!(
            "probe_metadata_read elapsed_ms={} file_bytes={} metadata_bytes={} read_operations={} read_bytes={} elements_visited={}",
            started.elapsed().as_millis(),
            len,
            bytes.len(),
            counters.operations,
            counters.bytes,
            counters.elements
        );
    }
    Ok((len, bytes))
}

#[cfg(test)]
thread_local! {
    static METADATA_READS: std::cell::Cell<usize> = const { std::cell::Cell::new(0) };
}

fn read(file: &File, offset: u64, size: usize) -> io::Result<Vec<u8>> {
    count_read(size);
    #[cfg(test)]
    METADATA_READS.with(|count| count.set(count.get() + 1));
    let mut bytes = super::bounded_buffer(size)?;
    read_exact_at(file, &mut bytes, offset)?;
    Ok(bytes)
}

fn append(
    file: &File,
    offset: u64,
    size: u64,
    bytes: &mut Vec<u8>,
    policy: &crate::ResourcePolicy,
) -> Result<()> {
    let size = usize::try_from(size)?;
    policy.check(
        "container metadata",
        bytes.len().saturating_add(size),
        policy.metadata_bytes,
    )?;
    if size > MAX_METADATA.saturating_sub(bytes.len()) {
        bail!("metadata memory budget exceeded");
    }
    bytes.try_reserve_exact(size)?;
    let start = bytes.len();
    bytes.resize(start + size, 0);
    count_read(size);
    read_exact_at(file, &mut bytes[start..], offset)?;
    Ok(())
}

fn mp4_metadata(
    file: &File,
    len: u64,
    policy: &crate::ResourcePolicy,
    control: &crate::WorkControl,
) -> Result<Vec<u8>> {
    let mut output = Vec::new();
    let mut offset = 0;
    for _ in 0..MAX_ELEMENTS {
        control.check()?;
        if offset == len {
            return Ok(output);
        }
        let header = read(file, offset, (len - offset).min(16) as usize)?;
        if header.len() < 8 {
            bail!("truncated MP4 box header");
        }
        let size32 = u32::from_be_bytes(header[..4].try_into()?);
        let (header_len, size) = match size32 {
            0 => (8, len - offset),
            1 if header.len() >= 16 => (16, u64::from_be_bytes(header[8..16].try_into()?)),
            1 => bail!("truncated extended MP4 box header"),
            value => (8, u64::from(value)),
        };
        if size < header_len || size > len - offset {
            bail!("invalid MP4 box size");
        }
        if matches!(&header[4..8], b"ftyp" | b"moov") {
            append(file, offset, size, &mut output, policy)?;
        }
        offset += size;
    }
    bail!("MP4 element budget exceeded")
}

#[derive(Clone, Copy)]
pub(crate) struct Element {
    pub(crate) unknown_size: bool,
    pub(crate) id: u32,
    pub(crate) payload: u64,
    pub(crate) end: u64,
}

pub(crate) fn element(file: &File, offset: u64, end: u64) -> Result<Element> {
    READ_COUNTERS.with(|cell| {
        let mut value = cell.get();
        value.elements += 1;
        cell.set(value);
    });
    if offset >= end {
        bail!("element offset outside parent");
    }
    let header = read(file, offset, (end - offset).min(12) as usize)?;
    let first = *header
        .first()
        .ok_or_else(|| anyhow::anyhow!("missing EBML element"))?;
    let id_len = first.leading_zeros() as usize + 1;
    if id_len > 4 || header.len() <= id_len {
        bail!("invalid EBML ID");
    }
    let id = header[..id_len]
        .iter()
        .fold(0_u32, |value, byte| (value << 8) | u32::from(*byte));
    let size_len = header[id_len].leading_zeros() as usize + 1;
    if size_len > 8 || header.len() < id_len + size_len {
        bail!("invalid EBML size");
    }
    let mut size = u64::from(header[id_len] & (0xff_u16 >> size_len) as u8);
    for byte in &header[id_len + 1..id_len + size_len] {
        size = (size << 8) | u64::from(*byte);
    }
    let payload = offset + (id_len + size_len) as u64;
    let unknown = size == (1_u64 << (size_len * 7)) - 1;
    let element_end = if unknown {
        end
    } else {
        payload
            .checked_add(size)
            .ok_or_else(|| anyhow::anyhow!("EBML size overflow"))?
    };
    if element_end > end {
        bail!("EBML element outside parent");
    }
    Ok(Element {
        unknown_size: unknown,
        id,
        payload,
        end: element_end,
    })
}

fn wrap(id: u32, payload: &[u8]) -> Vec<u8> {
    let mut bytes = Vec::with_capacity(12 + payload.len());
    let id = id.to_be_bytes();
    bytes.extend_from_slice(&id[id.iter().position(|byte| *byte != 0).unwrap_or(3)..]);
    bytes.extend_from_slice(&((1_u64 << 56) | payload.len() as u64).to_be_bytes());
    bytes.extend_from_slice(payload);
    bytes
}

fn mkv_metadata(
    file: &File,
    len: u64,
    policy: &crate::ResourcePolicy,
    control: &crate::WorkControl,
) -> Result<Vec<u8>> {
    let mut offset = 0;
    let mut segment = None;
    for _ in 0..MAX_ELEMENTS {
        control.check()?;
        if offset >= len {
            break;
        }
        let current = element(file, offset, len)?;
        if current.id == 0x1853_8067 {
            segment = Some(current);
            break;
        }
        offset = current.end;
    }
    let segment = segment.ok_or_else(|| anyhow::anyhow!("missing Matroska Segment"))?;
    if let Some(metadata) = indexed_mkv_metadata(file, segment, policy, control)? {
        return Ok(metadata);
    }
    let mut metadata = Vec::new();
    offset = segment.payload;
    let mut visits = 0;
    while offset < segment.end {
        control.check()?;
        visits += 1;
        if visits > MAX_ELEMENTS {
            bail!("Matroska element budget exceeded");
        }
        let current = element(file, offset, segment.end)?;
        if current.unknown_size {
            bail!("unknown-size Matroska children require a finalized source");
        }
        append_mkv_metadata_element(file, offset, current, &mut metadata, policy, control)?;
        offset = current.end;
    }
    let mut output = wrap(0x1a45_dfa3, &[]);
    output.extend(wrap(0x1853_8067, &metadata));
    Ok(output)
}

// RFC 9559, section 6.3: a SeekHead indexes every top-level element other than
// itself. Only use it when it is first and its entries validate against the file;
// older or malformed layouts retain the bounded sequential walk above.
fn indexed_mkv_metadata(
    file: &File,
    segment: Element,
    policy: &crate::ResourcePolicy,
    control: &crate::WorkControl,
) -> Result<Option<Vec<u8>>> {
    match indexed_mkv_metadata_inner(file, segment, policy, control) {
        Ok(value) => Ok(value),
        Err(_) => Ok(None),
    }
}

fn indexed_mkv_metadata_inner(
    file: &File,
    segment: Element,
    policy: &crate::ResourcePolicy,
    control: &crate::WorkControl,
) -> Result<Option<Vec<u8>>> {
    let first = element(file, segment.payload, segment.end)?;
    if first.id != 0x114d_9b74 || first.unknown_size {
        return Ok(None);
    }
    let mut heads = vec![first];
    let mut references = Vec::new();
    let mut head_offsets = std::collections::HashSet::from([segment.payload]);
    for head_index in 0..2 {
        let Some(head) = heads.get(head_index).copied() else {
            break;
        };
        let mut offset = head.payload;
        let mut visits = 0;
        while offset < head.end {
            control.check()?;
            visits += 1;
            if visits > 1024 || references.len() >= 1024 {
                return Ok(None);
            }
            let seek = element(file, offset, head.end)?;
            if seek.unknown_size {
                return Ok(None);
            }
            if seek.id == 0x4d_bb {
                let mut id = None;
                let mut position = None;
                let mut child_offset = seek.payload;
                while child_offset < seek.end {
                    let child = element(file, child_offset, seek.end)?;
                    if child.unknown_size || child.end - child.payload > 8 {
                        return Ok(None);
                    }
                    if child.id == 0x53ab || child.id == 0x53ac {
                        let bytes =
                            read(file, child.payload, (child.end - child.payload) as usize)?;
                        if bytes.is_empty() {
                            return Ok(None);
                        }
                        let value = bytes
                            .iter()
                            .fold(0_u64, |acc, byte| (acc << 8) | u64::from(*byte));
                        if child.id == 0x53ab {
                            id = u32::try_from(value).ok();
                        } else {
                            position = Some(value);
                        }
                    }
                    child_offset = child.end;
                }
                let (Some(id), Some(position)) = (id, position) else {
                    return Ok(None);
                };
                let Some(absolute) = segment.payload.checked_add(position) else {
                    return Ok(None);
                };
                if absolute >= segment.end {
                    return Ok(None);
                }
                if id == 0x114d_9b74 {
                    if !head_offsets.insert(absolute) || heads.len() >= 2 {
                        return Ok(None);
                    }
                    let nested = element(file, absolute, segment.end)?;
                    if nested.id != id || nested.unknown_size {
                        return Ok(None);
                    }
                    heads.push(nested);
                } else if matches!(id, 0x1549_a966 | 0x1654_ae6b | 0x1043_a770 | 0x1941_a469) {
                    references.push((absolute, id));
                }
            }
            offset = seek.end;
        }
    }
    if !references.iter().any(|(_, id)| *id == 0x1549_a966)
        || !references.iter().any(|(_, id)| *id == 0x1654_ae6b)
    {
        return Ok(None);
    }
    references.sort_unstable();
    if references.windows(2).any(|pair| pair[0].0 == pair[1].0) {
        return Ok(None);
    }
    let mut metadata = Vec::new();
    for (offset, id) in references {
        control.check()?;
        let actual = element(file, offset, segment.end)?;
        if actual.id != id || actual.unknown_size {
            return Ok(None);
        }
        append_mkv_metadata_element(file, offset, actual, &mut metadata, policy, control)?;
    }
    let mut output = wrap(0x1a45_dfa3, &[]);
    output.extend(wrap(0x1853_8067, &metadata));
    Ok(Some(output))
}

fn append_mkv_metadata_element(
    file: &File,
    offset: u64,
    current: Element,
    metadata: &mut Vec<u8>,
    policy: &crate::ResourcePolicy,
    control: &crate::WorkControl,
) -> Result<()> {
    match current.id {
        0x1549_a966 | 0x1654_ae6b | 0x1043_a770 => {
            append(file, offset, current.end - offset, metadata, policy)?;
        }
        0x1941_a469 => {
            // Probe needs attachment count, not embedded image/font data.
            let mut attachment_offset = current.payload;
            let mut attachments = Vec::new();
            let mut visits = 0;
            while attachment_offset < current.end {
                control.check()?;
                visits += 1;
                if visits > MAX_ELEMENTS {
                    bail!("Matroska attachment budget exceeded");
                }
                let attachment = element(file, attachment_offset, current.end)?;
                if attachment.id == 0x61a7 {
                    attachments.extend(wrap(0x61a7, &[]));
                }
                attachment_offset = attachment.end;
            }
            metadata.extend(wrap(0x1941_a469, &attachments));
        }
        _ => {}
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::{Seek, SeekFrom, Write};

    fn indexed_mkv_fixture(corrupt_index: bool) -> Vec<u8> {
        fn seek(id: u32, position: u64) -> Vec<u8> {
            let mut children = wrap(0x53ab, &id.to_be_bytes());
            children.extend(wrap(0x53ac, &position.to_be_bytes()));
            wrap(0x4d_bb, &children)
        }
        let info = wrap(0x1549_a966, &[]);
        let tracks = wrap(0x1654_ae6b, &[]);
        let chapters = wrap(0x1043_a770, &[]);
        let attachments = wrap(0x1941_a469, &wrap(0x61a7, &[]));
        let ids = [0x1549_a966, 0x1654_ae6b, 0x1043_a770, 0x1941_a469];
        let head_len = wrap(
            0x114d_9b74,
            &ids.iter().flat_map(|id| seek(*id, 0)).collect::<Vec<_>>(),
        )
        .len();
        let mut positions = Vec::new();
        let mut cursor = head_len as u64;
        positions.push(cursor);
        cursor += info.len() as u64;
        positions.push(cursor);
        cursor += tracks.len() as u64;
        for _ in 0..1000 {
            cursor += wrap(0x1f43_b675, &[]).len() as u64;
        }
        positions.push(cursor);
        cursor += chapters.len() as u64;
        positions.push(cursor);
        let head_children = ids
            .iter()
            .zip(positions)
            .flat_map(|(id, position)| {
                seek(
                    *id,
                    if corrupt_index && *id == 0x1043_a770 {
                        position + 1
                    } else {
                        position
                    },
                )
            })
            .collect::<Vec<_>>();
        let mut segment = wrap(0x114d_9b74, &head_children);
        segment.extend(info);
        segment.extend(tracks);
        for _ in 0..1000 {
            segment.extend(wrap(0x1f43_b675, &[]));
        }
        segment.extend(chapters);
        segment.extend(attachments);
        let mut file = wrap(0x1a45_dfa3, &[]);
        file.extend(wrap(0x1853_8067, &segment));
        file
    }

    #[test]
    fn indexed_matroska_reads_late_metadata_without_visiting_clusters() {
        let file = tempfile::NamedTempFile::new().unwrap();
        std::fs::write(file.path(), indexed_mkv_fixture(false)).unwrap();
        METADATA_READS.with(|counter| counter.set(0));
        let (_, indexed) = read_metadata(file.path()).unwrap();
        let indexed_reads = METADATA_READS.with(|counter| counter.get());
        assert!(indexed_reads < 80, "indexed reads: {indexed_reads}");

        std::fs::write(file.path(), indexed_mkv_fixture(true)).unwrap();
        METADATA_READS.with(|counter| counter.set(0));
        let (_, fallback) = read_metadata(file.path()).unwrap();
        let fallback_reads = METADATA_READS.with(|counter| counter.get());
        assert_eq!(indexed, fallback);
        assert!(fallback_reads > 1000, "fallback reads: {fallback_reads}");
    }

    #[test]
    fn skips_sparse_mp4_payload_and_finds_tail_metadata() {
        let mut file = tempfile::NamedTempFile::new().unwrap();
        file.write_all(&[
            0, 0, 0, 16, b'f', b't', b'y', b'p', b'i', b's', b'o', b'm', 0, 0, 0, 0,
        ])
        .unwrap();
        let size = 128_u32 * 1024 * 1024;
        file.write_all(&size.to_be_bytes()).unwrap();
        file.write_all(b"mdat").unwrap();
        file.seek(SeekFrom::Start(16 + u64::from(size))).unwrap();
        file.write_all(&[0, 0, 0, 8, b'm', b'o', b'o', b'v'])
            .unwrap();
        let (len, metadata) = read_metadata(file.path()).unwrap();
        assert_eq!(len, 24 + u64::from(size));
        assert_eq!(metadata.len(), 24);
    }

    #[test]
    fn finds_metadata_after_sparse_payload_above_four_gib() {
        let mut file = tempfile::NamedTempFile::new().unwrap();
        file.write_all(&[
            0, 0, 0, 16, b'f', b't', b'y', b'p', b'i', b's', b'o', b'm', 0, 0, 0, 0,
        ])
        .unwrap();
        let payload_box_size = 5_u64 * 1024 * 1024 * 1024 + 16;
        file.write_all(&1_u32.to_be_bytes()).unwrap();
        file.write_all(b"mdat").unwrap();
        file.write_all(&payload_box_size.to_be_bytes()).unwrap();
        file.seek(SeekFrom::Start(16 + payload_box_size)).unwrap();
        file.write_all(&[0, 0, 0, 8, b'm', b'o', b'o', b'v'])
            .unwrap();
        let (len, metadata) = read_metadata(file.path()).unwrap();
        assert_eq!(len, 24 + payload_box_size);
        assert_eq!(metadata.len(), 24);
    }

    #[test]
    fn ebml_size_reader_rejects_truncation_and_handles_unknown_segment_size() {
        let mut file = tempfile::NamedTempFile::new().unwrap();
        file.write_all(&[0x18, 0x53, 0x80, 0x67, 0xff]).unwrap();
        let header = element(file.as_file(), 0, 5).unwrap();
        assert_eq!(header.payload, 5);
        assert_eq!(header.end, 5);
        assert!(element(file.as_file(), 0, 4).is_err());
    }
}
