//! Streaming, gain-insensitive acoustic features and conservative diagonal matching.
//! PCM never leaves the bounded STFT ring. No title, duration template or chapter
//! label participates in a repeated-audio boundary.
use anyhow::{Result, ensure};
use serde::{Deserialize, Serialize};
use std::{
    collections::{BTreeMap, HashMap, HashSet, VecDeque},
    time::{Duration, Instant},
};

pub(super) const RATE: usize = 8_000;
const FFT: usize = 1024;
const HOP: usize = 256;
pub(super) const HOP_MS: u64 = 32;
pub(super) const MAX_FRAMES: usize = 22_000;
const BANDS: usize = 32;

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
    source_rate: Option<u32>,
    source_samples: u64,
    phase: u64,
    sum: f64,
    count: usize,
    emitted: u64,
    ring: VecDeque<f64>,
    since_feature: usize,
    previous: Option<[f64; BANDS]>,
    frames: Vec<Feature>,
    processing_time: Duration,
}
impl Builder {
    pub fn new(start: u64, end: u64) -> Self {
        Self {
            start,
            end,
            anchor: None,
            source_rate: None,
            source_samples: 0,
            phase: 0,
            sum: 0.0,
            count: 0,
            emitted: 0,
            ring: VecDeque::with_capacity(FFT),
            since_feature: 0,
            previous: None,
            frames: Vec::new(),
            processing_time: Duration::ZERO,
        }
    }
    pub fn push(&mut self, pts_ms: u64, samples: &[f32], rate: u32, channels: usize) -> Result<()> {
        let started = Instant::now();
        ensure!(
            (8_000..=96_000).contains(&rate) && (1..=8).contains(&channels),
            "Unsupported analysis PCM format"
        );
        ensure!(
            samples.len().is_multiple_of(channels) && samples.len() <= 32_768,
            "Analysis PCM frame exceeds bounds"
        );
        if let Some(existing) = self.source_rate {
            ensure!(existing == rate, "Analysis audio rate changed");
        }
        let expected = self
            .anchor
            .unwrap_or(pts_ms)
            .saturating_add(self.source_samples.saturating_mul(1000) / u64::from(rate));
        if self.anchor.is_none() || expected.abs_diff(pts_ms) > 150 {
            self.anchor = Some(pts_ms);
            self.source_samples = 0;
            self.emitted = 0;
            self.phase = 0;
            self.sum = 0.0;
            self.count = 0;
            self.ring.clear();
            self.since_feature = 0;
            self.previous = None;
        }
        self.source_rate = Some(rate);
        for frame in samples.chunks_exact(channels) {
            ensure!(
                frame
                    .iter()
                    .all(|sample| sample.is_finite() && sample.abs() <= 32.0),
                "Invalid decoded PCM"
            );
            self.sum +=
                frame.iter().map(|sample| f64::from(*sample)).sum::<f64>() / channels as f64;
            self.count += 1;
            self.source_samples += 1;
            self.phase += RATE as u64;
            if self.phase >= u64::from(rate) {
                self.phase -= u64::from(rate);
                let value = self.sum / self.count as f64;
                self.sum = 0.0;
                self.count = 0;
                self.ring.push_back(value);
                self.emitted += 1;
                self.since_feature += 1;
                if self.ring.len() > FFT {
                    self.ring.pop_front();
                }
                if self.ring.len() == FFT && self.since_feature >= HOP {
                    self.since_feature = 0;
                    let time = self
                        .anchor
                        .unwrap_or(pts_ms)
                        .saturating_add((self.emitted - FFT as u64) * 1000 / RATE as u64);
                    let (bands, valid) = spectrum(&self.ring);
                    let mut bits = 0u64;
                    for band in 0..BANDS {
                        if bands[band] > bands[(band + 1) % BANDS] {
                            bits |= 1 << band;
                        }
                        if let Some(previous) = self.previous {
                            let now = bands[band] - bands[(band + 1) % BANDS];
                            let before = previous[band] - previous[(band + 1) % BANDS];
                            if now > before {
                                bits |= 1 << (band + BANDS);
                            }
                        }
                    }
                    let valid = valid && self.previous.is_some();
                    self.previous = Some(bands);
                    if time >= self.start && time.saturating_add(128) <= self.end {
                        ensure!(
                            self.frames.len() < MAX_FRAMES,
                            "Acoustic fingerprint frame budget exceeded"
                        );
                        self.frames.push(Feature {
                            t: time,
                            b: bits,
                            v: valid,
                        });
                    }
                }
            }
        }
        self.processing_time += started.elapsed();
        Ok(())
    }
    pub fn processing_time(&self) -> Duration {
        self.processing_time
    }
    pub fn finish(self) -> Window {
        Window {
            start_ms: self.start,
            end_ms: self.end,
            frames: self.frames,
        }
    }
}

fn spectrum(samples: &VecDeque<f64>) -> ([f64; BANDS], bool) {
    let mut re = [0.0f64; FFT];
    let mut im = [0.0f64; FFT];
    let mut square = 0.0;
    for (index, value) in samples.iter().enumerate() {
        square += value * value;
        re[index] =
            value * (0.5 - 0.5 * (std::f64::consts::TAU * index as f64 / (FFT - 1) as f64).cos());
    }
    let mut j = 0;
    for i in 1..FFT {
        let mut bit = FFT >> 1;
        while j & bit != 0 {
            j ^= bit;
            bit >>= 1;
        }
        j ^= bit;
        if i < j {
            re.swap(i, j);
        }
    }
    let mut width = 2;
    while width <= FFT {
        let angle = -std::f64::consts::TAU / width as f64;
        let (step_im, step_re) = angle.sin_cos();
        for block in (0..FFT).step_by(width) {
            let (mut wr, mut wi) = (1.0, 0.0);
            for k in 0..width / 2 {
                let right = block + k + width / 2;
                let tr = wr * re[right] - wi * im[right];
                let ti = wr * im[right] + wi * re[right];
                let left = block + k;
                re[right] = re[left] - tr;
                im[right] = im[left] - ti;
                re[left] += tr;
                im[left] += ti;
                let next = wr * step_re - wi * step_im;
                wi = wr * step_im + wi * step_re;
                wr = next;
            }
        }
        width *= 2;
    }
    let mut bands = [0.0; BANDS];
    for (band, energy) in bands.iter_mut().enumerate() {
        let lo = (90.0f64 * (3800.0f64 / 90.0).powf(band as f64 / BANDS as f64) * FFT as f64
            / RATE as f64)
            .floor() as usize;
        let hi = (90.0f64 * (3800.0f64 / 90.0).powf((band + 1) as f64 / BANDS as f64) * FFT as f64
            / RATE as f64)
            .ceil() as usize;
        *energy = (lo.max(1)..hi.min(FFT / 2))
            .map(|bin| re[bin] * re[bin] + im[bin] * im[bin])
            .sum::<f64>();
    }
    let total = bands.iter().sum::<f64>();
    let entropy = bands
        .iter()
        .filter(|energy| **energy > 0.0)
        .map(|energy| {
            let p = energy / total.max(1e-30);
            -p * p.log2()
        })
        .sum::<f64>();
    let valid = square / FFT as f64 > 1e-6 && entropy > 1.4;
    for band in &mut bands {
        *band = (*band / total.max(1e-30) + 1e-12).ln();
    }
    (bands, valid)
}

/// Matches many changing acoustic signatures along one measured time offset.
/// Frequent/low-information LSH buckets are discarded before offset voting.
pub(super) fn compare(input: &Window, reference: &Window) -> Vec<Match> {
    if input.frames.len() < 625 || reference.frames.len() < 625 {
        return Vec::new();
    }
    let mut buckets: HashMap<(u8, u8), Vec<usize>> = HashMap::new();
    for (index, frame) in reference
        .frames
        .iter()
        .enumerate()
        .filter(|(_, frame)| frame.v)
    {
        for part in 0..8u8 {
            buckets
                .entry((part, (frame.b >> (part * 8)) as u8))
                .or_default()
                .push(index);
        }
    }
    let mut votes: HashMap<i64, usize> = HashMap::new();
    let mut work = 0usize;
    for frame in input.frames.iter().filter(|frame| frame.v).step_by(3) {
        let mut seen = HashSet::new();
        for part in 0..8u8 {
            let Some(indices) = buckets.get(&(part, (frame.b >> (part * 8)) as u8)) else {
                continue;
            };
            if indices.len() > 96 {
                continue;
            }
            for index in indices {
                work += 1;
                if work > 12_000_000 {
                    break;
                }
                if !seen.insert(*index) {
                    continue;
                }
                let other = &reference.frames[*index];
                if (frame.b ^ other.b).count_ones() <= 12 {
                    let offset = (other.t as i64 - frame.t as i64).div_euclid(HOP_MS as i64);
                    *votes.entry(offset).or_default() += 1;
                }
            }
        }
        if work > 12_000_000 {
            break;
        }
    }
    let mut ranked = votes
        .into_iter()
        .filter(|(_, count)| *count >= 24)
        .collect::<Vec<_>>();
    ranked.sort_unstable_by_key(|entry| std::cmp::Reverse(entry.1));
    ranked.truncate(16);
    let mut matches = Vec::new();
    for (offset, _) in ranked {
        let mut distances = Vec::with_capacity(input.frames.len());
        let mut reference_index = 0usize;
        for frame in &input.frames {
            let wanted = frame.t as i64 + offset * HOP_MS as i64;
            while reference_index + 1 < reference.frames.len()
                && (reference.frames[reference_index + 1].t as i64 - wanted).abs()
                    <= (reference.frames[reference_index].t as i64 - wanted).abs()
            {
                reference_index += 1;
            }
            let other = &reference.frames[reference_index];
            let distance =
                if frame.v && other.v && (other.t as i64 - wanted).unsigned_abs() <= HOP_MS {
                    (frame.b ^ other.b).count_ones()
                } else {
                    64
                };
            distances.push(distance);
        }
        // A rolling 1s context prevents a single matching hash from starting or
        // extending a range. At most 0.5s of isolated corruption can be bridged.
        let mut smooth = Vec::with_capacity(distances.len());
        for index in 0..distances.len() {
            let start = index.saturating_sub(8);
            let end = (index + 9).min(distances.len());
            let good = distances[start..end]
                .iter()
                .filter(|distance| **distance <= 14)
                .count();
            smooth.push(good * 10 >= (end - start) * 8);
        }
        let mut begin = None;
        let mut bad = 0;
        for (index, good) in smooth
            .iter()
            .copied()
            .chain(std::iter::once(false))
            .enumerate()
        {
            if good {
                if begin.is_none() {
                    begin = Some(index);
                }
                bad = 0;
            } else if let Some(start) = begin {
                bad += 1;
                if bad > 15 || index == smooth.len() {
                    let end = index.saturating_sub(bad);
                    if end > start
                        && input.frames[end].t.saturating_sub(input.frames[start].t) >= 20_500
                    {
                        let slice = &distances[start..=end];
                        let coverage = slice.iter().filter(|distance| **distance <= 14).count()
                            as f64
                            / slice.len() as f64;
                        let mean = slice.iter().map(|distance| *distance as f64).sum::<f64>()
                            / slice.len() as f64;
                        let unique = input.frames[start..=end]
                            .iter()
                            .filter(|frame| frame.v)
                            .map(|frame| frame.b)
                            .collect::<HashSet<_>>()
                            .len();
                        if coverage >= 0.88 && mean <= 10.0 && unique >= 100 {
                            matches.push(Match {
                                start_ms: input.frames[start].t + 128,
                                end_ms: input.frames[end].t.saturating_sub(128),
                                quality: (1.0 - mean / 64.0) * 0.55 + coverage * 0.45,
                            });
                        }
                    }
                    begin = None;
                    bad = 0;
                }
            }
        }
    }
    matches.sort_by(|a, b| {
        b.quality
            .total_cmp(&a.quality)
            .then((b.end_ms - b.start_ms).cmp(&(a.end_ms - a.start_ms)))
    });
    let mut accepted: Vec<Match> = Vec::new();
    for found in matches {
        if found.end_ms - found.start_ms <= 180_000
            && !accepted
                .iter()
                .any(|other| found.start_ms < other.end_ms && found.end_ms > other.start_ms)
        {
            accepted.push(found);
        }
        if accepted.len() >= 4 {
            break;
        }
    }
    accepted
}

/// Intersections of independently measured ranges are safer than their union.
pub(super) fn consensus(a: &[Match], b: &[Match]) -> Vec<Match> {
    let mut output = BTreeMap::new();
    for left in a {
        for right in b {
            let start = left.start_ms.max(right.start_ms);
            let end = left.end_ms.min(right.end_ms);
            if end.saturating_sub(start) < 20_000 {
                continue;
            }
            let overlap = (end - start) as f64
                / (left.end_ms - left.start_ms).max(right.end_ms - right.start_ms) as f64;
            if overlap < 0.80 {
                continue;
            }
            let quality = left.quality.min(right.quality);
            if quality >= 0.90 {
                output.insert(
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
    output.into_values().collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    fn random_bits(state: &mut u64) -> u64 {
        *state ^= *state << 13;
        *state ^= *state >> 7;
        *state ^= *state << 17;
        *state
    }
    fn fixture(seed: u64, length: usize) -> Window {
        let mut state = seed;
        Window {
            start_ms: 0,
            end_ms: length as u64 * HOP_MS,
            frames: (0..length)
                .map(|i| Feature {
                    t: i as u64 * HOP_MS,
                    b: random_bits(&mut state),
                    v: true,
                })
                .collect(),
        }
    }
    #[test]
    fn repetition_requires_long_changing_coherent_content_and_two_peer_consensus() {
        let target = fixture(12345, 3000);
        let mut a = fixture(98765, 3000);
        let mut b = fixture(46543, 3000);
        for i in 500..2000 {
            a.frames[i + 100].b = target.frames[i].b ^ 0x101;
            b.frames[i - 120].b = target.frames[i].b ^ 0x404;
        }
        let first = compare(&target, &a);
        let second = compare(&target, &b);
        let both = consensus(&first, &second);
        assert_eq!(both.len(), 1);
        assert!(both[0].start_ms >= 15_500 && both[0].start_ms < 16_500);
        assert!(both[0].end_ms > 63_000 && both[0].end_ms < 64_000);
        assert!(compare(&target, &fixture(565657, 3000)).is_empty());
        let mut short = fixture(29292, 3000);
        for i in 600..1100 {
            short.frames[i + 10].b = target.frames[i].b;
        }
        assert!(compare(&target, &short).is_empty());
    }
    #[test]
    fn silence_and_constant_signatures_cannot_become_skip_ranges() {
        let window = Window {
            start_ms: 0,
            end_ms: 90_000,
            frames: (0..2800)
                .map(|i| Feature {
                    t: i * HOP_MS,
                    b: 0xabcdef0123456789,
                    v: true,
                })
                .collect(),
        };
        assert!(compare(&window, &window).is_empty());
        let mut builder = Builder::new(0, 6000);
        for packet in 0..60 {
            builder.push(packet * 100, &[0.0; 800], 8000, 1).unwrap();
        }
        assert!(builder.finish().frames.iter().all(|frame| !frame.v));
    }
    #[test]
    fn spectral_signature_ignores_gain_and_rejects_invalid_pcm() {
        let mut a = Builder::new(0, 6000);
        let mut b = Builder::new(0, 6000);
        for packet in 0..60u64 {
            let pcm = (0..800)
                .map(|i| {
                    let t = (packet * 800 + i) as f64 / 8000.0;
                    ((std::f64::consts::TAU * (200.0 + 25.0 * t.sin()) * t).sin() * 0.2
                        + (std::f64::consts::TAU * 733.0 * t).sin() * 0.15
                        + (std::f64::consts::TAU * 1823.0 * t).sin() * 0.1)
                        as f32
                })
                .collect::<Vec<_>>();
            a.push(packet * 100, &pcm, 8000, 1).unwrap();
            b.push(
                packet * 100,
                &pcm.iter().map(|v| v * 2.0).collect::<Vec<_>>(),
                8000,
                1,
            )
            .unwrap();
        }
        assert_eq!(
            a.finish().frames.iter().map(|f| f.b).collect::<Vec<_>>(),
            b.finish().frames.iter().map(|f| f.b).collect::<Vec<_>>()
        );
        assert!(Builder::new(0, 1000).push(0, &[f32::NAN], 8000, 1).is_err());
    }
}
