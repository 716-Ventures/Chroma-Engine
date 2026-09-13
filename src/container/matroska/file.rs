//! Positional Matroska indexing: retain cluster offsets, read block headers,
//! and leave compressed payload in the source until a packet is requested.
use anyhow::{Result, bail};

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
                        let bytes = source.read_window_unvalidated(
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
                validate_metadata_budget(&cues, source.policy().parse_limits())?;
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
        let span = matroska_timecode_to_ms(32_768, self.scale).saturating_add(1);
        let first = self
            .clusters
            .partition_point(|cluster| cluster.time_ms < start.saturating_sub(span));
        let mut visits = 0;
        let mut packet_count = 0_usize;
        let mut video_started = vec![false; windows.len()];
        let mut video_ended = vec![false; windows.len()];
        let limits = source.policy().parse_limits();
        for cluster in &self.clusters[first..] {
            if cluster.time_ms.saturating_sub(span) >= end {
                break;
            }
            source.visit_cluster();
            let mut offset = cluster.payload;
            while offset < cluster.end {
                visit(&mut visits)?;
                let element = source.element_at(offset, cluster.end)?;
                let block = match element.id {
                    0xa3 => Some(read_block(source, &element, None)?),
                    0xa0 => read_group(source, &element, &mut visits)?,
                    _ => None,
                };
                if let Some(block) = block
                    && let Some(track_index) = selected
                        .iter()
                        .position(|track| track.number == block.track_number)
                {
                    let timestamp = matroska_timecode_to_ms(
                        cluster
                            .timecode
                            .saturating_add(i64::from(block.relative_timecode)),
                        self.scale,
                    );
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
                            &block,
                            timestamp,
                            selected[track_index].frame_duration,
                        )
                        .ok_or_else(|| anyhow::anyhow!("Matroska packet budget exceeded"))?;
                    }
                }
                offset = element.end;
            }
        }
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
}

fn visit(visits: &mut usize) -> Result<()> {
    *visits += 1;
    if *visits > 2_000_000 {
        bail!("Matroska scan work budget exceeded");
    }
    Ok(())
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
        let prefix = source.read_window_unvalidated(element.payload, size)?;
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
