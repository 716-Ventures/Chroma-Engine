//! Recover only validated single-layer profile 5; never infer HDR from a filename.
use super::*;
use dolby_vision::rpu::dovi_rpu::DoviRpu;
use std::io;

pub(crate) fn recover(
    metadata: &[u8],
    mut read: impl FnMut(u64, usize) -> io::Result<Vec<u8>>,
    control: &crate::WorkControl,
) -> anyhow::Result<Option<Vec<u8>>> {
    let Some(moov) = find_atom(metadata, b"moov") else {
        return Ok(None);
    };
    let mut remaining = 2 * 1024 * 1024;
    for (ordinal, trak) in AtomIter::new(moov)
        .filter(|a| a.kind == *b"trak")
        .take(4)
        .enumerate()
    {
        control.check()?;
        if parse_trak_kind(trak.payload) != Some(Mp4TrackKind::Video) {
            continue;
        }
        let Some(config) = parse_trak_codec_config(trak.payload, Mp4TrackKind::Video, "v0".into())
        else {
            continue;
        };
        let Some(entry) = config.source_video_sample_entry.as_ref() else {
            continue;
        };
        if !matches!(&entry.codec_fourcc, b"hvc1" | b"hev1")
            || parse_video_sample_entry_dynamic_range(entry.codec_fourcc, &entry.payload)
                != Mp4DynamicRange::Unknown
            || parse_video_sample_entry_pixel_format(entry.codec_fourcc, &entry.payload).as_deref()
                != Some("yuv420-10bit")
        {
            continue;
        }
        let Some(level) = recovery_level(entry, config.maximum_frame_rate) else {
            continue;
        };
        let Some(mdia) = find_atom(trak.payload, b"mdia") else {
            continue;
        };
        let Some(stbl) = find_atom(mdia, b"minf").and_then(|b| find_atom(b, b"stbl")) else {
            continue;
        };
        let Some(stsd) = find_atom(stbl, b"stsd") else {
            continue;
        };
        // Switching sample descriptions cannot be represented by a single repair.
        if stsd.get(4..8).and_then(read_u32) != Some(1) {
            continue;
        }
        let Some((offset, sizes)) = first_samples(stbl) else {
            continue;
        };
        let Some(length_size @ 1..=4) = config.nalu_length_size else {
            continue;
        };
        let mut offset = offset;
        let mut confirmed = false;
        let mut rejected = false;
        for size in sizes {
            control.check()?;
            if size == 0 || size > remaining {
                break;
            }
            remaining -= size;
            let packet = read(offset, size)?;
            offset = offset
                .checked_add(size as u64)
                .ok_or_else(|| anyhow::anyhow!("packet offset overflow"))?;
            match profile5_packet(&packet, length_size as usize) {
                Some(found) => confirmed |= found,
                None => {
                    rejected = true;
                    break;
                }
            }
        }
        if !confirmed || rejected {
            continue;
        }
        let mut payload = entry.payload.clone();
        let mut record = [0u8; 24];
        record[0] = 1;
        record[2] = 5 << 1;
        record[3] = (level << 3) | 5; // RPU + base layer, no enhancement layer.
        payload.extend_from_slice(&32u32.to_be_bytes());
        payload.extend_from_slice(b"dvcC");
        payload.extend_from_slice(&record);
        let fourcc = if entry.codec_fourcc == *b"hvc1" {
            *b"dvh1"
        } else {
            *b"dvhe"
        };
        let mut repaired_stsd = stsd[..8].to_vec();
        repaired_stsd.extend(box_bytes(fourcc, &payload));
        let repaired_trak = replace_path(
            trak.payload,
            &[*b"mdia", *b"minf", *b"stbl", *b"stsd"],
            &repaired_stsd,
        );
        let repaired_moov = replace_nth(moov, *b"trak", ordinal, &repaired_trak);
        return Ok(Some(replace_nth(metadata, *b"moov", 0, &repaired_moov)));
    }
    Ok(None)
}

fn first_samples(stbl: &[u8]) -> Option<(u64, Vec<usize>)> {
    let offset = if let Some(co64) = find_atom(stbl, b"co64") {
        (co64.get(4..8).and_then(read_u32)? > 0).then_some(())?;
        read_u64(co64.get(8..16)?)?
    } else {
        let stco = find_atom(stbl, b"stco")?;
        (stco.get(4..8).and_then(read_u32)? > 0).then_some(())?;
        u64::from(read_u32(stco.get(8..12)?)?)
    };
    let stsc = find_atom(stbl, b"stsc")?;
    (read_u32(stsc.get(8..12)?)? == 1 && read_u32(stsc.get(16..20)?)? == 1).then_some(())?;
    let count = read_u32(stsc.get(12..16)?)?.min(2);
    let stsz = find_atom(stbl, b"stsz")?;
    let fixed = read_u32(stsz.get(4..8)?)?;
    let count = count.min(read_u32(stsz.get(8..12)?)?);
    let sizes = (0..count)
        .map(|i| {
            if fixed != 0 {
                Some(fixed as usize)
            } else {
                let at = 12 + i as usize * 4;
                Some(read_u32(stsz.get(at..at + 4)?)? as usize)
            }
        })
        .collect::<Option<Vec<_>>>()?;
    Some((offset, sizes))
}

fn profile5_packet(packet: &[u8], length: usize) -> Option<bool> {
    let mut at = 0usize;
    let mut found = false;
    let mut count = 0;
    while at < packet.len() {
        count += 1;
        if count > 1024 {
            return None;
        }
        let n = packet
            .get(at..at.checked_add(length)?)?
            .iter()
            .fold(0usize, |v, b| (v << 8) | usize::from(*b));
        at += length;
        if n < 2 {
            return None;
        }
        let nal = packet.get(at..at.checked_add(n)?)?;
        at += n;
        if nal[0] & 0x80 != 0 || nal[1] & 7 == 0 {
            return None;
        }
        match (nal[0] >> 1) & 63 {
            62 => {
                if n > 16 * 1024 {
                    return None;
                }
                let rpu = DoviRpu::parse_unspec62_nalu(nal).ok()?;
                if rpu.dovi_profile != 5 {
                    return None;
                }
                found = true;
            }
            63 => return None,
            _ => (),
        }
    }
    Some(found)
}

fn recovery_level(entry: &Mp4SourceVideoSampleEntry, fps: Option<f64>) -> Option<u8> {
    let width = read_u16(entry.payload.get(24..26)?)? as u64;
    let height = read_u16(entry.payload.get(26..28)?)? as u64;
    let fps = fps.filter(|f| f.is_finite() && *f > 0.0)?;
    if width == 0 || height == 0 {
        return None;
    }
    let children =
        sample_entry_child_boxes(Mp4TrackKind::Video, entry.codec_fourcc, &entry.payload)?;
    let hvcc = find_atom(children, b"hvcC")?;
    let high_tier = hvcc.get(1)? & 0x20 != 0;
    let bitrate = read_u32(find_atom(children, b"btrt")?.get(4..8)?)?;
    if bitrate == 0 {
        return None;
    }
    // Dolby's published width, pixel-rate and main/high-tier bitrate limits.
    [
        (1, 1280, 22_118_400u64),
        (2, 1280, 27_648_000),
        (3, 1920, 49_766_400),
        (4, 2560, 62_208_000),
        (5, 3840, 124_416_000),
        (6, 3840, 199_065_600),
        (7, 3840, 248_832_000),
    ]
    .into_iter()
    .find_map(|(level, max_width, pps)| {
        (width <= max_width
            && width as f64 * height as f64 * fps <= pps as f64 * 1.000001
            && bitrate
                <= if high_tier {
                    if level <= 2 {
                        50_000_000
                    } else if level <= 5 {
                        70_000_000
                    } else {
                        130_000_000
                    }
                } else if level <= 5 {
                    20_000_000
                } else {
                    25_000_000
                })
        .then_some(level)
    })
}

fn box_bytes(kind: [u8; 4], payload: &[u8]) -> Vec<u8> {
    let mut out = Vec::with_capacity(payload.len() + 8);
    out.extend_from_slice(&((payload.len() + 8) as u32).to_be_bytes());
    out.extend_from_slice(&kind);
    out.extend_from_slice(payload);
    out
}
fn replace_nth(bytes: &[u8], kind: [u8; 4], nth: usize, payload: &[u8]) -> Vec<u8> {
    let mut ordinal = 0;
    let mut out = Vec::new();
    for atom in AtomIter::new(bytes) {
        let chosen = atom.kind == kind && ordinal == nth;
        if atom.kind == kind {
            ordinal += 1;
        }
        out.extend(box_bytes(
            atom.kind,
            if chosen { payload } else { atom.payload },
        ));
    }
    out
}
fn replace_path(bytes: &[u8], path: &[[u8; 4]], payload: &[u8]) -> Vec<u8> {
    if path.len() == 1 {
        return replace_nth(bytes, path[0], 0, payload);
    }
    let child = find_atom(bytes, &path[0]).expect("validated sample-description path");
    replace_nth(bytes, path[0], 0, &replace_path(child, &path[1..], payload))
}

#[cfg(test)]
mod tests {
    use super::*;
    fn packet(profile5: bool) -> Vec<u8> {
        let config = dolby_vision::rpu::generate::GenerateConfig::default();
        let rpu = if profile5 {
            DoviRpu::profile5_config(&config)
        } else {
            DoviRpu::profile81_config(&config)
        }
        .unwrap();
        let nal = rpu.write_hevc_unspec62_nalu().unwrap();
        [(nal.len() as u32).to_be_bytes().as_slice(), &nal].concat()
    }
    #[test]
    fn requires_fully_valid_profile5_not_just_rpu_nal_type() {
        assert_eq!(profile5_packet(&packet(true), 4), Some(true));
        assert_eq!(profile5_packet(&packet(false), 4), None);
        assert_eq!(profile5_packet(&[0, 0, 0, 2, 2, 1], 4), Some(false));
        let mut corrupt = packet(true);
        let last = corrupt.len() - 3;
        corrupt[last] ^= 1;
        assert_eq!(profile5_packet(&corrupt, 4), None);
        assert_eq!(profile5_packet(&[0, 0, 255, 255, 124, 1], 4), None);
    }
    #[test]
    fn refuses_enhancement_nals_and_truncated_lengths() {
        let mut p = packet(true);
        p.extend_from_slice(&[0, 0, 0, 2, 126, 1]);
        assert_eq!(profile5_packet(&p, 4), None);
        assert_eq!(profile5_packet(&[0, 0, 0], 4), None);
    }
    #[test]
    fn level_uses_source_geometry_and_timing() {
        let mut entry = Mp4SourceVideoSampleEntry {
            codec_fourcc: *b"hvc1",
            payload: vec![0; 78],
        };
        entry.payload[24..26].copy_from_slice(&3840u16.to_be_bytes());
        entry.payload[26..28].copy_from_slice(&1608u16.to_be_bytes());
        let mut hvcc = vec![0; 23];
        hvcc[1] = 0x22;
        entry.payload.extend(box_bytes(*b"hvcC", &hvcc));
        entry
            .payload
            .extend(box_bytes(*b"btrt", &[0, 0, 0, 0, 1, 0, 0, 0, 1, 0, 0, 0]));
        assert_eq!(recovery_level(&entry, Some(24.0000012)), Some(6));
        assert_eq!(recovery_level(&entry, None), None);
        assert_eq!(recovery_level(&entry, Some(120.0)), None);
    }
}
