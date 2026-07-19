#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct AacAudioSpecificConfig {
    pub object_type: u8,
    pub sample_rate: u32,
    pub channel_config: u8,
}

use thiserror::Error;

use crate::packet::ChunkSample;

#[derive(Debug, Error, PartialEq, Eq)]
pub enum AacError {
    #[error("AudioSpecificConfig is truncated")]
    TruncatedConfig,
    #[error("unsupported AAC sampling frequency index")]
    UnsupportedSampleRate,
    #[error("AAC frame is too large for ADTS")]
    FrameTooLarge,
    #[error("sample payload range is outside the chunk payload")]
    SampleOutOfBounds,
}

pub fn parse_audio_specific_config(bytes: &[u8]) -> Result<AacAudioSpecificConfig, AacError> {
    let first = *bytes.first().ok_or(AacError::TruncatedConfig)?;
    let second = *bytes.get(1).ok_or(AacError::TruncatedConfig)?;
    let object_type = first >> 3;
    let frequency_index = ((first & 0x07) << 1) | (second >> 7);
    let channel_config = (second >> 3) & 0x0f;
    let sample_rate =
        sample_rate_for_index(frequency_index).ok_or(AacError::UnsupportedSampleRate)?;
    Ok(AacAudioSpecificConfig {
        object_type,
        sample_rate,
        channel_config,
    })
}

pub fn aac_chunk_to_adts(
    payload: &[u8],
    samples: &[ChunkSample],
    config: AacAudioSpecificConfig,
) -> Result<Vec<u8>, AacError> {
    let mut out = Vec::with_capacity(payload.len().saturating_add(samples.len() * 7));
    for sample in samples {
        let start = sample.payload_offset as usize;
        let end = start
            .checked_add(sample.byte_count as usize)
            .ok_or(AacError::SampleOutOfBounds)?;
        if end > payload.len() {
            return Err(AacError::SampleOutOfBounds);
        }
        out.extend_from_slice(&adts_header(sample.byte_count as usize, config)?);
        out.extend_from_slice(&payload[start..end]);
    }
    Ok(out)
}

pub fn adts_header(
    payload_len: usize,
    config: AacAudioSpecificConfig,
) -> Result<[u8; 7], AacError> {
    let frame_len = payload_len.checked_add(7).ok_or(AacError::FrameTooLarge)?;
    if frame_len > 0x1fff {
        return Err(AacError::FrameTooLarge);
    }
    let frequency_index =
        sample_rate_index(config.sample_rate).ok_or(AacError::UnsupportedSampleRate)?;
    let profile = config.object_type.saturating_sub(1).min(3);
    let channel = config.channel_config & 0x07;

    Ok([
        0xff,
        0xf1,
        (profile << 6) | (frequency_index << 2) | ((channel & 0x04) >> 2),
        ((channel & 0x03) << 6) | (((frame_len >> 11) as u8) & 0x03),
        ((frame_len >> 3) & 0xff) as u8,
        (((frame_len & 0x07) as u8) << 5) | 0x1f,
        0xfc,
    ])
}

fn sample_rate_for_index(index: u8) -> Option<u32> {
    Some(match index {
        0 => 96_000,
        1 => 88_200,
        2 => 64_000,
        3 => 48_000,
        4 => 44_100,
        5 => 32_000,
        6 => 24_000,
        7 => 22_050,
        8 => 16_000,
        9 => 12_000,
        10 => 11_025,
        11 => 8_000,
        12 => 7_350,
        _ => return None,
    })
}

fn sample_rate_index(sample_rate: u32) -> Option<u8> {
    (0..=12).find(|index| sample_rate_for_index(*index) == Some(sample_rate))
}

#[cfg(test)]
mod tests {
    use crate::packet::{ChunkSample, TimeDelta, TimePoint};

    use super::*;

    #[test]
    fn parses_audio_specific_config() {
        let config = parse_audio_specific_config(&[0x11, 0x90]).unwrap();
        assert_eq!(config.object_type, 2);
        assert_eq!(config.sample_rate, 48_000);
        assert_eq!(config.channel_config, 2);
    }

    #[test]
    fn wraps_aac_samples_in_adts() {
        let payload = b"aaabbbb";
        let samples = vec![sample(0, 0, 3), sample(1, 3, 4)];
        let config = AacAudioSpecificConfig {
            object_type: 2,
            sample_rate: 48_000,
            channel_config: 2,
        };
        let out = aac_chunk_to_adts(payload, &samples, config).unwrap();
        assert_eq!(&out[0..2], &[0xff, 0xf1]);
        assert_eq!(&out[7..10], b"aaa");
        assert_eq!(&out[10..12], &[0xff, 0xf1]);
        assert_eq!(&out[17..21], b"bbbb");
    }

    fn sample(index: u32, payload_offset: u64, byte_count: u32) -> ChunkSample {
        ChunkSample {
            index,
            payload_offset,
            byte_count,
            pts: TimePoint::millis(0),
            dts: TimePoint::millis(0),
            duration: TimeDelta::millis(1),
            keyframe: true,
        }
    }
}
