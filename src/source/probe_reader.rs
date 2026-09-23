//! Bounded positional metadata reads. Media payload is never mapped or copied.
use std::{fs::File, io, path::Path};

use anyhow::{Context, Result, bail};

use super::{MediaSourceIdentity, read_exact_at};

const MAX_METADATA: usize = 64 * 1024 * 1024;
const MAX_ELEMENTS: usize = 100_000;

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
    Ok((len, bytes))
}

fn read(file: &File, offset: u64, size: usize) -> io::Result<Vec<u8>> {
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

pub(crate) struct Element {
    pub(crate) unknown_size: bool,
    pub(crate) id: u32,
    pub(crate) payload: u64,
    pub(crate) end: u64,
}

pub(crate) fn element(file: &File, offset: u64, end: u64) -> Result<Element> {
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
        match current.id {
            0x1549_a966 | 0x1654_ae6b | 0x1043_a770 => {
                append(file, offset, current.end - offset, &mut metadata, policy)?;
            }
            0x1941_a469 => {
                // Probe needs attachment count, not embedded image/font data.
                let mut attachment_offset = current.payload;
                let mut attachments = Vec::new();
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
        offset = current.end;
    }
    let mut output = wrap(0x1a45_dfa3, &[]);
    output.extend(wrap(0x1853_8067, &metadata));
    Ok(output)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::{Seek, SeekFrom, Write};

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
