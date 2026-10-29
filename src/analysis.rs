//! Bounded native skip-range analysis; uncertain evidence never becomes a marker.
use anyhow::{Context, Result, ensure};
use serde::{Deserialize, Serialize};
use std::{
    path::{Path, PathBuf},
    time::UNIX_EPOCH,
};

mod audio;
mod visual;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub(super) enum MarkerKind {
    Intro,
    Recap,
    Credits,
    Commercial,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub(super) struct DetectedMarker {
    #[serde(rename = "type")]
    pub kind: MarkerKind,
    pub start_time_offset: u64,
    pub end_time_offset: u64,
    pub confidence: f64,
    pub method: String,
    pub reference_count: usize,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub(super) struct AnalysisOutput {
    schema_version: u32,
    duration_ms: u64,
    source_size_bytes: u64,
    source_modified_ns: String,
    chapters: Vec<crate::probe::Chapter>,
    markers: Vec<DetectedMarker>,
    notices: Vec<String>,
}

pub(super) fn analyze(
    input: &Path,
    references: &[PathBuf],
    cache_dir: &Path,
    audio_only: bool,
) -> Result<AnalysisOutput> {
    ensure!(
        references.len() <= 2,
        "At most two reference episodes may be analyzed"
    );
    let before = std::fs::metadata(input)?;
    let modified = before.modified()?.duration_since(UNIX_EPOCH)?.as_nanos();
    let probe = crate::probe::probe_media_source(input)?;
    let duration = probe
        .duration_ms
        .context("Source duration is unavailable")?;
    ensure!(
        duration > 0 && duration <= 12 * 3600 * 1000,
        "Source duration exceeds the analysis limit"
    );
    std::fs::create_dir_all(cache_dir)?;
    let mut markers = Vec::new();
    let mut notices = Vec::new();
    if !references.is_empty() {
        match audio::detect(input, references, cache_dir) {
            Ok(found) => markers.extend(found),
            Err(error) => {
                eprintln!("Audio skip analysis unavailable: {error}");
                notices.push("Audio repetition analysis was unavailable for this source.".into());
            }
        }
    }
    if !audio_only {
        match visual::detect(input, cache_dir) {
            Ok(found) => markers.extend(found),
            Err(error) => {
                eprintln!("Visual credit analysis unavailable: {error}");
                notices.push("Visual credits analysis was unavailable for this source.".into());
            }
        }
    }
    let after = std::fs::metadata(input)?;
    ensure!(
        before.len() == after.len() && before.modified()? == after.modified()?,
        "Media source changed during analysis"
    );
    markers.retain(|marker| {
        marker.confidence.is_finite()
            && marker.confidence >= 0.90
            && marker.confidence <= 1.0
            && marker.start_time_offset < marker.end_time_offset
            && marker.end_time_offset <= duration
    });
    // Prefer a repeated-audio boundary when two detectors cover the same range.
    // Disjoint credit rolls remain separate, preserving intervening scenes.
    markers.sort_by(|a, b| {
        b.confidence
            .total_cmp(&a.confidence)
            .then(b.reference_count.cmp(&a.reference_count))
    });
    let mut accepted: Vec<DetectedMarker> = Vec::new();
    for marker in markers {
        if !accepted.iter().any(|other| {
            marker.start_time_offset < other.end_time_offset
                && marker.end_time_offset > other.start_time_offset
        }) {
            accepted.push(marker);
        }
        if accepted.len() >= 128 {
            break;
        }
    }
    accepted.sort_by_key(|marker| marker.start_time_offset);
    Ok(AnalysisOutput {
        schema_version: 1,
        duration_ms: duration,
        source_size_bytes: before.len(),
        source_modified_ns: modified.to_string(),
        chapters: probe.chapters,
        markers: accepted,
        notices,
    })
}
