use serde::{Deserialize, Serialize};
use thiserror::Error;

/// DTS core sync word in normal big-endian 16-bit representation.
pub const DTS_CORE_SYNC_BE: [u8; 4] = [0x7f, 0xfe, 0x80, 0x01];

/// Parsed DTS core frame header facts needed by Chroma's audio bridge.
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct DtsCoreFrameHeader {
    /// Byte offset of the sync word in the packet payload.
    pub offset: usize,
    /// Full compressed DTS core frame size in bytes.
    pub frame_size: usize,
    /// PCM samples represented by this frame per channel.
    pub sample_count: u32,
    /// Source sample rate in Hz.
    pub sample_rate: u32,
    /// DTS channel-arrangement code.
    pub channel_arrangement: u8,
    /// Best-effort decoded channel count including LFE when present.
    pub channels: u32,
    /// True when the DTS core header declares an LFE channel.
    pub lfe: bool,
    /// Nominal compressed bitrate in bits per second, when the code is known.
    pub bitrate_bps: Option<u32>,
}

/// DTS packet parsing errors.
#[derive(Debug, Error, PartialEq, Eq)]
pub enum DtsParseError {
    /// No supported DTS core sync word was found.
    #[error("no supported DTS core sync word found")]
    SyncNotFound,
    /// The header ends before all fixed DTS core fields are available.
    #[error("truncated DTS core header")]
    TruncatedHeader,
    /// A parsed field value is outside the supported DTS core table.
    #[error("unsupported DTS core header field: {field}={value}")]
    UnsupportedField {
        /// Header field name.
        field: &'static str,
        /// Header field value.
        value: u32,
    },
    /// Header frame size does not fit inside the packet payload.
    #[error("DTS core frame size exceeds packet payload")]
    FrameOutOfBounds,
}

/// Returns every parseable DTS core frame inside a compressed packet payload.
pub fn parse_dts_core_frames(payload: &[u8]) -> Result<Vec<DtsCoreFrameHeader>, DtsParseError> {
    let mut frames = Vec::new();
    let mut offset = 0_usize;
    while let Some(sync) = find_dts_core_sync(&payload[offset..]) {
        let absolute = offset + sync;
        let header = parse_dts_core_frame_at(payload, absolute)?;
        offset = absolute.saturating_add(header.frame_size.max(1));
        frames.push(header);
    }
    if frames.is_empty() {
        Err(DtsParseError::SyncNotFound)
    } else {
        Ok(frames)
    }
}

/// Parses one DTS core frame header at a known sync offset.
pub fn parse_dts_core_frame_at(
    payload: &[u8],
    offset: usize,
) -> Result<DtsCoreFrameHeader, DtsParseError> {
    let header = payload
        .get(offset..)
        .ok_or(DtsParseError::TruncatedHeader)?;
    if header.len() < 12 {
        return Err(DtsParseError::TruncatedHeader);
    }
    if !header.starts_with(&DTS_CORE_SYNC_BE) {
        return Err(DtsParseError::SyncNotFound);
    }

    let mut bits = BitReader::new(header);
    let sync = bits.read(32).ok_or(DtsParseError::TruncatedHeader)?;
    if sync != 0x7ffe_8001 {
        return Err(DtsParseError::SyncNotFound);
    }

    let _frame_type = bits.read(1).ok_or(DtsParseError::TruncatedHeader)?;
    let _deficit_sample_count = bits.read(5).ok_or(DtsParseError::TruncatedHeader)?;
    let _crc_present = bits.read(1).ok_or(DtsParseError::TruncatedHeader)?;
    let block_count = bits.read(7).ok_or(DtsParseError::TruncatedHeader)? + 1;
    let frame_size = bits.read(14).ok_or(DtsParseError::TruncatedHeader)? + 1;
    let channel_arrangement = bits.read(6).ok_or(DtsParseError::TruncatedHeader)? as u8;
    let sample_rate_code = bits.read(4).ok_or(DtsParseError::TruncatedHeader)? as u8;
    let bitrate_code = bits.read(5).ok_or(DtsParseError::TruncatedHeader)? as u8;
    let _embedded_downmix = bits.read(1).ok_or(DtsParseError::TruncatedHeader)?;
    let _embedded_dynamic_range = bits.read(1).ok_or(DtsParseError::TruncatedHeader)?;
    let _embedded_timestamp = bits.read(1).ok_or(DtsParseError::TruncatedHeader)?;
    let _auxiliary_data = bits.read(1).ok_or(DtsParseError::TruncatedHeader)?;
    let _hdcd = bits.read(1).ok_or(DtsParseError::TruncatedHeader)?;
    let _extension_audio = bits.read(3).ok_or(DtsParseError::TruncatedHeader)?;
    let _extended_coding = bits.read(1).ok_or(DtsParseError::TruncatedHeader)?;
    let _audio_sync_word_insertion = bits.read(1).ok_or(DtsParseError::TruncatedHeader)?;
    let lfe_code = bits.read(2).ok_or(DtsParseError::TruncatedHeader)? as u8;

    let frame_size = usize::try_from(frame_size).map_err(|_| DtsParseError::FrameOutOfBounds)?;
    if offset
        .checked_add(frame_size)
        .is_none_or(|end| end > payload.len())
    {
        return Err(DtsParseError::FrameOutOfBounds);
    }

    let sample_rate = dts_sample_rate(sample_rate_code).ok_or(DtsParseError::UnsupportedField {
        field: "sampleRateCode",
        value: u32::from(sample_rate_code),
    })?;
    let base_channels =
        dts_base_channel_count(channel_arrangement).ok_or(DtsParseError::UnsupportedField {
            field: "channelArrangement",
            value: u32::from(channel_arrangement),
        })?;
    let lfe = lfe_code != 0;
    Ok(DtsCoreFrameHeader {
        offset,
        frame_size,
        sample_count: block_count * 32,
        sample_rate,
        channel_arrangement,
        channels: base_channels + u32::from(lfe),
        lfe,
        bitrate_bps: dts_bitrate_bps(bitrate_code),
    })
}

fn find_dts_core_sync(payload: &[u8]) -> Option<usize> {
    payload
        .windows(DTS_CORE_SYNC_BE.len())
        .position(|window| window == DTS_CORE_SYNC_BE)
}

fn dts_sample_rate(code: u8) -> Option<u32> {
    match code {
        1 => Some(8_000),
        2 => Some(16_000),
        3 => Some(32_000),
        6 => Some(11_025),
        7 => Some(22_050),
        8 => Some(44_100),
        11 => Some(12_000),
        12 => Some(24_000),
        13 => Some(48_000),
        14 => Some(96_000),
        15 => Some(192_000),
        _ => None,
    }
}

fn dts_base_channel_count(arrangement: u8) -> Option<u32> {
    match arrangement {
        0 => Some(1),
        1..=4 => Some(2),
        5 | 6 => Some(3),
        7 | 8 => Some(4),
        9 => Some(5),
        10 | 11 => Some(6),
        12 => Some(7),
        13 | 14 => Some(8),
        15 => Some(6),
        _ => None,
    }
}

fn dts_bitrate_bps(code: u8) -> Option<u32> {
    const RATES: [Option<u32>; 32] = [
        Some(32_000),
        Some(56_000),
        Some(64_000),
        Some(96_000),
        Some(112_000),
        Some(128_000),
        Some(192_000),
        Some(224_000),
        Some(256_000),
        Some(320_000),
        Some(384_000),
        Some(448_000),
        Some(512_000),
        Some(576_000),
        Some(640_000),
        Some(768_000),
        Some(960_000),
        Some(1_024_000),
        Some(1_152_000),
        Some(1_280_000),
        Some(1_344_000),
        Some(1_408_000),
        Some(1_411_200),
        Some(1_472_000),
        Some(1_536_000),
        Some(1_920_000),
        Some(2_048_000),
        Some(3_072_000),
        Some(3_840_000),
        None,
        None,
        None,
    ];
    RATES.get(usize::from(code)).copied().flatten()
}

struct BitReader<'a> {
    bytes: &'a [u8],
    bit_offset: usize,
}

impl<'a> BitReader<'a> {
    fn new(bytes: &'a [u8]) -> Self {
        Self {
            bytes,
            bit_offset: 0,
        }
    }

    fn read(&mut self, bit_count: u8) -> Option<u32> {
        if bit_count > 32 {
            return None;
        }
        let end = self.bit_offset.checked_add(usize::from(bit_count))?;
        if end > self.bytes.len() * 8 {
            return None;
        }

        let mut out = 0_u32;
        for _ in 0..bit_count {
            let byte = self.bytes[self.bit_offset / 8];
            let shift = 7 - (self.bit_offset % 8);
            out = (out << 1) | u32::from((byte >> shift) & 1);
            self.bit_offset += 1;
        }
        Some(out)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_dts_core_header() {
        let packet = test_dts_header(512, 9, 13, 24, 1);

        let frames = parse_dts_core_frames(&packet).expect("parse synthetic DTS frame");

        assert_eq!(frames.len(), 1);
        assert_eq!(frames[0].frame_size, 512);
        assert_eq!(frames[0].sample_count, 512);
        assert_eq!(frames[0].sample_rate, 48_000);
        assert_eq!(frames[0].channels, 6);
        assert_eq!(frames[0].bitrate_bps, Some(1_536_000));
    }

    #[test]
    fn rejects_out_of_bounds_frame() {
        let mut packet = test_dts_header(4096, 9, 13, 24, 1);
        packet.truncate(512);

        let err = parse_dts_core_frames(&packet).expect_err("frame size exceeds payload");

        assert_eq!(err, DtsParseError::FrameOutOfBounds);
    }

    fn test_dts_header(
        frame_size: u16,
        channel_arrangement: u8,
        sample_rate_code: u8,
        bitrate_code: u8,
        lfe_code: u8,
    ) -> Vec<u8> {
        let mut writer = BitWriter::new();
        writer.write(0x7ffe_8001, 32);
        writer.write(1, 1);
        writer.write(31, 5);
        writer.write(0, 1);
        writer.write(15, 7);
        writer.write(u32::from(frame_size - 1), 14);
        writer.write(u32::from(channel_arrangement), 6);
        writer.write(u32::from(sample_rate_code), 4);
        writer.write(u32::from(bitrate_code), 5);
        writer.write(0, 10);
        writer.write(u32::from(lfe_code), 2);
        let mut bytes = writer.into_bytes();
        bytes.resize(frame_size as usize, 0);
        bytes
    }

    struct BitWriter {
        bytes: Vec<u8>,
        bit_offset: usize,
    }

    impl BitWriter {
        fn new() -> Self {
            Self {
                bytes: Vec::new(),
                bit_offset: 0,
            }
        }

        fn write(&mut self, value: u32, bit_count: u8) {
            for bit in (0..bit_count).rev() {
                if self.bit_offset.is_multiple_of(8) {
                    self.bytes.push(0);
                }
                if ((value >> bit) & 1) != 0 {
                    let index = self.bytes.len() - 1;
                    self.bytes[index] |= 1 << (7 - (self.bit_offset % 8));
                }
                self.bit_offset += 1;
            }
        }

        fn into_bytes(self) -> Vec<u8> {
            self.bytes
        }
    }
}
