use anyhow::{Result, bail};

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Fmp4MediaValidation {
    pub(crate) track_count: usize,
    pub(crate) sample_count: usize,
    pub(crate) payload_bytes: usize,
}

pub(crate) fn validate_media_fragment(bytes: &[u8]) -> Result<Fmp4MediaValidation> {
    let moof = top_level_box_payload(bytes, b"moof")
        .ok_or_else(|| anyhow::anyhow!("fMP4 media fragment is missing moof"))?;
    let mdat_offset = find_top_level_box(bytes, b"mdat")
        .ok_or_else(|| anyhow::anyhow!("fMP4 media fragment is missing mdat"))?;
    let mdat = box_payload_at(bytes, mdat_offset)
        .ok_or_else(|| anyhow::anyhow!("fMP4 media fragment has malformed mdat"))?;
    let mdat_payload_start = mdat_offset
        .checked_add(8)
        .ok_or_else(|| anyhow::anyhow!("fMP4 media fragment mdat offset overflowed"))?;
    let mdat_payload_end = mdat_payload_start
        .checked_add(mdat.len())
        .ok_or_else(|| anyhow::anyhow!("fMP4 media fragment mdat length overflowed"))?;

    let trafs = child_box_payloads(moof, b"traf");
    if trafs.is_empty() {
        bail!("fMP4 media fragment moof does not contain any traf boxes");
    }

    let mut sample_count = 0_usize;
    let mut payload_bytes = 0_usize;
    let mut previous_data_end = mdat_payload_start;
    for traf in &trafs {
        let tfdt = child_box_payload(traf, b"tfdt")
            .ok_or_else(|| anyhow::anyhow!("fMP4 media fragment traf is missing tfdt"))?;
        let base_decode_time = tfdt_base_decode_time(tfdt)
            .ok_or_else(|| anyhow::anyhow!("fMP4 media fragment has malformed tfdt"))?;
        let trun = child_box_payload(traf, b"trun")
            .ok_or_else(|| anyhow::anyhow!("fMP4 media fragment traf is missing trun"))?;
        let inspected = inspect_trun(trun)
            .ok_or_else(|| anyhow::anyhow!("fMP4 media fragment has malformed trun"))?;
        if inspected.samples.is_empty() {
            bail!("fMP4 media fragment trun must contain at least one sample");
        }
        if inspected.samples[0].flags & 0x0100_0000 != 0 {
            bail!("fMP4 media fragment track starts with a non-sync sample");
        }
        let data_offset = usize::try_from(inspected.data_offset)
            .map_err(|_| anyhow::anyhow!("fMP4 media fragment has negative trun data offset"))?;
        let (track_payload_bytes, _) = inspected.samples.iter().try_fold(
            (0_usize, base_decode_time),
            |(sum, decode_time), sample| {
                if sample.duration == 0 {
                    bail!("fMP4 media fragment contains a zero-duration sample");
                }
                if sample.size == 0 {
                    bail!("fMP4 media fragment contains a zero-size sample");
                }
                if sample.composition_time_offset < 0 {
                    bail!("fMP4 media fragment contains a negative composition offset");
                }
                let next_decode_time = decode_time
                    .checked_add(u64::from(sample.duration))
                    .ok_or_else(|| anyhow::anyhow!("fMP4 media fragment decode time overflowed"))?;
                let next_sum = sum.checked_add(sample.size as usize).ok_or_else(|| {
                    anyhow::anyhow!("fMP4 media fragment sample bytes overflowed")
                })?;
                Ok::<_, anyhow::Error>((next_sum, next_decode_time))
            },
        )?;
        let data_end = data_offset
            .checked_add(track_payload_bytes)
            .ok_or_else(|| anyhow::anyhow!("fMP4 media fragment data offset overflowed"))?;
        if data_offset < mdat_payload_start || data_end > mdat_payload_end {
            bail!("fMP4 media fragment trun data offset points outside mdat payload");
        }
        if data_offset < previous_data_end {
            bail!("fMP4 media fragment track payload ranges overlap or move backwards");
        }
        previous_data_end = data_end;
        sample_count = sample_count.saturating_add(inspected.samples.len());
        payload_bytes = payload_bytes.saturating_add(track_payload_bytes);
    }

    Ok(Fmp4MediaValidation {
        track_count: trafs.len(),
        sample_count,
        payload_bytes,
    })
}

fn find_top_level_box(bytes: &[u8], name: &[u8; 4]) -> Option<usize> {
    let mut offset = 0_usize;
    while offset.checked_add(8)? <= bytes.len() {
        let size = u32::from_be_bytes(bytes[offset..offset + 4].try_into().ok()?) as usize;
        if size < 8 || offset.checked_add(size)? > bytes.len() {
            return None;
        }
        if &bytes[offset + 4..offset + 8] == name {
            return Some(offset);
        }
        offset = offset.checked_add(size)?;
    }
    None
}

fn top_level_box_payload<'a>(bytes: &'a [u8], name: &[u8; 4]) -> Option<&'a [u8]> {
    let offset = find_top_level_box(bytes, name)?;
    box_payload_at(bytes, offset)
}

fn child_box_payload<'a>(bytes: &'a [u8], name: &[u8; 4]) -> Option<&'a [u8]> {
    let mut offset = 0_usize;
    while offset.checked_add(8)? <= bytes.len() {
        let size = u32::from_be_bytes(bytes[offset..offset + 4].try_into().ok()?) as usize;
        if size < 8 || offset.checked_add(size)? > bytes.len() {
            return None;
        }
        if &bytes[offset + 4..offset + 8] == name {
            return Some(&bytes[offset + 8..offset + size]);
        }
        offset = offset.checked_add(size)?;
    }
    None
}

fn child_box_payloads<'a>(bytes: &'a [u8], name: &[u8; 4]) -> Vec<&'a [u8]> {
    let mut offset = 0_usize;
    let mut out = Vec::new();
    while offset + 8 <= bytes.len() {
        let Some(size) = bytes
            .get(offset..offset + 4)
            .and_then(|raw| raw.try_into().ok())
            .map(u32::from_be_bytes)
            .map(|size| size as usize)
        else {
            return Vec::new();
        };
        if size < 8 || offset.saturating_add(size) > bytes.len() {
            return Vec::new();
        }
        if &bytes[offset + 4..offset + 8] == name {
            out.push(&bytes[offset + 8..offset + size]);
        }
        offset += size;
    }
    out
}

fn box_payload_at(bytes: &[u8], offset: usize) -> Option<&[u8]> {
    let size = u32::from_be_bytes(bytes.get(offset..offset + 4)?.try_into().ok()?) as usize;
    if size < 8 || offset.checked_add(size)? > bytes.len() {
        return None;
    }
    Some(&bytes[offset + 8..offset + size])
}

fn tfdt_base_decode_time(payload: &[u8]) -> Option<u64> {
    if payload.len() < 12 || payload[0] != 1 {
        return None;
    }
    Some(u64::from_be_bytes(payload[4..12].try_into().ok()?))
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct InspectedTrun {
    data_offset: i32,
    samples: Vec<InspectedSample>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct InspectedSample {
    duration: u32,
    size: u32,
    flags: u32,
    composition_time_offset: i32,
}

fn inspect_trun(payload: &[u8]) -> Option<InspectedTrun> {
    if payload.len() < 12 || payload[0] != 1 {
        return None;
    }
    let flags = u32::from_be_bytes([0, payload[1], payload[2], payload[3]]);
    if flags != 0x000f01 {
        return None;
    }
    let sample_count = u32::from_be_bytes(payload[4..8].try_into().ok()?) as usize;
    let data_offset = i32::from_be_bytes(payload[8..12].try_into().ok()?);
    let mut offset = 12_usize;
    let mut samples = Vec::with_capacity(sample_count);
    for _ in 0..sample_count {
        let end = offset.checked_add(16)?;
        if end > payload.len() {
            return None;
        }
        samples.push(InspectedSample {
            duration: u32::from_be_bytes(payload[offset..offset + 4].try_into().ok()?),
            size: u32::from_be_bytes(payload[offset + 4..offset + 8].try_into().ok()?),
            flags: u32::from_be_bytes(payload[offset + 8..offset + 12].try_into().ok()?),
            composition_time_offset: i32::from_be_bytes(
                payload[offset + 12..offset + 16].try_into().ok()?,
            ),
        });
        offset = end;
    }
    (offset == payload.len()).then_some(InspectedTrun {
        data_offset,
        samples,
    })
}
