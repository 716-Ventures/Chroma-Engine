use crate::packet::ChunkSample;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HevcDecoderConfig {
    pub general_profile_idc: u8,
    pub general_level_idc: u8,
    pub nalu_length_size: u8,
    pub arrays: Vec<HevcNalArray>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HevcNalArray {
    pub nal_unit_type: u8,
    pub units: Vec<Vec<u8>>,
}

#[derive(Debug, thiserror::Error, PartialEq, Eq)]
pub enum HevcParseError {
    #[error("HEVC decoder configuration record is truncated")]
    TruncatedConfig,
    #[error("HEVC NAL length size must be between 1 and 4 bytes")]
    InvalidLengthSize,
    #[error("HEVC sample has a truncated NAL length prefix")]
    TruncatedLengthPrefix,
    #[error("HEVC sample has a truncated NAL unit")]
    TruncatedNalUnit,
    #[error("HEVC Annex B output exceeds the resource limit")]
    ResourceLimit,
}

pub fn parse_hevc_decoder_config(payload: &[u8]) -> Result<HevcDecoderConfig, HevcParseError> {
    if payload.len() < 23 {
        return Err(HevcParseError::TruncatedConfig);
    }
    let nalu_length_size = (payload[21] & 0x03) + 1;
    if !(1..=4).contains(&nalu_length_size) {
        return Err(HevcParseError::InvalidLengthSize);
    }
    let array_count = payload[22] as usize;
    let mut offset = 23_usize;
    if array_count > payload.len().saturating_sub(offset) / 3 {
        return Err(HevcParseError::TruncatedConfig);
    }
    let mut unit_total = 0usize;
    let mut arrays = Vec::with_capacity(array_count);
    for _ in 0..array_count {
        let header = *payload.get(offset).ok_or(HevcParseError::TruncatedConfig)?;
        offset += 1;
        let unit_count = read_u16(payload, offset)? as usize;
        offset += 2;
        // Every unit needs at least its 16-bit size, even an empty NAL.
        if unit_count > payload.len().saturating_sub(offset) / 2 {
            return Err(HevcParseError::TruncatedConfig);
        }
        // Empty NALs still allocate Vec metadata. Bound their aggregate count
        // independently of the serialized byte geometry (up to 12x expansion).
        unit_total = unit_total
            .checked_add(unit_count)
            .filter(|total| *total <= 4_096)
            .ok_or(HevcParseError::ResourceLimit)?;
        let mut units = Vec::with_capacity(unit_count);
        for _ in 0..unit_count {
            let len = read_u16(payload, offset)? as usize;
            offset += 2;
            let end = offset
                .checked_add(len)
                .ok_or(HevcParseError::TruncatedConfig)?;
            if end > payload.len() {
                return Err(HevcParseError::TruncatedConfig);
            }
            units.push(payload[offset..end].to_vec());
            offset = end;
        }
        arrays.push(HevcNalArray {
            nal_unit_type: header & 0x3f,
            units,
        });
    }
    Ok(HevcDecoderConfig {
        general_profile_idc: payload[1] & 0x1f,
        general_level_idc: payload[12],
        nalu_length_size,
        arrays,
    })
}

pub fn hevc_sample_to_annex_b(
    sample: &[u8],
    nalu_length_size: u8,
) -> Result<Vec<u8>, HevcParseError> {
    if !(1..=4).contains(&nalu_length_size) {
        return Err(HevcParseError::InvalidLengthSize);
    }
    let mut out = Vec::new();
    append_hevc_annex_b(&mut out, sample, nalu_length_size)?;
    Ok(out)
}

const MAX_ANNEX_B_BYTES: usize = 64 * 1024 * 1024;

fn append_hevc_annex_b(
    out: &mut Vec<u8>,
    sample: &[u8],
    nalu_length_size: u8,
) -> Result<(), HevcParseError> {
    let mut offset = 0_usize;
    while offset < sample.len() {
        let len_end = offset
            .checked_add(nalu_length_size as usize)
            .ok_or(HevcParseError::TruncatedLengthPrefix)?;
        if len_end > sample.len() {
            return Err(HevcParseError::TruncatedLengthPrefix);
        }
        let mut len = 0_usize;
        for byte in &sample[offset..len_end] {
            len = (len << 8) | usize::from(*byte);
        }
        offset = len_end;
        let nal_end = offset
            .checked_add(len)
            .ok_or(HevcParseError::TruncatedNalUnit)?;
        if nal_end > sample.len() {
            return Err(HevcParseError::TruncatedNalUnit);
        }
        let extra = len.checked_add(4).ok_or(HevcParseError::ResourceLimit)?;
        if out
            .len()
            .checked_add(extra)
            .is_none_or(|size| size > MAX_ANNEX_B_BYTES)
        {
            return Err(HevcParseError::ResourceLimit);
        }
        out.try_reserve(extra)
            .map_err(|_| HevcParseError::ResourceLimit)?;
        out.extend_from_slice(&[0, 0, 0, 1]);
        out.extend_from_slice(&sample[offset..nal_end]);
        offset = nal_end;
    }
    Ok(())
}

/// First coded-picture NAL type in a length-prefixed access unit.
pub(crate) fn sample_vcl_type(
    sample: &[u8],
    length_size: u8,
) -> Result<Option<u8>, HevcParseError> {
    if !(1..=4).contains(&length_size) {
        return Err(HevcParseError::InvalidLengthSize);
    }
    let mut remaining = sample;
    let mut first = None;
    while !remaining.is_empty() {
        let prefix = remaining
            .get(..usize::from(length_size))
            .ok_or(HevcParseError::TruncatedLengthPrefix)?;
        let len = prefix
            .iter()
            .fold(0usize, |n, b| (n << 8) | usize::from(*b));
        remaining = &remaining[prefix.len()..];
        let nal = remaining
            .get(..len)
            .filter(|nal| nal.len() >= 2)
            .ok_or(HevcParseError::TruncatedNalUnit)?;
        let kind = (nal[0] >> 1) & 63;
        if kind <= 31 && first.is_none() {
            first = Some(kind);
        }
        remaining = &remaining[len..];
    }
    Ok(first)
}

pub fn hevc_decoder_config_to_annex_b(payload: &[u8]) -> Result<Vec<u8>, HevcParseError> {
    let config = parse_hevc_decoder_config(payload)?;
    Ok(hevc_parameter_sets_to_annex_b(&config))
}

pub fn hevc_parameter_sets_to_annex_b(config: &HevcDecoderConfig) -> Vec<u8> {
    let byte_count = config
        .arrays
        .iter()
        .filter(|array| matches!(array.nal_unit_type, 32..=34))
        .flat_map(|array| array.units.iter())
        .map(|unit| 4 + unit.len())
        .sum();
    let mut out = Vec::with_capacity(byte_count);
    for array in &config.arrays {
        if !matches!(array.nal_unit_type, 32..=34) {
            continue;
        }
        for unit in &array.units {
            out.extend_from_slice(&[0, 0, 0, 1]);
            out.extend_from_slice(unit);
        }
    }
    out
}

pub fn hevc_chunk_to_annex_b(
    payload: &[u8],
    samples: &[ChunkSample],
    nalu_length_size: u8,
) -> Result<Vec<u8>, HevcParseError> {
    if !(1..=4).contains(&nalu_length_size) {
        return Err(HevcParseError::InvalidLengthSize);
    }
    let mut out = Vec::new();
    for sample in samples {
        let start =
            usize::try_from(sample.payload_offset).map_err(|_| HevcParseError::TruncatedNalUnit)?;
        let end = start
            .checked_add(sample.byte_count as usize)
            .ok_or(HevcParseError::TruncatedNalUnit)?;
        if end > payload.len() {
            return Err(HevcParseError::TruncatedNalUnit);
        }
        append_hevc_annex_b(&mut out, &payload[start..end], nalu_length_size)?;
    }
    Ok(out)
}

fn read_u16(bytes: &[u8], offset: usize) -> Result<u16, HevcParseError> {
    let end = offset
        .checked_add(2)
        .ok_or(HevcParseError::TruncatedConfig)?;
    let raw = bytes
        .get(offset..end)
        .ok_or(HevcParseError::TruncatedConfig)?;
    Ok(u16::from_be_bytes(
        raw.try_into()
            .map_err(|_| HevcParseError::TruncatedConfig)?,
    ))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn classifies_random_access_and_leading_pictures_without_copying_payloads() {
        for kind in [0, 1, 8, 9, 16, 19, 21] {
            let sample = [0, 0, 0, 2, kind << 1, 1];
            assert_eq!(sample_vcl_type(&sample, 4), Ok(Some(kind)));
        }
        assert_eq!(sample_vcl_type(&[0, 0, 0, 2, 64, 1], 4), Ok(None));
        assert!(sample_vcl_type(&[0, 0, 0, 3, 42, 1], 4).is_err());
        assert!(sample_vcl_type(&[0, 0, 0], 4).is_err());
    }
    use crate::packet::{TimeDelta, TimePoint};

    #[test]
    fn parses_hevc_decoder_config_arrays() {
        let mut config = vec![
            1, 1, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 120, 0, 0, 0, 0, 0, 0, 0, 0, 3, 1,
        ];
        config.extend_from_slice(&[0x20, 0, 1, 0, 2, 0xaa, 0xbb]);
        let parsed = parse_hevc_decoder_config(&config).unwrap();
        assert_eq!(parsed.general_profile_idc, 1);
        assert_eq!(parsed.general_level_idc, 120);
        assert_eq!(parsed.nalu_length_size, 4);
        assert_eq!(parsed.arrays[0].nal_unit_type, 32);
        assert_eq!(parsed.arrays[0].units[0], vec![0xaa, 0xbb]);
    }

    #[test]
    fn rejects_truncated_config_counts_before_allocating() {
        let mut config = vec![0; 23];
        config[22] = 255;
        assert_eq!(
            parse_hevc_decoder_config(&config),
            Err(HevcParseError::TruncatedConfig)
        );
        config[22] = 1;
        config.extend_from_slice(&[0x20, 0xff, 0xff]);
        assert_eq!(
            parse_hevc_decoder_config(&config),
            Err(HevcParseError::TruncatedConfig)
        );
    }

    #[test]
    fn decoder_config_bounds_empty_parameter_set_metadata() {
        let mut config = vec![0; 23];
        config[22] = 2;
        for _ in 0..2 {
            config.extend_from_slice(&[0x20, 0x08, 0x01]); // 2049 empty units.
            config.resize(config.len() + 2 * 2_049, 0);
        }
        assert_eq!(
            parse_hevc_decoder_config(&config),
            Err(HevcParseError::ResourceLimit)
        );
    }

    #[test]
    fn annex_b_expansion_enforces_aggregate_limit() {
        let mut out = vec![0; MAX_ANNEX_B_BYTES - 4];
        assert_eq!(
            append_hevc_annex_b(&mut out, &[1, 0x40], 1),
            Err(HevcParseError::ResourceLimit)
        );
        assert_eq!(
            hevc_chunk_to_annex_b(&[], &[], 0),
            Err(HevcParseError::InvalidLengthSize)
        );
    }

    #[test]
    fn converts_hevc_sample_to_annex_b() {
        let sample = [0, 0, 0, 2, 0xaa, 0xbb, 0, 0, 0, 1, 0xcc];
        let out = hevc_sample_to_annex_b(&sample, 4).unwrap();
        assert_eq!(out, vec![0, 0, 0, 1, 0xaa, 0xbb, 0, 0, 0, 1, 0xcc]);
    }

    #[test]
    fn converts_hevc_decoder_config_parameter_sets_to_annex_b() {
        let mut config = vec![
            1, 1, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 120, 0, 0, 0, 0, 0, 0, 0, 0, 3, 4,
        ];
        config.extend_from_slice(&[0x20, 0, 1, 0, 2, 0xaa, 0xbb]);
        config.extend_from_slice(&[0x21, 0, 1, 0, 1, 0xcc]);
        config.extend_from_slice(&[0x22, 0, 1, 0, 1, 0xdd]);
        config.extend_from_slice(&[0x27, 0, 1, 0, 1, 0xee]);

        let out = hevc_decoder_config_to_annex_b(&config).unwrap();
        assert_eq!(
            out,
            vec![0, 0, 0, 1, 0xaa, 0xbb, 0, 0, 0, 1, 0xcc, 0, 0, 0, 1, 0xdd,]
        );
    }

    #[test]
    fn converts_hevc_chunk_to_annex_b() {
        let payload = [0, 0, 0, 2, 0xaa, 0xbb, 0, 0, 0, 1, 0xcc];
        let samples = vec![sample(0, 0, 6), sample(1, 6, 5)];
        let out = hevc_chunk_to_annex_b(&payload, &samples, 4).unwrap();
        assert_eq!(out, vec![0, 0, 0, 1, 0xaa, 0xbb, 0, 0, 0, 1, 0xcc]);
    }

    fn sample(index: u32, payload_offset: u64, byte_count: u32) -> ChunkSample {
        ChunkSample {
            index,
            payload_offset,
            byte_count,
            pts: TimePoint::millis(0),
            dts: TimePoint::millis(0),
            duration: TimeDelta::millis(1),
            keyframe: index == 0,
        }
    }
}
