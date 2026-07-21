use std::path::Path;

use anyhow::{Result, bail};
use serde::Serialize;

use crate::{
    codec::ac3::{parse_ac3_specific_box, parse_eac3_specific_box},
    container::matroska::{MatroskaTrack, MatroskaTrackKind, looks_like_ebml},
    fmp4::{
        Fmp4SampleEntry, Fmp4Track, Fmp4TrackKind, fragment_track_from_chunk_samples,
        fragment_track_from_encoded_audio_frames, init_segment, media_fragment,
    },
    packet::{ChunkSample, ExtractedChunk, TimePoint, TimeScale},
    source::MappedMediaFile,
    transcode::{
        AudioDecodeCodec, AudioFrameTiming, EncodedAudioFrame, VideoCodec,
        build_audio_decode_input, decode_dts_core_to_interleaved_i16,
        encode_aac_from_interleaved_i16,
    },
};

const VIDEO_TRACK_ID: u32 = 1;
const AUDIO_TRACK_ID: u32 = 2;
const DEFAULT_VIDEO_BITRATE: u32 = 16_000_000;
const DEFAULT_AUDIO_BITRATE: u32 = 384_000;
const DEFAULT_SEGMENT_MS: u64 = 4_000;
const AAC_FRAMES_PER_PACKET: u32 = 1_024;

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
/// Options for native Matroska-to-fMP4 HLS transcoding.
pub struct NativeFmp4TranscodeOptions {
    /// Source video track id, such as `v0`.
    pub video_track_id: Option<String>,
    /// Source audio track id, such as `a0`.
    pub audio_track_id: Option<String>,
    /// Target chunk duration used by packet extraction.
    pub segment_ms: u64,
    /// Target H.264 bitrate.
    pub video_bitrate: u32,
    /// Target E-AC-3 bitrate.
    pub audio_bitrate: u32,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
/// Summary emitted after writing a native transcode init segment.
pub struct NativeFmp4TranscodeInitOutput {
    /// Output path written by the engine.
    pub output: String,
    /// Selected source video track id.
    pub video_track_id: String,
    /// Selected source audio track id.
    pub audio_track_id: String,
    /// Init segment byte count.
    pub byte_count: u64,
    /// Encoded or copied output video codec.
    pub video_codec: String,
    /// Encoded output audio codec.
    pub audio_codec: String,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
/// Summary emitted after writing a native transcode media segment.
pub struct NativeFmp4TranscodeSegmentOutput {
    /// Output path written by the engine.
    pub output: String,
    /// Segment index.
    pub index: u32,
    /// Selected source video track id.
    pub video_track_id: String,
    /// Selected source audio track id.
    pub audio_track_id: String,
    /// Media segment byte count.
    pub byte_count: u64,
    /// Decoded video frame count.
    pub decoded_video_frames: usize,
    /// Output video sample count.
    pub video_sample_count: usize,
    /// Encoded video frame count. Deprecated for packet-copy output; use `video_sample_count`.
    pub encoded_video_frames: usize,
    /// Encoded audio frame count.
    pub encoded_audio_frames: usize,
    /// Encoded or copied output audio codec.
    pub audio_codec: String,
    /// First video timestamp in milliseconds.
    pub first_video_pts_ms: Option<u64>,
    /// First audio timestamp in milliseconds.
    pub first_audio_pts_ms: Option<u64>,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
/// Summary emitted after writing native transcode startup assets.
pub struct NativeFmp4TranscodeStartOutput {
    /// Init segment path written by the engine.
    pub init_output: String,
    /// Media segment path written by the engine.
    pub segment_output: String,
    /// Segment index.
    pub index: u32,
    /// Selected source video track id.
    pub video_track_id: String,
    /// Selected source audio track id.
    pub audio_track_id: String,
    /// Init segment byte count.
    pub init_byte_count: u64,
    /// Media segment byte count.
    pub segment_byte_count: u64,
    /// Decoded video frame count.
    pub decoded_video_frames: usize,
    /// Output video sample count.
    pub video_sample_count: usize,
    /// Encoded video frame count. Deprecated for packet-copy output; use `video_sample_count`.
    pub encoded_video_frames: usize,
    /// Encoded audio frame count.
    pub encoded_audio_frames: usize,
    /// Encoded or copied output audio codec.
    pub audio_codec: String,
}

impl Default for NativeFmp4TranscodeOptions {
    fn default() -> Self {
        Self {
            video_track_id: None,
            audio_track_id: None,
            segment_ms: DEFAULT_SEGMENT_MS,
            video_bitrate: DEFAULT_VIDEO_BITRATE,
            audio_bitrate: DEFAULT_AUDIO_BITRATE,
        }
    }
}

/// Writes a native fMP4 init segment for the Matroska transcode path.
pub fn write_native_fmp4_transcode_init(
    input: &Path,
    output: &Path,
    options: NativeFmp4TranscodeOptions,
) -> Result<NativeFmp4TranscodeInitOutput> {
    let source = MappedMediaFile::open(input)?;
    let segment = transcode_matroska_segment(source.as_ref(), 0, &options)?;
    std::fs::write(output, &segment.init_segment)?;
    Ok(NativeFmp4TranscodeInitOutput {
        output: output.display().to_string(),
        video_track_id: segment.video_track_id,
        audio_track_id: segment.audio_track_id,
        byte_count: segment.init_segment.len() as u64,
        video_codec: segment.video_codec,
        audio_codec: segment.audio_codec,
    })
}

/// Writes one native fMP4 media segment for the Matroska transcode path.
pub fn write_native_fmp4_transcode_segment(
    input: &Path,
    output: &Path,
    index: u32,
    options: NativeFmp4TranscodeOptions,
) -> Result<NativeFmp4TranscodeSegmentOutput> {
    let source = MappedMediaFile::open(input)?;
    let segment = transcode_matroska_segment(source.as_ref(), index, &options)?;
    std::fs::write(output, &segment.media_segment)?;
    Ok(NativeFmp4TranscodeSegmentOutput {
        output: output.display().to_string(),
        index,
        video_track_id: segment.video_track_id,
        audio_track_id: segment.audio_track_id,
        byte_count: segment.media_segment.len() as u64,
        decoded_video_frames: segment.decoded_video_frames,
        video_sample_count: segment.video_sample_count,
        encoded_video_frames: segment.encoded_video_frames,
        encoded_audio_frames: segment.encoded_audio_frames,
        audio_codec: segment.audio_codec,
        first_video_pts_ms: segment.first_video_pts.map(TimePoint::as_millis),
        first_audio_pts_ms: segment.first_audio_pts.map(TimePoint::as_millis),
    })
}

/// Writes a native fMP4 init segment and one media segment from a single transcode pass.
pub fn write_native_fmp4_transcode_start(
    input: &Path,
    init_output: &Path,
    segment_output: &Path,
    index: u32,
    options: NativeFmp4TranscodeOptions,
) -> Result<NativeFmp4TranscodeStartOutput> {
    let source = MappedMediaFile::open(input)?;
    let segment = transcode_matroska_segment(source.as_ref(), index, &options)?;
    std::fs::write(init_output, &segment.init_segment)?;
    std::fs::write(segment_output, &segment.media_segment)?;
    Ok(NativeFmp4TranscodeStartOutput {
        init_output: init_output.display().to_string(),
        segment_output: segment_output.display().to_string(),
        index,
        video_track_id: segment.video_track_id,
        audio_track_id: segment.audio_track_id,
        init_byte_count: segment.init_segment.len() as u64,
        segment_byte_count: segment.media_segment.len() as u64,
        decoded_video_frames: segment.decoded_video_frames,
        video_sample_count: segment.video_sample_count,
        encoded_video_frames: segment.encoded_video_frames,
        encoded_audio_frames: segment.encoded_audio_frames,
        audio_codec: segment.audio_codec,
    })
}

struct TranscodedSegment {
    video_track_id: String,
    audio_track_id: String,
    video_codec: String,
    audio_codec: String,
    init_segment: Vec<u8>,
    media_segment: Vec<u8>,
    decoded_video_frames: usize,
    video_sample_count: usize,
    encoded_video_frames: usize,
    encoded_audio_frames: usize,
    first_video_pts: Option<TimePoint>,
    first_audio_pts: Option<TimePoint>,
}

struct AudioSegment {
    fragment: crate::fmp4::Fmp4FragmentTrack,
    sample_entry: Fmp4SampleEntry,
    codec: String,
    timescale: u32,
    default_sample_duration: u32,
    encoded_frame_count: usize,
    first_pts: Option<TimePoint>,
}

fn transcode_matroska_segment(
    bytes: &[u8],
    index: u32,
    options: &NativeFmp4TranscodeOptions,
) -> Result<TranscodedSegment> {
    if !looks_like_ebml(bytes) {
        bail!("native fMP4 transcode currently supports Matroska/WebM sources");
    }

    let meta = crate::container::matroska::parse_basic_metadata(bytes);
    let (video_track_id, video_track) = select_matroska_track(
        &meta.tracks,
        MatroskaTrackKind::Video,
        options.video_track_id.as_deref(),
    )
    .ok_or_else(|| anyhow::anyhow!("no matching Matroska video track found"))?;
    let (audio_track_id, audio_track) = select_matroska_track(
        &meta.tracks,
        MatroskaTrackKind::Audio,
        options.audio_track_id.as_deref(),
    )
    .ok_or_else(|| anyhow::anyhow!("no matching Matroska audio track found"))?;
    let _video_codec = video_codec_from_label(&video_track.codec)?;
    let decoder_config = video_track
        .codec_private
        .clone()
        .ok_or_else(|| anyhow::anyhow!("missing Matroska video decoder config"))?;
    let segment_ms = options.segment_ms.max(1);
    let (video_manifest, video_payload) =
        crate::container::matroska::extract_chunk(bytes, Some(&video_track_id), segment_ms, index)?;
    let (audio_manifest, audio_payload) =
        crate::container::matroska::extract_chunk(bytes, Some(&audio_track_id), segment_ms, index)?;

    let video_timescale = chunk_time_scale(&video_manifest).units_per_second;
    let video_fragment = fragment_track_from_chunk_samples(
        VIDEO_TRACK_ID,
        &video_manifest.samples,
        video_payload,
        video_timescale,
    )?;

    let audio_segment = matroska_audio_segment(
        audio_track,
        audio_manifest,
        audio_payload,
        options.audio_bitrate,
    )?;

    let tracks = vec![
        Fmp4Track {
            id: VIDEO_TRACK_ID,
            kind: Fmp4TrackKind::Video,
            timescale: video_timescale,
            default_sample_duration: video_manifest
                .samples
                .first()
                .map(|sample| {
                    rescale_units(
                        sample.duration.units,
                        sample.duration.scale,
                        video_timescale,
                    )
                    .max(1)
                    .min(u64::from(u32::MAX)) as u32
                })
                .unwrap_or(1),
            default_sample_size: 0,
            default_sample_flags: 0x0101_0000,
            sample_entry: matroska_video_sample_entry(video_track, decoder_config)?,
        },
        Fmp4Track {
            id: AUDIO_TRACK_ID,
            kind: Fmp4TrackKind::Audio,
            timescale: audio_segment.timescale,
            default_sample_duration: audio_segment.default_sample_duration,
            default_sample_size: 0,
            default_sample_flags: 0x0200_0000,
            sample_entry: audio_segment.sample_entry,
        },
    ];
    let init = init_segment(&tracks)?;
    let media = media_fragment(
        index.saturating_add(1),
        &[video_fragment, audio_segment.fragment],
    )?;

    Ok(TranscodedSegment {
        video_track_id,
        audio_track_id,
        video_codec: video_codec_string(video_track),
        audio_codec: audio_segment.codec,
        init_segment: init,
        media_segment: media,
        decoded_video_frames: 0,
        video_sample_count: video_manifest.samples.len(),
        encoded_video_frames: 0,
        encoded_audio_frames: audio_segment.encoded_frame_count,
        first_video_pts: video_manifest.samples.first().map(|sample| sample.pts),
        first_audio_pts: audio_segment.first_pts,
    })
}

fn select_matroska_track<'a>(
    tracks: &'a [MatroskaTrack],
    kind: MatroskaTrackKind,
    requested_track_id: Option<&str>,
) -> Option<(String, &'a MatroskaTrack)> {
    let mut fallback = None;
    let mut video_index = 0_u32;
    let mut audio_index = 0_u32;
    for track in tracks {
        let track_id = match track.kind {
            MatroskaTrackKind::Video => {
                let id = format!("v{video_index}");
                video_index += 1;
                id
            }
            MatroskaTrackKind::Audio => {
                let id = format!("a{audio_index}");
                audio_index += 1;
                id
            }
            MatroskaTrackKind::Subtitle | MatroskaTrackKind::Unknown => continue,
        };
        if track.kind != kind {
            continue;
        }
        if fallback.is_none() {
            fallback = Some((track_id.clone(), track));
        }
        if requested_track_id
            .map(|requested| requested == track_id)
            .unwrap_or(track.default)
        {
            return Some((track_id, track));
        }
    }
    if requested_track_id.is_some() {
        None
    } else {
        fallback
    }
}

fn video_codec_from_label(codec: &str) -> Result<VideoCodec> {
    match codec {
        "h264" => Ok(VideoCodec::H264),
        "hevc" => Ok(VideoCodec::Hevc),
        _ => bail!("selected video track codec {codec} is not supported by native decode"),
    }
}

fn matroska_video_sample_entry(
    track: &MatroskaTrack,
    codec_config: Vec<u8>,
) -> Result<Fmp4SampleEntry> {
    match track.codec.as_str() {
        "h264" => Ok(Fmp4SampleEntry::Avc {
            codec_config,
            width: track.width.unwrap_or(0).min(u32::from(u16::MAX)) as u16,
            height: track.height.unwrap_or(0).min(u32::from(u16::MAX)) as u16,
        }),
        "hevc" => Ok(Fmp4SampleEntry::Hevc {
            codec_config,
            width: track.width.unwrap_or(0).min(u32::from(u16::MAX)) as u16,
            height: track.height.unwrap_or(0).min(u32::from(u16::MAX)) as u16,
        }),
        other => bail!("selected video track codec {other} is not supported by fMP4 packet-copy"),
    }
}

fn video_codec_string(track: &MatroskaTrack) -> String {
    match track.codec.as_str() {
        "h264" => "h264".to_string(),
        "hevc" => "hevc".to_string(),
        other => other.to_string(),
    }
}

fn matroska_audio_segment(
    track: &MatroskaTrack,
    manifest: ExtractedChunk,
    payload: Vec<u8>,
    bitrate: u32,
) -> Result<AudioSegment> {
    match track.codec.as_str() {
        "dts" => transcode_dts_audio_segment(&manifest, &payload, bitrate),
        "aac" | "ac3" | "eac3" => copy_matroska_audio_segment(track, manifest, payload),
        other => bail!("selected audio track codec {other} is not supported by native fMP4 output"),
    }
}

fn transcode_dts_audio_segment(
    manifest: &ExtractedChunk,
    payload: &[u8],
    bitrate: u32,
) -> Result<AudioSegment> {
    let audio_decode_input = build_audio_decode_input(
        AudioDecodeCodec::Dts,
        chunk_time_scale(manifest),
        &manifest.samples,
        payload,
        true,
    )?;
    let decoded_audio = decode_dts_core_to_interleaved_i16(&audio_decode_input)?;
    let decoded_pcm = decoded_audio
        .frames
        .iter()
        .flat_map(|frame| frame.pcm.iter().copied())
        .collect::<Vec<_>>();
    let bridge_format = crate::transcode::PcmAudioFormat {
        sample_rate: decoded_audio.stream.format.sample_rate,
        channels: decoded_audio.stream.format.channels.min(2),
    };
    let bridge_pcm = stereo_pcm_from_interleaved(
        &decoded_pcm,
        decoded_audio.stream.format.channels,
        bridge_format.channels,
    )?;
    let mut encoded_audio = encode_aac_from_interleaved_i16(bridge_format, &bridge_pcm, bitrate)?;
    rebase_audio_frames(
        &mut encoded_audio.frames,
        first_sample_time(manifest, bridge_format.sample_rate),
    );
    let fragment = fragment_track_from_encoded_audio_frames(AUDIO_TRACK_ID, &encoded_audio.frames)?;
    let decoder_config = encoded_audio.stream.decoder_config.clone().ok_or_else(|| {
        anyhow::anyhow!("AAC encoder did not return decoder config for DTS bridge")
    })?;
    Ok(AudioSegment {
        fragment,
        sample_entry: Fmp4SampleEntry::Aac {
            decoder_config,
            channel_count: bridge_format.channels.min(u32::from(u16::MAX)) as u16,
            sample_rate: bridge_format.sample_rate,
        },
        codec: "aac".to_string(),
        timescale: bridge_format.sample_rate,
        default_sample_duration: AAC_FRAMES_PER_PACKET,
        encoded_frame_count: encoded_audio.frames.len(),
        first_pts: encoded_audio.frames.first().map(|frame| frame.timing.pts),
    })
}

fn copy_matroska_audio_segment(
    track: &MatroskaTrack,
    manifest: ExtractedChunk,
    payload: Vec<u8>,
) -> Result<AudioSegment> {
    let timescale = chunk_time_scale(&manifest).units_per_second;
    let sample_entry = matroska_audio_sample_entry(track, &manifest.samples, &payload)?;
    let default_sample_duration = default_sample_duration(&manifest, timescale);
    let first_pts = manifest.samples.first().map(|sample| sample.pts);
    let fragment =
        fragment_track_from_chunk_samples(AUDIO_TRACK_ID, &manifest.samples, payload, timescale)?;
    Ok(AudioSegment {
        fragment,
        sample_entry,
        codec: fmp4_audio_codec_string(track),
        timescale,
        default_sample_duration,
        encoded_frame_count: 0,
        first_pts,
    })
}

fn matroska_audio_sample_entry(
    track: &MatroskaTrack,
    samples: &[ChunkSample],
    payload: &[u8],
) -> Result<Fmp4SampleEntry> {
    match track.codec.as_str() {
        "aac" => Ok(Fmp4SampleEntry::Aac {
            decoder_config: track
                .codec_private
                .clone()
                .ok_or_else(|| anyhow::anyhow!("missing Matroska AAC private data"))?,
            channel_count: track.channels.unwrap_or(2).min(u32::from(u16::MAX)) as u16,
            sample_rate: track.sample_rate.unwrap_or(48_000),
        }),
        "ac3" => {
            let frame = first_sample_payload(samples, payload)?;
            Ok(Fmp4SampleEntry::Ac3 {
                dac3: parse_ac3_specific_box(frame)?.dac3_payload(),
                channel_count: track.channels.unwrap_or(2).min(u32::from(u16::MAX)) as u16,
                sample_rate: track.sample_rate.unwrap_or(48_000),
            })
        }
        "eac3" => {
            let access_unit = first_sample_payload(samples, payload)?;
            Ok(Fmp4SampleEntry::Eac3 {
                dec3: parse_eac3_specific_box(access_unit)?.dec3_payload(),
                channel_count: track.channels.unwrap_or(2).min(u32::from(u16::MAX)) as u16,
                sample_rate: track.sample_rate.unwrap_or(48_000),
            })
        }
        other => bail!("selected audio track codec {other} is not supported by fMP4 packet-copy"),
    }
}

fn fmp4_audio_codec_string(track: &MatroskaTrack) -> String {
    match track.codec.as_str() {
        "aac" => "aac".to_string(),
        "ac3" => "ac-3".to_string(),
        "eac3" => "ec-3".to_string(),
        other => other.to_string(),
    }
}

fn first_sample_payload<'a>(samples: &[ChunkSample], payload: &'a [u8]) -> Result<&'a [u8]> {
    let sample = samples
        .first()
        .ok_or_else(|| anyhow::anyhow!("audio chunk has no samples"))?;
    let start = usize::try_from(sample.payload_offset)?;
    let size = usize::try_from(sample.byte_count)?;
    let end = start
        .checked_add(size)
        .ok_or_else(|| anyhow::anyhow!("audio sample byte range overflowed"))?;
    payload
        .get(start..end)
        .ok_or_else(|| anyhow::anyhow!("audio sample byte range is outside chunk payload"))
}

fn default_sample_duration(chunk: &ExtractedChunk, timescale: u32) -> u32 {
    chunk
        .samples
        .first()
        .map(|sample| {
            rescale_units(sample.duration.units, sample.duration.scale, timescale)
                .max(1)
                .min(u64::from(u32::MAX)) as u32
        })
        .unwrap_or(1)
}

fn chunk_time_scale(chunk: &ExtractedChunk) -> TimeScale {
    chunk
        .samples
        .first()
        .map(|sample| sample.pts.scale)
        .unwrap_or(TimeScale::MILLIS)
}

fn first_sample_time(chunk: &ExtractedChunk, sample_rate: u32) -> u64 {
    chunk
        .samples
        .first()
        .map(|sample| rescale_units(sample.pts.units, sample.pts.scale, sample_rate))
        .unwrap_or(0)
}

fn rebase_audio_frames(frames: &mut [EncodedAudioFrame], start_sample: u64) {
    for frame in frames {
        let rebased_start = start_sample.saturating_add(frame.timing.start_sample);
        frame.timing = AudioFrameTiming {
            pts: TimePoint {
                units: rebased_start,
                scale: frame.timing.pts.scale,
            },
            duration: frame.timing.duration,
            start_sample: rebased_start,
            sample_count: frame.timing.sample_count,
            reanchored: frame.timing.reanchored,
        };
    }
}

fn rescale_units(units: u64, from: TimeScale, to_units_per_second: u32) -> u64 {
    if from.units_per_second == 0 || to_units_per_second == 0 {
        return 0;
    }
    let numerator = u128::from(units) * u128::from(to_units_per_second);
    let denominator = u128::from(from.units_per_second);
    ((numerator + (denominator / 2)) / denominator).min(u128::from(u64::MAX)) as u64
}

fn stereo_pcm_from_interleaved(
    pcm: &[i16],
    source_channels: u32,
    target_channels: u32,
) -> Result<Vec<i16>> {
    if source_channels == 0 || target_channels == 0 {
        bail!("audio bridge channel count must be greater than zero");
    }
    if !pcm.len().is_multiple_of(source_channels as usize) {
        bail!("decoded PCM sample count does not align to the source channel count");
    }
    if source_channels == target_channels {
        return Ok(pcm.to_vec());
    }
    let source_channels = source_channels as usize;
    let target_channels = target_channels.min(2) as usize;
    let mut out = Vec::with_capacity((pcm.len() / source_channels) * target_channels);
    for frame in pcm.chunks_exact(source_channels) {
        match target_channels {
            1 => out.push(frame[0]),
            2 => {
                out.push(frame[0]);
                out.push(*frame.get(1).unwrap_or(&frame[0]));
            }
            _ => unreachable!("target_channels is clamped to stereo"),
        }
    }
    Ok(out)
}
