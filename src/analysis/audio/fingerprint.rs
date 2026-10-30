//! Standard Chromaprint algorithm 2 and its upstream segment matcher.
//! Only bounded fingerprints are retained; decoded PCM is streamed and discarded.
use anyhow::{Result, ensure};
use rusty_chromaprint::{Configuration, Fingerprinter, match_fingerprints};
use serde::{Deserialize, Serialize};
use std::{
    collections::{BTreeMap, HashSet},
    time::{Duration, Instant},
};

pub(super) const MAX_FRAMES: usize = 8000;
#[derive(Clone, Debug, Serialize, Deserialize)]
pub(super) struct Feature {
    pub t: u64,
    pub b: u64,
    pub v: bool,
}
#[derive(Clone, Debug, Serialize, Deserialize)]
pub(super) struct Window {
    pub start_ms: u64,
    pub end_ms: u64,
    pub frames: Vec<Feature>,
}
#[derive(Debug, Clone)]
pub(super) struct Match {
    pub start_ms: u64,
    pub end_ms: u64,
    pub quality: f64,
}

pub(super) struct Builder {
    start: u64,
    end: u64,
    anchor: Option<u64>,
    format: Option<(u32, usize)>,
    printer: Fingerprinter,
    samples: u64,
    audible: bool,
    processing_time: Duration,
}
impl Builder {
    pub fn new(start: u64, end: u64) -> Self {
        Self {
            start,
            end,
            anchor: None,
            format: None,
            printer: Fingerprinter::new(&Configuration::preset_test2()),
            samples: 0,
            audible: false,
            processing_time: Duration::ZERO,
        }
    }
    pub fn push(&mut self, pts: u64, samples: &[f32], rate: u32, channels: usize) -> Result<()> {
        let started = Instant::now();
        ensure!(
            (8000..=96000).contains(&rate) && (1..=8).contains(&channels),
            "Unsupported analysis PCM format"
        );
        ensure!(
            samples.len().is_multiple_of(channels)
                && samples.len() <= 32768
                && samples.iter().all(|v| v.is_finite() && v.abs() <= 32.0),
            "Invalid analysis PCM"
        );
        if let Some(format) = self.format {
            ensure!(format == (rate, channels), "Analysis PCM format changed");
        } else {
            self.printer.start(rate, channels as u32)?;
            self.format = Some((rate, channels));
        }
        let first = ((self.start.saturating_sub(pts) * u64::from(rate)).div_ceil(1000) as usize)
            .min(samples.len() / channels);
        let last = ((self.end.saturating_sub(pts) * u64::from(rate)).div_ceil(1000) as usize)
            .min(samples.len() / channels);
        if first < last {
            let anchor = pts + first as u64 * 1000 / u64::from(rate);
            if let Some(old) = self.anchor {
                let expected = old + self.samples * 1000 / u64::from(rate);
                ensure!(
                    expected.abs_diff(anchor) <= 150,
                    "Discontinuous analysis PCM timeline"
                );
            } else {
                self.anchor = Some(anchor);
            }
            let pcm = samples[first * channels..last * channels]
                .iter()
                .map(|v| {
                    self.audible |= v.abs() > 0.0001;
                    (v.clamp(-1.0, 1.0) * 32767.0).round() as i16
                })
                .collect::<Vec<_>>();
            self.printer.consume(&pcm);
            self.samples += (last - first) as u64;
            ensure!(
                self.printer.fingerprint().len() <= MAX_FRAMES,
                "Chromaprint frame budget exceeded"
            );
        }
        self.processing_time += started.elapsed();
        Ok(())
    }
    pub fn processing_time(&self) -> Duration {
        self.processing_time
    }
    pub fn finish(mut self) -> Window {
        self.printer.finish();
        let config = Configuration::preset_test2();
        let anchor = self.anchor.unwrap_or(self.start);
        let frames = self
            .printer
            .fingerprint()
            .iter()
            .take(MAX_FRAMES)
            .enumerate()
            .map(|(i, b)| Feature {
                t: anchor
                    + (i as f64 * f64::from(config.item_duration_in_seconds()) * 1000.0).round()
                        as u64,
                b: u64::from(*b),
                v: self.audible,
            })
            .collect();
        Window {
            start_ms: self.start,
            end_ms: self.end,
            frames,
        }
    }
}

pub(super) fn compare(input: &Window, reference: &Window) -> Vec<Match> {
    // Silence and constant tones are not repeated-program evidence. Reject them
    // before the upstream alignment histogram, which is quadratic for constants.
    let eligible = |w: &Window| {
        w.frames.len() <= MAX_FRAMES
            && w.frames
                .iter()
                .filter(|f| f.v)
                .map(|f| f.b)
                .collect::<HashSet<_>>()
                .len()
                >= 32
    };
    if !eligible(input) || !eligible(reference) {
        return Vec::new();
    }
    let a = input.frames.iter().map(|f| f.b as u32).collect::<Vec<_>>();
    let b = reference
        .frames
        .iter()
        .map(|f| f.b as u32)
        .collect::<Vec<_>>();
    let Ok(segments) = match_fingerprints(&a, &b, &Configuration::preset_test2()) else {
        return Vec::new();
    };
    segments
        .into_iter()
        .filter_map(|s| {
            let first = input.frames.get(s.offset1)?;
            let last = input
                .frames
                .get(s.offset1.checked_add(s.items_count)?.checked_sub(1)?)?;
            let end = last.t.min(input.end_ms);
            if !(15000..=300000).contains(&end.saturating_sub(first.t))
                || !s.score.is_finite()
                || s.score > 3.2
            {
                return None;
            }
            Some(Match {
                start_ms: first.t,
                end_ms: end,
                quality: 1.0 - s.score / 32.0,
            })
        })
        .collect()
}
/// Independent peer agreement; never expand a range into unmatched material.
pub(super) fn consensus(a: &[Match], b: &[Match]) -> Vec<Match> {
    let mut out = BTreeMap::new();
    for left in a {
        for right in b {
            let start = left.start_ms.max(right.start_ms);
            let end = left.end_ms.min(right.end_ms);
            if end.saturating_sub(start) < 15000 {
                continue;
            }
            let overlap = (end - start) as f64
                / (left.end_ms - left.start_ms).max(right.end_ms - right.start_ms) as f64;
            let quality = left.quality.min(right.quality);
            if overlap >= 0.8 && quality >= 0.9 {
                out.insert(
                    (start, end),
                    Match {
                        start_ms: start,
                        end_ms: end,
                        quality,
                    },
                );
            }
        }
    }
    out.into_values().collect()
}
#[cfg(test)]
mod tests {
    use super::*;
    fn sequence(seed: u64) -> Window {
        let mut state = seed;
        Window {
            start_ms: 0,
            end_ms: 120000,
            frames: (0..900)
                .map(|i| {
                    state ^= state << 13;
                    state ^= state >> 7;
                    state ^= state << 17;
                    Feature {
                        t: i * 124,
                        b: u64::from(state as u32),
                        v: true,
                    }
                })
                .collect(),
        }
    }
    #[test]
    fn upstream_matcher_finds_shifted_material_and_rejects_unrelated_or_constant_audio() {
        let target = sequence(77);
        let mut peer = sequence(88);
        for i in 150..650 {
            peer.frames[i + 75].b = target.frames[i].b;
        }
        let found = compare(&target, &peer);
        assert!(!found.is_empty());
        assert!(found.iter().any(|m| m.start_ms < 25000 && m.end_ms > 70000));
        assert!(compare(&target, &sequence(99)).is_empty());
        for f in &mut peer.frames {
            f.b = 0;
        }
        assert!(compare(&target, &peer).is_empty());
    }
    #[test]
    fn fingerprint_streaming_is_independent_of_packet_boundaries_and_rejects_invalid_pcm() {
        let pcm = (0..48000 * 8)
            .map(|i| ((i as f64 * 0.051).sin() * 0.25) as f32)
            .collect::<Vec<_>>();
        fn build(pcm: &[f32], batch: usize) -> Vec<u64> {
            let mut b = Builder::new(0, 8000);
            for (i, p) in pcm.chunks(batch).enumerate() {
                b.push((i * batch) as u64 * 1000 / 48000, p, 48000, 1)
                    .unwrap();
            }
            b.finish().frames.into_iter().map(|f| f.b).collect()
        }
        assert_eq!(build(&pcm, 4800), build(&pcm, 960));
        assert!(Builder::new(0, 1000).push(0, &[f32::NAN], 8000, 1).is_err());
        let mut b = Builder::new(0, 6000);
        for i in 0..60 {
            b.push(i * 100, &[0.0; 800], 8000, 1).unwrap();
        }
        assert!(b.finish().frames.iter().all(|f| !f.v));
    }
}
