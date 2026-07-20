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
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AvcParameterSets {
    pub nalu_length_size: u8,
    pub sps: Vec<Vec<u8>>,
    pub pps: Vec<Vec<u8>>,
}

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
        let sample_start = sample.payload_offset as usize;
        let sample_end = sample_start
            .checked_add(sample.byte_count as usize)
            .ok_or(H264ParseError::SampleOutOfBounds)?;
        if sample_end > payload.len() {
            return Err(H264ParseError::SampleOutOfBounds);
        }
        nalus.extend(parse_avc_sample_nalus(
            sample.index,
            sample.payload_offset,
            &payload[sample_start..sample_end],
            nalu_length_size,
        )?);
    }

    Ok(nalus)
}

pub fn parse_avc_sample_nalus(
    sample_index: u32,
    sample_payload_offset: u64,
    sample_payload: &[u8],
    nalu_length_size: u8,
) -> Result<Vec<AvcNalUnit>, H264ParseError> {
    if !(1..=4).contains(&nalu_length_size) {
        return Err(H264ParseError::InvalidLengthSize);
    }

    let prefix_len = nalu_length_size as usize;
    let mut offset = 0_usize;
    let mut nalus = Vec::new();

    while offset < sample_payload.len() {
        let prefix_end = offset
            .checked_add(prefix_len)
            .ok_or(H264ParseError::TruncatedLengthPrefix)?;
        if prefix_end > sample_payload.len() {
            return Err(H264ParseError::TruncatedLengthPrefix);
        }
        let nal_len = read_be_uint(&sample_payload[offset..prefix_end]);
        offset = prefix_end;
        let nal_end = offset
            .checked_add(nal_len)
            .ok_or(H264ParseError::TruncatedNalUnit)?;
        if nal_end > sample_payload.len() {
            return Err(H264ParseError::TruncatedNalUnit);
        }
        if nal_len > 0 {
            let nal_unit_type = sample_payload[offset] & 0x1f;
            nalus.push(AvcNalUnit {
                sample_index,
                sample_payload_offset,
                payload_offset: sample_payload_offset.saturating_add(offset as u64),
                byte_count: nal_len as u32,
                nal_unit_type,
            });
        }
        offset = nal_end;
    }

    Ok(nalus)
}

pub fn avc_sample_to_annex_b(
    sample_payload: &[u8],
    nalu_length_size: u8,
) -> Result<Vec<u8>, H264ParseError> {
    let nalus = parse_avc_sample_nalus(0, 0, sample_payload, nalu_length_size)?;
    let mut out = Vec::with_capacity(sample_payload.len());
    for nalu in nalus {
        let start = nalu.payload_offset as usize;
        let end = start
            .checked_add(nalu.byte_count as usize)
            .ok_or(H264ParseError::TruncatedNalUnit)?;
        if end > sample_payload.len() {
            return Err(H264ParseError::TruncatedNalUnit);
        }
        out.extend_from_slice(&[0, 0, 0, 1]);
        out.extend_from_slice(&sample_payload[start..end]);
    }
    Ok(out)
}

pub fn avc_chunk_to_annex_b(
    payload: &[u8],
    samples: &[ChunkSample],
    nalu_length_size: u8,
) -> Result<Vec<u8>, H264ParseError> {
    let mut out = Vec::with_capacity(payload.len());
    for sample in samples {
        let start = sample.payload_offset as usize;
        let end = start
            .checked_add(sample.byte_count as usize)
            .ok_or(H264ParseError::SampleOutOfBounds)?;
        if end > payload.len() {
            return Err(H264ParseError::SampleOutOfBounds);
        }
        out.extend_from_slice(&avc_sample_to_annex_b(
            &payload[start..end],
            nalu_length_size,
        )?);
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
