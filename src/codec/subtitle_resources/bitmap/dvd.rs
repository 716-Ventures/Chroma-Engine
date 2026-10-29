//! DVD subpicture units and VobSub IDX/PES framing. All offsets stay within
//! the supplied subtitle bytes; no authored filename is opened by this module.
//! Format reference: <https://dvd.sourceforge.net/dvdinfo/spu.html>.
use super::{BitmapRect, DisplaySet, MAX_ENCODED_BYTES, MAX_RGBA_BYTES, checked_rgba_len};
use anyhow::{Context, Result, bail};
use std::collections::HashSet;

const MAX_SPU: usize = 65_535;

#[derive(Debug, Clone)]
pub(super) struct VobSubEntry {
    pub time_ms: u64,
    pub file_position: u64,
}

#[derive(Debug, Clone)]
pub(super) struct VobSubTrack {
    pub index: u8,
    pub language: String,
    pub entries: Vec<VobSubEntry>,
}

#[derive(Debug, Clone)]
pub(super) struct VobSubIndex {
    pub width: u32,
    pub height: u32,
    pub palette: [[u8; 3]; 16],
    pub tracks: Vec<VobSubTrack>,
}

/// Parse the palette, canvas, languages and byte positions in a VobSub index.
pub(super) fn parse_idx(text: &str) -> Result<VobSubIndex> {
    if text.len() > MAX_ENCODED_BYTES {
        bail!("VobSub index exceeds limit");
    }
    let mut size = None;
    let mut palette = None;
    let mut tracks: Vec<VobSubTrack> = Vec::new();
    let mut delay: i64 = 0;
    let mut entries = 0;
    for line in text.lines().map(str::trim) {
        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        if let Some(value) = line.strip_prefix("size:") {
            let (w, h) = value
                .trim()
                .split_once('x')
                .context("Invalid VobSub canvas")?;
            let (w, h) = (w.trim().parse::<u32>()?, h.trim().parse::<u32>()?);
            checked_rgba_len(w, h)?;
            size = Some((w, h));
        } else if let Some(value) = line.strip_prefix("palette:") {
            let values = value.split(',').map(str::trim).collect::<Vec<_>>();
            if values.len() != 16 {
                bail!("VobSub palette needs sixteen colors");
            }
            let mut colors = [[0; 3]; 16];
            for (out, value) in colors.iter_mut().zip(values) {
                if value.len() != 6 {
                    bail!("Invalid VobSub palette color");
                }
                let rgb = u32::from_str_radix(value, 16)?;
                *out = [(rgb >> 16) as u8, (rgb >> 8) as u8, rgb as u8];
            }
            palette = Some(colors);
        } else if let Some(value) = line.strip_prefix("delay:") {
            let value = value.trim();
            let negative = value.starts_with('-');
            let ms = parse_time(value.trim_start_matches(['-', '+']))?;
            delay = i64::try_from(ms)? * if negative { -1 } else { 1 };
        } else if let Some(value) = line.strip_prefix("id:") {
            let (language, index) = value.split_once(',').context("Invalid VobSub language")?;
            let index = index
                .trim()
                .strip_prefix("index:")
                .context("Invalid VobSub stream index")?
                .trim()
                .parse::<u8>()?;
            if index > 31 || tracks.len() >= 32 || tracks.iter().any(|t| t.index == index) {
                bail!("Invalid or repeated VobSub stream");
            }
            let language = language.trim();
            if language.is_empty()
                || language.len() > 16
                || !language
                    .bytes()
                    .all(|v| v.is_ascii_alphanumeric() || v == b'-')
            {
                bail!("Invalid VobSub language tag");
            }
            tracks.push(VobSubTrack {
                index,
                language: language.into(),
                entries: Vec::new(),
            });
        } else if let Some(value) = line.strip_prefix("timestamp:") {
            let (time, position) = value.split_once(',').context("Invalid VobSub timestamp")?;
            let time = i128::from(parse_time(time.trim())?) + i128::from(delay);
            let time_ms = u64::try_from(time.max(0))?;
            let file_position = u64::from_str_radix(
                position
                    .trim()
                    .strip_prefix("filepos:")
                    .context("Missing VobSub byte position")?
                    .trim(),
                16,
            )?;
            let track = tracks
                .last_mut()
                .context("VobSub timestamp before language")?;
            if track.entries.last().is_some_and(|previous| {
                previous.time_ms > time_ms || previous.file_position >= file_position
            }) {
                bail!("Nonmonotonic VobSub index");
            }
            entries += 1;
            if entries > 100_000 {
                bail!("VobSub cue count exceeds limit");
            }
            track.entries.push(VobSubEntry {
                time_ms,
                file_position,
            });
        }
    }
    let (width, height) = size.context("VobSub canvas missing")?;
    Ok(VobSubIndex {
        width,
        height,
        palette: palette.context("VobSub palette missing")?,
        tracks,
    })
}

fn parse_time(value: &str) -> Result<u64> {
    let values = value
        .split(':')
        .map(str::parse::<u64>)
        .collect::<std::result::Result<Vec<_>, _>>()?;
    if values.len() != 4 || values[1] >= 60 || values[2] >= 60 || values[3] >= 1000 {
        bail!("Invalid VobSub clock");
    }
    values[0]
        .checked_mul(3_600_000)
        .and_then(|v| v.checked_add(values[1] * 60_000 + values[2] * 1000 + values[3]))
        .context("VobSub clock overflow")
}

/// Reassemble one SPU from private-stream-1 PES packets beginning at an IDX
/// file position. At most 8 MiB is scanned and only the requested stream is read.
pub(super) fn extract_spu(bytes: &[u8], position: u64, stream: u8) -> Result<Vec<u8>> {
    if stream > 31 {
        bail!("Invalid VobSub stream index");
    }
    let start = usize::try_from(position)?;
    if start >= bytes.len() {
        bail!("VobSub position outside source");
    }
    let end = start.saturating_add(MAX_ENCODED_BYTES).min(bytes.len());
    let mut cursor = start;
    let mut output = Vec::new();
    let mut expected = None;
    let mut visits = 0;
    while cursor + 4 <= end {
        visits += 1;
        if visits > 100_000 {
            bail!("VobSub PES work limit exceeded");
        }
        if bytes[cursor..cursor + 3] != [0, 0, 1] {
            let scan_end = cursor.saturating_add(2048).min(end);
            let relative = bytes[cursor..scan_end]
                .windows(3)
                .position(|v| v == [0, 0, 1])
                .context("Invalid VobSub pack framing")?;
            if relative == 0 {
                bail!("Invalid VobSub framing");
            }
            cursor += relative;
            continue;
        }
        let kind = bytes[cursor + 3];
        if kind == 0xb9 {
            break;
        }
        if kind == 0xba {
            let head = bytes.get(cursor + 4).context("Truncated VobSub pack")?;
            let len = if head & 0xc0 == 0x40 {
                14 + usize::from(*bytes.get(cursor + 13).context("Truncated VobSub pack")? & 7)
            } else if head & 0xf0 == 0x20 {
                12
            } else {
                bail!("Invalid VobSub pack header")
            };
            cursor = cursor
                .checked_add(len)
                .filter(|v| *v <= end)
                .context("Truncated VobSub pack")?;
            continue;
        }
        if cursor + 6 > end {
            bail!("Truncated VobSub PES header");
        }
        let len = usize::from(u16::from_be_bytes([bytes[cursor + 4], bytes[cursor + 5]]));
        let next = cursor
            .checked_add(6 + len)
            .filter(|v| *v <= end)
            .context("VobSub PES outside source")?;
        if kind == 0xbd {
            let packet = &bytes[cursor + 6..next];
            let mut offset = 0;
            if packet.first().is_some_and(|v| v & 0xc0 == 0x80) {
                if packet.len() < 3 {
                    bail!("Truncated VobSub PES flags");
                }
                offset = 3 + usize::from(packet[2]);
            } else {
                while packet.get(offset) == Some(&0xff) {
                    offset += 1;
                }
                if packet.get(offset).is_some_and(|v| v & 0xc0 == 0x40) {
                    offset += 2;
                }
                offset += match packet.get(offset).map(|v| v >> 4) {
                    Some(2) => 5,
                    Some(3) => 10,
                    Some(0) if packet.get(offset) == Some(&0x0f) => 1,
                    _ => bail!("Invalid VobSub MPEG-1 PES header"),
                };
            }
            let id = *packet.get(offset).context("VobSub PES payload missing")?;
            if id == 0x20 + stream {
                let payload = &packet[offset + 1..];
                if output
                    .len()
                    .checked_add(payload.len())
                    .is_none_or(|n| n > MAX_SPU + 2048)
                {
                    bail!("VobSub SPU exceeds limit");
                }
                output.extend_from_slice(payload);
                if expected.is_none() && output.len() >= 2 {
                    expected = Some(usize::from(u16::from_be_bytes([output[0], output[1]])));
                }
                if let Some(len) = expected {
                    if len < 4 {
                        bail!("Invalid VobSub SPU size");
                    }
                    if output.len() >= len {
                        output.truncate(len);
                        return Ok(output);
                    }
                }
            }
        }
        cursor = next;
    }
    bail!("Incomplete VobSub SPU")
}

pub(super) struct DvdDecoder {
    width: u32,
    height: u32,
    palette: [[u8; 3]; 16],
    pending: Vec<DisplaySet>,
    last_packet_time: Option<u64>,
}

impl DvdDecoder {
    pub(super) fn retained_bytes(&self) -> usize {
        std::mem::size_of::<Self>()
            .saturating_add(
                self.pending
                    .capacity()
                    .saturating_mul(std::mem::size_of::<DisplaySet>()),
            )
            .saturating_add(
                self.pending
                    .iter()
                    .map(|frame| {
                        frame
                            .images
                            .capacity()
                            .saturating_mul(std::mem::size_of::<BitmapRect>())
                            .saturating_add(
                                frame
                                    .images
                                    .iter()
                                    .map(|image| image.rgba.capacity())
                                    .sum::<usize>(),
                            )
                    })
                    .sum::<usize>(),
            )
    }
    pub(super) fn new(private: &[u8]) -> Result<Self> {
        let text = std::str::from_utf8(private).context("Invalid VobSub private metadata")?;
        let index = parse_idx(text)?;
        Ok(Self::with_palette(index.width, index.height, index.palette))
    }
    pub(super) fn with_palette(width: u32, height: u32, palette: [[u8; 3]; 16]) -> Self {
        Self {
            width,
            height,
            palette,
            pending: Vec::new(),
            last_packet_time: None,
        }
    }
    pub(super) fn push_packet(&mut self, bytes: &[u8], time_ms: u64) -> Result<Vec<DisplaySet>> {
        self.push_packet_timed(bytes, time_ms, None)
    }
    pub(super) fn push_packet_timed(
        &mut self,
        bytes: &[u8],
        time_ms: u64,
        duration_ms: Option<u64>,
    ) -> Result<Vec<DisplaySet>> {
        if self.last_packet_time.is_some_and(|last| last > time_ms) {
            bail!("Nonmonotonic DVD packet times");
        }
        self.last_packet_time = Some(time_ms);
        let mut frames = self.decode(bytes, time_ms, duration_ms)?;
        if let Some(start) = frames
            .iter()
            .find(|frame| !frame.images.is_empty())
            .map(|frame| frame.time_ms)
        {
            self.pending.retain(|frame| frame.time_ms < start);
        }
        self.pending
            .iter()
            .chain(&frames)
            .flat_map(|frame| &frame.images)
            .try_fold(0usize, |total, image| {
                total.checked_add(image.rgba.capacity())
            })
            .filter(|total| *total <= MAX_RGBA_BYTES)
            .context("DVD pending displays exceed memory limit")?;
        self.pending.append(&mut frames);
        self.pending.sort_by_key(|frame| frame.time_ms);
        let split = self
            .pending
            .partition_point(|frame| frame.time_ms <= time_ms);
        Ok(self.pending.drain(..split).collect())
    }
    pub(super) fn finish(&mut self) -> Vec<DisplaySet> {
        std::mem::take(&mut self.pending)
    }
    pub(super) fn decode(
        &mut self,
        bytes: &[u8],
        time_ms: u64,
        duration_ms: Option<u64>,
    ) -> Result<Vec<DisplaySet>> {
        checked_rgba_len(self.width, self.height)?;
        if bytes.len() < 4 || bytes.len() > MAX_SPU {
            bail!("Invalid DVD subpicture size");
        }
        let size = usize::from(u16::from_be_bytes([bytes[0], bytes[1]]));
        if size != bytes.len() {
            bail!("DVD subpicture length mismatch");
        }
        let controls = usize::from(u16::from_be_bytes([bytes[2], bytes[3]]));
        if controls < 4 || controls + 4 > size {
            bail!("DVD control table outside subpicture");
        }
        let mut cursor = controls;
        let mut seen = HashSet::new();
        let mut colors = [0, 1, 2, 3];
        let mut alpha = [0, 15, 15, 15];
        let mut area = None;
        let mut offsets = None;
        let mut active = false;
        let mut output = Vec::new();
        let mut last_time = 0;
        let mut retained = 0usize;
        loop {
            if !seen.insert(cursor) || seen.len() > 4096 {
                bail!("Cyclic or excessive DVD control table");
            }
            let header = bytes
                .get(cursor..cursor + 4)
                .context("Truncated DVD control sequence")?;
            let date = u64::from(u16::from_be_bytes([header[0], header[1]])) * 1024 / 90;
            if date < last_time {
                bail!("Nonmonotonic DVD display times");
            }
            last_time = date;
            let next = usize::from(u16::from_be_bytes([header[2], header[3]]));
            let mut at = cursor + 4;
            let mut changed = false;
            loop {
                let command = *bytes.get(at).context("Unterminated DVD command sequence")?;
                at += 1;
                match command {
                    0 => {
                        active = true;
                        changed = true;
                    }
                    1 => {
                        active = true;
                        changed = true;
                    }
                    2 => {
                        active = false;
                        changed = true;
                    }
                    3 | 4 => {
                        let value = bytes
                            .get(at..at + 2)
                            .context("Truncated DVD palette command")?;
                        at += 2;
                        let word = u16::from_be_bytes([value[0], value[1]]);
                        let values = [
                            (word & 15) as u8,
                            ((word >> 4) & 15) as u8,
                            ((word >> 8) & 15) as u8,
                            ((word >> 12) & 15) as u8,
                        ];
                        if command == 3 {
                            colors = values;
                        } else {
                            alpha = values;
                        }
                        changed = true;
                    }
                    5 => {
                        let v = bytes
                            .get(at..at + 6)
                            .context("Truncated DVD display rectangle")?;
                        at += 6;
                        let x = (u32::from(v[0]) << 4) | (u32::from(v[1]) >> 4);
                        let right = ((u32::from(v[1]) & 15) << 8) | u32::from(v[2]);
                        let y = (u32::from(v[3]) << 4) | (u32::from(v[4]) >> 4);
                        let bottom = ((u32::from(v[4]) & 15) << 8) | u32::from(v[5]);
                        if right < x || bottom < y || right >= self.width || bottom >= self.height {
                            bail!("DVD rectangle outside canvas");
                        }
                        area = Some((x, y, right - x + 1, bottom - y + 1));
                        changed = true;
                    }
                    6 => {
                        let v = bytes
                            .get(at..at + 4)
                            .context("Truncated DVD field offsets")?;
                        at += 4;
                        let top = usize::from(u16::from_be_bytes([v[0], v[1]]));
                        let bottom = usize::from(u16::from_be_bytes([v[2], v[3]]));
                        if top < 4 || bottom < 4 || top >= controls || bottom >= controls {
                            bail!("DVD field offset outside pixel data");
                        }
                        offsets = Some((top, bottom));
                        changed = true;
                    }
                    7 => bail!("DVD per-line color changes are not supported"),
                    0xff => break,
                    _ => bail!("Unknown DVD display command"),
                }
                if at > size {
                    bail!("DVD command outside subpicture");
                }
            }
            if changed {
                let images = if active {
                    let (x, y, width, height) = area.context("DVD display rectangle missing")?;
                    let (top, bottom) = offsets.context("DVD pixel data offsets missing")?;
                    let indices = decode_fields(&bytes[..controls], top, bottom, width, height)?;
                    let rgba_len = checked_rgba_len(width, height)?;
                    retained = retained
                        .checked_add(rgba_len)
                        .filter(|n| *n <= MAX_RGBA_BYTES)
                        .context("DVD display set exceeds memory limit")?;
                    let mut rgba = Vec::with_capacity(rgba_len);
                    for code in indices {
                        let code = usize::from(code);
                        let rgb = self.palette[usize::from(colors[code])];
                        rgba.extend_from_slice(&[rgb[0], rgb[1], rgb[2], alpha[code] * 17]);
                    }
                    vec![BitmapRect {
                        x,
                        y,
                        width,
                        height,
                        rgba,
                    }]
                } else {
                    Vec::new()
                };
                output.push(DisplaySet {
                    time_ms: time_ms
                        .checked_add(date)
                        .context("DVD timestamp overflow")?,
                    canvas_width: self.width,
                    canvas_height: self.height,
                    images,
                });
            }
            if next == cursor {
                break;
            }
            if next < controls || next + 4 > size || next < at {
                bail!("DVD control pointer outside sequence");
            }
            cursor = next;
        }
        if active && let Some(duration) = duration_ms.filter(|v| *v > last_time) {
            output.push(DisplaySet {
                time_ms: time_ms
                    .checked_add(duration)
                    .context("DVD timestamp overflow")?,
                canvas_width: self.width,
                canvas_height: self.height,
                images: Vec::new(),
            });
        }
        Ok(output)
    }
}

struct Nibbles<'a> {
    data: &'a [u8],
    at: usize,
}
impl Nibbles<'_> {
    fn next(&mut self) -> Result<u8> {
        let value = *self.data.get(self.at / 2).context("Truncated DVD RLE")?;
        let value = if self.at & 1 == 0 {
            value >> 4
        } else {
            value & 15
        };
        self.at += 1;
        Ok(value)
    }
    fn align(&mut self) {
        self.at = (self.at + 1) & !1;
    }
}
fn decode_fields(
    bytes: &[u8],
    top: usize,
    bottom: usize,
    width: u32,
    height: u32,
) -> Result<Vec<u8>> {
    checked_rgba_len(width, height)?;
    let mut pixels = vec![0; usize::try_from(u64::from(width) * u64::from(height))?];
    for (parity, offset) in [(0, top), (1, bottom)] {
        let mut reader = Nibbles {
            data: bytes,
            at: offset * 2,
        };
        for row in (parity..height as usize).step_by(2) {
            let mut x = 0;
            while x < width as usize {
                let mut code = u16::from(reader.next()?);
                if code < 4 {
                    code = (code << 4) | u16::from(reader.next()?);
                    if code < 16 {
                        code = (code << 4) | u16::from(reader.next()?);
                        if code < 64 {
                            code = (code << 4) | u16::from(reader.next()?);
                        }
                    }
                }
                let run = usize::from(code >> 2);
                let run = if run == 0 { width as usize - x } else { run };
                if run > width as usize - x {
                    bail!("DVD RLE run exceeds row");
                }
                pixels[row * width as usize + x..row * width as usize + x + run]
                    .fill((code & 3) as u8);
                x += run;
            }
            reader.align();
        }
    }
    Ok(pixels)
}

#[cfg(test)]
mod tests {
    use super::*;
    fn spu() -> Vec<u8> {
        let mut b = vec![0, 0, 0, 6, 0x90, 0xa0];
        b.extend([
            0, 0, 0, 30, 3, 0x32, 0x10, 4, 0xff, 0xf0, 5, 0, 0, 1, 0, 0, 1, 6, 0, 4, 0, 5, 1, 0xff,
        ]);
        b.extend([0, 90, 0, 30, 2, 0xff]);
        let n = b.len() as u16;
        b[..2].copy_from_slice(&n.to_be_bytes());
        b
    }
    #[test]
    fn dvd_decodes_fields_colors_alpha_and_explicit_stop() {
        let mut palette = [[0; 3]; 16];
        palette[1] = [255, 255, 255];
        palette[2] = [255, 0, 0];
        let mut decoder = DvdDecoder::with_palette(720, 480, palette);
        let frames = decoder.decode(&spu(), 1000, None).unwrap();
        assert_eq!(frames.len(), 2);
        assert_eq!(frames[0].time_ms, 1000);
        assert_eq!(frames[1].time_ms, 2024);
        assert!(frames[1].images.is_empty());
        let image = &frames[0].images[0];
        assert_eq!((image.width, image.height), (2, 2));
        assert_eq!(&image.rgba[..8], &[255, 255, 255, 255, 255, 255, 255, 255]);
        assert_eq!(&image.rgba[8..], &[255, 0, 0, 255, 255, 0, 0, 255]);
    }
    #[test]
    fn dvd_rejects_truncated_commands_offsets_and_overlong_runs() {
        let mut decoder = DvdDecoder::with_palette(720, 480, [[0; 3]; 16]);
        for len in 0..spu().len() {
            assert!(decoder.push_packet(&spu()[..len], 0).is_err());
        }
        let mut data = spu();
        data[4] = 0xd0;
        assert!(decoder.push_packet(&data, 0).is_err());
        let mut data = spu();
        data[26] = 255;
        assert!(decoder.push_packet(&data, 0).is_err());
    }
    #[test]
    fn dvd_replacement_cancels_a_previous_subpicture_future_stop() {
        let mut decoder = DvdDecoder::with_palette(720, 480, [[255; 3]; 16]);
        assert_eq!(decoder.push_packet(&spu(), 1000).unwrap().len(), 1);
        assert_eq!(decoder.push_packet(&spu(), 1500).unwrap().len(), 1);
        let last = decoder.finish();
        assert_eq!(last.len(), 1);
        assert_eq!(last[0].time_ms, 2524);
        assert!(last[0].images.is_empty());
        let mut without_stop = spu();
        without_stop.truncate(30);
        without_stop[0..2].copy_from_slice(&30_u16.to_be_bytes());
        without_stop[8..10].copy_from_slice(&6_u16.to_be_bytes());
        let mut decoder = DvdDecoder::with_palette(720, 480, [[255; 3]; 16]);
        assert_eq!(
            decoder
                .push_packet_timed(&without_stop, 1000, Some(500))
                .unwrap()
                .len(),
            1
        );
        assert_eq!(decoder.finish()[0].time_ms, 1500);
    }
    #[test]
    fn dvd_short_extended_and_end_of_line_codes_keep_pixel_count() {
        for (width, bytes) in [
            (1, vec![0x50]),
            (4, vec![0x11]),
            (16, vec![0x04, 0x10]),
            (64, vec![0x01, 0x01]),
            (300, vec![0, 1]),
        ] {
            let pixels = decode_fields(&bytes, 0, 0, width, 1).unwrap();
            assert_eq!(pixels.len(), width as usize);
            assert!(pixels.iter().all(|code| *code == 1));
        }
        assert!(decode_fields(&[0x11], 0, 0, 3, 1).is_err());
        assert!(decode_fields(&[0], 0, 0, 300, 1).is_err());
    }
    #[test]
    fn idx_preserves_languages_delays_and_checked_positions() {
        let text = "size: 720x480\npalette: 000000,ffffff,ff0000,00ff00,000000,000000,000000,000000,000000,000000,000000,000000,000000,000000,000000,000000\nid: en, index: 0\ndelay: -00:00:00:100\ntimestamp: 00:00:01:000, filepos: 00000000\ntimestamp: 00:00:02:000, filepos: 00000800\nid: ja, index: 1\ntimestamp: 00:00:03:000, filepos: 00001000\n";
        let index = parse_idx(text).unwrap();
        assert_eq!((index.width, index.height), (720, 480));
        assert_eq!(index.tracks.len(), 2);
        assert_eq!(index.tracks[0].language, "en");
        assert_eq!(index.tracks[0].entries[0].time_ms, 900);
        assert_eq!(index.tracks[1].entries[0].file_position, 4096);
        assert!(parse_idx(&text.replace("00000800", "00000000")).is_err());
        assert!(parse_idx(&text.replace("720x480", "999999x999999")).is_err());
    }
    #[test]
    fn pes_reassembles_requested_stream_only_and_rejects_incomplete_spu() {
        fn pes(id: u8, payload: &[u8]) -> Vec<u8> {
            let mut p = vec![0, 0, 1, 0xbd];
            p.extend(u16::try_from(payload.len() + 4).unwrap().to_be_bytes());
            p.extend([0x80, 0, 0, id]);
            p.extend(payload);
            p
        }
        let data = spu();
        let mut ps = pes(0x21, &data);
        ps.extend(pes(0x20, &data[..8]));
        ps.extend(pes(0x20, &data[8..]));
        assert_eq!(extract_spu(&ps, 0, 0).unwrap(), data);
        assert!(extract_spu(&ps[..ps.len() - 1], 0, 0).is_err());
        assert!(extract_spu(&ps, ps.len() as u64, 0).is_err());
    }
}
