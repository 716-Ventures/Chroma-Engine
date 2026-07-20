use thiserror::Error;

use crate::packet::{
    extract_packet_payload, packet_samples_for_range, ChunkPlan, ExtractedChunk, NativeChunk,
    PacketExtractError, PacketRange, PacketRef, TimeDelta, TimePoint, TimeScale,
};

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
                last_seen_ms = timestamp_ms;
                key_aligned = block.keyframe;
                saw_block = true;
            }

            let should_cut = absolute_packet_index > 0
                && block.keyframe
                && timestamp_ms.saturating_sub(chunk_start_ms) >= target_ms;
            if should_cut {
                if current_index >= start_chunk && current_index < end_chunk {
                    let chunk = NativeChunk {
                        index: current_index,
                        start: TimePoint::millis(chunk_start_ms),
                        duration: TimeDelta::millis(last_seen_ms.saturating_sub(chunk_start_ms)),
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
                push_block_packets(
                    &mut packets,
                    &block,
                    timestamp_ms,
                    selected.frame_duration_ms,
                );
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
            duration: TimeDelta::millis(last_seen_ms.saturating_sub(chunk_start_ms)),
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
    if let Some(plan) = parse_cue_chunk_plan(segment, &selected, timecode_scale, target_ms) {
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
                last_seen_ms = timestamp_ms;
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
                    duration: TimeDelta::millis(last_seen_ms.saturating_sub(chunk_start_ms)),
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
                duration: TimeDelta::millis(last_start_ms.saturating_sub(chunk_start_ms)),
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
    if let Some(cue_position) = find_cue_position_from_seek_head(segment) {
        if let Some(cues) = ElementIter::new(segment.get(cue_position..)?)
            .next()
            .filter(|element| element.id == 0x1c53_bb6b)
        {
            return Some(cues.payload);
        }
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
            0x23e3_83 => default_duration_ns = read_uint(child.payload),
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
        default_duration_ns,
        codec_private,
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

#[derive(Debug, Clone, PartialEq, Eq)]
struct SelectedChunkTrack {
    id: String,
    number: u64,
    frame_duration_ms: Option<u64>,
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
                selected[track_index].frame_duration_ms,
            );
        }

        if saw_window_packet && cluster_ms > end_ms {
            break 'clusters;
        }
    }

    if out.iter().any(|track| track.packets.is_empty()) {
        return None;
    }
    for track in &mut out {
        let last_duration =
            infer_last_packet_duration(&track.packets).unwrap_or_else(|| TimeDelta::millis(0));
        if let Some(last) = track.packets.last_mut() {
            last.duration = last_duration;
        }
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
                frame_duration_ms: track_frame_duration_ms(track),
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

fn track_frame_duration_ms(track: &MatroskaTrack) -> Option<u64> {
    if let Some(ns) = track.default_duration_ns {
        let ms = (ns.saturating_add(500_000)) / 1_000_000;
        if ms > 0 {
            return Some(ms);
        }
    }
    if track.kind == MatroskaTrackKind::Audio && track.codec == "aac" {
        let sample_rate = u64::from(track.sample_rate?);
        return Some(
            1024_u64
                .saturating_mul(1000)
                .saturating_add(sample_rate / 2)
                / sample_rate,
        );
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
            if let Some(prev) = last_pts {
                if let Some(previous) = packets.last_mut() {
                    previous.duration = TimeDelta::millis(timestamp_ms.saturating_sub(prev));
                }
            }
            push_block_packets(&mut packets, &block, timestamp_ms, None);
            last_pts = Some(timestamp_ms);
        }
    }

    if packets.is_empty() {
        return None;
    }

    let last_duration =
        infer_last_packet_duration(&packets).unwrap_or_else(|| TimeDelta::millis(0));
    if let Some(last) = packets.last_mut() {
        last.duration = last_duration;
    }
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
    frame_duration_ms: Option<u64>,
) {
    let frame_duration_ms = frame_duration_ms.unwrap_or(0);
    for (idx, frame) in block.frames.iter().enumerate() {
        let pts = timestamp_ms.saturating_add(frame_duration_ms.saturating_mul(idx as u64));
        if let Some(previous) = packets.last_mut() {
            previous.duration = TimeDelta::millis(pts.saturating_sub(previous.pts.as_millis()));
        }
        packets.push(PacketRef {
            source_offset: frame.payload_offset,
            size: frame.payload_size,
            pts: TimePoint::millis(pts),
            dts: TimePoint::millis(pts),
            duration: TimeDelta::millis(frame_duration_ms),
            keyframe: block.keyframe,
        });
    }
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

fn find_first_child(bytes: &[u8], id: u32) -> Option<&[u8]> {
    ElementIter::new(bytes)
        .find(|element| element.id == id)
        .map(|element| element.payload)
}

fn find_first_child_element(bytes: &[u8], id: u32) -> Option<Element<'_>> {
    ElementIter::new(bytes).find(|element| element.id == id)
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
    payload_offset: usize,
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
            payload_offset: payload_start,
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

fn read_signed_vint(bytes: &[u8]) -> Option<(isize, usize)> {
    let (value, len) = read_vint_size(bytes)?;
    let bits = 7_usize.checked_mul(len)?;
    let bias = (1_isize.checked_shl((bits - 1) as u32)?).checked_sub(1)?;
    let signed = isize::try_from(value).ok()?.checked_sub(bias)?;
    Some((signed, len))
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
        assert!(size < 0x7f);
        out.push(0x80 | size as u8);
    }
}
