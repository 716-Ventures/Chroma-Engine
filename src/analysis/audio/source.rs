//! Selected native audio packets only; no video decode, external process or full-file PCM.
use super::fingerprint::{Builder, Window};
use crate::{
    container::{matroska, mp4},
    packet::PacketRef,
    source::MediaSource,
};
use anyhow::{Context, Result, bail, ensure};
use std::{
    path::Path,
    time::{Duration, Instant},
};

#[derive(Clone)]
pub(super) struct Spec {
    pub id: String,
    pub codec: String,
    pub language: Option<String>,
    pub config: Vec<u8>,
    pub sample_rate: u32,
    pub duration_ms: u64,
}
fn language(value: Option<&str>) -> Option<String> {
    value.map(|v| match v.to_ascii_lowercase().as_str() {
        "jpn" | "ja" => "ja".into(),
        "eng" | "en" => "en".into(),
        "und" => "und".into(),
        _ => v.to_ascii_lowercase(),
    })
}
fn supported(codec: &str) -> bool {
    matches!(codec, "aac" | "ac3" | "eac3" | "opus" | "dts" | "truehd")
}

pub(super) fn open(path: &Path, wanted: Option<&str>) -> Result<(MediaSource, Spec)> {
    let source = MediaSource::open_with_context(
        path,
        crate::EngineRuntime::global()?,
        crate::WorkControl::with_deadline(Instant::now() + Duration::from_secs(240)),
    )?;
    let (duration, mut tracks) = if matroska::looks_like_ebml(source.as_ref()) {
        let metadata = matroska::parse_basic_metadata(source.as_ref());
        let duration = metadata
            .duration_ms
            .context("Audio analysis requires source duration")?;
        let tracks = metadata
            .tracks
            .iter()
            .filter(|t| t.kind == matroska::MatroskaTrackKind::Audio)
            .enumerate()
            .map(|(i, t)| {
                (
                    Spec {
                        id: format!("a{i}"),
                        codec: t.codec.clone(),
                        language: language(t.language.as_deref()),
                        config: t.codec_private.clone().unwrap_or_default(),
                        sample_rate: t.sample_rate.unwrap_or(48_000),
                        duration_ms: duration,
                    },
                    t.default,
                )
            })
            .collect::<Vec<_>>();
        (duration, tracks)
    } else if mp4::looks_like_mp4(source.as_ref()) {
        let metadata = mp4::parse_basic_metadata(source.as_ref());
        let duration = metadata
            .duration_ms
            .context("Audio analysis requires source duration")?;
        let mut tracks = Vec::new();
        for (i, t) in metadata
            .tracks
            .iter()
            .filter(|t| t.kind == mp4::Mp4TrackKind::Audio)
            .enumerate()
        {
            let id = format!("a{i}");
            let config = mp4::parse_codec_config(source.as_ref(), Some(&id))
                .and_then(|c| c.description_hex)
                .map(|hex| decode_hex(&hex))
                .transpose()?
                .unwrap_or_default();
            tracks.push((
                Spec {
                    id,
                    codec: t.codec.clone(),
                    language: language(t.language.as_deref()),
                    config,
                    sample_rate: t.sample_rate.unwrap_or(48_000),
                    duration_ms: duration,
                },
                t.default,
            ));
        }
        (duration, tracks)
    } else {
        bail!("Audio repetition analysis requires MP4 or Matroska");
    };
    ensure!(
        (120_000..=12 * 3600 * 1000).contains(&duration),
        "Audio repetition source duration exceeds bounds"
    );
    tracks.retain(|(t, _)| supported(&t.codec));
    // Match the same authored language across peers. Prefer an original Japanese
    // program when available, avoiding mixing dubbed dialogue with another edit.
    tracks.sort_by_key(|(t, default)| {
        (
            wanted.is_some_and(|w| t.language.as_deref() != Some(w)),
            wanted.is_none() && t.language.as_deref() != Some("ja"),
            !*default,
        )
    });
    let (spec, _) = tracks
        .into_iter()
        .find(|(t, _)| wanted.is_none_or(|w| t.language.as_deref() == Some(w)))
        .context("No matching supported audio program")?;
    ensure!(
        spec.config.len() <= 64 * 1024,
        "Audio configuration exceeds bounds"
    );
    source.validate_current()?;
    Ok((source, spec))
}
fn decode_hex(text: &str) -> Result<Vec<u8>> {
    ensure!(
        text.len() <= 128 * 1024
            && text.len().is_multiple_of(2)
            && text.bytes().all(|b| b.is_ascii_hexdigit()),
        "Invalid audio decoder configuration"
    );
    (0..text.len())
        .step_by(2)
        .map(|i| u8::from_str_radix(&text[i..i + 2], 16).map_err(Into::into))
        .collect()
}

fn packets(source: &MediaSource, id: &str, start: u64, end: u64) -> Result<Vec<PacketRef>> {
    source.validate_current()?;
    let packets = if matroska::looks_like_ebml(source.as_ref()) {
        source
            .matroska_index()?
            .packets(source, &[id], start, end)?
            .into_iter()
            .next()
            .context("Audio packet window unavailable")?
            .packets
    } else {
        let metadata = source.mp4_metadata_for_tracks(&[id])?;
        mp4::parse_packet_track(&metadata, Some(id))
            .context("Audio packet index unavailable")?
            .packets
            .into_iter()
            .filter(|p| {
                p.pts.as_millis() < end
                    && p.pts.as_millis().saturating_add(p.duration.as_millis()) > start
            })
            .collect()
    };
    ensure!(
        packets.len() <= 40_000,
        "Audio window packet count exceeds bounds"
    );
    ensure!(
        packets.iter().all(|p| p.size <= 256 * 1024),
        "Audio access unit exceeds bounds"
    );
    Ok(packets)
}

enum Decoder {
    Aac(Box<rusty_aac::AacDecoder>),
    Eac3(Box<oxideav_ac3::eac3::decoder::Eac3DecoderState>),
    Ac3(Box<dyn oxideav_core::Decoder>),
    Opus(Box<crate::transcode::OpusAudioDecoderSession>),
    Dts(Box<crate::transcode::DtsAudioDecoderSession>),
    TrueHd(Box<crate::transcode::TrueHdAudioDecoderSession>),
}
impl Decoder {
    fn new(spec: &Spec) -> Result<Self> {
        match spec.codec.as_str() {
            "aac" => Ok(Self::Aac(Box::new(
                rusty_aac::AacDecoder::with_config_bytes(&spec.config)
                    .map_err(|e| anyhow::anyhow!("AAC decoder initialization failed: {e}"))?,
            ))),
            "eac3" => Ok(Self::Eac3(Box::default())),
            "ac3" => {
                let mut p = oxideav_core::CodecParameters::audio(oxideav_core::CodecId::new("ac3"));
                p.channels = Some(2);
                p.sample_rate = Some(spec.sample_rate);
                p.sample_format = Some(oxideav_core::SampleFormat::S16);
                Ok(Self::Ac3(oxideav_ac3::decoder::make_decoder(&p)?))
            }
            "opus" => Ok(Self::Opus(Box::new(
                crate::transcode::OpusAudioDecoderSession::new(&spec.config)?,
            ))),
            "dts" => Ok(Self::Dts(Box::default())),
            "truehd" => Ok(Self::TrueHd(Box::default())),
            _ => bail!("Unsupported acoustic analysis codec"),
        }
    }
    fn push(&mut self, bytes: &[u8], pts: u64, rate: u32, builder: &mut Builder) -> Result<()> {
        use crate::{
            packet::{TimeDelta, TimePoint, TimeScale},
            transcode::{AudioDecodeCodec, AudioDecodeInput, CompressedAudioPacket},
        };
        let native_codec = match self {
            Self::Opus(_) => Some(AudioDecodeCodec::Opus),
            Self::Dts(_) => Some(AudioDecodeCodec::Dts),
            Self::TrueHd(_) => Some(AudioDecodeCodec::TrueHd),
            _ => None,
        };
        if let Some(codec) = native_codec {
            let input = AudioDecodeInput {
                codec,
                time_scale: TimeScale::MILLIS,
                packets: vec![CompressedAudioPacket {
                    index: 0,
                    pts: TimePoint::millis(pts),
                    dts: TimePoint::millis(pts),
                    duration: TimeDelta::millis(0),
                    bytes,
                }],
                end_of_stream: false,
            };
            let decoded = match self {
                Self::Opus(d) => d.decode(&input)?,
                Self::Dts(d) => d.decode(&input)?,
                Self::TrueHd(d) => d.decode(&input)?,
                _ => unreachable!(),
            };
            for frame in decoded.frames {
                let samples = frame
                    .samples
                    .iter()
                    .map(|v| f32::from(*v) / 32768.0)
                    .collect::<Vec<_>>();
                builder.push(
                    frame.timing.pts.as_millis(),
                    &samples,
                    decoded.format.sample_rate,
                    decoded.format.channels as usize,
                )?;
            }
            return Ok(());
        }
        match self {
            Self::Opus(_) | Self::Dts(_) | Self::TrueHd(_) => unreachable!(),
            Self::Aac(decoder) => {
                let decoded = decoder
                    .decode(bytes, None)
                    .map_err(|e| anyhow::anyhow!("AAC packet decode failed: {e}"))?;
                builder.push(
                    pts,
                    &decoded.samples,
                    decoded.sample_rate,
                    usize::from(decoded.channels),
                )?;
            }
            Self::Eac3(state) => {
                let before = state.frames_zero_filled;
                let decoded = oxideav_ac3::eac3::decoder::decode_eac3_packet(state, bytes)?;
                ensure!(
                    state.frames_zero_filled == before && state.last_error.is_none(),
                    "E-AC-3 feature is not supported by the native decoder"
                );
                let samples = decoded
                    .pcm_s16le
                    .as_chunks::<2>()
                    .0
                    .iter()
                    .map(|b| f32::from(i16::from_le_bytes([b[0], b[1]])) / 32768.0)
                    .collect::<Vec<_>>();
                builder.push(
                    pts,
                    &samples,
                    decoded.sample_rate,
                    usize::from(decoded.channels),
                )?;
            }
            Self::Ac3(decoder) => {
                let packet = oxideav_core::Packet::new(
                    0,
                    oxideav_core::TimeBase::new(1, 1000),
                    bytes.to_vec(),
                );
                decoder.send_packet(&packet)?;
                let frame = decoder.receive_frame()?;
                let oxideav_core::Frame::Audio(frame) = frame else {
                    bail!("Native decoder did not return audio");
                };
                ensure!(
                    frame.data.len() == 1 && frame.samples > 0 && frame.data[0].len() <= 64 * 1024,
                    "Invalid native audio output"
                );
                let samples = frame.data[0]
                    .as_chunks::<2>()
                    .0
                    .iter()
                    .map(|b| f32::from(i16::from_le_bytes([b[0], b[1]])) / 32768.0)
                    .collect::<Vec<_>>();
                let channels = samples.len() / frame.samples as usize;
                builder.push(pts, &samples, rate, channels)?;
            }
        }
        Ok(())
    }
}

pub(super) fn capture(source: &MediaSource, spec: &Spec) -> Result<Vec<Window>> {
    let duration = spec.duration_ms;
    let early_end = 600_000.min(duration / 4);
    let late_start = duration.saturating_sub(300_000).max(early_end);
    let mut output = Vec::new();
    let mut total_bytes = 0usize;
    for (start, end) in [(0, early_end), (late_start, duration)] {
        source.check_work()?;
        let window_started = Instant::now();
        let index_started = Instant::now();
        let packets = packets(source, &spec.id, start.saturating_sub(1000), end)?;
        let index_seconds = index_started.elapsed().as_secs_f64();
        let mut read_time = Duration::ZERO;
        let mut decode_time = Duration::ZERO;
        let mut progress = Instant::now();
        let mut processed_packets = 0usize;
        let mut builder = Builder::new(start, end);
        let mut decoder = Decoder::new(spec)?;
        // Bound sequential I/O and validate identity around each batch. Only
        // selected audio bytes reach the decoder, even across interleaved video.
        for batch in packets.chunks(256) {
            source.check_work()?;
            let read_started = Instant::now();
            let bytes = source.read_analysis_packets(batch)?;
            read_time += read_started.elapsed();
            total_bytes = total_bytes
                .checked_add(bytes.len())
                .context("Audio compressed byte count overflow")?;
            ensure!(
                total_bytes <= 128 * 1024 * 1024,
                "Audio analysis compressed payload budget exceeded"
            );
            let mut offset = 0;
            for packet in batch {
                let end = offset + packet.size as usize;
                let before_features = builder.processing_time();
                let decode_started = Instant::now();
                decoder.push(
                    &bytes[offset..end],
                    packet.pts.as_millis(),
                    spec.sample_rate,
                    &mut builder,
                )?;
                decode_time += decode_started
                    .elapsed()
                    .saturating_sub(builder.processing_time() - before_features);
                processed_packets += 1;
                offset = end;
            }
            source.check_work()?;
            if progress.elapsed() >= Duration::from_secs(30) {
                eprintln!(
                    "Acoustic window {start}..{end} ms: {processed_packets}/{} packets; index={index_seconds:.3}s read={:.3}s decode={:.3}s features={:.3}s",
                    packets.len(),
                    read_time.as_secs_f64(),
                    decode_time.as_secs_f64(),
                    builder.processing_time().as_secs_f64()
                );
                progress = Instant::now();
            }
        }
        eprintln!(
            "Acoustic window {start}..{end} ms complete: {} packets; index={index_seconds:.3}s read={:.3}s decode={:.3}s features={:.3}s total={:.3}s",
            packets.len(),
            read_time.as_secs_f64(),
            decode_time.as_secs_f64(),
            builder.processing_time().as_secs_f64(),
            window_started.elapsed().as_secs_f64()
        );
        output.push(builder.finish());
    }
    source.validate_current()?;
    Ok(output)
}
