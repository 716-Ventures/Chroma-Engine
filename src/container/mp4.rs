use crate::container::ContainerKind;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Mp4BasicMetadata {
    pub major_brand: Option<String>,
    pub compatible_brands: Vec<String>,
    pub duration_ms: Option<u64>,
}

pub fn looks_like_mp4(head: &[u8]) -> bool {
    head.len() >= 12 && &head[4..8] == b"ftyp"
}

pub fn sniff_mp4_brand(head: &[u8]) -> ContainerKind {
    if !looks_like_mp4(head) || head.len() < 12 {
        return ContainerKind::Unknown;
    }

    let major = &head[8..12];
    match major {
        b"qt  " => ContainerKind::Mov,
        _ => ContainerKind::Mp4,
    }
}

pub fn parse_basic_metadata(bytes: &[u8]) -> Mp4BasicMetadata {
    let mut meta = Mp4BasicMetadata {
        major_brand: None,
        compatible_brands: Vec::new(),
        duration_ms: None,
    };

    for atom in AtomIter::new(bytes) {
        if atom.kind == *b"ftyp" {
            parse_ftyp(atom.payload, &mut meta);
        } else if atom.kind == *b"moov" {
            parse_moov(atom.payload, &mut meta);
        }
    }

    meta
}

fn parse_ftyp(payload: &[u8], meta: &mut Mp4BasicMetadata) {
    if payload.len() < 8 {
        return;
    }
    meta.major_brand = fourcc_to_string(&payload[0..4]);
    let mut offset = 8;
    while offset + 4 <= payload.len() {
        if let Some(brand) = fourcc_to_string(&payload[offset..offset + 4]) {
            meta.compatible_brands.push(brand);
        }
        offset += 4;
    }
}

fn parse_moov(payload: &[u8], meta: &mut Mp4BasicMetadata) {
    for atom in AtomIter::new(payload) {
        if atom.kind == *b"mvhd" {
            meta.duration_ms = parse_mvhd_duration_ms(atom.payload);
        }
    }
}

fn parse_mvhd_duration_ms(payload: &[u8]) -> Option<u64> {
    let version = *payload.first()?;
    if version == 1 {
        if payload.len() < 32 {
            return None;
        }
        let timescale = read_u32(&payload[20..24])?;
        let duration = read_u64(&payload[24..32])?;
        duration_to_ms(duration, timescale)
    } else {
        if payload.len() < 20 {
            return None;
        }
        let timescale = read_u32(&payload[12..16])?;
        let duration = read_u32(&payload[16..20])? as u64;
        duration_to_ms(duration, timescale)
    }
}

fn duration_to_ms(duration: u64, timescale: u32) -> Option<u64> {
    if timescale == 0 {
        return None;
    }
    Some(duration.saturating_mul(1000) / u64::from(timescale))
}

fn fourcc_to_string(bytes: &[u8]) -> Option<String> {
    if bytes.len() != 4 || !bytes.iter().all(|b| b.is_ascii_graphic() || *b == b' ') {
        return None;
    }
    Some(String::from_utf8_lossy(bytes).trim().to_string())
}

fn read_u32(bytes: &[u8]) -> Option<u32> {
    Some(u32::from_be_bytes(bytes.try_into().ok()?))
}

fn read_u64(bytes: &[u8]) -> Option<u64> {
    Some(u64::from_be_bytes(bytes.try_into().ok()?))
}

#[derive(Debug, Clone, Copy)]
struct Atom<'a> {
    kind: [u8; 4],
    payload: &'a [u8],
}

struct AtomIter<'a> {
    bytes: &'a [u8],
    offset: usize,
}

impl<'a> AtomIter<'a> {
    fn new(bytes: &'a [u8]) -> Self {
        Self { bytes, offset: 0 }
    }
}

impl<'a> Iterator for AtomIter<'a> {
    type Item = Atom<'a>;

    fn next(&mut self) -> Option<Self::Item> {
        if self.offset + 8 > self.bytes.len() {
            return None;
        }

        let start = self.offset;
        let size32 = read_u32(&self.bytes[start..start + 4])? as usize;
        let kind: [u8; 4] = self.bytes[start + 4..start + 8].try_into().ok()?;
        let (header, size) = if size32 == 1 {
            if start + 16 > self.bytes.len() {
                return None;
            }
            let size64 = read_u64(&self.bytes[start + 8..start + 16])? as usize;
            (16, size64)
        } else if size32 == 0 {
            (8, self.bytes.len() - start)
        } else {
            (8, size32)
        };

        if size < header || start + size > self.bytes.len() {
            self.offset = self.bytes.len();
            return None;
        }

        self.offset = start + size;
        Some(Atom {
            kind,
            payload: &self.bytes[start + header..start + size],
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn detects_ftyp() {
        let mut head = vec![0_u8; 16];
        head[4..8].copy_from_slice(b"ftyp");
        head[8..12].copy_from_slice(b"isom");
        assert!(looks_like_mp4(&head));
        assert_eq!(sniff_mp4_brand(&head), ContainerKind::Mp4);
    }

    #[test]
    fn detects_mov_brand() {
        let mut head = vec![0_u8; 16];
        head[4..8].copy_from_slice(b"ftyp");
        head[8..12].copy_from_slice(b"qt  ");
        assert_eq!(sniff_mp4_brand(&head), ContainerKind::Mov);
    }

    #[test]
    fn parses_mvhd_duration() {
        let mut data = Vec::new();
        data.extend_from_slice(&24_u32.to_be_bytes());
        data.extend_from_slice(b"ftyp");
        data.extend_from_slice(b"isom");
        data.extend_from_slice(&0_u32.to_be_bytes());
        data.extend_from_slice(b"isom");
        data.extend_from_slice(b"mp42");

        let mut mvhd_payload = vec![0_u8; 20];
        mvhd_payload[12..16].copy_from_slice(&1000_u32.to_be_bytes());
        mvhd_payload[16..20].copy_from_slice(&12_345_u32.to_be_bytes());

        let mvhd_size = 8 + mvhd_payload.len() as u32;
        let moov_size = 8 + mvhd_size;
        data.extend_from_slice(&moov_size.to_be_bytes());
        data.extend_from_slice(b"moov");
        data.extend_from_slice(&mvhd_size.to_be_bytes());
        data.extend_from_slice(b"mvhd");
        data.extend_from_slice(&mvhd_payload);

        let meta = parse_basic_metadata(&data);
        assert_eq!(meta.major_brand.as_deref(), Some("isom"));
        assert_eq!(meta.duration_ms, Some(12_345));
    }
}
