use std::path::PathBuf;

use anyhow::Result;
use serde::Serialize;

use crate::hls::{
    HlsOptions, HlsSegmentInfo, HlsVodPlaylistPlan, write_hls_fmp4_init, write_hls_fmp4_segment,
    write_hls_fmp4_segments, write_hls_fmp4_vod, write_hls_segment, write_hls_segments,
    write_hls_vod,
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
