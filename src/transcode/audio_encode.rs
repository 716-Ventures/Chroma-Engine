use serde::{Deserialize, Serialize};
use thiserror::Error;

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

#[derive(Debug, Clone, Copy)]
enum DolbyCodec {
    Ac3,
    Eac3,
}

struct DolbyEncoderSession {
    format: PcmAudioFormat,
    bitrate: u32,
    codec: DolbyCodec,
    encoded_batches: u64,
    clock: AudioSampleClock,
    decoder_config: Option<Vec<u8>>,
    encoder: Box<dyn oxideav_core::Encoder>,
}

impl std::fmt::Debug for DolbyEncoderSession {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("DolbyEncoderSession")
            .field("format", &self.format)
            .field("bitrate", &self.bitrate)
            .field("codec", &self.codec)
            .field("encoded_batches", &self.encoded_batches)
            .field("next_sample", &self.clock.next_sample())
            .finish_non_exhaustive()
    }
}

impl DolbyEncoderSession {
    fn new(
        format: PcmAudioFormat,
        bitrate: u32,
        codec: DolbyCodec,
    ) -> Result<Self, AudioEncodeError> {
        validate_dolby_config(format, bitrate)?;
        let channels =
            u16::try_from(format.channels).map_err(|_| AudioEncodeError::InvalidInput {
                reason: format!("unsupported Dolby channel count {}", format.channels),
            })?;
        let codec_id = match codec {
            DolbyCodec::Ac3 => "ac3",
            DolbyCodec::Eac3 => "eac3",
        };
        let mut params = oxideav_core::CodecParameters::audio(oxideav_core::CodecId::new(codec_id));
        params.sample_rate = Some(format.sample_rate);
        params.channels = Some(channels);
        params.channel_layout = Some(oxideav_core::ChannelLayout::from_count(channels));
        params.sample_format = Some(oxideav_core::SampleFormat::S16);
        params.bit_rate = Some(u64::from(bitrate));
        let encoder = match codec {
            DolbyCodec::Ac3 => oxideav_ac3::encoder::make_encoder(&params),
            DolbyCodec::Eac3 => oxideav_ac3::eac3::make_encoder(&params),
        }
        .map_err(|error| AudioEncodeError::BackendUnavailable {
            reason: format!("portable {codec_id} encoder initialization failed: {error}"),
        })?;
        Ok(Self {
            format,
            bitrate,
            codec,
            encoded_batches: 0,
            clock: AudioSampleClock::new(AudioClockConfig {
                sample_rate: format.sample_rate,
                discontinuity_threshold_ms: 100,
            }),
            decoder_config: None,
            encoder,
        })
    }

    fn encode(&mut self, pcm: &[i16]) -> Result<EncodedAudioOutput, AudioEncodeError> {
        validate_pcm(self.format, pcm, self.bitrate)?;
        let sample_count = pcm.len() / self.format.channels as usize;
        let sample_count =
            u32::try_from(sample_count).map_err(|_| AudioEncodeError::InvalidInput {
                reason: "PCM batch contains more than u32::MAX samples per channel".to_string(),
            })?;
        let mut bytes = Vec::with_capacity(pcm.len().saturating_mul(2));
        for sample in pcm {
            bytes.extend_from_slice(&sample.to_le_bytes());
        }
        self.encoder
            .send_frame(&oxideav_core::Frame::Audio(oxideav_core::AudioFrame {
                samples: sample_count,
                pts: None,
                data: vec![bytes],
            }))
            .map_err(dolby_encode_error("PCM submission"))?;

        self.encoded_batches = self.encoded_batches.saturating_add(1);
        self.drain_packets()
    }

    fn finish(&mut self) -> Result<EncodedAudioOutput, AudioEncodeError> {
        self.encoder.flush().map_err(dolby_encode_error("flush"))?;
        self.drain_packets()
    }

    fn drain_packets(&mut self) -> Result<EncodedAudioOutput, AudioEncodeError> {
        let mut frames = Vec::new();
        loop {
            match self.encoder.receive_packet() {
                Ok(packet) => {
                    let frame_samples = u32::try_from(packet.duration.unwrap_or(1_536))
                        .unwrap_or(1_536)
                        .max(1);
                    if self.decoder_config.is_none() {
                        self.decoder_config = match self.codec {
                            DolbyCodec::Ac3 => {
                                crate::codec::ac3::parse_ac3_specific_box(&packet.data)
                                    .ok()
                                    .map(|config| config.dac3_payload().to_vec())
                            }
                            DolbyCodec::Eac3 => {
                                crate::codec::ac3::parse_eac3_specific_box(&packet.data)
                                    .ok()
                                    .map(|config| config.dec3_payload())
                            }
                        };
                    }
                    frames.push(frame_from_payload(
                        &mut self.clock,
                        packet.data,
                        frame_samples,
                    ));
                }
                Err(oxideav_core::Error::NeedMore) => break,
                Err(error) => return Err(dolby_encode_error("packet receive")(error)),
            }
        }
        Ok(EncodedAudioOutput {
            stream: EncodedAudioStream {
                codec: match self.codec {
                    DolbyCodec::Ac3 => AudioCodec::Ac3,
                    DolbyCodec::Eac3 => AudioCodec::Eac3,
                },
                sample_rate: self.format.sample_rate,
                channels: self.format.channels,
                decoder_config: self.decoder_config.clone(),
            },
            frames,
        })
    }
}

fn dolby_encode_error(
    operation: &'static str,
) -> impl FnOnce(oxideav_core::Error) -> AudioEncodeError {
    move |error| AudioEncodeError::BackendFailed {
        reason: format!("portable Dolby {operation} failed: {error}"),
    }
}

/// Retained portable AC-3 encoder for interleaved signed 16-bit PCM.
#[derive(Debug)]
pub struct CpuAc3EncoderSession(DolbyEncoderSession);

impl CpuAc3EncoderSession {
    /// Creates a portable AC-3 encoder for one to six channels.
    pub fn new(format: PcmAudioFormat, bitrate: u32) -> Result<Self, AudioEncodeError> {
        DolbyEncoderSession::new(format, bitrate, DolbyCodec::Ac3).map(Self)
    }

    /// Encodes one PCM batch while retaining codec and sample-clock state.
    pub fn encode(&mut self, pcm: &[i16]) -> Result<EncodedAudioOutput, AudioEncodeError> {
        self.0.encode(pcm)
    }

    /// Flushes a zero-padded partial syncframe and returns all remaining output.
    pub fn finish(&mut self) -> Result<EncodedAudioOutput, AudioEncodeError> {
        self.0.finish()
    }

    /// Returns the number of PCM batches submitted to this session.
    pub fn encoded_batches(&self) -> u64 {
        self.0.encoded_batches
    }
}

/// Retained portable E-AC-3 encoder for interleaved signed 16-bit PCM.
#[derive(Debug)]
pub struct CpuEac3EncoderSession(DolbyEncoderSession);

impl CpuEac3EncoderSession {
    /// Creates a portable E-AC-3 encoder for one to six channels.
    pub fn new(format: PcmAudioFormat, bitrate: u32) -> Result<Self, AudioEncodeError> {
        DolbyEncoderSession::new(format, bitrate, DolbyCodec::Eac3).map(Self)
    }

    /// Encodes one PCM batch while retaining codec and sample-clock state.
    pub fn encode(&mut self, pcm: &[i16]) -> Result<EncodedAudioOutput, AudioEncodeError> {
        self.0.encode(pcm)
    }

    /// Flushes a zero-padded partial syncframe and returns all remaining output.
    pub fn finish(&mut self) -> Result<EncodedAudioOutput, AudioEncodeError> {
        self.0.finish()
    }

    /// Returns the number of PCM batches submitted to this session.
    pub fn encoded_batches(&self) -> u64 {
        self.0.encoded_batches
    }
}

/// Portable, retained AAC-LC encoder facade backed by safe scalar Rust code.
pub struct CpuAacEncoderSession {
    encoder: rusty_aac::encode::stream::StreamingAacEncoder,
    format: PcmAudioFormat,
    bitrate: u32,
    encoded_batches: u64,
    clock: AudioSampleClock,
}

impl std::fmt::Debug for CpuAacEncoderSession {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("CpuAacEncoderSession")
            .field("format", &self.format)
            .field("bitrate", &self.bitrate)
            .field("encoded_batches", &self.encoded_batches)
            .field("next_sample", &self.clock.next_sample())
            .finish_non_exhaustive()
    }
}

impl CpuAacEncoderSession {
    /// Creates a portable AAC-LC encoder for one to six PCM channels.
    pub fn new(format: PcmAudioFormat, bitrate: u32) -> Result<Self, AudioEncodeError> {
        validate_cpu_aac_config(format, bitrate)?;
        Ok(Self {
            encoder: rusty_aac::encode::stream::StreamingAacEncoder::new(
                format.channels as u16,
                format.sample_rate,
                aac_lc_encode_bitrate(format, bitrate),
            )
            .map_err(cpu_aac_error)?,
            format,
            bitrate,
            encoded_batches: 0,
            clock: AudioSampleClock::new(AudioClockConfig {
                sample_rate: format.sample_rate,
                discontinuity_threshold_ms: 100,
            }),
        })
    }

    /// Encodes one PCM batch and advances this session's continuous sample clock.
    pub fn encode(&mut self, pcm: &[i16]) -> Result<EncodedAudioOutput, AudioEncodeError> {
        validate_pcm(self.format, pcm, self.bitrate)?;
        let output = encode_cpu_aac_batch(self, pcm)?;
        self.encoded_batches = self.encoded_batches.saturating_add(1);
        Ok(output)
    }

    /// Returns the number of batches encoded by this session.
    pub fn encoded_batches(&self) -> u64 {
        self.encoded_batches
    }

    /// Flushes residual PCM and MDCT overlap once, at actual end of stream.
    pub fn finish(&mut self) -> Result<EncodedAudioOutput, AudioEncodeError> {
        let packets = self.encoder.finish().map_err(cpu_aac_error)?;
        cpu_aac_output(self, packets)
    }

    /// Codec priming in samples per channel; not part of valid source PCM.
    pub fn delay_samples(&self) -> u32 {
        self.encoder.delay_samples()
    }

    /// Number of valid input PCM samples per channel.
    pub fn valid_samples(&self) -> u64 {
        self.encoder.valid_samples()
    }
}

/// Preferred AAC-LC encoder for the current host.
#[derive(Debug)]
pub enum AacEncoderSession {
    /// macOS AudioToolbox implementation.
    AudioToolbox(AudioToolboxAacEncoderSession),
    /// Portable safe-Rust implementation.
    Cpu(Box<CpuAacEncoderSession>),
}

impl AacEncoderSession {
    /// Creates the preferred host encoder, falling back to the portable CPU implementation.
    pub fn new(format: PcmAudioFormat, bitrate: u32) -> Result<Self, AudioEncodeError> {
        #[cfg(target_os = "macos")]
        if let Ok(session) = AudioToolboxAacEncoderSession::new(format, bitrate) {
            return Ok(Self::AudioToolbox(session));
        }
        CpuAacEncoderSession::new(format, bitrate).map(|session| Self::Cpu(Box::new(session)))
    }

    /// Encodes one interleaved signed 16-bit PCM batch.
    pub fn encode(&mut self, pcm: &[i16]) -> Result<EncodedAudioOutput, AudioEncodeError> {
        match self {
            Self::AudioToolbox(session) => session.encode(pcm),
            Self::Cpu(session) => session.encode(pcm),
        }
    }

    /// Returns the number of batches encoded by this session.
    pub fn encoded_batches(&self) -> u64 {
        match self {
            Self::AudioToolbox(session) => session.encoded_batches(),
            Self::Cpu(session) => session.encoded_batches(),
        }
    }

    /// Returns the stable capability name for the selected backend.
    pub fn backend_name(&self) -> &'static str {
        match self {
            Self::AudioToolbox(_) => "chroma-audiotoolbox-aac",
            Self::Cpu(_) => "chroma-cpu-aac",
        }
    }
}

/// Retained AudioToolbox AAC encoder with a continuous sample clock.
pub struct AudioToolboxAacEncoderSession {
    format: PcmAudioFormat,
    bitrate: u32,
    encoded_batches: u64,
    clock: AudioSampleClock,
    #[cfg(target_os = "macos")]
    destination: audiotoolbox::AudioStreamBasicDescription,
    #[cfg(target_os = "macos")]
    converter: audiotoolbox::AudioConverter,
}

impl std::fmt::Debug for AudioToolboxAacEncoderSession {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("AudioToolboxAacEncoderSession")
            .field("format", &self.format)
            .field("bitrate", &self.bitrate)
            .field("encoded_batches", &self.encoded_batches)
            .field("next_sample", &self.clock.next_sample())
            .finish_non_exhaustive()
    }
}

impl AudioToolboxAacEncoderSession {
    /// Creates one AAC encoder that can serve multiple PCM batches.
    pub fn new(format: PcmAudioFormat, bitrate: u32) -> Result<Self, AudioEncodeError> {
        validate_pcm_config(format, bitrate)?;
        platform_new_aac_encoder_session(format, bitrate)
    }

    /// Encodes one PCM batch while retaining converter and clock state.
    pub fn encode(&mut self, pcm: &[i16]) -> Result<EncodedAudioOutput, AudioEncodeError> {
        validate_pcm(self.format, pcm, self.bitrate)?;
        let output = platform_encode_aac_with_retained_session(self, pcm)?;
        self.encoded_batches = self.encoded_batches.saturating_add(1);
        Ok(output)
    }

    /// Returns the number of batches encoded by this native session.
    pub fn encoded_batches(&self) -> u64 {
        self.encoded_batches
    }
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

/// Encodes interleaved signed 16-bit PCM with the preferred host AAC-LC backend.
pub fn encode_aac_from_interleaved_i16(
    format: PcmAudioFormat,
    pcm: &[i16],
    bitrate: u32,
) -> Result<EncodedAudioOutput, AudioEncodeError> {
    let mut session = AacEncoderSession::new(format, bitrate)?;
    let mut output = session.encode(pcm)?;
    if let AacEncoderSession::Cpu(encoder) = &mut session {
        output.frames.extend(encoder.finish()?.frames);
    }
    Ok(output)
}

/// Encodes interleaved signed 16-bit PCM with the portable CPU AAC-LC backend.
pub fn encode_aac_cpu_from_interleaved_i16(
    format: PcmAudioFormat,
    pcm: &[i16],
    bitrate: u32,
) -> Result<EncodedAudioOutput, AudioEncodeError> {
    let mut encoder = CpuAacEncoderSession::new(format, bitrate)?;
    let mut output = encoder.encode(pcm)?;
    output.frames.extend(encoder.finish()?.frames);
    Ok(output)
}

/// Encodes interleaved signed 16-bit PCM with the portable AC-3 backend.
pub fn encode_ac3_cpu_from_interleaved_i16(
    format: PcmAudioFormat,
    pcm: &[i16],
    bitrate: u32,
) -> Result<EncodedAudioOutput, AudioEncodeError> {
    CpuAc3EncoderSession::new(format, bitrate)?.encode(pcm)
}

/// Encodes interleaved signed 16-bit PCM with the portable E-AC-3 backend.
pub fn encode_eac3_cpu_from_interleaved_i16(
    format: PcmAudioFormat,
    pcm: &[i16],
    bitrate: u32,
) -> Result<EncodedAudioOutput, AudioEncodeError> {
    CpuEac3EncoderSession::new(format, bitrate)?.encode(pcm)
}

fn validate_dolby_config(format: PcmAudioFormat, bitrate: u32) -> Result<(), AudioEncodeError> {
    validate_pcm_config(format, bitrate)?;
    if !matches!(format.sample_rate, 32_000 | 44_100 | 48_000) {
        return Err(AudioEncodeError::InvalidInput {
            reason: format!(
                "AC-3/E-AC-3 encode supports 32000, 44100, or 48000 Hz, got {}",
                format.sample_rate
            ),
        });
    }
    if !(1..=6).contains(&format.channels) {
        return Err(AudioEncodeError::InvalidInput {
            reason: format!(
                "AC-3/E-AC-3 encode supports one to six channels, got {}",
                format.channels
            ),
        });
    }
    Ok(())
}

fn validate_cpu_aac_config(format: PcmAudioFormat, bitrate: u32) -> Result<(), AudioEncodeError> {
    validate_pcm_config(format, bitrate)?;
    aac_lc_audio_specific_config(format)?;
    if format.channels > 6 {
        return Err(AudioEncodeError::UnsupportedAacConfig {
            reason: format!(
                "portable AAC-LC encode supports one to six channels, got {}",
                format.channels
            ),
        });
    }
    Ok(())
}

fn encode_cpu_aac_batch(
    retained: &mut CpuAacEncoderSession,
    pcm: &[i16],
) -> Result<EncodedAudioOutput, AudioEncodeError> {
    let samples = pcm
        .iter()
        .map(|sample| f32::from(*sample) / 32_768.0)
        .collect::<Vec<_>>();
    let packets = retained.encoder.push_pcm(&samples).map_err(cpu_aac_error)?;
    cpu_aac_output(retained, packets)
}

fn cpu_aac_output(
    retained: &mut CpuAacEncoderSession,
    packets: Vec<rusty_aac::encode::EncodedPacket>,
) -> Result<EncodedAudioOutput, AudioEncodeError> {
    let format = retained.format;
    let frames = packets
        .into_iter()
        .map(|packet| frame_from_payload(&mut retained.clock, packet.data, packet.duration))
        .collect();
    Ok(EncodedAudioOutput {
        stream: EncodedAudioStream {
            codec: AudioCodec::Aac,
            sample_rate: format.sample_rate,
            channels: format.channels,
            decoder_config: Some(aac_lc_audio_specific_config(format)?),
        },
        frames,
    })
}

fn cpu_aac_error(error: rusty_aac::Error) -> AudioEncodeError {
    AudioEncodeError::BackendFailed {
        reason: format!("portable AAC encoder: {error}"),
    }
}

fn validate_pcm(format: PcmAudioFormat, pcm: &[i16], bitrate: u32) -> Result<(), AudioEncodeError> {
    validate_pcm_config(format, bitrate)?;
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
    Ok(())
}

fn validate_pcm_config(format: PcmAudioFormat, bitrate: u32) -> Result<(), AudioEncodeError> {
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
    if bitrate == 0 {
        return Err(AudioEncodeError::InvalidInput {
            reason: "bitrate must be greater than zero".to_string(),
        });
    }
    Ok(())
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

fn aac_lc_encode_bitrate(format: PcmAudioFormat, requested: u32) -> u32 {
    let channel_scaled_max = 160_000_u32.saturating_mul(format.channels.max(1));
    requested.min(channel_scaled_max).clamp(64_000, 320_000)
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

#[cfg(target_os = "macos")]
fn platform_new_aac_encoder_session(
    format: PcmAudioFormat,
    bitrate: u32,
) -> Result<AudioToolboxAacEncoderSession, AudioEncodeError> {
    use audiotoolbox::{
        AUDIO_FORMAT_MPEG4_AAC, AudioConverter, AudioFormat, AudioStreamBasicDescription,
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
        .set_encode_bit_rate(aac_lc_encode_bitrate(format, bitrate))
        .map_err(|error| AudioEncodeError::BackendFailed {
            reason: error.to_string(),
        })?;
    Ok(AudioToolboxAacEncoderSession {
        format,
        bitrate,
        encoded_batches: 0,
        clock: AudioSampleClock::new(AudioClockConfig {
            sample_rate: format.sample_rate,
            discontinuity_threshold_ms: 100,
        }),
        destination,
        converter,
    })
}

#[cfg(target_os = "macos")]
fn platform_encode_aac_with_retained_session(
    retained: &mut AudioToolboxAacEncoderSession,
    pcm: &[i16],
) -> Result<EncodedAudioOutput, AudioEncodeError> {
    use audiotoolbox::{AudioConversionInput, AudioConverter};

    let format = retained.format;
    let destination = retained.destination;
    let converter: &AudioConverter = &retained.converter;
    if retained.encoded_batches > 0 {
        converter
            .reset()
            .map_err(|error| AudioEncodeError::BackendFailed {
                reason: error.to_string(),
            })?;
    }

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
        &mut retained.clock,
    )?;

    Ok(EncodedAudioOutput { stream, frames })
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
    clock: &mut AudioSampleClock,
) -> Result<Vec<EncodedAudioFrame>, AudioEncodeError> {
    split_audio_packets(
        data,
        packet_descriptions,
        packet_count,
        packet_samples,
        "AAC",
        clock,
    )
}

#[cfg(target_os = "macos")]
fn split_audio_packets(
    data: Vec<u8>,
    packet_descriptions: &[audiotoolbox::AudioStreamPacketDescription],
    packet_count: u32,
    packet_samples: u32,
    label: &str,
    clock: &mut AudioSampleClock,
) -> Result<Vec<EncodedAudioFrame>, AudioEncodeError> {
    if packet_descriptions.is_empty() {
        if data.is_empty() || packet_count == 0 {
            return Ok(Vec::new());
        }
        return Ok(vec![frame_from_payload(clock, data, packet_samples)]);
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
        frames.push(frame_from_payload(clock, payload, samples));
    }
    Ok(frames)
}

#[cfg(not(target_os = "macos"))]
fn platform_new_aac_encoder_session(
    _format: PcmAudioFormat,
    _bitrate: u32,
) -> Result<AudioToolboxAacEncoderSession, AudioEncodeError> {
    Err(AudioEncodeError::BackendUnavailable {
        reason: "AudioToolbox AAC encode is only available on macOS".to_string(),
    })
}

#[cfg(not(target_os = "macos"))]
fn platform_encode_aac_with_retained_session(
    _retained: &mut AudioToolboxAacEncoderSession,
    _pcm: &[i16],
) -> Result<EncodedAudioOutput, AudioEncodeError> {
    Err(AudioEncodeError::BackendUnavailable {
        reason: "AudioToolbox AAC encode is only available on macOS".to_string(),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn portable_ac3_encodes_parseable_syncframe() {
        let format = PcmAudioFormat {
            sample_rate: 48_000,
            channels: 2,
        };
        let pcm = vec![0_i16; 1_536 * 2];
        let encoded = encode_ac3_cpu_from_interleaved_i16(format, &pcm, 192_000).unwrap();

        assert_eq!(encoded.stream.codec, AudioCodec::Ac3);
        assert_eq!(encoded.frames.len(), 1);
        assert_eq!(encoded.frames[0].timing.sample_count, 1_536);
        assert!(crate::codec::ac3::parse_ac3_specific_box(&encoded.frames[0].payload).is_ok());
        assert!(encoded.stream.decoder_config.is_some());
    }

    #[test]
    fn portable_eac3_encodes_parseable_syncframe() {
        let format = PcmAudioFormat {
            sample_rate: 48_000,
            channels: 2,
        };
        let pcm = vec![0_i16; 1_536 * 2];
        let encoded = encode_eac3_cpu_from_interleaved_i16(format, &pcm, 192_000).unwrap();

        assert_eq!(encoded.stream.codec, AudioCodec::Eac3);
        assert_eq!(encoded.frames.len(), 1);
        assert_eq!(encoded.frames[0].timing.sample_count, 1_536);
        assert!(crate::codec::ac3::parse_eac3_specific_box(&encoded.frames[0].payload).is_ok());
        assert!(encoded.stream.decoder_config.is_some());
    }

    #[test]
    fn portable_eac3_finish_flushes_partial_syncframe() {
        let format = PcmAudioFormat {
            sample_rate: 48_000,
            channels: 2,
        };
        let mut session = CpuEac3EncoderSession::new(format, 192_000).unwrap();
        let pending = session.encode(&vec![0_i16; 512 * 2]).unwrap();
        assert!(pending.frames.is_empty());

        let flushed = session.finish().unwrap();
        assert_eq!(flushed.frames.len(), 1);
        assert_eq!(flushed.frames[0].timing.sample_count, 1_536);
        assert!(flushed.stream.decoder_config.is_some());
    }
    use crate::transcode::AudioClockConfig;

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

    #[test]
    fn portable_aac_encodes_decodable_raw_access_units() {
        let format = PcmAudioFormat {
            sample_rate: 48_000,
            channels: 2,
        };
        let pcm = sine_pcm(format, 2048);
        let encoded = encode_aac_cpu_from_interleaved_i16(format, &pcm, 128_000)
            .expect("portable AAC encode");

        assert_eq!(encoded.stream.codec, AudioCodec::Aac);
        assert_eq!(
            encoded.stream.decoder_config.as_deref(),
            Some(&[0x11, 0x90][..])
        );
        assert!(!encoded.frames.is_empty());

        let mut decoder = rusty_aac::AacDecoder::with_config_bytes(
            encoded
                .stream
                .decoder_config
                .as_deref()
                .expect("AAC config"),
        )
        .expect("construct AAC decoder");
        for frame in &encoded.frames {
            let decoded = decoder
                .decode(&frame.payload, None)
                .expect("decode portable AAC access unit");
            assert_eq!(decoded.sample_rate, format.sample_rate);
            assert_eq!(u32::from(decoded.channels), format.channels);
            assert!(!decoded.samples.is_empty());
        }
    }

    #[test]
    fn portable_aac_session_retains_batch_clock() {
        let format = PcmAudioFormat {
            sample_rate: 48_000,
            channels: 1,
        };
        let pcm = sine_pcm(format, 4096);
        let mut session = CpuAacEncoderSession::new(format, 96_000).expect("CPU AAC session");

        let first = session.encode(&pcm).expect("first batch");
        let second = session.encode(&pcm).expect("second batch");
        let first_end = first
            .frames
            .last()
            .expect("first frame")
            .timing
            .start_sample
            + u64::from(
                first
                    .frames
                    .last()
                    .expect("first frame")
                    .timing
                    .sample_count,
            );

        assert_eq!(session.encoded_batches(), 2);
        assert_eq!(
            second
                .frames
                .first()
                .expect("second frame")
                .timing
                .start_sample,
            first_end
        );
    }

    #[test]
    fn streaming_aac_is_identical_across_non_frame_aligned_pushes() {
        for sample_rate in [44_100, 48_000] {
            for channels in [2, 6] {
                let format = PcmAudioFormat {
                    sample_rate,
                    channels,
                };
                let pcm = sine_pcm(format, 8193);
                let mut reference = CpuAacEncoderSession::new(format, 320_000).unwrap();
                let mut expected = reference.encode(&pcm).unwrap().frames;
                expected.extend(reference.finish().unwrap().frames);
                let mut segmented = CpuAacEncoderSession::new(format, 320_000).unwrap();
                let mut actual = Vec::new();
                for chunk in pcm.chunks(137 * channels as usize) {
                    actual.extend(segmented.encode(chunk).unwrap().frames);
                }
                actual.extend(segmented.finish().unwrap().frames);
                assert!(segmented.finish().unwrap().frames.is_empty());
                assert_eq!(segmented.valid_samples(), 8193);
                assert_eq!(segmented.delay_samples(), 1024);
                assert_eq!(actual.len(), 8193_usize.div_ceil(1024) + 1);
                assert_eq!(actual, expected);
                let mut decoder = rusty_aac::AacDecoder::with_config_bytes(
                    &aac_lc_audio_specific_config(format).unwrap(),
                )
                .unwrap();
                for frame in actual {
                    assert!(
                        !decoder
                            .decode(&frame.payload, None)
                            .unwrap()
                            .samples
                            .is_empty()
                    );
                }
            }
        }
    }

    #[test]
    fn portable_aac_rejects_seven_channels() {
        let error = CpuAacEncoderSession::new(
            PcmAudioFormat {
                sample_rate: 48_000,
                channels: 7,
            },
            320_000,
        )
        .expect_err("reject unsupported channel layout");

        assert!(error.to_string().contains("one to six channels"));
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

    #[cfg(target_os = "macos")]
    #[test]
    fn macos_aac_session_reuses_converter_and_clock() {
        let format = PcmAudioFormat {
            sample_rate: 48_000,
            channels: 2,
        };
        let pcm = silent_pcm(format, 4096);
        let mut session = AudioToolboxAacEncoderSession::new(format, 128_000)
            .expect("create retained AAC session");

        let first = session.encode(&pcm).expect("encode first AAC batch");
        let second = session.encode(&pcm).expect("encode second AAC batch");

        assert_eq!(session.encoded_batches(), 2);
        let first_end = first
            .frames
            .last()
            .expect("first frame")
            .timing
            .start_sample
            + u64::from(
                first
                    .frames
                    .last()
                    .expect("first frame")
                    .timing
                    .sample_count,
            );
        assert_eq!(
            second
                .frames
                .first()
                .expect("second frame")
                .timing
                .start_sample,
            first_end
        );
    }

    #[cfg(target_os = "macos")]
    fn silent_pcm(format: PcmAudioFormat, frames: usize) -> Vec<i16> {
        vec![0; frames * format.channels as usize]
    }

    fn sine_pcm(format: PcmAudioFormat, frames: usize) -> Vec<i16> {
        (0..frames)
            .flat_map(|frame| {
                let phase =
                    (frame as f32 * 440.0 * std::f32::consts::TAU) / format.sample_rate as f32;
                std::iter::repeat_n((phase.sin() * 12_000.0) as i16, format.channels as usize)
            })
            .collect()
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
