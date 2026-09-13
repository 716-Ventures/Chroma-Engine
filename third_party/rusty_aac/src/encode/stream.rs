// Chroma extension to rusty_aac 0.5.0 (Apache-2.0).
//! Bounded AAC-LC encoding with one-block lookahead and retained MDCT overlap.
//! Transient detection is causal (EWMA energy), so push boundaries cannot change
//! encoded output. The upstream whole-clip population detector is not used.
use super::*;

struct Block {
    channels: Vec<Vec<f32>>,
    attacks: Vec<bool>,
}

/// A single serial AAC stream. At most one complete lookahead block and one
/// partial PCM block are retained, independent of the stream's duration.
pub struct StreamingAacEncoder {
    encoder: AacEncoder,
    previous: Vec<Vec<f32>>,
    partial: Vec<Vec<f32>>,
    pending: VecDeque<Block>,
    energy: Vec<f64>,
    previous_short: Vec<bool>,
    plan: Vec<Elem>,
    input_samples: u64,
    frames: u64,
    finished: bool,
}

impl StreamingAacEncoder {
    /// Builds a stream using sine windows and the existing AAC-LC rate loop.
    pub fn new(channels: u16, sample_rate: u32, bitrate: u32) -> Result<Self> {
        let mut encoder = AacEncoder::new(AacEncoderConfig {
            bitrate_bps: bitrate,
            ..Default::default()
        });
        encoder.init(channels, sample_rate)?;
        let channels = channels as usize;
        let plan = element_plan(channels).ok_or_else(|| Error::invalid("invalid AAC channels"))?;
        Ok(Self {
            encoder,
            previous: vec![vec![0.0; FRAME_LEN]; channels],
            partial: (0..channels)
                .map(|_| Vec::with_capacity(FRAME_LEN))
                .collect(),
            pending: VecDeque::new(),
            energy: vec![0.0; channels],
            previous_short: vec![false; plan.len()],
            plan,
            input_samples: 0,
            frames: 0,
            finished: false,
        })
    }

    /// Consumes an interleaved PCM batch; never pads or flushes its boundary.
    pub fn push_pcm(&mut self, pcm: &[f32]) -> Result<Vec<EncodedPacket>> {
        let channels = self.encoder.channels;
        if self.finished || pcm.len() % channels != 0 || pcm.len() > 16 * 1024 * 1024 {
            return Err(Error::invalid(
                "invalid or oversized streaming AAC PCM batch",
            ));
        }
        if pcm.iter().any(|sample| !sample.is_finite()) {
            return Err(Error::invalid("nonfinite AAC PCM"));
        }
        self.input_samples = self
            .input_samples
            .checked_add((pcm.len() / channels) as u64)
            .ok_or_else(|| Error::invalid("AAC sample clock overflow"))?;
        let mut output = Vec::new();
        for frame in pcm.chunks_exact(channels) {
            for (samples, value) in self.partial.iter_mut().zip(frame) {
                samples.push(*value);
            }
            if self.partial[0].len() == FRAME_LEN {
                self.queue_partial();
                if self.pending.len() > 1 {
                    output.push(self.emit()?);
                }
            }
        }
        Ok(output)
    }

    fn queue_partial(&mut self) {
        let channels = self
            .partial
            .iter_mut()
            .map(|channel| {
                channel.resize(FRAME_LEN, 0.0);
                std::mem::replace(channel, Vec::with_capacity(FRAME_LEN))
            })
            .collect::<Vec<_>>();
        let attacks = channels
            .iter()
            .zip(&mut self.energy)
            .map(|(channel, average)| {
                let mut attack = false;
                for sub in channel.chunks_exact(SHORT_HALF) {
                    let energy = sub.iter().map(|x| f64::from(*x).powi(2)).sum::<f64>();
                    if *average > 1e-3 && energy > 10.0 * *average {
                        attack = true;
                    }
                    *average = 0.75 * *average + 0.25 * energy;
                }
                // The initial frame cannot be short: it has no preceding LongStart.
                attack && (self.frames != 0 || !self.pending.is_empty())
            })
            .collect();
        self.pending.push_back(Block { channels, attacks });
    }

    fn emit(&mut self) -> Result<EncodedPacket> {
        use WindowSequence::*;
        let block = self
            .pending
            .pop_front()
            .ok_or_else(|| Error::invalid("empty AAC queue"))?;
        let mut sequences = Vec::with_capacity(self.plan.len());
        for (index, element) in self.plan.iter().enumerate() {
            let attack = |flags: &[bool]| match *element {
                Elem::Sce(ch) => flags[ch],
                Elem::Cpe(left, right) => flags[left] || flags[right],
                Elem::Lfe(_) => false,
            };
            let next_attack = self
                .pending
                .front()
                .is_some_and(|next| attack(&next.attacks));
            let short = attack(&block.attacks) || (self.previous_short[index] && next_attack);
            let sequence = if short {
                EightShort
            } else if next_attack {
                LongStart
            } else if self.previous_short[index] {
                LongStop
            } else {
                OnlyLong
            };
            sequences.push(vec![OnlyLong, sequence]);
            self.previous_short[index] = short;
        }
        for ((buffer, previous), current) in self
            .encoder
            .chans
            .iter_mut()
            .zip(&self.previous)
            .zip(&block.channels)
        {
            buffer.clear();
            buffer.extend_from_slice(previous);
            buffer.extend_from_slice(current);
        }
        let swb = swb_offsets(true, self.encoder.fs_index);
        let swb_s = swb_offsets(false, self.encoder.fs_index);
        let sine = crate::dsp::sine_window(SHORT_N);
        let frame_budget = (self.encoder.bitrate as usize * FRAME_LEN
            / self.encoder.sample_rate as usize)
            .saturating_sub(59);
        let per_channel = (frame_budget / self.encoder.channels).saturating_sub(7);
        let (data, _) = self.encoder.encode_frame(
            1,
            &swb,
            &swb_s,
            &sine,
            &sine,
            &sequences,
            &[false, false],
            per_channel,
            &self.plan,
        );
        self.previous = block.channels;
        let pts = self
            .frames
            .checked_mul(FRAME_LEN as u64)
            .and_then(|pts| i64::try_from(pts).ok())
            .ok_or_else(|| Error::invalid("AAC timestamp overflow"))?;
        self.frames += 1;
        Ok(EncodedPacket {
            data,
            pts,
            duration: FRAME_LEN as u32,
        })
    }

    /// Pads the final partial block and flushes overlap exactly once at EOS.
    pub fn finish(&mut self) -> Result<Vec<EncodedPacket>> {
        if self.finished {
            return Ok(Vec::new());
        }
        self.finished = true;
        if self.input_samples == 0 {
            return Ok(Vec::new());
        }
        let mut output = Vec::new();
        if !self.partial[0].is_empty() {
            self.queue_partial();
            if self.pending.len() > 1 {
                output.push(self.emit()?);
            }
        }
        self.pending.push_back(Block {
            channels: vec![vec![0.0; FRAME_LEN]; self.encoder.channels],
            attacks: vec![false; self.encoder.channels],
        });
        while !self.pending.is_empty() {
            output.push(self.emit()?);
        }
        Ok(output)
    }

    /// MDCT priming present once per stream, not once per push.
    pub const fn delay_samples(&self) -> u32 {
        FRAME_LEN as u32
    }
    /// Valid input samples per channel, excluding final padding and priming.
    pub fn valid_samples(&self) -> u64 {
        self.input_samples
    }
}
