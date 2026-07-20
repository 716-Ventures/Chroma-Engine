use serde::{Deserialize, Serialize};

use crate::packet::{TimeDelta, TimePoint, TimeScale};
use crate::transcode::rescale_units_rounded;

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
/// Configuration for sample-accurate audio output timing.
pub struct AudioClockConfig {
    /// Output sample rate. This is also the clock time scale.
    pub sample_rate: u32,
    /// Maximum source-timestamp drift tolerated before the clock re-anchors.
    pub discontinuity_threshold_ms: u64,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
/// Timing assigned to one encoded audio access unit.
pub struct AudioFrameTiming {
    /// Output presentation timestamp on the exact sample clock.
    pub pts: TimePoint,
    /// Output duration on the exact sample clock.
    pub duration: TimeDelta,
    /// First output sample index in this frame.
    pub start_sample: u64,
    /// Number of output samples in this frame.
    pub sample_count: u32,
    /// True when this frame started a new timing anchor.
    pub reanchored: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
/// Sample-count driven audio clock for native decode/encode paths.
///
/// Container timestamps can be rounded to coarse timebases, especially in
/// Matroska. This clock uses source PTS only to choose an initial anchor and to
/// detect real discontinuities; normal frame-to-frame timing advances by sample
/// count so encoded output buffers abut exactly.
pub struct AudioSampleClock {
    scale: TimeScale,
    discontinuity_threshold_samples: u64,
    next_sample: u64,
    anchored: bool,
}

impl Default for AudioClockConfig {
    fn default() -> Self {
        Self {
            sample_rate: 48_000,
            discontinuity_threshold_ms: 100,
        }
    }
}

impl AudioSampleClock {
    /// Creates a new sample clock.
    pub fn new(config: AudioClockConfig) -> Self {
        let sample_rate = config.sample_rate.max(1);
        let scale = TimeScale {
            units_per_second: sample_rate,
        };
        Self {
            scale,
            discontinuity_threshold_samples: rescale_units_rounded(
                config.discontinuity_threshold_ms,
                TimeScale::MILLIS,
                scale,
            ),
            next_sample: 0,
            anchored: false,
        }
    }

    /// Returns the output time scale.
    pub fn scale(&self) -> TimeScale {
        self.scale
    }

    /// Returns the next sample index that will be assigned.
    pub fn next_sample(&self) -> u64 {
        self.next_sample
    }

    /// Resets the clock to the beginning of the output timeline.
    pub fn reset(&mut self) {
        self.next_sample = 0;
        self.anchored = false;
    }

    /// Assigns timing to one decoded/encoded audio frame.
    pub fn stamp_frame(
        &mut self,
        source_pts: Option<TimePoint>,
        sample_count: u32,
    ) -> AudioFrameTiming {
        let source_sample =
            source_pts.map(|pts| rescale_units_rounded(pts.units, pts.scale, self.scale));
        let mut reanchored = false;

        if let Some(source_sample) = source_sample
            && (!self.anchored
                || sample_delta(self.next_sample, source_sample)
                    > self.discontinuity_threshold_samples)
        {
            self.next_sample = source_sample;
            self.anchored = true;
            reanchored = true;
        }

        let start_sample = self.next_sample;
        self.next_sample = self.next_sample.saturating_add(u64::from(sample_count));

        AudioFrameTiming {
            pts: TimePoint {
                units: start_sample,
                scale: self.scale,
            },
            duration: TimeDelta {
                units: u64::from(sample_count),
                scale: self.scale,
            },
            start_sample,
            sample_count,
            reanchored,
        }
    }
}

fn sample_delta(a: u64, b: u64) -> u64 {
    a.abs_diff(b)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn stamps_contiguous_frames_from_sample_count() {
        let mut clock = AudioSampleClock::new(AudioClockConfig::default());

        let first = clock.stamp_frame(Some(TimePoint::millis(0)), 1024);
        let second = clock.stamp_frame(Some(TimePoint::millis(21)), 1024);
        let third = clock.stamp_frame(Some(TimePoint::millis(43)), 1024);

        assert!(first.reanchored);
        assert_eq!(first.pts.units, 0);
        assert_eq!(second.pts.units, 1024);
        assert_eq!(third.pts.units, 2048);
        assert_eq!(third.duration.units, 1024);
        assert!(!second.reanchored);
        assert!(!third.reanchored);
    }

    #[test]
    fn ignores_container_quantization_jitter_below_threshold() {
        let mut clock = AudioSampleClock::new(AudioClockConfig {
            sample_rate: 44_100,
            discontinuity_threshold_ms: 100,
        });

        let first = clock.stamp_frame(Some(TimePoint::millis(0)), 1536);
        let second = clock.stamp_frame(Some(TimePoint::millis(35)), 1536);

        assert!(first.reanchored);
        assert!(!second.reanchored);
        assert_eq!(second.pts.units, 1536);
        assert_eq!(clock.next_sample(), 3072);
    }

    #[test]
    fn reanchors_on_real_discontinuity() {
        let mut clock = AudioSampleClock::new(AudioClockConfig {
            sample_rate: 48_000,
            discontinuity_threshold_ms: 100,
        });

        clock.stamp_frame(Some(TimePoint::millis(0)), 1024);
        let after_seek = clock.stamp_frame(Some(TimePoint::millis(5000)), 1024);

        assert!(after_seek.reanchored);
        assert_eq!(after_seek.pts.units, 240_000);
        assert_eq!(after_seek.start_sample, 240_000);
    }

    #[test]
    fn supports_clocked_output_before_source_pts_arrives() {
        let mut clock = AudioSampleClock::new(AudioClockConfig::default());

        let first = clock.stamp_frame(None, 960);
        let second = clock.stamp_frame(None, 960);

        assert_eq!(first.pts.units, 0);
        assert_eq!(second.pts.units, 960);
        assert!(!first.reanchored);
        assert!(!second.reanchored);
    }
}
