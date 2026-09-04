mod ebml;

use thiserror::Error;

use crate::codec::pixel_format::{
    pixel_format_from_avc_decoder_config, pixel_format_from_hevc_decoder_config,
};
use crate::packet::{
    ChunkPlan, ExtractedChunk, NativeChunk, PacketExtractError, PacketRange, PacketRef, TimeDelta,
    TimePoint, TimeRounding, TimeScale, extract_packet_payload, packet_samples_for_range,
};

use ebml::{
    ElementIter, count_children, find_first_child, find_first_child_element, read_float,
    read_signed_vint, read_string, read_uint, read_vint_size,
};

#[derive(Debug, Clone, PartialEq)]
pub struct MatroskaBasicMetadata {
    pub duration_ms: Option<u64>,
    pub tracks: Vec<MatroskaTrack>,
    pub chapters: Vec<MatroskaChapter>,
    pub attachment_count: u32,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MatroskaChapter {
    pub id: String,
    pub start_ms: u64,
    pub end_ms: Option<u64>,
    pub title: Option<String>,
    pub language: Option<String>,
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
    pub pixel_format: Option<String>,
    pub channels: Option<u32>,
    pub sample_rate: Option<u32>,
    pub atmos: bool,
    pub object_audio_candidate: bool,
    pub default_duration_ns: Option<u64>,
    pub codec_private: Option<Vec<u8>>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MatroskaTrackKind {
    Video,
    Audio,
    Subtitle,
    Unknown,
}

#[derive(Debug, Error, PartialEq, Eq)]
pub enum MatroskaChunkExtractError {
    #[error("no matching Matroska track found")]
    NoTrack,
    #[error("no matching Matroska chunk found")]
    NoChunk,
    #[error("Matroska packet payload extraction failed: {0}")]
    Packet(#[from] PacketExtractError),
}

pub fn looks_like_ebml(head: &[u8]) -> bool {
    head.len() >= 4 && head[0..4] == [0x1a, 0x45, 0xdf, 0xa3]
}

pub fn extract_chunk(
    bytes: &[u8],
    requested_track_id: Option<&str>,
    target_ms: u64,
    chunk_index: u32,
) -> Result<(ExtractedChunk, Vec<u8>), MatroskaChunkExtractError> {
    extract_window(bytes, requested_track_id, target_ms, chunk_index, 1)?
        .into_iter()
        .next()
        .ok_or(MatroskaChunkExtractError::NoChunk)
}

#[cfg(test)]
fn extract_time_range(
    bytes: &[u8],
    requested_track_id: Option<&str>,
    range_start_ms: u64,
    range_end_ms: u64,
    output_index: u32,
) -> Result<(ExtractedChunk, Vec<u8>), MatroskaChunkExtractError> {
    let meta = parse_basic_metadata(bytes);
    let selected = select_chunk_track(&meta.tracks, requested_track_id)
        .ok_or(MatroskaChunkExtractError::NoTrack)?;
    let window_start_ms = range_start_ms.saturating_sub(1_000);
    let mut packets =
        parse_packet_tracks_in_time_window(bytes, &[&selected.id], window_start_ms, range_end_ms)
            .and_then(|mut tracks| tracks.pop())
            .ok_or(MatroskaChunkExtractError::NoChunk)?
            .packets
            .into_iter()
            .filter(|packet| {
                let start = packet.pts.as_millis();
                let end = start.saturating_add(packet.duration.as_millis().max(1));
                end > range_start_ms && start < range_end_ms
            })
            .collect::<Vec<_>>();
    if packets.is_empty() {
        return Err(MatroskaChunkExtractError::NoChunk);
    }
    let last_duration =
        infer_last_packet_duration(&packets).unwrap_or_else(|| TimeDelta::millis(0));
    if let Some(last) = packets.last_mut() {
        last.duration = last_duration;
    }
    let first_packet_ms = packets
        .first()
        .map(|packet| packet.pts.as_millis())
        .unwrap_or(range_start_ms);
    let last_end_ms = packets
        .last()
        .map(|packet| {
            packet
                .pts
                .as_millis()
                .saturating_add(packet.duration.as_millis())
        })
        .unwrap_or(range_end_ms);
    let chunk = NativeChunk {
        index: output_index,
        start: TimePoint::millis(first_packet_ms),
        duration: TimeDelta::millis(last_end_ms.saturating_sub(first_packet_ms).max(1)),
        packet_range: PacketRange {
            start: 0,
            end: packets.len() as u32,
        },
        key_aligned: packets
            .first()
            .map(|packet| packet.keyframe)
            .unwrap_or(false),
    };
    extract_packets_as_chunk(bytes, &selected.id, chunk, &packets)
}

pub fn extract_window(
    bytes: &[u8],
    requested_track_id: Option<&str>,
    target_ms: u64,
    start_chunk: u32,
    chunk_count: u32,
) -> Result<Vec<(ExtractedChunk, Vec<u8>)>, MatroskaChunkExtractError> {
    let meta = parse_basic_metadata(bytes);
    let selected = select_chunk_track(&meta.tracks, requested_track_id)
        .ok_or(MatroskaChunkExtractError::NoTrack)?;
    let segment_element =
        find_first_child_element(bytes, 0x1853_8067).ok_or(MatroskaChunkExtractError::NoTrack)?;
    let segment = segment_element.payload;
    let segment_base_offset = segment_element.payload_offset;
    let timecode_scale = parse_segment_timecode_scale(segment);
    let end_chunk = start_chunk.saturating_add(chunk_count);
    let mut out = Vec::new();
    let mut current_index = 0_u32;
    let mut chunk_start_ms = 0_u64;
    let mut last_seen_ms = 0_u64;
    let mut key_aligned = true;
    let mut saw_block = false;
    let mut packets: Vec<PacketRef> = Vec::new();
    let mut absolute_packet_index = 0_u32;

    for cluster in ElementIter::new(segment).filter(|element| element.id == 0x1f43_b675) {
        let cluster_timecode = parse_cluster_timecode(cluster.payload).unwrap_or(0);
        for block in ClusterBlockIter::new_with_base(
            cluster.payload,
            segment_base_offset + cluster.payload_offset,
        ) {
            if block.track_number != selected.number {
                continue;
            }

            let timestamp_ms = matroska_timecode_to_ms(
                cluster_timecode.saturating_add_signed(i64::from(block.relative_timecode)),
                timecode_scale,
            );
            if !saw_block {
                chunk_start_ms = timestamp_ms;
                key_aligned = block.keyframe;
                saw_block = true;
            }

            let should_cut = absolute_packet_index > 0
                && block.keyframe
                && timestamp_ms.saturating_sub(chunk_start_ms) >= target_ms;
            if should_cut {
                let chunk_end_ms = timestamp_ms;
                if current_index >= start_chunk && current_index < end_chunk {
                    let chunk = NativeChunk {
                        index: current_index,
                        start: TimePoint::millis(chunk_start_ms),
                        duration: TimeDelta::millis(chunk_end_ms.saturating_sub(chunk_start_ms)),
                        packet_range: PacketRange {
                            start: 0,
                            end: packets.len() as u32,
                        },
                        key_aligned,
                    };
                    out.push(extract_packets_as_chunk(
                        bytes,
                        &selected.id,
                        chunk,
                        &packets,
                    )?);
                }
                current_index = current_index.saturating_add(1);
                if current_index >= end_chunk {
                    return Ok(out);
                }
                packets.clear();
                chunk_start_ms = timestamp_ms;
                key_aligned = block.keyframe;
            }

            let collect = current_index >= start_chunk && current_index < end_chunk;
            if collect {
                push_block_packets(&mut packets, &block, timestamp_ms, selected.frame_duration);
            }
            last_seen_ms = timestamp_ms;
            absolute_packet_index = absolute_packet_index.saturating_add(1);
        }
    }

    if saw_block && current_index >= start_chunk && current_index < end_chunk {
        let last_duration =
            infer_last_packet_duration(&packets).unwrap_or_else(|| TimeDelta::millis(0));
        if let Some(last) = packets.last_mut() {
            last.duration = last_duration;
        }
        let chunk = NativeChunk {
            index: current_index,
            start: TimePoint::millis(chunk_start_ms),
            duration: TimeDelta::millis(
                packets
                    .last()
                    .map(|packet| {
                        packet
                            .pts
                            .as_millis()
                            .saturating_add(packet.duration.as_millis())
                    })
                    .unwrap_or(last_seen_ms)
                    .saturating_sub(chunk_start_ms),
            ),
            packet_range: PacketRange {
                start: 0,
                end: packets.len() as u32,
            },
            key_aligned,
        };
        out.push(extract_packets_as_chunk(
            bytes,
            &selected.id,
            chunk,
            &packets,
        )?);
    }

    Ok(out)
}

fn extract_packets_as_chunk(
    bytes: &[u8],
    track_id: &str,
    chunk: NativeChunk,
    packets: &[PacketRef],
) -> Result<(ExtractedChunk, Vec<u8>), MatroskaChunkExtractError> {
    let payload = extract_packet_payload(bytes, packets, chunk.packet_range)?;
    let samples = packet_samples_for_range(packets, chunk.packet_range)?;
    let packet_count = chunk
        .packet_range
        .end
        .saturating_sub(chunk.packet_range.start);
    Ok((
        ExtractedChunk {
            track_id: track_id.to_string(),
            chunk,
            packet_count,
            byte_count: payload.len() as u64,
            samples,
        },
        payload,
    ))
}

pub fn parse_packet_track(
    bytes: &[u8],
    requested_track_id: Option<&str>,
) -> Option<MatroskaPacketTrack> {
    let meta = parse_basic_metadata(bytes);
    let selected = select_chunk_track(&meta.tracks, requested_track_id)?;
    let segment_element = find_first_child_element(bytes, 0x1853_8067)?;
    let segment = segment_element.payload;
    let timecode_scale = parse_segment_timecode_scale(segment);
    let packets = parse_track_packets(
        segment,
        segment_element.payload_offset,
        selected.number,
        selected.kind,
        selected.frame_duration,
        timecode_scale,
    )?;
    Some(MatroskaPacketTrack {
        id: selected.id,
        packets,
    })
}

pub fn parse_basic_metadata(bytes: &[u8]) -> MatroskaBasicMetadata {
    let mut meta = MatroskaBasicMetadata {
        duration_ms: None,
        tracks: Vec::new(),
        chapters: Vec::new(),
        attachment_count: 0,
    };

    let Some(segment) = find_first_child(bytes, 0x1853_8067) else {
        return meta;
    };

    for child in ElementIter::new(segment) {
        match child.id {
            0x1549_a966 => parse_info(child.payload, &mut meta),
            0x1654_ae6b => parse_tracks(child.payload, &mut meta),
            0x1043_a770 => parse_chapters(child.payload, &mut meta),
            0x1941_a469 => meta.attachment_count = count_children(child.payload, 0x61a7),
            // Cluster is the media-data boundary for normal Matroska files. Probing should not
            // scan packet payloads once metadata sections have been collected.
            0x1f43_b675 if meta.duration_ms.is_some() && !meta.tracks.is_empty() => break,
            _ => {}
        }
    }

    meta
}

pub fn parse_chunk_plan(
    bytes: &[u8],
    requested_track_id: Option<&str>,
    target_ms: u64,
) -> Option<ChunkPlan> {
    if target_ms == 0 {
        return Some(ChunkPlan {
            track_ids: Vec::new(),
            chunks: Vec::new(),
        });
    }

    let meta = parse_basic_metadata(bytes);
    let selected = select_chunk_track(&meta.tracks, requested_track_id)?;
    let segment = find_first_child(bytes, 0x1853_8067)?;
    let timecode_scale = parse_segment_timecode_scale(segment);
    if let Some(mut plan) = parse_cue_chunk_plan(segment, &selected, timecode_scale, target_ms) {
        if let Some(last) = plan.chunks.last_mut()
            && let Some(duration_ms) = meta.duration_ms
        {
            last.duration =
                TimeDelta::millis(duration_ms.saturating_sub(last.start.as_millis()).max(1));
        }
        return Some(plan);
    }
    if segment.len() > 512 * 1024 * 1024 {
        return None;
    }

    let mut chunks = Vec::new();
    let mut block_index = 0_u32;
    let mut chunk_start_block = 0_u32;
    let mut chunk_start_ms = 0_u64;
    let mut last_seen_ms = 0_u64;
    let mut key_aligned = true;
    let mut saw_block = false;

    for cluster in ElementIter::new(segment).filter(|element| element.id == 0x1f43_b675) {
        let cluster_timecode = parse_cluster_timecode(cluster.payload).unwrap_or(0);
        for block in ClusterBlockIter::new(cluster.payload) {
            if block.track_number != selected.number {
                continue;
            }

            let timestamp_ms = matroska_timecode_to_ms(
                cluster_timecode.saturating_add_signed(i64::from(block.relative_timecode)),
                timecode_scale,
            );
            if !saw_block {
                chunk_start_ms = timestamp_ms;
                key_aligned = block.keyframe;
                saw_block = true;
            }

            let should_cut = block_index > chunk_start_block
                && block.keyframe
                && timestamp_ms.saturating_sub(chunk_start_ms) >= target_ms;
            if should_cut {
                chunks.push(NativeChunk {
                    index: chunks.len() as u32,
                    start: TimePoint::millis(chunk_start_ms),
                    duration: TimeDelta::millis(timestamp_ms.saturating_sub(chunk_start_ms)),
                    packet_range: PacketRange {
                        start: chunk_start_block,
                        end: block_index,
                    },
                    key_aligned,
                });
                chunk_start_block = block_index;
                chunk_start_ms = timestamp_ms;
                key_aligned = block.keyframe;
            }

            last_seen_ms = timestamp_ms;
            block_index += 1;
        }
    }

    if saw_block {
        chunks.push(NativeChunk {
            index: chunks.len() as u32,
            start: TimePoint::millis(chunk_start_ms),
            duration: TimeDelta::millis(last_seen_ms.saturating_sub(chunk_start_ms)),
            packet_range: PacketRange {
                start: chunk_start_block,
                end: block_index,
            },
            key_aligned,
        });
    }

    Some(ChunkPlan {
        track_ids: vec![selected.id],
        chunks,
    })
}

fn parse_cue_chunk_plan(
    segment: &[u8],
    selected: &SelectedChunkTrack,
    timecode_scale: u64,
    target_ms: u64,
) -> Option<ChunkPlan> {
    let cues = find_cues_payload(segment)?;
    let mut cue_times = ElementIter::new(cues)
        .filter(|element| element.id == 0xbb)
        .filter_map(|cue| parse_cue_point(cue.payload, selected.number, timecode_scale))
        .collect::<Vec<_>>();
    cue_times.sort_unstable();
    cue_times.dedup();
    if cue_times.is_empty() {
        return None;
    }

    let mut chunks = Vec::new();
    let mut chunk_start_cue = 0_u32;
    let mut chunk_start_ms = cue_times[0];
    let mut last_start_ms = chunk_start_ms;

    for (idx, cue_ms) in cue_times.iter().copied().enumerate().skip(1) {
        if cue_ms.saturating_sub(chunk_start_ms) >= target_ms {
            chunks.push(NativeChunk {
                index: chunks.len() as u32,
                start: TimePoint::millis(chunk_start_ms),
                duration: TimeDelta::millis(cue_ms.saturating_sub(chunk_start_ms)),
                packet_range: PacketRange {
                    start: chunk_start_cue,
                    end: idx as u32,
                },
                key_aligned: true,
            });
            chunk_start_cue = idx as u32;
            chunk_start_ms = cue_ms;
        }
        last_start_ms = cue_ms;
    }

    chunks.push(NativeChunk {
        index: chunks.len() as u32,
        start: TimePoint::millis(chunk_start_ms),
        duration: TimeDelta::millis(last_start_ms.saturating_sub(chunk_start_ms)),
        packet_range: PacketRange {
            start: chunk_start_cue,
            end: cue_times.len() as u32,
        },
        key_aligned: true,
    });

    Some(ChunkPlan {
        track_ids: vec![selected.id.clone()],
        chunks,
    })
}

fn find_cues_payload(segment: &[u8]) -> Option<&[u8]> {
    if let Some(cue_position) = find_cue_position_from_seek_head(segment)
        && let Some(cues) = ElementIter::new(segment.get(cue_position..)?)
            .next()
            .filter(|element| element.id == 0x1c53_bb6b)
    {
        return Some(cues.payload);
    }

    if segment.len() <= 512 * 1024 * 1024 {
        return ElementIter::new(segment)
            .find(|element| element.id == 0x1c53_bb6b)
            .map(|element| element.payload);
    }

    None
}

fn find_cue_position_from_seek_head(segment: &[u8]) -> Option<usize> {
    let seek_head = ElementIter::new(segment).find(|element| element.id == 0x114d_9b74)?;
    for seek in ElementIter::new(seek_head.payload).filter(|element| element.id == 0x4d_bb) {
        let mut seek_id = None;
        let mut position = None;
        for child in ElementIter::new(seek.payload) {
            match child.id {
                0x53ab => seek_id = read_uint(child.payload),
                0x53ac => {
                    position =
                        read_uint(child.payload).and_then(|value| usize::try_from(value).ok())
                }
                _ => {}
            }
        }
        if seek_id == Some(0x1c53_bb6b) {
            return position;
        }
    }
    None
}

fn parse_cue_point(payload: &[u8], selected_track_number: u64, timecode_scale: u64) -> Option<u64> {
    let mut cue_time = None;
    let mut has_selected_track = false;

    for element in ElementIter::new(payload) {
        match element.id {
            0xb3 => cue_time = read_uint(element.payload),
            0xb7 => {
                has_selected_track |= cue_positions_track(element.payload)
                    .map(|track| track == selected_track_number)
                    .unwrap_or(false);
            }
            _ => {}
        }
    }

    has_selected_track
        .then(|| matroska_timecode_to_ms(cue_time.unwrap_or(0) as i64, timecode_scale))
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct CueLocation {
    time_ms: u64,
    cluster_position: usize,
}

fn cue_cluster_position_for_time(
    segment: &[u8],
    selected_track_number: u64,
    start_ms: u64,
    timecode_scale: u64,
) -> Option<usize> {
    let cues = find_cues_payload(segment)?;
    ElementIter::new(cues)
        .filter(|element| element.id == 0xbb)
        .filter_map(|cue| {
            parse_cue_point_location(cue.payload, selected_track_number, timecode_scale)
        })
        .filter(|cue| cue.time_ms <= start_ms)
        .max_by_key(|cue| cue.time_ms)
        .map(|cue| cue.cluster_position)
}

fn parse_cue_point_location(
    payload: &[u8],
    selected_track_number: u64,
    timecode_scale: u64,
) -> Option<CueLocation> {
    let mut cue_time = None;
    let mut cluster_position = None;

    for element in ElementIter::new(payload) {
        match element.id {
            0xb3 => cue_time = read_uint(element.payload),
            0xb7 => {
                if let Some(position) =
                    cue_position_for_track(element.payload, selected_track_number)
                {
                    cluster_position = Some(position);
                }
            }
            _ => {}
        }
    }

    Some(CueLocation {
        time_ms: matroska_timecode_to_ms(cue_time.unwrap_or(0) as i64, timecode_scale),
        cluster_position: cluster_position?,
    })
}

fn cue_position_for_track(payload: &[u8], selected_track_number: u64) -> Option<usize> {
    let mut track = None;
    let mut cluster_position = None;
    for element in ElementIter::new(payload) {
        match element.id {
            0xf7 => track = read_uint(element.payload),
            0xf1 => {
                cluster_position =
                    read_uint(element.payload).and_then(|value| usize::try_from(value).ok())
            }
            _ => {}
        }
    }
    (track == Some(selected_track_number)).then_some(cluster_position?)
}

fn cue_positions_track(payload: &[u8]) -> Option<u64> {
    ElementIter::new(payload)
        .find(|element| element.id == 0xf7)
        .and_then(|element| read_uint(element.payload))
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

fn parse_segment_timecode_scale(segment: &[u8]) -> u64 {
    ElementIter::new(segment)
        .find(|element| element.id == 0x1549_a966)
        .and_then(|info| {
            ElementIter::new(info.payload)
                .find(|element| element.id == 0x002a_d7b1)
                .and_then(|element| read_uint(element.payload))
        })
        .unwrap_or(1_000_000)
}

fn parse_cluster_timecode(cluster: &[u8]) -> Option<i64> {
    ElementIter::new(cluster)
        .find(|element| element.id == 0xe7)
        .and_then(|element| read_uint(element.payload))
        .and_then(|value| i64::try_from(value).ok())
}

fn parse_tracks(payload: &[u8], meta: &mut MatroskaBasicMetadata) {
    for entry in ElementIter::new(payload).filter(|e| e.id == 0xae) {
        if let Some(track) = parse_track_entry(entry.payload, meta.tracks.len() as u32) {
            meta.tracks.push(track);
        }
    }
}

fn parse_chapters(payload: &[u8], meta: &mut MatroskaBasicMetadata) {
    for edition in ElementIter::new(payload).filter(|element| element.id == 0x45b9) {
        for atom in ElementIter::new(edition.payload).filter(|element| element.id == 0xb6) {
            parse_chapter_atom(atom.payload, &mut meta.chapters);
        }
    }
}

fn parse_chapter_atom(payload: &[u8], chapters: &mut Vec<MatroskaChapter>) {
    let mut uid = None;
    let mut start_ms = None;
    let mut end_ms = None;
    let mut title = None;
    let mut language = None;
    let mut nested_atoms = Vec::new();

    for child in ElementIter::new(payload) {
        match child.id {
            0x73c4 => uid = read_uint(child.payload),
            0x91 => start_ms = read_uint(child.payload).map(nanoseconds_to_millis),
            0x92 => end_ms = read_uint(child.payload).map(nanoseconds_to_millis),
            0x80 => {
                let display = parse_chapter_display(child.payload);
                title = title.or(display.title);
                language = language.or(display.language);
            }
            0xb6 => nested_atoms.push(child.payload),
            _ => {}
        }
    }

    if let Some(start_ms) = start_ms {
        let index = chapters.len();
        chapters.push(MatroskaChapter {
            id: uid
                .map(|uid| uid.to_string())
                .unwrap_or_else(|| format!("ch{index}")),
            start_ms,
            end_ms,
            title,
            language,
        });
    }

    for nested in nested_atoms {
        parse_chapter_atom(nested, chapters);
    }
}

#[derive(Debug, Default)]
struct ChapterDisplay {
    title: Option<String>,
    language: Option<String>,
}

fn parse_chapter_display(payload: &[u8]) -> ChapterDisplay {
    let mut display = ChapterDisplay::default();
    for child in ElementIter::new(payload) {
        match child.id {
            0x85 => display.title = read_string(child.payload),
            0x437c | 0x437d => display.language = read_string(child.payload),
            _ => {}
        }
    }
    display
}

fn nanoseconds_to_millis(value: u64) -> u64 {
    value / 1_000_000
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
    let mut default_duration_ns = None;
    let mut codec_private = None;

    for child in ElementIter::new(payload) {
        match child.id {
            0xd7 => number = read_uint(child.payload),
            0x83 => kind = track_type(read_uint(child.payload).unwrap_or_default()),
            0x86 => codec_id = read_string(child.payload),
            0x63a2 => codec_private = Some(child.payload.to_vec()),
            0x0022_b59c | 0x0022_b59d => language = read_string(child.payload),
            0x536e => name = read_string(child.payload),
            0x88 => default = read_uint(child.payload).unwrap_or(0) != 0,
            0x55aa => forced = read_uint(child.payload).unwrap_or(0) != 0,
            0x0023_e383 => default_duration_ns = read_uint(child.payload),
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
    let pixel_format = (kind == MatroskaTrackKind::Video)
        .then(|| video_pixel_format_from_codec_private(&codec, codec_private.as_deref()))
        .flatten();
    let audio_features = matroska_audio_features(&codec);
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
        pixel_format,
        channels,
        sample_rate,
        atmos: audio_features.atmos,
        object_audio_candidate: audio_features.object_audio_candidate,
        default_duration_ns,
        codec_private,
    })
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct AudioCodingFeatures {
    atmos: bool,
    object_audio_candidate: bool,
}

fn matroska_audio_features(codec: &str) -> AudioCodingFeatures {
    let object_audio_candidate = matches!(codec, "eac3" | "truehd");
    AudioCodingFeatures {
        atmos: false,
        object_audio_candidate,
    }
}

fn video_pixel_format_from_codec_private(
    codec: &str,
    codec_private: Option<&[u8]>,
) -> Option<String> {
    let private = codec_private?;
    match codec {
        "h264" => pixel_format_from_avc_decoder_config(private),
        "hevc" => pixel_format_from_hevc_decoder_config(private),
        _ => None,
    }
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

#[derive(Debug, Clone, PartialEq, Eq)]
struct SelectedChunkTrack {
    id: String,
    number: u64,
    kind: MatroskaTrackKind,
    frame_duration: Option<TimeDelta>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MatroskaPacketTrack {
    pub id: String,
    pub packets: Vec<PacketRef>,
}

pub fn parse_packet_tracks_in_time_window(
    bytes: &[u8],
    requested_track_ids: &[&str],
    start_ms: u64,
    end_ms: u64,
) -> Option<Vec<MatroskaPacketTrack>> {
    let meta = parse_basic_metadata(bytes);
    let selected = requested_track_ids
        .iter()
        .map(|track_id| select_chunk_track(&meta.tracks, Some(track_id)))
        .collect::<Option<Vec<_>>>()?;
    let segment_element = find_first_child_element(bytes, 0x1853_8067)?;
    let segment = segment_element.payload;
    let segment_base_offset = segment_element.payload_offset;
    let timecode_scale = parse_segment_timecode_scale(segment);
    let scan_offset =
        cue_cluster_position_for_time(segment, selected[0].number, start_ms, timecode_scale)
            .unwrap_or(0);
    let mut out = selected
        .iter()
        .map(|track| MatroskaPacketTrack {
            id: track.id.clone(),
            packets: Vec::new(),
        })
        .collect::<Vec<_>>();
    let mut saw_window_packet = false;
    let scan = segment.get(scan_offset..)?;

    'clusters: for cluster in ElementIter::new(scan).filter(|element| element.id == 0x1f43_b675) {
        let cluster_timecode = parse_cluster_timecode(cluster.payload).unwrap_or(0);
        let cluster_ms = matroska_timecode_to_ms(cluster_timecode, timecode_scale);
        if saw_window_packet && cluster_ms >= end_ms {
            break;
        }

        for block in ClusterBlockIter::new_with_base(
            cluster.payload,
            segment_base_offset + scan_offset + cluster.payload_offset,
        ) {
            let Some(track_index) = selected
                .iter()
                .position(|track| track.number == block.track_number)
            else {
                continue;
            };
            let timestamp_ms = matroska_timecode_to_ms(
                cluster_timecode.saturating_add_signed(i64::from(block.relative_timecode)),
                timecode_scale,
            );
            if timestamp_ms >= end_ms {
                continue;
            }
            if timestamp_ms < start_ms {
                continue;
            }
            saw_window_packet = true;
            push_block_packets(
                &mut out[track_index].packets,
                &block,
                timestamp_ms,
                selected[track_index].frame_duration,
            );
        }

        if saw_window_packet && cluster_ms > end_ms {
            break 'clusters;
        }
    }

    if out.iter().any(|track| track.packets.is_empty()) {
        return None;
    }
    for (track, selected) in out.iter_mut().zip(selected.iter()) {
        repair_matroska_packet_timing(&mut track.packets, selected.kind, selected.frame_duration);
    }
    Some(out)
}

fn select_chunk_track(
    tracks: &[MatroskaTrack],
    requested_track_id: Option<&str>,
) -> Option<SelectedChunkTrack> {
    let mut video_index = 0_u32;
    let mut audio_index = 0_u32;
    let mut subtitle_index = 0_u32;
    let mut unknown_index = 0_u32;

    for track in tracks {
        let id = match track.kind {
            MatroskaTrackKind::Video => next_track_id("v", &mut video_index),
            MatroskaTrackKind::Audio => next_track_id("a", &mut audio_index),
            MatroskaTrackKind::Subtitle => next_track_id("s", &mut subtitle_index),
            MatroskaTrackKind::Unknown => next_track_id("x", &mut unknown_index),
        };
        let selected = requested_track_id
            .map(|requested| requested == id)
            .unwrap_or(track.kind == MatroskaTrackKind::Video);
        if selected {
            return Some(SelectedChunkTrack {
                id,
                number: track.number,
                kind: track.kind,
                frame_duration: track_frame_duration(track),
            });
        }
    }

    None
}

fn next_track_id(prefix: &str, counter: &mut u32) -> String {
    let id = format!("{prefix}{counter}");
    *counter += 1;
    id
}

fn track_frame_duration(track: &MatroskaTrack) -> Option<TimeDelta> {
    if let Some(ns) = track.default_duration_ns
        && ns > 0
    {
        return Some(TimeDelta {
            units: ns,
            scale: TimeScale {
                units_per_second: 1_000_000_000,
            },
        });
    }
    if track.kind == MatroskaTrackKind::Audio && track.codec == "aac" {
        return Some(TimeDelta {
            units: 1024,
            scale: TimeScale {
                units_per_second: track.sample_rate?,
            },
        });
    }
    None
}

fn matroska_timecode_to_ms(timecode: i64, scale_ns: u64) -> u64 {
    if timecode <= 0 {
        return 0;
    }
    (timecode as u64).saturating_mul(scale_ns) / 1_000_000
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct ClusterBlock {
    track_number: u64,
    relative_timecode: i16,
    keyframe: bool,
    frames: Vec<BlockFrame>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct BlockFrame {
    payload_offset: u64,
    payload_size: u32,
}

struct ClusterBlockIter<'a> {
    base_offset: usize,
    inner: ElementIter<'a>,
}

impl<'a> ClusterBlockIter<'a> {
    fn new(cluster: &'a [u8]) -> Self {
        Self::new_with_base(cluster, 0)
    }

    fn new_with_base(cluster: &'a [u8], base_offset: usize) -> Self {
        Self {
            base_offset,
            inner: ElementIter::new(cluster),
        }
    }
}

impl Iterator for ClusterBlockIter<'_> {
    type Item = ClusterBlock;

    fn next(&mut self) -> Option<Self::Item> {
        for element in self.inner.by_ref() {
            match element.id {
                0xa3 => {
                    if let Some(block) = parse_block(
                        element.payload,
                        self.base_offset + element.payload_offset,
                        None,
                    ) {
                        return Some(block);
                    }
                }
                0xa0 => {
                    if let Some(block) = parse_block_group(
                        element.payload,
                        self.base_offset + element.payload_offset,
                    ) {
                        return Some(block);
                    }
                }
                _ => {}
            }
        }
        None
    }
}

fn parse_track_packets(
    segment: &[u8],
    segment_base_offset: usize,
    track_number: u64,
    kind: MatroskaTrackKind,
    frame_duration: Option<TimeDelta>,
    timecode_scale: u64,
) -> Option<Vec<PacketRef>> {
    let mut packets: Vec<PacketRef> = Vec::new();
    let mut last_pts = None;

    for cluster in ElementIter::new(segment).filter(|element| element.id == 0x1f43_b675) {
        let cluster_timecode = parse_cluster_timecode(cluster.payload).unwrap_or(0);
        for block in ClusterBlockIter::new_with_base(
            cluster.payload,
            segment_base_offset + cluster.payload_offset,
        ) {
            if block.track_number != track_number {
                continue;
            }
            let timestamp_ms = matroska_timecode_to_ms(
                cluster_timecode.saturating_add_signed(i64::from(block.relative_timecode)),
                timecode_scale,
            );
            if let Some(prev) = last_pts
                && let Some(previous) = packets.last_mut()
            {
                previous.duration = TimeDelta::millis(timestamp_ms.saturating_sub(prev));
            }
            push_block_packets(&mut packets, &block, timestamp_ms, frame_duration);
            last_pts = Some(timestamp_ms);
        }
    }

    if packets.is_empty() {
        return None;
    }

    repair_matroska_packet_timing(&mut packets, kind, frame_duration);
    Some(packets)
}

fn infer_last_packet_duration(packets: &[PacketRef]) -> Option<TimeDelta> {
    let [.., prev, last] = packets else {
        return None;
    };
    Some(TimeDelta {
        units: last.pts.units.saturating_sub(prev.pts.units),
        scale: TimeScale::MILLIS,
    })
}

fn push_block_packets(
    packets: &mut Vec<PacketRef>,
    block: &ClusterBlock,
    timestamp_ms: u64,
    frame_duration: Option<TimeDelta>,
) {
    let frame_duration = frame_duration.unwrap_or_else(|| TimeDelta::millis(0));
    let base_pts = if frame_duration.scale == TimeScale::MILLIS {
        TimePoint::millis(timestamp_ms)
    } else {
        TimePoint {
            units: TimeScale::MILLIS
                .checked_rescale_u64(timestamp_ms, frame_duration.scale, TimeRounding::Nearest)
                .unwrap_or(0),
            scale: frame_duration.scale,
        }
    };
    for (idx, frame) in block.frames.iter().enumerate() {
        let pts_units = base_pts
            .units
            .saturating_add(frame_duration.units.saturating_mul(idx as u64));
        let pts = TimePoint {
            units: pts_units,
            scale: base_pts.scale,
        };
        if let Some(previous) = packets.last_mut() {
            previous.duration = if previous.pts.scale == pts.scale {
                TimeDelta {
                    units: pts.units.saturating_sub(previous.pts.units),
                    scale: pts.scale,
                }
            } else {
                TimeDelta::millis(pts.as_millis().saturating_sub(previous.pts.as_millis()))
            };
        }
        packets.push(PacketRef {
            source_offset: frame.payload_offset,
            size: frame.payload_size,
            pts,
            dts: pts,
            duration: frame_duration,
            keyframe: block.keyframe,
        });
    }
}

fn repair_matroska_packet_timing(
    packets: &mut [PacketRef],
    kind: MatroskaTrackKind,
    frame_duration: Option<TimeDelta>,
) {
    if packets.is_empty() {
        return;
    }

    if kind == MatroskaTrackKind::Video {
        let duration = frame_duration
            .or_else(|| infer_nominal_frame_duration(packets))
            .unwrap_or_else(|| TimeDelta::millis(1));
        let first_dts = packets[0].pts;
        for (index, packet) in packets.iter_mut().enumerate() {
            packet.dts = if first_dts.scale == duration.scale {
                TimePoint {
                    units: first_dts
                        .units
                        .saturating_add(duration.units.saturating_mul(index as u64)),
                    scale: first_dts.scale,
                }
            } else {
                TimePoint::millis(
                    first_dts
                        .as_millis()
                        .saturating_add(duration.as_millis().max(1).saturating_mul(index as u64)),
                )
            };
            packet.duration = duration;
        }
        return;
    }

    let last_duration = infer_last_packet_duration(packets).unwrap_or_else(|| TimeDelta::millis(0));
    if let Some(last) = packets.last_mut() {
        last.duration = last_duration;
    }
}

fn infer_nominal_frame_duration(packets: &[PacketRef]) -> Option<TimeDelta> {
    let mut deltas = packets
        .windows(2)
        .filter_map(|pair| {
            if pair[0].pts.scale == pair[1].pts.scale {
                let a = pair[0].pts.units;
                let b = pair[1].pts.units;
                (b > a).then_some(TimeDelta {
                    units: b - a,
                    scale: pair[0].pts.scale,
                })
            } else {
                let a = pair[0].pts.as_millis();
                let b = pair[1].pts.as_millis();
                (b > a).then_some(TimeDelta::millis(b - a))
            }
        })
        .filter(|delta| delta.as_millis() <= 250)
        .collect::<Vec<_>>();
    if deltas.is_empty() {
        return None;
    }
    deltas.sort_unstable_by_key(|delta| delta.as_millis());
    Some(deltas[deltas.len() / 2])
}

fn parse_block_group(payload: &[u8], base_offset: usize) -> Option<ClusterBlock> {
    let mut block = None;
    let mut has_reference = false;
    for element in ElementIter::new(payload) {
        match element.id {
            0xa1 => {
                block = parse_block(
                    element.payload,
                    base_offset + element.payload_offset,
                    Some(false),
                )
            }
            0xfb => has_reference = true,
            _ => {}
        }
    }
    block.map(|mut block| {
        block.keyframe = !has_reference;
        block
    })
}

fn parse_block(
    payload: &[u8],
    payload_base_offset: usize,
    forced_keyframe: Option<bool>,
) -> Option<ClusterBlock> {
    let (track_number, track_len) = read_vint_size(payload)?;
    if payload.len() < track_len + 3 {
        return None;
    }
    let timecode_offset = track_len;
    let relative_timecode = i16::from_be_bytes(
        payload[timecode_offset..timecode_offset + 2]
            .try_into()
            .ok()?,
    );
    let flags = payload[timecode_offset + 2];
    let data_offset = track_len + 3;
    let frames = parse_block_frames(payload, payload_base_offset, data_offset, flags)?;
    Some(ClusterBlock {
        track_number: track_number as u64,
        relative_timecode,
        keyframe: forced_keyframe.unwrap_or(flags & 0x80 != 0),
        frames,
    })
}

fn parse_block_frames(
    payload: &[u8],
    payload_base_offset: usize,
    data_offset: usize,
    flags: u8,
) -> Option<Vec<BlockFrame>> {
    let lacing = (flags >> 1) & 0x03;
    match lacing {
        0 => {
            let size = payload.len().checked_sub(data_offset)?;
            Some(vec![BlockFrame {
                payload_offset: (payload_base_offset + data_offset) as u64,
                payload_size: u32::try_from(size).ok()?,
            }])
        }
        1 => parse_xiph_laced_frames(payload, payload_base_offset, data_offset),
        2 => parse_fixed_laced_frames(payload, payload_base_offset, data_offset),
        3 => parse_ebml_laced_frames(payload, payload_base_offset, data_offset),
        _ => None,
    }
}

fn parse_xiph_laced_frames(
    payload: &[u8],
    payload_base_offset: usize,
    data_offset: usize,
) -> Option<Vec<BlockFrame>> {
    let frame_count = usize::from(*payload.get(data_offset)?) + 1;
    let mut cursor = data_offset + 1;
    let mut sizes = Vec::with_capacity(frame_count);
    let mut known_total = 0_usize;
    for _ in 0..frame_count.saturating_sub(1) {
        let mut size = 0_usize;
        loop {
            let b = usize::from(*payload.get(cursor)?);
            cursor += 1;
            size = size.checked_add(b)?;
            if b != 255 {
                break;
            }
        }
        known_total = known_total.checked_add(size)?;
        sizes.push(size);
    }
    let remaining = payload.len().checked_sub(cursor)?;
    let last = remaining.checked_sub(known_total)?;
    sizes.push(last);
    frames_from_sizes(payload_base_offset, cursor, &sizes)
}

fn parse_fixed_laced_frames(
    payload: &[u8],
    payload_base_offset: usize,
    data_offset: usize,
) -> Option<Vec<BlockFrame>> {
    let frame_count = usize::from(*payload.get(data_offset)?) + 1;
    if frame_count == 0 {
        return None;
    }
    let cursor = data_offset + 1;
    let remaining = payload.len().checked_sub(cursor)?;
    if remaining % frame_count != 0 {
        return None;
    }
    let size = remaining / frame_count;
    frames_from_sizes(payload_base_offset, cursor, &vec![size; frame_count])
}

fn parse_ebml_laced_frames(
    payload: &[u8],
    payload_base_offset: usize,
    data_offset: usize,
) -> Option<Vec<BlockFrame>> {
    let frame_count = usize::from(*payload.get(data_offset)?) + 1;
    let mut cursor = data_offset + 1;
    let (first_size, first_len) = read_vint_size(payload.get(cursor..)?)?;
    cursor += first_len;
    let mut sizes = Vec::with_capacity(frame_count);
    sizes.push(first_size);
    let mut previous = isize::try_from(first_size).ok()?;
    let mut known_total = first_size;
    for _ in 1..frame_count.saturating_sub(1) {
        let (delta, len) = read_signed_vint(payload.get(cursor..)?)?;
        cursor += len;
        previous = previous.checked_add(delta)?;
        if previous < 0 {
            return None;
        }
        let size = usize::try_from(previous).ok()?;
        known_total = known_total.checked_add(size)?;
        sizes.push(size);
    }
    let remaining = payload.len().checked_sub(cursor)?;
    let last = remaining.checked_sub(known_total)?;
    sizes.push(last);
    frames_from_sizes(payload_base_offset, cursor, &sizes)
}

fn frames_from_sizes(
    payload_base_offset: usize,
    mut cursor: usize,
    sizes: &[usize],
) -> Option<Vec<BlockFrame>> {
    let mut frames = Vec::with_capacity(sizes.len());
    for size in sizes {
        frames.push(BlockFrame {
            payload_offset: (payload_base_offset + cursor) as u64,
            payload_size: u32::try_from(*size).ok()?,
        });
        cursor = cursor.checked_add(*size)?;
    }
    Some(frames)
}

trait SaturatingAddSigned {
    fn saturating_add_signed(self, rhs: i64) -> i64;
}

impl SaturatingAddSigned for i64 {
    fn saturating_add_signed(self, rhs: i64) -> i64 {
        if rhs >= 0 {
            self.saturating_add(rhs)
        } else {
            self.saturating_sub(rhs.saturating_abs())
        }
    }
}

#[cfg(test)]
mod tests {
    use proptest::prelude::*;

    use super::*;

    proptest! {
        #[test]
        fn arbitrary_matroska_bytes_do_not_panic(bytes in prop::collection::vec(any::<u8>(), 0..4096)) {
            let _ = looks_like_ebml(&bytes);
            let _ = parse_basic_metadata(&bytes);
            let _ = parse_chunk_plan(&bytes, None, 4_000);
            let _ = parse_packet_track(&bytes, None);
        }

        #[test]
        fn matroska_timecode_conversion_is_monotonic(
            first in -10_000_i64..10_000,
            delta in 0_i64..10_000,
            scale in 1_u64..10_000_000,
        ) {
            let second = first.saturating_add(delta);
            prop_assert!(
                matroska_timecode_to_ms(first, scale)
                    <= matroska_timecode_to_ms(second, scale)
            );
        }
    }

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
            &[
                elem(
                    0xe0,
                    &[elem(0xb0, &[0x07, 0x80]), elem(0xba, &[0x04, 0x38])].concat(),
                ),
                elem(
                    0x63a2,
                    &[
                        1, 110, 0, 31, 0xff, 0xe1, 0, 1, 0x67, 1, 0, 1, 0x68, 0xfd, 0xfa, 0xfa,
                    ],
                ),
            ],
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
        let chapters = elem(
            0x1043_a770,
            &elem(
                0x45b9,
                &elem(
                    0xb6,
                    &[
                        elem(0x73c4, &[0x2a]),
                        elem(0x91, &5_000_000_000_u64.to_be_bytes()),
                        elem(0x92, &10_000_000_000_u64.to_be_bytes()),
                        elem(
                            0x80,
                            &[elem(0x85, b"Opening"), elem(0x437c, b"eng")].concat(),
                        ),
                    ]
                    .concat(),
                ),
            ),
        );
        let segment = elem(0x1853_8067, &[info, tracks, chapters].concat());
        let mut bytes = elem(0x1a45_dfa3, &[]);
        bytes.extend_from_slice(&segment);

        let meta = parse_basic_metadata(&bytes);
        assert_eq!(meta.duration_ms, Some(12_500));
        assert_eq!(meta.tracks.len(), 2);
        assert_eq!(meta.tracks[0].codec, "h264");
        assert_eq!(meta.tracks[0].width, Some(1920));
        assert_eq!(meta.tracks[0].height, Some(1080));
        assert_eq!(meta.tracks[0].pixel_format.as_deref(), Some("yuv420-10bit"));
        assert_eq!(meta.tracks[1].codec, "aac");
        assert_eq!(meta.tracks[1].channels, Some(2));
        assert_eq!(meta.tracks[1].sample_rate, Some(48000));
        assert!(!meta.tracks[1].object_audio_candidate);
        assert_eq!(meta.chapters.len(), 1);
        assert_eq!(meta.chapters[0].id, "42");
        assert_eq!(meta.chapters[0].start_ms, 5_000);
        assert_eq!(meta.chapters[0].end_ms, Some(10_000));
        assert_eq!(meta.chapters[0].title.as_deref(), Some("Opening"));
        assert_eq!(meta.chapters[0].language.as_deref(), Some("eng"));
    }

    #[test]
    fn marks_matroska_object_audio_candidates() {
        let tracks = elem(
            0x1654_ae6b,
            &[
                track_entry(1, 2, "A_EAC3", &[]),
                track_entry(2, 2, "A_TRUEHD", &[]),
                track_entry(3, 2, "A_AAC", &[]),
            ]
            .concat(),
        );
        let segment = elem(0x1853_8067, &tracks);
        let mut bytes = elem(0x1a45_dfa3, &[]);
        bytes.extend_from_slice(&segment);

        let meta = parse_basic_metadata(&bytes);
        let eac3 = &meta.tracks[0];
        let truehd = &meta.tracks[1];
        let aac = &meta.tracks[2];

        assert!(!eac3.atmos);
        assert!(eac3.object_audio_candidate);
        assert!(truehd.object_audio_candidate);
        assert!(!aac.object_audio_candidate);
    }

    #[test]
    fn plans_chunks_from_matroska_clusters() {
        let info = elem(
            0x1549_a966,
            &[elem(0x002a_d7b1, &1_000_000_u64.to_be_bytes()[5..])].concat(),
        );
        let video = track_entry(1, 1, "V_MPEG4/ISO/AVC", &[]);
        let tracks = elem(0x1654_ae6b, &video);
        let cluster0 = cluster(
            0,
            &[
                simple_block(1, 0, true),
                simple_block(1, 1000, false),
                simple_block(1, 2000, true),
            ],
        );
        let cluster1 = cluster(
            3000,
            &[simple_block(1, 0, false), simple_block(1, 1000, true)],
        );
        let segment = elem(0x1853_8067, &[info, tracks, cluster0, cluster1].concat());
        let mut bytes = elem(0x1a45_dfa3, &[]);
        bytes.extend_from_slice(&segment);

        let plan = parse_chunk_plan(&bytes, None, 2_000).unwrap();
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
        let starts = plan
            .chunks
            .iter()
            .map(|chunk| chunk.start.as_millis())
            .collect::<Vec<_>>();
        let durations = plan
            .chunks
            .iter()
            .map(|chunk| chunk.duration.as_millis())
            .collect::<Vec<_>>();
        assert_eq!(starts, vec![0, 2000, 4000]);
        assert_eq!(durations, vec![2000, 2000, 0]);
    }

    #[test]
    fn extracts_matroska_track_by_time_range() {
        let info = elem(
            0x1549_a966,
            &[elem(0x002a_d7b1, &1_000_000_u64.to_be_bytes()[5..])].concat(),
        );
        let video = track_entry(1, 1, "V_MPEG4/ISO/AVC", &[]);
        let audio = track_entry(2, 2, "A_AC3", &[]);
        let tracks = elem(0x1654_ae6b, &[video, audio].concat());
        let cluster0 = cluster(
            0,
            &[
                simple_block_with_payload(2, 3000, true, b"a3000"),
                simple_block_with_payload(2, 4000, true, b"a4000"),
                simple_block_with_payload(2, 5000, true, b"a5000"),
                simple_block_with_payload(2, 6000, true, b"a6000"),
                simple_block_with_payload(2, 7000, true, b"a7000"),
                simple_block_with_payload(2, 8000, true, b"a8000"),
                simple_block_with_payload(2, 9000, true, b"a9000"),
            ],
        );
        let segment = elem(0x1853_8067, &[info, tracks, cluster0].concat());
        let mut bytes = elem(0x1a45_dfa3, &[]);
        bytes.extend_from_slice(&segment);

        let (chunk, payload) = extract_time_range(&bytes, Some("a0"), 4671, 8675, 1).unwrap();
        assert_eq!(chunk.chunk.index, 1);
        assert_eq!(chunk.chunk.start.as_millis(), 4000);
        assert_eq!(chunk.packet_count, 5);
        assert_eq!(payload, b"a4000a5000a6000a7000a8000");
    }

    #[test]
    fn extracts_matroska_payloads_with_file_absolute_offsets() {
        let info = elem(
            0x1549_a966,
            &[elem(0x002a_d7b1, &1_000_000_u64.to_be_bytes()[5..])].concat(),
        );
        let video = track_entry(1, 1, "V_MPEG4/ISO/AVC", &[]);
        let tracks = elem(0x1654_ae6b, &video);
        let cluster0 = cluster(
            0,
            &[
                simple_block_with_payload(1, 0, true, b"frame-one"),
                simple_block_with_payload(1, 1000, false, b"frame-two"),
            ],
        );
        let segment = elem(0x1853_8067, &[info, tracks, cluster0].concat());
        let mut bytes = elem(0x1a45_dfa3, &[]);
        bytes.extend_from_slice(&segment);

        let (chunk, payload) = extract_chunk(&bytes, Some("v0"), 4_000, 0).unwrap();
        assert_eq!(chunk.packet_count, 2);
        assert_eq!(payload, b"frame-oneframe-two");
    }

    #[test]
    fn repairs_matroska_video_dts_for_decode_order_b_frames() {
        let info = elem(
            0x1549_a966,
            &[elem(0x002a_d7b1, &1_000_000_u64.to_be_bytes()[5..])].concat(),
        );
        let video = track_entry(
            1,
            1,
            "V_MPEG4/ISO/AVC",
            &[elem(0x0023_e383, &40_000_000_u64.to_be_bytes()[4..])],
        );
        let tracks = elem(0x1654_ae6b, &video);
        let cluster0 = cluster(
            0,
            &[
                simple_block(1, 0, true),
                simple_block(1, 120, false),
                simple_block(1, 40, false),
                simple_block(1, 80, false),
            ],
        );
        let segment = elem(0x1853_8067, &[info, tracks, cluster0].concat());
        let mut bytes = elem(0x1a45_dfa3, &[]);
        bytes.extend_from_slice(&segment);

        let track = parse_packet_track(&bytes, Some("v0")).unwrap();
        let pts = track
            .packets
            .iter()
            .map(|packet| packet.pts.as_millis())
            .collect::<Vec<_>>();
        let dts = track
            .packets
            .iter()
            .map(|packet| packet.dts.as_millis())
            .collect::<Vec<_>>();
        let durations = track
            .packets
            .iter()
            .map(|packet| packet.duration.as_millis())
            .collect::<Vec<_>>();

        assert_eq!(pts, vec![0, 120, 40, 80]);
        assert_eq!(dts, vec![0, 40, 80, 120]);
        assert_eq!(durations, vec![40, 40, 40, 40]);
    }

    #[test]
    fn preserves_fractional_matroska_default_video_duration() {
        let info = elem(
            0x1549_a966,
            &[elem(0x002a_d7b1, &1_000_000_u64.to_be_bytes()[5..])].concat(),
        );
        let video = track_entry(
            1,
            1,
            "V_MPEG4/ISO/AVC",
            &[elem(0x0023_e383, &41_708_333_u64.to_be_bytes()[4..])],
        );
        let tracks = elem(0x1654_ae6b, &video);
        let cluster0 = cluster(
            0,
            &[
                simple_block(1, 0, true),
                simple_block(1, 42, false),
                simple_block(1, 83, false),
            ],
        );
        let segment = elem(0x1853_8067, &[info, tracks, cluster0].concat());
        let mut bytes = elem(0x1a45_dfa3, &[]);
        bytes.extend_from_slice(&segment);

        let track = parse_packet_track(&bytes, Some("v0")).unwrap();
        let scale = TimeScale {
            units_per_second: 1_000_000_000,
        };

        assert_eq!(
            track.packets[0].duration,
            TimeDelta {
                units: 41_708_333,
                scale
            }
        );
        assert_eq!(
            track.packets[1].dts,
            TimePoint {
                units: 41_708_333,
                scale
            }
        );
        assert_eq!(
            track.packets[2].dts,
            TimePoint {
                units: 83_416_666,
                scale
            }
        );
    }

    #[test]
    fn plans_chunks_from_matroska_cues_before_scanning_clusters() {
        let info = elem(
            0x1549_a966,
            &[elem(0x002a_d7b1, &1_000_000_u64.to_be_bytes()[5..])].concat(),
        );
        let video = track_entry(1, 1, "V_MPEG4/ISO/AVC", &[]);
        let tracks = elem(0x1654_ae6b, &video);
        let cues = elem(
            0x1c53_bb6b,
            &[
                cue_point(0, 1),
                cue_point(1000, 1),
                cue_point(2200, 1),
                cue_point(3100, 1),
                cue_point(4300, 1),
            ]
            .concat(),
        );
        let segment = elem(0x1853_8067, &[info, tracks, cues].concat());
        let mut bytes = elem(0x1a45_dfa3, &[]);
        bytes.extend_from_slice(&segment);

        let plan = parse_chunk_plan(&bytes, None, 2_000).unwrap();
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
        let starts = plan
            .chunks
            .iter()
            .map(|chunk| chunk.start.as_millis())
            .collect::<Vec<_>>();
        let durations = plan
            .chunks
            .iter()
            .map(|chunk| chunk.duration.as_millis())
            .collect::<Vec<_>>();
        assert_eq!(starts, vec![0, 2200, 4300]);
        assert_eq!(durations, vec![2200, 2100, 0]);
    }

    #[test]
    fn finds_matroska_cues_through_seek_head() {
        let info = elem(
            0x1549_a966,
            &[elem(0x002a_d7b1, &1_000_000_u64.to_be_bytes()[5..])].concat(),
        );
        let video = track_entry(1, 1, "V_MPEG4/ISO/AVC", &[]);
        let tracks = elem(0x1654_ae6b, &video);
        let cues = elem(
            0x1c53_bb6b,
            &[cue_point(0, 1), cue_point(2200, 1), cue_point(4300, 1)].concat(),
        );
        let mut seek_head = seek_head_for_cues(0);
        let cue_position = seek_head.len() + info.len() + tracks.len();
        seek_head = seek_head_for_cues(cue_position as u8);
        let segment = elem(0x1853_8067, &[seek_head, info, tracks, cues].concat());
        let mut bytes = elem(0x1a45_dfa3, &[]);
        bytes.extend_from_slice(&segment);

        let plan = parse_chunk_plan(&bytes, None, 2_000).unwrap();
        assert_eq!(plan.track_ids, vec!["v0"]);
        assert_eq!(plan.chunks.len(), 3);
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

    fn cluster(timecode: u64, blocks: &[Vec<u8>]) -> Vec<u8> {
        let mut payload = elem(0xe7, &timecode.to_be_bytes()[6..]);
        for block in blocks {
            payload.extend_from_slice(block);
        }
        elem(0x1f43_b675, &payload)
    }

    fn simple_block(track_number: u8, relative_timecode: i16, keyframe: bool) -> Vec<u8> {
        simple_block_with_payload(track_number, relative_timecode, keyframe, &[0xde, 0xad])
    }

    fn simple_block_with_payload(
        track_number: u8,
        relative_timecode: i16,
        keyframe: bool,
        block_payload: &[u8],
    ) -> Vec<u8> {
        let mut payload = Vec::new();
        payload.push(0x80 | track_number);
        payload.extend_from_slice(&relative_timecode.to_be_bytes());
        payload.push(if keyframe { 0x80 } else { 0x00 });
        payload.extend_from_slice(block_payload);
        elem(0xa3, &payload)
    }

    fn cue_point(timecode: u64, track_number: u8) -> Vec<u8> {
        elem(
            0xbb,
            &[
                elem(0xb3, &timecode.to_be_bytes()[6..]),
                elem(0xb7, &elem(0xf7, &[track_number])),
            ]
            .concat(),
        )
    }

    fn seek_head_for_cues(cue_position: u8) -> Vec<u8> {
        elem(
            0x114d_9b74,
            &elem(
                0x4d_bb,
                &[
                    elem(0x53ab, &0x1c53_bb6b_u32.to_be_bytes()),
                    elem(0x53ac, &[cue_position]),
                ]
                .concat(),
            ),
        )
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
        if size < 0x7f {
            out.push(0x80 | size as u8);
        } else {
            assert!(size < 0x3fff);
            out.push(0x40 | ((size >> 8) as u8));
            out.push((size & 0xff) as u8);
        }
    }
}
