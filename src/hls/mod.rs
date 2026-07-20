use std::{
    fs::{create_dir_all, write, File},
    path::{Path, PathBuf},
};

use anyhow::{anyhow, bail, Context, Result};
use memmap2::Mmap;
use serde::{Deserialize, Serialize};

use crate::{
    codec::{
        aac::{adts_header, parse_audio_specific_config, AacAudioSpecificConfig},
        h264::{avc_sample_to_annex_b, parse_avc_decoder_config, AvcParameterSets},
        hevc::{hevc_sample_to_annex_b, parse_hevc_decoder_config, HevcDecoderConfig},
    },
    container::{
        matroska::{self, MatroskaTrackKind},
        mp4::{self, Mp4TrackKind},
    },
    packet::PacketRef,
};

const VIDEO_PID: u16 = 0x0100;
const AUDIO_PID: u16 = 0x0101;
const PMT_PID: u16 = 0x1000;
const VIDEO_STREAM_ID: u8 = 0xe0;
const AUDIO_STREAM_ID: u8 = 0xc0;
const PRIVATE_STREAM_ID: u8 = 0xbd;
const TS_CLOCK: u64 = 90_000;
const MIN_SEGMENT_MS: u64 = 1_000;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HlsOptions {
    pub segment_target_ms: u64,
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
pub struct HlsOutput {
    pub master_playlist: PathBuf,
    pub media_playlist: PathBuf,
    pub segment_count: usize,
    pub target_duration_seconds: u64,
    pub video_track_id: String,
    pub audio_track_id: String,
    pub bandwidth_bits_per_second: u64,
    pub video_codec: String,
    pub audio_codec: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct HlsSegmentInfo {
    pub index: usize,
    pub start_ms: u64,
    pub duration_ms: u64,
    pub uri: String,
}

pub struct HlsVodPlan {
    source: Mmap,
    tracks: HlsTrackSet,
    windows: Vec<SegmentWindow>,
    target_duration_seconds: u64,
    bandwidth_bits_per_second: u64,
}

impl HlsVodPlan {
    pub fn open(input: &Path, options: HlsOptions) -> Result<Self> {
        let file = File::open(input).with_context(|| format!("open {}", input.display()))?;
        let source =
            unsafe { Mmap::map(&file) }.with_context(|| format!("map {}", input.display()))?;
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
        let target_duration_seconds = windows
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
            .max(1);
        let bandwidth_bits_per_second = estimate_hls_bandwidth_bits_per_second(&tracks, &windows);

        Ok(Self {
            source,
            tracks,
            windows,
            target_duration_seconds,
            bandwidth_bits_per_second,
        })
    }

    pub fn segment_count(&self) -> usize {
        self.windows.len()
    }

    pub fn target_duration_seconds(&self) -> u64 {
        self.target_duration_seconds
    }

    pub fn bandwidth_bits_per_second(&self) -> u64 {
        self.bandwidth_bits_per_second
    }

    pub fn video_codec(&self) -> &str {
        &self.tracks.video.codec_string
    }

    pub fn audio_codec(&self) -> &str {
        &self.tracks.audio.codec_string
    }

    pub fn video_track_id(&self) -> &str {
        &self.tracks.video.id
    }

    pub fn audio_track_id(&self) -> &str {
        &self.tracks.audio.id
    }

    pub fn segments(&self) -> Vec<HlsSegmentInfo> {
        self.windows
            .iter()
            .map(|window| HlsSegmentInfo {
                index: window.index,
                start_ms: window.start_ms,
                duration_ms: window.end_ms.saturating_sub(window.start_ms).max(1),
                uri: segment_name(window.index),
            })
            .collect()
    }

    pub fn master_playlist(&self) -> String {
        master_playlist_body(
            self.video_codec(),
            self.audio_codec(),
            self.bandwidth_bits_per_second,
        )
    }

    pub fn media_playlist(&self) -> String {
        let durations: Vec<u64> = self
            .windows
            .iter()
            .map(|window| window.end_ms.saturating_sub(window.start_ms).max(1))
            .collect();
        media_playlist_body(self.target_duration_seconds, &durations)
    }

    pub fn mux_segment(&self, index: usize) -> Result<Vec<u8>> {
        let window = self
            .windows
            .get(index)
            .copied()
            .ok_or_else(|| anyhow!("HLS segment index {index} is out of range"))?;
        mux_segment(self.source.as_ref(), &self.tracks, window)
    }

    pub fn write_segment(&self, index: usize, output: &Path) -> Result<HlsSegmentInfo> {
        let window = self
            .windows
            .get(index)
            .copied()
            .ok_or_else(|| anyhow!("HLS segment index {index} is out of range"))?;
        let segment = mux_segment(self.source.as_ref(), &self.tracks, window)?;
        if let Some(parent) = output.parent() {
            create_dir_all(parent)?;
        }
        write(output, segment)?;
        Ok(HlsSegmentInfo {
            index: window.index,
            start_ms: window.start_ms,
            duration_ms: window.end_ms.saturating_sub(window.start_ms).max(1),
            uri: segment_name(window.index),
        })
    }

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
            let segment = mux_segment(self.source.as_ref(), &self.tracks, window)?;
            write(output_dir.join(segment_name(window.index)), segment)?;
            written.push(HlsSegmentInfo {
                index: window.index,
                start_ms: window.start_ms,
                duration_ms: window.end_ms.saturating_sub(window.start_ms).max(1),
                uri: segment_name(window.index),
            });
        }
        Ok(written)
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
    packets: Vec<PacketRef>,
    payload: PayloadKind,
}

#[derive(Debug, Clone)]
enum PayloadKind {
    Avc {
        nalu_length_size: u8,
        parameter_sets: AvcParameterSets,
    },
    Hevc {
        nalu_length_size: u8,
        parameter_sets: HevcDecoderConfig,
    },
    Aac {
        config: AacAudioSpecificConfig,
    },
    Ac3,
    Eac3,
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
    write(&media_playlist, plan.media_playlist())?;
    let master_playlist = output_dir.join("master.m3u8");
    write(&master_playlist, plan.master_playlist())?;

    Ok(HlsOutput {
        master_playlist,
        media_playlist,
        segment_count: plan.segment_count(),
        target_duration_seconds: plan.target_duration_seconds(),
        video_track_id: plan.video_track_id().to_string(),
        audio_track_id: plan.audio_track_id().to_string(),
        bandwidth_bits_per_second: plan.bandwidth_bits_per_second(),
        video_codec: plan.video_codec().to_string(),
        audio_codec: plan.audio_codec().to_string(),
    })
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
    match (kind.into(), codec) {
        (HlsTrackKind::Video, "h264" | "hevc") => true,
        (HlsTrackKind::Audio, "aac" | "ac3" | "eac3") => true,
        _ => false,
    }
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
                parameter_sets,
            }
        }
        other => bail!("native HLS MP4 video codec {other} is not supported"),
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
        other => bail!("native HLS MP4 audio codec {other} is not supported"),
    };

    Ok(HlsTrackSet {
        video: HlsTrack {
            id: video_track_id,
            codec_string: video_config
                .codec_string
                .unwrap_or_else(|| fallback_video_codec_string(video_meta.codec.as_str())),
            packets: video_packets,
            payload: video_payload,
        },
        audio: HlsTrack {
            id: audio_track_id,
            codec_string: audio_config
                .codec_string
                .unwrap_or_else(|| fallback_audio_codec_string(audio_meta.codec.as_str())),
            packets: audio_packets,
            payload: audio_payload,
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
            PayloadKind::Avc {
                nalu_length_size: parse_avc_decoder_config(avc)?.nalu_length_size,
                parameter_sets: parse_avc_decoder_config(avc)?,
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
                parameter_sets,
            }
        }
        other => bail!("native HLS Matroska video codec {other} is not supported"),
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
        other => bail!("native HLS Matroska audio codec {other} is not supported"),
    };

    Ok(HlsTrackSet {
        video: HlsTrack {
            id: video_track_id,
            codec_string: matroska_video_codec_string(video),
            packets: video_packets,
            payload: video_payload,
        },
        audio: HlsTrack {
            id: audio_track_id,
            codec_string: matroska_audio_codec_string(audio),
            packets: audio_packets,
            payload: audio_payload,
        },
    })
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
        if duration < min_duration_ms {
            if let Some(last) = out.last_mut() {
                last.end_ms = window.end_ms;
                continue;
            }
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

#[derive(Debug, Default)]
struct OutputTimestampSanitizer {
    last_video_dts90: Option<u64>,
    last_audio_dts90: Option<u64>,
}

impl OutputTimestampSanitizer {
    fn sanitize(&mut self, is_video: bool, pts90: u64, dts90: u64) -> (u64, u64) {
        let slot = if is_video {
            &mut self.last_video_dts90
        } else {
            &mut self.last_audio_dts90
        };
        let out_dts = match *slot {
            Some(last) if dts90 <= last => last.saturating_add(1),
            _ => dts90,
        };
        *slot = Some(out_dts);
        (pts90.max(out_dts), out_dts)
    }
}

fn packet_to_payload(bytes: &[u8], packet: &PacketRef, kind: &PayloadKind) -> Result<Vec<u8>> {
    let start = packet.source_offset as usize;
    let end = start
        .checked_add(packet.size as usize)
        .ok_or_else(|| anyhow!("packet range overflows"))?;
    if end > bytes.len() {
        bail!("packet range is outside source");
    }
    match kind {
        PayloadKind::Avc {
            nalu_length_size,
            parameter_sets,
        } => {
            let mut out = Vec::new();
            if packet.keyframe {
                for sps in &parameter_sets.sps {
                    out.extend_from_slice(&[0, 0, 0, 1]);
                    out.extend_from_slice(sps);
                }
                for pps in &parameter_sets.pps {
                    out.extend_from_slice(&[0, 0, 0, 1]);
                    out.extend_from_slice(pps);
                }
            }
            out.extend_from_slice(&h264_sample_to_annex_b(
                &bytes[start..end],
                *nalu_length_size,
            )?);
            Ok(out)
        }
        PayloadKind::Aac { config } => {
            let mut out = Vec::with_capacity(packet.size as usize + 7);
            out.extend_from_slice(&adts_header(packet.size as usize, *config)?);
            out.extend_from_slice(&bytes[start..end]);
            Ok(out)
        }
        PayloadKind::Hevc {
            nalu_length_size,
            parameter_sets,
        } => {
            let mut out = Vec::new();
            if packet.keyframe {
                for array in &parameter_sets.arrays {
                    if !matches!(array.nal_unit_type, 32 | 33 | 34) {
                        continue;
                    }
                    for unit in &array.units {
                        out.extend_from_slice(&[0, 0, 0, 1]);
                        out.extend_from_slice(unit);
                    }
                }
            }
            out.extend_from_slice(&hevc_sample_to_annex_b(
                &bytes[start..end],
                *nalu_length_size,
            )?);
            Ok(out)
        }
        PayloadKind::Ac3 | PayloadKind::Eac3 => Ok(bytes[start..end].to_vec()),
    }
}

fn h264_sample_to_annex_b(sample: &[u8], nalu_length_size: u8) -> Result<Vec<u8>> {
    if looks_like_annex_b(sample) {
        return Ok(sample.to_vec());
    }
    Ok(avc_sample_to_annex_b(sample, nalu_length_size)?)
}

fn looks_like_annex_b(sample: &[u8]) -> bool {
    sample.starts_with(&[0, 0, 1]) || sample.starts_with(&[0, 0, 0, 1])
}

struct TsMuxer {
    out: Vec<u8>,
    continuity: [u8; 8192],
    video_stream_type: u8,
    audio_stream_type: u8,
}

impl TsMuxer {
    fn new(video_stream_type: u8, audio_stream_type: u8) -> Self {
        Self {
            out: Vec::new(),
            continuity: [0; 8192],
            video_stream_type,
            audio_stream_type,
        }
    }

    fn into_bytes(self) -> Vec<u8> {
        self.out
    }

    fn write_pat_pmt(&mut self) {
        self.write_psi(0, &pat_section());
        self.write_psi(
            PMT_PID,
            &pmt_section(self.video_stream_type, self.audio_stream_type),
        );
    }

    fn write_psi(&mut self, pid: u16, section: &[u8]) {
        let mut payload = Vec::with_capacity(section.len() + 1);
        payload.push(0);
        payload.extend_from_slice(section);
        self.write_ts_packets(pid, true, None, &payload);
    }

    fn write_pes(&mut self, pid: u16, stream_id: u8, sample: &TimedPayload, with_pcr: bool) {
        let mut payload = pes_packet(stream_id, sample.pts90, sample.dts90, &sample.bytes);
        let pcr = if with_pcr { Some(sample.dts90) } else { None };
        self.write_ts_packets(pid, true, pcr, &payload);
        payload.clear();
    }

    fn write_ts_packets(
        &mut self,
        pid: u16,
        payload_start: bool,
        pcr: Option<u64>,
        payload: &[u8],
    ) {
        let mut offset = 0_usize;
        let mut first = true;
        while offset < payload.len() || (payload.is_empty() && first) {
            let remaining = payload.len().saturating_sub(offset);
            let include_pcr = first && pcr.is_some();
            let base_payload_capacity = if include_pcr { 176 } else { 184 };
            let payload_len = remaining.min(base_payload_capacity);
            let needs_stuffing = payload_len < base_payload_capacity;
            let adaptation = include_pcr || needs_stuffing;
            let mut packet = [0xff_u8; 188];
            packet[0] = 0x47;
            packet[1] =
                ((if first && payload_start { 0x40 } else { 0 }) | ((pid >> 8) as u8)) & 0x5f;
            packet[2] = pid as u8;
            let cc = self.continuity[pid as usize] & 0x0f;
            self.continuity[pid as usize] = (cc + 1) & 0x0f;
            packet[3] = (if adaptation { 0x30 } else { 0x10 }) | cc;

            let payload_offset = if adaptation {
                let adaptation_total = 184_usize.saturating_sub(payload_len);
                packet[4] = adaptation_total.saturating_sub(1) as u8;
                packet[5] = if include_pcr { 0x10 } else { 0x00 };
                if let Some(pcr_base) = pcr.filter(|_| include_pcr) {
                    write_pcr(&mut packet[6..12], pcr_base);
                }
                4 + adaptation_total
            } else {
                4
            };
            let payload_end = payload_offset + payload_len;
            packet[payload_offset..payload_end]
                .copy_from_slice(&payload[offset..offset + payload_len]);
            self.out.extend_from_slice(&packet);
            offset += payload_len;
            first = false;
        }
    }
}

fn pat_section() -> Vec<u8> {
    let mut section = vec![
        0x00,
        0xb0,
        0x0d,
        0x00,
        0x01,
        0xc1,
        0x00,
        0x00,
        0x00,
        0x01,
        0xe0 | ((PMT_PID >> 8) as u8 & 0x1f),
        PMT_PID as u8,
    ];
    append_crc32(&mut section);
    section
}

fn pmt_section(video_stream_type: u8, audio_stream_type: u8) -> Vec<u8> {
    let audio_descriptors = pmt_audio_descriptors(audio_stream_type);
    let section_length = 5 + 4 + 2 + 5 + 5 + audio_descriptors.len() + 4;
    let mut section = Vec::with_capacity(3 + section_length);
    section.extend_from_slice(&[
        0x02,
        0xb0 | ((section_length >> 8) as u8 & 0x0f),
        section_length as u8,
        0x00,
        0x01,
        0xc1,
        0x00,
        0x00,
        0xe0 | ((VIDEO_PID >> 8) as u8 & 0x1f),
        VIDEO_PID as u8,
        0xf0,
        0x00,
        video_stream_type,
        0xe0 | ((VIDEO_PID >> 8) as u8 & 0x1f),
        VIDEO_PID as u8,
        0xf0,
        0x00,
        audio_stream_type,
        0xe0 | ((AUDIO_PID >> 8) as u8 & 0x1f),
        AUDIO_PID as u8,
        0xf0 | ((audio_descriptors.len() >> 8) as u8 & 0x0f),
        audio_descriptors.len() as u8,
    ]);
    section.extend_from_slice(&audio_descriptors);
    append_crc32(&mut section);
    section
}

fn pmt_audio_descriptors(audio_stream_type: u8) -> Vec<u8> {
    match audio_stream_type {
        0x81 => registration_descriptor(*b"AC-3"),
        0x87 => {
            let mut descriptors = registration_descriptor(*b"AC-3");
            descriptors.extend_from_slice(&registration_descriptor(*b"EAC3"));
            descriptors
        }
        _ => Vec::new(),
    }
}

fn registration_descriptor(format_identifier: [u8; 4]) -> Vec<u8> {
    let mut out = Vec::with_capacity(6);
    out.push(0x05);
    out.push(0x04);
    out.extend_from_slice(&format_identifier);
    out
}

fn ts_stream_type(payload: &PayloadKind) -> u8 {
    match payload {
        PayloadKind::Avc { .. } => 0x1b,
        PayloadKind::Hevc { .. } => 0x24,
        PayloadKind::Aac { .. } => 0x0f,
        PayloadKind::Ac3 => 0x81,
        PayloadKind::Eac3 => 0x87,
    }
}

fn audio_stream_id(payload: &PayloadKind) -> u8 {
    match payload {
        PayloadKind::Ac3 | PayloadKind::Eac3 => PRIVATE_STREAM_ID,
        _ => AUDIO_STREAM_ID,
    }
}

fn pes_packet(stream_id: u8, pts90: u64, dts90: u64, payload: &[u8]) -> Vec<u8> {
    let has_dts = pts90 != dts90;
    let header_len = if has_dts { 10 } else { 5 };
    let pes_len = payload.len().saturating_add(3 + header_len);
    let pes_len = if pes_len > u16::MAX as usize {
        0
    } else {
        pes_len as u16
    };
    let mut out = Vec::with_capacity(payload.len() + 19);
    out.extend_from_slice(&[0, 0, 1, stream_id]);
    out.extend_from_slice(&pes_len.to_be_bytes());
    out.push(0x80);
    out.push(if has_dts { 0xc0 } else { 0x80 });
    out.push(header_len as u8);
    write_pts(&mut out, if has_dts { 0x03 } else { 0x02 }, pts90);
    if has_dts {
        write_pts(&mut out, 0x01, dts90);
    }
    out.extend_from_slice(payload);
    out
}

fn write_pts(out: &mut Vec<u8>, prefix: u8, ts: u64) {
    let ts = ts & 0x1fff_ffff;
    out.push((prefix << 4) | (((ts >> 30) as u8 & 0x07) << 1) | 1);
    out.push((ts >> 22) as u8);
    out.push((((ts >> 15) as u8 & 0x7f) << 1) | 1);
    out.push((ts >> 7) as u8);
    out.push(((ts as u8 & 0x7f) << 1) | 1);
}

fn write_pcr(out: &mut [u8], pcr_base: u64) {
    let base = pcr_base & 0x1fff_ffff;
    out[0] = (base >> 25) as u8;
    out[1] = (base >> 17) as u8;
    out[2] = (base >> 9) as u8;
    out[3] = (base >> 1) as u8;
    out[4] = ((base as u8 & 0x01) << 7) | 0x7e;
    out[5] = 0;
}

fn append_crc32(section: &mut Vec<u8>) {
    let crc = crc32_mpeg(section);
    section.extend_from_slice(&crc.to_be_bytes());
}

fn crc32_mpeg(bytes: &[u8]) -> u32 {
    let mut crc = 0xffff_ffff_u32;
    for byte in bytes {
        crc ^= u32::from(*byte) << 24;
        for _ in 0..8 {
            crc = if crc & 0x8000_0000 != 0 {
                (crc << 1) ^ 0x04c1_1db7
            } else {
                crc << 1
            };
        }
    }
    crc
}

trait To90Khz {
    fn to_90khz(self, units: u64) -> u64;
}

impl To90Khz for crate::packet::TimeScale {
    fn to_90khz(self, units: u64) -> u64 {
        if self.units_per_second == 0 {
            return 0;
        }
        units.saturating_mul(TS_CLOCK) / u64::from(self.units_per_second)
    }
}

fn master_playlist_body(
    video_codec: &str,
    audio_codec: &str,
    bandwidth_bits_per_second: u64,
) -> String {
    format!(
        "#EXTM3U\n#EXT-X-VERSION:3\n#EXT-X-STREAM-INF:BANDWIDTH={bandwidth_bits_per_second},CODECS=\"{video_codec},{audio_codec}\"\n0/playlist.m3u8\n"
    )
}

fn fallback_video_codec_string(codec: &str) -> String {
    match codec {
        "hevc" => "hvc1.1.6.L120".to_string(),
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
        "hvc1.{profile_space}{profile_idc}.{compatibility:X}.{tier}{level_idc}"
    ))
}

fn media_playlist_body(target_duration_seconds: u64, durations_ms: &[u64]) -> String {
    let mut out = format!(
        "#EXTM3U\n#EXT-X-VERSION:3\n#EXT-X-TARGETDURATION:{target_duration_seconds}\n#EXT-X-MEDIA-SEQUENCE:0\n#EXT-X-PLAYLIST-TYPE:VOD\n"
    );
    for (index, duration_ms) in durations_ms.iter().enumerate() {
        out.push_str(&format!(
            "#EXTINF:{:.3},\n{}\n",
            *duration_ms as f64 / 1000.0,
            segment_name(index)
        ));
    }
    out.push_str("#EXT-X-ENDLIST\n");
    out
}

fn segment_name(index: usize) -> String {
    format!("seg-{index:05}.ts")
}

fn hex_to_bytes(hex: &str) -> Result<Vec<u8>> {
    let clean = hex.trim();
    if clean.len() % 2 != 0 {
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

    fn test_plan_with_one_window() -> HlsVodPlan {
        let file = tempfile::tempfile().expect("tempfile");
        file.set_len(1).expect("size temp file");
        let source = unsafe { Mmap::map(&file).expect("map temp file") };
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
                    packets: vec![packet],
                    payload: PayloadKind::Avc {
                        nalu_length_size: 4,
                        parameter_sets: AvcParameterSets {
                            nalu_length_size: 4,
                            sps: Vec::new(),
                            pps: Vec::new(),
                        },
                    },
                },
                audio: HlsTrack {
                    id: "a0".to_string(),
                    codec_string: "mp4a.40.2".to_string(),
                    packets: Vec::new(),
                    payload: PayloadKind::Aac {
                        config: AacAudioSpecificConfig {
                            object_type: 2,
                            sample_rate: 48_000,
                            channel_config: 2,
                        },
                    },
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
                packets: vec![packet(0, 20_000), packet(1_000, 40_000)],
                payload: PayloadKind::Avc {
                    nalu_length_size: 4,
                    parameter_sets: AvcParameterSets {
                        nalu_length_size: 4,
                        sps: Vec::new(),
                        pps: Vec::new(),
                    },
                },
            },
            audio: HlsTrack {
                id: "a0".to_string(),
                codec_string: "mp4a.40.2".to_string(),
                packets: vec![packet(0, 5_000), packet(1_000, 5_000)],
                payload: PayloadKind::Aac {
                    config: AacAudioSpecificConfig {
                        object_type: 2,
                        sample_rate: 48_000,
                        channel_config: 2,
                    },
                },
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
                width: None,
                height: None,
                channels: None,
                sample_rate: None,
            },
            mp4::Mp4Track {
                index: 1,
                kind: Mp4TrackKind::Audio,
                codec: "aac".to_string(),
                duration_ms: None,
                width: None,
                height: None,
                channels: Some(2),
                sample_rate: Some(48_000),
            },
            mp4::Mp4Track {
                index: 2,
                kind: Mp4TrackKind::Audio,
                codec: "eac3".to_string(),
                duration_ms: None,
                width: None,
                height: None,
                channels: Some(6),
                sample_rate: Some(48_000),
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
                channels: Some(2),
                sample_rate: Some(48_000),
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
                channels: Some(6),
                sample_rate: Some(48_000),
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
