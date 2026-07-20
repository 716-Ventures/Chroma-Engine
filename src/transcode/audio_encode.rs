use serde::{Deserialize, Serialize};
use thiserror::Error;

use oxideav_core::{AudioFrame, CodecId, CodecParameters, Frame, SampleFormat};

use crate::transcode::{
    AudioClockConfig, AudioCodec, AudioSampleClock, EncodedAudioFrame, EncodedAudioStream,
};

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
/// PCM input format accepted by native audio encoders.
pub struct PcmAudioFormat {
    /// PCM sample rate in Hz.
    pub sample_rate: u32,
    /// Interleaved channel count.
    pub channels: u32,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
/// Encoded audio stream description plus emitted access units.
pub struct EncodedAudioOutput {
    /// Output stream description.
    pub stream: EncodedAudioStream,
    /// Encoded access units.
    pub frames: Vec<EncodedAudioFrame>,
}

#[derive(Debug, Error)]
/// Error returned by native audio encode backends.
pub enum AudioEncodeError {
    /// The requested PCM input shape is invalid or unsupported.
    #[error("invalid PCM input: {reason}")]
    InvalidInput {
        /// Diagnostic reason.
        reason: String,
    },
    /// The requested AAC output cannot be represented in MPEG-4 AudioSpecificConfig.
    #[error("unsupported AAC config: {reason}")]
    UnsupportedAacConfig {
        /// Diagnostic reason.
        reason: String,
    },
    /// No native backend is available on this platform.
    #[error("native audio encode backend is unavailable: {reason}")]
    BackendUnavailable {
        /// Diagnostic reason.
        reason: String,
    },
    /// The native backend returned an error.
    #[error("native audio encode failed: {reason}")]
    BackendFailed {
        /// Diagnostic reason.
        reason: String,
    },
}

/// Encodes interleaved signed 16-bit PCM to AAC-LC using the native host backend.
pub fn encode_aac_from_interleaved_i16(
    format: PcmAudioFormat,
    pcm: &[i16],
    bitrate: u32,
) -> Result<EncodedAudioOutput, AudioEncodeError> {
    validate_pcm(format, pcm, bitrate)?;
    platform_encode_aac_from_interleaved_i16(format, pcm, bitrate)
}

/// Encodes interleaved signed 16-bit PCM to AC-3 using Chroma Engine's Rust bridge backend.
pub fn encode_ac3_from_interleaved_i16(
    format: PcmAudioFormat,
    pcm: &[i16],
    bitrate: u32,
) -> Result<EncodedAudioOutput, AudioEncodeError> {
    validate_pcm(format, pcm, bitrate)?;
    validate_dolby_bridge_format(format)?;
    platform_encode_ac3_from_interleaved_i16(format, pcm, bitrate)
}

/// Encodes interleaved signed 16-bit PCM to E-AC-3 using Chroma Engine's Rust bridge backend.
pub fn encode_eac3_from_interleaved_i16(
    format: PcmAudioFormat,
    pcm: &[i16],
    bitrate: u32,
) -> Result<EncodedAudioOutput, AudioEncodeError> {
    validate_pcm(format, pcm, bitrate)?;
    validate_dolby_bridge_format(format)?;
    platform_encode_eac3_from_interleaved_i16(format, pcm, bitrate)
}

fn validate_pcm(format: PcmAudioFormat, pcm: &[i16], bitrate: u32) -> Result<(), AudioEncodeError> {
    if format.sample_rate == 0 {
        return Err(AudioEncodeError::InvalidInput {
            reason: "sample_rate must be greater than zero".to_string(),
        });
    }
    if format.channels == 0 {
        return Err(AudioEncodeError::InvalidInput {
            reason: "channels must be greater than zero".to_string(),
        });
    }
    if pcm.is_empty() {
        return Err(AudioEncodeError::InvalidInput {
            reason: "PCM buffer is empty".to_string(),
        });
    }
    if !pcm.len().is_multiple_of(format.channels as usize) {
        return Err(AudioEncodeError::InvalidInput {
            reason: "PCM sample count must align to the interleaved channel count".to_string(),
        });
    }
    if bitrate == 0 {
        return Err(AudioEncodeError::InvalidInput {
            reason: "bitrate must be greater than zero".to_string(),
        });
    }
    Ok(())
}

fn validate_dolby_bridge_format(format: PcmAudioFormat) -> Result<(), AudioEncodeError> {
    if format.channels > 6 {
        return Err(AudioEncodeError::InvalidInput {
            reason: "AC-3/E-AC-3 bridge output supports at most 6 channels".to_string(),
        });
    }
    match format.sample_rate {
        32_000 | 44_100 | 48_000 => Ok(()),
        _ => Err(AudioEncodeError::InvalidInput {
            reason: format!(
                "AC-3/E-AC-3 bridge output does not support {} Hz",
                format.sample_rate
            ),
        }),
    }
}

fn aac_lc_audio_specific_config(format: PcmAudioFormat) -> Result<Vec<u8>, AudioEncodeError> {
    let sample_rate_index = aac_sample_rate_index(format.sample_rate).ok_or_else(|| {
        AudioEncodeError::UnsupportedAacConfig {
            reason: format!("unsupported AAC-LC sample rate {}", format.sample_rate),
        }
    })?;
    let channels =
        u8::try_from(format.channels).map_err(|_| AudioEncodeError::UnsupportedAacConfig {
            reason: format!("unsupported AAC-LC channel count {}", format.channels),
        })?;
    if channels > 7 {
        return Err(AudioEncodeError::UnsupportedAacConfig {
            reason: format!("unsupported AAC-LC channel count {}", format.channels),
        });
    }

    let audio_object_type = 2_u8;
    Ok(vec![
        (audio_object_type << 3) | (sample_rate_index >> 1),
        ((sample_rate_index & 1) << 7) | (channels << 3),
    ])
}

fn aac_sample_rate_index(sample_rate: u32) -> Option<u8> {
    [
        96_000, 88_200, 64_000, 48_000, 44_100, 32_000, 24_000, 22_050, 16_000, 12_000, 11_025,
        8_000, 7_350,
    ]
    .iter()
    .position(|rate| *rate == sample_rate)
    .and_then(|index| u8::try_from(index).ok())
}

fn frame_from_payload(
    clock: &mut AudioSampleClock,
    payload: Vec<u8>,
    packet_samples: u32,
) -> EncodedAudioFrame {
    EncodedAudioFrame {
        timing: clock.stamp_frame(None, packet_samples),
        payload,
        discontinuity: false,
    }
}

const DOLBY_AUDIO_FRAMES_PER_PACKET: u32 = 1536;

#[cfg(target_os = "macos")]
fn platform_encode_aac_from_interleaved_i16(
    format: PcmAudioFormat,
    pcm: &[i16],
    bitrate: u32,
) -> Result<EncodedAudioOutput, AudioEncodeError> {
    use audiotoolbox::{
        AUDIO_FORMAT_MPEG4_AAC, AudioConversionInput, AudioConverter, AudioFormat,
        AudioStreamBasicDescription,
    };

    let source = AudioStreamBasicDescription::linear_pcm_i16(
        f64::from(format.sample_rate),
        format.channels,
        true,
    );
    let destination = AudioFormat::format_info(AudioStreamBasicDescription {
        mSampleRate: f64::from(format.sample_rate),
        mFormatID: AUDIO_FORMAT_MPEG4_AAC,
        mFormatFlags: 0,
        mBytesPerPacket: 0,
        mFramesPerPacket: 1024,
        mBytesPerFrame: 0,
        mChannelsPerFrame: format.channels,
        mBitsPerChannel: 0,
        mReserved: 0,
    })
    .map_err(|error| AudioEncodeError::BackendFailed {
        reason: error.to_string(),
    })?;
    let converter = AudioConverter::new(&source, &destination).map_err(|error| {
        AudioEncodeError::BackendFailed {
            reason: error.to_string(),
        }
    })?;
    converter
        .set_encode_bit_rate(bitrate)
        .map_err(|error| AudioEncodeError::BackendFailed {
            reason: error.to_string(),
        })?;

    let pcm_bytes = i16_slice_as_ne_bytes(pcm);
    let input_frames = u32::try_from(pcm.len() / format.channels as usize).map_err(|_| {
        AudioEncodeError::InvalidInput {
            reason: "PCM frame count exceeds UInt32::MAX".to_string(),
        }
    })?;
    let output_packet_capacity = input_frames.div_ceil(destination.mFramesPerPacket.max(1)) + 8;
    let encoded = converter
        .fill_complex_buffer_once(
            AudioConversionInput {
                data: &pcm_bytes,
                packet_count: input_frames,
                packet_descriptions: None,
                channels: format.channels,
            },
            output_packet_capacity.max(1),
        )
        .map_err(|error| AudioEncodeError::BackendFailed {
            reason: error.to_string(),
        })?;

    let decoder_config = aac_lc_audio_specific_config(format)?;
    let stream = EncodedAudioStream {
        codec: AudioCodec::Aac,
        sample_rate: format.sample_rate,
        channels: format.channels,
        decoder_config: Some(decoder_config),
    };
    let frames = split_aac_packets(
        encoded.data,
        &encoded.packet_descriptions,
        encoded.packet_count,
        destination.mFramesPerPacket.max(1),
        format,
    )?;

    Ok(EncodedAudioOutput { stream, frames })
}

fn platform_encode_ac3_from_interleaved_i16(
    format: PcmAudioFormat,
    pcm: &[i16],
    bitrate: u32,
) -> Result<EncodedAudioOutput, AudioEncodeError> {
    encode_oxideav_dolby_from_interleaved_i16(format, pcm, bitrate, AudioCodec::Ac3)
}

fn platform_encode_eac3_from_interleaved_i16(
    format: PcmAudioFormat,
    pcm: &[i16],
    bitrate: u32,
) -> Result<EncodedAudioOutput, AudioEncodeError> {
    encode_oxideav_dolby_from_interleaved_i16(format, pcm, bitrate, AudioCodec::Eac3)
}

fn encode_oxideav_dolby_from_interleaved_i16(
    format: PcmAudioFormat,
    pcm: &[i16],
    bitrate: u32,
    codec: AudioCodec,
) -> Result<EncodedAudioOutput, AudioEncodeError> {
    let codec_id = match codec {
        AudioCodec::Ac3 => "ac3",
        AudioCodec::Eac3 => "eac3",
        AudioCodec::Aac => unreachable!("AAC uses the AAC encoder path"),
    };
    let channels = u16::try_from(format.channels).map_err(|_| AudioEncodeError::InvalidInput {
        reason: "channel count exceeds u16::MAX".to_string(),
    })?;
    let mut params = CodecParameters::audio(CodecId::new(codec_id));
    params.sample_rate = Some(format.sample_rate);
    params.channels = Some(channels);
    params.sample_format = Some(SampleFormat::S16);
    params.bit_rate = Some(u64::from(bitrate));

    let mut encoder = match codec {
        AudioCodec::Ac3 => oxideav_ac3::encoder::make_encoder(&params),
        AudioCodec::Eac3 => oxideav_ac3::eac3::make_encoder(&params),
        AudioCodec::Aac => unreachable!("AAC uses the AAC encoder path"),
    }
    .map_err(|error| AudioEncodeError::BackendFailed {
        reason: error.to_string(),
    })?;

    let pcm_bytes = i16_slice_as_le_bytes(pcm);
    let input_frames = u32::try_from(pcm.len() / format.channels as usize).map_err(|_| {
        AudioEncodeError::InvalidInput {
            reason: "PCM frame count exceeds UInt32::MAX".to_string(),
        }
    })?;
    encoder
        .send_frame(&Frame::Audio(AudioFrame {
            samples: input_frames,
            pts: Some(0),
            data: vec![pcm_bytes],
        }))
        .map_err(|error| AudioEncodeError::BackendFailed {
            reason: error.to_string(),
        })?;
    encoder
        .flush()
        .map_err(|error| AudioEncodeError::BackendFailed {
            reason: error.to_string(),
        })?;

    let stream = EncodedAudioStream {
        codec,
        sample_rate: format.sample_rate,
        channels: format.channels,
        decoder_config: None,
    };
    let mut clock = AudioSampleClock::new(AudioClockConfig {
        sample_rate: format.sample_rate,
        discontinuity_threshold_ms: 100,
    });
    let mut frames = Vec::with_capacity((input_frames / DOLBY_AUDIO_FRAMES_PER_PACKET) as usize);
    loop {
        match encoder.receive_packet() {
            Ok(packet) => frames.push(frame_from_payload(
                &mut clock,
                packet.data,
                DOLBY_AUDIO_FRAMES_PER_PACKET,
            )),
            Err(error) if error.is_need_more() || error.is_eof() => break,
            Err(error) => {
                return Err(AudioEncodeError::BackendFailed {
                    reason: error.to_string(),
                });
            }
        }
    }

    Ok(EncodedAudioOutput { stream, frames })
}

fn i16_slice_as_le_bytes(pcm: &[i16]) -> Vec<u8> {
    let mut out = Vec::with_capacity(std::mem::size_of_val(pcm));
    for sample in pcm {
        out.extend_from_slice(&sample.to_le_bytes());
    }
    out
}

#[cfg(target_os = "macos")]
fn i16_slice_as_ne_bytes(pcm: &[i16]) -> Vec<u8> {
    let mut out = Vec::with_capacity(std::mem::size_of_val(pcm));
    for sample in pcm {
        out.extend_from_slice(&sample.to_ne_bytes());
    }
    out
}

#[cfg(target_os = "macos")]
fn split_aac_packets(
    data: Vec<u8>,
    packet_descriptions: &[audiotoolbox::AudioStreamPacketDescription],
    packet_count: u32,
    packet_samples: u32,
    format: PcmAudioFormat,
) -> Result<Vec<EncodedAudioFrame>, AudioEncodeError> {
    split_audio_packets(
        data,
        packet_descriptions,
        packet_count,
        packet_samples,
        format,
        "AAC",
    )
}

#[cfg(target_os = "macos")]
fn split_audio_packets(
    data: Vec<u8>,
    packet_descriptions: &[audiotoolbox::AudioStreamPacketDescription],
    packet_count: u32,
    packet_samples: u32,
    format: PcmAudioFormat,
    label: &str,
) -> Result<Vec<EncodedAudioFrame>, AudioEncodeError> {
    let mut clock = AudioSampleClock::new(AudioClockConfig {
        sample_rate: format.sample_rate,
        discontinuity_threshold_ms: 100,
    });
    if packet_descriptions.is_empty() {
        if data.is_empty() || packet_count == 0 {
            return Ok(Vec::new());
        }
        return Ok(vec![frame_from_payload(&mut clock, data, packet_samples)]);
    }

    let mut frames = Vec::with_capacity(packet_descriptions.len());
    for desc in packet_descriptions.iter().take(packet_count as usize) {
        let start =
            usize::try_from(desc.mStartOffset).map_err(|_| AudioEncodeError::BackendFailed {
                reason: format!("AudioToolbox returned a negative {label} packet offset"),
            })?;
        let size = desc.mDataByteSize as usize;
        let end = start
            .checked_add(size)
            .ok_or_else(|| AudioEncodeError::BackendFailed {
                reason: format!("AudioToolbox {label} packet range overflowed"),
            })?;
        let payload = data
            .get(start..end)
            .ok_or_else(|| AudioEncodeError::BackendFailed {
                reason: format!("AudioToolbox {label} packet range exceeded output buffer"),
            })?
            .to_vec();
        let samples = if desc.mVariableFramesInPacket == 0 {
            packet_samples
        } else {
            desc.mVariableFramesInPacket
        };
        frames.push(frame_from_payload(&mut clock, payload, samples));
    }
    Ok(frames)
}

#[cfg(not(target_os = "macos"))]
fn platform_encode_aac_from_interleaved_i16(
    _format: PcmAudioFormat,
    _pcm: &[i16],
    _bitrate: u32,
) -> Result<EncodedAudioOutput, AudioEncodeError> {
    Err(AudioEncodeError::BackendUnavailable {
        reason: "AudioToolbox AAC encode is only available on macOS".to_string(),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn builds_aac_lc_audio_specific_config() {
        let config = aac_lc_audio_specific_config(PcmAudioFormat {
            sample_rate: 48_000,
            channels: 2,
        })
        .unwrap();

        assert_eq!(config, vec![0x11, 0x90]);
    }

    #[test]
    fn rejects_pcm_that_does_not_align_to_channels() {
        let err = encode_aac_from_interleaved_i16(
            PcmAudioFormat {
                sample_rate: 48_000,
                channels: 2,
            },
            &[0, 1, 2],
            128_000,
        )
        .expect_err("reject misaligned PCM");

        assert!(err.to_string().contains("align"));
    }

    #[cfg(target_os = "macos")]
    #[test]
    fn macos_audiotoolbox_encodes_aac_packets() {
        let format = PcmAudioFormat {
            sample_rate: 48_000,
            channels: 2,
        };
        let pcm = silent_pcm(format, 4096);

        let encoded = encode_aac_from_interleaved_i16(format, &pcm, 128_000)
            .expect("AudioToolbox AAC encode");

        assert_eq!(encoded.stream.codec, AudioCodec::Aac);
        assert_eq!(
            encoded.stream.decoder_config.as_deref(),
            Some(&[0x11, 0x90][..])
        );
        assert!(!encoded.frames.is_empty());
        assert!(encoded.frames.iter().all(|frame| !frame.payload.is_empty()));
        for pair in encoded.frames.windows(2) {
            assert_eq!(
                pair[0].timing.start_sample + u64::from(pair[0].timing.sample_count),
                pair[1].timing.start_sample
            );
        }
    }

    #[test]
    fn oxideav_encodes_ac3_packets() {
        let format = PcmAudioFormat {
            sample_rate: 48_000,
            channels: 2,
        };
        let pcm = silent_pcm(format, 4096);

        let encoded =
            encode_ac3_from_interleaved_i16(format, &pcm, 192_000).expect("oxideav AC-3 encode");

        assert_eq!(encoded.stream.codec, AudioCodec::Ac3);
        assert_eq!(encoded.stream.decoder_config, None);
        assert!(!encoded.frames.is_empty());
        assert!(encoded.frames.iter().all(|frame| {
            frame.payload.starts_with(&[0x0b, 0x77]) && frame.timing.sample_count == 1536
        }));
        assert_contiguous_audio_frames(&encoded.frames);
    }

    #[test]
    fn oxideav_encodes_eac3_packets() {
        let format = PcmAudioFormat {
            sample_rate: 48_000,
            channels: 6,
        };
        let pcm = silent_pcm(format, 4608);

        let encoded =
            encode_eac3_from_interleaved_i16(format, &pcm, 768_000).expect("oxideav E-AC-3 encode");

        assert_eq!(encoded.stream.codec, AudioCodec::Eac3);
        assert_eq!(encoded.stream.decoder_config, None);
        assert!(!encoded.frames.is_empty());
        assert!(encoded.frames.iter().all(|frame| {
            frame.payload.starts_with(&[0x0b, 0x77]) && frame.timing.sample_count == 1536
        }));
        assert_contiguous_audio_frames(&encoded.frames);
    }

    #[test]
    fn rejects_dolby_bridge_unsupported_channel_count() {
        let err = encode_eac3_from_interleaved_i16(
            PcmAudioFormat {
                sample_rate: 48_000,
                channels: 8,
            },
            &[0; 8 * 1536],
            1_024_000,
        )
        .expect_err("reject too many bridge channels");

        assert!(err.to_string().contains("at most 6 channels"));
    }

    fn silent_pcm(format: PcmAudioFormat, frames: usize) -> Vec<i16> {
        vec![0; frames * format.channels as usize]
    }

    fn assert_contiguous_audio_frames(frames: &[EncodedAudioFrame]) {
        for pair in frames.windows(2) {
            assert_eq!(
                pair[0].timing.start_sample + u64::from(pair[0].timing.sample_count),
                pair[1].timing.start_sample
            );
        }
    }

    #[test]
    fn frame_from_payload_uses_sample_clock() {
        let mut clock = AudioSampleClock::new(AudioClockConfig::default());

        let first = frame_from_payload(&mut clock, vec![1], 1024);
        let second = frame_from_payload(&mut clock, vec![2], 1024);

        assert_eq!(first.timing.start_sample, 0);
        assert_eq!(second.timing.start_sample, 1024);
    }

    #[test]
    fn unsupported_aac_sample_rate_is_rejected() {
        let err = aac_lc_audio_specific_config(PcmAudioFormat {
            sample_rate: 12_345,
            channels: 2,
        })
        .expect_err("reject unsupported rate");

        assert!(err.to_string().contains("sample rate"));
    }
}
