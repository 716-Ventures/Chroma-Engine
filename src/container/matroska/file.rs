//! Positional Matroska indexing: retain cluster offsets, read block headers,
//! and leave compressed payload in the source until a packet is requested.
use anyhow::{Result, bail};
mod seek;

use super::*;
use crate::source::{MediaSource, probe_reader::Element as FileElement};

#[derive(Debug)]
struct Cluster {
    timecode: i64,
    time_ms: u64,
    payload: u64,
    end: u64,
}

#[derive(Debug)]
pub(crate) struct MatroskaFileIndex {
    metadata: MatroskaBasicMetadata,
    clusters: Vec<Cluster>,
    cues: Option<Vec<u8>>,
    scale: u64,
    segment: Option<FileElement>,
    anchors: Vec<(u64, u64)>,
}

impl MatroskaFileIndex {
    pub(crate) fn open(source: &MediaSource) -> Result<Self> {
        source.validate_current()?;
        let metadata = parse_basic_metadata(source.as_ref());
        let metadata_segment = find_first_child(source.as_ref(), 0x1853_8067)
            .ok_or_else(|| anyhow::anyhow!("missing Matroska metadata"))?;
        let scale = parse_segment_timecode_scale(metadata_segment);
        let mut index = Self {
            metadata,
            clusters: Vec::new(),
            cues: None,
            scale,
            segment: None,
            anchors: Vec::new(),
        };
        let mut offset = 0;
        let mut visits = 0;
        let segment = loop {
            visit(&mut visits)?;
            let element = source.element_at(offset, source.len())?;
            if element.id == 0x1853_8067 {
                break element;
            }
            offset = element.end;
        };
        index.segment = Some(segment);
        if let Some(indexed) = seek::load(source, segment, scale) {
            if std::env::var_os("CHROMA_PROBE_DIAGNOSTICS").is_some() {
                eprintln!(
                    "matroska_cue_index indexed=true anchors={}",
                    indexed.anchors.len()
                );
            }
            index.cues = Some(indexed.bytes);
            index.anchors = indexed.anchors;
            source.validate_current()?;
            return Ok(index);
        }
        offset = segment.payload;
        while offset < segment.end {
            visit(&mut visits)?;
            let element = source.element_at(offset, segment.end)?;
            if element.unknown_size {
                bail!("unknown-size Matroska children require a finalized source");
            }
            if element.id == 0x1f43_b675 {
                let mut child_offset = element.payload;
                let mut time = None;
                while child_offset < element.end {
                    visit(&mut visits)?;
                    let child = source.element_at(child_offset, element.end)?;
                    if child.id == 0xe7 {
                        if child.end - child.payload > 8 {
                            bail!("invalid cluster timestamp");
                        }
                        let bytes = source.read_metadata_prefix(
                            child.payload,
                            (child.end - child.payload) as usize,
                        )?;
                        time = read_uint(&bytes).and_then(|time| i64::try_from(time).ok());
                        break;
                    }
                    child_offset = child.end;
                }
                let timecode = time.ok_or_else(|| anyhow::anyhow!("missing cluster timestamp"))?;
                let time_ms = matroska_timecode_to_ms(timecode, scale);
                if index
                    .clusters
                    .last()
                    .is_some_and(|cluster| cluster.time_ms > time_ms)
                {
                    bail!("nonmonotonic Matroska cluster timestamps");
                }
                index.clusters.try_reserve(1)?;
                if index.clusters.len() >= source.policy().parse_limits().max_boxes {
                    bail!("Matroska cluster index budget exceeded");
                }
                index.clusters.push(Cluster {
                    timecode,
                    time_ms,
                    payload: element.payload,
                    end: element.end,
                });
            } else if element.id == 0x1c53_bb6b {
                let cues = source
                    .read_window_unvalidated(offset, usize::try_from(element.end - offset)?)?;
                validate_cues_budget(&cues, source.policy())?;
                index.cues = Some(cues);
            }
            offset = element.end;
        }
        source.validate_current()?;
        Ok(index)
    }

    pub(crate) fn plan(
        &self,
        source: &MediaSource,
        track: &str,
        target_ms: u64,
    ) -> Result<ChunkPlan> {
        let selected = select_chunk_track(&self.metadata.tracks, Some(track))
            .ok_or_else(|| anyhow::anyhow!("no matching Matroska track"))?;
        if let Some(cues) = &self.cues
            && let Some(mut plan) = parse_cue_chunk_plan(cues, &selected, self.scale, target_ms)
        {
            if let Some(last) = plan.chunks.last_mut()
                && let Some(duration) = self.metadata.duration_ms
            {
                last.duration =
                    TimeDelta::millis(duration.saturating_sub(last.start.as_millis()).max(1));
            }
            return Ok(plan);
        }
        let packets = self
            .packets(source, &[track], 0, u64::MAX)?
            .remove(0)
            .packets;
        Ok(crate::packet::plan_track_chunks(track, &packets, target_ms))
    }

    /// Preserve the manifest's unique track plans while visiting uncued audio
    /// blocks once. Retain one pending packet per track and one laced block.
    pub(crate) fn plans(
        &self,
        source: &MediaSource,
        tracks: &[&str],
        target_ms: u64,
    ) -> Result<Vec<ChunkPlan>> {
        source.validate_current()?;
        let selected = tracks
            .iter()
            .map(|track| select_chunk_track(&self.metadata.tracks, Some(track)))
            .collect::<Option<Vec<_>>>()
            .ok_or_else(|| anyhow::anyhow!("no matching Matroska track"))?;
        let mut plans = Vec::with_capacity(tracks.len());
        let mut audio = Vec::new();
        for (index, (track, selected)) in tracks.iter().zip(&selected).enumerate() {
            let cued = self
                .cues
                .as_ref()
                .and_then(|cues| parse_cue_chunk_plan(cues, selected, self.scale, target_ms));
            if let Some(mut plan) = cued {
                if let Some(last) = plan.chunks.last_mut()
                    && let Some(duration) = self.metadata.duration_ms
                {
                    last.duration =
                        TimeDelta::millis(duration.saturating_sub(last.start.as_millis()).max(1));
                }
                plans.push(Some(plan));
            } else if selected.kind == MatroskaTrackKind::Audio {
                plans.push(None);
                audio.push((index, AudioPlanBuilder::new(target_ms)));
            } else {
                plans.push(Some(self.plan(source, track, target_ms)?));
            }
        }
        if !audio.is_empty() {
            let limits = source.policy().parse_limits();
            let mut scratch = Vec::new();
            self.for_each_block(source, 0, u64::MAX, |block, timestamp| {
                if timestamp == u64::MAX {
                    return Ok(());
                }
                if let Some((index, builder)) = audio
                    .iter_mut()
                    .find(|(index, _)| selected[*index].number == block.track_number)
                {
                    builder.check_packet_budget(block.frames.len(), limits)?;
                    push_block_packets(
                        &mut scratch,
                        block,
                        timestamp,
                        selected[*index].frame_duration,
                    )
                    .ok_or_else(|| anyhow::anyhow!("Matroska packet budget exceeded"))?;
                    for packet in scratch.drain(..) {
                        builder.push(packet);
                    }
                }
                Ok(())
            })?;
            for (index, builder) in audio {
                plans[index] = Some(builder.finish(tracks[index])?);
            }
        }
        source.validate_current()?;
        plans
            .into_iter()
            .map(|plan| plan.ok_or_else(|| anyhow::anyhow!("missing Matroska chunk plan")))
            .collect()
    }

    pub(crate) fn packets(
        &self,
        source: &MediaSource,
        tracks: &[&str],
        start: u64,
        end: u64,
    ) -> Result<Vec<MatroskaPacketTrack>> {
        let windows = tracks
            .iter()
            .map(|track| (*track, start, end))
            .collect::<Vec<_>>();
        self.packet_windows(source, &windows)
    }

    /// Extract differently prerolled selected tracks in one local cluster pass.
    pub(crate) fn packet_windows(
        &self,
        source: &MediaSource,
        windows: &[(&str, u64, u64)],
    ) -> Result<Vec<MatroskaPacketTrack>> {
        if windows.is_empty() || windows.iter().any(|(_, start, end)| start >= end) {
            bail!("invalid Matroska packet window");
        }
        let start = windows.iter().map(|(_, start, _)| *start).min().unwrap();
        let end = windows.iter().map(|(_, _, end)| *end).max().unwrap();
        source.validate_current()?;
        let selected = windows
            .iter()
            .map(|(track, _, _)| select_chunk_track(&self.metadata.tracks, Some(track)))
            .collect::<Option<Vec<_>>>()
            .ok_or_else(|| anyhow::anyhow!("no matching Matroska track"))?;
        let mut output = windows
            .iter()
            .map(|(track, _, _)| MatroskaPacketTrack {
                id: (*track).into(),
                packets: Vec::new(),
            })
            .collect::<Vec<_>>();
        let mut packet_count = 0_usize;
        let mut video_started = vec![false; windows.len()];
        let mut video_ended = vec![false; windows.len()];
        let limits = source.policy().parse_limits();
        self.for_each_block(source, start, end, |block, timestamp| {
            if let Some(track_index) = selected
                .iter()
                .position(|track| track.number == block.track_number)
            {
                // Video windows are decode-order GOP partitions. Selecting
                // by PTS alone can omit the next CRA while retaining its
                // leading B pictures, which then reference a missing frame.
                let include = if selected[track_index].kind == MatroskaTrackKind::Video {
                    if block.keyframe && timestamp >= windows[track_index].2 {
                        video_ended[track_index] = true;
                    }
                    if block.keyframe && timestamp >= windows[track_index].1 {
                        video_started[track_index] = true;
                    }
                    video_started[track_index] && !video_ended[track_index]
                } else {
                    timestamp >= windows[track_index].1 && timestamp < windows[track_index].2
                };
                if include {
                    packet_count = packet_count
                        .checked_add(block.frames.len())
                        .ok_or_else(|| anyhow::anyhow!("Matroska packet count overflow"))?;
                    if packet_count > limits.max_samples_per_track
                        || packet_count.saturating_mul(std::mem::size_of::<PacketRef>())
                            > limits.max_index_bytes
                    {
                        bail!("cumulative Matroska packet budget exceeded");
                    }
                    push_block_packets(
                        &mut output[track_index].packets,
                        block,
                        timestamp,
                        selected[track_index].frame_duration,
                    )
                    .ok_or_else(|| anyhow::anyhow!("Matroska packet budget exceeded"))?;
                }
            }
            Ok(())
        })?;
        for (output, selected) in output.iter_mut().zip(&selected) {
            if output.packets.is_empty() {
                bail!("no packets in Matroska time window");
            }
            repair_matroska_packet_timing(
                &mut output.packets,
                selected.kind,
                selected.frame_duration,
            );
        }
        source.validate_current()?;
        Ok(output)
    }

    /// Presence sampling stops as soon as all requested tracks have appeared.
    /// It does not build a packet index for the following 32 seconds of video.
    pub(crate) fn tracks_present(
        &self,
        source: &MediaSource,
        tracks: &[&str],
        start: u64,
        end: u64,
    ) -> Result<bool> {
        source.validate_current()?;
        let selected = tracks
            .iter()
            .map(|id| select_chunk_track(&self.metadata.tracks, Some(id)))
            .collect::<Option<Vec<_>>>()
            .ok_or_else(|| anyhow::anyhow!("Missing sample track"))?;
        let mut found = vec![false; selected.len()];
        self.for_each_block_until(source, start, end, |block, time| {
            if time >= start
                && time < end
                && let Some(i) = selected.iter().position(|s| s.number == block.track_number)
            {
                found[i] = !block.frames.is_empty();
            }
            Ok(found.iter().all(|v| *v))
        })?;
        source.validate_current()?;
        if std::env::var_os("CHROMA_PROBE_DIAGNOSTICS").is_some() {
            eprintln!("matroska_audio_presence start={start} end={end} found={found:?}");
        }
        Ok(found.iter().all(|v| *v))
    }
    fn for_each_block(
        &self,
        source: &MediaSource,
        start: u64,
        end: u64,
        mut consume: impl FnMut(&ClusterBlock, u64) -> Result<()>,
    ) -> Result<()> {
        self.for_each_block_until(source, start, end, |block, time| {
            consume(block, time)?;
            Ok(false)
        })
    }
    fn for_each_block_until(
        &self,
        source: &MediaSource,
        start: u64,
        end: u64,
        mut consume: impl FnMut(&ClusterBlock, u64) -> Result<bool>,
    ) -> Result<()> {
        let span = matroska_timecode_to_ms(32768, self.scale).saturating_add(1);
        let mut visits = 0;
        if !self.anchors.is_empty() {
            let segment = self
                .segment
                .ok_or_else(|| anyhow::anyhow!("Missing Segment"))?;
            // CueTime belongs to a Block and can differ from Cluster time by a
            // signed 16-bit offset. Two spans ensure no early cluster is lost.
            let anchor = self.anchors.iter().rfind(|(time, _)| {
                start > span.saturating_mul(2)
                    && *time < start.saturating_sub(span.saturating_mul(2))
            });
            let mut offset = segment.payload;
            if let Some((time, position)) = anchor {
                let element = source.element_at(*position, segment.end)?;
                if element.id == 0x1f43_b675
                    && let Ok(cluster) = read_cluster(source, element, self.scale, &mut visits)
                    && cluster.time_ms.abs_diff(*time) <= span
                {
                    offset = *position;
                }
            }
            let mut previous = None;
            while offset < segment.end {
                visit(&mut visits)?;
                let element = source.element_at(offset, segment.end)?;
                if element.unknown_size {
                    bail!("unknown-size Matroska children require a finalized source");
                }
                if element.id == 0x1f43_b675 {
                    let cluster = read_cluster(source, element, self.scale, &mut visits)?;
                    if previous.is_some_and(|last| last > cluster.time_ms) {
                        bail!("nonmonotonic Matroska cluster timestamps");
                    }
                    previous = Some(cluster.time_ms);
                    if cluster.time_ms.saturating_sub(span) >= end {
                        break;
                    }
                    if cluster.time_ms >= start.saturating_sub(span)
                        && scan_cluster(source, &cluster, self.scale, &mut visits, &mut consume)?
                    {
                        return Ok(());
                    }
                }
                offset = element.end;
            }
        } else {
            let first = self
                .clusters
                .partition_point(|c| c.time_ms < start.saturating_sub(span));
            for cluster in &self.clusters[first..] {
                if cluster.time_ms.saturating_sub(span) >= end {
                    break;
                }
                if scan_cluster(source, cluster, self.scale, &mut visits, &mut consume)? {
                    return Ok(());
                }
            }
        }
        Ok(())
    }
}

struct AudioPlanBuilder {
    planner: crate::packet::TrackChunkPlanner,
    pending: Option<PacketRef>,
    last_duration: Option<TimeDelta>,
    count: usize,
}

impl AudioPlanBuilder {
    fn new(target_ms: u64) -> Self {
        Self {
            planner: crate::packet::TrackChunkPlanner::new(target_ms),
            pending: None,
            last_duration: None,
            count: 0,
        }
    }

    fn check_packet_budget(
        &self,
        additional: usize,
        limits: super::super::ParseLimits,
    ) -> Result<()> {
        let count = self.count.checked_add(additional);
        let defaults = super::super::ParseLimits::default();
        if count.is_none_or(|count| {
            count
                > limits
                    .max_samples_per_track
                    .min(defaults.max_samples_per_track)
                || count.saturating_mul(std::mem::size_of::<PacketRef>())
                    > limits.max_index_bytes.min(defaults.max_index_bytes)
        }) {
            bail!("cumulative Matroska packet budget exceeded");
        }
        Ok(())
    }

    fn push(&mut self, packet: PacketRef) {
        if let Some(mut previous) = self.pending.take() {
            previous.duration = if previous.pts.scale == packet.pts.scale {
                TimeDelta {
                    units: packet.pts.units.saturating_sub(previous.pts.units),
                    scale: packet.pts.scale,
                }
            } else {
                TimeDelta::millis(
                    packet
                        .pts
                        .as_millis()
                        .saturating_sub(previous.pts.as_millis()),
                )
            };
            self.last_duration = Some(previous.duration);
            self.planner.push(&previous);
        }
        self.pending = Some(packet);
        self.count += 1;
    }

    fn finish(mut self, track: &str) -> Result<ChunkPlan> {
        let mut last = self
            .pending
            .take()
            .ok_or_else(|| anyhow::anyhow!("no packets in Matroska time window"))?;
        last.duration = self.last_duration.unwrap_or_else(|| TimeDelta::millis(0));
        self.planner.push(&last);
        Ok(self.planner.finish(track))
    }
}

fn visit(visits: &mut usize) -> Result<()> {
    *visits += 1;
    if *visits > 2_000_000 {
        bail!("Matroska scan work budget exceeded");
    }
    Ok(())
}

pub(crate) fn indexed_metadata_elements(
    source: &MediaSource,
    segment: FileElement,
    id: u32,
) -> Result<Option<Vec<FileElement>>> {
    match seek::indexed_elements(source, segment, id) {
        Ok(v) => Ok(v),
        Err(_) => {
            source.check_work()?;
            source.validate_current()?;
            Ok(None)
        }
    }
}

/// Consume validated blocks while their headers are still in the read window.
/// A false result requires the caller to discard partial output before falling
/// back to a complete scan. Consumer errors propagate rather than causing retries.
pub(crate) fn visit_indexed_subtitle_blocks(
    source: &MediaSource,
    segment: FileElement,
    scale: u64,
    tracks: &[u64],
    consume: impl FnMut(u64, FileElement) -> Result<()>,
) -> Result<bool> {
    seek::visit_subtitle_blocks(source, segment, scale, tracks, consume)
}

fn read_cluster(
    source: &MediaSource,
    element: FileElement,
    scale: u64,
    visits: &mut usize,
) -> Result<Cluster> {
    let mut offset = element.payload;
    while offset < element.end {
        visit(visits)?;
        let field = source.element_at(offset, element.end)?;
        if field.id == 0xe7 {
            if field.end - field.payload > 8 {
                bail!("Invalid cluster timestamp");
            }
            let bytes =
                source.read_metadata_prefix(field.payload, (field.end - field.payload) as usize)?;
            let timecode = read_uint(&bytes)
                .and_then(|v| i64::try_from(v).ok())
                .ok_or_else(|| anyhow::anyhow!("Invalid cluster timestamp"))?;
            // Conversion uses the caller's scale below.
            return Ok(Cluster {
                timecode,
                time_ms: matroska_timecode_to_ms(timecode, scale),
                payload: element.payload,
                end: element.end,
            });
        }
        offset = field.end;
    }
    bail!("Missing cluster timestamp")
}
fn scan_cluster(
    source: &MediaSource,
    cluster: &Cluster,
    scale: u64,
    visits: &mut usize,
    consume: &mut impl FnMut(&ClusterBlock, u64) -> Result<bool>,
) -> Result<bool> {
    source.visit_cluster();
    let mut offset = cluster.payload;
    while offset < cluster.end {
        visit(visits)?;
        let element = source.element_at(offset, cluster.end)?;
        let block = match element.id {
            0xa3 => Some(read_block(source, &element, None)?),
            0xa0 => read_group(source, &element, visits)?,
            _ => None,
        };
        if let Some(block) = block {
            let timestamp = matroska_timecode_to_ms(
                cluster
                    .timecode
                    .saturating_add(i64::from(block.relative_timecode)),
                scale,
            );
            if consume(&block, timestamp)? {
                return Ok(true);
            }
        }
        offset = element.end;
    }
    Ok(false)
}

fn read_group(
    source: &MediaSource,
    group: &FileElement,
    visits: &mut usize,
) -> Result<Option<ClusterBlock>> {
    let mut offset = group.payload;
    let mut block = None;
    let mut reference = false;
    while offset < group.end {
        visit(visits)?;
        let element = source.element_at(offset, group.end)?;
        match element.id {
            0xa1 => block = Some(read_block(source, &element, Some(false))?),
            0xfb => reference = true,
            _ => {}
        }
        offset = element.end;
    }
    Ok(block.map(|mut block| {
        block.keyframe = !reference;
        block
    }))
}

fn read_block(
    source: &MediaSource,
    element: &FileElement,
    keyframe: Option<bool>,
) -> Result<ClusterBlock> {
    let total = usize::try_from(element.end - element.payload)?;
    let mut size = total.min(16);
    loop {
        let prefix = if size <= 64 {
            source.read_metadata_prefix(element.payload, size)?
        } else {
            source.read_window_unvalidated(element.payload, size)?
        };
        if let Some(block) =
            parse_block_prefix(&prefix, total, usize::try_from(element.payload)?, keyframe)
        {
            return Ok(block);
        }
        if size == total || size >= 64 * 1024 {
            bail!("invalid or oversized Matroska lacing header");
        }
        size = (size * 2).min(total);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn element(id: u32, payload: &[u8]) -> Vec<u8> {
        let id = id.to_be_bytes();
        let first = id.iter().position(|byte| *byte != 0).unwrap();
        [
            &id[first..],
            &((1_u64 << 56) | payload.len() as u64).to_be_bytes(),
            payload,
        ]
        .concat()
    }

    fn track(number: u8, kind: u8, duration: Option<u64>) -> Vec<u8> {
        let mut bytes = [
            element(0xd7, &[number]),
            element(0x83, &[kind]),
            element(
                0x86,
                if kind == 1 {
                    b"V_MPEG4/ISO/AVC"
                } else {
                    b"A_AAC"
                },
            ),
        ]
        .concat();
        if let Some(duration) = duration {
            bytes.extend(element(0x23e383, &duration.to_be_bytes()));
        }
        element(0xae, &bytes)
    }

    fn block(track: u8, time: i16, flags: u8, lace: &[u8]) -> Vec<u8> {
        element(
            0xa3,
            &[
                &[0x80 | track],
                &time.to_be_bytes()[..],
                &[flags],
                lace,
                &[7; 12],
            ]
            .concat(),
        )
    }

    fn fixture(cue_audio: bool) -> (tempfile::TempDir, std::path::PathBuf) {
        delayed_audio_fixture(cue_audio, 0)
    }

    fn delayed_audio_fixture(
        cue_audio: bool,
        delay: i16,
    ) -> (tempfile::TempDir, std::path::PathBuf) {
        let tracks = element(
            0x1654_ae6b,
            &[
                track(1, 1, Some(41_708_333)),
                track(2, 2, Some(20_000_001)),
                track(3, 2, None),
            ]
            .concat(),
        );
        let mut cues = Vec::new();
        for time in [0_u64, 4_000, 8_000] {
            let mut positions = element(0xb7, &element(0xf7, &[1]));
            if cue_audio {
                positions.extend(element(0xb7, &element(0xf7, &[3])));
            }
            cues.extend(element(
                0xbb,
                &[element(0xb3, &time.to_be_bytes()), positions].concat(),
            ));
        }
        let clusters = [
            (
                0_u64,
                vec![
                    block(1, 0, 0x80, &[]),
                    block(2, delay, 0x80, &[]),
                    block(3, delay, 0, &[]),
                    block(2, delay + 20, 0x82, &[2, 3, 4]), // Xiph lacing.
                    block(3, delay, 0x80, &[]),
                    block(3, delay - 2, 0x80, &[]),
                ],
            ),
            (
                1_000,
                vec![
                    block(2, 0, 0x84, &[2]),             // Fixed lacing.
                    block(3, 0, 0x86, &[2, 0x83, 0xc0]), // EBML lacing.
                ],
            ),
            (
                4_000,
                vec![
                    block(1, 0, 0x80, &[]),
                    block(2, 0, 0, &[]),
                    block(3, 0, 0x80, &[]),
                    block(2, 100, 0x80, &[]),
                    block(3, -100, 0, &[]),
                ],
            ),
        ]
        .into_iter()
        .map(|(time, blocks)| {
            element(
                0x1f43_b675,
                &[
                    element(0xe7, &time.to_be_bytes()),
                    element(0xec, &[0; 8_192]),
                    blocks.concat(),
                ]
                .concat(),
            )
        })
        .collect::<Vec<_>>()
        .concat();
        let bytes = [
            element(0x1a45_dfa3, &[]),
            element(
                0x1853_8067,
                &[
                    element(
                        0x1549_a966,
                        &[
                            element(0x2ad7b1, &1_000_000_u64.to_be_bytes()),
                            element(0x4489, &10_000_f64.to_be_bytes()),
                        ]
                        .concat(),
                    ),
                    tracks,
                    element(0x1c53_bb6b, &cues),
                    clusters,
                ]
                .concat(),
            ),
        ]
        .concat();
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("multi-audio.mkv");
        std::fs::write(&path, bytes).unwrap();
        (directory, path)
    }

    #[test]
    fn three_linked_seek_heads_keep_the_indexed_path() {
        let seek = |id: u32, pos: u64| {
            element(
                0x4dbb,
                &[
                    element(0x53ab, &id.to_be_bytes()),
                    element(0x53ac, &pos.to_be_bytes()),
                ]
                .concat(),
            )
        };
        let head = |entries: &[(u32, u64)]| {
            element(
                0x114d9b74,
                &entries
                    .iter()
                    .flat_map(|(id, pos)| seek(*id, *pos))
                    .collect::<Vec<_>>(),
            )
        };
        let info = element(0x1549a966, &element(0x4489, &10_000_f64.to_be_bytes()));
        let tracks = element(0x1654ae6b, &track(1, 1, Some(40_000_000)));
        let first_len = head(&[(0x114d9b74, 0)]).len() as u64;
        let clusters = element(0x1f43b675, &element(0xe7, &[0])).repeat(1000);
        let cluster_pos = first_len + info.len() as u64 + tracks.len() as u64;
        let cues = element(
            0x1c53bb6b,
            &element(
                0xbb,
                &[
                    element(0xb3, &[0]),
                    element(
                        0xb7,
                        &[
                            element(0xf7, &[1]),
                            element(0xf1, &cluster_pos.to_be_bytes()),
                        ]
                        .concat(),
                    ),
                ]
                .concat(),
            ),
        );
        let cues_pos = cluster_pos + clusters.len() as u64;
        let second_pos = cues_pos + cues.len() as u64;
        let second_len = head(&[(0x114d9b74, 0), (0x1549a966, first_len)]).len() as u64;
        let third_pos = second_pos + second_len;
        let bytes = element(
            0x18538067,
            &[
                head(&[(0x114d9b74, second_pos)]),
                info.clone(),
                tracks,
                clusters,
                cues,
                head(&[(0x114d9b74, third_pos), (0x1549a966, first_len)]),
                head(&[
                    (0x1654ae6b, first_len + info.len() as u64),
                    (0x1c53bb6b, cues_pos),
                ]),
            ]
            .concat(),
        );
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("linked.mkv");
        std::fs::write(&path, [element(0x1a45dfa3, &[]), bytes].concat()).unwrap();
        let source = MediaSource::open_packet_copy(&path).unwrap();
        assert_eq!(source.matroska_index().unwrap().anchors.len(), 1);
        assert_eq!(source.io_stats().2, 0);
    }

    #[test]
    fn subtitle_heavy_cues_use_bounded_sample_index_budget() {
        let point = element(
            0xbb,
            &[
                element(0xb3, &[0]),
                element(
                    0xb7,
                    &[
                        element(0xf7, &[1]),
                        element(0xf1, &[0]),
                        element(0xf0, &[0]),
                    ]
                    .concat(),
                ),
            ]
            .concat(),
        );
        let bytes = element(0x1c53bb6b, &point.repeat(25_000));
        let policy = crate::ResourcePolicy::default();
        assert!(validate_metadata_budget(&bytes, policy.parse_limits()).is_err());
        validate_cues_budget(&bytes, &policy).unwrap();
        let tight = crate::ResourcePolicy {
            index_bytes: 64 * 1024,
            ..policy
        };
        assert!(validate_cues_budget(&bytes, &tight).is_err());
    }

    fn indexed_fixture() -> (tempfile::TempDir, std::path::PathBuf, Vec<u8>) {
        let info = element(
            0x1549a966,
            &[
                element(0x2ad7b1, &1000000_u64.to_be_bytes()),
                element(0x4489, &400000_f64.to_be_bytes()),
            ]
            .concat(),
        );
        let tracks = element(
            0x1654ae6b,
            &[track(1, 1, Some(41708333)), track(2, 2, Some(20000000))].concat(),
        );
        let head = |position: u64| {
            element(
                0x114d9b74,
                &element(
                    0x4dbb,
                    &[
                        element(0x53ab, &0x1c53bb6b_u32.to_be_bytes()),
                        element(0x53ac, &position.to_be_bytes()),
                    ]
                    .concat(),
                ),
            )
        };
        let mut clusters = Vec::new();
        let mut cue_bytes = Vec::new();
        let prefix = head(0).len() + info.len() + tracks.len();
        for i in 0..100_u64 {
            let position = prefix + clusters.len();
            if i % 2 == 0 {
                cue_bytes.extend(element(
                    0xbb,
                    &[
                        element(0xb3, &(i * 4000).to_be_bytes()),
                        element(
                            0xb7,
                            &[
                                element(0xf7, &[1]),
                                element(0xf1, &(position as u64).to_be_bytes()),
                            ]
                            .concat(),
                        ),
                    ]
                    .concat(),
                ));
            }
            let mut payload = [
                element(0xe7, &(i * 4000).to_be_bytes()),
                block(1, 0, 0x80, &[]),
                block(2, 0, 0x80, &[]),
                element(0xec, &[0; 32768]),
            ]
            .concat();
            if i >= 8 {
                payload.extend(block(2, -30000, 0x80, &[]));
            }
            clusters.extend(element(0x1f43b675, &payload));
        }
        let cue_offset = prefix + clusters.len();
        let cues = element(0x1c53bb6b, &cue_bytes);
        let bytes = [
            element(0x1a45dfa3, &[]),
            element(
                0x18538067,
                &[head(cue_offset as u64), info, tracks, clusters, cues].concat(),
            ),
        ]
        .concat();
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("indexed.mkv");
        std::fs::write(&path, &bytes).unwrap();
        (dir, path, bytes)
    }
    #[test]
    fn seek_head_windows_preserve_uncued_clusters_negative_offsets_and_exact_packets() {
        let (_dir, path, bytes) = indexed_fixture();
        let source = MediaSource::open_packet_copy(&path).unwrap();
        let index = source.matroska_index().unwrap();
        assert_eq!(index.anchors.len(), 50);
        assert!(index.clusters.is_empty());
        for (start, end) in [
            (0, 1),
            (2000, 2001),
            (130000, 130020),
            (270000, 270020),
            (396000, 400000),
        ] {
            let actual = index.packets(&source, &["a0"], start, end);
            let expected =
                super::super::parse_packet_tracks_in_time_window(&bytes, &["a0"], start, end);
            match (actual, expected) {
                (Ok(actual), Some(expected)) => assert_eq!(actual, expected, "{start}..{end}"),
                (Err(_), None) => {}
                other => panic!("window disagreement at {start}..{end}: {other:?}"),
            }
        }
    }
    #[test]
    fn first_audio_presence_stops_without_indexing_following_clusters() {
        let (_dir, path, _) = indexed_fixture();
        let source = MediaSource::open_packet_copy(&path).unwrap();
        assert!(
            source
                .matroska_index()
                .unwrap()
                .tracks_present(&source, &["a0"], 0, 1000)
                .unwrap()
        );
        assert_eq!(source.io_stats().2, 1);
        let other = MediaSource::open_packet_copy(&path).unwrap();
        other
            .matroska_index()
            .unwrap()
            .packets(&other, &["a0"], 0, 1000)
            .unwrap();
        assert!(other.io_stats().2 > source.io_stats().2);
    }

    #[test]
    fn delayed_audio_does_not_trigger_a_global_packet_index() {
        let (_dir, path) = delayed_audio_fixture(false, 2000);
        let source = MediaSource::open_packet_copy(&path).unwrap();
        let manifest = crate::playback_manifest::build_file_matroska_streaming_manifest(
            &source,
            &path,
            Default::default(),
        )
        .unwrap();
        assert!(
            manifest
                .tracks
                .iter()
                .filter(|t| t.kind == "audio")
                .all(|t| t.time_window_addressed && t.chunks.is_empty())
        );
    }

    #[test]
    fn streaming_manifest_skips_global_audio_index_and_keeps_video_and_decoder_metadata() {
        let (_dir, path) = fixture(false);
        let full_source = MediaSource::open_packet_copy(&path).unwrap();
        let full = crate::playback_manifest::build_file_matroska_manifest(
            &full_source,
            &path,
            Default::default(),
        )
        .unwrap();
        let source = MediaSource::open_packet_copy(&path).unwrap();
        let streaming = crate::playback_manifest::build_file_matroska_streaming_manifest(
            &source,
            &path,
            Default::default(),
        )
        .unwrap();
        assert!(
            streaming
                .tracks
                .iter()
                .filter(|track| track.kind == "audio")
                .all(|track| track.time_window_addressed && track.chunks.is_empty())
        );
        let mut normalized = streaming.clone();
        for track in &mut normalized.tracks {
            if track.time_window_addressed {
                track.time_window_addressed = false;
                track.chunks = full
                    .tracks
                    .iter()
                    .find(|old| old.id == track.id)
                    .unwrap()
                    .chunks
                    .clone();
            }
        }
        assert_eq!(normalized, full);
    }

    #[test]
    fn batch_audio_plans_match_individual_plans_with_one_cluster_pass() {
        for cue_audio in [false, true] {
            let (_directory, path) = fixture(cue_audio);
            let source = MediaSource::open_packet_copy(&path).unwrap();
            let index = source.matroska_index().unwrap();
            let tracks = ["v0", "a0", "a1"];
            let before = source.io_stats();
            let expected = tracks
                .iter()
                .map(|track| index.plan(&source, track, 1_000).unwrap())
                .collect::<Vec<_>>();
            let individual = source.io_stats();
            let actual = index.plans(&source, &tracks, 1_000).unwrap();
            let batch = source.io_stats();
            assert_eq!(actual, expected, "audio cues: {cue_audio}");
            assert_eq!(individual.2 - before.2, if cue_audio { 3 } else { 6 });
            assert_eq!(batch.2 - individual.2, 3);
            assert!(batch.1 - individual.1 <= individual.1 - before.1);
            if !cue_audio {
                assert!(batch.1 - individual.1 < individual.1 - before.1);
            }
            eprintln!(
                "audio_plan_read_proof audio_cues={cue_audio} individual_read_ops={} batch_read_ops={} individual_cluster_visits={} batch_cluster_visits={}",
                individual.1 - before.1,
                batch.1 - individual.1,
                individual.2 - before.2,
                batch.2 - individual.2,
            );

            let source = MediaSource::open_packet_copy(&path).unwrap();
            let manifest = crate::playback_manifest::build_file_matroska_manifest(
                &source,
                &path,
                crate::playback_manifest::MatroskaManifestOptions {
                    chunk_target_ms: 1_000,
                    include_audio: true,
                },
            )
            .unwrap();
            assert_eq!(source.io_stats().2, 3);
            for (track, expected) in manifest.tracks.iter().zip(expected) {
                assert_eq!(track.chunks, expected.chunks);
            }
        }
    }

    #[test]
    fn streaming_audio_timing_matches_repaired_packet_timing_at_edges() {
        let nanos = TimeScale {
            units_per_second: 1_000_000_000,
        };
        for times in [
            vec![(123, nanos)],
            vec![(0, nanos), (0, nanos), (100_000_001, nanos), (3, nanos)],
            vec![
                (0, TimeScale::MILLIS),
                (250_000_001, nanos),
                (500, TimeScale::MILLIS),
            ],
        ] {
            let mut packets = times
                .into_iter()
                .enumerate()
                .map(|(index, (units, scale))| PacketRef {
                    source_offset: index as u64,
                    size: 1,
                    pts: TimePoint { units, scale },
                    dts: TimePoint { units, scale },
                    duration: TimeDelta { units: 1, scale },
                    keyframe: index % 2 == 0,
                })
                .collect::<Vec<_>>();
            let mut builder = AudioPlanBuilder::new(100);
            for packet in packets.iter().cloned() {
                builder.push(packet);
            }
            // Reproduce push_block_packets' next-PTS duration updates before
            // the existing final-packet repair, including mixed time scales.
            for index in 1..packets.len() {
                let next = packets[index].pts;
                let previous = &mut packets[index - 1];
                previous.duration = if previous.pts.scale == next.scale {
                    TimeDelta {
                        units: next.units.saturating_sub(previous.pts.units),
                        scale: next.scale,
                    }
                } else {
                    TimeDelta::millis(next.as_millis().saturating_sub(previous.pts.as_millis()))
                };
            }
            repair_matroska_packet_timing(&mut packets, MatroskaTrackKind::Audio, None);
            assert_eq!(
                builder.finish("a0").unwrap(),
                crate::packet::plan_track_chunks("a0", &packets, 100)
            );
        }
        assert!(AudioPlanBuilder::new(100).finish("a0").is_err());
    }

    #[test]
    fn batch_audio_keeps_packet_budgets_independent_per_track() {
        let (_directory, path) = fixture(false);
        let source = MediaSource::open_packet_copy(&path).unwrap();
        let index = source.matroska_index().unwrap();
        let counts = ["a0", "a1"].map(|track| {
            index.packets(&source, &[track], 0, u64::MAX).unwrap()[0]
                .packets
                .len()
        });
        let per_track_max = *counts.iter().max().unwrap();
        assert!(counts.iter().sum::<usize>() > per_track_max);
        for limit in [per_track_max, per_track_max - 1] {
            let runtime = crate::EngineRuntime::new(crate::ResourcePolicy {
                index_bytes: limit * std::mem::size_of::<PacketRef>(),
                ..crate::ResourcePolicy::default()
            })
            .unwrap();
            let source =
                MediaSource::open_with_context(&path, runtime, crate::WorkControl::default())
                    .unwrap();
            let index = source.matroska_index().unwrap();
            assert_eq!(
                index.plan(&source, "a0", 1_000).is_ok(),
                limit == per_track_max
            );
            assert_eq!(
                index.plans(&source, &["a0", "a1"], 1_000).is_ok(),
                limit == per_track_max
            );
        }
        let mut builder = AudioPlanBuilder::new(100);
        builder.count = usize::MAX;
        assert!(
            builder
                .check_packet_budget(1, super::super::super::ParseLimits::default())
                .is_err()
        );
    }

    #[test]
    fn batch_audio_scan_honors_cancellation_and_empty_tracks() {
        let (_directory, path) = fixture(false);
        let control = crate::WorkControl::default();
        let source = MediaSource::open_with_context(
            &path,
            crate::EngineRuntime::new(crate::ResourcePolicy::default()).unwrap(),
            control.clone(),
        )
        .unwrap();
        let index = source.matroska_index().unwrap();
        let mut visits = 0;
        assert!(
            index
                .for_each_block(&source, 0, u64::MAX, |_, _| {
                    visits += 1;
                    control.cancel();
                    Ok(())
                })
                .is_err()
        );
        assert_eq!(visits, 1);
        assert!(index.plans(&source, &["a0", "a1"], 1_000).is_err());

        let empty = [
            element(0x1a45_dfa3, &[]),
            element(0x1853_8067, &element(0x1654_ae6b, &track(1, 2, None))),
        ]
        .concat();
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("empty-track.mkv");
        std::fs::write(&path, empty).unwrap();
        let source = MediaSource::open_packet_copy(&path).unwrap();
        let index = source.matroska_index().unwrap();
        assert!(index.plan(&source, "a0", 1_000).is_err());
        assert!(index.plans(&source, &["a0"], 1_000).is_err());
    }
}
