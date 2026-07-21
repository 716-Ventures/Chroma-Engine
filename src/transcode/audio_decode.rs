use serde::{Deserialize, Serialize};
use thiserror::Error;

use crate::{
    codec::dts::{DtsCoreFrameHeader, DtsParseError, parse_dts_core_frames},
    packet::{ChunkSample, TimeDelta, TimePoint, TimeScale},
    transcode::PcmAudioFormat,
};

/// Source audio codecs accepted by Chroma Engine's decode-facing bridge API.
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub enum AudioDecodeCodec {
    /// DTS or DTS-HD source packets with a DTS core substream.
    Dts,
}

/// Borrowed compressed audio packet ready for native bridge analysis or decode.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct CompressedAudioPacket<'a> {
    /// Source packet index.
    pub index: u32,
    /// Presentation timestamp carried by the packet.
    pub pts: TimePoint,
    /// Decode timestamp carried by the packet.
    pub dts: TimePoint,
    /// Packet duration.
    pub duration: TimeDelta,
    /// Borrowed compressed packet bytes.
    pub bytes: &'a [u8],
}

/// Packet batch and stream metadata sent into a native audio bridge.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AudioDecodeInput<'a> {
    /// Source codec carried by the compressed packets.
    pub codec: AudioDecodeCodec,
    /// Decoder time scale for packet timestamps.
    pub time_scale: TimeScale,
    /// Ordered compressed packets.
    pub packets: Vec<CompressedAudioPacket<'a>>,
    /// True when this input is the final batch and the decoder must drain after it.
    pub end_of_stream: bool,
}

/// Stable DTS packet facts needed before PCM decode/encode can run.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct DtsAudioPacketProbe {
    /// Source packet index.
    pub packet_index: u32,
    /// Packet presentation timestamp.
    pub pts: TimePoint,
    /// Packet decode timestamp.
    pub dts: TimePoint,
    /// Packet duration.
    pub duration: TimeDelta,
    /// Parsed DTS core frames in this packet.
    pub frames: Vec<DtsCoreFrameHeader>,
}

/// Aggregate DTS bridge facts for one packet chunk.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct DtsAudioBridgeProbe {
    /// Decoder-facing source codec.
    pub codec: AudioDecodeCodec,
    /// Best-effort PCM output shape inferred from DTS core headers.
    pub pcm_format: PcmAudioFormat,
    /// Number of compressed packets inspected.
    pub packet_count: usize,
    /// Number of DTS core frames found.
    pub dts_core_frame_count: usize,
    /// Total PCM frames represented by the DTS core stream per channel.
    pub pcm_frame_count: u64,
    /// First packet PTS, when available.
    pub first_pts: Option<TimePoint>,
    /// Last packet PTS, when available.
    pub last_pts: Option<TimePoint>,
    /// True when every parsed DTS core frame reports the same sample rate and channel count.
    pub stable_format: bool,
    /// Per-packet DTS core facts.
    pub packets: Vec<DtsAudioPacketProbe>,
}

/// Error returned while preparing or probing audio decode input.
#[derive(Debug, Error, PartialEq, Eq)]
pub enum AudioDecodeError {
    /// Packet sample metadata points outside the payload.
    #[error("packet sample byte range is outside the decode payload")]
    PacketOutOfBounds,
    /// The caller provided an unsupported compressed source codec.
    #[error("unsupported audio decode codec")]
    UnsupportedCodec,
    /// No parseable compressed audio frame was found.
    #[error("no decodable audio frames found")]
    NoFrames,
    /// The compressed source stream changed PCM shape inside the chunk.
    #[error("audio format changed inside decode chunk")]
    FormatChanged,
    /// DTS packet parsing failed.
    #[error("DTS packet parsing failed: {0}")]
    Dts(#[from] DtsParseError),
    /// The native decode backend returned an error.
    #[error("native audio decode failed: {reason}")]
    BackendFailed {
        /// Diagnostic reason.
        reason: String,
    },
    /// DTS can be inspected for routing, but no production decoder is currently executable.
    #[error("audio decode capability is unsupported: {codec}")]
    UnsupportedCapability {
        /// Unsupported source codec.
        codec: &'static str,
    },
}

/// Builds zero-copy compressed audio packets from extracted chunk metadata.
pub fn build_audio_decode_input<'a>(
    codec: AudioDecodeCodec,
    time_scale: TimeScale,
    samples: &[ChunkSample],
    payload: &'a [u8],
    end_of_stream: bool,
) -> Result<AudioDecodeInput<'a>, AudioDecodeError> {
    let packets = samples
        .iter()
        .map(|sample| {
            let start = usize::try_from(sample.payload_offset)
                .map_err(|_| AudioDecodeError::PacketOutOfBounds)?;
            let size = usize::try_from(sample.byte_count)
                .map_err(|_| AudioDecodeError::PacketOutOfBounds)?;
            let end = start
                .checked_add(size)
                .ok_or(AudioDecodeError::PacketOutOfBounds)?;
            let bytes = payload
                .get(start..end)
                .ok_or(AudioDecodeError::PacketOutOfBounds)?;
            Ok(CompressedAudioPacket {
                index: sample.index,
                pts: sample.pts,
                dts: sample.dts,
                duration: sample.duration,
                bytes,
            })
        })
        .collect::<std::result::Result<Vec<_>, AudioDecodeError>>()?;

    Ok(AudioDecodeInput {
        codec,
        time_scale,
        packets,
        end_of_stream,
    })
}

/// Inspects DTS packets and returns the stable PCM shape required by the audio bridge.
pub fn probe_dts_audio_bridge(
    input: &AudioDecodeInput<'_>,
) -> Result<DtsAudioBridgeProbe, AudioDecodeError> {
    if input.codec != AudioDecodeCodec::Dts {
        return Err(AudioDecodeError::UnsupportedCodec);
    }

    let mut packets = Vec::new();
    let mut pcm_format = None;
    let mut stable_format = true;
    let mut dts_core_frame_count = 0_usize;
    let mut pcm_frame_count = 0_u64;

    for packet in &input.packets {
        let frames = parse_dts_core_frames(packet.bytes)?;
        for frame in &frames {
            let format = PcmAudioFormat {
                sample_rate: frame.sample_rate,
                channels: frame.channels.min(6),
            };
            if let Some(existing) = pcm_format {
                if existing != format {
                    stable_format = false;
                }
            } else {
                pcm_format = Some(format);
            }
            dts_core_frame_count += 1;
            pcm_frame_count += u64::from(frame.sample_count);
        }
        packets.push(DtsAudioPacketProbe {
            packet_index: packet.index,
            pts: packet.pts,
            dts: packet.dts,
            duration: packet.duration,
            frames,
        });
    }

    let pcm_format = pcm_format.ok_or(AudioDecodeError::NoFrames)?;
    Ok(DtsAudioBridgeProbe {
        codec: input.codec,
        pcm_format,
        packet_count: input.packets.len(),
        dts_core_frame_count,
        pcm_frame_count,
        first_pts: input.packets.first().map(|packet| packet.pts),
        last_pts: input.packets.last().map(|packet| packet.pts),
        stable_format,
        packets,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::packet::ChunkSample;

    #[test]
    fn build_audio_decode_input_rejects_bad_sample_range() {
        let sample = ChunkSample {
            index: 0,
            payload_offset: 10,
            byte_count: 4,
            pts: TimePoint::millis(0),
            dts: TimePoint::millis(0),
            duration: TimeDelta::millis(20),
            keyframe: true,
        };

        let err = build_audio_decode_input(
            AudioDecodeCodec::Dts,
            TimeScale::MILLIS,
            &[sample],
            &[0, 1, 2],
            true,
        )
        .expect_err("reject out-of-bounds sample");

        assert_eq!(err, AudioDecodeError::PacketOutOfBounds);
    }
}
