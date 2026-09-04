use std::{
    cell::RefCell,
    path::Path,
    sync::atomic::{AtomicU64, Ordering},
};

use anyhow::{Result, bail};
use serde::Serialize;

use super::scaler::{constrained_h264_format, scale_bgra};
use crate::{
    codec::ac3::{parse_ac3_specific_box, parse_eac3_specific_box},
    container::matroska::{
        MatroskaTrack, MatroskaTrackKind, looks_like_ebml, parse_chunk_plan,
        parse_packet_tracks_in_time_window,
    },
    fmp4::{
        Fmp4SampleEntry, Fmp4Track, Fmp4TrackKind, fragment_track_from_chunk_samples,
        fragment_track_from_encoded_audio_frames, fragment_track_from_encoded_video_frames,
        init_segment, media_fragment,
    },
    output::publish_bytes,
    packet::{
        ChunkPlan, ChunkSample, ExtractedChunk, NativeChunk, PacketRange, PacketRef, TimeDelta,
        TimePoint, TimeScale, extract_packet_payload, packet_samples_for_range,
    },
    source::MappedMediaFile,
    transcode::{
        AudioDecodeCodec, BgraDecoderSession, H264EncoderSession, RawVideoFormat, RawVideoFrameRef,
        RawVideoPixelFormat, VideoCodec, build_audio_decode_input, build_video_decode_input,
        decode_truehd_to_interleaved_i16, encode_aac_from_interleaved_i16,
    },
};

const VIDEO_TRACK_ID: u32 = 1;
const AUDIO_TRACK_ID: u32 = 2;
const DEFAULT_VIDEO_BITRATE: u32 = 16_000_000;
const DEFAULT_AUDIO_BITRATE: u32 = 384_000;
const DEFAULT_SEGMENT_MS: u64 = 4_000;
const VIDEO_DECODE_BATCH_PACKETS: usize = 16;

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
    /// Target bitrate for transcoded audio.
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

/// Immutable segment plan exposed by a retained native fMP4 transcode session.
#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct NativeFmp4TranscodePlan {
    /// Selected source video track id.
    pub video_track_id: String,
    /// Selected source audio track id.
    pub audio_track_id: String,
    /// Planned video operation.
    pub video_mode: NativeFmp4VideoMode,
    /// Keyframe-aligned output segment windows.
    pub chunks: ChunkPlan,
}

/// Lifecycle counters for a retained native fMP4 transcode session.
#[derive(Debug, Clone, Copy, Serialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct NativeFmp4TranscodeSessionStats {
    /// Source snapshots created by this session.
    pub source_opens: u64,
    /// Track-selection and cue-plan passes completed at session open.
    pub index_parses: u64,
    /// Segment transcode requests served by this session.
    pub segment_requests: u64,
    /// Source identity validations performed before transcoding.
    pub source_validations: u64,
    /// Native video decoder sessions created, including discontinuity resets.
    pub video_decoder_sessions_created: u64,
    /// Native video encoder sessions created, including discontinuity resets.
    pub video_encoder_sessions_created: u64,
    /// Sequential segment requests that reused both native video sessions.
    pub codec_session_reuses: u64,
    /// Discontinuous requests that required fresh native video sessions.
    pub codec_session_resets: u64,
}

/// Stateful Matroska-to-fMP4 transcode session retaining one immutable source snapshot.
#[derive(Debug)]
pub struct NativeFmp4TranscodeSession {
    source: MappedMediaFile,
    options: NativeFmp4TranscodeOptions,
    prepared: PreparedTranscode,
    codec_pipeline: RefCell<Option<NativeVideoCodecPipeline>>,
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
        let codec_pipeline = match options.video_mode {
            NativeFmp4VideoMode::Copy => None,
            NativeFmp4VideoMode::H264 => Some(NativeVideoCodecPipeline::new(&prepared, &options)?),
        };
        Ok(Self {
            source,
            options,
            prepared,
            codec_pipeline: RefCell::new(codec_pipeline),
            segment_requests: AtomicU64::new(0),
            source_validations: AtomicU64::new(0),
        })
    }

    /// Returns measured session lifecycle counters.
    pub fn stats(&self) -> NativeFmp4TranscodeSessionStats {
        let codec_pipeline = self.codec_pipeline.borrow();
        let codec_stats = codec_pipeline
            .as_ref()
            .map(NativeVideoCodecPipeline::stats)
            .unwrap_or_default();
        NativeFmp4TranscodeSessionStats {
            source_opens: 1,
            index_parses: 1,
            segment_requests: self.segment_requests.load(Ordering::Relaxed),
            source_validations: self.source_validations.load(Ordering::Relaxed),
            video_decoder_sessions_created: codec_stats.decoder_sessions_created,
            video_encoder_sessions_created: codec_stats.encoder_sessions_created,
            codec_session_reuses: codec_stats.reuses,
            codec_session_resets: codec_stats.resets,
        }
    }

    /// Returns the immutable routing and segment plan retained by the session.
    pub fn plan(&self) -> NativeFmp4TranscodePlan {
        NativeFmp4TranscodePlan {
            video_track_id: self.prepared.video_track_id.clone(),
            audio_track_id: self.prepared.audio_track_id.clone(),
            video_mode: self.options.video_mode,
            chunks: self.prepared.video_chunks.clone(),
        }
    }

    fn render(&self, index: u32) -> Result<TranscodedSegment> {
        self.segment_requests.fetch_add(1, Ordering::Relaxed);
        self.source.validate_current()?;
        self.source_validations.fetch_add(1, Ordering::Relaxed);
        let mut codec_pipeline = self.codec_pipeline.borrow_mut();
        if let Some(pipeline) = codec_pipeline.as_mut() {
            pipeline.prepare_for(index, &self.prepared, &self.options)?;
        }
        let result = transcode_prepared_segment(
            self.source.as_ref(),
            index,
            &self.options,
            &self.prepared,
            codec_pipeline.as_mut(),
        );
        if result.is_ok()
            && let Some(pipeline) = codec_pipeline.as_mut()
        {
            pipeline.mark_completed(index);
        }
        result
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

    /// Writes a contiguous media-segment window while reusing native codecs.
    pub fn write_segments(
        &self,
        output_dir: &Path,
        start_index: u32,
        count: u32,
    ) -> Result<Vec<NativeFmp4TranscodeSegmentOutput>> {
        let mut outputs = Vec::with_capacity(usize::try_from(count)?);
        for offset in 0..count {
            let index = start_index
                .checked_add(offset)
                .ok_or_else(|| anyhow::anyhow!("transcode segment index overflowed"))?;
            outputs
                .push(self.write_segment(&output_dir.join(format!("seg-{index:05}.m4s")), index)?);
        }
        Ok(outputs)
    }

    /// Writes an fMP4 HLS media playlist for a planned contiguous window.
    pub fn write_media_playlist(
        &self,
        output: &Path,
        init_uri: &str,
        start_index: u32,
        count: u32,
    ) -> Result<()> {
        let start = usize::try_from(start_index)?;
        let end = start
            .checked_add(usize::try_from(count)?)
            .ok_or_else(|| anyhow::anyhow!("playlist segment window overflowed"))?;
        let chunks = self
            .prepared
            .video_chunks
            .chunks
            .get(start..end)
            .ok_or_else(|| anyhow::anyhow!("playlist segment window is out of range"))?;
        let playlist = render_transcode_media_playlist(
            chunks,
            init_uri,
            start_index,
            end == self.prepared.video_chunks.chunks.len(),
        );
        publish_bytes(output, playlist.as_bytes())?;
        Ok(())
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

fn render_transcode_media_playlist(
    chunks: &[NativeChunk],
    init_uri: &str,
    media_sequence: u32,
    end_list: bool,
) -> String {
    let target_duration = chunks
        .iter()
        .map(|chunk| chunk.duration.as_millis().div_ceil(1_000))
        .max()
        .unwrap_or(1)
        .max(1);
    let mut playlist = format!(
        "#EXTM3U\n#EXT-X-VERSION:7\n#EXT-X-TARGETDURATION:{target_duration}\n#EXT-X-MEDIA-SEQUENCE:{media_sequence}\n#EXT-X-MAP:URI=\"{init_uri}\"\n"
    );
    for chunk in chunks {
        playlist.push_str(&format!(
            "#EXTINF:{:.3},\nseg-{:05}.m4s\n",
            chunk.duration.as_millis() as f64 / 1_000.0,
            chunk.index
        ));
    }
    if end_list {
        playlist.push_str("#EXT-X-ENDLIST\n");
    }
    playlist
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
    video_chunks: ChunkPlan,
}

#[derive(Debug, Default, Clone, Copy)]
struct NativeVideoCodecStats {
    decoder_sessions_created: u64,
    encoder_sessions_created: u64,
    reuses: u64,
    resets: u64,
}

#[derive(Debug)]
struct NativeVideoCodecPipeline {
    decoder: BgraDecoderSession,
    encoder: H264EncoderSession,
    last_completed_index: Option<u32>,
    stats: NativeVideoCodecStats,
}

impl NativeVideoCodecPipeline {
    fn new(prepared: &PreparedTranscode, options: &NativeFmp4TranscodeOptions) -> Result<Self> {
        let (decode_format, encode_format) = video_formats(&prepared.video_track);
        Ok(Self {
            decoder: BgraDecoderSession::new(
                prepared.video_codec,
                decode_format,
                &prepared.decoder_config,
            )?,
            encoder: H264EncoderSession::new(encode_format, options.video_bitrate)?,
            last_completed_index: None,
            stats: NativeVideoCodecStats {
                decoder_sessions_created: 1,
                encoder_sessions_created: 1,
                ..NativeVideoCodecStats::default()
            },
        })
    }

    fn prepare_for(
        &mut self,
        index: u32,
        prepared: &PreparedTranscode,
        options: &NativeFmp4TranscodeOptions,
    ) -> Result<()> {
        match self.last_completed_index {
            None => Ok(()),
            Some(previous) if previous.checked_add(1) == Some(index) => {
                self.stats.reuses = self.stats.reuses.saturating_add(1);
                Ok(())
            }
            Some(_) => {
                let prior = self.stats;
                let replacement = Self::new(prepared, options)?;
                self.decoder = replacement.decoder;
                self.encoder = replacement.encoder;
                self.last_completed_index = None;
                self.stats = NativeVideoCodecStats {
                    decoder_sessions_created: prior.decoder_sessions_created.saturating_add(1),
                    encoder_sessions_created: prior.encoder_sessions_created.saturating_add(1),
                    reuses: prior.reuses,
                    resets: prior.resets.saturating_add(1),
                };
                Ok(())
            }
        }
    }

    fn mark_completed(&mut self, index: u32) {
        self.last_completed_index = Some(index);
    }

    fn stats(&self) -> NativeVideoCodecStats {
        debug_assert_eq!(
            self.decoder.decoded_batches(),
            self.encoder.encoded_batches()
        );
        self.stats
    }
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
    let (audio_track_id, audio_track) =
        select_matroska_audio_track(&meta.tracks, options.audio_track_id.as_deref())
            .ok_or_else(|| anyhow::anyhow!("no matching Matroska audio track found"))?;
    let video_codec = video_codec_from_label(&video_track.codec)?;
    let decoder_config = video_track
        .codec_private
        .clone()
        .or_else(|| (video_codec == VideoCodec::Av1).then(Vec::new))
        .ok_or_else(|| anyhow::anyhow!("missing Matroska video decoder config"))?;
    let video_chunks = parse_chunk_plan(bytes, Some(&video_track_id), options.segment_ms.max(1))
        .ok_or_else(|| anyhow::anyhow!("could not plan selected Matroska video track"))?;
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
        video_chunks,
    })
}

fn transcode_prepared_segment(
    bytes: &[u8],
    index: u32,
    options: &NativeFmp4TranscodeOptions,
    prepared: &PreparedTranscode,
    codec_pipeline: Option<&mut NativeVideoCodecPipeline>,
) -> Result<TranscodedSegment> {
    let mut video_chunk = prepared
        .video_chunks
        .chunks
        .get(usize::try_from(index)?)
        .cloned()
        .ok_or_else(|| anyhow::anyhow!("transcode segment {index} is out of range"))?;
    let video_start_ms = video_chunk.start.as_millis();
    let video_end_ms = video_start_ms.saturating_add(video_chunk.duration.as_millis());
    let video_packets = parse_packet_window(
        bytes,
        &prepared.video_track_id,
        video_start_ms,
        video_end_ms,
    )?;
    video_chunk.packet_range = PacketRange {
        start: 0,
        end: u32::try_from(video_packets.len())?,
    };
    let (video_manifest, video_payload) =
        extract_indexed_chunk(bytes, &prepared.video_track_id, &video_packets, video_chunk)?;
    // Include the preceding audio packet so a packet spanning the video boundary is not
    // dropped. TrueHD uses the same bounded preroll to locate its preceding major sync.
    let audio_scan_start_ms = video_start_ms.saturating_sub(1_000);
    let audio_packets = parse_packet_window(
        bytes,
        &prepared.audio_track_id,
        audio_scan_start_ms,
        video_end_ms,
    )?;
    let (audio_manifest, audio_payload) = extract_indexed_time_range(
        bytes,
        &prepared.audio_track_id,
        &audio_packets,
        video_start_ms,
        video_end_ms,
        index,
        prepared.audio_track.codec == "truehd",
    )?;

    let video_segment = matroska_video_segment(
        &prepared.video_track,
        prepared.video_codec,
        prepared.decoder_config.clone(),
        video_manifest,
        video_payload,
        options,
        codec_pipeline,
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

fn parse_packet_window(
    bytes: &[u8],
    track_id: &str,
    start_ms: u64,
    end_ms: u64,
) -> Result<Vec<PacketRef>> {
    parse_packet_tracks_in_time_window(bytes, &[track_id], start_ms, end_ms)
        .and_then(|mut tracks| tracks.pop())
        .map(|track| track.packets)
        .ok_or_else(|| {
            anyhow::anyhow!("Matroska track {track_id} has no packets in {start_ms}..{end_ms} ms")
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
    preroll_to_truehd_sync: bool,
) -> Result<(ExtractedChunk, Vec<u8>)> {
    let target_start = packets
        .iter()
        .position(|packet| {
            packet
                .pts
                .as_millis()
                .saturating_add(packet.duration.as_millis().max(1))
                > start_ms
        })
        .ok_or_else(|| anyhow::anyhow!("audio track has no packets for segment {index}"))?;
    let start = if preroll_to_truehd_sync {
        (0..=target_start)
            .rev()
            .find(|packet_index| packet_contains_truehd_major_sync(bytes, &packets[*packet_index]))
            .unwrap_or(0)
    } else {
        target_start
    };
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

fn packet_contains_truehd_major_sync(bytes: &[u8], packet: &PacketRef) -> bool {
    let Ok(start) = usize::try_from(packet.source_offset) else {
        return false;
    };
    let Some(end) = start.checked_add(packet.size as usize) else {
        return false;
    };
    let Some(payload) = bytes.get(start..end) else {
        return false;
    };
    payload
        .windows(4)
        .any(|window| matches!(window, [0xF8, 0x72, 0x6F, 0xBA] | [0xF8, 0x72, 0x6F, 0xBB]))
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

fn select_matroska_audio_track<'a>(
    tracks: &'a [MatroskaTrack],
    requested_track_id: Option<&str>,
) -> Option<(String, &'a MatroskaTrack)> {
    let audio_tracks = tracks
        .iter()
        .filter(|track| track.kind == MatroskaTrackKind::Audio)
        .enumerate()
        .map(|(index, track)| (format!("a{index}"), track))
        .collect::<Vec<_>>();
    if let Some(requested) = requested_track_id {
        return audio_tracks
            .into_iter()
            .find(|(track_id, _)| track_id == requested);
    }

    audio_tracks
        .iter()
        .find(|(_, track)| track.default && matroska_audio_track_executable(track))
        .or_else(|| {
            audio_tracks
                .iter()
                .find(|(_, track)| matroska_audio_track_executable(track))
        })
        .or_else(|| audio_tracks.iter().find(|(_, track)| track.default))
        .or_else(|| audio_tracks.first())
        .map(|(track_id, track)| (track_id.clone(), *track))
}

fn matroska_audio_track_executable(track: &MatroskaTrack) -> bool {
    matches!(track.codec.as_str(), "aac" | "ac3" | "eac3" | "truehd")
}

fn video_codec_from_label(codec: &str) -> Result<VideoCodec> {
    match codec {
        "h264" => Ok(VideoCodec::H264),
        "hevc" => Ok(VideoCodec::Hevc),
        "av1" => Ok(VideoCodec::Av1),
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
    codec_pipeline: Option<&mut NativeVideoCodecPipeline>,
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
            codec_pipeline
                .ok_or_else(|| anyhow::anyhow!("missing retained video codec pipeline"))?,
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
    codec_pipeline: &mut NativeVideoCodecPipeline,
) -> Result<VideoSegment> {
    let (decode_format, encode_format) = video_formats(track);
    let sample_batches = manifest
        .samples
        .chunks(VIDEO_DECODE_BATCH_PACKETS)
        .collect::<Vec<_>>();
    let mut encoded_frames = Vec::new();
    let mut output_decoder_config = None;
    let mut output_time_scale = None;
    let mut decoded_frame_count = 0;

    for (batch_index, samples) in sample_batches.iter().enumerate() {
        let decode_input = build_video_decode_input(
            codec,
            chunk_time_scale(manifest),
            Some(&decoder_config),
            samples,
            payload,
            batch_index + 1 == sample_batches.len(),
        )?;
        let decoded = codec_pipeline.decoder.decode(&decode_input)?;
        decoded_frame_count += decoded.frames.len();
        if decoded.frames.is_empty() {
            continue;
        }
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
                // The H.264 encoder disables frame reordering, so output decode
                // order is presentation order even when the source carried B-frames.
                dts: frame.pts,
                duration: frame.duration,
                bytes: pixels.as_ref(),
                keyframe: frame.keyframe,
            })
            .collect::<Vec<_>>();
        let encoded = codec_pipeline.encoder.encode(&raw_frames)?;
        output_time_scale = Some(encoded.stream.time_scale);
        if output_decoder_config.is_none() {
            output_decoder_config = encoded.stream.decoder_config;
        }
        encoded_frames.extend(encoded.frames);
    }

    let decoder_config = output_decoder_config
        .ok_or_else(|| anyhow::anyhow!("H.264 encoder did not return decoder config"))?;
    let fragment = fragment_track_from_encoded_video_frames(VIDEO_TRACK_ID, &encoded_frames)?;
    let default_sample_duration = encoded_frames
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
        timescale: output_time_scale
            .ok_or_else(|| anyhow::anyhow!("H.264 encoder emitted no frames"))?
            .units_per_second,
        default_sample_duration,
        codec: "h264".to_string(),
        decoded_frame_count,
        sample_count: encoded_frames.len(),
        encoded_frame_count: encoded_frames.len(),
        first_pts: encoded_frames.first().map(|frame| frame.pts),
    })
}

fn video_formats(track: &MatroskaTrack) -> (RawVideoFormat, RawVideoFormat) {
    let frame_duration_ns = track.default_duration_ns.unwrap_or(41_666_667);
    let (frame_rate_num, frame_rate_den) = frame_rate_from_duration_ns(frame_duration_ns);
    let decode_format = RawVideoFormat {
        width: track.width.unwrap_or(1920),
        height: track.height.unwrap_or(1080),
        frame_rate_num,
        frame_rate_den,
        pixel_format: RawVideoPixelFormat::Bgra,
    };
    (decode_format, constrained_h264_format(decode_format))
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
    bitrate: u32,
) -> Result<AudioSegment> {
    match track.codec.as_str() {
        "dts" => bail!(
            "DTS audio track {} requires a native decoder capability that is not currently executable",
            track.index
        ),
        "truehd" => transcode_truehd_audio_segment(manifest, payload, bitrate),
        "aac" | "ac3" | "eac3" => copy_matroska_audio_segment(track, manifest, payload),
        other => bail!("selected audio track codec {other} is not supported by native fMP4 output"),
    }
}

fn transcode_truehd_audio_segment(
    manifest: ExtractedChunk,
    payload: Vec<u8>,
    bitrate: u32,
) -> Result<AudioSegment> {
    let input = build_audio_decode_input(
        AudioDecodeCodec::TrueHd,
        chunk_time_scale(&manifest),
        &manifest.samples,
        &payload,
        true,
    )?;
    let decoded = decode_truehd_to_interleaved_i16(&input)?;
    let output_scale = decoded.format.sample_rate;
    let window_start = rescale_units(
        manifest.chunk.start.units,
        manifest.chunk.start.scale,
        output_scale,
    );
    let window_end = window_start.saturating_add(rescale_units(
        manifest.chunk.duration.units,
        manifest.chunk.duration.scale,
        output_scale,
    ));
    let channels = decoded.format.channels as usize;
    let mut first_sample = None;
    let mut reanchored = false;
    let mut pcm = Vec::new();
    for frame in &decoded.frames {
        let frame_start = frame.timing.start_sample;
        let frame_end = frame_start.saturating_add(u64::from(frame.timing.sample_count));
        let overlap_start = frame_start.max(window_start);
        let overlap_end = frame_end.min(window_end);
        if overlap_start >= overlap_end {
            continue;
        }
        let first_frame = first_sample.is_none();
        first_sample.get_or_insert(overlap_start);
        if first_frame {
            reanchored = frame.timing.reanchored || overlap_start != frame_start;
        }
        let skip_frames = usize::try_from(overlap_start.saturating_sub(frame_start))?;
        let take_frames = usize::try_from(overlap_end.saturating_sub(overlap_start))?;
        let sample_start = skip_frames
            .checked_mul(channels)
            .ok_or_else(|| anyhow::anyhow!("TrueHD PCM crop offset overflowed"))?;
        let sample_count = take_frames
            .checked_mul(channels)
            .ok_or_else(|| anyhow::anyhow!("TrueHD PCM crop length overflowed"))?;
        let sample_end = sample_start
            .checked_add(sample_count)
            .ok_or_else(|| anyhow::anyhow!("TrueHD PCM crop range overflowed"))?;
        pcm.extend_from_slice(
            frame
                .samples
                .get(sample_start..sample_end)
                .ok_or_else(|| anyhow::anyhow!("TrueHD PCM frame was shorter than declared"))?,
        );
    }
    let first_sample = first_sample
        .ok_or_else(|| anyhow::anyhow!("TrueHD decoder emitted no PCM in the segment window"))?;
    let mut encoded = encode_aac_from_interleaved_i16(decoded.format, &pcm, bitrate)?;
    for (index, frame) in encoded.frames.iter_mut().enumerate() {
        frame.timing.start_sample = frame.timing.start_sample.saturating_add(first_sample);
        frame.timing.pts.units = frame.timing.pts.units.saturating_add(first_sample);
        if index == 0 {
            frame.timing.reanchored = reanchored;
            frame.discontinuity = reanchored;
        }
    }

    let decoder_config = encoded
        .stream
        .decoder_config
        .clone()
        .ok_or_else(|| anyhow::anyhow!("AAC encoder returned no decoder configuration"))?;
    let channel_count = u16::try_from(encoded.stream.channels)
        .map_err(|_| anyhow::anyhow!("AAC channel count exceeds u16"))?;
    let timescale = encoded.stream.sample_rate;
    let default_sample_duration = encoded
        .frames
        .first()
        .map(|frame| frame.timing.sample_count.max(1))
        .unwrap_or(1);
    let first_pts = encoded.frames.first().map(|frame| frame.timing.pts);
    let fragment = fragment_track_from_encoded_audio_frames(AUDIO_TRACK_ID, &encoded.frames)?;

    Ok(AudioSegment {
        fragment,
        sample_entry: Fmp4SampleEntry::Aac {
            decoder_config,
            channel_count,
            sample_rate: timescale,
        },
        codec: "aac".to_string(),
        timescale,
        default_sample_duration,
        encoded_frame_count: encoded.frames.len(),
        first_pts,
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
    fn matroska_audio_selection_prefers_executable_alternate() {
        let tracks = vec![
            test_audio_track("dts", true),
            test_audio_track("truehd", false),
            test_audio_track("ac3", false),
        ];

        let (track_id, track) =
            select_matroska_audio_track(&tracks, None).expect("selected audio track");

        assert_eq!(track_id, "a1");
        assert_eq!(track.codec, "truehd");
        let (explicit_id, explicit) =
            select_matroska_audio_track(&tracks, Some("a0")).expect("explicit audio track");
        assert_eq!(explicit_id, "a0");
        assert_eq!(explicit.codec, "dts");
    }

    #[test]
    fn transcodes_truehd_fixture_to_clocked_aac_fragment() {
        let payload = truehd::process::EXAMPLE_DATA.to_vec();
        let source_pts = TimePoint::millis(500);
        let manifest = ExtractedChunk {
            track_id: "a0".to_string(),
            chunk: NativeChunk {
                index: 0,
                start: source_pts,
                duration: TimeDelta::millis(10),
                packet_range: PacketRange { start: 0, end: 1 },
                key_aligned: true,
            },
            packet_count: 1,
            byte_count: payload.len() as u64,
            samples: vec![ChunkSample {
                index: 0,
                payload_offset: 0,
                byte_count: payload.len() as u32,
                pts: source_pts,
                dts: source_pts,
                duration: TimeDelta::millis(10),
                keyframe: true,
            }],
        };

        let segment = transcode_truehd_audio_segment(manifest, payload, 384_000)
            .expect("transcode TrueHD fixture");

        assert_eq!(segment.codec, "aac");
        assert!(segment.encoded_frame_count > 0);
        assert_eq!(segment.fragment.samples.len(), segment.encoded_frame_count);
        assert!(!segment.fragment.payload.is_empty());
        assert_eq!(segment.first_pts.unwrap().as_millis(), 500);
        assert!(matches!(segment.sample_entry, Fmp4SampleEntry::Aac { .. }));
    }

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

    #[test]
    fn transcode_playlist_uses_planned_durations_and_sequence() {
        let chunks = [
            NativeChunk {
                index: 4,
                start: TimePoint::millis(10_000),
                duration: TimeDelta::millis(4_125),
                packet_range: PacketRange { start: 0, end: 1 },
                key_aligned: true,
            },
            NativeChunk {
                index: 5,
                start: TimePoint::millis(14_125),
                duration: TimeDelta::millis(3_875),
                packet_range: PacketRange { start: 1, end: 2 },
                key_aligned: true,
            },
        ];

        let playlist = render_transcode_media_playlist(&chunks, "init.mp4", 4, true);

        assert!(playlist.contains("#EXT-X-TARGETDURATION:5"));
        assert!(playlist.contains("#EXT-X-MEDIA-SEQUENCE:4"));
        assert!(playlist.contains("#EXTINF:4.125,\nseg-00004.m4s"));
        assert!(playlist.ends_with("#EXT-X-ENDLIST\n"));
    }

    fn test_audio_track(codec: &str, default: bool) -> MatroskaTrack {
        MatroskaTrack {
            index: 0,
            number: 1,
            kind: MatroskaTrackKind::Audio,
            codec: codec.to_string(),
            language: Some("eng".to_string()),
            name: None,
            default,
            forced: false,
            width: None,
            height: None,
            pixel_format: None,
            channels: Some(6),
            sample_rate: Some(48_000),
            atmos: false,
            object_audio_candidate: false,
            default_duration_ns: None,
            codec_private: None,
        }
    }
}
