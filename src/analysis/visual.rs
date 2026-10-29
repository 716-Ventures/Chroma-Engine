//! Conservative rolling-credit evidence from actual native-decoded images.
//! The late-source search window bounds work; it never supplies marker times.
//! Static cards, logos, darkness and silence alone are not credit evidence.
mod native_frames;
use super::{DetectedMarker, MarkerKind};
use anyhow::{Result, ensure};
use native_frames::{Frame, Sampler};
use std::path::Path;

const COARSE_MS: u64 = 8000;
const PAIR_MS: u64 = 500;
const FINE_MS: u64 = 1000;
const MIN_ROLL_MS: u64 = 20_000;

struct Features {
    time_ms: u64,
    width: usize,
    height: usize,
    mask: Vec<u8>,
    points: Vec<usize>,
    pixels: usize,
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
    let points = mask
        .iter()
        .enumerate()
        .filter_map(|(index, value)| (*value != 0).then_some(index))
        .collect();
    let dark = frame.luma.iter().filter(|value| **value <= 35).count();
    let mut output = Features {
        time_ms: frame.time_ms,
        width: frame.width,
        height: frame.height,
        mask,
        points,
        pixels: bright,
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

fn overlap(first: &Features, second: &Features, shift: usize, horizontal: isize) -> f64 {
    let mut intersection = 0usize;
    for &point in &first.points {
        let (x, y) = (point % first.width, point / first.width);
        if y >= shift {
            let next = x.checked_add_signed(horizontal);
            if let Some(next) = next.filter(|next| *next < first.width) {
                intersection += usize::from(
                    first.mask[y * first.width + x] != 0
                        && second.mask[(y - shift) * first.width + next] != 0,
                );
            }
        }
    }
    let union = first.pixels + second.pixels - intersection;
    intersection as f64 / union.max(1) as f64
}
fn rolling(first: &Features, second: &Features) -> Option<f64> {
    if !first.text_like
        || !second.text_like
        || first.width != second.width
        || first.height != second.height
        || !(250..=900).contains(&second.time_ms.saturating_sub(first.time_ms))
    {
        return None;
    }
    let stationary = overlap(first, second, 0, 0);
    let mut best = 0.0f64;
    let mut downward = 0.0f64;
    for shift in 1..=first.height / 6 {
        best = best.max(overlap(first, second, shift, 0));
        downward = downward.max(overlap(second, first, shift, 0));
    }
    let horizontal = overlap(first, second, 0, -2).max(overlap(first, second, 0, 2));
    // A static title/logo has a stronger zero-shift match; a tracking shot has
    // horizontal motion or large non-glyph components. Require actual ascent.
    if best < 0.53 || best < stationary + 0.08 || best < horizontal + 0.04 || best < downward + 0.08
    {
        return None;
    }
    Some(best)
}
#[derive(Clone, Copy)]
struct Evidence {
    start: u64,
    end: u64,
    score: f64,
}
fn sample(sampler: &mut Sampler, time: u64) -> Result<Option<Evidence>> {
    let Some(first) = sampler.frame(time)? else {
        return Ok(None);
    };
    let first = features(first)?;
    let Some(second) = sampler.frame(time.saturating_add(PAIR_MS))? else {
        return Ok(None);
    };
    let second = features(second)?;
    Ok(rolling(&first, &second).map(|score| Evidence {
        start: first.time_ms,
        end: second.time_ms,
        score,
    }))
}

fn ranges(samples: &[Option<Evidence>], minimum: u64, max_gap: u64) -> Vec<(u64, u64, f64)> {
    let mut output = Vec::new();
    let mut run: Option<(u64, u64, f64, usize)> = None;
    let finish = |run: &mut Option<(u64, u64, f64, usize)>, output: &mut Vec<(u64, u64, f64)>| {
        if let Some((start, end, score, count)) = run.take()
            && end.saturating_sub(start) >= minimum
            && count >= 4
        {
            output.push((start, end, score / count as f64));
        }
    };
    for evidence in samples {
        if let Some(evidence) = evidence {
            if run.is_some_and(|(_, end, _, _)| evidence.start.saturating_sub(end) > max_gap) {
                finish(&mut run, &mut output);
            }
            match &mut run {
                Some((_, end, score, count)) => {
                    *end = evidence.end;
                    *score += evidence.score;
                    *count += 1;
                }
                None => run = Some((evidence.start, evidence.end, evidence.score, 1)),
            }
        } else {
            finish(&mut run, &mut output);
        }
    }
    finish(&mut run, &mut output);
    output
}

pub(super) fn detect(input: &Path, _cache_dir: &Path) -> Result<Vec<DetectedMarker>> {
    let mut sampler = Sampler::open(input)?;
    let duration = sampler.duration_ms();
    // Only the search cost is bounded by source duration. Every accepted
    // boundary below comes from positively observed rolling-text frames.
    let window = (duration * 35 / 100).min(12 * 60 * 1000);
    if window < MIN_ROLL_MS {
        return Ok(Vec::new());
    }
    let first = duration - window;
    let end = duration.saturating_sub(PAIR_MS);
    let mut coarse = Vec::new();
    let mut time = first;
    while time < end {
        coarse.push(sample(&mut sampler, time)?);
        time = time.saturating_add(COARSE_MS);
    }
    let candidates = ranges(&coarse, MIN_ROLL_MS, COARSE_MS);
    let mut markers = Vec::new();
    let mut fine_calls = 0;
    for (start, stop, _) in candidates {
        let mut fine = Vec::new();
        let mut time = start.saturating_sub(COARSE_MS).max(first);
        let last = stop.saturating_add(COARSE_MS).min(end);
        while time < last {
            fine_calls += 1;
            ensure!(fine_calls <= 900, "Visual refinement work limit exceeded");
            fine.push(sample(&mut sampler, time)?);
            time = time.saturating_add(FINE_MS);
        }
        // Validate the entire run at one-second cadence, not just its edges;
        // any intervening scene splits it instead of being skipped over.
        for (start, end, score) in ranges(&fine, MIN_ROLL_MS, FINE_MS) {
            markers.push(DetectedMarker {
                kind: MarkerKind::Credits,
                start_time_offset: start,
                end_time_offset: end.min(duration),
                confidence: (0.90 + (score - 0.53).max(0.0) * 0.15).min(0.98),
                method: "native-visual-rolling-credits-v1".into(),
                reference_count: 0,
            });
            if markers.len() >= 8 {
                break;
            }
        }
        if markers.len() >= 8 {
            break;
        }
    }
    sampler.finish()?;
    Ok(markers)
}

#[cfg(test)]
mod tests {
    use super::*;
    fn text(shift: usize, time_ms: u64) -> Frame {
        let width = 320;
        let height = 180;
        let mut luma = vec![0; width * height];
        // Connected H-shaped glyphs, varied line lengths and four narrow
        // columns resemble a dense credit roll rather than one solid logo.
        for line in 0..14 {
            for column in 0..(28 - line % 4) {
                let x = 50 + column * 7;
                let y = line * 13;
                for dy in 0..7 {
                    for dx in 0..5 {
                        if (dx == 0 || dx == 4 || dy == 3)
                            && y + dy >= shift
                            && y + dy - shift < height
                        {
                            luma[(y + dy - shift) * width + x + dx] = 220;
                        }
                    }
                }
            }
        }
        Frame {
            time_ms,
            width,
            height,
            luma,
            color_fraction: 0.0,
        }
    }
    #[test]
    fn sustained_monochrome_text_requires_real_upward_motion() {
        let first = features(&text(0, 1000)).unwrap();
        let next = features(&text(3, 1500)).unwrap();
        assert!(first.text_like && next.text_like);
        assert!(rolling(&first, &next).unwrap() > 0.8);
        assert!(rolling(&first, &features(&text(0, 1500)).unwrap()).is_none());
        assert!(rolling(&next, &features(&text(0, 2000)).unwrap()).is_none());
    }
    #[test]
    fn darkness_logos_subtitles_color_and_moving_scenery_are_not_credits() {
        let mut frame = text(0, 1000);
        frame.luma.fill(0);
        assert!(!features(&frame).unwrap().text_like);
        for y in 70..100 {
            for x in 80..240 {
                frame.luma[y * 320 + x] = 220;
            }
        }
        assert!(!features(&frame).unwrap().text_like);
        frame = text(0, 1000);
        for y in 0..150 {
            frame.luma[y * 320..(y + 1) * 320].fill(0);
        }
        assert!(!features(&frame).unwrap().text_like);
        frame = text(0, 1000);
        frame.color_fraction = 0.5;
        assert!(!features(&frame).unwrap().text_like);
        frame = text(0, 1000);
        for (index, pixel) in frame.luma.iter_mut().enumerate() {
            if index % 7 == 0 {
                *pixel = 170;
            }
        }
        assert!(!features(&frame).unwrap().text_like);
    }
    #[test]
    fn upward_moving_monochrome_starfield_does_not_count_as_text_rows() {
        let mut frame = text(0, 1000);
        frame.luma.fill(0);
        for index in 0..300 {
            let x = 3 + (index * 67) % 314;
            let y = 2 + (index * 37) % 174;
            for (dx, dy) in [(0, 0), (1, 0), (-1, 0), (0, 1), (0, -1)] {
                frame.luma[(y as isize + dy) as usize * frame.width + (x as isize + dx) as usize] =
                    220;
            }
        }
        assert!(!features(&frame).unwrap().text_like);
    }
    #[test]
    fn observed_boundaries_stop_at_post_credit_scene_and_preserve_later_roll() {
        let mut samples = Vec::new();
        for second in 0..70 {
            samples.push(if !(25..40).contains(&second) {
                Some(Evidence {
                    start: second * 1000,
                    end: second * 1000 + 500,
                    score: 0.85,
                })
            } else {
                None
            });
        }
        let ranges = ranges(&samples, MIN_ROLL_MS, FINE_MS);
        assert_eq!(ranges.len(), 2);
        assert_eq!((ranges[0].0, ranges[0].1), (0, 24_500));
        assert_eq!((ranges[1].0, ranges[1].1), (40_000, 69_500));
        assert!(
            !ranges
                .iter()
                .any(|(start, end, _)| *start <= 30_000 && *end > 30_000)
        );
    }
    #[test]
    fn short_or_sparse_visual_matches_never_create_a_skip_range() {
        let samples = (0..10)
            .map(|second| {
                Some(Evidence {
                    start: second * 1000,
                    end: second * 1000 + 500,
                    score: 1.0,
                })
            })
            .collect::<Vec<_>>();
        assert!(ranges(&samples, MIN_ROLL_MS, FINE_MS).is_empty());
        let sparse = (0..10)
            .map(|second| {
                Some(Evidence {
                    start: second * 5000,
                    end: second * 5000 + 500,
                    score: 1.0,
                })
            })
            .collect::<Vec<_>>();
        assert!(ranges(&sparse, MIN_ROLL_MS, FINE_MS).is_empty());
        let frame = Frame {
            time_ms: 0,
            width: usize::MAX,
            height: 180,
            luma: vec![],
            color_fraction: 0.0,
        };
        assert!(features(&frame).is_err());
    }
    #[test]
    #[ignore = "requires a privately supplied real source; performs bounded native decode"]
    fn actual_native_source_qualification() {
        let input = std::env::var_os("CHROMA_VISUAL_SOURCE").expect("private source path required");
        let output = std::env::var_os("CHROMA_VISUAL_PROOF").expect("private proof path required");
        let begin = std::time::Instant::now();
        let found = detect(Path::new(&input), Path::new("/private/tmp")).unwrap();
        let summary =
            serde_json::json!({"elapsedSeconds":begin.elapsed().as_secs_f64(),"markers":found});
        std::fs::write(output, serde_json::to_vec_pretty(&summary).unwrap()).unwrap();
        println!(
            "Visual source qualified: {} observed ranges in {:.2}s",
            summary["markers"].as_array().unwrap().len(),
            begin.elapsed().as_secs_f64()
        );
    }
    #[test]
    #[ignore = "requires a privately supplied real source and private image-evidence directory"]
    fn actual_native_frame_evidence() {
        let input = std::env::var_os("CHROMA_VISUAL_SOURCE").expect("private source path required");
        let output = std::path::PathBuf::from(
            std::env::var_os("CHROMA_VISUAL_PROOF").expect("private evidence directory required"),
        );
        std::fs::create_dir_all(&output).unwrap();
        let mut sampler = Sampler::open(Path::new(&input)).unwrap();
        let mut rows = Vec::new();
        for offset in [60, 120, 180, 240, 300, 360, 480, 600] {
            let time = sampler.duration_ms().saturating_sub(offset * 1000);
            let Some(frame) = sampler.frame(time).unwrap().cloned() else {
                continue;
            };
            let mut encoder = png::Encoder::new(
                std::fs::File::create(output.join(format!("tail-{offset}.png"))).unwrap(),
                frame.width as u32,
                frame.height as u32,
            );
            encoder.set_color(png::ColorType::Grayscale);
            encoder.set_depth(png::BitDepth::Eight);
            encoder
                .write_header()
                .unwrap()
                .write_image_data(&frame.luma)
                .unwrap();
            let first = features(&frame).unwrap();
            let Some(next) = sampler.frame(time + PAIR_MS).unwrap() else {
                continue;
            };
            let second = features(next).unwrap();
            let (best_shift, best) = (1..=first.height / 6)
                .map(|shift| (shift, overlap(&first, &second, shift, 0)))
                .max_by(|a, b| a.1.total_cmp(&b.1))
                .unwrap();
            let downward = (1..=first.height / 6)
                .map(|shift| overlap(&second, &first, shift, 0))
                .fold(0.0f64, f64::max);
            rows.push(serde_json::json!({"tailSeconds":offset,"timeMs":frame.time_ms,"pairDeltaMs":second.time_ms.saturating_sub(first.time_ms),"textLike":first.text_like,"nextTextLike":second.text_like,"brightFraction":first.pixels as f64/(first.width*first.height) as f64,"colorFraction":frame.color_fraction,"darkFraction":first.diagnostics.0,"glyphs":first.diagnostics.1,"lines":first.diagnostics.2,"glyphFraction":first.diagnostics.3,"baselineFraction":first.diagnostics.4,"bestUpwardOverlap":best,"bestUpwardShift":best_shift,"bestDownwardOverlap":downward,"stationaryOverlap":overlap(&first,&second,0,0),"acceptedRoll":rolling(&first,&second)}));
        }
        sampler.finish().unwrap();
        std::fs::write(
            output.join("features.json"),
            serde_json::to_vec_pretty(&rows).unwrap(),
        )
        .unwrap();
        println!("{} private sampled frame pairs qualified", rows.len());
    }
}
