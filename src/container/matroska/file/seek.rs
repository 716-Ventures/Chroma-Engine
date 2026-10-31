//! Validated SeekHead/Cues anchors. Positions remain in the original Segment;
//! uncued clusters between anchors are still walked by the selected time window.
use super::*;
use std::collections::{BTreeMap, HashSet};

pub(super) struct IndexedCues {
    pub bytes: Vec<u8>,
    pub anchors: Vec<(u64, u64)>,
}
pub(super) fn load(source: &MediaSource, segment: FileElement, scale: u64) -> Option<IndexedCues> {
    load_checked(source, segment, scale).ok().flatten()
}
fn load_checked(
    source: &MediaSource,
    segment: FileElement,
    scale: u64,
) -> Result<Option<IndexedCues>> {
    let Some(positions) = positions(source, segment)? else {
        return Ok(None);
    };
    let Some(offset) = positions.get(&0x1c53bb6b).copied() else {
        return Ok(None);
    };
    let cues = source.element_at(offset, segment.end)?;
    if cues.id != 0x1c53_bb6b || cues.unknown_size {
        return Ok(None);
    }
    let size = usize::try_from(cues.end - offset)?;
    source.policy().check(
        "container metadata",
        source.as_ref().len().saturating_add(size),
        source.policy().metadata_bytes,
    )?;
    let bytes = source.read_window_unvalidated(offset, size)?;
    validate_cues_budget(&bytes, source.policy())?;
    let payload = find_first_child(&bytes, 0x1c53_bb6b)
        .ok_or_else(|| anyhow::anyhow!("Cues payload missing"))?;
    let mut by_position = BTreeMap::<u64, u64>::new();
    for cue in ElementIter::new(payload).filter(|e| e.id == 0xbb) {
        let Some(time) = find_first_child(cue.payload, 0xb3)
            .and_then(read_uint)
            .and_then(|v| i64::try_from(v).ok())
        else {
            return Ok(None);
        };
        let time = matroska_timecode_to_ms(time, scale);
        for track in ElementIter::new(cue.payload).filter(|e| e.id == 0xb7) {
            let Some(position) = find_first_child(track.payload, 0xf1).and_then(read_uint) else {
                return Ok(None);
            };
            let absolute = segment
                .payload
                .checked_add(position)
                .filter(|p| *p < segment.end)
                .ok_or_else(|| anyhow::anyhow!("Cue position outside Segment"))?;
            // Include tree-node overhead and the final anchor vector in the
            // index budget, rather than bounding only the encoded Cues bytes.
            source.policy().check(
                "Matroska cue index",
                by_position.len().saturating_add(1).saturating_mul(128),
                source.policy().index_bytes,
            )?;
            by_position
                .entry(absolute)
                .and_modify(|t| *t = (*t).min(time))
                .or_insert(time);
        }
    }
    let anchors = by_position
        .into_iter()
        .map(|(position, time)| (time, position))
        .collect::<Vec<_>>();
    if anchors.is_empty() || anchors.windows(2).any(|w| w[0].0 > w[1].0) {
        return Ok(None);
    }
    Ok(Some(IndexedCues { bytes, anchors }))
}

fn positions(source: &MediaSource, segment: FileElement) -> Result<Option<BTreeMap<u32, u64>>> {
    let first = source.element_at(segment.payload, segment.end)?;
    if first.id != 0x114d_9b74 || first.unknown_size {
        return Ok(None);
    }
    let mut heads = vec![segment.payload];
    let mut seen = HashSet::new();
    let mut positions = BTreeMap::new();
    for index in 0..16 {
        let Some(offset) = heads.get(index).copied() else {
            break;
        };
        if !seen.insert(offset) {
            return Ok(None);
        }
        let head = source.element_at(offset, segment.end)?;
        if head.id != 0x114d_9b74 || head.unknown_size {
            return Ok(None);
        }
        let bytes = source
            .read_window_unvalidated(head.payload, usize::try_from(head.end - head.payload)?)?;
        validate_metadata_budget(&bytes, source.policy().parse_limits())?;
        for (entry_index, entry) in ElementIter::new(&bytes)
            .filter(|e| e.id == 0x4dbb)
            .take(1025)
            .enumerate()
        {
            if entry_index >= 1024 {
                return Ok(None);
            }
            let Some(id) = find_first_child(entry.payload, 0x53ab)
                .and_then(read_uint)
                .and_then(|v| u32::try_from(v).ok())
            else {
                return Ok(None);
            };
            let Some(relative) = find_first_child(entry.payload, 0x53ac).and_then(read_uint) else {
                return Ok(None);
            };
            let absolute = segment
                .payload
                .checked_add(relative)
                .filter(|v| *v < segment.end)
                .ok_or_else(|| anyhow::anyhow!("Seek position outside Segment"))?;
            if id == 0x114d9b74 {
                if heads.len() >= 16 || heads.contains(&absolute) {
                    return Ok(None);
                }
                heads.push(absolute);
                continue;
            }
            // Cluster references may repeat; only metadata positions are
            // needed here. Linked SeekHeads are followed separately above.
            if id == 0x1f43b675 {
                continue;
            }
            if positions
                .insert(id, absolute)
                .is_some_and(|old| old != absolute)
            {
                return Ok(None);
            }
        }
    }
    Ok(Some(positions))
}

pub(super) fn indexed_elements(
    source: &MediaSource,
    segment: FileElement,
    id: u32,
) -> Result<Option<Vec<FileElement>>> {
    let Some(positions) = positions(source, segment)? else {
        return Ok(None);
    };
    if !positions.contains_key(&0x1549a966) || !positions.contains_key(&0x1654ae6b) {
        return Ok(None);
    }
    let mut out = Vec::new();
    if let Some(offset) = positions.get(&id) {
        let element = source.element_at(*offset, segment.end)?;
        if element.id != id || element.unknown_size {
            return Ok(None);
        }
        out.push(element);
    } else {
        return Ok(None);
    }
    Ok(Some(out))
}

/// RFC 9559 section 16: independent subtitle frames are indexed individually,
/// with durations recommended. Use only a track-complete, directly addressed
/// index for every requested independent subtitle track; missing/invalid references fall
/// back to the full scanner. Referenced/stateful blocks require that full pass.
pub(super) fn visit_subtitle_blocks(
    source: &MediaSource,
    segment: FileElement,
    scale: u64,
    tracks: &[u64],
    mut consume: impl FnMut(u64, FileElement) -> Result<()>,
) -> Result<bool> {
    let Some(index) = load(source, segment, scale) else {
        return Ok(false);
    };
    let Some(payload) = find_first_child(&index.bytes, 0x1c53_bb6b) else {
        return Ok(false);
    };
    let mut counts = vec![0usize; tracks.len()];
    let mut addresses = BTreeMap::new();
    for cue in ElementIter::new(payload).filter(|e| e.id == 0xbb) {
        let Some(time) = find_first_child(cue.payload, 0xb3).and_then(read_uint) else {
            return Ok(false);
        };
        for entry in ElementIter::new(cue.payload).filter(|e| e.id == 0xb7) {
            let Some(number) = find_first_child(entry.payload, 0xf7).and_then(read_uint) else {
                return Ok(false);
            };
            let Some(track) = tracks.iter().position(|n| *n == number) else {
                continue;
            };
            let Some(position) = find_first_child(entry.payload, 0xf1).and_then(read_uint) else {
                return Ok(false);
            };
            let Some(relative) = find_first_child(entry.payload, 0xf0).and_then(read_uint) else {
                return Ok(false);
            };
            // Older muxers omit CueDuration; the actual BlockDuration is
            // retained by the existing subtitle parser, not invented here.
            if find_first_child(entry.payload, 0xea)
                .and_then(read_uint)
                .is_some_and(|v| v != 0)
            {
                return Ok(false);
            }
            let Some(position) = segment
                .payload
                .checked_add(position)
                .filter(|p| *p < segment.end)
            else {
                return Ok(false);
            };
            source.policy().check(
                "Matroska subtitle cue index",
                addresses.len().saturating_add(1).saturating_mul(128),
                source.policy().index_bytes,
            )?;
            if addresses
                .insert((position, relative), (number, time))
                .is_some()
            {
                return Ok(false);
            }
            counts[track] += 1;
            if addresses.len() > 100000 {
                return Ok(false);
            }
        }
    }
    if counts.contains(&0) {
        return Ok(false);
    }
    let mut current = None;
    let mut visits = 0;
    for ((position, relative), (number, time)) in addresses {
        if current
            .as_ref()
            .is_none_or(|(offset, _)| *offset != position)
        {
            let element = source.element_at(position, segment.end)?;
            if element.id != 0x1f43_b675 || element.unknown_size {
                return Ok(false);
            }
            current = Some((position, read_cluster(source, element, scale, &mut visits)?));
        }
        let cluster = &current.as_ref().unwrap().1;
        let Some(offset) = cluster
            .payload
            .checked_add(relative)
            .filter(|p| *p < cluster.end)
        else {
            return Ok(false);
        };
        let element = source.element_at(offset, cluster.end)?;
        source.prefetch_metadata_span(offset, usize::try_from(element.end - offset)?)?;
        let block = match element.id {
            0xa3 => Some(read_block(source, &element, None)?),
            0xa0 => read_group(source, &element, &mut visits)?,
            _ => None,
        };
        let Some(block) = block else { return Ok(false) };
        let actual = i128::from(cluster.timecode) + i128::from(block.relative_timecode);
        if std::env::var_os("CHROMA_PROBE_DIAGNOSTICS").is_some() && !block.keyframe {
            eprintln!("subtitle_cue_read fallback=stateful_block track={number}");
        }
        if block.track_number != number
            || block.frames.len() != 1
            || !block.keyframe
            || actual != i128::from(time)
        {
            return Ok(false);
        }
        consume(u64::try_from(cluster.timecode)?, element)?;
    }
    source.validate_current()?;
    Ok(true)
}
