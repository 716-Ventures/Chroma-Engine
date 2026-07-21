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

/// Stable output description for decoded PCM emitted by a native bridge decoder.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct DecodedPcmStream {
    /// Raw PCM format emitted by the decoder.
    pub format: PcmAudioFormat,
    /// Source codec decoded into PCM.
    pub source_codec: AudioDecodeCodec,
    /// Backend that produced the PCM.
    pub decoder: String,
}

/// One decoded PCM frame emitted by a native bridge decoder.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct DecodedPcmFrame {
    /// Presentation timestamp.
    pub pts: TimePoint,
    /// Decoded PCM frame count per channel.
    pub sample_count: u32,
    /// Interleaved signed 16-bit PCM.
    pub pcm: Vec<i16>,
}

/// Decoded PCM stream plus frames emitted by a backend.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct DecodedPcmOutput {
    /// Output stream description.
    pub stream: DecodedPcmStream,
    /// Decoded PCM frames.
    pub frames: Vec<DecodedPcmFrame>,
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

/// Decodes DTS core frames to interleaved signed 16-bit PCM.
pub fn decode_dts_core_to_interleaved_i16(
    input: &AudioDecodeInput<'_>,
) -> Result<DecodedPcmOutput, AudioDecodeError> {
    let probe = probe_dts_audio_bridge(input)?;
    if !probe.stable_format {
        return Err(AudioDecodeError::FormatChanged);
    }

    use oxideav_core::{CodecId, CodecParameters, Frame, Packet, TimeBase};

    let params = CodecParameters::audio(CodecId::new("dts"));
    let mut decoder = oxideav_dts::decoder::make_decoder(&params).map_err(|error| {
        AudioDecodeError::BackendFailed {
            reason: error.to_string(),
        }
    })?;
    let time_base = TimeBase::new(1, i64::from(input.time_scale.units_per_second.max(1)));
    let mut decoded_frames = Vec::new();

    for (packet, packet_probe) in input.packets.iter().zip(&probe.packets) {
        for frame_header in &packet_probe.frames {
            let start = frame_header.offset;
            let end = start + frame_header.frame_size;
            let frame_bytes = packet
                .bytes
                .get(start..end)
                .ok_or(AudioDecodeError::PacketOutOfBounds)?;
            let oxide_packet = Packet::new(0, time_base, frame_bytes.to_vec())
                .with_pts(timepoint_units(packet.pts))
                .with_dts(timepoint_units(packet.dts))
                .with_duration(timedelta_units(packet.duration));

            decoder.send_packet(&oxide_packet).map_err(|error| {
                AudioDecodeError::BackendFailed {
                    reason: error.to_string(),
                }
            })?;

            loop {
                match decoder.receive_frame() {
                    Ok(Frame::Audio(frame)) => {
                        let plane =
                            frame
                                .data
                                .first()
                                .ok_or_else(|| AudioDecodeError::BackendFailed {
                                    reason: "DTS decoder returned audio frame with no PCM plane"
                                        .to_string(),
                                })?;
                        decoded_frames.push(DecodedPcmFrame {
                            pts: packet.pts,
                            sample_count: frame.samples,
                            pcm: le_bytes_to_i16(plane),
                        });
                    }
                    Ok(_) => {
                        return Err(AudioDecodeError::BackendFailed {
                            reason: "DTS decoder returned non-audio frame".to_string(),
                        });
                    }
                    Err(error) if error.is_need_more() => break,
                    Err(error) => {
                        return Err(AudioDecodeError::BackendFailed {
                            reason: error.to_string(),
                        });
                    }
                }
            }
        }
    }

    let decoded_channels = decoded_frames
        .first()
        .and_then(|frame| {
            let samples = usize::try_from(frame.sample_count).ok()?;
            u32::try_from(frame.pcm.len().checked_div(samples)?).ok()
        })
        .filter(|channels| *channels > 0)
        .unwrap_or(probe.pcm_format.channels);

    Ok(DecodedPcmOutput {
        stream: DecodedPcmStream {
            format: PcmAudioFormat {
                sample_rate: probe.pcm_format.sample_rate,
                channels: decoded_channels,
            },
            source_codec: AudioDecodeCodec::Dts,
            decoder: "oxideav-dts-core".to_string(),
        },
        frames: decoded_frames,
    })
}

/// Selects the E-AC-3 bridge channel count for decoded PCM.
pub fn eac3_bridge_channel_count(decoded_channels: u32) -> u32 {
    match decoded_channels {
        0 | 1 => 1,
        2 => 2,
        3..=6 => 6,
        _ => 8,
    }
}

/// Converts interleaved PCM to a requested channel count by truncating extra channels
/// or zero-padding missing channels.
pub fn normalize_interleaved_channels(
    pcm: &[i16],
    source_channels: u32,
    target_channels: u32,
) -> Result<Vec<i16>, AudioDecodeError> {
    if source_channels == 0 || target_channels == 0 {
        return Err(AudioDecodeError::BackendFailed {
            reason: "audio bridge channel count must be greater than zero".to_string(),
        });
    }
    if !pcm.len().is_multiple_of(source_channels as usize) {
        return Err(AudioDecodeError::BackendFailed {
            reason: "decoded PCM sample count does not align to source channels".to_string(),
        });
    }
    if source_channels == target_channels {
        return Ok(pcm.to_vec());
    }

    let source_channels = source_channels as usize;
    let target_channels = target_channels as usize;
    let frame_count = pcm.len() / source_channels;
    let mut out = Vec::with_capacity(frame_count * target_channels);
    for frame in pcm.chunks_exact(source_channels) {
        let copied = source_channels.min(target_channels);
        out.extend_from_slice(&frame[..copied]);
        out.resize(out.len() + target_channels - copied, 0);
    }
    Ok(out)
}

fn timepoint_units(value: TimePoint) -> i64 {
    i64::try_from(value.units).unwrap_or(i64::MAX)
}

fn timedelta_units(value: TimeDelta) -> i64 {
    i64::try_from(value.units).unwrap_or(i64::MAX)
}

fn le_bytes_to_i16(bytes: &[u8]) -> Vec<i16> {
    bytes
        .chunks_exact(2)
        .map(|chunk| i16::from_le_bytes([chunk[0], chunk[1]]))
        .collect()
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
