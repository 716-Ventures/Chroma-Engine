//! Retained audio decode, PCM tail and AAC overlap across contiguous segments.
use super::*;
use crate::transcode::{
    CpuAacEncoderSession, DecodedPcmAudioFrame, DtsAudioDecoderSession, EncodedAudioFrame,
    OpusAudioDecoderSession, PcmAudioFormat, TrueHdAudioDecoderSession,
};
use std::collections::VecDeque;

enum Decoder {
    TrueHd(Box<TrueHdAudioDecoderSession>),
    Dts(Box<DtsAudioDecoderSession>),
    Opus(OpusAudioDecoderSession),
}

pub(super) struct AudioPipeline {
    decoder: Decoder,
    codec: AudioDecodeCodec,
    encoder: Option<CpuAacEncoderSession>,
    format: Option<PcmAudioFormat>,
    pcm: VecDeque<DecodedPcmAudioFrame>,
    next_pcm: Option<u64>,
    anchor: Option<u64>,
    last_offset: Option<u64>,
    completed: Option<u32>,
    poisoned: bool,
}

impl std::fmt::Debug for AudioPipeline {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("AudioPipeline")
            .field("completed", &self.completed)
            .field("poisoned", &self.poisoned)
            .finish_non_exhaustive()
    }
}

impl AudioPipeline {
    pub(super) fn new(track: &PreparedAudioTrack) -> Result<Option<Self>> {
        let (codec, decoder) = match track.codec.as_str() {
            "truehd" => (AudioDecodeCodec::TrueHd, Decoder::TrueHd(Box::default())),
            "dts" => (AudioDecodeCodec::Dts, Decoder::Dts(Box::default())),
            "opus" => (
                AudioDecodeCodec::Opus,
                Decoder::Opus(OpusAudioDecoderSession::new(
                    track
                        .codec_private
                        .as_deref()
                        .ok_or_else(|| anyhow::anyhow!("missing OpusHead"))?,
                )?),
            ),
            _ => return Ok(None),
        };
        Ok(Some(Self {
            decoder,
            codec,
            encoder: None,
            format: None,
            pcm: VecDeque::new(),
            next_pcm: None,
            anchor: None,
            last_offset: None,
            completed: None,
            poisoned: false,
        }))
    }

    pub(super) fn prepare(&mut self, index: u32, track: &PreparedAudioTrack) -> Result<()> {
        if self.poisoned
            || self
                .completed
                .is_some_and(|previous| previous.checked_add(1) != Some(index))
        {
            *self = Self::new(track)?.ok_or_else(|| anyhow::anyhow!("audio codec changed"))?;
        }
        self.poisoned = true;
        Ok(())
    }
    pub(super) fn complete(&mut self, index: u32) {
        self.completed = Some(index);
        self.poisoned = false;
    }

    pub(super) fn render(
        &mut self,
        manifest: ExtractedChunk,
        payload: &[u8],
        packets: &[PacketRef],
        bitrate: u32,
        eos: bool,
        source: &MappedMediaFile,
    ) -> Result<AudioSegment> {
        let mut input = build_audio_decode_input(
            self.codec,
            chunk_time_scale(&manifest),
            &manifest.samples,
            payload,
            eos,
        )?;
        input.packets.retain(|packet| {
            packets.get(packet.index as usize).is_some_and(|packet| {
                self.last_offset
                    .is_none_or(|last| packet.source_offset > last)
            })
        });
        let mut output = Vec::new();
        let mut output_bytes = 0usize;
        // One compressed access unit bounds decoder output bursts independently
        // of channel count and codec-specific frame duration.
        for chunk in input.packets.chunks(1) {
            source.check_work()?;
            let batch = crate::transcode::AudioDecodeInput {
                codec: input.codec,
                time_scale: input.time_scale,
                packets: chunk.to_vec(),
                end_of_stream: false,
            };
            let decoded = match &mut self.decoder {
                Decoder::TrueHd(decoder) => decoder.decode(&batch)?,
                Decoder::Dts(decoder) => decoder.decode(&batch)?,
                Decoder::Opus(decoder) => decoder.decode(&batch)?,
            };
            if self.format.is_some_and(|format| format != decoded.format) {
                bail!("audio format changed within retained stream");
            }
            let pcm_bytes = decoded
                .frames
                .iter()
                .chain(self.pcm.iter())
                .try_fold(0usize, |bytes, frame| {
                    bytes.checked_add(frame.samples.len().saturating_mul(2))
                })
                .ok_or_else(|| anyhow::anyhow!("PCM byte accounting overflow"))?;
            source.policy().check(
                "retained PCM",
                pcm_bytes,
                source.policy().decoded_batch_bytes,
            )?;
            self.format = Some(decoded.format);
            if self.encoder.is_none() {
                self.encoder = Some(CpuAacEncoderSession::new(decoded.format, bitrate)?);
            }
            self.pcm.extend(decoded.frames);
            self.last_offset = chunk
                .last()
                .and_then(|packet| packets.get(packet.index as usize))
                .map(|packet| packet.source_offset);
            let previous_frames = output.len();
            self.feed_pcm(&manifest, &mut output)?;
            output_bytes = output[previous_frames..]
                .iter()
                .try_fold(output_bytes, |bytes, frame| {
                    bytes.checked_add(frame.payload.len())
                })
                .ok_or_else(|| anyhow::anyhow!("AAC output accounting overflow"))?;
            source.policy().check(
                "encoded audio",
                output_bytes,
                source.policy().output_bytes / 2,
            )?;
        }
        self.feed_pcm(&manifest, &mut output)?;
        let encoder = self
            .encoder
            .as_mut()
            .ok_or_else(|| anyhow::anyhow!("audio decoder emitted no PCM"))?;
        if eos {
            output.extend(encoder.finish()?.frames);
        }
        let format = self
            .format
            .ok_or_else(|| anyhow::anyhow!("missing audio format"))?;
        let anchor = self
            .anchor
            .ok_or_else(|| anyhow::anyhow!("no PCM in segment window"))?;
        for frame in &mut output {
            frame.timing.start_sample = frame.timing.start_sample.saturating_add(anchor);
            frame.timing.pts.units = frame.timing.pts.units.saturating_add(anchor);
        }
        if output.is_empty() {
            bail!("audio window is too short for streaming AAC lookahead");
        }
        let decoder_config = rusty_aac::encode::audio_specific_config_bytes(
            format.sample_rate,
            format.channels as u16,
        );
        Ok(AudioSegment {
            fragment: fragment_track_from_encoded_audio_frames(AUDIO_TRACK_ID, &output)?,
            sample_entry: Fmp4SampleEntry::Aac {
                decoder_config,
                channel_count: format.channels as u16,
                sample_rate: format.sample_rate,
            },
            codec: "aac".into(),
            timescale: format.sample_rate,
            default_sample_duration: 1024,
            encoded_frame_count: output.len(),
            first_pts: output.first().map(|frame| frame.timing.pts),
        })
    }

    fn feed_pcm(
        &mut self,
        manifest: &ExtractedChunk,
        output: &mut Vec<EncodedAudioFrame>,
    ) -> Result<()> {
        let Some(format) = self.format else {
            return Ok(());
        };
        let start = rescale_units(
            manifest.chunk.start.units,
            manifest.chunk.start.scale,
            format.sample_rate,
        );
        let end = start.saturating_add(rescale_units(
            manifest.chunk.duration.units,
            manifest.chunk.duration.scale,
            format.sample_rate,
        ));
        while let Some(frame) = self.pcm.front() {
            let frame_start = frame.timing.start_sample;
            let frame_end = frame_start.saturating_add(u64::from(frame.timing.sample_count));
            if frame_start >= end {
                break;
            }
            let from = frame_start.max(self.next_pcm.unwrap_or(start));
            let to = frame_end.min(end);
            if from < to {
                if self.next_pcm.is_some_and(|next| from > next) {
                    bail!("audio PCM gap requires a new discontinuity");
                }
                self.anchor.get_or_insert(from);
                let channels = format.channels as usize;
                let lo = usize::try_from(from - frame_start)?
                    .checked_mul(channels)
                    .ok_or_else(|| anyhow::anyhow!("PCM offset overflow"))?;
                let hi = usize::try_from(to - frame_start)?
                    .checked_mul(channels)
                    .ok_or_else(|| anyhow::anyhow!("PCM offset overflow"))?;
                let pcm = frame
                    .samples
                    .get(lo..hi)
                    .ok_or_else(|| anyhow::anyhow!("PCM frame shorter than timing"))?;
                output.extend(
                    self.encoder
                        .as_mut()
                        .ok_or_else(|| anyhow::anyhow!("missing AAC encoder"))?
                        .encode(pcm)?
                        .frames,
                );
                self.next_pcm = Some(to);
            }
            if frame_end <= end {
                self.pcm.pop_front();
            } else {
                break;
            }
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn retained_dts_residual_pcm_matches_continuous_encode() {
        for rate in [44_100, 48_000] {
            let mut encoder =
                oxideav_dts::CoreEncoder::new(oxideav_dts::EncoderConfig::new(rate, 2).unwrap())
                    .unwrap();
            let pcm = (0..16_384)
                .map(|sample| (sample as f64 * 0.07).sin() * 0.1)
                .collect::<Vec<_>>();
            let mut encoded = encoder.push(&[&pcm, &pcm]).unwrap();
            encoded.extend(encoder.flush());
            let scale = TimeScale {
                units_per_second: rate,
            };
            let mut payload = Vec::new();
            let packets = encoded
                .iter()
                .enumerate()
                .map(|(index, bytes)| {
                    let offset = payload.len() as u64;
                    payload.extend_from_slice(bytes);
                    PacketRef {
                        source_offset: offset,
                        size: bytes.len() as u32,
                        pts: TimePoint {
                            units: index as u64 * 512,
                            scale,
                        },
                        dts: TimePoint {
                            units: index as u64 * 512,
                            scale,
                        },
                        duration: TimeDelta { units: 512, scale },
                        keyframe: true,
                    }
                })
                .collect::<Vec<_>>();
            let manifest = |start, duration, count: u32| ExtractedChunk {
                track_id: "a0".into(),
                packet_count: count,
                byte_count: payload.len() as u64,
                chunk: NativeChunk {
                    index: 0,
                    start: TimePoint {
                        units: start,
                        scale,
                    },
                    duration: TimeDelta {
                        units: duration,
                        scale,
                    },
                    packet_range: PacketRange {
                        start: 0,
                        end: count,
                    },
                    key_aligned: true,
                },
                samples: packet_samples_for_range(
                    &packets,
                    PacketRange {
                        start: 0,
                        end: count,
                    },
                )
                .unwrap(),
            };
            let track = PreparedAudioTrack {
                codec: "dts".into(),
                channels: 2,
                sample_rate: rate,
                codec_private: None,
            };
            let file = tempfile::NamedTempFile::new().unwrap();
            let source = MappedMediaFile::open_packet_copy(file.path()).unwrap();
            let total = packets.len() as u64 * 512;
            let mut continuous = AudioPipeline::new(&track).unwrap().unwrap();
            continuous.prepare(0, &track).unwrap();
            let expected = continuous
                .render(
                    manifest(0, total, packets.len() as u32),
                    &payload,
                    &packets,
                    128_000,
                    true,
                    &source,
                )
                .unwrap();
            let mut segmented = AudioPipeline::new(&track).unwrap().unwrap();
            segmented.prepare(0, &track).unwrap();
            let first = segmented
                .render(
                    manifest(0, 4097, 10),
                    &payload,
                    &packets,
                    128_000,
                    false,
                    &source,
                )
                .unwrap();
            segmented.complete(0);
            segmented.prepare(1, &track).unwrap();
            let second = segmented
                .render(
                    manifest(4097, total - 4097, packets.len() as u32),
                    &payload,
                    &packets,
                    128_000,
                    true,
                    &source,
                )
                .unwrap();
            assert_eq!(
                [first.fragment.payload, second.fragment.payload].concat(),
                expected.fragment.payload
            );
        }
    }

    #[test]
    fn retained_opus_to_aac_matches_continuous_decode_and_encode() {
        let mut head = b"OpusHead".to_vec();
        head.extend_from_slice(&[1, 2, 0, 0, 0x80, 0xbb, 0, 0, 0, 0, 0]);
        let track = PreparedAudioTrack {
            codec: "opus".into(),
            codec_private: Some(head),
            channels: 2,
            sample_rate: 48_000,
        };
        let file = tempfile::NamedTempFile::new().unwrap();
        let source = MappedMediaFile::open_packet_copy(file.path()).unwrap();
        let packets = (0..100)
            .map(|i| PacketRef {
                source_offset: i * 3,
                size: 3,
                pts: TimePoint::millis(i * 20),
                dts: TimePoint::millis(i * 20),
                duration: TimeDelta::millis(20),
                keyframe: true,
            })
            .collect::<Vec<_>>();
        let payload = [0xf8, 0xff, 0xfe].repeat(100);
        let manifest = |start: u64, duration: u64, count: u32| ExtractedChunk {
            track_id: "a0".into(),
            packet_count: count,
            byte_count: u64::from(count) * 3,
            chunk: NativeChunk {
                index: (start / 1000) as u32,
                start: TimePoint::millis(start),
                duration: TimeDelta::millis(duration),
                packet_range: PacketRange {
                    start: 0,
                    end: count,
                },
                key_aligned: true,
            },
            samples: packet_samples_for_range(
                &packets,
                PacketRange {
                    start: 0,
                    end: count,
                },
            )
            .unwrap(),
        };
        let mut continuous = AudioPipeline::new(&track).unwrap().unwrap();
        continuous.prepare(0, &track).unwrap();
        let expected = continuous
            .render(
                manifest(0, 2000, 100),
                &payload,
                &packets,
                128_000,
                true,
                &source,
            )
            .unwrap();
        let mut segmented = AudioPipeline::new(&track).unwrap().unwrap();
        segmented.prepare(0, &track).unwrap();
        let first = segmented
            .render(
                manifest(0, 1000, 55),
                &payload,
                &packets,
                128_000,
                false,
                &source,
            )
            .unwrap();
        segmented.complete(0);
        segmented.prepare(1, &track).unwrap();
        let second = segmented
            .render(
                manifest(1000, 1000, 100),
                &payload,
                &packets,
                128_000,
                true,
                &source,
            )
            .unwrap();
        assert_eq!(
            [first.fragment.payload, second.fragment.payload].concat(),
            expected.fragment.payload
        );
        let end = first.fragment.base_decode_time
            + first
                .fragment
                .samples
                .iter()
                .map(|sample| u64::from(sample.duration))
                .sum::<u64>();
        assert_eq!(end, second.fragment.base_decode_time);
        assert_eq!(
            first.fragment.samples.len() + second.fragment.samples.len(),
            expected.fragment.samples.len()
        );
        // A failed or discontinuous request must discard every decoder/PCM/AAC state.
        segmented.prepare(1, &track).unwrap();
        assert!(segmented.encoder.is_none());
        assert!(segmented.last_offset.is_none());
    }
}
