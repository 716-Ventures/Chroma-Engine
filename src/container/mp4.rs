use crate::{
    container::ContainerKind,
    packet::{
        extract_packet_payload, packet_samples_for_range, plan_track_chunks, ChunkPlan,
        ExtractedChunk, NativeChunk, PacketExtractError, PacketRange, PacketRef, TimeDelta,
        TimePoint, TimeScale,
    },
};
use serde::{Deserialize, Serialize};

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

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Mp4PacketIndex {
    pub tracks: Vec<Mp4TrackPacketIndex>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Mp4TrackPacketIndex {
    pub track_id: String,
    pub track_index: u32,
    pub kind: Mp4TrackKind,
    pub timescale: TimeScale,
    pub packets: Vec<PacketRef>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct Mp4CodecConfig {
    pub track_id: String,
    pub track_kind: String,
    pub codec: String,
    pub sample_entry: String,
    pub config_box: Option<String>,
    pub codec_string: Option<String>,
    pub description_hex: Option<String>,
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

pub fn parse_packet_index(bytes: &[u8]) -> Mp4PacketIndex {
    let mut tracks = Vec::new();
    let mut video_index = 0_u32;
    let mut audio_index = 0_u32;
    let mut subtitle_index = 0_u32;
    let mut unknown_index = 0_u32;

    for atom in AtomIter::new(bytes) {
        if atom.kind != *b"moov" {
            continue;
        }

        for trak in AtomIter::new(atom.payload).filter(|atom| atom.kind == *b"trak") {
            let track_index = tracks.len() as u32;
            let Some(kind) = parse_trak_kind(trak.payload) else {
                continue;
            };
            let track_id = match kind {
                Mp4TrackKind::Video => next_track_id("v", &mut video_index),
                Mp4TrackKind::Audio => next_track_id("a", &mut audio_index),
                Mp4TrackKind::Subtitle => next_track_id("s", &mut subtitle_index),
                Mp4TrackKind::Unknown => next_track_id("x", &mut unknown_index),
            };
            if let Some(track) = parse_trak_packet_index(trak.payload, track_index, kind, track_id)
            {
                tracks.push(track);
            }
        }
    }

    Mp4PacketIndex { tracks }
}

pub fn parse_packet_track(
    bytes: &[u8],
    requested_track_id: Option<&str>,
) -> Option<Mp4TrackPacketIndex> {
    let mut video_index = 0_u32;
    let mut audio_index = 0_u32;
    let mut subtitle_index = 0_u32;
    let mut unknown_index = 0_u32;
    let mut absolute_track_index = 0_u32;

    for atom in AtomIter::new(bytes) {
        if atom.kind != *b"moov" {
            continue;
        }

        for trak in AtomIter::new(atom.payload).filter(|atom| atom.kind == *b"trak") {
            let Some(kind) = parse_trak_kind(trak.payload) else {
                absolute_track_index += 1;
                continue;
            };
            let track_id = match kind {
                Mp4TrackKind::Video => next_track_id("v", &mut video_index),
                Mp4TrackKind::Audio => next_track_id("a", &mut audio_index),
                Mp4TrackKind::Subtitle => next_track_id("s", &mut subtitle_index),
                Mp4TrackKind::Unknown => next_track_id("x", &mut unknown_index),
            };
            let should_parse = requested_track_id
                .map(|requested| requested == track_id)
                .unwrap_or(kind == Mp4TrackKind::Video);
            if should_parse {
                return parse_trak_packet_index(trak.payload, absolute_track_index, kind, track_id);
            }
            absolute_track_index += 1;
        }
    }

    None
}

pub fn parse_chunk_plan(
    bytes: &[u8],
    requested_track_id: Option<&str>,
    target_ms: u64,
) -> Option<ChunkPlan> {
    let mut video_index = 0_u32;
    let mut audio_index = 0_u32;
    let mut subtitle_index = 0_u32;
    let mut unknown_index = 0_u32;

    for atom in AtomIter::new(bytes) {
        if atom.kind != *b"moov" {
            continue;
        }

        for trak in AtomIter::new(atom.payload).filter(|atom| atom.kind == *b"trak") {
            let Some(kind) = parse_trak_kind(trak.payload) else {
                continue;
            };
            let track_id = match kind {
                Mp4TrackKind::Video => next_track_id("v", &mut video_index),
                Mp4TrackKind::Audio => next_track_id("a", &mut audio_index),
                Mp4TrackKind::Subtitle => next_track_id("s", &mut subtitle_index),
                Mp4TrackKind::Unknown => next_track_id("x", &mut unknown_index),
            };
            let should_parse = requested_track_id
                .map(|requested| requested == track_id)
                .unwrap_or(kind == Mp4TrackKind::Video);
            if should_parse {
                let (scale, table) = parse_trak_sample_table(trak.payload)?;
                return Some(table.into_chunk_plan(&track_id, scale, target_ms));
            }
        }
    }

    None
}

pub fn extract_chunk(
    bytes: &[u8],
    requested_track_id: Option<&str>,
    target_ms: u64,
    chunk_index: u32,
) -> Result<(ExtractedChunk, Vec<u8>), Mp4ChunkExtractError> {
    let track =
        parse_packet_track(bytes, requested_track_id).ok_or(Mp4ChunkExtractError::NoTrack)?;
    let plan = plan_track_chunks(&track.track_id, &track.packets, target_ms);
    let chunk = plan
        .chunks
        .into_iter()
        .find(|chunk| chunk.index == chunk_index)
        .ok_or(Mp4ChunkExtractError::NoChunk)?;
    let payload = extract_packet_payload(bytes, &track.packets, chunk.packet_range)?;
    let samples = packet_samples_for_range(&track.packets, chunk.packet_range)?;
    let packet_count = chunk
        .packet_range
        .end
        .saturating_sub(chunk.packet_range.start);
    let manifest = ExtractedChunk {
        track_id: track.track_id,
        chunk,
        packet_count,
        byte_count: payload.len() as u64,
        samples,
    };
    Ok((manifest, payload))
}

pub fn parse_codec_config(
    bytes: &[u8],
    requested_track_id: Option<&str>,
) -> Option<Mp4CodecConfig> {
    let mut video_index = 0_u32;
    let mut audio_index = 0_u32;
    let mut subtitle_index = 0_u32;
    let mut unknown_index = 0_u32;

    for atom in AtomIter::new(bytes) {
        if atom.kind != *b"moov" {
            continue;
        }

        for trak in AtomIter::new(atom.payload).filter(|atom| atom.kind == *b"trak") {
            let kind = parse_trak_kind(trak.payload)?;
            let track_id = match kind {
                Mp4TrackKind::Video => next_track_id("v", &mut video_index),
                Mp4TrackKind::Audio => next_track_id("a", &mut audio_index),
                Mp4TrackKind::Subtitle => next_track_id("s", &mut subtitle_index),
                Mp4TrackKind::Unknown => next_track_id("x", &mut unknown_index),
            };
            let should_parse = requested_track_id
                .map(|requested| requested == track_id)
                .unwrap_or(kind == Mp4TrackKind::Video);
            if should_parse {
                return parse_trak_codec_config(trak.payload, kind, track_id);
            }
        }
    }

    None
}

#[derive(Debug, thiserror::Error, PartialEq, Eq)]
pub enum Mp4ChunkExtractError {
    #[error("no matching MP4 packet-indexed track found")]
    NoTrack,
    #[error("no matching native chunk found")]
    NoChunk,
    #[error("{0}")]
    Packet(#[from] PacketExtractError),
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

fn parse_trak_kind(payload: &[u8]) -> Option<Mp4TrackKind> {
    let mdia = find_atom(payload, b"mdia")?;
    Some(handler_to_track_kind(
        parse_hdlr(find_atom(mdia, b"hdlr")?).as_deref(),
    ))
}

fn parse_trak_packet_index(
    payload: &[u8],
    track_index: u32,
    kind: Mp4TrackKind,
    track_id: String,
) -> Option<Mp4TrackPacketIndex> {
    let (scale, table) = parse_trak_sample_table(payload)?;
    let packets = table.into_packets(scale);

    Some(Mp4TrackPacketIndex {
        track_id,
        track_index,
        kind,
        timescale: scale,
        packets,
    })
}

fn parse_trak_sample_table(payload: &[u8]) -> Option<(TimeScale, SampleTable)> {
    let mdia = find_atom(payload, b"mdia")?;
    let timescale = parse_mdhd_timescale(find_atom(mdia, b"mdhd")?)?;
    let stbl = find_atom(find_atom(mdia, b"minf")?, b"stbl")?;
    let table = parse_sample_table(stbl)?;
    Some((
        TimeScale {
            units_per_second: timescale,
        },
        table,
    ))
}

fn parse_trak_codec_config(
    payload: &[u8],
    kind: Mp4TrackKind,
    track_id: String,
) -> Option<Mp4CodecConfig> {
    let mdia = find_atom(payload, b"mdia")?;
    let stbl = find_atom(find_atom(mdia, b"minf")?, b"stbl")?;
    let entry = parse_stsd_entry(find_atom(stbl, b"stsd")?)?;
    let sample_entry = fourcc_to_string(&entry.codec_fourcc)?;
    let codec = sample_entry_codec(&entry.codec_fourcc, kind);
    let config = codec_config_from_sample_entry(kind, &entry);

    Some(Mp4CodecConfig {
        track_id,
        track_kind: mp4_track_kind_name(kind).to_string(),
        codec,
        sample_entry,
        config_box: config
            .as_ref()
            .and_then(|config| fourcc_to_string(&config.box_type)),
        codec_string: config
            .as_ref()
            .and_then(|config| config.codec_string.clone()),
        description_hex: config.map(|config| hex_string(config.description)),
    })
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct SampleEntry<'a> {
    codec_fourcc: [u8; 4],
    payload: &'a [u8],
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct SampleTable {
    sample_sizes: Vec<u32>,
    sample_durations: Vec<u64>,
    keyframes: Vec<bool>,
    chunk_offsets: Vec<u64>,
    sample_to_chunk: Vec<SampleToChunk>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct SampleToChunk {
    first_chunk: u32,
    samples_per_chunk: u32,
}

impl SampleTable {
    fn into_packets(self, scale: TimeScale) -> Vec<PacketRef> {
        let mut packets = Vec::with_capacity(self.sample_sizes.len());
        let mut sample_index = 0_usize;
        let mut pts = 0_u64;
        let mut sample_to_chunk_index = 0_usize;

        for (chunk_idx, chunk_offset) in self.chunk_offsets.iter().copied().enumerate() {
            let chunk_number = (chunk_idx + 1) as u32;
            sample_to_chunk_index =
                self.advance_sample_to_chunk(sample_to_chunk_index, chunk_number);
            let samples_per_chunk =
                self.sample_to_chunk[sample_to_chunk_index].samples_per_chunk as usize;
            let mut offset_in_chunk = 0_u64;

            for _ in 0..samples_per_chunk {
                let Some(size) = self.sample_sizes.get(sample_index).copied() else {
                    break;
                };
                let duration = self
                    .sample_durations
                    .get(sample_index)
                    .copied()
                    .unwrap_or(0);
                packets.push(PacketRef {
                    source_offset: chunk_offset.saturating_add(offset_in_chunk),
                    size,
                    pts: TimePoint { units: pts, scale },
                    dts: TimePoint { units: pts, scale },
                    duration: TimeDelta {
                        units: duration,
                        scale,
                    },
                    keyframe: self.keyframes.get(sample_index).copied().unwrap_or(true),
                });
                sample_index += 1;
                pts = pts.saturating_add(duration);
                offset_in_chunk = offset_in_chunk.saturating_add(u64::from(size));
            }

            if sample_index >= self.sample_sizes.len() {
                break;
            }
        }

        packets
    }

    fn advance_sample_to_chunk(&self, mut index: usize, chunk_number: u32) -> usize {
        while self
            .sample_to_chunk
            .get(index + 1)
            .is_some_and(|entry| chunk_number >= entry.first_chunk)
        {
            index += 1;
        }
        index
    }

    fn into_chunk_plan(self, track_id: &str, scale: TimeScale, target_ms: u64) -> ChunkPlan {
        if self.sample_sizes.is_empty() || target_ms == 0 {
            return ChunkPlan {
                track_ids: Vec::new(),
                chunks: Vec::new(),
            };
        }

        let mut chunks = Vec::new();
        let mut sample_index = 0_usize;
        let mut pts = 0_u64;
        let mut chunk_start_sample = 0_u32;
        let mut chunk_start_ms = 0_u64;
        let mut last_end_ms = 0_u64;
        let mut key_aligned = self.keyframes.first().copied().unwrap_or(true);
        let mut sample_to_chunk_index = 0_usize;

        'chunks: for (chunk_idx, _) in self.chunk_offsets.iter().enumerate() {
            let chunk_number = (chunk_idx + 1) as u32;
            sample_to_chunk_index =
                self.advance_sample_to_chunk(sample_to_chunk_index, chunk_number);
            let samples_per_chunk =
                self.sample_to_chunk[sample_to_chunk_index].samples_per_chunk as usize;

            for _ in 0..samples_per_chunk {
                if sample_index >= self.sample_sizes.len() {
                    break 'chunks;
                }

                let packet_start_ms = scale.to_millis(pts);
                let duration = self
                    .sample_durations
                    .get(sample_index)
                    .copied()
                    .unwrap_or(0);
                let packet_end_ms = scale.to_millis(pts.saturating_add(duration));
                let packet_is_keyframe = self.keyframes.get(sample_index).copied().unwrap_or(true);
                let should_cut = sample_index as u32 > chunk_start_sample
                    && packet_is_keyframe
                    && packet_start_ms.saturating_sub(chunk_start_ms) >= target_ms;
                if should_cut {
                    chunks.push(NativeChunk {
                        index: chunks.len() as u32,
                        start: TimePoint::millis(chunk_start_ms),
                        duration: TimeDelta::millis(last_end_ms.saturating_sub(chunk_start_ms)),
                        packet_range: PacketRange {
                            start: chunk_start_sample,
                            end: sample_index as u32,
                        },
                        key_aligned,
                    });
                    chunk_start_sample = sample_index as u32;
                    chunk_start_ms = packet_start_ms;
                    key_aligned = packet_is_keyframe;
                }

                pts = pts.saturating_add(duration);
                last_end_ms = packet_end_ms;
                sample_index += 1;
            }
        }

        chunks.push(NativeChunk {
            index: chunks.len() as u32,
            start: TimePoint::millis(chunk_start_ms),
            duration: TimeDelta::millis(last_end_ms.saturating_sub(chunk_start_ms)),
            packet_range: PacketRange {
                start: chunk_start_sample,
                end: sample_index as u32,
            },
            key_aligned,
        });

        ChunkPlan {
            track_ids: vec![track_id.to_string()],
            chunks,
        }
    }
}

fn parse_sample_table(stbl: &[u8]) -> Option<SampleTable> {
    let sample_sizes = find_atom(stbl, b"stsz").and_then(parse_stsz)?;
    let sample_count = sample_sizes.len();
    let sample_durations = find_atom(stbl, b"stts")
        .and_then(|payload| parse_stts(payload, sample_count))
        .unwrap_or_else(|| vec![0; sample_count]);
    let keyframes = find_atom(stbl, b"stss")
        .and_then(|payload| parse_stss(payload, sample_count))
        .unwrap_or_else(|| vec![true; sample_count]);
    let chunk_offsets = find_atom(stbl, b"stco")
        .and_then(parse_stco)
        .or_else(|| find_atom(stbl, b"co64").and_then(parse_co64))?;
    let sample_to_chunk = find_atom(stbl, b"stsc").and_then(parse_stsc)?;

    Some(SampleTable {
        sample_sizes,
        sample_durations,
        keyframes,
        chunk_offsets,
        sample_to_chunk,
    })
}

fn next_track_id(prefix: &str, counter: &mut u32) -> String {
    let id = format!("{prefix}{counter}");
    *counter += 1;
    id
}

fn find_atom<'a>(payload: &'a [u8], kind: &[u8; 4]) -> Option<&'a [u8]> {
    AtomIter::new(payload)
        .find(|atom| atom.kind == *kind)
        .map(|atom| atom.payload)
}

fn parse_mdhd_timescale(payload: &[u8]) -> Option<u32> {
    let version = *payload.first()?;
    if version == 1 {
        if payload.len() < 24 {
            return None;
        }
        read_u32(&payload[20..24])
    } else {
        if payload.len() < 16 {
            return None;
        }
        read_u32(&payload[12..16])
    }
}

fn parse_stts(payload: &[u8], sample_count: usize) -> Option<Vec<u64>> {
    if payload.len() < 8 {
        return None;
    }
    let entry_count = read_u32(&payload[4..8])? as usize;
    let mut offset = 8;
    let mut durations = Vec::with_capacity(sample_count);
    for _ in 0..entry_count {
        if offset + 8 > payload.len() {
            return None;
        }
        let count = read_u32(&payload[offset..offset + 4])? as usize;
        let delta = u64::from(read_u32(&payload[offset + 4..offset + 8])?);
        durations.extend(std::iter::repeat(delta).take(count));
        offset += 8;
    }
    durations.truncate(sample_count);
    if durations.len() == sample_count {
        Some(durations)
    } else {
        None
    }
}

fn parse_stss(payload: &[u8], sample_count: usize) -> Option<Vec<bool>> {
    if payload.len() < 8 {
        return None;
    }
    let entry_count = read_u32(&payload[4..8])? as usize;
    let mut out = vec![false; sample_count];
    let mut offset = 8;
    for _ in 0..entry_count {
        if offset + 4 > payload.len() {
            return None;
        }
        let sample_number = read_u32(&payload[offset..offset + 4])? as usize;
        if (1..=sample_count).contains(&sample_number) {
            out[sample_number - 1] = true;
        }
        offset += 4;
    }
    Some(out)
}

fn parse_stsc(payload: &[u8]) -> Option<Vec<SampleToChunk>> {
    if payload.len() < 8 {
        return None;
    }
    let entry_count = read_u32(&payload[4..8])? as usize;
    let mut offset = 8;
    let mut out = Vec::with_capacity(entry_count);
    for _ in 0..entry_count {
        if offset + 12 > payload.len() {
            return None;
        }
        out.push(SampleToChunk {
            first_chunk: read_u32(&payload[offset..offset + 4])?,
            samples_per_chunk: read_u32(&payload[offset + 4..offset + 8])?,
        });
        offset += 12;
    }
    if out.is_empty() {
        None
    } else {
        Some(out)
    }
}

fn parse_stsz(payload: &[u8]) -> Option<Vec<u32>> {
    if payload.len() < 12 {
        return None;
    }
    let fixed_size = read_u32(&payload[4..8])?;
    let sample_count = read_u32(&payload[8..12])? as usize;
    if fixed_size != 0 {
        return Some(vec![fixed_size; sample_count]);
    }
    let mut offset = 12;
    let mut out = Vec::with_capacity(sample_count);
    for _ in 0..sample_count {
        if offset + 4 > payload.len() {
            return None;
        }
        out.push(read_u32(&payload[offset..offset + 4])?);
        offset += 4;
    }
    Some(out)
}

fn parse_stco(payload: &[u8]) -> Option<Vec<u64>> {
    if payload.len() < 8 {
        return None;
    }
    let entry_count = read_u32(&payload[4..8])? as usize;
    let mut offset = 8;
    let mut out = Vec::with_capacity(entry_count);
    for _ in 0..entry_count {
        if offset + 4 > payload.len() {
            return None;
        }
        out.push(u64::from(read_u32(&payload[offset..offset + 4])?));
        offset += 4;
    }
    Some(out)
}

fn parse_co64(payload: &[u8]) -> Option<Vec<u64>> {
    if payload.len() < 8 {
        return None;
    }
    let entry_count = read_u32(&payload[4..8])? as usize;
    let mut offset = 8;
    let mut out = Vec::with_capacity(entry_count);
    for _ in 0..entry_count {
        if offset + 8 > payload.len() {
            return None;
        }
        out.push(read_u64(&payload[offset..offset + 8])?);
        offset += 8;
    }
    Some(out)
}

fn parse_stsd(payload: &[u8]) -> Option<SampleEntryInfo> {
    let entry = parse_stsd_entry(payload)?;
    Some(parse_sample_entry(entry.codec_fourcc, entry.payload))
}

fn parse_stsd_entry(payload: &[u8]) -> Option<SampleEntry<'_>> {
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
    Some(SampleEntry {
        codec_fourcc,
        payload: entry_payload,
    })
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct SampleEntryCodecConfig {
    box_type: [u8; 4],
    codec_string: Option<String>,
    description: Vec<u8>,
}

fn codec_config_from_sample_entry(
    kind: Mp4TrackKind,
    entry: &SampleEntry<'_>,
) -> Option<SampleEntryCodecConfig> {
    let child_boxes = sample_entry_child_boxes(kind, entry.codec_fourcc, entry.payload)?;
    for atom in AtomIter::new(child_boxes) {
        match atom.kind {
            kind if kind == *b"avcC" => {
                return Some(SampleEntryCodecConfig {
                    box_type: atom.kind,
                    codec_string: avc_codec_string(atom.payload),
                    description: atom.payload.to_vec(),
                });
            }
            kind if kind == *b"hvcC" => {
                return Some(SampleEntryCodecConfig {
                    box_type: atom.kind,
                    codec_string: hevc_codec_string(atom.payload, &entry.codec_fourcc),
                    description: atom.payload.to_vec(),
                });
            }
            kind if kind == *b"esds" => {
                let asc = audio_specific_config_from_esds(atom.payload);
                return Some(SampleEntryCodecConfig {
                    box_type: atom.kind,
                    codec_string: asc
                        .as_ref()
                        .map(|config| format!("mp4a.40.{}", config.audio_object_type)),
                    description: asc.map_or_else(|| atom.payload.to_vec(), |config| config.bytes),
                });
            }
            _ => {}
        }
    }
    None
}

fn sample_entry_child_boxes(
    kind: Mp4TrackKind,
    codec_fourcc: [u8; 4],
    payload: &[u8],
) -> Option<&[u8]> {
    let offset = if is_video_sample_entry(&codec_fourcc) {
        78
    } else if is_audio_sample_entry(&codec_fourcc) {
        28
    } else if kind == Mp4TrackKind::Subtitle {
        8
    } else {
        return None;
    };
    payload.get(offset..)
}

fn avc_codec_string(payload: &[u8]) -> Option<String> {
    if payload.len() < 4 {
        return None;
    }
    Some(format!(
        "avc1.{:02X}{:02X}{:02X}",
        payload[1], payload[2], payload[3]
    ))
}

fn hevc_codec_string(payload: &[u8], sample_entry: &[u8; 4]) -> Option<String> {
    if payload.len() < 13 {
        return None;
    }
    let prefix = if sample_entry == b"hev1" {
        "hev1"
    } else {
        "hvc1"
    };
    let profile_space = payload[1] >> 6;
    let profile_idc = payload[1] & 0x1f;
    let compatibility = read_u32(&payload[2..6])?;
    let tier = if payload[12] & 0x80 != 0 { "H" } else { "L" };
    let level = payload[12] & 0x7f;
    let space = match profile_space {
        1 => "A",
        2 => "B",
        3 => "C",
        _ => "",
    };
    Some(format!(
        "{prefix}.{space}{profile_idc}.{:X}.{tier}{level}",
        compatibility.reverse_bits()
    ))
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct AudioSpecificConfig {
    audio_object_type: u8,
    bytes: Vec<u8>,
}

fn audio_specific_config_from_esds(payload: &[u8]) -> Option<AudioSpecificConfig> {
    let descriptors = payload.get(4..).unwrap_or(payload);
    let config = find_descriptor_payload(descriptors, 0x05)?;
    let first = *config.first()?;
    let second = *config.get(1)?;
    let audio_object_type = first >> 3;
    let _frequency_index = ((first & 0x07) << 1) | (second >> 7);
    Some(AudioSpecificConfig {
        audio_object_type,
        bytes: config.to_vec(),
    })
}

fn find_descriptor_payload(payload: &[u8], tag: u8) -> Option<&[u8]> {
    let mut offset = 0;
    while offset + 2 <= payload.len() {
        let descriptor_tag = payload[offset];
        offset += 1;
        let (size, size_len) = read_descriptor_size(&payload[offset..])?;
        offset += size_len;
        let end = offset.checked_add(size)?;
        if end > payload.len() {
            return None;
        }
        let body = &payload[offset..end];
        if descriptor_tag == tag {
            return Some(body);
        }
        let nested_body = match descriptor_tag {
            0x03 => body.get(3..).unwrap_or_default(),
            0x04 => body.get(13..).unwrap_or_default(),
            _ => body,
        };
        if let Some(nested) = find_descriptor_payload(nested_body, tag) {
            return Some(nested);
        }
        offset = end;
    }
    None
}

fn read_descriptor_size(payload: &[u8]) -> Option<(usize, usize)> {
    let mut size = 0_usize;
    for (idx, byte) in payload.iter().take(4).enumerate() {
        size = (size << 7) | usize::from(byte & 0x7f);
        if byte & 0x80 == 0 {
            return Some((size, idx + 1));
        }
    }
    None
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

fn mp4_track_kind_name(kind: Mp4TrackKind) -> &'static str {
    match kind {
        Mp4TrackKind::Video => "video",
        Mp4TrackKind::Audio => "audio",
        Mp4TrackKind::Subtitle => "subtitle",
        Mp4TrackKind::Unknown => "unknown",
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

fn hex_string(bytes: Vec<u8>) -> String {
    const HEX: &[u8; 16] = b"0123456789abcdef";
    let mut out = String::with_capacity(bytes.len() * 2);
    for byte in bytes {
        out.push(HEX[(byte >> 4) as usize] as char);
        out.push(HEX[(byte & 0x0f) as usize] as char);
    }
    out
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

    #[test]
    fn parses_mp4_packet_index_from_sample_tables() {
        let mut data = ftyp();
        data.extend_from_slice(&atom(b"free", &[0_u8; 128]));
        data.extend_from_slice(&atom(
            b"moov",
            &trak_with_samples(
                b"vide",
                b"avc1",
                &[100, 110, 120, 130, 140],
                &[40, 40, 40, 40, 40],
                &[1, 3, 5],
                &[200, 520],
                &[(1, 2), (2, 3)],
            ),
        ));

        let index = parse_packet_index(&data);
        assert_eq!(index.tracks.len(), 1);
        let track = &index.tracks[0];
        assert_eq!(track.track_id, "v0");
        assert_eq!(track.kind, Mp4TrackKind::Video);
        assert_eq!(track.packets.len(), 5);
        assert_eq!(track.packets[0].source_offset, 200);
        assert_eq!(track.packets[1].source_offset, 300);
        assert_eq!(track.packets[2].source_offset, 520);
        assert_eq!(track.packets[3].source_offset, 640);
        assert_eq!(track.packets[4].source_offset, 770);
        assert_eq!(track.packets[0].pts.units, 0);
        assert_eq!(track.packets[2].pts.units, 80);
        assert!(track.packets[0].keyframe);
        assert!(!track.packets[1].keyframe);
        assert!(track.packets[2].keyframe);
    }

    #[test]
    fn plans_mp4_chunks_directly_from_sample_tables() {
        let mut data = ftyp();
        data.extend_from_slice(&atom(
            b"moov",
            &trak_with_samples(
                b"vide",
                b"avc1",
                &[100, 110, 120, 130, 140],
                &[1000, 1000, 1000, 1000, 1000],
                &[1, 3, 5],
                &[200, 520],
                &[(1, 2), (2, 3)],
            ),
        ));

        let plan = parse_chunk_plan(&data, None, 2_000).unwrap();
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
    fn extracts_mp4_chunk_payload_from_sample_offsets() {
        let mut data = ftyp();
        data.extend_from_slice(&atom(
            b"moov",
            &trak_with_samples(
                b"vide",
                b"avc1",
                &[4, 3, 2],
                &[1000, 1000, 1000],
                &[1, 3],
                &[700, 900],
                &[(1, 2), (2, 1)],
            ),
        ));
        data.resize(1_000, 0);
        data[700..704].copy_from_slice(b"aaaa");
        data[704..707].copy_from_slice(b"bbb");
        data[900..902].copy_from_slice(b"cc");

        let (manifest, payload) = extract_chunk(&data, None, 2_000, 0).unwrap();
        assert_eq!(manifest.track_id, "v0");
        assert_eq!(manifest.packet_count, 2);
        assert_eq!(manifest.byte_count, 7);
        assert_eq!(manifest.samples.len(), 2);
        assert_eq!(manifest.samples[0].payload_offset, 0);
        assert_eq!(manifest.samples[0].byte_count, 4);
        assert_eq!(manifest.samples[1].payload_offset, 4);
        assert_eq!(manifest.samples[1].byte_count, 3);
        assert_eq!(payload, b"aaaabbb");
    }

    #[test]
    fn extracts_h264_codec_config_from_avcc() {
        let mut data = ftyp();
        let avcc = atom(b"avcC", &[1, 0x64, 0x00, 0x1f, 0xff, 0xe1, 0, 0]);
        data.extend_from_slice(&atom(
            b"moov",
            &trak_with_sample_entry_payload(
                b"vide",
                b"avc1",
                video_sample_entry_payload(Some((1920, 1080)), &avcc),
                3000,
                1000,
            ),
        ));

        let config = parse_codec_config(&data, None).unwrap();
        assert_eq!(config.track_id, "v0");
        assert_eq!(config.track_kind, "video");
        assert_eq!(config.codec, "h264");
        assert_eq!(config.sample_entry, "avc1");
        assert_eq!(config.config_box.as_deref(), Some("avcC"));
        assert_eq!(config.codec_string.as_deref(), Some("avc1.64001F"));
        assert_eq!(config.description_hex.as_deref(), Some("0164001fffe10000"));
    }

    #[test]
    fn extracts_aac_codec_config_from_esds() {
        let mut data = ftyp();
        let esds = atom(
            b"esds",
            &[
                [0, 0, 0, 0].as_slice(),
                &es_descriptor(&decoder_config_descriptor(&decoder_specific_descriptor(&[
                    0x12, 0x10,
                ]))),
            ]
            .concat(),
        );
        data.extend_from_slice(&atom(
            b"moov",
            &trak_with_sample_entry_payload(
                b"soun",
                b"mp4a",
                audio_sample_entry_payload(2, 48000, &esds),
                3000,
                1000,
            ),
        ));

        let config = parse_codec_config(&data, Some("a0")).unwrap();
        assert_eq!(config.track_id, "a0");
        assert_eq!(config.track_kind, "audio");
        assert_eq!(config.codec, "aac");
        assert_eq!(config.sample_entry, "mp4a");
        assert_eq!(config.config_box.as_deref(), Some("esds"));
        assert_eq!(config.codec_string.as_deref(), Some("mp4a.40.2"));
        assert_eq!(config.description_hex.as_deref(), Some("1210"));
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

    fn trak_with_samples(
        handler: &[u8; 4],
        sample_entry: &[u8; 4],
        sample_sizes: &[u32],
        sample_durations: &[u32],
        sync_samples: &[u32],
        chunk_offsets: &[u32],
        sample_to_chunk: &[(u32, u32)],
    ) -> Vec<u8> {
        atom(
            b"trak",
            &[
                tkhd(Some((1920, 1080))),
                atom(
                    b"mdia",
                    &[
                        mdhd(1000, sample_durations.iter().sum()),
                        hdlr(handler),
                        atom(
                            b"minf",
                            &atom(
                                b"stbl",
                                &[
                                    stsd(sample_entry, Some((1920, 1080)), None),
                                    stts(sample_durations),
                                    stss(sync_samples),
                                    stsc(sample_to_chunk),
                                    stsz(sample_sizes),
                                    stco(chunk_offsets),
                                ]
                                .concat(),
                            ),
                        ),
                    ]
                    .concat(),
                ),
            ]
            .concat(),
        )
    }

    fn trak_with_sample_entry_payload(
        handler: &[u8; 4],
        sample_entry: &[u8; 4],
        sample_entry_payload: Vec<u8>,
        duration: u32,
        timescale: u32,
    ) -> Vec<u8> {
        atom(
            b"trak",
            &[
                tkhd(Some((1920, 1080))),
                atom(
                    b"mdia",
                    &[
                        mdhd(timescale, duration),
                        hdlr(handler),
                        atom(
                            b"minf",
                            &atom(
                                b"stbl",
                                &stsd_with_entry_payload(sample_entry, &sample_entry_payload),
                            ),
                        ),
                    ]
                    .concat(),
                ),
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

    fn stsd_with_entry_payload(sample_entry: &[u8; 4], entry_payload: &[u8]) -> Vec<u8> {
        let entry = atom(sample_entry, entry_payload);
        let mut payload = Vec::new();
        payload.extend_from_slice(&[0, 0, 0, 0]);
        payload.extend_from_slice(&1_u32.to_be_bytes());
        payload.extend_from_slice(&entry);
        atom(b"stsd", &payload)
    }

    fn stts(sample_durations: &[u32]) -> Vec<u8> {
        let mut payload = Vec::new();
        payload.extend_from_slice(&[0, 0, 0, 0]);
        payload.extend_from_slice(&(sample_durations.len() as u32).to_be_bytes());
        for duration in sample_durations {
            payload.extend_from_slice(&1_u32.to_be_bytes());
            payload.extend_from_slice(&duration.to_be_bytes());
        }
        atom(b"stts", &payload)
    }

    fn stss(sync_samples: &[u32]) -> Vec<u8> {
        let mut payload = Vec::new();
        payload.extend_from_slice(&[0, 0, 0, 0]);
        payload.extend_from_slice(&(sync_samples.len() as u32).to_be_bytes());
        for sample in sync_samples {
            payload.extend_from_slice(&sample.to_be_bytes());
        }
        atom(b"stss", &payload)
    }

    fn stsc(entries: &[(u32, u32)]) -> Vec<u8> {
        let mut payload = Vec::new();
        payload.extend_from_slice(&[0, 0, 0, 0]);
        payload.extend_from_slice(&(entries.len() as u32).to_be_bytes());
        for (first_chunk, samples_per_chunk) in entries {
            payload.extend_from_slice(&first_chunk.to_be_bytes());
            payload.extend_from_slice(&samples_per_chunk.to_be_bytes());
            payload.extend_from_slice(&1_u32.to_be_bytes());
        }
        atom(b"stsc", &payload)
    }

    fn stsz(sample_sizes: &[u32]) -> Vec<u8> {
        let mut payload = Vec::new();
        payload.extend_from_slice(&[0, 0, 0, 0]);
        payload.extend_from_slice(&0_u32.to_be_bytes());
        payload.extend_from_slice(&(sample_sizes.len() as u32).to_be_bytes());
        for size in sample_sizes {
            payload.extend_from_slice(&size.to_be_bytes());
        }
        atom(b"stsz", &payload)
    }

    fn stco(chunk_offsets: &[u32]) -> Vec<u8> {
        let mut payload = Vec::new();
        payload.extend_from_slice(&[0, 0, 0, 0]);
        payload.extend_from_slice(&(chunk_offsets.len() as u32).to_be_bytes());
        for offset in chunk_offsets {
            payload.extend_from_slice(&offset.to_be_bytes());
        }
        atom(b"stco", &payload)
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

    fn video_sample_entry_payload(size: Option<(u32, u32)>, child_boxes: &[u8]) -> Vec<u8> {
        let mut payload = vec![0_u8; 78];
        if let Some((w, h)) = size {
            payload[24..26].copy_from_slice(&(w as u16).to_be_bytes());
            payload[26..28].copy_from_slice(&(h as u16).to_be_bytes());
        }
        payload.extend_from_slice(child_boxes);
        payload
    }

    fn audio_sample_entry_payload(channels: u16, sample_rate: u32, child_boxes: &[u8]) -> Vec<u8> {
        let mut payload = vec![0_u8; 28];
        payload[16..18].copy_from_slice(&channels.to_be_bytes());
        payload[24..28].copy_from_slice(&(sample_rate << 16).to_be_bytes());
        payload.extend_from_slice(child_boxes);
        payload
    }

    fn es_descriptor(nested: &[u8]) -> Vec<u8> {
        descriptor(0x03, &[[0, 1, 0].as_slice(), nested].concat())
    }

    fn decoder_config_descriptor(nested: &[u8]) -> Vec<u8> {
        descriptor(
            0x04,
            &[
                [0x40, 0x15].as_slice(),
                &[0, 0, 0],
                &128_000_u32.to_be_bytes(),
                &128_000_u32.to_be_bytes(),
                nested,
            ]
            .concat(),
        )
    }

    fn decoder_specific_descriptor(config: &[u8]) -> Vec<u8> {
        descriptor(0x05, config)
    }

    fn descriptor(tag: u8, payload: &[u8]) -> Vec<u8> {
        assert!(payload.len() < 0x80);
        let mut out = Vec::with_capacity(payload.len() + 2);
        out.push(tag);
        out.push(payload.len() as u8);
        out.extend_from_slice(payload);
        out
    }

    fn atom(kind: &[u8; 4], payload: &[u8]) -> Vec<u8> {
        let mut out = Vec::with_capacity(payload.len() + 8);
        out.extend_from_slice(&((payload.len() + 8) as u32).to_be_bytes());
        out.extend_from_slice(kind);
        out.extend_from_slice(payload);
        out
    }
}
