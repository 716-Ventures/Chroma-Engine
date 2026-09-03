use std::{
    fs::create_dir_all,
    path::{Path, PathBuf},
};

mod playlist;
mod transport_stream;

use transport_stream::*;

use anyhow::anyhow;
use serde::{Deserialize, Serialize};
use thiserror::Error;

use crate::{
    codec::{
        aac::{AacAudioSpecificConfig, AacError, adts_header, parse_audio_specific_config},
        ac3::{parse_ac3_specific_box, parse_eac3_specific_box},
        h264::{AvcParameterSets, H264ParseError, avc_sample_to_annex_b, parse_avc_decoder_config},
        hevc::{
            HevcParseError, hevc_decoder_config_to_annex_b, hevc_sample_to_annex_b,
            parse_hevc_decoder_config,
        },
    },
    container::{
        matroska::{self, MatroskaTrackKind},
        mp4::{self, Mp4TrackKind},
    },
    error::EngineErrorCode,
    fmp4::{
        Fmp4FragmentTrack, Fmp4SampleEntry, Fmp4Track, Fmp4TrackKind, decode_time_for_timescale,
        init_segment, media_fragment, samples_from_packets_with_timescale,
    },
    output::publish_bytes,
    packet::{ChunkPlan, PacketRef},
    source::MappedMediaFile,
};

use playlist::{
    fmp4_media_playlist_body, fmp4_segment_name, master_playlist_body, media_playlist_body,
    segment_name,
};

type Result<T> = std::result::Result<T, HlsError>;

macro_rules! bail {
    ($($arg:tt)*) => {
        return Err(HlsError::message(format!($($arg)*)))
    };
}

const VIDEO_PID: u16 = 0x0100;
const AUDIO_PID: u16 = 0x0101;
const PMT_PID: u16 = 0x1000;
const VIDEO_STREAM_ID: u8 = 0xe0;
const AUDIO_STREAM_ID: u8 = 0xc0;
const PRIVATE_STREAM_ID: u8 = 0xbd;
const TS_CLOCK: u64 = 90_000;
const MIN_SEGMENT_MS: u64 = 1_000;

#[derive(Debug, Error)]
/// Error returned by native HLS planning and writing.
pub enum HlsError {
    /// Filesystem I/O failed while reading or writing HLS output.
    #[error(transparent)]
    Io(#[from] std::io::Error),
    /// A lower-level parser or muxer rejected the source.
    #[error(transparent)]
    Internal(#[from] anyhow::Error),
    /// AAC parsing or framing failed.
    #[error(transparent)]
    Aac(#[from] AacError),
    /// H.264 parsing or conversion failed.
    #[error(transparent)]
    H264(#[from] H264ParseError),
    /// HEVC parsing or conversion failed.
    #[error(transparent)]
    Hevc(#[from] HevcParseError),
    /// The source or requested operation is unsupported by native HLS.
    #[error("{0}")]
    Unsupported(String),
}

impl HlsError {
    /// Returns the stable Chroma Engine error code for this HLS failure.
    pub fn code(&self) -> EngineErrorCode {
        match self {
            Self::Io(_) => EngineErrorCode::OutputIoFailed,
            Self::Internal(_) => EngineErrorCode::HlsInternal,
            Self::Aac(_) => EngineErrorCode::AacFailed,
            Self::H264(_) => EngineErrorCode::H264Failed,
            Self::Hevc(_) => EngineErrorCode::HevcFailed,
            Self::Unsupported(_) => EngineErrorCode::HlsUnsupported,
        }
    }

    fn message(message: String) -> Self {
        Self::Unsupported(message)
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
/// Options for native HLS generation.
pub struct HlsOptions {
    /// Target segment duration in milliseconds.
    pub segment_target_ms: u64,
    /// Optional semantic audio track id to select.
    pub audio_track_id: Option<String>,
}

impl Default for HlsOptions {
    fn default() -> Self {
        Self {
            segment_target_ms: 4_000,
            audio_track_id: None,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
/// Files and stream metadata produced by an HLS write.
pub struct HlsOutput {
    /// Path to the master playlist.
    pub master_playlist: PathBuf,
    /// Path to the selected variant media playlist.
    pub media_playlist: PathBuf,
    #[serde(skip_serializing_if = "Option::is_none")]
    /// Path to the fMP4 init segment when fMP4 output is used.
    pub init_segment: Option<PathBuf>,
    /// Number of media segments written.
    pub segment_count: usize,
    /// HLS target duration in seconds.
    pub target_duration_seconds: u64,
    /// Selected video track id.
    pub video_track_id: String,
    /// Selected audio track id.
    pub audio_track_id: String,
    /// Estimated stream bandwidth in bits per second.
    pub bandwidth_bits_per_second: u64,
    /// Video codec string advertised in the playlist.
    pub video_codec: String,
    /// Audio codec string advertised in the playlist.
    pub audio_codec: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
/// Input metadata for one HLS audio rendition.
pub struct HlsAudioRenditionInput {
    /// Stable source audio track id.
    pub track_id: String,
    /// RFC 6381 codec string for this audio rendition.
    pub codec: String,
    /// Audio language when available.
    pub language: Option<String>,
    /// Human-readable audio rendition name when available.
    pub name: Option<String>,
    /// Whether this rendition should be marked as default.
    pub default: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
/// One audio rendition emitted in a multi-audio HLS plan.
pub struct HlsAudioRendition {
    /// Stable source audio track id.
    pub track_id: String,
    /// RFC 6381 codec string for this audio rendition.
    pub codec: String,
    /// Audio language when available.
    pub language: Option<String>,
    /// Human-readable audio rendition name.
    pub name: String,
    /// Whether this rendition is the default selection.
    pub default: bool,
    /// Relative URI for this rendition's media playlist.
    pub playlist_uri: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
/// Multi-audio HLS output plan with one shared video playlist.
pub struct HlsMultiAudioOutputPlan {
    /// Selected video track id.
    pub video_track_id: String,
    /// RFC 6381 video codec string.
    pub video_codec: String,
    /// Relative URI for the shared video media playlist.
    pub video_playlist_uri: String,
    /// HLS audio group id referenced by the variant stream.
    pub audio_group_id: String,
    /// One audio rendition per selected audio track.
    pub audio_renditions: Vec<HlsAudioRendition>,
    /// Estimated variant bandwidth in bits per second.
    pub bandwidth_bits_per_second: u64,
    /// Complete HLS master playlist body.
    pub master_playlist: String,
    /// Whether this plan duplicates video work per audio rendition.
    pub duplicates_video_work: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
/// Metadata for a generated HLS media segment.
pub struct HlsSegmentInfo {
    /// Zero-based segment index.
    pub index: usize,
    /// Segment start timestamp in milliseconds.
    pub start_ms: u64,
    /// Segment duration in milliseconds.
    pub duration_ms: u64,
    /// Segment URI relative to its media playlist.
    pub uri: String,
}

/// Open VOD plan that can mux MPEG-TS or fMP4 segments on demand.
pub struct HlsVodPlan {
    source: MappedMediaFile,
    tracks: HlsTrackSet,
    windows: Vec<SegmentWindow>,
    target_duration_seconds: u64,
    bandwidth_bits_per_second: u64,
}

/// Builds a multi-audio HLS master playlist plan around one shared video playlist.
pub fn plan_multi_audio_hls_outputs(
    video_track_id: &str,
    video_codec: &str,
    bandwidth_bits_per_second: u64,
    audio_inputs: &[HlsAudioRenditionInput],
) -> HlsMultiAudioOutputPlan {
    let audio_group_id = "audio".to_string();
    let video_playlist_uri = "video/playlist.m3u8".to_string();
    let audio_renditions = audio_inputs
        .iter()
        .enumerate()
        .map(|(index, input)| {
            let safe_id = safe_hls_path_component(&input.track_id);
            HlsAudioRendition {
                track_id: input.track_id.clone(),
                codec: input.codec.clone(),
                language: input.language.clone(),
                name: input
                    .name
                    .clone()
                    .unwrap_or_else(|| format!("Audio {}", index + 1)),
                default: input.default,
                playlist_uri: format!("audio/{safe_id}/playlist.m3u8"),
            }
        })
        .collect::<Vec<_>>();
    let master_playlist = multi_audio_master_playlist_body(
        video_codec,
        bandwidth_bits_per_second,
        &audio_group_id,
        &video_playlist_uri,
        &audio_renditions,
    );

    HlsMultiAudioOutputPlan {
        video_track_id: video_track_id.to_string(),
        video_codec: video_codec.to_string(),
        video_playlist_uri,
        audio_group_id,
        audio_renditions,
        bandwidth_bits_per_second,
        master_playlist,
        duplicates_video_work: false,
    }
}

/// Lightweight HLS playlist plan that does not retain source bytes.
pub struct HlsVodPlaylistPlan {
    video_track_id: String,
    audio_track_id: String,
    video_codec: String,
    audio_codec: String,
    windows: Vec<SegmentWindow>,
    target_duration_seconds: u64,
    bandwidth_bits_per_second: u64,
}

impl HlsVodPlaylistPlan {
    /// Opens a source and builds playlist metadata without writing segments.
    pub fn open(input: &Path, options: HlsOptions) -> Result<Self> {
        let source = MappedMediaFile::open(input)?;
        let source_len = source.len();
        let bytes = source.as_ref();
        let segment_target_ms = options.segment_target_ms.max(500);

        if mp4::looks_like_mp4(bytes) {
            hls_playlist_plan_from_mp4(
                bytes,
                source_len,
                options.audio_track_id.as_deref(),
                segment_target_ms,
            )
        } else if matroska::looks_like_ebml(bytes) {
            hls_playlist_plan_from_matroska(
                bytes,
                source_len,
                options.audio_track_id.as_deref(),
                segment_target_ms,
            )
        } else {
            bail!("native HLS currently supports MP4/MOV and Matroska/WebM sources");
        }
    }

    /// Returns the number of planned segments.
    pub fn segment_count(&self) -> usize {
        self.windows.len()
    }

    /// Returns the target duration advertised in playlists.
    pub fn target_duration_seconds(&self) -> u64 {
        self.target_duration_seconds
    }

    /// Returns the estimated stream bandwidth in bits per second.
    pub fn bandwidth_bits_per_second(&self) -> u64 {
        self.bandwidth_bits_per_second
    }

    /// Returns the selected video codec string.
    pub fn video_codec(&self) -> &str {
        &self.video_codec
    }

    /// Returns the selected audio codec string.
    pub fn audio_codec(&self) -> &str {
        &self.audio_codec
    }

    /// Returns the selected video track id.
    pub fn video_track_id(&self) -> &str {
        &self.video_track_id
    }

    /// Returns the selected audio track id.
    pub fn audio_track_id(&self) -> &str {
        &self.audio_track_id
    }

    /// Returns metadata for every planned MPEG-TS segment.
    pub fn segments(&self) -> Vec<HlsSegmentInfo> {
        segment_infos(&self.windows)
    }

    /// Returns the master playlist body.
    pub fn master_playlist(&self) -> String {
        master_playlist_body(
            self.video_codec(),
            self.audio_codec(),
            self.bandwidth_bits_per_second,
        )
    }

    /// Returns the MPEG-TS media playlist body.
    pub fn media_playlist(&self) -> String {
        media_playlist_for_windows(self.target_duration_seconds, &self.windows)
    }

    /// Returns the fragmented MP4 media playlist body.
    pub fn fmp4_media_playlist(&self) -> String {
        fmp4_media_playlist_for_windows(self.target_duration_seconds, &self.windows)
    }
}

impl HlsVodPlan {
    /// Opens a source and builds a segment muxing plan.
    pub fn open(input: &Path, options: HlsOptions) -> Result<Self> {
        let source = MappedMediaFile::open(input)?;
        Self::from_source(source, options)
    }

    fn from_source(source: MappedMediaFile, options: HlsOptions) -> Result<Self> {
        let bytes = source.as_ref();
        let tracks = if mp4::looks_like_mp4(bytes) {
            hls_tracks_from_mp4(bytes, options.audio_track_id.as_deref())?
        } else if matroska::looks_like_ebml(bytes) {
            hls_tracks_from_matroska(bytes, options.audio_track_id.as_deref())?
        } else {
            bail!("native HLS currently supports MP4/MOV and Matroska/WebM sources");
        };

        if tracks.video.packets.is_empty() || tracks.audio.packets.is_empty() {
            bail!(
                "native HLS requires one packet-indexed video track and one packet-indexed audio track"
            );
        }

        let segment_target_ms = options.segment_target_ms.max(500);
        let windows = segment_windows(&tracks.video.packets, segment_target_ms);
        if windows.is_empty() {
            bail!("native HLS could not build keyframe-aligned segment windows");
        }
        let target_duration_seconds = target_duration_seconds_for_windows(&windows);
        let bandwidth_bits_per_second = estimate_hls_bandwidth_bits_per_second(&tracks, &windows);

        Ok(Self {
            source,
            tracks,
            windows,
            target_duration_seconds,
            bandwidth_bits_per_second,
        })
    }

    /// Returns the number of planned segments.
    pub fn segment_count(&self) -> usize {
        self.windows.len()
    }

    /// Returns the target duration advertised in playlists.
    pub fn target_duration_seconds(&self) -> u64 {
        self.target_duration_seconds
    }

    /// Returns the estimated stream bandwidth in bits per second.
    pub fn bandwidth_bits_per_second(&self) -> u64 {
        self.bandwidth_bits_per_second
    }

    /// Returns the selected video codec string.
    pub fn video_codec(&self) -> &str {
        &self.tracks.video.codec_string
    }

    /// Returns the selected audio codec string.
    pub fn audio_codec(&self) -> &str {
        &self.tracks.audio.codec_string
    }

    /// Returns the selected video track id.
    pub fn video_track_id(&self) -> &str {
        &self.tracks.video.id
    }

    /// Returns the selected audio track id.
    pub fn audio_track_id(&self) -> &str {
        &self.tracks.audio.id
    }

    /// Returns metadata for every planned MPEG-TS segment.
    pub fn segments(&self) -> Vec<HlsSegmentInfo> {
        segment_infos(&self.windows)
    }

    /// Returns the master playlist body.
    pub fn master_playlist(&self) -> String {
        master_playlist_body(
            self.video_codec(),
            self.audio_codec(),
            self.bandwidth_bits_per_second,
        )
    }

    /// Returns the MPEG-TS media playlist body.
    pub fn media_playlist(&self) -> String {
        media_playlist_for_windows(self.target_duration_seconds, &self.windows)
    }

    /// Returns the fragmented MP4 media playlist body.
    pub fn fmp4_media_playlist(&self) -> String {
        fmp4_media_playlist_for_windows(self.target_duration_seconds, &self.windows)
    }

    /// Muxes one MPEG-TS segment into memory.
    pub fn mux_segment(&self, index: usize) -> Result<Vec<u8>> {
        self.source.validate_current()?;
        let window = self
            .windows
            .get(index)
            .copied()
            .ok_or_else(|| anyhow!("HLS segment index {index} is out of range"))?;
        mux_segment(self.source.as_ref(), &self.tracks, window)
    }

    /// Writes one MPEG-TS segment to disk.
    pub fn write_segment(&self, index: usize, output: &Path) -> Result<HlsSegmentInfo> {
        let window = self
            .windows
            .get(index)
            .copied()
            .ok_or_else(|| anyhow!("HLS segment index {index} is out of range"))?;
        let segment = self.mux_segment(index)?;
        if let Some(parent) = output.parent() {
            create_dir_all(parent)?;
        }
        publish_bytes(output, &segment)?;
        Ok(HlsSegmentInfo {
            index: window.index,
            start_ms: window.start_ms,
            duration_ms: window.end_ms.saturating_sub(window.start_ms).max(1),
            uri: segment_name(window.index),
        })
    }

    /// Writes a range of MPEG-TS segments to `output_dir`.
    pub fn write_segments(
        &self,
        start_index: usize,
        count: usize,
        output_dir: &Path,
    ) -> Result<Vec<HlsSegmentInfo>> {
        if count == 0 {
            return Ok(Vec::new());
        }
        if start_index >= self.windows.len() {
            bail!("HLS segment start index {start_index} is out of range");
        }

        create_dir_all(output_dir)?;
        let end_index = start_index.saturating_add(count).min(self.windows.len());
        let mut written = Vec::with_capacity(end_index.saturating_sub(start_index));
        for index in start_index..end_index {
            let window = self.windows[index];
            let segment = self.mux_segment(index)?;
            publish_bytes(&output_dir.join(segment_name(window.index)), &segment)?;
            written.push(HlsSegmentInfo {
                index: window.index,
                start_ms: window.start_ms,
                duration_ms: window.end_ms.saturating_sub(window.start_ms).max(1),
                uri: segment_name(window.index),
            });
        }
        Ok(written)
    }

    /// Builds the fragmented MP4 init segment.
    pub fn fmp4_init_segment(&self) -> Result<Vec<u8>> {
        self.source.validate_current()?;
        fmp4_init_segment_for_tracks(&self.tracks)
    }

    /// Muxes one fragmented MP4 media segment into memory.
    pub fn mux_fmp4_segment(&self, index: usize) -> Result<Vec<u8>> {
        self.source.validate_current()?;
        let window = self
            .windows
            .get(index)
            .copied()
            .ok_or_else(|| anyhow!("HLS segment index {index} is out of range"))?;
        mux_fmp4_segment(self.source.as_ref(), &self.tracks, window)
    }

    /// Writes one fragmented MP4 media segment to disk.
    pub fn write_fmp4_segment(&self, index: usize, output: &Path) -> Result<HlsSegmentInfo> {
        let window = self
            .windows
            .get(index)
            .copied()
            .ok_or_else(|| anyhow!("HLS segment index {index} is out of range"))?;
        let segment = self.mux_fmp4_segment(index)?;
        if let Some(parent) = output.parent() {
            create_dir_all(parent)?;
        }
        publish_bytes(output, &segment)?;
        Ok(HlsSegmentInfo {
            index: window.index,
            start_ms: window.start_ms,
            duration_ms: window.end_ms.saturating_sub(window.start_ms).max(1),
            uri: fmp4_segment_name(window.index),
        })
    }

    /// Writes a range of fragmented MP4 media segments to `output_dir`.
    pub fn write_fmp4_segments(
        &self,
        start_index: usize,
        count: usize,
        output_dir: &Path,
    ) -> Result<Vec<HlsSegmentInfo>> {
        if count == 0 {
            return Ok(Vec::new());
        }
        if start_index >= self.windows.len() {
            bail!("HLS segment start index {start_index} is out of range");
        }

        create_dir_all(output_dir)?;
        let end_index = start_index.saturating_add(count).min(self.windows.len());
        let mut written = Vec::with_capacity(end_index.saturating_sub(start_index));
        for index in start_index..end_index {
            let output = output_dir.join(fmp4_segment_name(index));
            written.push(self.write_fmp4_segment(index, &output)?);
        }
        Ok(written)
    }
}

fn segment_infos(windows: &[SegmentWindow]) -> Vec<HlsSegmentInfo> {
    windows
        .iter()
        .map(|window| HlsSegmentInfo {
            index: window.index,
            start_ms: window.start_ms,
            duration_ms: window.end_ms.saturating_sub(window.start_ms).max(1),
            uri: segment_name(window.index),
        })
        .collect()
}

fn media_playlist_for_windows(target_duration_seconds: u64, windows: &[SegmentWindow]) -> String {
    let durations: Vec<u64> = windows
        .iter()
        .map(|window| window.end_ms.saturating_sub(window.start_ms).max(1))
        .collect();
    media_playlist_body(target_duration_seconds, &durations)
}

fn fmp4_media_playlist_for_windows(
    target_duration_seconds: u64,
    windows: &[SegmentWindow],
) -> String {
    let durations: Vec<u64> = windows
        .iter()
        .map(|window| window.end_ms.saturating_sub(window.start_ms).max(1))
        .collect();
    fmp4_media_playlist_body(target_duration_seconds, &durations)
}

fn multi_audio_master_playlist_body(
    video_codec: &str,
    bandwidth_bits_per_second: u64,
    audio_group_id: &str,
    video_playlist_uri: &str,
    audio_renditions: &[HlsAudioRendition],
) -> String {
    let mut out = String::from("#EXTM3U\n#EXT-X-VERSION:7\n");
    for rendition in audio_renditions {
        let default = if rendition.default { "YES" } else { "NO" };
        let autoselect = if rendition.language.is_some() {
            "YES"
        } else {
            "NO"
        };
        let language = rendition
            .language
            .as_ref()
            .map(|value| format!(",LANGUAGE=\"{}\"", escape_hls_attr(value)))
            .unwrap_or_default();
        out.push_str(&format!(
            "#EXT-X-MEDIA:TYPE=AUDIO,GROUP-ID=\"{}\",NAME=\"{}\"{language},DEFAULT={default},AUTOSELECT={autoselect},URI=\"{}\"\n",
            escape_hls_attr(audio_group_id),
            escape_hls_attr(&rendition.name),
            escape_hls_attr(&rendition.playlist_uri),
        ));
    }

    let codecs = hls_variant_codecs(video_codec, audio_renditions);
    out.push_str(&format!(
        "#EXT-X-STREAM-INF:BANDWIDTH={bandwidth_bits_per_second},CODECS=\"{codecs}\",AUDIO=\"{}\"\n{video_playlist_uri}\n",
        escape_hls_attr(audio_group_id)
    ));
    out
}

fn hls_variant_codecs(video_codec: &str, audio_renditions: &[HlsAudioRendition]) -> String {
    let mut codecs = vec![video_codec.to_string()];
    for rendition in audio_renditions {
        if !codecs.iter().any(|codec| codec == &rendition.codec) {
            codecs.push(rendition.codec.clone());
        }
    }
    codecs.join(",")
}

fn escape_hls_attr(value: &str) -> String {
    value.replace('\\', "\\\\").replace('"', "\\\"")
}

fn safe_hls_path_component(value: &str) -> String {
    let safe = value
        .chars()
        .map(|ch| {
            if ch.is_ascii_alphanumeric() || matches!(ch, '-' | '_') {
                ch
            } else {
                '_'
            }
        })
        .collect::<String>();
    if safe.is_empty() {
        "audio".to_string()
    } else {
        safe
    }
}

#[derive(Debug, Clone)]
struct HlsTrackSet {
    video: HlsTrack,
    audio: HlsTrack,
}

#[derive(Debug, Clone)]
struct HlsTrack {
    id: String,
    codec_string: String,
    timescale: u32,
    packets: Vec<PacketRef>,
    payload: PayloadKind,
    fmp4_sample_entry: Option<Fmp4SampleEntry>,
}

#[derive(Debug, Clone)]
enum PayloadKind {
    Avc {
        nalu_length_size: u8,
        parameter_sets: AvcParameterSets,
    },
    Hevc {
        nalu_length_size: u8,
        parameter_sets_annex_b: Vec<u8>,
    },
    Aac {
        config: AacAudioSpecificConfig,
    },
    Ac3,
    Eac3,
    RawAudio,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct SegmentWindow {
    index: usize,
    start_ms: u64,
    end_ms: u64,
}

#[derive(Debug, Clone)]
struct TimedPayload {
    pts90: u64,
    dts90: u64,
    bytes: Vec<u8>,
}

/// Writes a complete MPEG-TS HLS VOD package.
pub fn write_hls_vod(input: &Path, output_dir: &Path, options: HlsOptions) -> Result<HlsOutput> {
    let plan = HlsVodPlan::open(input, options)?;

    create_dir_all(output_dir)?;
    let variant_dir = output_dir.join("0");
    create_dir_all(&variant_dir)?;

    for segment in plan.segments() {
        let output = variant_dir.join(&segment.uri);
        plan.write_segment(segment.index, &output)?;
    }

    let media_playlist = variant_dir.join("playlist.m3u8");
    publish_bytes(&media_playlist, plan.media_playlist().as_bytes())?;
    let master_playlist = output_dir.join("master.m3u8");
    publish_bytes(&master_playlist, plan.master_playlist().as_bytes())?;

    Ok(HlsOutput {
        master_playlist,
        media_playlist,
        init_segment: None,
        segment_count: plan.segment_count(),
        target_duration_seconds: plan.target_duration_seconds(),
        video_track_id: plan.video_track_id().to_string(),
        audio_track_id: plan.audio_track_id().to_string(),
        bandwidth_bits_per_second: plan.bandwidth_bits_per_second(),
        video_codec: plan.video_codec().to_string(),
        audio_codec: plan.audio_codec().to_string(),
    })
}

/// Writes a complete fragmented MP4 HLS VOD package.
pub fn write_hls_fmp4_vod(
    input: &Path,
    output_dir: &Path,
    options: HlsOptions,
) -> Result<HlsOutput> {
    let source = map_input(input)?;
    let bytes = source.as_ref();
    if matroska::looks_like_ebml(bytes) {
        source.validate_current()?;
        return write_matroska_hls_fmp4_vod(bytes, output_dir, options);
    }

    let plan = HlsVodPlan::from_source(source, options)?;
    if !supports_fmp4_audio(&plan.tracks.audio.payload) {
        bail!("fMP4 HLS currently requires AAC, AC-3, or E-AC-3 audio");
    }

    create_dir_all(output_dir)?;
    let variant_dir = output_dir.join("0");
    create_dir_all(&variant_dir)?;

    let init_path = variant_dir.join("init.mp4");
    publish_bytes(&init_path, &plan.fmp4_init_segment()?)?;
    for segment in plan.segments() {
        let output = variant_dir.join(fmp4_segment_name(segment.index));
        plan.write_fmp4_segment(segment.index, &output)?;
    }

    let media_playlist = variant_dir.join("playlist.m3u8");
    publish_bytes(&media_playlist, plan.fmp4_media_playlist().as_bytes())?;
    let master_playlist = output_dir.join("master.m3u8");
    publish_bytes(&master_playlist, plan.master_playlist().as_bytes())?;

    Ok(HlsOutput {
        master_playlist,
        media_playlist,
        init_segment: Some(init_path),
        segment_count: plan.segment_count(),
        target_duration_seconds: plan.target_duration_seconds(),
        video_track_id: plan.video_track_id().to_string(),
        audio_track_id: plan.audio_track_id().to_string(),
        bandwidth_bits_per_second: plan.bandwidth_bits_per_second(),
        video_codec: plan.video_codec().to_string(),
        audio_codec: plan.audio_codec().to_string(),
    })
}

/// Writes only the fragmented MP4 init segment for a source.
pub fn write_hls_fmp4_init(input: &Path, output: &Path, options: HlsOptions) -> Result<()> {
    let source = map_input(input)?;
    let bytes = source.as_ref();
    if matroska::looks_like_ebml(bytes) {
        source.validate_current()?;
        return write_matroska_hls_fmp4_init(bytes, output, options);
    }

    let plan = HlsVodPlan::from_source(source, options)?;
    if !supports_fmp4_audio(&plan.tracks.audio.payload) {
        bail!("fMP4 HLS currently requires AAC, AC-3, or E-AC-3 audio");
    }
    if let Some(parent) = output.parent() {
        create_dir_all(parent)?;
    }
    publish_bytes(output, &plan.fmp4_init_segment()?)?;
    Ok(())
}

/// Writes one fragmented MP4 media segment for a source.
pub fn write_hls_fmp4_segment(
    input: &Path,
    index: usize,
    output: &Path,
    options: HlsOptions,
) -> Result<HlsSegmentInfo> {
    let source = map_input(input)?;
    let bytes = source.as_ref();
    if matroska::looks_like_ebml(bytes) {
        source.validate_current()?;
        return write_matroska_hls_fmp4_segment(bytes, index, output, options);
    }

    let plan = HlsVodPlan::from_source(source, options)?;
    if !supports_fmp4_audio(&plan.tracks.audio.payload) {
        bail!("fMP4 HLS currently requires AAC, AC-3, or E-AC-3 audio");
    }
    plan.write_fmp4_segment(index, output)
}

/// Writes a range of fragmented MP4 media segments for a source.
pub fn write_hls_fmp4_segments(
    input: &Path,
    output_dir: &Path,
    start_index: usize,
    count: usize,
    options: HlsOptions,
) -> Result<Vec<HlsSegmentInfo>> {
    if count == 0 {
        return Ok(Vec::new());
    }
    let source = map_input(input)?;
    let bytes = source.as_ref();
    if matroska::looks_like_ebml(bytes) {
        source.validate_current()?;
        return write_matroska_hls_fmp4_segments(bytes, output_dir, start_index, count, options);
    }

    let plan = HlsVodPlan::from_source(source, options)?;
    if !supports_fmp4_audio(&plan.tracks.audio.payload) {
        bail!("fMP4 HLS currently requires AAC, AC-3, or E-AC-3 audio");
    }
    plan.write_fmp4_segments(start_index, count, output_dir)
}

fn map_input(input: &Path) -> Result<MappedMediaFile> {
    Ok(MappedMediaFile::open(input)?)
}

/// Writes one MPEG-TS media segment for a source.
pub fn write_hls_segment(
    input: &Path,
    index: usize,
    output: &Path,
    options: HlsOptions,
) -> Result<HlsSegmentInfo> {
    let source = MappedMediaFile::open(input)?;
    let bytes = source.as_ref();
    if matroska::looks_like_ebml(bytes) {
        source.validate_current()?;
        return write_matroska_hls_segment(bytes, index, output, options);
    }

    let plan = HlsVodPlan::from_source(source, options)?;
    plan.write_segment(index, output)
}

/// Writes a range of MPEG-TS media segments for a source.
pub fn write_hls_segments(
    input: &Path,
    output_dir: &Path,
    start_index: usize,
    count: usize,
    options: HlsOptions,
) -> Result<Vec<HlsSegmentInfo>> {
    if count == 0 {
        return Ok(Vec::new());
    }

    let source = MappedMediaFile::open(input)?;
    let bytes = source.as_ref();
    if matroska::looks_like_ebml(bytes) {
        source.validate_current()?;
        let segment_target_ms = options.segment_target_ms.max(500);
        let plan = hls_playlist_plan_from_matroska(
            bytes,
            bytes.len() as u64,
            options.audio_track_id.as_deref(),
            segment_target_ms,
        )?;
        if start_index >= plan.windows.len() {
            bail!("HLS segment start index {start_index} is out of range");
        }
        create_dir_all(output_dir)?;
        let end_index = start_index.saturating_add(count).min(plan.windows.len());
        let mut written = Vec::with_capacity(end_index.saturating_sub(start_index));
        for index in start_index..end_index {
            written.push(write_matroska_hls_segment_from_plan(
                bytes,
                index,
                &output_dir.join(segment_name(index)),
                &plan,
            )?);
        }
        return Ok(written);
    }

    let plan = HlsVodPlan::from_source(source, options)?;
    plan.write_segments(start_index, count, output_dir)
}

fn select_mp4_hls_track<'a>(
    tracks: &'a [mp4::Mp4Track],
    kind: Mp4TrackKind,
    requested_track_id: Option<&str>,
) -> Option<(String, &'a mp4::Mp4Track)> {
    let mut video_index = 0_u32;
    let mut audio_index = 0_u32;
    let mut subtitle_index = 0_u32;
    let mut unknown_index = 0_u32;
    for track in tracks {
        let track_id = match track.kind {
            Mp4TrackKind::Video => next_semantic_track_id("v", &mut video_index),
            Mp4TrackKind::Audio => next_semantic_track_id("a", &mut audio_index),
            Mp4TrackKind::Subtitle => next_semantic_track_id("s", &mut subtitle_index),
            Mp4TrackKind::Unknown => next_semantic_track_id("x", &mut unknown_index),
        };
        if track.kind != kind || !is_supported_hls_codec(kind, &track.codec) {
            continue;
        }
        if requested_track_id
            .map(|requested| requested == track_id)
            .unwrap_or(true)
        {
            return Some((track_id, track));
        }
    }
    None
}

fn select_matroska_hls_track<'a>(
    tracks: &'a [matroska::MatroskaTrack],
    kind: MatroskaTrackKind,
    requested_track_id: Option<&str>,
) -> Option<(String, &'a matroska::MatroskaTrack)> {
    let mut video_index = 0_u32;
    let mut audio_index = 0_u32;
    let mut subtitle_index = 0_u32;
    let mut unknown_index = 0_u32;
    let mut first_supported = None;
    for track in tracks {
        let track_id = match track.kind {
            MatroskaTrackKind::Video => next_semantic_track_id("v", &mut video_index),
            MatroskaTrackKind::Audio => next_semantic_track_id("a", &mut audio_index),
            MatroskaTrackKind::Subtitle => next_semantic_track_id("s", &mut subtitle_index),
            MatroskaTrackKind::Unknown => next_semantic_track_id("x", &mut unknown_index),
        };
        if track.kind != kind || !is_supported_hls_codec(kind, &track.codec) {
            continue;
        }
        if requested_track_id
            .map(|requested| requested == track_id)
            .unwrap_or(false)
        {
            return Some((track_id, track));
        }
        if requested_track_id.is_none() && track.default {
            return Some((track_id, track));
        }
        if requested_track_id.is_none() && first_supported.is_none() {
            first_supported = Some((track_id, track));
        }
    }
    first_supported
}

fn next_semantic_track_id(prefix: &str, counter: &mut u32) -> String {
    let id = format!("{prefix}{counter}");
    *counter = counter.saturating_add(1);
    id
}

fn is_supported_hls_codec<K>(kind: K, codec: &str) -> bool
where
    K: Into<HlsTrackKind>,
{
    matches!(
        (kind.into(), codec),
        (HlsTrackKind::Video, "h264" | "hevc") | (HlsTrackKind::Audio, "aac" | "ac3" | "eac3")
    )
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum HlsTrackKind {
    Video,
    Audio,
    Other,
}

impl From<Mp4TrackKind> for HlsTrackKind {
    fn from(kind: Mp4TrackKind) -> Self {
        match kind {
            Mp4TrackKind::Video => Self::Video,
            Mp4TrackKind::Audio => Self::Audio,
            Mp4TrackKind::Subtitle | Mp4TrackKind::Unknown => Self::Other,
        }
    }
}

pub fn write_hls_fmp4_segment_window(
    input: &Path,
    output: &Path,
    options: HlsOptions,
    index: usize,
    start_ms: u64,
    end_ms: u64,
) -> Result<HlsSegmentInfo> {
    let source = MappedMediaFile::open(input)?;
    let bytes = source.as_ref();
    if !matroska::looks_like_ebml(bytes) {
        bail!("windowed fMP4 HLS segment currently supports Matroska/WebM sources");
    }
    let window = SegmentWindow {
        index,
        start_ms,
        end_ms: end_ms.max(start_ms.saturating_add(1)),
    };
    let tracks =
        matroska_hls_tracks_for_selected_window(bytes, options.audio_track_id.as_deref(), window)?;
    if !supports_fmp4_audio(&tracks.audio.payload) {
        bail!("fMP4 HLS currently requires AAC, AC-3, or E-AC-3 audio");
    }
    let segment = mux_fmp4_segment(bytes, &tracks, window)?;
    if let Some(parent) = output.parent() {
        create_dir_all(parent)?;
    }
    publish_bytes(output, &segment)?;
    Ok(HlsSegmentInfo {
        index: window.index,
        start_ms: window.start_ms,
        duration_ms: window.end_ms.saturating_sub(window.start_ms).max(1),
        uri: fmp4_segment_name(window.index),
    })
}

impl From<MatroskaTrackKind> for HlsTrackKind {
    fn from(kind: MatroskaTrackKind) -> Self {
        match kind {
            MatroskaTrackKind::Video => Self::Video,
            MatroskaTrackKind::Audio => Self::Audio,
            MatroskaTrackKind::Subtitle | MatroskaTrackKind::Unknown => Self::Other,
        }
    }
}

fn hls_tracks_from_mp4(
    bytes: &[u8],
    requested_audio_track_id: Option<&str>,
) -> Result<HlsTrackSet> {
    let meta = mp4::parse_basic_metadata(bytes);
    let (video_track_id, video_meta) =
        select_mp4_hls_track(&meta.tracks, Mp4TrackKind::Video, None)
            .ok_or_else(|| anyhow!("native HLS MP4 path currently requires H.264 or HEVC video"))?;
    let (audio_track_id, audio_meta) =
        select_mp4_hls_track(&meta.tracks, Mp4TrackKind::Audio, requested_audio_track_id)
            .ok_or_else(|| {
                requested_audio_track_id
                    .map(|track_id| {
                        anyhow!(
                            "native HLS MP4 path could not use requested audio track {track_id}"
                        )
                    })
                    .unwrap_or_else(|| {
                        anyhow!("native HLS MP4 path currently requires AAC, AC-3, or E-AC-3 audio")
                    })
            })?;
    let video_config = mp4::parse_codec_config(bytes, Some(&video_track_id))
        .ok_or_else(|| anyhow!("missing MP4 video decoder config"))?;
    let audio_config = mp4::parse_codec_config(bytes, Some(&audio_track_id))
        .ok_or_else(|| anyhow!("missing MP4 audio decoder config"))?;
    let video_packets = mp4::parse_packet_track(bytes, Some(&video_track_id))
        .ok_or_else(|| anyhow!("missing MP4 video packet index"))?
        .packets;
    let audio_packets = mp4::parse_packet_track(bytes, Some(&audio_track_id))
        .ok_or_else(|| anyhow!("missing MP4 audio packet index"))?
        .packets;

    let video_payload = match video_meta.codec.as_str() {
        "h264" => {
            let avc = hex_to_bytes(
                video_config
                    .description_hex
                    .as_deref()
                    .ok_or_else(|| anyhow!("missing avcC"))?,
            )?;
            PayloadKind::Avc {
                nalu_length_size: video_config.nalu_length_size.unwrap_or(4),
                parameter_sets: parse_avc_decoder_config(&avc)?,
            }
        }
        "hevc" => {
            let hvc = hex_to_bytes(
                video_config
                    .description_hex
                    .as_deref()
                    .ok_or_else(|| anyhow!("missing hvcC"))?,
            )?;
            let parameter_sets = parse_hevc_decoder_config(&hvc)?;
            PayloadKind::Hevc {
                nalu_length_size: video_config
                    .nalu_length_size
                    .unwrap_or(parameter_sets.nalu_length_size),
                parameter_sets_annex_b: hevc_decoder_config_to_annex_b(&hvc)?,
            }
        }
        other => bail!("native HLS MP4 video codec {other} is not supported"),
    };
    let video_fmp4_sample_entry = match video_meta.codec.as_str() {
        "h264" => Some(Fmp4SampleEntry::Avc {
            codec_config: hex_to_bytes(
                video_config
                    .description_hex
                    .as_deref()
                    .ok_or_else(|| anyhow!("missing avcC"))?,
            )?,
            width: clamped_u16(video_meta.width.unwrap_or(0)),
            height: clamped_u16(video_meta.height.unwrap_or(0)),
        }),
        "hevc" => Some(Fmp4SampleEntry::Hevc {
            codec_config: hex_to_bytes(
                video_config
                    .description_hex
                    .as_deref()
                    .ok_or_else(|| anyhow!("missing hvcC"))?,
            )?,
            width: clamped_u16(video_meta.width.unwrap_or(0)),
            height: clamped_u16(video_meta.height.unwrap_or(0)),
        }),
        _ => None,
    };
    let audio_payload = match audio_meta.codec.as_str() {
        "aac" => {
            let asc = hex_to_bytes(
                audio_config
                    .description_hex
                    .as_deref()
                    .ok_or_else(|| anyhow!("missing AudioSpecificConfig"))?,
            )?;
            PayloadKind::Aac {
                config: parse_audio_specific_config(&asc)?,
            }
        }
        "ac3" => PayloadKind::Ac3,
        "eac3" => PayloadKind::Eac3,
        "mp3" | "flac" | "alac" => PayloadKind::RawAudio,
        other => bail!("native HLS MP4 audio codec {other} is not supported"),
    };
    let audio_fmp4_sample_entry = match audio_meta.codec.as_str() {
        "aac" => Some(Fmp4SampleEntry::Aac {
            decoder_config: hex_to_bytes(
                audio_config
                    .description_hex
                    .as_deref()
                    .ok_or_else(|| anyhow!("missing AudioSpecificConfig"))?,
            )?,
            channel_count: clamped_u16(audio_meta.channels.unwrap_or(2)),
            sample_rate: audio_meta.sample_rate.unwrap_or(48_000),
        }),
        "ac3" => {
            let first_packet = audio_packets
                .first()
                .ok_or_else(|| anyhow!("missing MP4 AC-3 packet for dac3"))?;
            let frame = packet_bytes(bytes, first_packet)?;
            Some(Fmp4SampleEntry::Ac3 {
                dac3: parse_ac3_specific_box(frame)?.dac3_payload(),
                channel_count: clamped_u16(audio_meta.channels.unwrap_or(2)),
                sample_rate: audio_meta.sample_rate.unwrap_or(48_000),
            })
        }
        "eac3" => {
            let first_packet = audio_packets
                .first()
                .ok_or_else(|| anyhow!("missing MP4 E-AC-3 packet for dec3"))?;
            let access_unit = packet_bytes(bytes, first_packet)?;
            Some(Fmp4SampleEntry::Eac3 {
                dec3: parse_eac3_specific_box(access_unit)?.dec3_payload(),
                channel_count: clamped_u16(audio_meta.channels.unwrap_or(2)),
                sample_rate: audio_meta.sample_rate.unwrap_or(48_000),
            })
        }
        "mp3" => Some(Fmp4SampleEntry::Mp3 {
            channel_count: clamped_u16(audio_meta.channels.unwrap_or(2)),
            sample_rate: audio_meta.sample_rate.unwrap_or(48_000),
        }),
        "flac" => Some(Fmp4SampleEntry::Flac {
            stream_info: hex_to_bytes(
                audio_config
                    .description_hex
                    .as_deref()
                    .ok_or_else(|| anyhow!("missing FLAC STREAMINFO"))?,
            )?,
            channel_count: clamped_u16(audio_meta.channels.unwrap_or(2)),
            sample_rate: audio_meta.sample_rate.unwrap_or(48_000),
        }),
        "alac" => Some(Fmp4SampleEntry::Alac {
            codec_config: hex_to_bytes(
                audio_config
                    .description_hex
                    .as_deref()
                    .ok_or_else(|| anyhow!("missing ALAC codec config"))?,
            )?,
            channel_count: clamped_u16(audio_meta.channels.unwrap_or(2)),
            sample_rate: audio_meta.sample_rate.unwrap_or(48_000),
        }),
        _ => None,
    };
    let video_timescale = video_packets
        .first()
        .map(|packet| packet.dts.scale.units_per_second)
        .unwrap_or(90_000);
    let audio_timescale = audio_packets
        .first()
        .map(|packet| packet.dts.scale.units_per_second)
        .unwrap_or_else(|| audio_meta.sample_rate.unwrap_or(48_000));

    Ok(HlsTrackSet {
        video: HlsTrack {
            id: video_track_id,
            codec_string: video_config
                .codec_string
                .unwrap_or_else(|| fallback_video_codec_string(video_meta.codec.as_str())),
            timescale: video_timescale,
            packets: video_packets,
            payload: video_payload,
            fmp4_sample_entry: video_fmp4_sample_entry,
        },
        audio: HlsTrack {
            id: audio_track_id,
            codec_string: audio_config
                .codec_string
                .unwrap_or_else(|| fallback_audio_codec_string(audio_meta.codec.as_str())),
            timescale: audio_timescale,
            packets: audio_packets,
            payload: audio_payload,
            fmp4_sample_entry: audio_fmp4_sample_entry,
        },
    })
}

fn hls_tracks_from_matroska(
    bytes: &[u8],
    requested_audio_track_id: Option<&str>,
) -> Result<HlsTrackSet> {
    let meta = matroska::parse_basic_metadata(bytes);
    let (video_track_id, video) =
        select_matroska_hls_track(&meta.tracks, MatroskaTrackKind::Video, None).ok_or_else(
            || anyhow!("native HLS Matroska path currently requires H.264 or HEVC video"),
        )?;
    let (audio_track_id, audio) = select_matroska_hls_track(
        &meta.tracks,
        MatroskaTrackKind::Audio,
        requested_audio_track_id,
    )
    .ok_or_else(|| {
        requested_audio_track_id
            .map(|track_id| {
                anyhow!("native HLS Matroska path could not use requested audio track {track_id}")
            })
            .unwrap_or_else(|| {
                anyhow!("native HLS Matroska path currently requires AAC, AC-3, or E-AC-3 audio")
            })
    })?;
    let video_packets = matroska::parse_packet_track(bytes, Some(&video_track_id))
        .ok_or_else(|| anyhow!("missing Matroska video packet index"))?
        .packets;
    let audio_packets = matroska::parse_packet_track(bytes, Some(&audio_track_id))
        .ok_or_else(|| anyhow!("missing Matroska audio packet index"))?
        .packets;
    let video_payload = match video.codec.as_str() {
        "h264" => {
            let avc = video
                .codec_private
                .as_deref()
                .ok_or_else(|| anyhow!("missing Matroska avcC private data"))?;
            let parameter_sets = parse_avc_decoder_config(avc)?;
            PayloadKind::Avc {
                nalu_length_size: parameter_sets.nalu_length_size,
                parameter_sets,
            }
        }
        "hevc" => {
            let hvc = video
                .codec_private
                .as_deref()
                .ok_or_else(|| anyhow!("missing Matroska hvcC private data"))?;
            let parameter_sets = parse_hevc_decoder_config(hvc)?;
            PayloadKind::Hevc {
                nalu_length_size: parameter_sets.nalu_length_size,
                parameter_sets_annex_b: hevc_decoder_config_to_annex_b(hvc)?,
            }
        }
        other => bail!("native HLS Matroska video codec {other} is not supported"),
    };
    let video_fmp4_sample_entry = match video.codec.as_str() {
        "h264" => Some(Fmp4SampleEntry::Avc {
            codec_config: video
                .codec_private
                .clone()
                .ok_or_else(|| anyhow!("missing Matroska avcC private data"))?,
            width: clamped_u16(video.width.unwrap_or(0)),
            height: clamped_u16(video.height.unwrap_or(0)),
        }),
        "hevc" => Some(Fmp4SampleEntry::Hevc {
            codec_config: video
                .codec_private
                .clone()
                .ok_or_else(|| anyhow!("missing Matroska hvcC private data"))?,
            width: clamped_u16(video.width.unwrap_or(0)),
            height: clamped_u16(video.height.unwrap_or(0)),
        }),
        _ => None,
    };
    let audio_payload = match audio.codec.as_str() {
        "aac" => {
            let asc = audio
                .codec_private
                .as_deref()
                .ok_or_else(|| anyhow!("missing Matroska AAC private data"))?;
            PayloadKind::Aac {
                config: parse_audio_specific_config(asc)?,
            }
        }
        "ac3" => PayloadKind::Ac3,
        "eac3" => PayloadKind::Eac3,
        "mp3" | "flac" | "alac" => PayloadKind::RawAudio,
        other => bail!("native HLS Matroska audio codec {other} is not supported"),
    };
    let audio_fmp4_sample_entry = match audio.codec.as_str() {
        "aac" => Some(Fmp4SampleEntry::Aac {
            decoder_config: audio
                .codec_private
                .clone()
                .ok_or_else(|| anyhow!("missing Matroska AAC private data"))?,
            channel_count: clamped_u16(audio.channels.unwrap_or(2)),
            sample_rate: audio.sample_rate.unwrap_or(48_000),
        }),
        "ac3" => {
            let first_packet = audio_packets
                .first()
                .ok_or_else(|| anyhow!("missing Matroska AC-3 packet for dac3"))?;
            let frame = packet_bytes(bytes, first_packet)?;
            Some(Fmp4SampleEntry::Ac3 {
                dac3: parse_ac3_specific_box(frame)?.dac3_payload(),
                channel_count: clamped_u16(audio.channels.unwrap_or(2)),
                sample_rate: audio.sample_rate.unwrap_or(48_000),
            })
        }
        "eac3" => {
            let first_packet = audio_packets
                .first()
                .ok_or_else(|| anyhow!("missing Matroska E-AC-3 packet for dec3"))?;
            let access_unit = packet_bytes(bytes, first_packet)?;
            Some(Fmp4SampleEntry::Eac3 {
                dec3: parse_eac3_specific_box(access_unit)?.dec3_payload(),
                channel_count: clamped_u16(audio.channels.unwrap_or(2)),
                sample_rate: audio.sample_rate.unwrap_or(48_000),
            })
        }
        "mp3" => Some(Fmp4SampleEntry::Mp3 {
            channel_count: clamped_u16(audio.channels.unwrap_or(2)),
            sample_rate: audio.sample_rate.unwrap_or(48_000),
        }),
        "flac" => Some(Fmp4SampleEntry::Flac {
            stream_info: audio
                .codec_private
                .clone()
                .ok_or_else(|| anyhow!("missing Matroska FLAC STREAMINFO"))?,
            channel_count: clamped_u16(audio.channels.unwrap_or(2)),
            sample_rate: audio.sample_rate.unwrap_or(48_000),
        }),
        "alac" => Some(Fmp4SampleEntry::Alac {
            codec_config: audio
                .codec_private
                .clone()
                .ok_or_else(|| anyhow!("missing Matroska ALAC codec config"))?,
            channel_count: clamped_u16(audio.channels.unwrap_or(2)),
            sample_rate: audio.sample_rate.unwrap_or(48_000),
        }),
        _ => None,
    };

    Ok(HlsTrackSet {
        video: HlsTrack {
            id: video_track_id,
            codec_string: matroska_video_codec_string(video),
            timescale: video_packets
                .first()
                .map(|packet| packet.dts.scale.units_per_second)
                .unwrap_or(90_000),
            packets: video_packets,
            payload: video_payload,
            fmp4_sample_entry: video_fmp4_sample_entry,
        },
        audio: HlsTrack {
            id: audio_track_id,
            codec_string: matroska_audio_codec_string(audio),
            timescale: audio_packets
                .first()
                .map(|packet| packet.dts.scale.units_per_second)
                .unwrap_or(48_000),
            packets: audio_packets,
            payload: audio_payload,
            fmp4_sample_entry: audio_fmp4_sample_entry,
        },
    })
}

fn matroska_video_payload_kind(track: &matroska::MatroskaTrack) -> Result<PayloadKind> {
    match track.codec.as_str() {
        "h264" => {
            let avc = track
                .codec_private
                .as_deref()
                .ok_or_else(|| anyhow!("missing Matroska avcC private data"))?;
            let parameter_sets = parse_avc_decoder_config(avc)?;
            Ok(PayloadKind::Avc {
                nalu_length_size: parameter_sets.nalu_length_size,
                parameter_sets,
            })
        }
        "hevc" => {
            let hvc = track
                .codec_private
                .as_deref()
                .ok_or_else(|| anyhow!("missing Matroska hvcC private data"))?;
            let parameter_sets = parse_hevc_decoder_config(hvc)?;
            Ok(PayloadKind::Hevc {
                nalu_length_size: parameter_sets.nalu_length_size,
                parameter_sets_annex_b: hevc_decoder_config_to_annex_b(hvc)?,
            })
        }
        other => bail!("native HLS Matroska video codec {other} is not supported"),
    }
}

fn matroska_audio_payload_kind(track: &matroska::MatroskaTrack) -> Result<PayloadKind> {
    match track.codec.as_str() {
        "aac" => {
            let asc = track
                .codec_private
                .as_deref()
                .ok_or_else(|| anyhow!("missing Matroska AAC private data"))?;
            Ok(PayloadKind::Aac {
                config: parse_audio_specific_config(asc)?,
            })
        }
        "ac3" => Ok(PayloadKind::Ac3),
        "eac3" => Ok(PayloadKind::Eac3),
        "mp3" | "flac" | "alac" => Ok(PayloadKind::RawAudio),
        other => bail!("native HLS Matroska audio codec {other} is not supported"),
    }
}

fn matroska_video_fmp4_sample_entry(
    track: &matroska::MatroskaTrack,
) -> Result<Option<Fmp4SampleEntry>> {
    match track.codec.as_str() {
        "h264" => Ok(Some(Fmp4SampleEntry::Avc {
            codec_config: track
                .codec_private
                .clone()
                .ok_or_else(|| anyhow!("missing Matroska avcC private data"))?,
            width: clamped_u16(track.width.unwrap_or(0)),
            height: clamped_u16(track.height.unwrap_or(0)),
        })),
        "hevc" => Ok(Some(Fmp4SampleEntry::Hevc {
            codec_config: track
                .codec_private
                .clone()
                .ok_or_else(|| anyhow!("missing Matroska hvcC private data"))?,
            width: clamped_u16(track.width.unwrap_or(0)),
            height: clamped_u16(track.height.unwrap_or(0)),
        })),
        _ => Ok(None),
    }
}

fn matroska_audio_fmp4_sample_entry(
    bytes: &[u8],
    track: &matroska::MatroskaTrack,
    packets: &[PacketRef],
) -> Result<Option<Fmp4SampleEntry>> {
    match track.codec.as_str() {
        "aac" => Ok(Some(Fmp4SampleEntry::Aac {
            decoder_config: track
                .codec_private
                .clone()
                .ok_or_else(|| anyhow!("missing Matroska AAC private data"))?,
            channel_count: clamped_u16(track.channels.unwrap_or(2)),
            sample_rate: track.sample_rate.unwrap_or(48_000),
        })),
        "ac3" => {
            let first_packet = packets
                .first()
                .ok_or_else(|| anyhow!("missing Matroska AC-3 packet for dac3"))?;
            let frame = packet_bytes(bytes, first_packet)?;
            Ok(Some(Fmp4SampleEntry::Ac3 {
                dac3: parse_ac3_specific_box(frame)?.dac3_payload(),
                channel_count: clamped_u16(track.channels.unwrap_or(2)),
                sample_rate: track.sample_rate.unwrap_or(48_000),
            }))
        }
        "eac3" => {
            let first_packet = packets
                .first()
                .ok_or_else(|| anyhow!("missing Matroska E-AC-3 packet for dec3"))?;
            let access_unit = packet_bytes(bytes, first_packet)?;
            Ok(Some(Fmp4SampleEntry::Eac3 {
                dec3: parse_eac3_specific_box(access_unit)?.dec3_payload(),
                channel_count: clamped_u16(track.channels.unwrap_or(2)),
                sample_rate: track.sample_rate.unwrap_or(48_000),
            }))
        }
        "mp3" => Ok(Some(Fmp4SampleEntry::Mp3 {
            channel_count: clamped_u16(track.channels.unwrap_or(2)),
            sample_rate: track.sample_rate.unwrap_or(48_000),
        })),
        "flac" => Ok(Some(Fmp4SampleEntry::Flac {
            stream_info: track
                .codec_private
                .clone()
                .ok_or_else(|| anyhow!("missing Matroska FLAC STREAMINFO"))?,
            channel_count: clamped_u16(track.channels.unwrap_or(2)),
            sample_rate: track.sample_rate.unwrap_or(48_000),
        })),
        "alac" => Ok(Some(Fmp4SampleEntry::Alac {
            codec_config: track
                .codec_private
                .clone()
                .ok_or_else(|| anyhow!("missing Matroska ALAC codec config"))?,
            channel_count: clamped_u16(track.channels.unwrap_or(2)),
            sample_rate: track.sample_rate.unwrap_or(48_000),
        })),
        _ => Ok(None),
    }
}

fn hls_playlist_plan_from_mp4(
    bytes: &[u8],
    source_len: u64,
    requested_audio_track_id: Option<&str>,
    segment_target_ms: u64,
) -> Result<HlsVodPlaylistPlan> {
    let meta = mp4::parse_basic_metadata(bytes);
    let (video_track_id, video_meta) =
        select_mp4_hls_track(&meta.tracks, Mp4TrackKind::Video, None)
            .ok_or_else(|| anyhow!("native HLS MP4 path currently requires H.264 or HEVC video"))?;
    let (audio_track_id, audio_meta) =
        select_mp4_hls_track(&meta.tracks, Mp4TrackKind::Audio, requested_audio_track_id)
            .ok_or_else(|| {
                requested_audio_track_id
                    .map(|track_id| {
                        anyhow!(
                            "native HLS MP4 path could not use requested audio track {track_id}"
                        )
                    })
                    .unwrap_or_else(|| {
                        anyhow!("native HLS MP4 path currently requires AAC, AC-3, or E-AC-3 audio")
                    })
            })?;
    let video_config = mp4::parse_codec_config(bytes, Some(&video_track_id))
        .ok_or_else(|| anyhow!("missing MP4 video decoder config"))?;
    let audio_config = mp4::parse_codec_config(bytes, Some(&audio_track_id))
        .ok_or_else(|| anyhow!("missing MP4 audio decoder config"))?;
    let video_plan = mp4::parse_chunk_plan(bytes, Some(&video_track_id), segment_target_ms)
        .ok_or_else(|| anyhow!("missing MP4 video chunk plan"))?;
    hls_playlist_plan_from_chunk_plan(
        video_track_id,
        audio_track_id,
        video_config
            .codec_string
            .unwrap_or_else(|| fallback_video_codec_string(video_meta.codec.as_str())),
        audio_config
            .codec_string
            .unwrap_or_else(|| fallback_audio_codec_string(audio_meta.codec.as_str())),
        video_plan,
        source_len,
        meta.duration_ms,
    )
}

fn hls_playlist_plan_from_matroska(
    bytes: &[u8],
    source_len: u64,
    requested_audio_track_id: Option<&str>,
    segment_target_ms: u64,
) -> Result<HlsVodPlaylistPlan> {
    let meta = matroska::parse_basic_metadata(bytes);
    let (video_track_id, video) =
        select_matroska_hls_track(&meta.tracks, MatroskaTrackKind::Video, None).ok_or_else(
            || anyhow!("native HLS Matroska path currently requires H.264 or HEVC video"),
        )?;
    let (audio_track_id, audio) = select_matroska_hls_track(
        &meta.tracks,
        MatroskaTrackKind::Audio,
        requested_audio_track_id,
    )
    .ok_or_else(|| {
        requested_audio_track_id
            .map(|track_id| {
                anyhow!("native HLS Matroska path could not use requested audio track {track_id}")
            })
            .unwrap_or_else(|| {
                anyhow!("native HLS Matroska path currently requires AAC, AC-3, or E-AC-3 audio")
            })
    })?;
    let video_plan = matroska::parse_chunk_plan(bytes, Some(&video_track_id), segment_target_ms)
        .ok_or_else(|| anyhow!("missing Matroska video chunk plan"))?;
    hls_playlist_plan_from_chunk_plan(
        video_track_id,
        audio_track_id,
        matroska_video_codec_string(video),
        matroska_audio_codec_string(audio),
        video_plan,
        source_len,
        meta.duration_ms,
    )
}

fn write_matroska_hls_segment(
    bytes: &[u8],
    index: usize,
    output: &Path,
    options: HlsOptions,
) -> Result<HlsSegmentInfo> {
    let segment_target_ms = options.segment_target_ms.max(500);
    let plan = hls_playlist_plan_from_matroska(
        bytes,
        bytes.len() as u64,
        options.audio_track_id.as_deref(),
        segment_target_ms,
    )?;
    write_matroska_hls_segment_from_plan(bytes, index, output, &plan)
}

fn write_matroska_hls_segment_from_plan(
    bytes: &[u8],
    index: usize,
    output: &Path,
    plan: &HlsVodPlaylistPlan,
) -> Result<HlsSegmentInfo> {
    let window = plan
        .windows
        .get(index)
        .copied()
        .ok_or_else(|| anyhow!("HLS segment index {index} is out of range"))?;
    let tracks = matroska_hls_tracks_for_window(bytes, plan, window)?;
    let segment = mux_segment(bytes, &tracks, window)?;
    if let Some(parent) = output.parent() {
        create_dir_all(parent)?;
    }
    publish_bytes(output, &segment)?;
    Ok(HlsSegmentInfo {
        index: window.index,
        start_ms: window.start_ms,
        duration_ms: window.end_ms.saturating_sub(window.start_ms).max(1),
        uri: segment_name(window.index),
    })
}

fn write_matroska_hls_fmp4_vod(
    bytes: &[u8],
    output_dir: &Path,
    options: HlsOptions,
) -> Result<HlsOutput> {
    let segment_target_ms = options.segment_target_ms.max(500);
    let plan = hls_playlist_plan_from_matroska(
        bytes,
        bytes.len() as u64,
        options.audio_track_id.as_deref(),
        segment_target_ms,
    )?;

    create_dir_all(output_dir)?;
    let variant_dir = output_dir.join("0");
    create_dir_all(&variant_dir)?;

    let init_path = variant_dir.join("init.mp4");
    write_matroska_hls_fmp4_init_from_plan(bytes, &init_path, &plan)?;
    for index in 0..plan.windows.len() {
        write_matroska_hls_fmp4_segment_from_plan(
            bytes,
            index,
            &variant_dir.join(fmp4_segment_name(index)),
            &plan,
        )?;
    }

    let media_playlist = variant_dir.join("playlist.m3u8");
    publish_bytes(&media_playlist, plan.fmp4_media_playlist().as_bytes())?;
    let master_playlist = output_dir.join("master.m3u8");
    publish_bytes(&master_playlist, plan.master_playlist().as_bytes())?;

    Ok(HlsOutput {
        master_playlist,
        media_playlist,
        init_segment: Some(init_path),
        segment_count: plan.windows.len(),
        target_duration_seconds: plan.target_duration_seconds,
        video_track_id: plan.video_track_id().to_string(),
        audio_track_id: plan.audio_track_id().to_string(),
        bandwidth_bits_per_second: plan.bandwidth_bits_per_second,
        video_codec: plan.video_codec().to_string(),
        audio_codec: plan.audio_codec().to_string(),
    })
}

fn write_matroska_hls_fmp4_init(bytes: &[u8], output: &Path, options: HlsOptions) -> Result<()> {
    let segment_target_ms = options.segment_target_ms.max(500);
    let tracks = matroska_hls_tracks_for_first_window(
        bytes,
        options.audio_track_id.as_deref(),
        segment_target_ms,
    )
    .or_else(|_| {
        let plan = hls_playlist_plan_from_matroska(
            bytes,
            bytes.len() as u64,
            options.audio_track_id.as_deref(),
            segment_target_ms,
        )?;
        let window = plan.windows.first().copied().ok_or_else(|| {
            anyhow!("native HLS could not build keyframe-aligned segment windows")
        })?;
        matroska_hls_tracks_for_window(bytes, &plan, window)
    })?;
    if !supports_fmp4_audio(&tracks.audio.payload) {
        bail!("fMP4 HLS currently requires AAC, AC-3, or E-AC-3 audio");
    }
    if let Some(parent) = output.parent() {
        create_dir_all(parent)?;
    }
    publish_bytes(output, &fmp4_init_segment_for_tracks(&tracks)?)?;
    Ok(())
}

fn write_matroska_hls_fmp4_init_from_plan(
    bytes: &[u8],
    output: &Path,
    plan: &HlsVodPlaylistPlan,
) -> Result<()> {
    let window =
        plan.windows.first().copied().ok_or_else(|| {
            anyhow!("native HLS could not build keyframe-aligned segment windows")
        })?;
    let tracks = matroska_hls_tracks_for_window(bytes, plan, window)?;
    if !supports_fmp4_audio(&tracks.audio.payload) {
        bail!("fMP4 HLS currently requires AAC, AC-3, or E-AC-3 audio");
    }
    if let Some(parent) = output.parent() {
        create_dir_all(parent)?;
    }
    publish_bytes(output, &fmp4_init_segment_for_tracks(&tracks)?)?;
    Ok(())
}

fn write_matroska_hls_fmp4_segment(
    bytes: &[u8],
    index: usize,
    output: &Path,
    options: HlsOptions,
) -> Result<HlsSegmentInfo> {
    let segment_target_ms = options.segment_target_ms.max(500);
    let plan = hls_playlist_plan_from_matroska(
        bytes,
        bytes.len() as u64,
        options.audio_track_id.as_deref(),
        segment_target_ms,
    )?;
    write_matroska_hls_fmp4_segment_from_plan(bytes, index, output, &plan)
}

fn write_matroska_hls_fmp4_segments(
    bytes: &[u8],
    output_dir: &Path,
    start_index: usize,
    count: usize,
    options: HlsOptions,
) -> Result<Vec<HlsSegmentInfo>> {
    let segment_target_ms = options.segment_target_ms.max(500);
    let plan = hls_playlist_plan_from_matroska(
        bytes,
        bytes.len() as u64,
        options.audio_track_id.as_deref(),
        segment_target_ms,
    )?;
    if start_index >= plan.windows.len() {
        bail!("HLS segment start index {start_index} is out of range");
    }
    create_dir_all(output_dir)?;
    let end_index = start_index.saturating_add(count).min(plan.windows.len());
    let mut written = Vec::with_capacity(end_index.saturating_sub(start_index));
    for index in start_index..end_index {
        written.push(write_matroska_hls_fmp4_segment_from_plan(
            bytes,
            index,
            &output_dir.join(fmp4_segment_name(index)),
            &plan,
        )?);
    }
    Ok(written)
}

fn write_matroska_hls_fmp4_segment_from_plan(
    bytes: &[u8],
    index: usize,
    output: &Path,
    plan: &HlsVodPlaylistPlan,
) -> Result<HlsSegmentInfo> {
    let window = plan
        .windows
        .get(index)
        .copied()
        .ok_or_else(|| anyhow!("HLS segment index {index} is out of range"))?;
    let tracks = matroska_hls_tracks_for_window(bytes, plan, window)?;
    if !supports_fmp4_audio(&tracks.audio.payload) {
        bail!("fMP4 HLS currently requires AAC, AC-3, or E-AC-3 audio");
    }
    let segment = mux_fmp4_segment(bytes, &tracks, window)?;
    if let Some(parent) = output.parent() {
        create_dir_all(parent)?;
    }
    publish_bytes(output, &segment)?;
    Ok(HlsSegmentInfo {
        index: window.index,
        start_ms: window.start_ms,
        duration_ms: window.end_ms.saturating_sub(window.start_ms).max(1),
        uri: fmp4_segment_name(window.index),
    })
}

fn matroska_hls_tracks_for_window(
    bytes: &[u8],
    plan: &HlsVodPlaylistPlan,
    window: SegmentWindow,
) -> Result<HlsTrackSet> {
    let meta = matroska::parse_basic_metadata(bytes);
    let (_, video) = select_matroska_hls_track(&meta.tracks, MatroskaTrackKind::Video, None)
        .ok_or_else(|| {
            anyhow!("native HLS Matroska path currently requires H.264 or HEVC video")
        })?;
    let (_, audio) = select_matroska_hls_track(
        &meta.tracks,
        MatroskaTrackKind::Audio,
        Some(plan.audio_track_id()),
    )
    .ok_or_else(|| {
        anyhow!(
            "native HLS Matroska path could not use requested audio track {}",
            plan.audio_track_id()
        )
    })?;
    let tracks = matroska::parse_packet_tracks_in_time_window(
        bytes,
        &[plan.video_track_id(), plan.audio_track_id()],
        window.start_ms,
        window.end_ms,
    )
    .ok_or_else(|| anyhow!("missing Matroska packets for HLS segment window"))?;
    let video_packets = tracks
        .iter()
        .find(|track| track.id == plan.video_track_id())
        .map(|track| track.packets.clone())
        .ok_or_else(|| anyhow!("missing Matroska video packets for HLS segment window"))?;
    let audio_packets = tracks
        .iter()
        .find(|track| track.id == plan.audio_track_id())
        .map(|track| track.packets.clone())
        .ok_or_else(|| anyhow!("missing Matroska audio packets for HLS segment window"))?;
    let audio_fmp4_sample_entry = matroska_audio_fmp4_sample_entry(bytes, audio, &audio_packets)?;

    Ok(HlsTrackSet {
        video: HlsTrack {
            id: plan.video_track_id().to_string(),
            codec_string: plan.video_codec().to_string(),
            timescale: video_packets
                .first()
                .map(|packet| packet.dts.scale.units_per_second)
                .unwrap_or(90_000),
            packets: video_packets,
            payload: matroska_video_payload_kind(video)?,
            fmp4_sample_entry: matroska_video_fmp4_sample_entry(video)?,
        },
        audio: HlsTrack {
            id: plan.audio_track_id().to_string(),
            codec_string: plan.audio_codec().to_string(),
            timescale: audio_packets
                .first()
                .map(|packet| packet.dts.scale.units_per_second)
                .unwrap_or(48_000),
            packets: audio_packets,
            payload: matroska_audio_payload_kind(audio)?,
            fmp4_sample_entry: audio_fmp4_sample_entry,
        },
    })
}

fn matroska_hls_tracks_for_first_window(
    bytes: &[u8],
    requested_audio_track_id: Option<&str>,
    segment_target_ms: u64,
) -> Result<HlsTrackSet> {
    matroska_hls_tracks_for_selected_window(
        bytes,
        requested_audio_track_id,
        SegmentWindow {
            index: 0,
            start_ms: 0,
            end_ms: segment_target_ms.max(MIN_SEGMENT_MS).max(1),
        },
    )
}

fn matroska_hls_tracks_for_selected_window(
    bytes: &[u8],
    requested_audio_track_id: Option<&str>,
    window: SegmentWindow,
) -> Result<HlsTrackSet> {
    let meta = matroska::parse_basic_metadata(bytes);
    let (video_track_id, video) =
        select_matroska_hls_track(&meta.tracks, MatroskaTrackKind::Video, None).ok_or_else(
            || anyhow!("native HLS Matroska path currently requires H.264 or HEVC video"),
        )?;
    let (audio_track_id, audio) = select_matroska_hls_track(
        &meta.tracks,
        MatroskaTrackKind::Audio,
        requested_audio_track_id,
    )
    .ok_or_else(|| {
        requested_audio_track_id
            .map(|track_id| {
                anyhow!("native HLS Matroska path could not use requested audio track {track_id}")
            })
            .unwrap_or_else(|| {
                anyhow!("native HLS Matroska path currently requires AAC, AC-3, or E-AC-3 audio")
            })
    })?;
    let tracks = matroska::parse_packet_tracks_in_time_window(
        bytes,
        &[video_track_id.as_str(), audio_track_id.as_str()],
        window.start_ms,
        window.end_ms,
    )
    .ok_or_else(|| anyhow!("missing Matroska packets for HLS init window"))?;
    let video_packets = tracks
        .iter()
        .find(|track| track.id == video_track_id)
        .map(|track| track.packets.clone())
        .filter(|packets| !packets.is_empty())
        .ok_or_else(|| anyhow!("missing Matroska video packets for HLS init window"))?;
    let audio_packets = tracks
        .iter()
        .find(|track| track.id == audio_track_id)
        .map(|track| track.packets.clone())
        .filter(|packets| !packets.is_empty())
        .ok_or_else(|| anyhow!("missing Matroska audio packets for HLS init window"))?;
    let audio_fmp4_sample_entry = matroska_audio_fmp4_sample_entry(bytes, audio, &audio_packets)?;

    Ok(HlsTrackSet {
        video: HlsTrack {
            id: video_track_id.clone(),
            codec_string: matroska_video_codec_string(video),
            timescale: video_packets
                .first()
                .map(|packet| packet.dts.scale.units_per_second)
                .unwrap_or(90_000),
            packets: video_packets,
            payload: matroska_video_payload_kind(video)?,
            fmp4_sample_entry: matroska_video_fmp4_sample_entry(video)?,
        },
        audio: HlsTrack {
            id: audio_track_id.clone(),
            codec_string: matroska_audio_codec_string(audio),
            timescale: audio_packets
                .first()
                .map(|packet| packet.dts.scale.units_per_second)
                .unwrap_or(48_000),
            packets: audio_packets,
            payload: matroska_audio_payload_kind(audio)?,
            fmp4_sample_entry: audio_fmp4_sample_entry,
        },
    })
}

fn hls_playlist_plan_from_chunk_plan(
    video_track_id: String,
    audio_track_id: String,
    video_codec: String,
    audio_codec: String,
    video_plan: ChunkPlan,
    source_len: u64,
    duration_ms: Option<u64>,
) -> Result<HlsVodPlaylistPlan> {
    let mut windows = chunk_plan_windows(&video_plan);
    if windows.is_empty() {
        bail!("native HLS could not build keyframe-aligned segment windows");
    }
    if let Some(duration_ms) = duration_ms
        && let Some(last) = windows.last_mut()
    {
        last.end_ms = last
            .end_ms
            .max(duration_ms)
            .max(last.start_ms.saturating_add(1));
    }
    let target_duration_seconds = target_duration_seconds_for_windows(&windows);
    let bandwidth_bits_per_second =
        estimate_hls_bandwidth_from_source_size(source_len, duration_ms);
    Ok(HlsVodPlaylistPlan {
        video_track_id,
        audio_track_id,
        video_codec,
        audio_codec,
        windows,
        target_duration_seconds,
        bandwidth_bits_per_second,
    })
}

fn chunk_plan_windows(plan: &ChunkPlan) -> Vec<SegmentWindow> {
    let mut windows = plan
        .chunks
        .iter()
        .map(|chunk| {
            let start_ms = chunk.start.as_millis();
            let end_ms = start_ms
                .saturating_add(chunk.duration.as_millis())
                .max(start_ms.saturating_add(1));
            SegmentWindow {
                index: chunk.index as usize,
                start_ms,
                end_ms,
            }
        })
        .collect::<Vec<_>>();
    for index in 0..windows.len().saturating_sub(1) {
        let next_start_ms = windows[index + 1].start_ms;
        windows[index].end_ms = next_start_ms.max(windows[index].start_ms.saturating_add(1));
    }
    collapse_short_windows(windows, MIN_SEGMENT_MS)
}

fn target_duration_seconds_for_windows(windows: &[SegmentWindow]) -> u64 {
    windows
        .iter()
        .map(|window| {
            window
                .end_ms
                .saturating_sub(window.start_ms)
                .max(1)
                .saturating_add(999)
                / 1000
        })
        .max()
        .unwrap_or(1)
        .max(1)
}

fn estimate_hls_bandwidth_from_source_size(source_len: u64, duration_ms: Option<u64>) -> u64 {
    let Some(duration_ms) = duration_ms.filter(|duration| *duration > 0) else {
        return 12_000_000;
    };
    let average = source_len.saturating_mul(8).saturating_mul(1_000) / duration_ms;
    average.saturating_add(average / 4).max(128_000)
}

fn segment_windows(video_packets: &[PacketRef], target_ms: u64) -> Vec<SegmentWindow> {
    let mut windows = Vec::new();
    let mut start_ms = video_packets[0].pts.as_millis();
    for packet in video_packets.iter().skip(1) {
        let packet_ms = packet.pts.as_millis();
        if packet.keyframe && packet_ms.saturating_sub(start_ms) >= target_ms {
            windows.push(SegmentWindow {
                index: windows.len(),
                start_ms,
                end_ms: packet_ms,
            });
            start_ms = packet_ms;
        }
    }
    let end_ms = video_packets
        .last()
        .map(packet_end_ms)
        .unwrap_or(start_ms)
        .max(start_ms.saturating_add(1));
    windows.push(SegmentWindow {
        index: windows.len(),
        start_ms,
        end_ms,
    });
    collapse_short_windows(windows, MIN_SEGMENT_MS)
}

fn collapse_short_windows(windows: Vec<SegmentWindow>, min_duration_ms: u64) -> Vec<SegmentWindow> {
    if windows.len() <= 1 || min_duration_ms == 0 {
        return windows;
    }
    let mut out: Vec<SegmentWindow> = Vec::with_capacity(windows.len());
    for window in windows {
        let duration = window.end_ms.saturating_sub(window.start_ms);
        if duration < min_duration_ms
            && let Some(last) = out.last_mut()
        {
            last.end_ms = window.end_ms;
            continue;
        }
        out.push(window);
    }
    if out.len() > 1 && out[0].end_ms.saturating_sub(out[0].start_ms) < min_duration_ms {
        let first = out.remove(0);
        out[0].start_ms = first.start_ms;
    }
    for (index, window) in out.iter_mut().enumerate() {
        window.index = index;
    }
    out
}

fn packet_end_ms(packet: &PacketRef) -> u64 {
    packet
        .pts
        .as_millis()
        .saturating_add(packet.duration.as_millis())
}

fn estimate_hls_bandwidth_bits_per_second(tracks: &HlsTrackSet, windows: &[SegmentWindow]) -> u64 {
    let video_bytes = track_bytes_by_window(&tracks.video, windows);
    let audio_bytes = track_bytes_by_window(&tracks.audio, windows);
    let max_bits_per_second = windows
        .iter()
        .enumerate()
        .map(|(index, window)| {
            let duration_ms = window.end_ms.saturating_sub(window.start_ms).max(1);
            let bytes = video_bytes
                .get(index)
                .copied()
                .unwrap_or(0)
                .saturating_add(audio_bytes.get(index).copied().unwrap_or(0));
            bytes.saturating_mul(8).saturating_mul(1_000) / duration_ms
        })
        .max()
        .unwrap_or(0);

    max_bits_per_second
        .saturating_add(max_bits_per_second / 10)
        .max(128_000)
}

fn track_bytes_by_window(track: &HlsTrack, windows: &[SegmentWindow]) -> Vec<u64> {
    let mut out = vec![0_u64; windows.len()];
    if windows.is_empty() {
        return out;
    }

    let mut window_index = 0_usize;
    for packet in &track.packets {
        let pts = packet.pts.as_millis();
        while window_index < windows.len() && pts >= windows[window_index].end_ms {
            window_index += 1;
        }
        if window_index >= windows.len() {
            break;
        }
        let window = &windows[window_index];
        if pts >= window.start_ms {
            out[window_index] = out[window_index].saturating_add(u64::from(packet.size));
        }
    }
    out
}

fn mux_segment(bytes: &[u8], tracks: &HlsTrackSet, window: SegmentWindow) -> Result<Vec<u8>> {
    let mut mux = TsMuxer::new(
        ts_stream_type(&tracks.video.payload),
        ts_stream_type(&tracks.audio.payload),
    );
    mux.write_pat_pmt();

    let mut samples = Vec::new();
    for packet in &tracks.video.packets {
        let pts = packet.pts.as_millis();
        if pts < window.start_ms || pts >= window.end_ms {
            continue;
        }
        samples.push((
            packet.dts.as_millis(),
            true,
            packet.pts.scale.to_90khz(packet.pts.units),
            packet.dts.scale.to_90khz(packet.dts.units),
            packet_to_payload(bytes, packet, &tracks.video.payload)?,
        ));
    }
    for packet in &tracks.audio.packets {
        let pts = packet.pts.as_millis();
        if pts < window.start_ms || pts >= window.end_ms {
            continue;
        }
        samples.push((
            packet.dts.as_millis(),
            false,
            packet.pts.scale.to_90khz(packet.pts.units),
            packet.dts.scale.to_90khz(packet.dts.units),
            packet_to_payload(bytes, packet, &tracks.audio.payload)?,
        ));
    }
    samples.sort_by_key(|sample| (sample.0, !sample.1));

    let mut timestamps = OutputTimestampSanitizer::default();
    for (_, is_video, pts90, dts90, payload) in samples {
        let (pts90, dts90) = timestamps.sanitize(is_video, pts90, dts90);
        let timed = TimedPayload {
            pts90,
            dts90,
            bytes: payload,
        };
        if is_video {
            mux.write_pes(VIDEO_PID, VIDEO_STREAM_ID, &timed, true);
        } else {
            mux.write_pes(
                AUDIO_PID,
                audio_stream_id(&tracks.audio.payload),
                &timed,
                false,
            );
        }
    }

    Ok(mux.into_bytes())
}

fn mux_fmp4_segment(bytes: &[u8], tracks: &HlsTrackSet, window: SegmentWindow) -> Result<Vec<u8>> {
    let video_packets = packets_in_window(&tracks.video.packets, window);
    let audio_packets = packets_in_window(&tracks.audio.packets, window);
    if video_packets.is_empty() {
        bail!("fMP4 segment {} contains no video samples", window.index);
    }
    if audio_packets.is_empty() {
        bail!("fMP4 segment {} contains no audio samples", window.index);
    }

    let video_payload = raw_packet_payload(bytes, &video_packets)?;
    let audio_payload = raw_packet_payload(bytes, &audio_packets)?;
    Ok(media_fragment(
        window.index as u32 + 1,
        &[
            Fmp4FragmentTrack {
                track_id: 1,
                base_decode_time: decode_time_for_timescale(
                    &video_packets[0],
                    tracks.video.timescale,
                ),
                samples: samples_from_packets_with_timescale(
                    &video_packets,
                    tracks.video.timescale,
                ),
                payload: video_payload,
            },
            Fmp4FragmentTrack {
                track_id: 2,
                base_decode_time: decode_time_for_timescale(
                    &audio_packets[0],
                    tracks.audio.timescale,
                ),
                samples: samples_from_packets_with_timescale(
                    &audio_packets,
                    tracks.audio.timescale,
                ),
                payload: audio_payload,
            },
        ],
    )?)
}

fn fmp4_init_segment_for_tracks(tracks: &HlsTrackSet) -> Result<Vec<u8>> {
    let video_entry = tracks
        .video
        .fmp4_sample_entry
        .clone()
        .ok_or_else(|| anyhow!("fMP4 HLS requires video sample-entry metadata"))?;
    let audio_entry = tracks
        .audio
        .fmp4_sample_entry
        .clone()
        .ok_or_else(|| anyhow!("fMP4 HLS requires audio sample-entry metadata"))?;
    Ok(init_segment(&[
        Fmp4Track {
            id: 1,
            kind: Fmp4TrackKind::Video,
            timescale: tracks.video.timescale,
            default_sample_duration: default_sample_duration(&tracks.video.packets),
            default_sample_size: 0,
            default_sample_flags: 0x0101_0000,
            sample_entry: video_entry,
        },
        Fmp4Track {
            id: 2,
            kind: Fmp4TrackKind::Audio,
            timescale: tracks.audio.timescale,
            default_sample_duration: default_sample_duration(&tracks.audio.packets),
            default_sample_size: 0,
            default_sample_flags: 0x0200_0000,
            sample_entry: audio_entry,
        },
    ])?)
}

fn packets_in_window(packets: &[PacketRef], window: SegmentWindow) -> Vec<PacketRef> {
    packets
        .iter()
        .filter(|packet| {
            let pts = packet.pts.as_millis();
            pts >= window.start_ms && pts < window.end_ms
        })
        .cloned()
        .collect()
}

fn raw_packet_payload(bytes: &[u8], packets: &[PacketRef]) -> Result<Vec<u8>> {
    let byte_count = packets
        .iter()
        .map(|packet| u64::from(packet.size))
        .sum::<u64>();
    let mut out = Vec::with_capacity(usize::try_from(byte_count).unwrap_or(bytes.len()));
    for packet in packets {
        out.extend_from_slice(packet_bytes(bytes, packet)?);
    }
    Ok(out)
}

fn packet_bytes<'a>(bytes: &'a [u8], packet: &PacketRef) -> Result<&'a [u8]> {
    let start = packet.source_offset as usize;
    let end = start
        .checked_add(packet.size as usize)
        .ok_or_else(|| anyhow!("packet range overflows"))?;
    if end > bytes.len() {
        bail!("packet range is outside source");
    }
    Ok(&bytes[start..end])
}

fn fallback_video_codec_string(codec: &str) -> String {
    match codec {
        "hevc" => "hev1.1.6.L120".to_string(),
        _ => "avc1.640028".to_string(),
    }
}

fn fallback_audio_codec_string(codec: &str) -> String {
    match codec {
        "ac3" => "ac-3".to_string(),
        "eac3" => "ec-3".to_string(),
        _ => "mp4a.40.2".to_string(),
    }
}

fn clamped_u16(value: u32) -> u16 {
    value.min(u32::from(u16::MAX)) as u16
}

fn matroska_video_codec_string(track: &matroska::MatroskaTrack) -> String {
    match (track.codec.as_str(), track.codec_private.as_deref()) {
        ("h264", Some(config)) if config.len() >= 4 => {
            format!("avc1.{:02X}{:02X}{:02X}", config[1], config[2], config[3])
        }
        ("hevc", Some(config)) => matroska_hevc_codec_string(config)
            .unwrap_or_else(|| fallback_video_codec_string(track.codec.as_str())),
        _ => fallback_video_codec_string(track.codec.as_str()),
    }
}

fn matroska_audio_codec_string(track: &matroska::MatroskaTrack) -> String {
    match (track.codec.as_str(), track.codec_private.as_deref()) {
        ("aac", Some(config)) => parse_audio_specific_config(config)
            .ok()
            .map(|config| format!("mp4a.40.{}", config.object_type))
            .unwrap_or_else(|| fallback_audio_codec_string(track.codec.as_str())),
        _ => fallback_audio_codec_string(track.codec.as_str()),
    }
}

fn matroska_hevc_codec_string(config: &[u8]) -> Option<String> {
    if config.len() < 13 {
        return None;
    }
    let profile_space = match (config[1] >> 6) & 0x03 {
        0 => "",
        1 => "A",
        2 => "B",
        3 => "C",
        _ => "",
    };
    let tier = if config[1] & 0x20 != 0 { "H" } else { "L" };
    let profile_idc = config[1] & 0x1f;
    let compatibility = u32::from_be_bytes(config[2..6].try_into().ok()?);
    let level_idc = config[12];
    Some(format!(
        "hev1.{profile_space}{profile_idc}.{:X}.{tier}{level_idc}",
        compatibility.reverse_bits()
    ))
}

fn default_sample_duration(packets: &[PacketRef]) -> u32 {
    packets
        .first()
        .map(|packet| packet.duration.units.min(u64::from(u32::MAX)).max(1) as u32)
        .unwrap_or(1)
}

fn hex_to_bytes(hex: &str) -> Result<Vec<u8>> {
    let clean = hex.trim();
    if !clean.len().is_multiple_of(2) {
        bail!("hex string has odd length");
    }
    let mut out = Vec::with_capacity(clean.len() / 2);
    let bytes = clean.as_bytes();
    for pair in bytes.chunks_exact(2) {
        let hi = hex_digit(pair[0])?;
        let lo = hex_digit(pair[1])?;
        out.push((hi << 4) | lo);
    }
    Ok(out)
}

fn hex_digit(byte: u8) -> Result<u8> {
    match byte {
        b'0'..=b'9' => Ok(byte - b'0'),
        b'a'..=b'f' => Ok(byte - b'a' + 10),
        b'A'..=b'F' => Ok(byte - b'A' + 10),
        _ => bail!("invalid hex digit"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn hls_errors_expose_stable_codes() {
        assert_eq!(
            HlsError::Unsupported("unsupported".to_string()).code(),
            EngineErrorCode::HlsUnsupported
        );
    }

    #[test]
    fn multi_audio_hls_plan_uses_one_shared_video_playlist() {
        let plan = plan_multi_audio_hls_outputs(
            "v0",
            "avc1.640028",
            8_000_000,
            &[
                HlsAudioRenditionInput {
                    track_id: "a0".to_string(),
                    codec: "mp4a.40.2".to_string(),
                    language: Some("eng".to_string()),
                    name: Some("English 5.1".to_string()),
                    default: true,
                },
                HlsAudioRenditionInput {
                    track_id: "director/commentary".to_string(),
                    codec: "ac-3".to_string(),
                    language: Some("eng".to_string()),
                    name: Some("Commentary".to_string()),
                    default: false,
                },
            ],
        );

        assert_eq!(plan.video_track_id, "v0");
        assert_eq!(plan.video_playlist_uri, "video/playlist.m3u8");
        assert!(!plan.duplicates_video_work);
        assert_eq!(plan.audio_renditions.len(), 2);
        assert_eq!(
            plan.audio_renditions[1].playlist_uri,
            "audio/director_commentary/playlist.m3u8"
        );
        assert!(plan.master_playlist.contains("#EXT-X-MEDIA:TYPE=AUDIO"));
        assert!(plan.master_playlist.contains("AUDIO=\"audio\""));
        assert!(plan.master_playlist.contains("video/playlist.m3u8"));
        assert!(
            plan.master_playlist
                .contains("CODECS=\"avc1.640028,mp4a.40.2,ac-3\"")
        );
    }

    #[test]
    fn matroska_hevc_codec_string_reverses_compatibility_flags() {
        let mut config = vec![0_u8; 23];
        config[1] = 0x22;
        config[2..6].copy_from_slice(&0x2000_0000_u32.to_be_bytes());
        config[12] = 153;

        assert_eq!(
            matroska_hevc_codec_string(&config).as_deref(),
            Some("hev1.2.4.H153")
        );
    }

    fn test_plan_with_one_window() -> HlsVodPlan {
        let file = tempfile::NamedTempFile::new().expect("tempfile");
        std::fs::write(file.path(), [0_u8]).expect("seed temp file");
        let source = MappedMediaFile::open(file.path()).expect("map temp file");
        let packet = PacketRef {
            source_offset: 0,
            size: 0,
            pts: crate::packet::TimePoint {
                units: 0,
                scale: crate::packet::TimeScale {
                    units_per_second: 1_000,
                },
            },
            dts: crate::packet::TimePoint {
                units: 0,
                scale: crate::packet::TimeScale {
                    units_per_second: 1_000,
                },
            },
            duration: crate::packet::TimeDelta {
                units: 1_000,
                scale: crate::packet::TimeScale {
                    units_per_second: 1_000,
                },
            },
            keyframe: true,
        };

        HlsVodPlan {
            source,
            tracks: HlsTrackSet {
                video: HlsTrack {
                    id: "v0".to_string(),
                    codec_string: "avc1.640028".to_string(),
                    timescale: 1_000,
                    packets: vec![packet],
                    payload: PayloadKind::Avc {
                        nalu_length_size: 4,
                        parameter_sets: AvcParameterSets {
                            nalu_length_size: 4,
                            sps: Vec::new(),
                            pps: Vec::new(),
                        },
                    },
                    fmp4_sample_entry: None,
                },
                audio: HlsTrack {
                    id: "a0".to_string(),
                    codec_string: "mp4a.40.2".to_string(),
                    timescale: 48_000,
                    packets: Vec::new(),
                    payload: PayloadKind::Aac {
                        config: AacAudioSpecificConfig {
                            object_type: 2,
                            sample_rate: 48_000,
                            channel_config: 2,
                        },
                    },
                    fmp4_sample_entry: None,
                },
            },
            windows: vec![SegmentWindow {
                index: 0,
                start_ms: 0,
                end_ms: 1_000,
            }],
            target_duration_seconds: 1,
            bandwidth_bits_per_second: 128_000,
        }
    }

    #[test]
    fn writes_pat_and_pmt_packets() {
        let mut mux = TsMuxer::new(0x1b, 0x0f);
        mux.write_pat_pmt();
        let out = mux.into_bytes();
        assert_eq!(out.len(), 376);
        assert_eq!(out[0], 0x47);
        assert_eq!(out[188], 0x47);
    }

    #[test]
    fn pmt_signals_dolby_audio_with_registration_descriptors() {
        let aac = pmt_section(0x1b, 0x0f);
        assert!(!aac.windows(4).any(|window| window == b"AC-3"));

        let ac3 = pmt_section(0x1b, 0x81);
        assert!(ac3.windows(4).any(|window| window == b"AC-3"));
        assert!(!ac3.windows(4).any(|window| window == b"EAC3"));

        let eac3 = pmt_section(0x24, 0x87);
        assert!(eac3.windows(4).any(|window| window == b"AC-3"));
        assert!(eac3.windows(4).any(|window| window == b"EAC3"));
    }

    #[test]
    fn video_pes_packets_carry_pcr_from_dts() {
        let mut mux = TsMuxer::new(0x1b, 0x0f);
        mux.write_pes(
            VIDEO_PID,
            VIDEO_STREAM_ID,
            &TimedPayload {
                pts90: 180_000,
                dts90: 90_000,
                bytes: vec![0, 0, 1, 9, 0x10],
            },
            true,
        );

        let out = mux.into_bytes();
        assert_eq!(out.len(), 188);
        assert_eq!(out[0], 0x47);
        assert_eq!(out[1] & 0x40, 0x40);
        assert_eq!(out[3] & 0x20, 0x20);
        assert_eq!(out[5] & 0x10, 0x10);
        assert_eq!(read_pcr_base(&out[6..12]), 90_000);
    }

    #[test]
    fn ts_continuity_counter_increments_across_large_pes_payloads() {
        let mut mux = TsMuxer::new(0x1b, 0x0f);
        mux.write_pes(
            VIDEO_PID,
            VIDEO_STREAM_ID,
            &TimedPayload {
                pts90: 90_000,
                dts90: 90_000,
                bytes: vec![0xaa; 600],
            },
            true,
        );

        let out = mux.into_bytes();
        let packets = ts_packets(&out);
        assert!(packets.len() > 3);
        for (index, packet) in packets.iter().enumerate() {
            assert_eq!(packet[0], 0x47);
            assert_eq!(ts_pid(packet), VIDEO_PID);
            assert_eq!(packet[3] & 0x0f, (index as u8) & 0x0f);
        }
    }

    #[test]
    fn audio_pes_packets_do_not_carry_pcr() {
        let mut mux = TsMuxer::new(0x1b, 0x0f);
        mux.write_pes(
            AUDIO_PID,
            AUDIO_STREAM_ID,
            &TimedPayload {
                pts90: 90_000,
                dts90: 90_000,
                bytes: vec![0xbb; 256],
            },
            false,
        );

        let out = mux.into_bytes();
        let packets = ts_packets(&out);
        assert!(packets.len() >= 2);
        assert_eq!(ts_pid(packets[0]), AUDIO_PID);
        assert_eq!(packets[0][1] & 0x40, 0x40);
        assert_eq!(packets[0][5] & 0x10, 0x00);
        assert_eq!(packets[1][1] & 0x40, 0x00);
    }

    fn ts_packets(bytes: &[u8]) -> Vec<&[u8]> {
        assert_eq!(bytes.len() % 188, 0);
        bytes.chunks_exact(188).collect()
    }

    fn top_level_boxes(bytes: &[u8]) -> Vec<[u8; 4]> {
        let mut out = Vec::new();
        let mut offset = 0;
        while offset + 8 <= bytes.len() {
            let size = u32::from_be_bytes(bytes[offset..offset + 4].try_into().unwrap()) as usize;
            assert!(size >= 8);
            out.push(bytes[offset + 4..offset + 8].try_into().unwrap());
            offset += size;
        }
        out
    }

    fn ts_pid(packet: &[u8]) -> u16 {
        (u16::from(packet[1] & 0x1f) << 8) | u16::from(packet[2])
    }

    fn read_pcr_base(bytes: &[u8]) -> u64 {
        (u64::from(bytes[0]) << 25)
            | (u64::from(bytes[1]) << 17)
            | (u64::from(bytes[2]) << 9)
            | (u64::from(bytes[3]) << 1)
            | (u64::from(bytes[4]) >> 7)
    }

    #[test]
    fn renders_media_playlist() {
        let body = media_playlist_body(4, &[1500, 4010]);
        assert!(body.contains("#EXT-X-TARGETDURATION:4"));
        assert!(body.contains("#EXTINF:1.500,"));
        assert!(body.contains("seg-00001.ts"));
        assert!(body.ends_with("#EXT-X-ENDLIST\n"));
    }

    #[test]
    fn renders_fmp4_media_playlist_with_init_map() {
        let body = fmp4_media_playlist_body(4, &[1500, 4010]);
        assert!(body.contains("#EXT-X-VERSION:7"));
        assert!(body.contains("#EXT-X-MAP:URI=\"init.mp4\""));
        assert!(body.contains("#EXTINF:4.010,"));
        assert!(body.contains("seg-00001.m4s"));
        assert!(body.ends_with("#EXT-X-ENDLIST\n"));
    }

    #[test]
    fn muxes_fmp4_segment_from_raw_mp4_samples() {
        let scale = crate::packet::TimeScale {
            units_per_second: 1_000,
        };
        let packet = |source_offset: u64, pts_ms: u64, size: u32, keyframe: bool| PacketRef {
            source_offset,
            size,
            pts: crate::packet::TimePoint {
                units: pts_ms,
                scale,
            },
            dts: crate::packet::TimePoint {
                units: pts_ms,
                scale,
            },
            duration: crate::packet::TimeDelta { units: 40, scale },
            keyframe,
        };
        let bytes = b"vvvvvaaaaa";
        let tracks = HlsTrackSet {
            video: HlsTrack {
                id: "v0".to_string(),
                codec_string: "avc1.640028".to_string(),
                timescale: 1_000,
                packets: vec![packet(0, 0, 5, true)],
                payload: PayloadKind::Avc {
                    nalu_length_size: 4,
                    parameter_sets: AvcParameterSets {
                        nalu_length_size: 4,
                        sps: Vec::new(),
                        pps: Vec::new(),
                    },
                },
                fmp4_sample_entry: Some(Fmp4SampleEntry::Avc {
                    codec_config: vec![1, 100, 0, 40, 0xff, 0xe1, 0, 0],
                    width: 1_920,
                    height: 1_080,
                }),
            },
            audio: HlsTrack {
                id: "a0".to_string(),
                codec_string: "mp4a.40.2".to_string(),
                timescale: 1_000,
                packets: vec![packet(5, 0, 5, true)],
                payload: PayloadKind::Aac {
                    config: AacAudioSpecificConfig {
                        object_type: 2,
                        sample_rate: 48_000,
                        channel_config: 2,
                    },
                },
                fmp4_sample_entry: Some(Fmp4SampleEntry::Aac {
                    decoder_config: vec![0x11, 0x90],
                    channel_count: 2,
                    sample_rate: 48_000,
                }),
            },
        };

        let segment = mux_fmp4_segment(
            bytes,
            &tracks,
            SegmentWindow {
                index: 0,
                start_ms: 0,
                end_ms: 1_000,
            },
        )
        .expect("fMP4 segment");

        assert_eq!(top_level_boxes(&segment), vec![*b"moof", *b"mdat"]);
        assert!(segment.ends_with(b"vvvvvaaaaa"));
    }

    #[test]
    fn estimates_hls_bandwidth_from_peak_segment_packet_bytes() {
        let scale = crate::packet::TimeScale {
            units_per_second: 1_000,
        };
        let packet = |pts_ms: u64, size: u32| PacketRef {
            source_offset: 0,
            size,
            pts: crate::packet::TimePoint {
                units: pts_ms,
                scale,
            },
            dts: crate::packet::TimePoint {
                units: pts_ms,
                scale,
            },
            duration: crate::packet::TimeDelta { units: 1, scale },
            keyframe: true,
        };
        let tracks = HlsTrackSet {
            video: HlsTrack {
                id: "v0".to_string(),
                codec_string: "avc1.640028".to_string(),
                timescale: 1_000,
                packets: vec![packet(0, 20_000), packet(1_000, 40_000)],
                payload: PayloadKind::Avc {
                    nalu_length_size: 4,
                    parameter_sets: AvcParameterSets {
                        nalu_length_size: 4,
                        sps: Vec::new(),
                        pps: Vec::new(),
                    },
                },
                fmp4_sample_entry: None,
            },
            audio: HlsTrack {
                id: "a0".to_string(),
                codec_string: "mp4a.40.2".to_string(),
                timescale: 1_000,
                packets: vec![packet(0, 5_000), packet(1_000, 5_000)],
                payload: PayloadKind::Aac {
                    config: AacAudioSpecificConfig {
                        object_type: 2,
                        sample_rate: 48_000,
                        channel_config: 2,
                    },
                },
                fmp4_sample_entry: None,
            },
        };
        let windows = vec![
            SegmentWindow {
                index: 0,
                start_ms: 0,
                end_ms: 1_000,
            },
            SegmentWindow {
                index: 1,
                start_ms: 1_000,
                end_ms: 2_000,
            },
        ];

        assert_eq!(
            estimate_hls_bandwidth_bits_per_second(&tracks, &windows),
            396_000
        );
        assert!(
            master_playlist_body("avc1.640028", "mp4a.40.2", 396_000).contains("BANDWIDTH=396000")
        );
    }

    #[test]
    fn write_segments_handles_empty_and_out_of_range_requests() {
        let plan = test_plan_with_one_window();
        let out = tempfile::tempdir().expect("tempdir");
        let empty = plan
            .write_segments(0, 0, out.path())
            .expect("zero count should be accepted");
        assert!(empty.is_empty());
        assert!(!out.path().join("seg-00000.ts").exists());

        let err = plan
            .write_segments(1, 1, out.path())
            .expect_err("out-of-range start should fail");
        assert!(err.to_string().contains("out of range"));
    }

    #[test]
    fn mp4_hls_audio_selection_uses_stable_audio_track_ids() {
        let tracks = vec![
            mp4::Mp4Track {
                index: 0,
                kind: Mp4TrackKind::Video,
                codec: "h264".to_string(),
                duration_ms: None,
                language: None,
                title: None,
                default: false,
                forced: false,
                frame_rate: None,
                bitrate_bps: None,
                dynamic_range: mp4::Mp4DynamicRange::Unknown,
                pixel_format: None,
                width: None,
                height: None,
                channels: None,
                sample_rate: None,
                atmos: false,
            },
            mp4::Mp4Track {
                index: 1,
                kind: Mp4TrackKind::Audio,
                codec: "aac".to_string(),
                duration_ms: None,
                language: None,
                title: None,
                default: true,
                forced: false,
                frame_rate: None,
                bitrate_bps: None,
                dynamic_range: mp4::Mp4DynamicRange::Unknown,
                pixel_format: None,
                width: None,
                height: None,
                channels: Some(2),
                sample_rate: Some(48_000),
                atmos: false,
            },
            mp4::Mp4Track {
                index: 2,
                kind: Mp4TrackKind::Audio,
                codec: "eac3".to_string(),
                duration_ms: None,
                language: None,
                title: None,
                default: false,
                forced: false,
                frame_rate: None,
                bitrate_bps: None,
                dynamic_range: mp4::Mp4DynamicRange::Unknown,
                pixel_format: None,
                width: None,
                height: None,
                channels: Some(6),
                sample_rate: Some(48_000),
                atmos: false,
            },
        ];

        let default_audio =
            select_mp4_hls_track(&tracks, Mp4TrackKind::Audio, None).expect("default audio");
        assert_eq!(default_audio.0, "a0");

        let requested_audio = select_mp4_hls_track(&tracks, Mp4TrackKind::Audio, Some("a1"))
            .expect("requested audio");
        assert_eq!(requested_audio.0, "a1");
        assert_eq!(requested_audio.1.codec, "eac3");

        assert!(select_mp4_hls_track(&tracks, Mp4TrackKind::Audio, Some("a9")).is_none());
    }

    #[test]
    fn matroska_hls_audio_selection_prefers_default_track() {
        let tracks = vec![
            matroska::MatroskaTrack {
                index: 0,
                number: 1,
                kind: MatroskaTrackKind::Audio,
                codec: "aac".to_string(),
                language: None,
                name: None,
                default: false,
                forced: false,
                width: None,
                height: None,
                pixel_format: None,
                channels: Some(2),
                sample_rate: Some(48_000),
                atmos: false,
                object_audio_candidate: false,
                default_duration_ns: None,
                codec_private: None,
            },
            matroska::MatroskaTrack {
                index: 1,
                number: 2,
                kind: MatroskaTrackKind::Audio,
                codec: "eac3".to_string(),
                language: None,
                name: None,
                default: true,
                forced: false,
                width: None,
                height: None,
                pixel_format: None,
                channels: Some(6),
                sample_rate: Some(48_000),
                atmos: false,
                object_audio_candidate: true,
                default_duration_ns: None,
                codec_private: None,
            },
        ];

        let default_audio = select_matroska_hls_track(&tracks, MatroskaTrackKind::Audio, None)
            .expect("default audio");
        assert_eq!(default_audio.0, "a1");
        assert_eq!(default_audio.1.codec, "eac3");

        let requested_audio =
            select_matroska_hls_track(&tracks, MatroskaTrackKind::Audio, Some("a0"))
                .expect("requested audio");
        assert_eq!(requested_audio.0, "a0");
    }

    #[test]
    fn matroska_hls_audio_selection_skips_unsupported_default_track() {
        let tracks = vec![
            matroska::MatroskaTrack {
                index: 0,
                number: 1,
                kind: MatroskaTrackKind::Audio,
                codec: "dts".to_string(),
                language: Some("eng".to_string()),
                name: None,
                default: true,
                forced: false,
                width: None,
                height: None,
                pixel_format: None,
                channels: Some(6),
                sample_rate: Some(48_000),
                atmos: false,
                object_audio_candidate: true,
                default_duration_ns: None,
                codec_private: None,
            },
            matroska::MatroskaTrack {
                index: 1,
                number: 2,
                kind: MatroskaTrackKind::Audio,
                codec: "ac3".to_string(),
                language: Some("eng".to_string()),
                name: None,
                default: false,
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
            },
        ];

        let default_audio = select_matroska_hls_track(&tracks, MatroskaTrackKind::Audio, None)
            .expect("fallback streamable audio");
        assert_eq!(default_audio.0, "a1");
        assert_eq!(default_audio.1.codec, "ac3");

        assert!(select_matroska_hls_track(&tracks, MatroskaTrackKind::Audio, Some("a0")).is_none());
    }

    #[test]
    fn collapses_pathological_short_windows() {
        let windows = vec![
            SegmentWindow {
                index: 0,
                start_ms: 0,
                end_ms: 40,
            },
            SegmentWindow {
                index: 1,
                start_ms: 40,
                end_ms: 4_000,
            },
            SegmentWindow {
                index: 2,
                start_ms: 4_000,
                end_ms: 4_120,
            },
            SegmentWindow {
                index: 3,
                start_ms: 4_120,
                end_ms: 8_500,
            },
        ];
        let collapsed = collapse_short_windows(windows, 1_000);
        assert_eq!(
            collapsed,
            vec![
                SegmentWindow {
                    index: 0,
                    start_ms: 0,
                    end_ms: 4_120,
                },
                SegmentWindow {
                    index: 1,
                    start_ms: 4_120,
                    end_ms: 8_500,
                },
            ]
        );
    }

    #[test]
    fn sanitizes_output_timestamps_per_stream() {
        let mut sanitizer = OutputTimestampSanitizer::default();
        assert_eq!(sanitizer.sanitize(true, 90, 90), (90, 90));
        assert_eq!(sanitizer.sanitize(true, 80, 90), (91, 91));
        assert_eq!(sanitizer.sanitize(false, 20, 10), (20, 10));
        assert_eq!(sanitizer.sanitize(false, 5, 10), (11, 11));
    }
}
