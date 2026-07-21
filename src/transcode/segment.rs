use std::path::Path;

use anyhow::{Result, bail};
use serde::Serialize;

use crate::{
    container::matroska::{MatroskaTrack, MatroskaTrackKind, looks_like_ebml},
    fmp4::{
        Fmp4SampleEntry, Fmp4Track, Fmp4TrackKind, fragment_track_from_encoded_audio_frames,
        fragment_track_from_encoded_video_frames, init_segment, media_fragment,
    },
    packet::{ExtractedChunk, TimePoint, TimeScale},
    source::MappedMediaFile,
    transcode::{
        AudioDecodeCodec, AudioFrameTiming, EncodedAudioFrame, RawVideoFormat, RawVideoFrameRef,
        RawVideoPixelFormat, VideoCodec, build_audio_decode_input, build_video_decode_input,
        decode_dts_core_to_interleaved_i16, decode_videotoolbox_bgra_frames,
        eac3_bridge_channel_count, encode_eac3_from_interleaved_i16,
        encode_h264_videotoolbox_bgra_frames, normalize_interleaved_channels,
    },
};

const VIDEO_TRACK_ID: u32 = 1;
const AUDIO_TRACK_ID: u32 = 2;
const DEFAULT_VIDEO_BITRATE: u32 = 16_000_000;
const DEFAULT_AUDIO_BITRATE: u32 = 640_000;
const DEFAULT_SEGMENT_MS: u64 = 4_000;
const EAC3_FRAMES_PER_PACKET: u32 = 1_536;

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
    /// Encoded output video codec.
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
    /// Encoded video frame count.
    pub encoded_video_frames: usize,
    /// Encoded audio frame count.
    pub encoded_audio_frames: usize,
    /// First video timestamp in milliseconds.
    pub first_video_pts_ms: Option<u64>,
    /// First audio timestamp in milliseconds.
    pub first_audio_pts_ms: Option<u64>,
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
        video_codec: "h264".to_string(),
        audio_codec: "eac3".to_string(),
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
        encoded_video_frames: segment.encoded_video_frames,
        encoded_audio_frames: segment.encoded_audio_frames,
        first_video_pts_ms: segment.first_video_pts.map(TimePoint::as_millis),
        first_audio_pts_ms: segment.first_audio_pts.map(TimePoint::as_millis),
    })
}

struct TranscodedSegment {
    video_track_id: String,
    audio_track_id: String,
    init_segment: Vec<u8>,
    media_segment: Vec<u8>,
    decoded_video_frames: usize,
    encoded_video_frames: usize,
    encoded_audio_frames: usize,
    first_video_pts: Option<TimePoint>,
    first_audio_pts: Option<TimePoint>,
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
    if audio_track.codec != "dts" {
        bail!(
            "selected audio track {} uses {}, not DTS",
            audio_track_id,
            audio_track.codec
        );
    }
    let video_codec = video_codec_from_label(&video_track.codec)?;
    let decoder_config = video_track
        .codec_private
        .clone()
        .ok_or_else(|| anyhow::anyhow!("missing Matroska video decoder config"))?;
    let segment_ms = options.segment_ms.max(1);
    let (video_manifest, video_payload) =
        crate::container::matroska::extract_chunk(bytes, Some(&video_track_id), segment_ms, index)?;
    let (audio_manifest, audio_payload) =
        crate::container::matroska::extract_chunk(bytes, Some(&audio_track_id), segment_ms, index)?;

    let video_format = matroska_video_format(video_track);
    let video_decode_input = build_video_decode_input(
        video_codec,
        chunk_time_scale(&video_manifest),
        Some(&decoder_config),
        &video_manifest.samples,
        &video_payload,
        true,
    )?;
    let decoded_video = decode_videotoolbox_bgra_frames(&video_decode_input, video_format)?;
    let raw_frames = decoded_video
        .frames
        .iter()
        .map(|frame| RawVideoFrameRef {
            pts: frame.pts,
            dts: frame.dts,
            duration: frame.duration,
            bytes: &frame.pixels,
            keyframe: frame.keyframe,
        })
        .collect::<Vec<_>>();
    let encoded_video =
        encode_h264_videotoolbox_bgra_frames(video_format, &raw_frames, options.video_bitrate)?;
    let video_config = encoded_video
        .stream
        .decoder_config
        .clone()
        .ok_or_else(|| anyhow::anyhow!("VideoToolbox did not emit H.264 decoder config"))?;
    let video_fragment =
        fragment_track_from_encoded_video_frames(VIDEO_TRACK_ID, &encoded_video.frames)?;

    let audio_decode_input = build_audio_decode_input(
        AudioDecodeCodec::Dts,
        chunk_time_scale(&audio_manifest),
        &audio_manifest.samples,
        &audio_payload,
        true,
    )?;
    let decoded_audio = decode_dts_core_to_interleaved_i16(&audio_decode_input)?;
    let decoded_pcm = decoded_audio
        .frames
        .iter()
        .flat_map(|frame| frame.pcm.iter().copied())
        .collect::<Vec<_>>();
    let bridge_channels = eac3_bridge_channel_count(decoded_audio.stream.format.channels);
    if bridge_channels > 6 {
        bail!("native E-AC-3 bridge supports at most 6 output channels");
    }
    let bridge_pcm = normalize_interleaved_channels(
        &decoded_pcm,
        decoded_audio.stream.format.channels,
        bridge_channels,
    )?;
    let bridge_format = crate::transcode::PcmAudioFormat {
        sample_rate: decoded_audio.stream.format.sample_rate,
        channels: bridge_channels,
    };
    let mut encoded_audio =
        encode_eac3_from_interleaved_i16(bridge_format, &bridge_pcm, options.audio_bitrate)?;
    rebase_audio_frames(
        &mut encoded_audio.frames,
        first_sample_time(&audio_manifest, bridge_format.sample_rate),
    );
    let audio_fragment =
        fragment_track_from_encoded_audio_frames(AUDIO_TRACK_ID, &encoded_audio.frames)?;

    let tracks = vec![
        Fmp4Track {
            id: VIDEO_TRACK_ID,
            kind: Fmp4TrackKind::Video,
            timescale: encoded_video.stream.time_scale.units_per_second,
            default_sample_duration: encoded_video
                .frames
                .first()
                .map(|frame| frame.duration.units.min(u64::from(u32::MAX)) as u32)
                .unwrap_or(1),
            default_sample_size: 0,
            default_sample_flags: 0x0101_0000,
            sample_entry: Fmp4SampleEntry::Avc {
                codec_config: video_config,
                width: encoded_video.stream.width.min(u32::from(u16::MAX)) as u16,
                height: encoded_video.stream.height.min(u32::from(u16::MAX)) as u16,
            },
        },
        Fmp4Track {
            id: AUDIO_TRACK_ID,
            kind: Fmp4TrackKind::Audio,
            timescale: bridge_format.sample_rate,
            default_sample_duration: EAC3_FRAMES_PER_PACKET,
            default_sample_size: 0,
            default_sample_flags: 0x0200_0000,
            sample_entry: Fmp4SampleEntry::Eac3 {
                dec3: eac3_dec3_config(bridge_format.sample_rate, bridge_format.channels),
                channel_count: bridge_format.channels.min(u32::from(u16::MAX)) as u16,
                sample_rate: bridge_format.sample_rate,
            },
        },
    ];
    let init = init_segment(&tracks)?;
    let media = media_fragment(index.saturating_add(1), &[video_fragment, audio_fragment])?;

    Ok(TranscodedSegment {
        video_track_id,
        audio_track_id,
        init_segment: init,
        media_segment: media,
        decoded_video_frames: decoded_video.frames.len(),
        encoded_video_frames: encoded_video.frames.len(),
        encoded_audio_frames: encoded_audio.frames.len(),
        first_video_pts: encoded_video.frames.first().map(|frame| frame.pts),
        first_audio_pts: encoded_audio.frames.first().map(|frame| frame.timing.pts),
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

fn matroska_video_format(track: &MatroskaTrack) -> RawVideoFormat {
    let (frame_rate_num, frame_rate_den) = frame_rate_from_default_duration(track);
    RawVideoFormat {
        width: track.width.unwrap_or(1_920),
        height: track.height.unwrap_or(1_080),
        frame_rate_num,
        frame_rate_den,
        pixel_format: RawVideoPixelFormat::Bgra,
    }
}

fn frame_rate_from_default_duration(track: &MatroskaTrack) -> (u32, u32) {
    let Some(ns) = track.default_duration_ns else {
        return (24_000, 1_000);
    };
    if ns == 0 {
        return (24_000, 1_000);
    }
    let rate_x1000 = (1_000_000_000_000_u64 + (ns / 2)) / ns;
    (u32::try_from(rate_x1000).unwrap_or(24_000).max(1), 1_000)
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

fn eac3_dec3_config(_sample_rate: u32, _channels: u32) -> Vec<u8> {
    vec![0x00, 0x10, 0x20, 0x0f, 0x00]
}
