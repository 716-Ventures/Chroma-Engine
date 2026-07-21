use std::{
    fs::{create_dir_all, read_to_string, write},
    path::PathBuf,
};

use anyhow::{Result, anyhow};
use serde::Serialize;

use crate::{
    NativePlaybackManifest,
    fmp4::{Fmp4SampleEntry, Fmp4Track, Fmp4TrackKind, init_segment},
    hls::{
        HlsOptions, HlsSegmentInfo, HlsVodPlaylistPlan, write_hls_fmp4_init,
        write_hls_fmp4_segment, write_hls_fmp4_segment_window, write_hls_fmp4_segments,
        write_hls_fmp4_vod, write_hls_segment, write_hls_segments, write_hls_vod,
    },
};

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
struct HlsPlanOutput {
    segment_count: usize,
    target_duration_seconds: u64,
    video_track_id: String,
    audio_track_id: String,
    bandwidth_bits_per_second: u64,
    video_codec: String,
    audio_codec: String,
    master_playlist: String,
    media_playlist: String,
    fmp4_media_playlist: String,
    segments: Vec<HlsSegmentInfo>,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
struct HlsSegmentsOutput {
    start: usize,
    requested_count: usize,
    segments: Vec<HlsSegmentInfo>,
}

pub(super) fn run_hls(
    input: PathBuf,
    output_dir: PathBuf,
    audio_track: Option<String>,
    segment_ms: u64,
) -> Result<()> {
    let output = write_hls_vod(&input, &output_dir, hls_options(audio_track, segment_ms))?;
    println!("{}", serde_json::to_string_pretty(&output)?);
    Ok(())
}

pub(super) fn run_hls_fmp4(
    input: PathBuf,
    output_dir: PathBuf,
    audio_track: Option<String>,
    segment_ms: u64,
) -> Result<()> {
    let output = write_hls_fmp4_vod(&input, &output_dir, hls_options(audio_track, segment_ms))?;
    println!("{}", serde_json::to_string_pretty(&output)?);
    Ok(())
}

pub(super) fn run_hls_fmp4_init(
    input: PathBuf,
    output: PathBuf,
    audio_track: Option<String>,
    segment_ms: u64,
) -> Result<()> {
    write_hls_fmp4_init(&input, &output, hls_options(audio_track, segment_ms))?;
    println!("{}", serde_json::json!({ "ok": true, "output": output }));
    Ok(())
}

pub(super) fn run_hls_fmp4_init_from_manifest(
    manifest: PathBuf,
    output: PathBuf,
    audio_track: Option<String>,
    width: Option<u16>,
    height: Option<u16>,
) -> Result<()> {
    let manifest: NativePlaybackManifest = serde_json::from_str(&read_to_string(manifest)?)?;
    let video = manifest
        .tracks
        .iter()
        .find(|track| track.kind == "video")
        .ok_or_else(|| anyhow!("manifest has no video track"))?;
    let audio = audio_track
        .as_deref()
        .and_then(|id| {
            manifest
                .tracks
                .iter()
                .find(|track| track.kind == "audio" && track.id == id)
        })
        .or_else(|| {
            manifest
                .tracks
                .iter()
                .find(|track| track.kind == "audio" && track.default)
        })
        .or_else(|| manifest.tracks.iter().find(|track| track.kind == "audio"))
        .ok_or_else(|| anyhow!("manifest has no audio track"))?;
    let video_entry = manifest_video_sample_entry(video, width, height)?;
    let audio_entry = manifest_audio_sample_entry(audio)?;
    let init = init_segment(&[
        Fmp4Track {
            id: 1,
            kind: Fmp4TrackKind::Video,
            timescale: 90_000,
            default_sample_duration: 0,
            default_sample_size: 0,
            default_sample_flags: 0x0101_0000,
            sample_entry: video_entry,
        },
        Fmp4Track {
            id: 2,
            kind: Fmp4TrackKind::Audio,
            timescale: audio.sample_rate.unwrap_or(48_000),
            default_sample_duration: 0,
            default_sample_size: 0,
            default_sample_flags: 0x0200_0000,
            sample_entry: audio_entry,
        },
    ])?;
    if let Some(parent) = output.parent() {
        create_dir_all(parent)?;
    }
    write(&output, init)?;
    println!("{}", serde_json::json!({ "ok": true, "output": output }));
    Ok(())
}

pub(super) fn run_hls_fmp4_segment(
    input: PathBuf,
    output: PathBuf,
    index: usize,
    audio_track: Option<String>,
    segment_ms: u64,
) -> Result<()> {
    let segment =
        write_hls_fmp4_segment(&input, index, &output, hls_options(audio_track, segment_ms))?;
    println!("{}", serde_json::to_string_pretty(&segment)?);
    Ok(())
}

pub(super) fn run_hls_fmp4_segment_window(
    input: PathBuf,
    output: PathBuf,
    index: usize,
    start_ms: u64,
    end_ms: u64,
    audio_track: Option<String>,
) -> Result<()> {
    let segment = write_hls_fmp4_segment_window(
        &input,
        &output,
        hls_options(audio_track, 4_000),
        index,
        start_ms,
        end_ms,
    )?;
    println!("{}", serde_json::to_string_pretty(&segment)?);
    Ok(())
}

pub(super) fn run_hls_fmp4_segments(
    input: PathBuf,
    output_dir: PathBuf,
    start: usize,
    count: usize,
    audio_track: Option<String>,
    segment_ms: u64,
) -> Result<()> {
    let segments = write_hls_fmp4_segments(
        &input,
        &output_dir,
        start,
        count,
        hls_options(audio_track, segment_ms),
    )?;
    print_segments(start, count, segments)
}

pub(super) fn run_hls_plan(
    input: PathBuf,
    audio_track: Option<String>,
    segment_ms: u64,
) -> Result<()> {
    let plan = HlsVodPlaylistPlan::open(&input, hls_options(audio_track, segment_ms))?;
    let output = HlsPlanOutput {
        segment_count: plan.segment_count(),
        target_duration_seconds: plan.target_duration_seconds(),
        video_track_id: plan.video_track_id().to_string(),
        audio_track_id: plan.audio_track_id().to_string(),
        bandwidth_bits_per_second: plan.bandwidth_bits_per_second(),
        video_codec: plan.video_codec().to_string(),
        audio_codec: plan.audio_codec().to_string(),
        master_playlist: plan.master_playlist(),
        media_playlist: plan.media_playlist(),
        fmp4_media_playlist: plan.fmp4_media_playlist(),
        segments: plan.segments(),
    };
    println!("{}", serde_json::to_string_pretty(&output)?);
    Ok(())
}

pub(super) fn run_hls_segment(
    input: PathBuf,
    output: PathBuf,
    index: usize,
    audio_track: Option<String>,
    segment_ms: u64,
) -> Result<()> {
    let segment = write_hls_segment(&input, index, &output, hls_options(audio_track, segment_ms))?;
    println!("{}", serde_json::to_string_pretty(&segment)?);
    Ok(())
}

pub(super) fn run_hls_segments(
    input: PathBuf,
    output_dir: PathBuf,
    start: usize,
    count: usize,
    audio_track: Option<String>,
    segment_ms: u64,
) -> Result<()> {
    let segments = write_hls_segments(
        &input,
        &output_dir,
        start,
        count,
        hls_options(audio_track, segment_ms),
    )?;
    print_segments(start, count, segments)
}

fn hls_options(audio_track: Option<String>, segment_ms: u64) -> HlsOptions {
    HlsOptions {
        segment_target_ms: segment_ms,
        audio_track_id: audio_track,
    }
}

fn manifest_video_sample_entry(
    track: &crate::ManifestTrack,
    width: Option<u16>,
    height: Option<u16>,
) -> Result<Fmp4SampleEntry> {
    let config = decode_hex(
        track
            .decoder_config_hex
            .as_deref()
            .ok_or_else(|| anyhow!("manifest video track has no decoder config"))?,
    )?;
    let width = width.unwrap_or(1920);
    let height = height.unwrap_or(1080);
    match track.codec.as_str() {
        "h264" => Ok(Fmp4SampleEntry::Avc {
            codec_config: config,
            width,
            height,
        }),
        "hevc" => Ok(Fmp4SampleEntry::Hevc {
            codec_config: config,
            width,
            height,
        }),
        other => Err(anyhow!(
            "manifest video codec {other} is not supported for fMP4 init"
        )),
    }
}

fn manifest_audio_sample_entry(track: &crate::ManifestTrack) -> Result<Fmp4SampleEntry> {
    let channels = u16::try_from(track.channels.unwrap_or(2)).unwrap_or(2);
    let sample_rate = track.sample_rate.unwrap_or(48_000);
    match track.codec.as_str() {
        "aac" => Ok(Fmp4SampleEntry::Aac {
            decoder_config: decode_hex(
                track
                    .decoder_config_hex
                    .as_deref()
                    .ok_or_else(|| anyhow!("manifest AAC track has no decoder config"))?,
            )?,
            channel_count: channels,
            sample_rate,
        }),
        "ac3" => Ok(Fmp4SampleEntry::Ac3 {
            dac3: [0x50, 0x51, 0x00],
            channel_count: channels,
            sample_rate,
        }),
        "eac3" => Ok(Fmp4SampleEntry::Eac3 {
            dec3: vec![0x00, 0x10, 0x20, 0x0f, 0x00],
            channel_count: channels,
            sample_rate,
        }),
        other => Err(anyhow!(
            "manifest audio codec {other} is not supported for fMP4 init"
        )),
    }
}

fn decode_hex(raw: &str) -> Result<Vec<u8>> {
    let s = raw.trim();
    if !s.len().is_multiple_of(2) {
        return Err(anyhow!("hex string has odd length"));
    }
    let mut out = Vec::with_capacity(s.len() / 2);
    let bytes = s.as_bytes();
    for i in (0..bytes.len()).step_by(2) {
        let hi = hex_nibble(bytes[i]).ok_or_else(|| anyhow!("invalid hex digit"))?;
        let lo = hex_nibble(bytes[i + 1]).ok_or_else(|| anyhow!("invalid hex digit"))?;
        out.push((hi << 4) | lo);
    }
    Ok(out)
}

fn hex_nibble(b: u8) -> Option<u8> {
    match b {
        b'0'..=b'9' => Some(b - b'0'),
        b'a'..=b'f' => Some(b - b'a' + 10),
        b'A'..=b'F' => Some(b - b'A' + 10),
        _ => None,
    }
}

fn print_segments(
    start: usize,
    requested_count: usize,
    segments: Vec<HlsSegmentInfo>,
) -> Result<()> {
    let output = HlsSegmentsOutput {
        start,
        requested_count,
        segments,
    };
    println!("{}", serde_json::to_string_pretty(&output)?);
    Ok(())
}
