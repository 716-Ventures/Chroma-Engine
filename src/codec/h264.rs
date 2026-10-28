use serde::{Deserialize, Serialize};
use thiserror::Error;

use crate::packet::ChunkSample;

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct AvcNalUnit {
    pub sample_index: u32,
    pub sample_payload_offset: u64,
    pub payload_offset: u64,
    pub byte_count: u32,
    pub nal_unit_type: u8,
}

#[derive(Debug, Error, PartialEq, Eq)]
pub enum H264ParseError {
    #[error("AVC NAL length size must be between 1 and 4 bytes")]
    InvalidLengthSize,
    #[error("sample payload range is outside the chunk payload")]
    SampleOutOfBounds,
    #[error("NAL length prefix is truncated")]
    TruncatedLengthPrefix,
    #[error("NAL payload is truncated")]
    TruncatedNalUnit,
    #[error("AVC decoder configuration record is truncated")]
    TruncatedAvcConfig,
    #[error("AVC NAL metadata or Annex B output exceeds the resource limit")]
    ResourceLimit,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AvcParameterSets {
    pub nalu_length_size: u8,
    pub sps: Vec<Vec<u8>>,
    pub pps: Vec<Vec<u8>>,
}

// Bound expanded NAL metadata independently of compressed input size. A stream
// of tiny NAL units can otherwise expand a 64 MiB packet window into over 1 GiB.
const MAX_NAL_UNITS: usize = 1_000_000;
const MAX_ANNEX_B_BYTES: usize = 64 * 1024 * 1024;

pub fn parse_avc_chunk_nalus(
    payload: &[u8],
    samples: &[ChunkSample],
    nalu_length_size: u8,
) -> Result<Vec<AvcNalUnit>, H264ParseError> {
    if !(1..=4).contains(&nalu_length_size) {
        return Err(H264ParseError::InvalidLengthSize);
    }
    let mut nalus = Vec::new();
    for sample in samples {
        let start = usize::try_from(sample.payload_offset)
            .map_err(|_| H264ParseError::SampleOutOfBounds)?;
        let end = start
            .checked_add(sample.byte_count as usize)
            .ok_or(H264ParseError::SampleOutOfBounds)?;
        let bytes = payload
            .get(start..end)
            .ok_or(H264ParseError::SampleOutOfBounds)?;
        append_avc_nalus(
            &mut nalus,
            sample.index,
            sample.payload_offset,
            bytes,
            nalu_length_size,
        )?;
    }
    Ok(nalus)
}

pub fn parse_avc_sample_nalus(
    sample_index: u32,
    sample_payload_offset: u64,
    sample_payload: &[u8],
    nalu_length_size: u8,
) -> Result<Vec<AvcNalUnit>, H264ParseError> {
    let mut nalus = Vec::new();
    append_avc_nalus(
        &mut nalus,
        sample_index,
        sample_payload_offset,
        sample_payload,
        nalu_length_size,
    )?;
    Ok(nalus)
}

fn append_avc_nalus(
    nalus: &mut Vec<AvcNalUnit>,
    sample_index: u32,
    sample_payload_offset: u64,
    sample_payload: &[u8],
    nalu_length_size: u8,
) -> Result<(), H264ParseError> {
    visit_avc_nalus(sample_payload, nalu_length_size, |offset, bytes| {
        if nalus.len() >= MAX_NAL_UNITS {
            return Err(H264ParseError::ResourceLimit);
        }
        let payload_offset = sample_payload_offset
            .checked_add(offset as u64)
            .ok_or(H264ParseError::SampleOutOfBounds)?;
        nalus
            .try_reserve(1)
            .map_err(|_| H264ParseError::ResourceLimit)?;
        nalus.push(AvcNalUnit {
            sample_index,
            sample_payload_offset,
            payload_offset,
            byte_count: bytes.len() as u32,
            nal_unit_type: bytes[0] & 0x1f,
        });
        Ok(())
    })
}

fn visit_avc_nalus(
    sample: &[u8],
    nalu_length_size: u8,
    mut visit: impl FnMut(usize, &[u8]) -> Result<(), H264ParseError>,
) -> Result<(), H264ParseError> {
    if !(1..=4).contains(&nalu_length_size) {
        return Err(H264ParseError::InvalidLengthSize);
    }
    let mut offset = 0usize;
    while offset < sample.len() {
        let prefix_end = offset
            .checked_add(usize::from(nalu_length_size))
            .ok_or(H264ParseError::TruncatedLengthPrefix)?;
        let prefix = sample
            .get(offset..prefix_end)
            .ok_or(H264ParseError::TruncatedLengthPrefix)?;
        let length = read_be_uint(prefix);
        let end = prefix_end
            .checked_add(length)
            .ok_or(H264ParseError::TruncatedNalUnit)?;
        let bytes = sample
            .get(prefix_end..end)
            .ok_or(H264ParseError::TruncatedNalUnit)?;
        if !bytes.is_empty() {
            visit(prefix_end, bytes)?;
        }
        offset = end;
    }
    Ok(())
}

pub fn avc_sample_to_annex_b(
    sample_payload: &[u8],
    nalu_length_size: u8,
) -> Result<Vec<u8>, H264ParseError> {
    let mut out = Vec::new();
    append_annex_b(&mut out, sample_payload, nalu_length_size)?;
    Ok(out)
}

fn append_annex_b(out: &mut Vec<u8>, sample: &[u8], length_size: u8) -> Result<(), H264ParseError> {
    // Validate and size before allocation; conversion never materializes a NAL
    // metadata vector or a separate temporary buffer for each sample.
    let mut size = out.len();
    visit_avc_nalus(sample, length_size, |_, bytes| {
        size = size
            .checked_add(4)
            .and_then(|size| size.checked_add(bytes.len()))
            .filter(|size| *size <= MAX_ANNEX_B_BYTES)
            .ok_or(H264ParseError::ResourceLimit)?;
        Ok(())
    })?;
    out.try_reserve_exact(size - out.len())
        .map_err(|_| H264ParseError::ResourceLimit)?;
    visit_avc_nalus(sample, length_size, |_, bytes| {
        out.extend_from_slice(&[0, 0, 0, 1]);
        out.extend_from_slice(bytes);
        Ok(())
    })
}

pub fn avc_chunk_to_annex_b(
    payload: &[u8],
    samples: &[ChunkSample],
    nalu_length_size: u8,
) -> Result<Vec<u8>, H264ParseError> {
    if !(1..=4).contains(&nalu_length_size) {
        return Err(H264ParseError::InvalidLengthSize);
    }
    let mut total = 0usize;
    for sample in samples {
        let start = usize::try_from(sample.payload_offset)
            .map_err(|_| H264ParseError::SampleOutOfBounds)?;
        let end = start
            .checked_add(sample.byte_count as usize)
            .ok_or(H264ParseError::SampleOutOfBounds)?;
        let bytes = payload
            .get(start..end)
            .ok_or(H264ParseError::SampleOutOfBounds)?;
        visit_avc_nalus(bytes, nalu_length_size, |_, bytes| {
            total = total
                .checked_add(4)
                .and_then(|size| size.checked_add(bytes.len()))
                .filter(|size| *size <= MAX_ANNEX_B_BYTES)
                .ok_or(H264ParseError::ResourceLimit)?;
            Ok(())
        })?;
    }
    let mut out = Vec::new();
    out.try_reserve_exact(total)
        .map_err(|_| H264ParseError::ResourceLimit)?;
    for sample in samples {
        let start = usize::try_from(sample.payload_offset)
            .map_err(|_| H264ParseError::SampleOutOfBounds)?;
        let end = start
            .checked_add(sample.byte_count as usize)
            .ok_or(H264ParseError::SampleOutOfBounds)?;
        let bytes = payload
            .get(start..end)
            .ok_or(H264ParseError::SampleOutOfBounds)?;
        append_annex_b(&mut out, bytes, nalu_length_size)?;
    }
    Ok(out)
}

pub fn parse_avc_decoder_config(payload: &[u8]) -> Result<AvcParameterSets, H264ParseError> {
    if payload.len() < 7 {
        return Err(H264ParseError::TruncatedAvcConfig);
    }
    let nalu_length_size = (payload[4] & 0x03) + 1;
    let sps_count = payload[5] & 0x1f;
    let mut offset = 6_usize;
    let mut sps = Vec::with_capacity(sps_count as usize);
    for _ in 0..sps_count {
        let len = read_u16(payload, offset)? as usize;
        offset += 2;
        let end = offset
            .checked_add(len)
            .ok_or(H264ParseError::TruncatedAvcConfig)?;
        if end > payload.len() {
            return Err(H264ParseError::TruncatedAvcConfig);
        }
        sps.push(payload[offset..end].to_vec());
        offset = end;
    }
    let pps_count = *payload
        .get(offset)
        .ok_or(H264ParseError::TruncatedAvcConfig)?;
    offset += 1;
    let mut pps = Vec::with_capacity(pps_count as usize);
    for _ in 0..pps_count {
        let len = read_u16(payload, offset)? as usize;
        offset += 2;
        let end = offset
            .checked_add(len)
            .ok_or(H264ParseError::TruncatedAvcConfig)?;
        if end > payload.len() {
            return Err(H264ParseError::TruncatedAvcConfig);
        }
        pps.push(payload[offset..end].to_vec());
        offset = end;
    }

    Ok(AvcParameterSets {
        nalu_length_size,
        sps,
        pps,
    })
}

fn read_be_uint(bytes: &[u8]) -> usize {
    bytes
        .iter()
        .fold(0_usize, |value, byte| (value << 8) | usize::from(*byte))
}

fn read_u16(bytes: &[u8], offset: usize) -> Result<u16, H264ParseError> {
    let end = offset
        .checked_add(2)
        .ok_or(H264ParseError::TruncatedAvcConfig)?;
    let slice = bytes
        .get(offset..end)
        .ok_or(H264ParseError::TruncatedAvcConfig)?;
    Ok(u16::from_be_bytes(
        slice
            .try_into()
            .map_err(|_| H264ParseError::TruncatedAvcConfig)?,
    ))
}

#[cfg(test)]
mod tests {
    use crate::packet::{ChunkSample, TimeDelta, TimePoint};

    use super::*;

    #[test]
    fn parses_avc_nalus_across_chunk_samples() {
        let payload = [
            [0, 0, 0, 2].as_slice(),
            &[0x65, 0x88],
            &[0, 0, 0, 1],
            &[0x41],
            &[0, 0, 0, 3],
            &[0x06, 0x01, 0x02],
        ]
        .concat();
        let samples = vec![sample(0, 0, 11), sample(1, 11, 7)];

        let nalus = parse_avc_chunk_nalus(&payload, &samples, 4).unwrap();
        assert_eq!(nalus.len(), 3);
        assert_eq!(nalus[0].sample_index, 0);
        assert_eq!(nalus[0].payload_offset, 4);
        assert_eq!(nalus[0].byte_count, 2);
        assert_eq!(nalus[0].nal_unit_type, 5);
        assert_eq!(nalus[1].sample_index, 0);
        assert_eq!(nalus[1].payload_offset, 10);
        assert_eq!(nalus[1].nal_unit_type, 1);
        assert_eq!(nalus[2].sample_index, 1);
        assert_eq!(nalus[2].payload_offset, 15);
        assert_eq!(nalus[2].nal_unit_type, 6);
    }

    #[test]
    fn converts_avc_sample_to_annex_b() {
        let sample = [
            [0, 0, 0, 2].as_slice(),
            &[0x65, 0x88],
            &[0, 0, 0, 1],
            &[0x41],
        ]
        .concat();
        let out = avc_sample_to_annex_b(&sample, 4).unwrap();
        assert_eq!(out, [0, 0, 0, 1, 0x65, 0x88, 0, 0, 0, 1, 0x41]);
    }

    #[test]
    fn tiny_nal_metadata_expansion_is_bounded_across_samples() {
        let payload = [1, 0x65].repeat(MAX_NAL_UNITS / 2 + 1);
        let count = payload.len() as u32;
        let samples = [sample(0, 0, count), sample(1, 0, count)];
        assert_eq!(
            parse_avc_chunk_nalus(&payload, &samples, 1),
            Err(H264ParseError::ResourceLimit)
        );
        // Packet conversion does not need the expanded metadata at all.
        assert_eq!(
            avc_sample_to_annex_b(&payload, 1).unwrap().len(),
            payload.len() / 2 * 5
        );
    }

    #[test]
    fn conversion_bounds_total_output_and_rejects_invalid_offsets() {
        let mut output = vec![0; MAX_ANNEX_B_BYTES - 4];
        assert_eq!(
            append_annex_b(&mut output, &[1, 0x65], 1),
            Err(H264ParseError::ResourceLimit)
        );
        assert_eq!(
            parse_avc_sample_nalus(0, u64::MAX, &[1, 0x65], 1),
            Err(H264ParseError::SampleOutOfBounds)
        );
        assert_eq!(
            avc_sample_to_annex_b(&[0, 1, 0x65, 0], 1).unwrap(),
            [0, 0, 0, 1, 0x65]
        );
        assert_eq!(
            avc_sample_to_annex_b(&[2, 0x65], 1),
            Err(H264ParseError::TruncatedNalUnit)
        );
        assert_eq!(
            avc_chunk_to_annex_b(&[], &[], 0),
            Err(H264ParseError::InvalidLengthSize)
        );
    }

    #[test]
    fn parses_avc_decoder_config_parameter_sets() {
        let config = [
            [1, 0x64, 0, 0x1f, 0xff, 0xe1].as_slice(),
            &[0, 3],
            &[0x67, 0x64, 0x00],
            &[1],
            &[0, 2],
            &[0x68, 0xeb],
        ]
        .concat();
        let parsed = parse_avc_decoder_config(&config).unwrap();
        assert_eq!(parsed.nalu_length_size, 4);
        assert_eq!(parsed.sps, vec![vec![0x67, 0x64, 0x00]]);
        assert_eq!(parsed.pps, vec![vec![0x68, 0xeb]]);
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
