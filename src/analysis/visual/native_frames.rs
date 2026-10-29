use crate::{
    packet::{ChunkPlan, PacketRange, PacketRef, packet_samples_for_range},
    source::MediaSource,
    transcode::{
        BgraDecoderSession, RawVideoFormat, RawVideoPixelFormat, VideoCodec,
        build_video_decode_input,
    },
};
use anyhow::{Context, Result, bail, ensure};
use std::{
    path::Path,
    time::{Duration, Instant},
};

#[derive(Clone)]
pub(super) struct Frame {
    pub time_ms: u64,
    pub width: usize,
    pub height: usize,
    pub luma: Vec<u8>,
    pub color_fraction: f64,
}
pub(super) struct Sampler {
    source: MediaSource,
    codec: VideoCodec,
    configuration: Vec<u8>,
    format: RawVideoFormat,
    duration: u64,
    plan: ChunkPlan,
    mp4_packets: Option<Vec<PacketRef>>,
    cached_chunk: Option<u32>,
    cache: Vec<Frame>,
    decoded: usize,
    calls: usize,
}
fn codec(value: &str) -> Result<VideoCodec> {
    match value {
        "h264" | "avc1" => Ok(VideoCodec::H264),
        "hevc" | "h265" | "hvc1" | "hev1" => Ok(VideoCodec::Hevc),
        "av1" => Ok(VideoCodec::Av1),
        _ => bail!("Video codec has no bounded native credit sampler"),
    }
}
impl Sampler {
    pub(super) fn open(input: &Path) -> Result<Self> {
        let source = MediaSource::open_with_context(
            input,
            crate::EngineRuntime::global()?,
            crate::WorkControl::with_deadline(Instant::now() + Duration::from_secs(240)),
        )?;
        let (codec, configuration, width, height, rate, duration, mp4_packets) =
            if crate::container::mp4::looks_like_mp4(source.as_ref()) {
                let metadata = source.mp4_metadata_for_tracks(&["v0"])?;
                let basic = crate::container::mp4::parse_basic_metadata(&metadata);
                let track = basic
                    .tracks
                    .iter()
                    .find(|track| track.kind == crate::container::mp4::Mp4TrackKind::Video)
                    .context("Video track missing")?;
                let config = crate::container::mp4::parse_codec_config(&metadata, Some("v0"))
                    .context("Video decoder configuration missing")?;
                let hex = config
                    .description_hex
                    .context("Video decoder configuration missing")?;
                ensure!(
                    hex.len() <= 128 * 1024 && hex.len().is_multiple_of(2),
                    "Video decoder configuration exceeds limit"
                );
                let mut configuration = Vec::with_capacity(hex.len() / 2);
                for pair in hex.as_bytes().as_chunks::<2>().0 {
                    let text = std::str::from_utf8(pair)?;
                    configuration.push(u8::from_str_radix(text, 16)?);
                }
                let packets = crate::container::mp4::parse_packet_track(&metadata, Some("v0"))
                    .context("Video packet table missing")?
                    .packets;
                (
                    codec(&track.codec)?,
                    configuration,
                    track.width.unwrap_or(0),
                    track.height.unwrap_or(0),
                    track.frame_rate.unwrap_or(24.0),
                    basic.duration_ms.or(track.duration_ms).unwrap_or(0),
                    Some(packets),
                )
            } else {
                let basic = crate::container::matroska::parse_basic_metadata(source.as_ref());
                let track = basic
                    .tracks
                    .iter()
                    .find(|track| {
                        track.kind == crate::container::matroska::MatroskaTrackKind::Video
                    })
                    .context("Video track missing")?;
                (
                    codec(&track.codec)?,
                    track.codec_private.clone().unwrap_or_default(),
                    track.width.unwrap_or(0),
                    track.height.unwrap_or(0),
                    1_000_000_000.0 / track.default_duration_ns.unwrap_or(41_666_667) as f64,
                    basic.duration_ms.unwrap_or(0),
                    None,
                )
            };
        ensure!(
            width > 0
                && height > 0
                && u64::from(width) * u64::from(height) <= source.policy().frame_pixels,
            "Video dimensions exceed analysis limit"
        );
        ensure!(
            duration > 0 && duration <= 12 * 3600 * 1000,
            "Video duration exceeds analysis limit"
        );
        let format = RawVideoFormat {
            width,
            height,
            frame_rate_num: (rate.clamp(1.0, 120.0) * 1000.0).round() as u32,
            frame_rate_den: 1000,
            pixel_format: RawVideoPixelFormat::Bgra,
        };
        let plan = source.chunk_plan(Some("v0"), 1000)?;
        ensure!(
            plan.chunks.len() <= 100_000,
            "Video seek index exceeds analysis limit"
        );
        Ok(Self {
            source,
            codec,
            configuration,
            format,
            duration,
            plan,
            mp4_packets,
            cached_chunk: None,
            cache: Vec::new(),
            decoded: 0,
            calls: 0,
        })
    }
    pub(super) fn duration_ms(&self) -> u64 {
        self.duration
    }
    pub(super) fn finish(&self) -> Result<()> {
        self.source.check_work()?;
        self.source.validate_current()
    }
    pub(super) fn frame(&mut self, time_ms: u64) -> Result<Option<&Frame>> {
        self.source.check_work()?;
        self.calls += 1;
        ensure!(
            self.calls <= 2000
                && self.decoded <= 60_000
                && self.source.io_stats().0 <= 2 * 1024 * 1024 * 1024,
            "Visual analysis work budget exceeded"
        );
        let Some(chunk) = self
            .plan
            .chunks
            .iter()
            .rfind(|chunk| chunk.start.as_millis() <= time_ms)
            .cloned()
        else {
            return Ok(None);
        };
        if self.cached_chunk != Some(chunk.index) {
            self.cache.clear();
            self.cached_chunk = None;
            ensure!(
                chunk.key_aligned && chunk.duration.as_millis() <= 10_000,
                "Video random-access interval exceeds analysis limit"
            );
            let packets = if let Some(packets) = &self.mp4_packets {
                packets
                    .get(chunk.packet_range.start as usize..chunk.packet_range.end as usize)
                    .context("Invalid video packet range")?
                    .to_vec()
            } else {
                self.source
                    .matroska_index()?
                    .packets(
                        &self.source,
                        &["v0"],
                        chunk.start.as_millis(),
                        chunk
                            .start
                            .as_millis()
                            .saturating_add(chunk.duration.as_millis()),
                    )?
                    .into_iter()
                    .next()
                    .context("Video packet window missing")?
                    .packets
            };
            ensure!(
                !packets.is_empty()
                    && packets.len() <= 1200
                    && packets.first().is_some_and(|p| p.keyframe),
                "Video packet window is not bounded/random-access aligned"
            );
            let compressed = packets
                .iter()
                .try_fold(0usize, |total, packet| {
                    total.checked_add(packet.size as usize)
                })
                .context("Video packet byte count overflow")?;
            ensure!(
                compressed <= 32 * 1024 * 1024,
                "Video analysis window exceeds byte limit"
            );
            let scale = (640.0 / f64::from(self.format.width))
                .min(360.0 / f64::from(self.format.height))
                .min(1.0);
            let preferred = RawVideoFormat {
                width: ((f64::from(self.format.width) * scale).round() as u32).clamp(32, 640) & !1,
                height: ((f64::from(self.format.height) * scale).round() as u32).clamp(32, 360)
                    & !1,
                ..self.format
            };
            let (mut decoder, actual) = BgraDecoderSession::new_scaled_with_policy(
                self.codec,
                self.format,
                preferred,
                &self.configuration,
                self.source.policy(),
            )?;
            let frame_bytes =
                usize::try_from(u64::from(actual.width) * u64::from(actual.height) * 4)?;
            ensure!(
                frame_bytes <= self.source.policy().decoded_batch_bytes / 2,
                "Native frame dimensions exceed bounded analysis memory"
            );
            let batch =
                (self.source.policy().decoded_batch_bytes / frame_bytes.max(1)).clamp(1, 16);
            for (index, group) in packets.chunks(batch).enumerate() {
                self.source.check_work()?;
                let bytes = self.source.read_packets(group)?;
                let samples = packet_samples_for_range(
                    group,
                    PacketRange {
                        start: 0,
                        end: group.len() as u32,
                    },
                )?;
                let input = build_video_decode_input(
                    self.codec,
                    group[0].pts.scale,
                    Some(&self.configuration),
                    &samples,
                    &bytes,
                    (index + 1) * batch >= packets.len(),
                )?;
                let decoded = decoder.decode(&input)?;
                let decoded_bytes = decoded
                    .frames
                    .iter()
                    .try_fold(0usize, |total, frame| total.checked_add(frame.pixels.len()))
                    .context("Decoded frame count overflow")?;
                ensure!(
                    decoded_bytes <= self.source.policy().decoded_batch_bytes,
                    "Decoded analysis batch exceeds memory limit"
                );
                self.decoded += decoded.frames.len();
                for frame in decoded.frames {
                    let time = frame.pts.as_millis();
                    if self
                        .cache
                        .last()
                        .is_some_and(|last| time < last.time_ms.saturating_add(100))
                    {
                        continue;
                    }
                    ensure!(self.cache.len() < 128, "Analysis frame cache exceeds limit");
                    self.cache.push(reduce(
                        &frame.pixels,
                        frame.format.width,
                        frame.format.height,
                        time,
                    )?);
                }
            }
            self.cache.sort_by_key(|frame| frame.time_ms);
            self.cached_chunk = Some(chunk.index);
        }
        let nearest = self
            .cache
            .iter()
            .min_by_key(|frame| frame.time_ms.abs_diff(time_ms));
        Ok(nearest.filter(|frame| frame.time_ms.abs_diff(time_ms) <= 200))
    }
}
fn reduce(bytes: &[u8], width: u32, height: u32, time_ms: u64) -> Result<Frame> {
    let expected = u64::from(width)
        .checked_mul(u64::from(height))
        .and_then(|pixels| pixels.checked_mul(4))
        .context("Frame size overflow")?;
    ensure!(
        width > 0 && height > 0 && expected == bytes.len() as u64,
        "Invalid BGRA analysis frame"
    );
    let sw = width as usize;
    let sh = height as usize;
    let width = 640usize.min(sw);
    let height = (sh * width / sw).clamp(1, 360);
    let mut luma = Vec::with_capacity(width * height);
    let mut colored = 0usize;
    let mut bright = 0usize;
    for y in 0..height {
        for x in 0..width {
            let source = ((y * sh / height) * sw + x * sw / width) * 4;
            let (b, g, r) = (
                u32::from(bytes[source]),
                u32::from(bytes[source + 1]),
                u32::from(bytes[source + 2]),
            );
            let value = ((r * 54 + g * 183 + b * 19) / 256) as u8;
            if value > 100 {
                bright += 1;
                if r.max(g).max(b) - r.min(g).min(b) > 48 {
                    colored += 1;
                }
            }
            luma.push(value);
        }
    }
    Ok(Frame {
        time_ms,
        width,
        height,
        luma,
        color_fraction: colored as f64 / bright.max(1) as f64,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn reduction_is_bounded_and_rejects_malformed_frame_layouts() {
        assert!(reduce(&[], 0, 0, 0).is_err());
        assert!(reduce(&[0; 3], 1, 1, 0).is_err());
        let mut pixels = vec![0; 640 * 360 * 4];
        for pixel in pixels.as_chunks_mut::<4>().0 {
            pixel.copy_from_slice(&[255, 255, 255, 255]);
        }
        let frame = reduce(&pixels, 640, 360, 1000).unwrap();
        assert_eq!(
            (frame.width, frame.height, frame.luma.len()),
            (640, 360, 230_400)
        );
        assert!(frame.luma.iter().all(|value| *value == 255));
        assert_eq!(frame.color_fraction, 0.0);
    }
}
