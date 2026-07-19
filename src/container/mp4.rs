use crate::container::ContainerKind;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Mp4BasicMetadata {
    pub major_brand: Option<String>,
    pub compatible_brands: Vec<String>,
    pub duration_ms: Option<u64>,
    pub tracks: Vec<Mp4Track>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Mp4Track {
    pub index: u32,
    pub kind: Mp4TrackKind,
    pub codec: String,
    pub duration_ms: Option<u64>,
    pub width: Option<u32>,
    pub height: Option<u32>,
    pub channels: Option<u32>,
    pub sample_rate: Option<u32>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Mp4TrackKind {
    Video,
    Audio,
    Subtitle,
    Unknown,
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
        tracks: Vec::new(),
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
        } else if atom.kind == *b"trak" {
            let index = meta.tracks.len() as u32;
            if let Some(track) = parse_trak(atom.payload, index) {
                meta.tracks.push(track);
            }
        }
    }
}

fn parse_trak(payload: &[u8], index: u32) -> Option<Mp4Track> {
    let mut tkhd_size: Option<(u32, u32)> = None;
    let mut mdia: Option<MdiaInfo> = None;

    for atom in AtomIter::new(payload) {
        if atom.kind == *b"tkhd" {
            tkhd_size = parse_tkhd_size(atom.payload);
        } else if atom.kind == *b"mdia" {
            mdia = parse_mdia(atom.payload);
        }
    }

    let mdia = mdia?;
    let stsd = mdia.sample_entry?;
    let kind = handler_to_track_kind(mdia.handler.as_deref());
    let codec = sample_entry_codec(&stsd.codec_fourcc, kind);
    let (width, height) = if stsd.width.is_some() || stsd.height.is_some() {
        (stsd.width, stsd.height)
    } else {
        tkhd_size
            .map(|(w, h)| (Some(w), Some(h)))
            .unwrap_or((None, None))
    };

    Some(Mp4Track {
        index,
        kind,
        codec,
        duration_ms: mdia.duration_ms,
        width,
        height,
        channels: stsd.channels,
        sample_rate: stsd.sample_rate,
    })
}

#[derive(Debug, Clone)]
struct MdiaInfo {
    handler: Option<String>,
    duration_ms: Option<u64>,
    sample_entry: Option<SampleEntryInfo>,
}

#[derive(Debug, Clone)]
struct SampleEntryInfo {
    codec_fourcc: [u8; 4],
    width: Option<u32>,
    height: Option<u32>,
    channels: Option<u32>,
    sample_rate: Option<u32>,
}

fn parse_mdia(payload: &[u8]) -> Option<MdiaInfo> {
    let mut handler = None;
    let mut duration_ms = None;
    let mut sample_entry = None;

    for atom in AtomIter::new(payload) {
        if atom.kind == *b"hdlr" {
            handler = parse_hdlr(atom.payload);
        } else if atom.kind == *b"mdhd" {
            duration_ms = parse_mdhd_duration_ms(atom.payload);
        } else if atom.kind == *b"minf" {
            sample_entry = parse_minf(atom.payload);
        }
    }

    Some(MdiaInfo {
        handler,
        duration_ms,
        sample_entry,
    })
}

fn parse_minf(payload: &[u8]) -> Option<SampleEntryInfo> {
    for atom in AtomIter::new(payload) {
        if atom.kind == *b"stbl" {
            return parse_stbl(atom.payload);
        }
    }
    None
}

fn parse_stbl(payload: &[u8]) -> Option<SampleEntryInfo> {
    for atom in AtomIter::new(payload) {
        if atom.kind == *b"stsd" {
            return parse_stsd(atom.payload);
        }
    }
    None
}

fn parse_stsd(payload: &[u8]) -> Option<SampleEntryInfo> {
    if payload.len() < 16 {
        return None;
    }
    let entry_count = read_u32(&payload[4..8])?;
    if entry_count == 0 {
        return None;
    }
    let entry_size = read_u32(&payload[8..12])? as usize;
    if entry_size < 8 || 8 + entry_size > payload.len() {
        return None;
    }
    let codec_fourcc: [u8; 4] = payload[12..16].try_into().ok()?;
    let entry_payload = &payload[16..8 + entry_size];
    Some(parse_sample_entry(codec_fourcc, entry_payload))
}

fn parse_sample_entry(codec_fourcc: [u8; 4], payload: &[u8]) -> SampleEntryInfo {
    let mut out = SampleEntryInfo {
        codec_fourcc,
        width: None,
        height: None,
        channels: None,
        sample_rate: None,
    };

    if is_video_sample_entry(&codec_fourcc) {
        if payload.len() >= 28 {
            out.width = read_u16(&payload[24..26]).map(u32::from);
            out.height = read_u16(&payload[26..28]).map(u32::from);
        }
    } else if is_audio_sample_entry(&codec_fourcc) && payload.len() >= 28 {
        out.channels = read_u16(&payload[16..18]).map(u32::from);
        out.sample_rate = read_u32(&payload[24..28]).map(|v| v >> 16);
    }

    out
}

fn parse_hdlr(payload: &[u8]) -> Option<String> {
    if payload.len() < 12 {
        return None;
    }
    fourcc_to_string(&payload[8..12])
}

fn parse_mdhd_duration_ms(payload: &[u8]) -> Option<u64> {
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

fn parse_tkhd_size(payload: &[u8]) -> Option<(u32, u32)> {
    let version = *payload.first()?;
    let (width_offset, height_offset) = if version == 1 { (88, 92) } else { (76, 80) };
    if payload.len() < height_offset + 4 {
        return None;
    }
    let width = fixed_16_16_to_u32(read_u32(&payload[width_offset..width_offset + 4])?);
    let height = fixed_16_16_to_u32(read_u32(&payload[height_offset..height_offset + 4])?);
    if width == 0 || height == 0 {
        return None;
    }
    Some((width, height))
}

fn fixed_16_16_to_u32(value: u32) -> u32 {
    (value >> 16) + u32::from((value & 0xffff) >= 0x8000)
}

fn handler_to_track_kind(handler: Option<&str>) -> Mp4TrackKind {
    match handler {
        Some("vide") => Mp4TrackKind::Video,
        Some("soun") => Mp4TrackKind::Audio,
        Some("text" | "sbtl" | "subt") => Mp4TrackKind::Subtitle,
        _ => Mp4TrackKind::Unknown,
    }
}

fn sample_entry_codec(fourcc: &[u8; 4], kind: Mp4TrackKind) -> String {
    match fourcc {
        b"avc1" | b"avc3" => "h264".to_string(),
        b"hvc1" | b"hev1" => "hevc".to_string(),
        b"dvh1" | b"dvhe" => "hevc".to_string(),
        b"av01" => "av1".to_string(),
        b"vp09" => "vp9".to_string(),
        b"mp4a" if kind == Mp4TrackKind::Audio => "aac".to_string(),
        b"ac-3" => "ac3".to_string(),
        b"ec-3" => "eac3".to_string(),
        b"alac" => "alac".to_string(),
        b"fLaC" => "flac".to_string(),
        b"Opus" => "opus".to_string(),
        b"tx3g" | b"text" => "mov_text".to_string(),
        b"wvtt" => "webvtt".to_string(),
        _ => fourcc_to_string(fourcc).unwrap_or_else(|| "unknown".to_string()),
    }
}

fn is_video_sample_entry(fourcc: &[u8; 4]) -> bool {
    matches!(
        fourcc,
        b"avc1" | b"avc3" | b"hvc1" | b"hev1" | b"dvh1" | b"dvhe" | b"av01" | b"vp09" | b"mp4v"
    )
}

fn is_audio_sample_entry(fourcc: &[u8; 4]) -> bool {
    matches!(
        fourcc,
        b"mp4a" | b"ac-3" | b"ec-3" | b"alac" | b"fLaC" | b"Opus" | b".mp3"
    )
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

fn read_u16(bytes: &[u8]) -> Option<u16> {
    Some(u16::from_be_bytes(bytes.try_into().ok()?))
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
        let mut data = ftyp();

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

    #[test]
    fn parses_video_and_audio_tracks() {
        let mut data = ftyp();
        let moov = atom(
            b"moov",
            &[
                trak(b"vide", b"avc1", Some((1920, 1080)), None, 3000, 1000),
                trak(b"soun", b"mp4a", None, Some((2, 48000)), 144000, 48000),
            ]
            .concat(),
        );
        data.extend_from_slice(&moov);

        let meta = parse_basic_metadata(&data);
        assert_eq!(meta.tracks.len(), 2);
        assert_eq!(meta.tracks[0].kind, Mp4TrackKind::Video);
        assert_eq!(meta.tracks[0].codec, "h264");
        assert_eq!(meta.tracks[0].width, Some(1920));
        assert_eq!(meta.tracks[0].height, Some(1080));
        assert_eq!(meta.tracks[0].duration_ms, Some(3000));
        assert_eq!(meta.tracks[1].kind, Mp4TrackKind::Audio);
        assert_eq!(meta.tracks[1].codec, "aac");
        assert_eq!(meta.tracks[1].channels, Some(2));
        assert_eq!(meta.tracks[1].sample_rate, Some(48000));
    }

    fn ftyp() -> Vec<u8> {
        atom(
            b"ftyp",
            &[
                b"isom".as_slice(),
                &0_u32.to_be_bytes(),
                b"isom".as_slice(),
                b"mp42".as_slice(),
            ]
            .concat(),
        )
    }

    fn trak(
        handler: &[u8; 4],
        sample_entry: &[u8; 4],
        size: Option<(u32, u32)>,
        audio: Option<(u16, u32)>,
        duration: u32,
        timescale: u32,
    ) -> Vec<u8> {
        atom(
            b"trak",
            &[
                tkhd(size),
                mdia(handler, sample_entry, size, audio, duration, timescale),
            ]
            .concat(),
        )
    }

    fn tkhd(size: Option<(u32, u32)>) -> Vec<u8> {
        let mut payload = vec![0_u8; 84];
        if let Some((w, h)) = size {
            payload[76..80].copy_from_slice(&(w << 16).to_be_bytes());
            payload[80..84].copy_from_slice(&(h << 16).to_be_bytes());
        }
        atom(b"tkhd", &payload)
    }

    fn mdia(
        handler: &[u8; 4],
        sample_entry: &[u8; 4],
        size: Option<(u32, u32)>,
        audio: Option<(u16, u32)>,
        duration: u32,
        timescale: u32,
    ) -> Vec<u8> {
        atom(
            b"mdia",
            &[
                mdhd(timescale, duration),
                hdlr(handler),
                atom(b"minf", &atom(b"stbl", &stsd(sample_entry, size, audio))),
            ]
            .concat(),
        )
    }

    fn mdhd(timescale: u32, duration: u32) -> Vec<u8> {
        let mut payload = vec![0_u8; 20];
        payload[12..16].copy_from_slice(&timescale.to_be_bytes());
        payload[16..20].copy_from_slice(&duration.to_be_bytes());
        atom(b"mdhd", &payload)
    }

    fn hdlr(handler: &[u8; 4]) -> Vec<u8> {
        let mut payload = vec![0_u8; 12];
        payload[8..12].copy_from_slice(handler);
        atom(b"hdlr", &payload)
    }

    fn stsd(
        sample_entry: &[u8; 4],
        size: Option<(u32, u32)>,
        audio: Option<(u16, u32)>,
    ) -> Vec<u8> {
        let entry = sample_entry_atom(sample_entry, size, audio);
        let mut payload = Vec::new();
        payload.extend_from_slice(&[0, 0, 0, 0]);
        payload.extend_from_slice(&1_u32.to_be_bytes());
        payload.extend_from_slice(&entry);
        atom(b"stsd", &payload)
    }

    fn sample_entry_atom(
        fourcc: &[u8; 4],
        size: Option<(u32, u32)>,
        audio: Option<(u16, u32)>,
    ) -> Vec<u8> {
        let mut payload = vec![0_u8; 28];
        if let Some((w, h)) = size {
            payload[24..26].copy_from_slice(&(w as u16).to_be_bytes());
            payload[26..28].copy_from_slice(&(h as u16).to_be_bytes());
        }
        if let Some((channels, sample_rate)) = audio {
            payload[16..18].copy_from_slice(&channels.to_be_bytes());
            payload[24..28].copy_from_slice(&(sample_rate << 16).to_be_bytes());
        }
        atom(fourcc, &payload)
    }

    fn atom(kind: &[u8; 4], payload: &[u8]) -> Vec<u8> {
        let mut out = Vec::with_capacity(payload.len() + 8);
        out.extend_from_slice(&((payload.len() + 8) as u32).to_be_bytes());
        out.extend_from_slice(kind);
        out.extend_from_slice(payload);
        out
    }
}
