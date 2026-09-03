use std::{
    path::Path,
    sync::atomic::{AtomicU64, Ordering},
};

use anyhow::{Result, bail};
use serde::Serialize;

use super::scaler::{constrained_h264_format, scale_bgra};
use crate::{
    codec::ac3::{parse_ac3_specific_box, parse_eac3_specific_box},
    container::matroska::{MatroskaTrack, MatroskaTrackKind, looks_like_ebml},
    fmp4::{
        Fmp4SampleEntry, Fmp4Track, Fmp4TrackKind, fragment_track_from_chunk_samples,
        fragment_track_from_encoded_video_frames, init_segment, media_fragment,
    },
    output::publish_bytes,
    packet::{
        ChunkPlan, ChunkSample, ExtractedChunk, NativeChunk, PacketRange, PacketRef, TimeDelta,
        TimePoint, TimeScale, extract_packet_payload, packet_samples_for_range, plan_track_chunks,
    },
    source::MappedMediaFile,
    transcode::{
        RawVideoFormat, RawVideoFrameRef, RawVideoPixelFormat, VideoCodec,
        build_video_decode_input, decode_videotoolbox_bgra_frames,
        encode_h264_videotoolbox_bgra_frames,
    },
};

const VIDEO_TRACK_ID: u32 = 1;
const AUDIO_TRACK_ID: u32 = 2;
const DEFAULT_VIDEO_BITRATE: u32 = 16_000_000;
const DEFAULT_AUDIO_BITRATE: u32 = 384_000;
const DEFAULT_SEGMENT_MS: u64 = 4_000;

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
    /// Output video path.
    pub video_mode: NativeFmp4VideoMode,
}

#[derive(Debug, Clone, Copy, Serialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
/// Native fMP4 video output mode.
pub enum NativeFmp4VideoMode {
    /// Packet-copy source video into fMP4.
    Copy,
    /// Decode source video and encode browser-oriented H.264.
    H264,
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

/// Lifecycle counters for a retained native fMP4 transcode session.
#[derive(Debug, Clone, Copy, Serialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct NativeFmp4TranscodeSessionStats {
    /// Source snapshots created by this session.
    pub source_opens: u64,
    /// Track-selection and packet-index passes completed at session open.
    pub index_parses: u64,
    /// Segment transcode requests served by this session.
    pub segment_requests: u64,
    /// Source identity validations performed before transcoding.
    pub source_validations: u64,
}

/// Stateful Matroska-to-fMP4 transcode session retaining one immutable source snapshot.
#[derive(Debug)]
pub struct NativeFmp4TranscodeSession {
    source: MappedMediaFile,
    options: NativeFmp4TranscodeOptions,
    prepared: PreparedTranscode,
    segment_requests: AtomicU64,
    source_validations: AtomicU64,
}

impl NativeFmp4TranscodeSession {
    /// Opens and validates a reusable transcode session.
    pub fn open(input: &Path, options: NativeFmp4TranscodeOptions) -> Result<Self> {
        let source = MappedMediaFile::open(input)?;
        if !looks_like_ebml(source.as_ref()) {
            bail!("native fMP4 transcode currently supports Matroska/WebM sources");
        }
        let prepared = prepare_transcode(source.as_ref(), &options)?;
        Ok(Self {
            source,
            options,
            prepared,
            segment_requests: AtomicU64::new(0),
            source_validations: AtomicU64::new(0),
        })
    }

    /// Returns measured session lifecycle counters.
    pub fn stats(&self) -> NativeFmp4TranscodeSessionStats {
        NativeFmp4TranscodeSessionStats {
            source_opens: 1,
            index_parses: 1,
            segment_requests: self.segment_requests.load(Ordering::Relaxed),
            source_validations: self.source_validations.load(Ordering::Relaxed),
        }
    }

    fn render(&self, index: u32) -> Result<TranscodedSegment> {
        self.segment_requests.fetch_add(1, Ordering::Relaxed);
        self.source.validate_current()?;
        self.source_validations.fetch_add(1, Ordering::Relaxed);
        transcode_prepared_segment(self.source.as_ref(), index, &self.options, &self.prepared)
    }

    /// Writes a native fMP4 init segment, deriving encoder configuration from segment zero.
    pub fn write_init(&self, output: &Path) -> Result<NativeFmp4TranscodeInitOutput> {
        let segment = self.render(0)?;
        publish_bytes(output, &segment.init_segment)?;
        Ok(NativeFmp4TranscodeInitOutput {
            output: output.display().to_string(),
            video_track_id: segment.video_track_id,
            audio_track_id: segment.audio_track_id,
            byte_count: segment.init_segment.len() as u64,
            video_codec: segment.video_codec,
            audio_codec: segment.audio_codec,
        })
    }

    /// Writes one media segment while retaining the source snapshot for later requests.
    pub fn write_segment(
        &self,
        output: &Path,
        index: u32,
    ) -> Result<NativeFmp4TranscodeSegmentOutput> {
        let segment = self.render(index)?;
        publish_bytes(output, &segment.media_segment)?;
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

    /// Writes init and media artifacts from one segment pass.
    pub fn write_start(
        &self,
        init_output: &Path,
        segment_output: &Path,
        index: u32,
    ) -> Result<NativeFmp4TranscodeStartOutput> {
        let segment = self.render(index)?;
        publish_bytes(init_output, &segment.init_segment)?;
        publish_bytes(segment_output, &segment.media_segment)?;
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
}

impl Default for NativeFmp4TranscodeOptions {
    fn default() -> Self {
        Self {
            video_track_id: None,
            audio_track_id: None,
            segment_ms: DEFAULT_SEGMENT_MS,
            video_bitrate: DEFAULT_VIDEO_BITRATE,
            audio_bitrate: DEFAULT_AUDIO_BITRATE,
            video_mode: NativeFmp4VideoMode::Copy,
        }
    }
}

/// Writes a native fMP4 init segment for the Matroska transcode path.
pub fn write_native_fmp4_transcode_init(
    input: &Path,
    output: &Path,
    options: NativeFmp4TranscodeOptions,
) -> Result<NativeFmp4TranscodeInitOutput> {
    NativeFmp4TranscodeSession::open(input, options)?.write_init(output)
}

/// Writes one native fMP4 media segment for the Matroska transcode path.
pub fn write_native_fmp4_transcode_segment(
    input: &Path,
    output: &Path,
    index: u32,
    options: NativeFmp4TranscodeOptions,
) -> Result<NativeFmp4TranscodeSegmentOutput> {
    NativeFmp4TranscodeSession::open(input, options)?.write_segment(output, index)
}

/// Writes a native fMP4 init segment and one media segment from a single transcode pass.
pub fn write_native_fmp4_transcode_start(
    input: &Path,
    init_output: &Path,
    segment_output: &Path,
    index: u32,
    options: NativeFmp4TranscodeOptions,
) -> Result<NativeFmp4TranscodeStartOutput> {
    NativeFmp4TranscodeSession::open(input, options)?.write_start(
        init_output,
        segment_output,
        index,
    )
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

struct VideoSegment {
    fragment: crate::fmp4::Fmp4FragmentTrack,
    sample_entry: Fmp4SampleEntry,
    timescale: u32,
    default_sample_duration: u32,
    codec: String,
    decoded_frame_count: usize,
    sample_count: usize,
    encoded_frame_count: usize,
    first_pts: Option<TimePoint>,
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

#[derive(Debug)]
struct PreparedTranscode {
    video_track_id: String,
    audio_track_id: String,
    video_track: MatroskaTrack,
    audio_track: MatroskaTrack,
    video_codec: VideoCodec,
    decoder_config: Vec<u8>,
    video_packets: Vec<PacketRef>,
    audio_packets: Vec<PacketRef>,
    video_chunks: ChunkPlan,
}

fn prepare_transcode(
    bytes: &[u8],
    options: &NativeFmp4TranscodeOptions,
) -> Result<PreparedTranscode> {
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
    let video_codec = video_codec_from_label(&video_track.codec)?;
    let decoder_config = video_track
        .codec_private
        .clone()
        .ok_or_else(|| anyhow::anyhow!("missing Matroska video decoder config"))?;
    let requested_ids = [video_track_id.as_str(), audio_track_id.as_str()];
    let mut indexed = crate::container::matroska::parse_packet_tracks_in_time_window(
        bytes,
        &requested_ids,
        0,
        u64::MAX,
    )
    .ok_or_else(|| anyhow::anyhow!("could not index selected Matroska tracks"))?;
    let audio_packets = indexed
        .pop()
        .ok_or_else(|| anyhow::anyhow!("missing indexed Matroska audio track"))?
        .packets;
    let video_packets = indexed
        .pop()
        .ok_or_else(|| anyhow::anyhow!("missing indexed Matroska video track"))?
        .packets;
    let video_chunks =
        plan_track_chunks(&video_track_id, &video_packets, options.segment_ms.max(1));
    if video_chunks.chunks.is_empty() {
        bail!("selected Matroska video track produced no transcode segments");
    }

    Ok(PreparedTranscode {
        video_track_id,
        audio_track_id,
        video_track: video_track.clone(),
        audio_track: audio_track.clone(),
        video_codec,
        decoder_config,
        video_packets,
        audio_packets,
        video_chunks,
    })
}

fn transcode_prepared_segment(
    bytes: &[u8],
    index: u32,
    options: &NativeFmp4TranscodeOptions,
    prepared: &PreparedTranscode,
) -> Result<TranscodedSegment> {
    let video_chunk = prepared
        .video_chunks
        .chunks
        .get(usize::try_from(index)?)
        .cloned()
        .ok_or_else(|| anyhow::anyhow!("transcode segment {index} is out of range"))?;
    let (video_manifest, video_payload) = extract_indexed_chunk(
        bytes,
        &prepared.video_track_id,
        &prepared.video_packets,
        video_chunk,
    )?;
    let video_start_ms = video_manifest.chunk.start.as_millis();
    let video_end_ms = video_start_ms.saturating_add(video_manifest.chunk.duration.as_millis());
    let (audio_manifest, audio_payload) = extract_indexed_time_range(
        bytes,
        &prepared.audio_track_id,
        &prepared.audio_packets,
        video_start_ms,
        video_end_ms,
        index,
    )?;

    let video_segment = matroska_video_segment(
        &prepared.video_track,
        prepared.video_codec,
        prepared.decoder_config.clone(),
        video_manifest,
        video_payload,
        options,
    )?;

    let audio_segment = matroska_audio_segment(
        &prepared.audio_track,
        audio_manifest,
        audio_payload,
        options.audio_bitrate,
    )?;

    let tracks = vec![
        Fmp4Track {
            id: VIDEO_TRACK_ID,
            kind: Fmp4TrackKind::Video,
            timescale: video_segment.timescale,
            default_sample_duration: video_segment.default_sample_duration,
            default_sample_size: 0,
            default_sample_flags: 0x0101_0000,
            sample_entry: video_segment.sample_entry,
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
        &[video_segment.fragment, audio_segment.fragment],
    )?;

    Ok(TranscodedSegment {
        video_track_id: prepared.video_track_id.clone(),
        audio_track_id: prepared.audio_track_id.clone(),
        video_codec: video_segment.codec,
        audio_codec: audio_segment.codec,
        init_segment: init,
        media_segment: media,
        decoded_video_frames: video_segment.decoded_frame_count,
        video_sample_count: video_segment.sample_count,
        encoded_video_frames: video_segment.encoded_frame_count,
        encoded_audio_frames: audio_segment.encoded_frame_count,
        first_video_pts: video_segment.first_pts,
        first_audio_pts: audio_segment.first_pts,
    })
}

fn extract_indexed_chunk(
    bytes: &[u8],
    track_id: &str,
    packets: &[PacketRef],
    chunk: NativeChunk,
) -> Result<(ExtractedChunk, Vec<u8>)> {
    let payload = extract_packet_payload(bytes, packets, chunk.packet_range)?;
    let samples = packet_samples_for_range(packets, chunk.packet_range)?;
    Ok((
        ExtractedChunk {
            track_id: track_id.to_string(),
            packet_count: chunk
                .packet_range
                .end
                .saturating_sub(chunk.packet_range.start),
            byte_count: payload.len() as u64,
            chunk,
            samples,
        },
        payload,
    ))
}

fn extract_indexed_time_range(
    bytes: &[u8],
    track_id: &str,
    packets: &[PacketRef],
    start_ms: u64,
    end_ms: u64,
    index: u32,
) -> Result<(ExtractedChunk, Vec<u8>)> {
    let start = packets
        .iter()
        .position(|packet| {
            packet
                .pts
                .as_millis()
                .saturating_add(packet.duration.as_millis().max(1))
                > start_ms
        })
        .ok_or_else(|| anyhow::anyhow!("audio track has no packets for segment {index}"))?;
    let end = packets
        .iter()
        .enumerate()
        .skip(start)
        .take_while(|(_, packet)| packet.pts.as_millis() < end_ms)
        .map(|(packet_index, _)| packet_index + 1)
        .last()
        .ok_or_else(|| anyhow::anyhow!("audio track has no packets for segment {index}"))?;
    let range = PacketRange {
        start: u32::try_from(start)?,
        end: u32::try_from(end)?,
    };
    let chunk = NativeChunk {
        index,
        start: TimePoint::millis(start_ms),
        duration: TimeDelta::millis(end_ms.saturating_sub(start_ms).max(1)),
        packet_range: range,
        key_aligned: true,
    };
    extract_indexed_chunk(bytes, track_id, packets, chunk)
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

fn matroska_video_segment(
    track: &MatroskaTrack,
    codec: VideoCodec,
    decoder_config: Vec<u8>,
    manifest: ExtractedChunk,
    payload: Vec<u8>,
    options: &NativeFmp4TranscodeOptions,
) -> Result<VideoSegment> {
    match options.video_mode {
        NativeFmp4VideoMode::Copy => {
            copy_matroska_video_segment(track, decoder_config, manifest, payload)
        }
        NativeFmp4VideoMode::H264 => transcode_h264_video_segment(
            track,
            codec,
            decoder_config,
            &manifest,
            &payload,
            options.video_bitrate,
        ),
    }
}

fn copy_matroska_video_segment(
    track: &MatroskaTrack,
    decoder_config: Vec<u8>,
    manifest: ExtractedChunk,
    payload: Vec<u8>,
) -> Result<VideoSegment> {
    let timescale = chunk_time_scale(&manifest).units_per_second;
    let sample_entry = matroska_video_sample_entry(track, decoder_config)?;
    let default_sample_duration = default_sample_duration(&manifest, timescale);
    let first_pts = manifest.samples.first().map(|sample| sample.pts);
    let sample_count = manifest.samples.len();
    let fragment =
        fragment_track_from_chunk_samples(VIDEO_TRACK_ID, &manifest.samples, payload, timescale)?;
    Ok(VideoSegment {
        fragment,
        sample_entry,
        timescale,
        default_sample_duration,
        codec: video_codec_string(track),
        decoded_frame_count: 0,
        sample_count,
        encoded_frame_count: 0,
        first_pts,
    })
}

fn transcode_h264_video_segment(
    track: &MatroskaTrack,
    codec: VideoCodec,
    decoder_config: Vec<u8>,
    manifest: &ExtractedChunk,
    payload: &[u8],
    bitrate: u32,
) -> Result<VideoSegment> {
    let frame_duration_ns = track.default_duration_ns.unwrap_or(41_666_667);
    let (frame_rate_num, frame_rate_den) = frame_rate_from_duration_ns(frame_duration_ns);
    let decode_format = RawVideoFormat {
        width: track.width.unwrap_or(1920),
        height: track.height.unwrap_or(1080),
        frame_rate_num,
        frame_rate_den,
        pixel_format: RawVideoPixelFormat::Bgra,
    };
    let encode_format = constrained_h264_format(decode_format);
    let decode_input = build_video_decode_input(
        codec,
        chunk_time_scale(manifest),
        Some(&decoder_config),
        &manifest.samples,
        payload,
        true,
    )?;
    let decoded = decode_videotoolbox_bgra_frames(&decode_input, decode_format)?;
    let frame_buffers = decoded
        .frames
        .iter()
        .map(|frame| {
            scale_bgra(
                &frame.pixels,
                decode_format.width,
                decode_format.height,
                encode_format.width,
                encode_format.height,
            )
        })
        .collect::<Result<Vec<_>>>()?;
    let raw_frames = decoded
        .frames
        .iter()
        .zip(frame_buffers.iter())
        .map(|(frame, pixels)| RawVideoFrameRef {
            pts: frame.pts,
            dts: frame.dts,
            duration: frame.duration,
            bytes: pixels.as_ref(),
            keyframe: frame.keyframe,
        })
        .collect::<Vec<_>>();
    let encoded = encode_h264_videotoolbox_bgra_frames(encode_format, &raw_frames, bitrate)?;
    let decoder_config = encoded
        .stream
        .decoder_config
        .clone()
        .ok_or_else(|| anyhow::anyhow!("H.264 encoder did not return decoder config"))?;
    let fragment = fragment_track_from_encoded_video_frames(VIDEO_TRACK_ID, &encoded.frames)?;
    let default_sample_duration = encoded
        .frames
        .first()
        .map(|frame| frame.duration.units.max(1).min(u64::from(u32::MAX)) as u32)
        .unwrap_or(1);
    Ok(VideoSegment {
        fragment,
        sample_entry: Fmp4SampleEntry::Avc {
            codec_config: decoder_config,
            width: encode_format.width.min(u32::from(u16::MAX)) as u16,
            height: encode_format.height.min(u32::from(u16::MAX)) as u16,
        },
        timescale: encoded.stream.time_scale.units_per_second,
        default_sample_duration,
        codec: "h264".to_string(),
        decoded_frame_count: decoded.frames.len(),
        sample_count: encoded.frames.len(),
        encoded_frame_count: encoded.frames.len(),
        first_pts: encoded.frames.first().map(|frame| frame.pts),
    })
}

fn frame_rate_from_duration_ns(duration_ns: u64) -> (u32, u32) {
    let duration_ns = duration_ns.max(1);
    if (41_700_000..=41_720_000).contains(&duration_ns) {
        return (24_000, 1_001);
    }
    if (33_360_000..=33_370_000).contains(&duration_ns) {
        return (30_000, 1_001);
    }
    if (16_680_000..=16_690_000).contains(&duration_ns) {
        return (60_000, 1_001);
    }

    let gcd = gcd_u64(1_000_000_000, duration_ns);
    let num = (1_000_000_000 / gcd).min(u64::from(u32::MAX)) as u32;
    let den = (duration_ns / gcd).min(u64::from(u32::MAX)) as u32;
    (num.max(1), den.max(1))
}

fn gcd_u64(mut a: u64, mut b: u64) -> u64 {
    while b != 0 {
        let next = a % b;
        a = b;
        b = next;
    }
    a.max(1)
}

fn matroska_audio_segment(
    track: &MatroskaTrack,
    manifest: ExtractedChunk,
    payload: Vec<u8>,
    _bitrate: u32,
) -> Result<AudioSegment> {
    match track.codec.as_str() {
        "dts" => bail!(
            "DTS audio track {} requires a native decoder capability that is not currently executable",
            track.index
        ),
        "aac" | "ac3" | "eac3" => copy_matroska_audio_segment(track, manifest, payload),
        other => bail!("selected audio track codec {other} is not supported by native fMP4 output"),
    }
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

fn rescale_units(units: u64, from: TimeScale, to_units_per_second: u32) -> u64 {
    if from.units_per_second == 0 || to_units_per_second == 0 {
        return 0;
    }
    let numerator = u128::from(units) * u128::from(to_units_per_second);
    let denominator = u128::from(from.units_per_second);
    ((numerator + (denominator / 2)) / denominator).min(u128::from(u64::MAX)) as u64
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn scaler_borrows_frames_when_dimensions_already_match() {
        let pixels = [1_u8, 2, 3, 255, 4, 5, 6, 255];
        let scaled = scale_bgra(&pixels, 2, 1, 2, 1).expect("scale");

        assert!(matches!(scaled, std::borrow::Cow::Borrowed(_)));
        assert_eq!(scaled.as_ref(), pixels);
    }

    #[test]
    fn half_scaler_averages_four_source_pixels() {
        let pixels = [
            0_u8, 0, 0, 0, 100, 100, 100, 100, 200, 200, 200, 200, 252, 252, 252, 252,
        ];
        let scaled = scale_bgra(&pixels, 2, 2, 1, 1).expect("scale");

        assert_eq!(scaled.as_ref(), [138, 138, 138, 138]);
    }
}
