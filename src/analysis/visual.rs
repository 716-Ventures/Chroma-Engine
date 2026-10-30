//! Keyframe credit screening with cached evidence and targeted boundary refinement.
mod native_frames;
use super::evidence_cache::EvidenceCache;
use super::{DetectedMarker, MarkerKind};
use anyhow::{Result, ensure};
use native_frames::{Frame, Sampler};
use serde::{Deserialize, Serialize};
use std::{collections::BTreeMap, path::Path};

const MIN_ROLL_MS: u64 = 20_000;

struct Features {
    mask: Vec<u8>,
    text_like: bool,
    #[cfg(test)]
    diagnostics: (f64, usize, usize, f64, f64),
}
fn features(frame: &Frame) -> Result<Features> {
    let size = frame
        .width
        .checked_mul(frame.height)
        .filter(|size| *size <= 640 * 360)
        .ok_or_else(|| anyhow::anyhow!("Visual feature frame exceeds limit"))?;
    ensure!(
        frame.width >= 32 && frame.height >= 32 && frame.luma.len() == size,
        "Invalid visual feature frame"
    );
    let mask = frame
        .luma
        .iter()
        .map(|value| u8::from(*value >= 110))
        .collect::<Vec<_>>();
    let bright = mask.iter().filter(|value| **value != 0).count();
    let dark = frame.luma.iter().filter(|value| **value <= 35).count();
    let mut output = Features {
        mask,
        text_like: false,
        #[cfg(test)]
        diagnostics: (dark as f64 / size as f64, 0, 0, 0.0, 0.0),
    };
    if dark as f64 / (size as f64) < 0.83
        || bright as f64 / (size as f64) < 0.005
        || bright as f64 / (size as f64) > 0.18
        || frame.color_fraction > 0.08
    {
        return Ok(output);
    }
    // Connected bright components must resemble many small glyphs aligned in
    // several text rows. Random stars/lights and solid logos do not suffice.
    let mut visited = vec![false; size];
    let mut queue = Vec::new();
    let mut glyphs = 0usize;
    let mut glyph_pixels = 0usize;
    let scale = frame.width.div_ceil(320).clamp(1, 2);
    let baseline_step = 3 * scale;
    let mut baselines = vec![0usize; frame.height.div_ceil(baseline_step)];
    for initial in 0..size {
        if visited[initial] || output.mask[initial] == 0 {
            continue;
        }
        visited[initial] = true;
        queue.clear();
        queue.push(initial);
        let (mut left, mut right, mut top, mut bottom) = (
            initial % frame.width,
            initial % frame.width,
            initial / frame.width,
            initial / frame.width,
        );
        let mut index = 0;
        while index < queue.len() {
            let current = queue[index];
            index += 1;
            let (x, y) = (current % frame.width, current / frame.width);
            left = left.min(x);
            right = right.max(x);
            top = top.min(y);
            bottom = bottom.max(y);
            let neighbors = [
                x.checked_sub(1).map(|_| current - 1),
                (x + 1 < frame.width).then_some(current + 1),
                y.checked_sub(1).map(|_| current - frame.width),
                (y + 1 < frame.height).then_some(current + frame.width),
            ];
            for neighbor in neighbors.into_iter().flatten() {
                if !visited[neighbor] && output.mask[neighbor] != 0 {
                    visited[neighbor] = true;
                    queue.push(neighbor);
                }
            }
        }
        let width = right - left + 1;
        let height = bottom - top + 1;
        let area = queue.len();
        if (2..=96 * scale * scale).contains(&area)
            && (2..=14 * scale).contains(&height)
            && (2..=18 * scale).contains(&width)
            && area * 8 >= width * height
            && area * 100 <= width * height * 90
        {
            glyphs += 1;
            glyph_pixels += area;
            baselines[bottom / baseline_step] += 1;
        }
    }
    // Merge neighboring baseline bins: antialiasing, lowercase descenders and
    // native scaling may displace a baseline by one or two sampled pixels.
    let mut lines = 0;
    let mut aligned = 0;
    let mut bin = 0;
    while bin < baselines.len() {
        let count = baselines[bin..(bin + 2).min(baselines.len())]
            .iter()
            .sum::<usize>();
        if count >= 5 {
            lines += 1;
            aligned += count;
            bin += 3;
        } else {
            bin += 1;
        }
    }
    let mut concentrated = baselines.clone();
    concentrated.sort_unstable_by_key(|count| std::cmp::Reverse(*count));
    let in_baselines = concentrated
        .iter()
        .take(frame.height.div_ceil(10 * scale))
        .sum::<usize>();
    output.text_like = glyphs >= 24
        && (3..=24).contains(&lines)
        && glyph_pixels * 100 >= bright * 65
        && aligned * 100 >= glyphs * 65
        && in_baselines * 100 >= glyphs * 70;
    #[cfg(test)]
    {
        output.diagnostics = (
            dark as f64 / size as f64,
            glyphs,
            lines,
            glyph_pixels as f64 / bright.max(1) as f64,
            in_baselines as f64 / glyphs.max(1) as f64,
        );
    }
    Ok(output)
}

// Bounded tail search, as in established media-center credits analyzers.
fn search_window(duration: u64) -> u64 {
    (duration * 35 / 100).min(15 * 60 * 1000)
}
#[derive(Clone, Default, Serialize, Deserialize)]
struct Observation {
    time: u64,
    text_like: bool,
    dark_fraction: f64,
    bright_fraction: f64,
    color_fraction: f64,
}
fn observe(frame: &Frame) -> Result<Observation> {
    let f = features(frame)?;
    let n = frame.luma.len() as f64;
    Ok(Observation {
        time: frame.time_ms,
        text_like: f.text_like,
        dark_fraction: frame.luma.iter().filter(|v| **v <= 35).count() as f64 / n,
        bright_fraction: frame.luma.iter().filter(|v| **v >= 110).count() as f64 / n,
        color_fraction: frame.color_fraction,
    })
}
#[derive(Default, Serialize, Deserialize)]
struct Evidence {
    version: u32,
    duration: u64,
    complete: bool,
    samples: Vec<Observation>,
    #[serde(default)]
    refined: BTreeMap<u64, Observation>,
}
fn valid(e: &Evidence, duration: u64) -> bool {
    e.version == 2
        && duration > 0
        && duration <= 12 * 3600 * 1000
        && e.duration == duration
        && e.refined.len() <= 4000
        && e.refined
            .iter()
            .all(|(t, o)| *t <= duration && o.time <= duration)
        && e.samples.len() <= 4000
        && e.samples.iter().all(|s| s.time <= duration)
        && e.samples.windows(2).all(|s| s[0].time < s[1].time)
}
fn candidates(samples: &[Observation]) -> Vec<(usize, usize)> {
    let mut out = Vec::new();
    let mut first: Option<usize> = None;
    for i in 0..=samples.len() {
        let positive = i < samples.len() && samples[i].text_like;
        let gap = i > 0
            && i < samples.len()
            && samples[i].time.saturating_sub(samples[i - 1].time) > 10000;
        if (!positive || gap)
            && let Some(start) = first.take()
            && i - start >= 4
            && samples[i - 1].time.saturating_sub(samples[start].time) >= MIN_ROLL_MS
        {
            out.push((start, i - 1));
        }
        if positive && first.is_none() {
            first = Some(i);
        }
    }
    out
}
fn refine(
    sampler: &mut Option<Sampler>,
    input: &Path,
    evidence: &mut Evidence,
    start: u64,
    end: u64,
    find_start: bool,
) -> Result<u64> {
    let mut time = start;
    let mut boundary = if find_start { end } else { start };
    while time <= end {
        if let std::collections::btree_map::Entry::Vacant(entry) = evidence.refined.entry(time) {
            if sampler.is_none() {
                *sampler = Some(Sampler::open(input)?);
            }
            if let Some(frame) = sampler.as_mut().unwrap().frame(time)? {
                entry.insert(observe(frame)?);
            }
        }
        if let Some(observed) = evidence.refined.get(&time) {
            if find_start && observed.text_like {
                return Ok(observed.time);
            }
            if !find_start {
                if !observed.text_like {
                    return Ok(boundary);
                }
                boundary = observed.time;
            }
        }
        time = time.saturating_add(1000);
    }
    Ok(boundary)
}
pub(super) fn detect(input: &Path, cache_dir: &Path) -> Result<Vec<DetectedMarker>> {
    let cache = EvidenceCache::open(input, cache_dir, "visual-keyframes-v2")?;
    let cached = cache.read::<Evidence>();
    // A complete evidence hit does not open the video decoder or packet index.
    let mut sampler = None;
    let mut evidence = if let Some(e) = cached.filter(|e| valid(e, e.duration) && e.complete) {
        e
    } else {
        let mut scan = Sampler::open(input)?;
        let duration = scan.duration_ms();
        let first = duration.saturating_sub(search_window(duration));
        let mut e = cache
            .read::<Evidence>()
            .filter(|e| valid(e, duration))
            .unwrap_or(Evidence {
                version: 2,
                duration,
                ..Default::default()
            });
        for time in scan.keyframe_times().into_iter().filter(|t| *t >= first) {
            if e.samples.last().is_some_and(|last| last.time >= time) {
                continue;
            }
            let observed = scan.keyframe(time)?.as_ref().map(observe).transpose()?;
            // Missing decoder output is unknown, never positive evidence.
            e.samples.push(observed.unwrap_or(Observation {
                time,
                ..Default::default()
            }));
            if e.samples.len() % 32 == 0 {
                cache.write(&e)?;
            }
        }
        scan.finish()?;
        e.complete = true;
        cache.write(&e)?;
        sampler = Some(scan);
        e
    };
    let mut markers = Vec::new();
    for (first, last) in candidates(&evidence.samples).into_iter().take(8) {
        let samples = &evidence.samples;
        let initial = samples[first].time;
        let final_time = samples[last].time;
        // Refinement touches only the two boundary GOP neighborhoods, not the
        // entire credit roll. Keyframe negatives split post-credit scenes.
        let before = first
            .checked_sub(1)
            .map(|i| samples[i].time)
            .unwrap_or(initial)
            .max(initial.saturating_sub(10000));
        let after = samples
            .get(last + 1)
            .map(|s| s.time)
            .unwrap_or(evidence.duration.saturating_sub(1))
            .min(final_time.saturating_add(10000));
        let start = refine(&mut sampler, input, &mut evidence, before, initial, true)?;
        let end = refine(&mut sampler, input, &mut evidence, final_time, after, false)?;
        if end.saturating_sub(start) >= MIN_ROLL_MS {
            markers.push(DetectedMarker {
                kind: MarkerKind::Credits,
                start_time_offset: start,
                end_time_offset: end,
                confidence: 0.93,
                method: "keyframe-credits-refined-v2".into(),
                reference_count: 0,
            });
        }
    }
    if let Some(scan) = sampler {
        scan.finish()?;
    }
    cache.validate()?;
    // Persist reusable boundary observations as well as the screening pass.
    cache.write(&evidence)?;
    Ok(markers)
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn scenes_and_sparse_or_short_evidence_split_credit_runs() {
        let samples = (0..36)
            .map(|i| Observation {
                time: i * 2000,
                text_like: !(14..20).contains(&i),
                ..Default::default()
            })
            .collect::<Vec<_>>();
        assert_eq!(candidates(&samples), [(0, 13), (20, 35)]);
        let mut sparse = samples[..14].to_vec();
        sparse[7].time += 20000;
        assert!(candidates(&sparse).is_empty());
        assert!(candidates(&samples[..5]).is_empty());
    }
    #[test]
    fn cached_clock_and_format_are_validated() {
        let mut e = Evidence {
            version: 2,
            duration: 100000,
            complete: true,
            refined: BTreeMap::new(),
            samples: vec![
                Observation {
                    time: 5,
                    text_like: true,
                    ..Default::default()
                },
                Observation {
                    time: 10,
                    text_like: false,
                    ..Default::default()
                },
            ],
        };
        assert!(valid(&e, 100000));
        e.samples[1].time = 4;
        assert!(!valid(&e, 100000));
        e.samples[1].time = 100001;
        assert!(!valid(&e, 100000));
    }
    #[test]
    fn complete_cached_boundaries_need_no_decoder_and_preserve_post_credit_gap() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("not-a-decodable-video");
        std::fs::write(&path, b"immutable fixture").unwrap();
        let samples = (0..60)
            .map(|i| Observation {
                time: i * 2000,
                text_like: (38..54).contains(&i),
                ..Default::default()
            })
            .collect();
        let refined = (74000..=108000)
            .step_by(1000)
            .map(|t| {
                (
                    t,
                    Observation {
                        time: t,
                        text_like: (75000..108000).contains(&t),
                        ..Default::default()
                    },
                )
            })
            .collect();
        let e = Evidence {
            version: 2,
            duration: 120000,
            complete: true,
            samples,
            refined,
        };
        EvidenceCache::open(&path, dir.path(), "visual-keyframes-v2")
            .unwrap()
            .write(&e)
            .unwrap();
        let found = detect(&path, dir.path()).unwrap();
        assert_eq!(found.len(), 1);
        assert_eq!(
            (found[0].start_time_offset, found[0].end_time_offset),
            (75000, 107000)
        );
        assert_eq!(
            detect(&path, dir.path()).unwrap()[0].end_time_offset,
            107000
        );
    }
    #[test]
    fn stationary_credit_text_passes_keyframe_screen_without_motion_requirement() {
        let mut frame = Frame {
            time_ms: 0,
            width: 320,
            height: 180,
            luma: vec![0; 320 * 180],
            color_fraction: 0.0,
        };
        for line in 0..14 {
            for column in 0..(28 - line % 4) {
                for dy in 0..7 {
                    for dx in 0..5 {
                        if dx == 0 || dx == 4 || dy == 3 {
                            let x = 50 + column * 7 + dx;
                            let y = line * 13 + dy;
                            if y < 180 {
                                frame.luma[y * 320 + x] = 220;
                            }
                        }
                    }
                }
            }
        }
        assert!(features(&frame).unwrap().text_like);
        frame.color_fraction = 0.5;
        assert!(!features(&frame).unwrap().text_like);
    }
    #[test]
    fn darkness_and_solid_logos_do_not_pass_text_screen() {
        let mut frame = Frame {
            time_ms: 0,
            width: 320,
            height: 180,
            luma: vec![0; 320 * 180],
            color_fraction: 0.0,
        };
        assert!(!features(&frame).unwrap().text_like);
        for y in 70..100 {
            for x in 80..240 {
                frame.luma[y * 320 + x] = 220;
            }
        }
        assert!(!features(&frame).unwrap().text_like);
    }
}
