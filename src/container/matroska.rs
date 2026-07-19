#[derive(Debug, Clone, PartialEq)]
pub struct MatroskaBasicMetadata {
    pub duration_ms: Option<u64>,
    pub tracks: Vec<MatroskaTrack>,
    pub attachment_count: u32,
}

#[derive(Debug, Clone, PartialEq)]
pub struct MatroskaTrack {
    pub index: u32,
    pub number: u64,
    pub kind: MatroskaTrackKind,
    pub codec: String,
    pub language: Option<String>,
    pub name: Option<String>,
    pub default: bool,
    pub forced: bool,
    pub width: Option<u32>,
    pub height: Option<u32>,
    pub channels: Option<u32>,
    pub sample_rate: Option<u32>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MatroskaTrackKind {
    Video,
    Audio,
    Subtitle,
    Unknown,
}

pub fn looks_like_ebml(head: &[u8]) -> bool {
    head.len() >= 4 && head[0..4] == [0x1a, 0x45, 0xdf, 0xa3]
}

pub fn parse_basic_metadata(bytes: &[u8]) -> MatroskaBasicMetadata {
    let mut meta = MatroskaBasicMetadata {
        duration_ms: None,
        tracks: Vec::new(),
        attachment_count: 0,
    };

    let Some(segment) = find_first_child(bytes, 0x1853_8067) else {
        return meta;
    };

    for child in ElementIter::new(segment) {
        match child.id {
            0x1549_a966 => parse_info(child.payload, &mut meta),
            0x1654_ae6b => parse_tracks(child.payload, &mut meta),
            0x1941_a469 => meta.attachment_count = count_children(child.payload, 0x61a7),
            // Cluster is the media-data boundary for normal Matroska files. Probing should not
            // scan packet payloads once metadata sections have been collected.
            0x1f43_b675 if meta.duration_ms.is_some() && !meta.tracks.is_empty() => break,
            _ => {}
        }
    }

    meta
}

fn parse_info(payload: &[u8], meta: &mut MatroskaBasicMetadata) {
    let mut timecode_scale = 1_000_000_u64;
    let mut duration = None;
    for child in ElementIter::new(payload) {
        match child.id {
            0x002a_d7b1 => {
                if let Some(v) = read_uint(child.payload) {
                    timecode_scale = v;
                }
            }
            0x4489 => duration = read_float(child.payload),
            _ => {}
        }
    }
    if let Some(duration_units) = duration {
        let ns = duration_units * timecode_scale as f64;
        if ns.is_finite() && ns >= 0.0 {
            meta.duration_ms = Some((ns / 1_000_000.0).round() as u64);
        }
    }
}

fn parse_tracks(payload: &[u8], meta: &mut MatroskaBasicMetadata) {
    for entry in ElementIter::new(payload).filter(|e| e.id == 0xae) {
        if let Some(track) = parse_track_entry(entry.payload, meta.tracks.len() as u32) {
            meta.tracks.push(track);
        }
    }
}

fn parse_track_entry(payload: &[u8], index: u32) -> Option<MatroskaTrack> {
    let mut number = None;
    let mut kind = MatroskaTrackKind::Unknown;
    let mut codec_id = None;
    let mut language = None;
    let mut name = None;
    let mut default = false;
    let mut forced = false;
    let mut width = None;
    let mut height = None;
    let mut channels = None;
    let mut sample_rate = None;

    for child in ElementIter::new(payload) {
        match child.id {
            0xd7 => number = read_uint(child.payload),
            0x83 => kind = track_type(read_uint(child.payload).unwrap_or_default()),
            0x86 => codec_id = read_string(child.payload),
            0x0022_b59c | 0x0022_b59d => language = read_string(child.payload),
            0x536e => name = read_string(child.payload),
            0x88 => default = read_uint(child.payload).unwrap_or(0) != 0,
            0x55aa => forced = read_uint(child.payload).unwrap_or(0) != 0,
            0xe0 => {
                let (w, h) = parse_video(child.payload);
                width = w;
                height = h;
            }
            0xe1 => {
                let (ch, sr) = parse_audio(child.payload);
                channels = ch;
                sample_rate = sr;
            }
            _ => {}
        }
    }

    let codec = codec_id.map(|id| normalize_codec_id(&id))?;
    Some(MatroskaTrack {
        index,
        number: number.unwrap_or(u64::from(index) + 1),
        kind,
        codec,
        language,
        name,
        default,
        forced,
        width,
        height,
        channels,
        sample_rate,
    })
}

fn parse_video(payload: &[u8]) -> (Option<u32>, Option<u32>) {
    let mut width = None;
    let mut height = None;
    for child in ElementIter::new(payload) {
        match child.id {
            0xb0 => width = read_uint(child.payload).and_then(|v| u32::try_from(v).ok()),
            0xba => height = read_uint(child.payload).and_then(|v| u32::try_from(v).ok()),
            _ => {}
        }
    }
    (width, height)
}

fn parse_audio(payload: &[u8]) -> (Option<u32>, Option<u32>) {
    let mut channels = None;
    let mut sample_rate = None;
    for child in ElementIter::new(payload) {
        match child.id {
            0x9f => channels = read_uint(child.payload).and_then(|v| u32::try_from(v).ok()),
            0xb5 => {
                sample_rate = read_float(child.payload).and_then(|v| {
                    if v.is_finite() && v > 0.0 {
                        Some(v.round() as u32)
                    } else {
                        None
                    }
                })
            }
            _ => {}
        }
    }
    (channels, sample_rate)
}

fn track_type(value: u64) -> MatroskaTrackKind {
    match value {
        1 => MatroskaTrackKind::Video,
        2 => MatroskaTrackKind::Audio,
        0x11 => MatroskaTrackKind::Subtitle,
        _ => MatroskaTrackKind::Unknown,
    }
}

fn normalize_codec_id(id: &str) -> String {
    let upper = id.trim().to_ascii_uppercase();
    match upper.as_str() {
        "V_MPEG4/ISO/AVC" => "h264",
        "V_MPEGH/ISO/HEVC" => "hevc",
        "V_AV1" => "av1",
        "V_VP9" => "vp9",
        "A_AAC" | "A_AAC/MPEG4/LC" | "A_AAC/MPEG2/LC" => "aac",
        "A_AC3" => "ac3",
        "A_EAC3" => "eac3",
        "A_DTS" => "dts",
        "A_TRUEHD" => "truehd",
        "A_FLAC" => "flac",
        "A_OPUS" => "opus",
        "A_VORBIS" => "vorbis",
        "S_TEXT/UTF8" => "subrip",
        "S_TEXT/ASS" => "ass",
        "S_TEXT/SSA" => "ssa",
        "S_TEXT/WEBVTT" => "webvtt",
        "S_HDMV/PGS" => "hdmv_pgs_subtitle",
        "S_VOBSUB" => "dvd_subtitle",
        _ => id.trim(),
    }
    .to_string()
}

fn find_first_child(bytes: &[u8], id: u32) -> Option<&[u8]> {
    ElementIter::new(bytes)
        .find(|element| element.id == id)
        .map(|element| element.payload)
}

fn count_children(bytes: &[u8], id: u32) -> u32 {
    ElementIter::new(bytes)
        .filter(|element| element.id == id)
        .count()
        .try_into()
        .unwrap_or(u32::MAX)
}

fn read_uint(bytes: &[u8]) -> Option<u64> {
    if bytes.is_empty() || bytes.len() > 8 {
        return None;
    }
    let mut value = 0_u64;
    for b in bytes {
        value = (value << 8) | u64::from(*b);
    }
    Some(value)
}

fn read_float(bytes: &[u8]) -> Option<f64> {
    match bytes.len() {
        4 => Some(f32::from_be_bytes(bytes.try_into().ok()?) as f64),
        8 => Some(f64::from_be_bytes(bytes.try_into().ok()?)),
        _ => None,
    }
}

fn read_string(bytes: &[u8]) -> Option<String> {
    let s = String::from_utf8_lossy(bytes)
        .trim_matches(char::from(0))
        .trim()
        .to_string();
    if s.is_empty() {
        None
    } else {
        Some(s)
    }
}

#[derive(Debug, Clone, Copy)]
struct Element<'a> {
    id: u32,
    payload: &'a [u8],
}

struct ElementIter<'a> {
    bytes: &'a [u8],
    offset: usize,
}

impl<'a> ElementIter<'a> {
    fn new(bytes: &'a [u8]) -> Self {
        Self { bytes, offset: 0 }
    }
}

impl<'a> Iterator for ElementIter<'a> {
    type Item = Element<'a>;

    fn next(&mut self) -> Option<Self::Item> {
        if self.offset >= self.bytes.len() {
            return None;
        }
        let (id, id_len) = read_vint_id(&self.bytes[self.offset..])?;
        let size_offset = self.offset + id_len;
        let (size, size_len) = read_vint_size(&self.bytes[size_offset..])?;
        let payload_start = size_offset + size_len;
        let payload_end = payload_start.checked_add(size)?;
        if payload_end > self.bytes.len() {
            self.offset = self.bytes.len();
            return None;
        }
        self.offset = payload_end;
        Some(Element {
            id,
            payload: &self.bytes[payload_start..payload_end],
        })
    }
}

fn read_vint_id(bytes: &[u8]) -> Option<(u32, usize)> {
    let first = *bytes.first()?;
    let len = vint_len(first)?;
    if len > 4 || bytes.len() < len {
        return None;
    }
    let mut value = 0_u32;
    for b in &bytes[..len] {
        value = (value << 8) | u32::from(*b);
    }
    Some((value, len))
}

fn read_vint_size(bytes: &[u8]) -> Option<(usize, usize)> {
    let first = *bytes.first()?;
    let len = vint_len(first)?;
    if len > 8 || bytes.len() < len {
        return None;
    }
    let mask = 1_u8 << (8 - len);
    let mut value = usize::from(first & !mask);
    for b in &bytes[1..len] {
        value = (value << 8) | usize::from(*b);
    }
    Some((value, len))
}

fn vint_len(first: u8) -> Option<usize> {
    if first == 0 {
        return None;
    }
    Some(first.leading_zeros() as usize + 1)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn detects_ebml() {
        assert!(looks_like_ebml(&[0x1a, 0x45, 0xdf, 0xa3, 0x9f]));
    }

    #[test]
    fn parses_info_and_tracks() {
        let info = elem(
            0x1549_a966,
            &[
                elem(0x002a_d7b1, &1_000_000_u64.to_be_bytes()[5..]),
                elem(0x4489, &12_500_f64.to_be_bytes()),
            ]
            .concat(),
        );
        let video = track_entry(
            1,
            1,
            "V_MPEG4/ISO/AVC",
            &[elem(
                0xe0,
                &[elem(0xb0, &[0x07, 0x80]), elem(0xba, &[0x04, 0x38])].concat(),
            )],
        );
        let audio = track_entry(
            2,
            2,
            "A_AAC",
            &[elem(
                0xe1,
                &[elem(0x9f, &[0x02]), elem(0xb5, &48_000_f64.to_be_bytes())].concat(),
            )],
        );
        let tracks = elem(0x1654_ae6b, &[video, audio].concat());
        let segment = elem(0x1853_8067, &[info, tracks].concat());
        let mut bytes = elem(0x1a45_dfa3, &[]);
        bytes.extend_from_slice(&segment);

        let meta = parse_basic_metadata(&bytes);
        assert_eq!(meta.duration_ms, Some(12_500));
        assert_eq!(meta.tracks.len(), 2);
        assert_eq!(meta.tracks[0].codec, "h264");
        assert_eq!(meta.tracks[0].width, Some(1920));
        assert_eq!(meta.tracks[0].height, Some(1080));
        assert_eq!(meta.tracks[1].codec, "aac");
        assert_eq!(meta.tracks[1].channels, Some(2));
        assert_eq!(meta.tracks[1].sample_rate, Some(48000));
    }

    fn track_entry(number: u8, kind: u8, codec: &str, extra: &[Vec<u8>]) -> Vec<u8> {
        let mut payload = Vec::new();
        payload.extend_from_slice(&elem(0xd7, &[number]));
        payload.extend_from_slice(&elem(0x83, &[kind]));
        payload.extend_from_slice(&elem(0x86, codec.as_bytes()));
        for e in extra {
            payload.extend_from_slice(e);
        }
        elem(0xae, &payload)
    }

    fn elem(id: u32, payload: &[u8]) -> Vec<u8> {
        let mut out = Vec::new();
        write_id(id, &mut out);
        write_size(payload.len(), &mut out);
        out.extend_from_slice(payload);
        out
    }

    fn write_id(id: u32, out: &mut Vec<u8>) {
        let bytes = id.to_be_bytes();
        let first = bytes
            .iter()
            .position(|b| *b != 0)
            .unwrap_or(bytes.len() - 1);
        out.extend_from_slice(&bytes[first..]);
    }

    fn write_size(size: usize, out: &mut Vec<u8>) {
        assert!(size < 0x7f);
        out.push(0x80 | size as u8);
    }
}
