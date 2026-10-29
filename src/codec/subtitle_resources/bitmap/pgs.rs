//! Stateful HDMV presentation graphics decoding. Container timestamps are the
//! presentation clock; SUP headers carry the same clock in 90 kHz units.
use super::{BitmapRect, DisplaySet, MAX_ENCODED_BYTES, MAX_RGBA_BYTES, checked_rgba_len};
use anyhow::{Context, Result, bail};
use std::collections::BTreeMap;

#[derive(Clone)]
struct Object {
    width: u32,
    height: u32,
    expected: usize,
    data: Vec<u8>,
    complete: bool,
}

#[derive(Clone)]
struct Placement {
    object_id: u16,
    window_id: u8,
    x: u32,
    y: u32,
    crop: Option<(u32, u32, u32, u32)>,
}

#[derive(Default)]
pub(super) struct PgsDecoder {
    palettes: BTreeMap<u8, [[u8; 4]; 256]>,
    objects: BTreeMap<u16, Object>,
    placements: Vec<Placement>,
    canvas_width: u32,
    canvas_height: u32,
    palette: u8,
    time_ms: u64,
    dirty: bool,
    has_composition: bool,
    pending: Vec<u8>,
    pending_time: u64,
    windows: BTreeMap<u8, (u32, u32, u32, u32)>,
}

impl PgsDecoder {
    pub(super) fn push_packet(&mut self, bytes: &[u8], time_ms: u64) -> Result<Vec<DisplaySet>> {
        if bytes.len() > MAX_ENCODED_BYTES
            || self.pending.len().saturating_add(bytes.len()) > MAX_ENCODED_BYTES
        {
            bail!("PGS packet size limit exceeded");
        }
        let retained = self.pending.len();
        let previous_time = self.pending_time;
        self.pending.extend_from_slice(bytes);
        let mut consumed = 0;
        let mut result = Vec::new();
        let mut result_bytes = 0usize;
        while self.pending.len() - consumed >= 3 {
            let kind = self.pending[consumed];
            let len = usize::from(u16::from_be_bytes([
                self.pending[consumed + 1],
                self.pending[consumed + 2],
            ]));
            let end = consumed + 3 + len;
            if end > self.pending.len() {
                break;
            }
            // A segment is at most 64 KiB. Keep the decoder independent of the
            // container packet's lifetime and avoid retaining video payloads.
            let body = self.pending[consumed + 3..end].to_vec();
            let segment_time = if consumed < retained {
                previous_time
            } else {
                time_ms
            };
            if let Some(display) = self.segment(kind, &body, segment_time)? {
                if result.len() >= 1024 {
                    bail!("PGS display count limit exceeded");
                }
                let display_bytes = display
                    .images
                    .iter()
                    .map(|image| image.rgba.capacity())
                    .sum::<usize>();
                result_bytes = result_bytes
                    .checked_add(display_bytes)
                    .context("PGS packet display size overflow")?;
                if result_bytes > MAX_RGBA_BYTES {
                    bail!("PGS packet display budget exceeded");
                }
                result.push(display);
            }
            consumed = end;
        }
        self.pending.drain(..consumed);
        self.pending_time = if consumed < retained {
            previous_time
        } else {
            time_ms
        };
        Ok(result)
    }

    pub(super) fn finish(&self) -> Result<()> {
        if !self.pending.is_empty() {
            bail!("Truncated PGS segment");
        }
        if self.objects.values().any(|object| !object.complete) {
            bail!("Truncated PGS object");
        }
        Ok(())
    }

    pub(super) fn retained_bytes(&self) -> usize {
        self.pending
            .capacity()
            .saturating_add(
                self.objects
                    .values()
                    .map(|object| object.data.capacity() + std::mem::size_of::<Object>() + 64)
                    .sum::<usize>(),
            )
            .saturating_add(self.palettes.len() * (256 * 4 + 64))
            .saturating_add(self.placements.capacity() * std::mem::size_of::<Placement>())
            .saturating_add(self.windows.len() * 96)
    }

    fn segment(&mut self, kind: u8, body: &[u8], time_ms: u64) -> Result<Option<DisplaySet>> {
        match kind {
            0x14 => self.palette(body)?,
            0x15 => self.object(body)?,
            0x16 => self.composition(body, time_ms)?,
            0x17 => self.windows(body)?,
            0x80 => {
                if !body.is_empty() {
                    bail!("Invalid PGS end segment");
                }
                if self.has_composition && self.dirty {
                    self.dirty = false;
                    return Ok(Some(self.display()?));
                }
            }
            _ => bail!("Unsupported PGS segment type"),
        }
        Ok(None)
    }

    fn palette(&mut self, body: &[u8]) -> Result<()> {
        if body.len() < 2 || !(body.len() - 2).is_multiple_of(5) {
            bail!("Invalid PGS palette segment");
        }
        let id = body[0];
        if !self.palettes.contains_key(&id) && self.palettes.len() >= 16 {
            bail!("PGS palette count limit exceeded");
        }
        let palette = self.palettes.entry(id).or_insert([[0; 4]; 256]);
        for entry in body[2..].as_chunks::<5>().0 {
            palette[usize::from(entry[0])] = ycbcr(entry[1], entry[2], entry[3], entry[4]);
        }
        self.dirty = true;
        Ok(())
    }

    fn object(&mut self, body: &[u8]) -> Result<()> {
        if body.len() < 4 {
            bail!("Truncated PGS object header");
        }
        let id = be16(body, 0)?;
        let sequence = body[3];
        if sequence & 0x3f != 0 {
            bail!("Invalid PGS object sequence flags");
        }
        let first = sequence & 0x80 != 0;
        let last = sequence & 0x40 != 0;
        let offset = if first {
            if body.len() < 11 {
                bail!("Truncated first PGS object fragment");
            }
            let declared =
                (usize::from(body[4]) << 16) | (usize::from(body[5]) << 8) | usize::from(body[6]);
            let expected = declared
                .checked_sub(4)
                .context("Invalid PGS object length")?;
            if expected > MAX_ENCODED_BYTES {
                bail!("PGS object size limit exceeded");
            }
            let width = u32::from(be16(body, 7)?);
            let height = u32::from(be16(body, 9)?);
            checked_rgba_len(width, height)?;
            if !self.objects.contains_key(&id) && self.objects.len() >= 256 {
                bail!("PGS object count limit exceeded");
            }
            let retained: usize = self
                .objects
                .iter()
                .filter(|(key, _)| **key != id)
                .map(|(_, object)| object.data.len())
                .sum();
            if retained.saturating_add(expected) > MAX_RGBA_BYTES {
                bail!("PGS retained object budget exceeded");
            }
            self.objects.insert(
                id,
                Object {
                    width,
                    height,
                    expected,
                    data: Vec::new(),
                    complete: false,
                },
            );
            11
        } else {
            4
        };
        let total = self
            .objects
            .values()
            .map(|object| object.data.len())
            .sum::<usize>();
        if total.saturating_add(body.len() - offset) > MAX_RGBA_BYTES {
            bail!("PGS retained object budget exceeded");
        }
        let object = self
            .objects
            .get_mut(&id)
            .context("PGS continuation without first fragment")?;
        if object.complete
            || object.data.len().saturating_add(body.len() - offset) > object.expected
        {
            bail!("PGS object fragment exceeds declared length");
        }
        object.data.extend_from_slice(&body[offset..]);
        if last {
            if object.data.len() != object.expected {
                bail!("Incomplete final PGS object fragment");
            }
            object.complete = true;
        }
        self.dirty = true;
        Ok(())
    }

    fn composition(&mut self, body: &[u8], time_ms: u64) -> Result<()> {
        if body.len() < 11 {
            bail!("Truncated PGS composition");
        }
        let width = u32::from(be16(body, 0)?);
        let height = u32::from(be16(body, 2)?);
        checked_rgba_len(width, height)?;
        let state = body[7];
        if ![0, 0x40, 0x80].contains(&state) {
            bail!("Invalid PGS composition state");
        }
        if state == 0x80 {
            self.objects.clear();
            self.palettes.clear();
            self.windows.clear();
        }
        let count = usize::from(body[10]);
        if count > 64 {
            bail!("PGS composition object count limit exceeded");
        }
        let mut placements = Vec::with_capacity(count);
        let mut offset = 11;
        for _ in 0..count {
            if body.len() - offset < 8 {
                bail!("Truncated PGS composition object");
            }
            let object_id = be16(body, offset)?;
            let window_id = body[offset + 2];
            let flags = body[offset + 3];
            if flags & 0x3f != 0 {
                bail!("Invalid PGS composition object flags");
            }
            let x = u32::from(be16(body, offset + 4)?);
            let y = u32::from(be16(body, offset + 6)?);
            offset += 8;
            let crop = if flags & 0x80 != 0 {
                if body.len() - offset < 8 {
                    bail!("Truncated PGS crop rectangle");
                }
                let crop = (
                    u32::from(be16(body, offset)?),
                    u32::from(be16(body, offset + 2)?),
                    u32::from(be16(body, offset + 4)?),
                    u32::from(be16(body, offset + 6)?),
                );
                offset += 8;
                Some(crop)
            } else {
                None
            };
            placements.push(Placement {
                object_id,
                window_id,
                x,
                y,
                crop,
            });
        }
        if offset != body.len() {
            bail!("Trailing PGS composition data");
        }
        self.placements = placements;
        self.canvas_width = width;
        self.canvas_height = height;
        self.palette = body[9];
        self.time_ms = time_ms;
        self.has_composition = true;
        self.dirty = true;
        Ok(())
    }

    fn windows(&mut self, body: &[u8]) -> Result<()> {
        let count = usize::from(*body.first().context("Missing PGS window count")?);
        if count > 64 || body.len() != 1 + count * 9 {
            bail!("Invalid PGS windows");
        }
        let mut windows = BTreeMap::new();
        for window in body[1..].as_chunks::<9>().0 {
            let x = u32::from(be16(window, 1)?);
            let y = u32::from(be16(window, 3)?);
            let width = u32::from(be16(window, 5)?);
            let height = u32::from(be16(window, 7)?);
            if width == 0
                || height == 0
                || x.saturating_add(width) > self.canvas_width
                || y.saturating_add(height) > self.canvas_height
            {
                bail!("PGS window outside canvas");
            }
            if windows.insert(window[0], (x, y, width, height)).is_some() {
                bail!("Duplicate PGS window identifier");
            }
        }
        self.windows = windows;
        Ok(())
    }

    fn display(&self) -> Result<DisplaySet> {
        let mut images = Vec::new();
        if !self.placements.is_empty() {
            let palette = self
                .palettes
                .get(&self.palette)
                .context("PGS composition palette missing")?;
            let mut retained = 0usize;
            for placement in &self.placements {
                let object = self
                    .objects
                    .get(&placement.object_id)
                    .context("PGS composition object missing")?;
                if !object.complete {
                    bail!("PGS composition references unfinished object");
                }
                let (mut left, mut top, mut width, mut height) =
                    placement
                        .crop
                        .unwrap_or((0, 0, object.width, object.height));
                if width == 0
                    || height == 0
                    || left.saturating_add(width) > object.width
                    || top.saturating_add(height) > object.height
                {
                    bail!("PGS object rectangle outside canvas");
                }
                let (wx, wy, ww, wh) = self.windows.get(&placement.window_id).copied().unwrap_or((
                    0,
                    0,
                    self.canvas_width,
                    self.canvas_height,
                ));
                let x = placement.x.max(wx);
                let y = placement.y.max(wy);
                let right = placement
                    .x
                    .saturating_add(width)
                    .min(wx + ww)
                    .min(self.canvas_width);
                let bottom = placement
                    .y
                    .saturating_add(height)
                    .min(wy + wh)
                    .min(self.canvas_height);
                if right <= x || bottom <= y {
                    continue;
                }
                left += x - placement.x;
                top += y - placement.y;
                width = right - x;
                height = bottom - y;
                let len = checked_rgba_len(width, height)?;
                retained = retained
                    .checked_add(len)
                    .context("PGS decoded image size overflow")?;
                let index_bytes = checked_rgba_len(object.width, object.height)? / 4;
                if retained.saturating_add(index_bytes) > MAX_RGBA_BYTES {
                    bail!("PGS decoded display budget exceeded");
                }
                let indices = decode_rle(&object.data, object.width, object.height)?;
                let mut rgba = Vec::new();
                rgba.try_reserve_exact(len)?;
                for row in top..top + height {
                    let start = (row as usize) * object.width as usize + left as usize;
                    for index in &indices[start..start + width as usize] {
                        rgba.extend_from_slice(&palette[usize::from(*index)]);
                    }
                }
                images.push(BitmapRect {
                    x,
                    y,
                    width,
                    height,
                    rgba,
                });
            }
        }
        Ok(DisplaySet {
            time_ms: self.time_ms,
            canvas_width: self.canvas_width,
            canvas_height: self.canvas_height,
            images,
        })
    }
}

fn be16(bytes: &[u8], offset: usize) -> Result<u16> {
    let pair = bytes
        .get(offset..offset + 2)
        .context("Truncated PGS integer")?;
    Ok(u16::from_be_bytes([pair[0], pair[1]]))
}

fn ycbcr(y: u8, cr: u8, cb: u8, alpha: u8) -> [u8; 4] {
    // HDMV uses studio-range BT.709 YCbCr. Retain straight alpha for PNG.
    let y = 1.164_383 * (f64::from(y) - 16.0);
    let cr = f64::from(cr) - 128.0;
    let cb = f64::from(cb) - 128.0;
    let byte = |value: f64| value.round().clamp(0.0, 255.0) as u8;
    [
        byte(y + 1.792_741 * cr),
        byte(y - 0.213_249 * cb - 0.532_909 * cr),
        byte(y + 2.112_402 * cb),
        alpha,
    ]
}

fn decode_rle(bytes: &[u8], width: u32, height: u32) -> Result<Vec<u8>> {
    let pixels = checked_rgba_len(width, height)? / 4;
    let mut output = Vec::new();
    output.try_reserve_exact(pixels)?;
    output.resize(pixels, 0);
    let mut cursor = 0;
    let mut x = 0usize;
    let mut row = 0usize;
    while cursor < bytes.len() {
        if row >= height as usize {
            bail!("PGS RLE data exceeds image height");
        }
        let value = bytes[cursor];
        cursor += 1;
        if value != 0 {
            if x >= width as usize {
                bail!("PGS RLE line exceeds image width");
            }
            output[row * width as usize + x] = value;
            x += 1;
            continue;
        }
        let flags = *bytes.get(cursor).context("Truncated PGS RLE escape")?;
        cursor += 1;
        if flags == 0 {
            // Unspecified remainder keeps palette index zero, including a
            // completely empty scan line; its alpha comes from that palette.
            row += 1;
            x = 0;
            continue;
        }
        let mut count = usize::from(flags & 0x3f);
        if flags & 0x40 != 0 {
            count =
                (count << 8) | usize::from(*bytes.get(cursor).context("Truncated PGS long run")?);
            cursor += 1;
        }
        if count == 0 || count > width as usize - x {
            bail!("PGS RLE run exceeds scan line");
        }
        let color = if flags & 0x80 != 0 {
            let color = *bytes.get(cursor).context("Truncated PGS colored run")?;
            cursor += 1;
            color
        } else {
            0
        };
        output[row * width as usize + x..row * width as usize + x + count].fill(color);
        x += count;
    }
    if row != height as usize && !(row + 1 == height as usize && x == width as usize) {
        bail!("Incomplete PGS RLE image");
    }
    Ok(output)
}

#[cfg(test)]
mod tests {
    use super::*;
    fn segment(kind: u8, data: &[u8]) -> Vec<u8> {
        let mut out = vec![kind];
        out.extend((data.len() as u16).to_be_bytes());
        out.extend(data);
        out
    }
    fn pcs(count: u8) -> Vec<u8> {
        let mut body = vec![0, 4, 0, 3, 0x10, 0, 1, 0x80, 0, 0, count];
        if count != 0 {
            body.extend([0, 1, 0, 0, 0, 1, 0, 1]);
        }
        segment(0x16, &body)
    }
    fn palette() -> Vec<u8> {
        segment(0x14, &[0, 0, 0, 16, 128, 128, 0, 1, 235, 128, 128, 255])
    }
    fn object(first: bool, last: bool, rle: &[u8]) -> Vec<u8> {
        let mut body = vec![
            0,
            1,
            0,
            (if first { 0x80 } else { 0 }) | (if last { 0x40 } else { 0 }),
        ];
        if first {
            body.extend([0, 0, 8, 0, 2, 0, 1]);
        }
        body.extend(rle);
        segment(0x15, &body)
    }
    #[test]
    fn fragmented_pgs_object_is_composed_and_a_later_empty_composition_clears() {
        let mut decoder = PgsDecoder::default();
        decoder.push_packet(&pcs(1), 1000).unwrap();
        decoder.push_packet(&palette(), 1000).unwrap();
        decoder
            .push_packet(&object(true, false, &[1]), 1000)
            .unwrap();
        decoder
            .push_packet(&object(false, true, &[1, 0, 0]), 1000)
            .unwrap();
        let displays = decoder.push_packet(&segment(0x80, &[]), 1000).unwrap();
        assert_eq!(displays.len(), 1);
        let image = &displays[0].images[0];
        assert_eq!((image.x, image.y, image.width, image.height), (1, 1, 2, 1));
        assert_eq!(image.rgba, [255, 255, 255, 255, 255, 255, 255, 255]);
        decoder.push_packet(&pcs(0), 2500).unwrap();
        assert!(
            decoder.push_packet(&segment(0x80, &[]), 2500).unwrap()[0]
                .images
                .is_empty()
        );
        decoder.finish().unwrap();
    }
    #[test]
    fn pgs_palette_update_recolors_existing_object_without_redecoding_source_payload() {
        let mut decoder = PgsDecoder::default();
        let mut all = pcs(1);
        all.extend(palette());
        all.extend(object(true, true, &[1, 1, 0, 0]));
        all.extend(segment(0x80, &[]));
        decoder.push_packet(&all, 1000).unwrap();
        let mut update = pcs(1);
        update[3 + 7] = 0; // Normal composition retains object cache.
        update[3 + 5..3 + 7].copy_from_slice(&2_u16.to_be_bytes());
        decoder.push_packet(&update, 2000).unwrap();
        decoder
            .push_packet(&segment(0x14, &[0, 1, 1, 16, 128, 128, 128]), 2000)
            .unwrap();
        let frame = decoder
            .push_packet(&segment(0x80, &[]), 2000)
            .unwrap()
            .remove(0);
        assert_eq!(frame.time_ms, 2000);
        assert_eq!(frame.images[0].rgba, [0, 0, 0, 128, 0, 0, 0, 128]);
    }
    #[test]
    fn pgs_rle_handles_short_long_transparent_and_colored_runs_with_strict_bounds() {
        assert_eq!(
            decode_rle(&[0, 0x83, 7, 0, 2, 0, 0], 5, 1).unwrap(),
            [7, 7, 7, 0, 0]
        );
        assert_eq!(
            decode_rle(&[0, 0xc1, 0, 9, 0, 0], 256, 1).unwrap(),
            vec![9; 256]
        );
        for data in [
            &[0][..],
            &[0, 0x40][..],
            &[0, 0x83][..],
            &[0, 0x86, 1][..],
            &[1][..],
        ] {
            assert!(decode_rle(data, 5, 1).is_err());
        }
        assert!(decode_rle(&[0, 0, 0, 0], 2, 1).is_err());
    }
    #[test]
    fn pgs_rejects_truncation_unknown_types_missing_objects_and_oversized_canvas() {
        let mut decoder = PgsDecoder::default();
        assert!(decoder.push_packet(&segment(0x99, &[]), 0).is_err());
        let mut decoder = PgsDecoder::default();
        decoder.push_packet(&pcs(1), 0).unwrap();
        decoder.push_packet(&palette(), 0).unwrap();
        assert!(decoder.push_packet(&segment(0x80, &[]), 0).is_err());
        let mut decoder = PgsDecoder::default();
        decoder.push_packet(&[0x15, 0, 9, 0], 0).unwrap();
        assert!(decoder.finish().is_err());
        let mut huge = pcs(0);
        huge[3..7].fill(255);
        assert!(PgsDecoder::default().push_packet(&huge, 0).is_err());
    }
    #[test]
    fn pgs_window_clipping_and_split_segment_keep_source_coordinates_and_first_timestamp() {
        let mut decoder = PgsDecoder::default();
        let composition = pcs(1);
        decoder.push_packet(&composition[..5], 1000).unwrap();
        decoder.push_packet(&composition[5..], 1500).unwrap();
        decoder
            .push_packet(&segment(0x17, &[1, 0, 0, 2, 0, 1, 0, 1, 0, 1]), 1500)
            .unwrap();
        decoder.push_packet(&palette(), 1500).unwrap();
        decoder
            .push_packet(&object(true, true, &[1, 1, 0, 0]), 1500)
            .unwrap();
        let display = decoder
            .push_packet(&segment(0x80, &[]), 1500)
            .unwrap()
            .remove(0);
        assert_eq!(display.time_ms, 1000);
        assert_eq!(
            (
                display.images[0].x,
                display.images[0].y,
                display.images[0].width,
                display.images[0].height
            ),
            (2, 1, 1, 1)
        );
        assert_eq!(display.images[0].rgba, [255, 255, 255, 255]);
    }
}
