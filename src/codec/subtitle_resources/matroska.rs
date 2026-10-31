//! Subtitle-specific positional scan retains BlockDuration and WebVTT additions.
//! Container mapping: <https://www.matroska.org/technical/subtitles.html>.
use crate::{
    container::matroska::MatroskaTrack,
    source::{MediaSource, probe_reader::Element},
};
use anyhow::{Context, Result, bail};
use std::collections::HashMap;

pub(super) struct Cue {
    pub start_ms: u64,
    pub duration_ms: u64,
    pub text: String,
    pub addition: Option<String>,
}

pub(super) fn cues(
    source: &MediaSource,
    tracks: &[&MatroskaTrack],
) -> Result<HashMap<u64, Vec<Cue>>> {
    source.validate_current()?;
    let compression = super::bitmap::compression::parse(source)?;
    let mut offset = 0;
    let mut visits = 0;
    let segment = loop {
        visit(source, &mut visits)?;
        let element = source.element_at(offset, source.len())?;
        if element.id == 0x1853_8067 {
            break element;
        }
        offset = element.end;
        if offset >= source.len() {
            bail!("missing Matroska segment");
        }
    };
    let mut scale = 1_000_000_u64;
    offset = segment.payload;
    while offset < segment.end {
        visit(source, &mut visits)?;
        let element = source.element_at(offset, segment.end)?;
        if element.id == 0x1549_a966 {
            let mut field_offset = element.payload;
            while field_offset < element.end {
                visit(source, &mut visits)?;
                let field = source.element_at(field_offset, element.end)?;
                if field.id == 0x002a_d7b1 {
                    scale = uint(source, field)?;
                }
                field_offset = field.end;
            }
            break;
        }
        offset = element.end;
    }
    let mut output: HashMap<u64, Vec<Cue>> = HashMap::new();
    let mut sizes: HashMap<u64, usize> = HashMap::new();
    let mut total = 0_usize;
    let mut invalid_tracks = std::collections::HashSet::new();
    let mut process_child = |input: Option<(u64, Element)>, visits: &mut usize| -> Result<()> {
        let Some((cluster_time, child)) = input else {
            output.clear();
            sizes.clear();
            total = 0;
            invalid_tracks.clear();
            return Ok(());
        };
        let mut block = None;
        let mut duration = None;
        let mut addition = None;
        if child.id == 0xa3 {
            block = Some(child);
        }
        if child.id == 0xa0 {
            let mut field_offset = child.payload;
            while field_offset < child.end {
                visit(source, visits)?;
                let field = source.element_at(field_offset, child.end)?;
                match field.id {
                    0xa1 => {
                        let len = usize::try_from(field.end - field.payload)?;
                        let prefix = source.read_metadata_prefix(field.payload, len.min(12))?;
                        let (number, _) = vint(&prefix).context("invalid subtitle block track")?;
                        if !tracks.iter().any(|track| track.number == number) {
                            return Ok(());
                        }
                        block = Some(field);
                    }
                    0x9b => duration = Some(uint(source, field)?),
                    0x75a1 => addition = Some(field),
                    _ => {}
                }
                field_offset = field.end;
            }
        }
        if let Some(block) = block {
            let len = usize::try_from(block.end - block.payload)?;
            let prefix = source.read_metadata_prefix(block.payload, len.min(12))?;
            let (number, track_len) = vint(&prefix).context("invalid subtitle block track")?;
            if let Some(track) = tracks.iter().find(|track| track.number == number) {
                if len < track_len + 3 || prefix.len() < track_len + 3 {
                    bail!("truncated subtitle block");
                }
                if prefix[track_len + 2] & 0x06 != 0 {
                    if invalid_tracks.insert(number)
                        && std::env::var_os("CHROMA_PROBE_DIAGNOSTICS").is_some()
                    {
                        eprintln!(
                            "subtitle_track_rejected track={number} reason=unsupported_or_invalid_cue"
                        );
                    }
                    return Ok(());
                }
                let relative = i16::from_be_bytes([prefix[track_len], prefix[track_len + 1]]);
                let ticks = i128::from(cluster_time) + i128::from(relative);
                let start_ms = u64::try_from(ticks.max(0))?.saturating_mul(scale) / 1_000_000;
                let duration_ms = duration
                    .map(|ticks| ticks.saturating_mul(scale) / 1_000_000)
                    .or(track.default_duration_ns.map(|ns| ns / 1_000_000));
                if duration_ms == Some(0) {
                    // Explicit zero-duration ASS events have no display time;
                    // they must not invalidate all visible cues in the track.
                    return Ok(());
                }
                let Some(duration_ms) = duration_ms.filter(|duration| *duration > 0) else {
                    if invalid_tracks.insert(number)
                        && std::env::var_os("CHROMA_PROBE_DIAGNOSTICS").is_some()
                    {
                        eprintln!(
                            "subtitle_track_rejected track={number} reason=unsupported_or_invalid_cue"
                        );
                    }
                    return Ok(());
                };
                let payload_len = len - track_len - 3;
                let size = sizes.entry(number).or_default();
                *size = size
                    .checked_add(payload_len)
                    .context("subtitle size overflow")?;
                total = total
                    .checked_add(payload_len + 128)
                    .context("subtitle aggregate overflow")?;
                if *size > super::SCRIPT_LIMIT
                    || total > super::AGGREGATE_LIMIT.min(source.policy().index_bytes)
                {
                    bail!(
                        "subtitle resource size limit exceeded: track={number}, script_bytes={size}, aggregate_bytes={total}"
                    );
                }
                let bytes = source
                    .read_window_unvalidated(block.payload + (track_len + 3) as u64, payload_len)?;
                let bytes = if let Some(encoding) = compression.get(&number) {
                    match encoding.decode(&bytes, 1, super::SCRIPT_LIMIT) {
                        Ok(decoded) => decoded,
                        Err(_) => {
                            invalid_tracks.insert(number);
                            return Ok(());
                        }
                    }
                } else {
                    bytes
                };
                let extra = bytes.len().saturating_sub(payload_len);
                *size = size
                    .checked_add(extra)
                    .context("expanded subtitle size overflow")?;
                total = total
                    .checked_add(extra)
                    .context("expanded subtitle aggregate overflow")?;
                if *size > super::SCRIPT_LIMIT
                    || total > super::AGGREGATE_LIMIT.min(source.policy().index_bytes)
                {
                    bail!("expanded subtitle resource size limit exceeded");
                }
                let Ok(text) = String::from_utf8(bytes) else {
                    if invalid_tracks.insert(number)
                        && std::env::var_os("CHROMA_PROBE_DIAGNOSTICS").is_some()
                    {
                        eprintln!(
                            "subtitle_track_rejected track={number} reason=unsupported_or_invalid_cue"
                        );
                    }
                    return Ok(());
                };
                let addition = addition
                    .map(|element| read_addition(source, element, visits))
                    .transpose()?
                    .flatten();
                let addition = if let Some(bytes) = addition {
                    let Ok(text) = String::from_utf8(bytes) else {
                        invalid_tracks.insert(number);
                        return Ok(());
                    };
                    Some(text)
                } else {
                    None
                };
                if let Some(ref addition) = addition {
                    *size = size
                        .checked_add(addition.len())
                        .context("subtitle addition size overflow")?;
                    total = total
                        .checked_add(addition.len())
                        .context("subtitle addition aggregate overflow")?;
                    if *size > super::SCRIPT_LIMIT
                        || total > super::AGGREGATE_LIMIT.min(source.policy().index_bytes)
                    {
                        bail!("subtitle additions size limit exceeded");
                    }
                }
                let cues = output.entry(number).or_default();
                if cues.len() >= 100_000 {
                    bail!("subtitle cue count limit exceeded");
                }
                cues.push(Cue {
                    start_ms,
                    duration_ms,
                    text,
                    addition,
                });
            }
        }
        Ok(())
    };
    let numbers = tracks.iter().map(|t| t.number).collect::<Vec<_>>();
    let indexed = crate::container::matroska::visit_indexed_subtitle_blocks(
        source,
        segment,
        scale,
        &numbers,
        |clock, child| process_child(Some((clock, child)), &mut visits),
    )?;
    if std::env::var_os("CHROMA_PROBE_DIAGNOSTICS").is_some() {
        eprintln!("subtitle_cue_read indexed={indexed} single_pass=true");
    }
    if !indexed {
        process_child(None, &mut visits)?;
        offset = segment.payload;
        while offset < segment.end {
            visit(source, &mut visits)?;
            let element = source.element_at(offset, segment.end)?;
            if element.id == 0x1f43_b675 {
                let mut child_offset = element.payload;
                let mut cluster_time = None;
                while child_offset < element.end {
                    visit(source, &mut visits)?;
                    let child = source.element_at(child_offset, element.end)?;
                    if child.id == 0xe7 {
                        cluster_time = Some(uint(source, child)?);
                        break;
                    }
                    child_offset = child.end;
                }
                let cluster_time = cluster_time.context("missing subtitle cluster time")?;
                child_offset = element.payload;
                while child_offset < element.end {
                    visit(source, &mut visits)?;
                    let child = source.element_at(child_offset, element.end)?;
                    process_child(Some((cluster_time, child)), &mut visits)?;
                    child_offset = child.end;
                }
            }
            offset = element.end;
        }
    }
    source.validate_current()?;
    output.retain(|number, _| !invalid_tracks.contains(number));
    Ok(output)
}

fn read_addition(
    source: &MediaSource,
    additions: Element,
    visits: &mut usize,
) -> Result<Option<Vec<u8>>> {
    let mut offset = additions.payload;
    while offset < additions.end {
        visit(source, visits)?;
        let more = source.element_at(offset, additions.end)?;
        if more.id == 0xa6 {
            let mut field_offset = more.payload;
            let mut id = 1;
            let mut data = None;
            while field_offset < more.end {
                visit(source, visits)?;
                let field = source.element_at(field_offset, more.end)?;
                match field.id {
                    0xee => id = uint(source, field)?,
                    0xa5 => data = Some(field),
                    _ => {}
                }
                field_offset = field.end;
            }
            if id == 1
                && let Some(data) = data
            {
                let len = usize::try_from(data.end - data.payload)?;
                if len > 64 * 1024 {
                    bail!("WebVTT cue additions limit exceeded");
                }
                return Ok(Some(source.read_window_unvalidated(data.payload, len)?));
            }
        }
        offset = more.end;
    }
    Ok(None)
}
fn visit(_source: &MediaSource, visits: &mut usize) -> Result<()> {
    *visits += 1;
    if *visits > 2_000_000 {
        bail!("subtitle scan work budget exceeded");
    }
    Ok(())
}
fn uint(source: &MediaSource, element: Element) -> Result<u64> {
    let len = usize::try_from(element.end - element.payload)?;
    if len == 0 || len > 8 {
        bail!("invalid subtitle metadata integer");
    }
    let bytes = source.read_metadata_prefix(element.payload, len)?;
    Ok(bytes
        .into_iter()
        .fold(0_u64, |value, byte| value << 8 | u64::from(byte)))
}
fn vint(bytes: &[u8]) -> Option<(u64, usize)> {
    let first = *bytes.first()?;
    let len = first.leading_zeros() as usize + 1;
    if len > 8 || bytes.len() < len {
        return None;
    }
    let mut value = u64::from(first & (0xff_u8 >> len));
    for byte in &bytes[1..len] {
        value = value << 8 | u64::from(*byte);
    }
    Some((value, len))
}
