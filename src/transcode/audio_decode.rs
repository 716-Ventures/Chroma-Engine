use serde::{Deserialize, Serialize};
use thiserror::Error;

use crate::{
    codec::dts::{DtsCoreFrameHeader, DtsParseError, parse_dts_core_frames},
    packet::{ChunkSample, TimeDelta, TimePoint, TimeScale},
    transcode::{AudioClockConfig, AudioFrameTiming, AudioSampleClock, PcmAudioFormat},
};

/// Source audio codecs accepted by Chroma Engine's decode-facing bridge API.
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub enum AudioDecodeCodec {
    /// DTS or DTS-HD source packets with a DTS core substream.
    Dts,
    /// Dolby TrueHD/MLP source packets.
    TrueHd,
}

/// One decoded interleaved signed 16-bit PCM frame.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct DecodedPcmAudioFrame {
    /// Sample-clock timing for this PCM frame.
    pub timing: AudioFrameTiming,
    /// Interleaved signed 16-bit PCM samples.
    pub samples: Vec<i16>,
}

/// Decoded PCM stream description and frames.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct DecodedPcmAudioOutput {
    /// Stable PCM output shape.
    pub format: PcmAudioFormat,
    /// Decoded PCM frames.
    pub frames: Vec<DecodedPcmAudioFrame>,
}

/// Retained portable TrueHD decoder.
pub struct TrueHdAudioDecoderSession {
    extractor: truehd::process::extract::Extractor,
    parser: truehd::process::parse::Parser,
    decoder: truehd::process::decode::Decoder,
    format: Option<PcmAudioFormat>,
    clock: Option<AudioSampleClock>,
    pending_pts: std::collections::VecDeque<TimePoint>,
    decoded_batches: u64,
}

impl std::fmt::Debug for TrueHdAudioDecoderSession {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("TrueHdAudioDecoderSession")
            .field("format", &self.format)
            .field("pending_timestamps", &self.pending_pts.len())
            .field("decoded_batches", &self.decoded_batches)
            .finish_non_exhaustive()
    }
}

impl Default for TrueHdAudioDecoderSession {
    fn default() -> Self {
        Self::new()
    }
}

impl TrueHdAudioDecoderSession {
    /// Creates a portable TrueHD decoder with no stream state.
    pub fn new() -> Self {
        Self {
            extractor: truehd::process::extract::Extractor::default(),
            parser: truehd::process::parse::Parser::default(),
            decoder: truehd::process::decode::Decoder::default(),
            format: None,
            clock: None,
            pending_pts: std::collections::VecDeque::new(),
            decoded_batches: 0,
        }
    }

    /// Decodes one ordered TrueHD packet batch to interleaved signed 16-bit PCM.
    pub fn decode(
        &mut self,
        input: &AudioDecodeInput<'_>,
    ) -> Result<DecodedPcmAudioOutput, AudioDecodeError> {
        if input.codec != AudioDecodeCodec::TrueHd {
            return Err(AudioDecodeError::UnsupportedCodec);
        }
        if input.time_scale.units_per_second == 0 {
            return Err(AudioDecodeError::BackendFailed {
                reason: "packet time scale must be greater than zero".to_string(),
            });
        }
        for packet in &input.packets {
            if packet.pts.scale != input.time_scale
                || packet.dts.scale != input.time_scale
                || packet.duration.scale != input.time_scale
            {
                return Err(AudioDecodeError::BackendFailed {
                    reason: "packet timestamps must match the audio input time scale".to_string(),
                });
            }
            self.pending_pts.push_back(packet.pts);
            self.extractor.push_bytes(packet.bytes);
        }

        let mut frames = Vec::new();
        loop {
            let extracted = match self.extractor.next() {
                Some(Ok(frame)) => frame,
                Some(Err(truehd::utils::errors::ExtractError::InsufficientData)) | None => break,
                Some(Err(error)) => {
                    return Err(AudioDecodeError::BackendFailed {
                        reason: format!("TrueHD frame extraction failed: {error}"),
                    });
                }
            };
            let access_unit =
                self.parser
                    .parse(&extracted)
                    .map_err(|error| AudioDecodeError::BackendFailed {
                        reason: format!("TrueHD access-unit parse failed: {error}"),
                    })?;
            let decoded = self
                .decoder
                // Presentation 1 is the format-defined six-channel presentation. The
                // decoder resolves it to the closest available presentation, including
                // the stereo presentation or the embedded downmix of a larger stream.
                .decode_presentation(&access_unit, 1)
                .map_err(|error| AudioDecodeError::BackendFailed {
                    reason: format!("TrueHD decode failed: {error}"),
                })?;
            let source_pts = self.pending_pts.pop_front();
            if decoded.is_duplicate {
                continue;
            }
            let channels = u32::try_from(decoded.channel_count).map_err(|_| {
                AudioDecodeError::BackendFailed {
                    reason: "TrueHD channel count exceeds u32".to_string(),
                }
            })?;
            let format = PcmAudioFormat {
                sample_rate: decoded.sampling_frequency,
                channels,
            };
            if let Some(existing) = self.format {
                if existing != format || decoded.substream_info_changed {
                    return Err(AudioDecodeError::FormatChanged);
                }
            } else {
                self.format = Some(format);
                self.clock = Some(AudioSampleClock::new(AudioClockConfig {
                    sample_rate: format.sample_rate,
                    discontinuity_threshold_ms: 100,
                }));
            }
            let sample_count = u32::try_from(decoded.sample_length).map_err(|_| {
                AudioDecodeError::BackendFailed {
                    reason: "TrueHD sample count exceeds u32".to_string(),
                }
            })?;
            let capacity = decoded
                .sample_length
                .checked_mul(decoded.channel_count)
                .ok_or_else(|| AudioDecodeError::BackendFailed {
                    reason: "TrueHD PCM sample count overflowed".to_string(),
                })?;
            let mut samples = Vec::with_capacity(capacity);
            for sample in decoded.pcm_data.iter().take(decoded.sample_length) {
                samples.extend(sample.iter().take(decoded.channel_count).map(|value| {
                    (value >> 8).clamp(i32::from(i16::MIN), i32::from(i16::MAX)) as i16
                }));
            }
            let timing = self
                .clock
                .as_mut()
                .expect("clock initialized with TrueHD format")
                .stamp_frame(source_pts, sample_count);
            frames.push(DecodedPcmAudioFrame { timing, samples });
        }
        self.decoded_batches = self.decoded_batches.saturating_add(1);
        let format = self.format.ok_or(AudioDecodeError::NoFrames)?;
        Ok(DecodedPcmAudioOutput { format, frames })
    }

    /// Returns the number of packet batches decoded by this session.
    pub fn decoded_batches(&self) -> u64 {
        self.decoded_batches
    }
}

/// Decodes one TrueHD packet batch using portable CPU software.
pub fn decode_truehd_to_interleaved_i16(
    input: &AudioDecodeInput<'_>,
) -> Result<DecodedPcmAudioOutput, AudioDecodeError> {
    TrueHdAudioDecoderSession::new().decode(input)
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

    #[test]
    fn truehd_fixture_decodes_to_clocked_pcm() {
        let input = AudioDecodeInput {
            codec: AudioDecodeCodec::TrueHd,
            time_scale: TimeScale::MILLIS,
            packets: vec![CompressedAudioPacket {
                index: 0,
                pts: TimePoint::millis(500),
                dts: TimePoint::millis(500),
                duration: TimeDelta::millis(10),
                bytes: truehd::process::EXAMPLE_DATA,
            }],
            end_of_stream: true,
        };

        let output = decode_truehd_to_interleaved_i16(&input).expect("decode TrueHD fixture");

        assert!(!output.frames.is_empty());
        assert!(output.format.sample_rate > 0);
        assert!(output.format.channels > 0);
        assert_eq!(output.frames[0].timing.pts.as_millis(), 500);
        assert!(output.frames.iter().all(|frame| !frame.samples.is_empty()));
    }
}
