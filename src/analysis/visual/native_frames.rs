use crate::{
    packet::{PacketRange, PacketRef, packet_samples_for_range},
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
    packets: Vec<PacketRef>,
    cached_chunk: Option<usize>,
    cache: Vec<Frame>,
    decoder: Option<(BgraDecoderSession, RawVideoFormat)>,
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
        // Build the selected tail packet metadata once. Repeated tiny Matroska
        // windows used to re-read a +/-32 second cluster neighborhood per GOP.
        let start = duration
            .saturating_sub(super::search_window(duration))
            .saturating_sub(10000);
        let packets = if let Some(all) = mp4_packets {
            let first = all
                .iter()
                .enumerate()
                .filter(|(_, p)| p.keyframe && p.pts.as_millis() <= start)
                .map(|(i, _)| i)
                .next_back()
                .unwrap_or(0);
            all[first..].to_vec()
        } else {
            source
                .matroska_index()?
                .packets(&source, &["v0"], start, duration)?
                .remove(0)
                .packets
        };
        ensure!(
            packets.len() <= 100000 && packets.iter().filter(|p| p.keyframe).count() <= 4000,
            "Visual packet index exceeds analysis limit"
        );
        Ok(Self {
            source,
            codec,
            configuration,
            format,
            duration,
            packets,
            cached_chunk: None,
            cache: Vec::new(),
            decoder: None,
            decoded: 0,
            calls: 0,
        })
    }
    pub(super) fn duration_ms(&self) -> u64 {
        self.duration
    }
    pub(super) fn finish(&self) -> Result<()> {
        self.source.check_work()?;
        self.source.validate_current()?;
        let (bytes, reads, clusters) = self.source.io_stats();
        eprintln!(
            "Visual evidence: decoded={} calls={} read_bytes={bytes} reads={reads} clusters={clusters}",
            self.decoded, self.calls
        );
        Ok(())
    }
    pub(super) fn keyframe_times(&self) -> Vec<u64> {
        self.packets
            .iter()
            .filter(|p| p.keyframe)
            .map(|p| p.pts.as_millis())
            .collect()
    }
    pub(super) fn keyframe(&mut self, time_ms: u64) -> Result<Option<Frame>> {
        let Some(packet) = self
            .packets
            .iter()
            .find(|p| p.keyframe && p.pts.as_millis() == time_ms)
            .cloned()
        else {
            return Ok(None);
        };
        Ok(self
            .decode(&[packet])?
            .into_iter()
            .min_by_key(|f| f.time_ms.abs_diff(time_ms)))
    }
    pub(super) fn frame(&mut self, time_ms: u64) -> Result<Option<&Frame>> {
        let Some(first) = self
            .packets
            .iter()
            .enumerate()
            .filter(|(_, p)| p.keyframe && p.pts.as_millis() <= time_ms)
            .map(|(i, _)| i)
            .next_back()
        else {
            return Ok(None);
        };
        if self.cached_chunk != Some(first) {
            let end = self.packets[first + 1..]
                .iter()
                .position(|p| p.keyframe)
                .map(|i| first + 1 + i)
                .unwrap_or(self.packets.len());
            let packets = self.packets[first..end].to_vec();
            ensure!(
                packets.len() <= 1200
                    && packets.last().is_none_or(|p| p
                        .pts
                        .as_millis()
                        .abs_diff(packets[0].pts.as_millis())
                        <= 10000),
                "Video random-access interval exceeds analysis limit"
            );
            self.cache = self.decode(&packets)?;
            self.cache.sort_by_key(|f| f.time_ms);
            self.cached_chunk = Some(first);
        }
        Ok(self
            .cache
            .iter()
            .min_by_key(|f| f.time_ms.abs_diff(time_ms))
            .filter(|f| f.time_ms.abs_diff(time_ms) <= 200))
    }
    fn decode(&mut self, packets: &[PacketRef]) -> Result<Vec<Frame>> {
        self.source.check_work()?;
        self.calls += 1;
        ensure!(
            self.calls <= 4500
                && self.decoded <= 60000
                && self.source.io_stats().0 <= 2 * 1024 * 1024 * 1024,
            "Visual analysis work budget exceeded"
        );
        let compressed = packets
            .iter()
            .try_fold(0usize, |n, p| n.checked_add(p.size as usize))
            .context("Video packet size overflow")?;
        ensure!(
            compressed <= 32 * 1024 * 1024,
            "Video analysis window exceeds byte limit"
        );
        let scale = (640.0 / f64::from(self.format.width))
            .min(360.0 / f64::from(self.format.height))
            .min(1.0);
        let preferred = RawVideoFormat {
            width: ((f64::from(self.format.width) * scale).round() as u32).clamp(32, 640) & !1,
            height: ((f64::from(self.format.height) * scale).round() as u32).clamp(32, 360) & !1,
            ..self.format
        };
        // Refinement jumps backwards from the completed screening pass. Start
        // a clean GOP decoder; retain the session across keyframe-only probes.
        if packets.len() > 1 {
            self.decoder = None;
        }
        if self.decoder.is_none() {
            self.decoder = Some(BgraDecoderSession::new_scaled_with_policy(
                self.codec,
                self.format,
                preferred,
                &self.configuration,
                self.source.policy(),
            )?);
        }
        let (decoder, actual) = self.decoder.as_mut().unwrap();
        let frame_bytes = usize::try_from(u64::from(actual.width) * u64::from(actual.height) * 4)?;
        ensure!(
            frame_bytes <= self.source.policy().decoded_batch_bytes / 2,
            "Native frame dimensions exceed bounded analysis memory"
        );
        let batch = (self.source.policy().decoded_batch_bytes / frame_bytes.max(1)).clamp(1, 16);
        let mut frames = Vec::new();
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
            let mut input = build_video_decode_input(
                self.codec,
                group[0].pts.scale,
                Some(&self.configuration),
                &samples,
                &bytes,
                (index + 1) * batch >= packets.len(),
            )?;
            if self.codec == VideoCodec::Hevc && packets.len() > 1 {
                let length_size =
                    crate::codec::hevc::parse_hevc_decoder_config(&self.configuration)?
                        .nalu_length_size;
                // HEVC RASL pictures may reference the preceding GOP and must
                // be discarded when entering an open GOP at its CRA picture.
                let mut retained = Vec::with_capacity(input.packets.len());
                for packet in input.packets {
                    let leading = matches!(
                        crate::codec::hevc::sample_vcl_type(packet.bytes, length_size)?,
                        Some(8 | 9)
                    );
                    if !leading {
                        retained.push(packet);
                    }
                }
                input.packets = retained;
            }
            let decoded = decoder.decode(&input)?;
            let decoded_bytes = decoded
                .frames
                .iter()
                .try_fold(0usize, |n, f| n.checked_add(f.pixels.len()))
                .context("Decoded frame size overflow")?;
            ensure!(
                decoded_bytes <= self.source.policy().decoded_batch_bytes,
                "Decoded analysis batch exceeds memory limit"
            );
            self.decoded += decoded.frames.len();
            for frame in decoded.frames {
                let time = frame.pts.as_millis();
                if frames
                    .last()
                    .is_some_and(|last: &Frame| time < last.time_ms.saturating_add(100))
                {
                    continue;
                }
                ensure!(frames.len() < 128, "Analysis frame cache exceeds limit");
                frames.push(reduce(
                    &frame.pixels,
                    frame.format.width,
                    frame.format.height,
                    time,
                )?);
            }
        }
        Ok(frames)
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
