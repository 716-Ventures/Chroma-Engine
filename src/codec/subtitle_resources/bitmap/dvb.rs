//! DVB bitmap subtitle display sets (ETSI EN 300 743). Decoding keeps page,
//! region, palette and object state across packets and publishes at end-of-set.
use super::{BitmapRect, DisplaySet, MAX_ENCODED_BYTES, MAX_RGBA_BYTES, checked_rgba_len};
use anyhow::{Context, Result, bail};
use std::collections::BTreeMap;

#[derive(Clone)]
struct Clut {
    two: [[u8; 4]; 4],
    four: [[u8; 4]; 16],
    eight: [[u8; 4]; 256],
}
impl Default for Clut {
    fn default() -> Self {
        let two = [
            [0, 0, 0, 0],
            [255, 255, 255, 255],
            [0, 0, 0, 255],
            [127, 127, 127, 255],
        ];
        let mut four = [[0; 4]; 16];
        for (i, color) in four.iter_mut().enumerate().skip(1) {
            let scale = if i & 8 == 0 { 255 } else { 127 };
            *color = [
                if i & 1 != 0 { scale } else { 0 },
                if i & 2 != 0 { scale } else { 0 },
                if i & 4 != 0 { scale } else { 0 },
                255,
            ];
        }
        let mut eight = [[0; 4]; 256];
        for (i, color) in eight.iter_mut().enumerate().skip(1) {
            if i < 8 {
                *color = [
                    if i & 1 != 0 { 255 } else { 0 },
                    if i & 2 != 0 { 255 } else { 0 },
                    if i & 4 != 0 { 255 } else { 0 },
                    63,
                ];
                continue;
            }
            let (base, low, high, alpha) = match i & 0x88 {
                0 => (0, 85, 170, 255),
                8 => (0, 85, 170, 127),
                128 => (127, 43, 85, 255),
                _ => (0, 43, 85, 255),
            };
            *color = [
                base + if i & 1 != 0 { low } else { 0 } + if i & 16 != 0 { high } else { 0 },
                base + if i & 2 != 0 { low } else { 0 } + if i & 32 != 0 { high } else { 0 },
                base + if i & 4 != 0 { low } else { 0 } + if i & 64 != 0 { high } else { 0 },
                alpha,
            ];
        }
        Self { two, four, eight }
    }
}

struct Region {
    width: u32,
    height: u32,
    depth: u8,
    clut: u8,
    fill: bool,
    background: u8,
    objects: Vec<(u16, u32, u32)>,
    pixels: Vec<u8>,
}
struct Object {
    top: Vec<u8>,
    bottom: Vec<u8>,
    non_modifying: bool,
}

pub(super) struct DvbDecoder {
    page: Option<u16>,
    ancillary: Option<u16>,
    width: u32,
    height: u32,
    window: (u32, u32, u32, u32),
    placements: Vec<(u8, u32, u32)>,
    regions: BTreeMap<u8, Region>,
    objects: BTreeMap<(u16, u16), Object>,
    cluts: BTreeMap<(u16, u8), Clut>,
    timeout_ms: u64,
    deadline: Option<u64>,
    pending: Vec<u8>,
    last_time: Option<u64>,
}

impl DvbDecoder {
    pub(super) fn retained_bytes(&self) -> usize {
        // Include vector capacity after drains and a conservative tree-node
        // allowance, so empty-but-allocated buffers still count against a job.
        std::mem::size_of::<Self>()
            .saturating_add(self.pending.capacity())
            .saturating_add(
                self.placements
                    .capacity()
                    .saturating_mul(std::mem::size_of::<(u8, u32, u32)>()),
            )
            .saturating_add(
                self.regions
                    .values()
                    .map(|region| {
                        std::mem::size_of::<Region>()
                            + 64
                            + region.pixels.capacity()
                            + region.objects.capacity() * std::mem::size_of::<(u16, u32, u32)>()
                    })
                    .sum::<usize>(),
            )
            .saturating_add(
                self.objects
                    .values()
                    .map(|object| {
                        std::mem::size_of::<Object>()
                            + 64
                            + object.top.capacity()
                            + object.bottom.capacity()
                    })
                    .sum::<usize>(),
            )
            .saturating_add(
                self.cluts
                    .len()
                    .saturating_mul(std::mem::size_of::<Clut>() + 64),
            )
    }
    pub(super) fn new(private: &[u8]) -> Result<Self> {
        if !private.is_empty() && private.len() != 4 && private.len() != 5 {
            bail!("Invalid DVB page metadata");
        }
        // Matroska muxers may append the DVB subtitling-type byte after the
        // composition/ancillary page IDs. Pixel geometry still comes from DDS.
        let (page, ancillary) = if private.len() >= 4 {
            (
                Some(u16::from_be_bytes([private[0], private[1]])),
                Some(u16::from_be_bytes([private[2], private[3]])),
            )
        } else {
            (None, None)
        };
        Ok(Self {
            page,
            ancillary,
            width: 720,
            height: 576,
            window: (0, 0, 720, 576),
            placements: Vec::new(),
            regions: BTreeMap::new(),
            objects: BTreeMap::new(),
            cluts: BTreeMap::new(),
            timeout_ms: 0,
            deadline: None,
            pending: Vec::new(),
            last_time: None,
        })
    }
    pub(super) fn push_packet(&mut self, bytes: &[u8], time_ms: u64) -> Result<Vec<DisplaySet>> {
        if self.last_time.is_some_and(|last| last > time_ms) {
            bail!("Nonmonotonic DVB packet times");
        }
        self.last_time = Some(time_ms);
        let mut result = Vec::new();
        let mut output_bytes = 0usize;
        if self.deadline.is_some_and(|deadline| deadline <= time_ms) {
            let deadline = self.deadline.take().unwrap();
            result.push(self.clear(deadline));
        }
        let bytes = if bytes.first() == Some(&0x20) {
            bytes.get(2..).context("Truncated DVB data identifier")?
        } else {
            bytes
        };
        if self
            .pending
            .len()
            .checked_add(bytes.len())
            .is_none_or(|n| n > MAX_ENCODED_BYTES)
        {
            bail!("DVB segment buffer exceeds limit");
        }
        self.pending.extend_from_slice(bytes);
        let mut cursor = 0;
        let mut visits = 0;
        while cursor < self.pending.len() {
            visits += 1;
            if visits > 100_000 {
                bail!("DVB segment work limit exceeded");
            }
            if self.pending[cursor] == 0xff {
                cursor += 1;
                continue;
            }
            if self.pending[cursor] != 0x0f {
                bail!("Invalid DVB segment sync");
            }
            if self.pending.len() - cursor < 6 {
                break;
            }
            let kind = self.pending[cursor + 1];
            let page = read16(&self.pending[cursor + 2..cursor + 4]);
            let len = usize::from(read16(&self.pending[cursor + 4..cursor + 6]));
            let end = cursor
                .checked_add(6 + len)
                .context("DVB segment size overflow")?;
            if end > self.pending.len() {
                break;
            }
            let data = self.pending[cursor + 6..end].to_vec();
            self.segment(kind, page, &data, time_ms, &mut result, &mut output_bytes)?;
            cursor = end;
        }
        self.pending.drain(..cursor);
        Ok(result)
    }
    pub(super) fn finish(&mut self) -> Result<Vec<DisplaySet>> {
        if !self.pending.is_empty() {
            bail!("Truncated final DVB segment");
        }
        Ok(self
            .deadline
            .take()
            .map(|time| self.clear(time))
            .into_iter()
            .collect())
    }
    fn clear(&self, time_ms: u64) -> DisplaySet {
        DisplaySet {
            time_ms,
            canvas_width: self.width,
            canvas_height: self.height,
            images: Vec::new(),
        }
    }
    fn segment(
        &mut self,
        kind: u8,
        page: u16,
        data: &[u8],
        time_ms: u64,
        result: &mut Vec<DisplaySet>,
        output_bytes: &mut usize,
    ) -> Result<()> {
        if kind == 0x10 && self.page.is_none() {
            self.page = Some(page);
        }
        if self.page != Some(page) && self.ancillary != Some(page) {
            return Ok(());
        }
        let composition = self.page == Some(page);
        match kind {
            0x10 if composition => self.page(data)?,
            0x11 if composition => self.region(data)?,
            0x12 => self.clut(page, data)?,
            0x13 => self.object(page, data)?,
            0x14 if composition => self.display(data)?,
            0x80 if composition => {
                if !data.is_empty() {
                    bail!("Invalid DVB end-of-display segment");
                }
                let frame = self.render(time_ms)?;
                let frame_bytes = frame
                    .images
                    .iter()
                    .try_fold(0usize, |total, image| {
                        total.checked_add(image.rgba.capacity())
                    })
                    .context("DVB display byte count overflow")?;
                *output_bytes = output_bytes
                    .checked_add(frame_bytes)
                    .filter(|bytes| *bytes <= MAX_RGBA_BYTES)
                    .context("DVB packet displays exceed memory limit")?;
                if result.len() >= 100_000 {
                    bail!("DVB packet display count exceeds limit");
                }
                self.deadline = if frame.images.is_empty() || self.timeout_ms == 0 {
                    None
                } else {
                    Some(
                        time_ms
                            .checked_add(self.timeout_ms)
                            .context("DVB timeout overflow")?,
                    )
                };
                result.push(frame);
            }
            _ => {}
        }
        Ok(())
    }
    fn page(&mut self, data: &[u8]) -> Result<()> {
        if data.len() < 2 || !(data.len() - 2).is_multiple_of(6) {
            bail!("Invalid DVB page composition");
        }
        self.timeout_ms = u64::from(data[0]) * 1000;
        let state = (data[1] >> 2) & 3;
        if state == 3 {
            bail!("Reserved DVB page state");
        }
        if state != 0 {
            self.regions.clear();
            self.objects.clear();
            self.cluts.clear();
        }
        let mut placements = Vec::new();
        for part in data[2..].as_chunks::<6>().0 {
            if placements.len() >= 256 || placements.iter().any(|(id, _, _)| *id == part[0]) {
                bail!("Repeated or excessive DVB regions");
            }
            placements.push((
                part[0],
                u32::from(read16(&part[2..4])),
                u32::from(read16(&part[4..6])),
            ));
        }
        self.placements = placements;
        Ok(())
    }
    fn display(&mut self, data: &[u8]) -> Result<()> {
        if data.len() < 5 {
            bail!("Truncated DVB display definition");
        }
        let width = u32::from(read16(&data[1..3])) + 1;
        let height = u32::from(read16(&data[3..5])) + 1;
        checked_rgba_len(width, height)?;
        let window = if data[0] & 8 != 0 {
            if data.len() != 13 {
                bail!("Invalid DVB display window");
            }
            let left = u32::from(read16(&data[5..7]));
            let right = u32::from(read16(&data[7..9]));
            let top = u32::from(read16(&data[9..11]));
            let bottom = u32::from(read16(&data[11..13]));
            if right < left || bottom < top || right >= width || bottom >= height {
                bail!("DVB window outside display");
            }
            (left, top, right - left + 1, bottom - top + 1)
        } else {
            if data.len() != 5 {
                bail!("Invalid DVB display length");
            }
            (0, 0, width, height)
        };
        self.width = width;
        self.height = height;
        self.window = window;
        Ok(())
    }
    fn region(&mut self, data: &[u8]) -> Result<()> {
        if data.len() < 10 {
            bail!("Truncated DVB region");
        }
        let width = u32::from(read16(&data[2..4]));
        let height = u32::from(read16(&data[4..6]));
        checked_rgba_len(width, height)?;
        let depth = (data[6] >> 2) & 7;
        if !(1..=3).contains(&depth) {
            bail!("Invalid DVB pixel depth");
        }
        let background = match depth {
            1 => (data[9] >> 2) & 3,
            2 => data[9] >> 4,
            _ => data[8],
        };
        let mut cursor = 10;
        let mut objects = Vec::new();
        while cursor < data.len() {
            let v = data
                .get(cursor..cursor + 6)
                .context("Truncated DVB region object")?;
            let id = read16(v);
            let kind = v[2] >> 6;
            let provider = (v[2] >> 4) & 3;
            if kind != 0 || provider != 0 {
                bail!("DVB character or receiver-ROM objects are unsupported");
            }
            let x = (u32::from(v[2] & 15) << 8) | u32::from(v[3]);
            let y = (u32::from(v[4] & 15) << 8) | u32::from(v[5]);
            if x >= width || y >= height || objects.len() >= 1024 {
                bail!("DVB object outside region or limit");
            }
            objects.push((id, x, y));
            cursor += 6;
        }
        let count = usize::try_from(u64::from(width) * u64::from(height))?;
        let other = self
            .regions
            .iter()
            .filter(|(id, _)| **id != data[0])
            .map(|(_, r)| r.pixels.len())
            .sum::<usize>();
        if count
            .checked_add(other)
            .is_none_or(|n| n > MAX_RGBA_BYTES / 4)
            || self.regions.len() >= 256 && !self.regions.contains_key(&data[0])
        {
            bail!("DVB region memory limit exceeded");
        }
        let previous = self.regions.remove(&data[0]);
        let pixels = previous
            .filter(|r| r.width == width && r.height == height && r.depth == depth)
            .map(|r| r.pixels)
            .unwrap_or_else(|| vec![background; count]);
        self.regions.insert(
            data[0],
            Region {
                width,
                height,
                depth,
                clut: data[7],
                fill: data[1] & 8 != 0,
                background,
                objects,
                pixels,
            },
        );
        Ok(())
    }
    fn clut(&mut self, page: u16, data: &[u8]) -> Result<()> {
        if data.len() < 2 {
            bail!("Truncated DVB palette");
        }
        if self.cluts.len() >= 512 && !self.cluts.contains_key(&(page, data[0])) {
            bail!("DVB palette count exceeds limit");
        }
        let mut clut = self
            .cluts
            .get(&(page, data[0]))
            .cloned()
            .unwrap_or_default();
        let mut cursor = 2;
        while cursor < data.len() {
            let id = usize::from(*data.get(cursor).context("Missing DVB palette index")?);
            let flags = *data.get(cursor + 1).context("Missing DVB palette flags")?;
            cursor += 2;
            let (y, cr, cb, t) = if flags & 1 != 0 {
                let v = data
                    .get(cursor..cursor + 4)
                    .context("Truncated DVB full palette entry")?;
                cursor += 4;
                (v[0], v[1], v[2], v[3])
            } else {
                let v = data
                    .get(cursor..cursor + 2)
                    .context("Truncated DVB reduced palette entry")?;
                cursor += 2;
                (
                    (v[0] & 0xfc),
                    (v[0] & 3) << 6 | (v[1] & 0xc0) >> 2,
                    (v[1] & 0x3c) << 2,
                    (v[1] & 3) << 6,
                )
            };
            let color = ycbcr(y, cr, cb, t);
            if flags & 0x80 != 0 && id < 4 {
                clut.two[id] = color;
            }
            if flags & 0x40 != 0 && id < 16 {
                clut.four[id] = color;
            }
            if flags & 0x20 != 0 {
                clut.eight[id] = color;
            }
            if flags & 0xe0 == 0 {
                bail!("DVB palette has no depth flag");
            }
        }
        self.cluts.insert((page, data[0]), clut);
        Ok(())
    }
    fn object(&mut self, page: u16, data: &[u8]) -> Result<()> {
        if data.len() < 7 {
            bail!("Truncated DVB object");
        }
        let method = (data[2] >> 2) & 3;
        if method != 0 {
            bail!("DVB non-interlaced object coding is unsupported");
        }
        let top = usize::from(read16(&data[3..5]));
        let bottom = usize::from(read16(&data[5..7]));
        let end = 7usize
            .checked_add(top)
            .and_then(|v| v.checked_add(bottom))
            .filter(|n| *n <= data.len())
            .context("DVB object fields outside segment")?;
        if data.len() - end > 1 {
            bail!("Invalid DVB object stuffing");
        }
        let key = (page, read16(data));
        let other = self
            .objects
            .iter()
            .filter(|(id, _)| **id != key)
            .map(|(_, o)| o.top.len() + o.bottom.len())
            .sum::<usize>();
        if top
            .checked_add(bottom)
            .and_then(|n| n.checked_add(other))
            .is_none_or(|n| n > MAX_ENCODED_BYTES)
            || self.objects.len() >= 4096 && !self.objects.contains_key(&key)
        {
            bail!("DVB object buffer exceeds limit");
        }
        self.objects.insert(
            key,
            Object {
                top: data[7..7 + top].to_vec(),
                bottom: data[7 + top..end].to_vec(),
                non_modifying: data[2] & 2 != 0,
            },
        );
        Ok(())
    }
    fn render(&mut self, time_ms: u64) -> Result<DisplaySet> {
        let mut images = Vec::new();
        let mut rgba_total = 0usize;
        let page = self.page.context("DVB composition page missing")?;
        for &(id, x, y) in &self.placements {
            let region = self
                .regions
                .get_mut(&id)
                .context("DVB region definition missing")?;
            let x = x
                .checked_add(self.window.0)
                .context("DVB horizontal position overflow")?;
            let y = y
                .checked_add(self.window.1)
                .context("DVB vertical position overflow")?;
            if x.checked_add(region.width)
                .is_none_or(|n| n > self.window.0 + self.window.2)
                || y.checked_add(region.height)
                    .is_none_or(|n| n > self.window.1 + self.window.3)
            {
                bail!("DVB region outside display window");
            }
            if region.fill {
                region.pixels.fill(region.background);
                region.fill = false;
            }
            for (object_id, ox, oy) in region.objects.clone() {
                let object = self
                    .objects
                    .get(&(page, object_id))
                    .or_else(|| {
                        self.ancillary
                            .and_then(|p| self.objects.get(&(p, object_id)))
                    })
                    .context("DVB bitmap object missing")?;
                paint_field(&object.top, region, ox, oy, object.non_modifying)?;
                paint_field(
                    if object.bottom.is_empty() {
                        &object.top
                    } else {
                        &object.bottom
                    },
                    region,
                    ox,
                    oy + 1,
                    object.non_modifying,
                )?;
            }
            let default = Clut::default();
            let palette = self
                .cluts
                .get(&(page, region.clut))
                .or_else(|| {
                    self.ancillary
                        .and_then(|p| self.cluts.get(&(p, region.clut)))
                })
                .unwrap_or(&default);
            let len = checked_rgba_len(region.width, region.height)?;
            rgba_total = rgba_total
                .checked_add(len)
                .filter(|n| *n <= MAX_RGBA_BYTES)
                .context("DVB display memory limit exceeded")?;
            let mut rgba = Vec::with_capacity(len);
            for &index in &region.pixels {
                rgba.extend_from_slice(match region.depth {
                    1 => palette
                        .two
                        .get(usize::from(index))
                        .context("DVB 2-bit color index invalid")?,
                    2 => palette
                        .four
                        .get(usize::from(index))
                        .context("DVB 4-bit color index invalid")?,
                    _ => &palette.eight[usize::from(index)],
                });
            }
            images.push(BitmapRect {
                x,
                y,
                width: region.width,
                height: region.height,
                rgba,
            });
        }
        Ok(DisplaySet {
            time_ms,
            canvas_width: self.width,
            canvas_height: self.height,
            images,
        })
    }
}

fn read16(v: &[u8]) -> u16 {
    u16::from_be_bytes([v[0], v[1]])
}
fn ycbcr(y: u8, cr: u8, cb: u8, t: u8) -> [u8; 4] {
    if y == 0 {
        return [0; 4];
    }
    let y = 298 * (i32::from(y) - 16);
    let cr = i32::from(cr) - 128;
    let cb = i32::from(cb) - 128;
    [
        ((y + 409 * cr + 128) >> 8).clamp(0, 255) as u8,
        ((y - 100 * cb - 208 * cr + 128) >> 8).clamp(0, 255) as u8,
        ((y + 516 * cb + 128) >> 8).clamp(0, 255) as u8,
        255 - t,
    ]
}

struct Bits<'a> {
    bytes: &'a [u8],
    position: usize,
}
impl Bits<'_> {
    fn get(&mut self, n: usize) -> Result<u16> {
        if n > 16
            || self
                .position
                .checked_add(n)
                .is_none_or(|p| p > self.bytes.len() * 8)
        {
            bail!("Truncated DVB pixel code");
        }
        let mut result = 0;
        for _ in 0..n {
            result = (result << 1)
                | u16::from((self.bytes[self.position / 8] >> (7 - self.position % 8)) & 1);
            self.position += 1;
        }
        Ok(result)
    }
    fn consumed(&self) -> usize {
        self.position.div_ceil(8)
    }
}
fn run(bits: &mut Bits<'_>, depth: u8) -> Result<Option<(usize, u8)>> {
    let code = bits.get(usize::from(depth))? as u8;
    if code != 0 {
        return Ok(Some((1, code)));
    }
    let (count, color) = match depth {
        2 => {
            if bits.get(1)? != 0 {
                (usize::from(bits.get(3)?) + 3, bits.get(2)? as u8)
            } else if bits.get(1)? != 0 {
                (1, 0)
            } else {
                match bits.get(2)? {
                    0 => return Ok(None),
                    1 => (2, 0),
                    2 => (usize::from(bits.get(4)?) + 12, bits.get(2)? as u8),
                    _ => (usize::from(bits.get(8)?) + 29, bits.get(2)? as u8),
                }
            }
        }
        4 => {
            if bits.get(1)? == 0 {
                let n = bits.get(3)?;
                if n == 0 {
                    return Ok(None);
                }
                (usize::from(n) + 2, 0)
            } else if bits.get(1)? == 0 {
                (usize::from(bits.get(2)?) + 4, bits.get(4)? as u8)
            } else {
                match bits.get(2)? {
                    0 => (1, 0),
                    1 => (2, 0),
                    2 => (usize::from(bits.get(4)?) + 9, bits.get(4)? as u8),
                    _ => (usize::from(bits.get(8)?) + 25, bits.get(4)? as u8),
                }
            }
        }
        8 => {
            let colored = bits.get(1)? != 0;
            let n = usize::from(bits.get(7)?);
            if !colored && n == 0 {
                return Ok(None);
            }
            if colored && n < 3 {
                bail!("Invalid DVB 8-bit colored run");
            }
            (n, if colored { bits.get(8)? as u8 } else { 0 })
        }
        _ => bail!("Unsupported DVB pixel code depth"),
    };
    Ok(Some((count, color)))
}
fn paint_field(
    bytes: &[u8],
    region: &mut Region,
    origin_x: u32,
    origin_y: u32,
    non_modifying: bool,
) -> Result<()> {
    if bytes.is_empty() || origin_y >= region.height {
        return Ok(());
    }
    let mut cursor = 0;
    let mut x = origin_x;
    let mut y = origin_y;
    let mut work = 0usize;
    let mut map24 = [0, 7, 8, 15];
    let mut map28 = [0, 0x77, 0x88, 0xff];
    let mut map48 = [0u8; 16];
    for (i, v) in map48.iter_mut().enumerate() {
        *v = (i * 17) as u8;
    }
    while cursor < bytes.len() {
        let kind = bytes[cursor];
        cursor += 1;
        match kind {
            0x10..=0x12 => {
                let depth = match kind {
                    0x10 => 2,
                    0x11 => 4,
                    _ => 8,
                };
                if depth > 1 << region.depth {
                    bail!("DVB object depth exceeds region");
                }
                let mut bits = Bits {
                    bytes: &bytes[cursor..],
                    position: 0,
                };
                while let Some((count, code)) = run(&mut bits, depth)? {
                    work = work
                        .checked_add(count)
                        .filter(|n| *n <= MAX_RGBA_BYTES / 4)
                        .context("DVB pixel work limit exceeded")?;
                    let mapped = match (depth, region.depth) {
                        (2, 2) => map24[usize::from(code)],
                        (2, 3) => map28[usize::from(code)],
                        (4, 3) => map48[usize::from(code)],
                        _ => code,
                    };
                    let right = x
                        .checked_add(u32::try_from(count)?)
                        .context("DVB row overflow")?;
                    if right > region.width || y >= region.height {
                        bail!("DVB pixel run outside region");
                    }
                    if !non_modifying || mapped != 1 {
                        region.pixels
                            [(y * region.width + x) as usize..(y * region.width + right) as usize]
                            .fill(mapped);
                    }
                    x = right;
                }
                cursor += bits.consumed();
            }
            0x20 => {
                let v = bytes
                    .get(cursor..cursor + 2)
                    .context("Truncated DVB 2-to-4 mapping")?;
                map24 = [v[0] >> 4, v[0] & 15, v[1] >> 4, v[1] & 15];
                cursor += 2;
            }
            0x21 => {
                map28.copy_from_slice(
                    bytes
                        .get(cursor..cursor + 4)
                        .context("Truncated DVB 2-to-8 mapping")?,
                );
                cursor += 4;
            }
            0x22 => {
                map48.copy_from_slice(
                    bytes
                        .get(cursor..cursor + 16)
                        .context("Truncated DVB 4-to-8 mapping")?,
                );
                cursor += 16;
            }
            0xf0 => {
                x = origin_x;
                y = y.checked_add(2).context("DVB row position overflow")?;
            }
            _ => bail!("Unknown DVB pixel data type"),
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    fn segment(kind: u8, data: &[u8]) -> Vec<u8> {
        let mut b = vec![15, kind, 0, 1];
        b.extend((data.len() as u16).to_be_bytes());
        b.extend(data);
        b
    }
    fn display_packet() -> Vec<u8> {
        let mut packet = segment(0x10, &[2, 4, 1, 0, 0, 10, 0, 20]);
        packet.extend(segment(
            0x11,
            &[1, 8, 0, 2, 0, 2, 4, 0, 0, 0, 0, 1, 0, 0, 0, 0],
        ));
        // 2-bit line: two white pixels, end-of-string, end-of-object-line.
        packet.extend(segment(0x13, &[0, 1, 0, 0, 4, 0, 0, 0x10, 0x50, 0, 0xf0]));
        packet.extend(segment(0x80, &[]));
        packet
    }
    #[test]
    fn dvb_decodes_interlaced_object_page_position_and_timeout() {
        let mut decoder = DvbDecoder::new(&[0, 1, 0, 2]).unwrap();
        let frames = decoder.push_packet(&display_packet(), 1000).unwrap();
        assert_eq!(frames.len(), 1);
        let frame = &frames[0];
        assert_eq!((frame.canvas_width, frame.canvas_height), (720, 576));
        let image = &frame.images[0];
        assert_eq!(
            (image.x, image.y, image.width, image.height),
            (10, 20, 2, 2)
        );
        assert!(
            image
                .rgba
                .as_chunks::<4>()
                .0
                .iter()
                .all(|v| *v == [255, 255, 255, 255])
        );
        let clear = decoder.finish().unwrap();
        assert_eq!(clear[0].time_ms, 3000);
        assert!(clear[0].images.is_empty());
    }
    #[test]
    fn dvb_page_clear_replaces_timeout_and_fragmented_segment_is_reassembled() {
        let p = display_packet();
        let mut d = DvbDecoder::new(&[]).unwrap();
        assert!(d.push_packet(&p[..5], 10).unwrap().is_empty());
        assert_eq!(d.push_packet(&p[5..], 10).unwrap().len(), 1);
        let mut clear = segment(0x10, &[0, 0]);
        clear.extend(segment(0x80, &[]));
        let frames = d.push_packet(&clear, 100).unwrap();
        assert_eq!(frames.len(), 1);
        assert!(frames[0].images.is_empty());
        assert!(d.finish().unwrap().is_empty());
    }
    #[test]
    fn dvb_pixel_code_variants_and_mapping_tables_are_bounded() {
        for (depth, bytes, expected) in [
            (2, vec![0x50, 0], vec![(1, 1), (1, 1)]),
            (4, vec![0x12, 0], vec![(1, 1), (1, 2)]),
            (8, vec![3, 4, 0, 0], vec![(1, 3), (1, 4)]),
        ] {
            let mut bits = Bits {
                bytes: &bytes,
                position: 0,
            };
            let mut actual = Vec::new();
            while let Some(value) = run(&mut bits, depth).unwrap() {
                actual.push(value);
            }
            assert_eq!(actual, expected);
        }
        let mut region = Region {
            width: 2,
            height: 1,
            depth: 3,
            clut: 0,
            fill: false,
            background: 0,
            objects: Vec::new(),
            pixels: vec![0; 2],
        };
        paint_field(
            &[0x21, 0, 11, 22, 33, 0x10, 0x50, 0, 0xf0],
            &mut region,
            0,
            0,
            false,
        )
        .unwrap();
        assert_eq!(region.pixels, [11, 11]);
        // Non-modifying color is a CLUT entry, after application of map tables.
        region.pixels.fill(9);
        paint_field(
            &[0x21, 0, 1, 22, 33, 0x10, 0x50, 0, 0xf0],
            &mut region,
            0,
            0,
            true,
        )
        .unwrap();
        assert_eq!(region.pixels, [9, 9]);
        paint_field(
            &[0x21, 0, 11, 22, 33, 0x10, 0x50, 0, 0xf0],
            &mut region,
            0,
            0,
            true,
        )
        .unwrap();
        assert_eq!(region.pixels, [11, 11]);
        assert!(paint_field(&[0x12, 3, 3, 3, 0, 0], &mut region, 0, 0, false).is_err());
    }
    #[test]
    fn dvb_clut_transparency_and_hostile_lengths_are_rejected() {
        assert_eq!(ycbcr(235, 128, 128, 0), [255, 255, 255, 255]);
        assert_eq!(ycbcr(0, 200, 200, 0), [0; 4]);
        let mut d = DvbDecoder::new(&[0, 1, 0, 2]).unwrap();
        let mut bad = segment(0x14, &[0, 255, 255, 255, 255]);
        assert!(d.push_packet(&bad, 0).is_err());
        bad = segment(0x13, &[0, 1, 0, 255, 255, 0, 0]);
        assert!(d.push_packet(&bad, 0).is_err());
        let mut d = DvbDecoder::new(&[]).unwrap();
        d.push_packet(&[15, 16, 0], 0).unwrap();
        assert!(d.finish().is_err());
    }
    #[test]
    fn dvb_repeated_end_segments_cannot_expand_a_packet_beyond_display_budget() {
        let mut d = DvbDecoder::new(&[0, 1, 0, 2]).unwrap();
        d.width = 1024;
        d.height = 1024;
        d.window = (0, 0, 1024, 1024);
        d.placements.push((1, 0, 0));
        d.regions.insert(
            1,
            Region {
                width: 1024,
                height: 1024,
                depth: 1,
                clut: 0,
                fill: false,
                background: 0,
                objects: Vec::new(),
                pixels: vec![1; 1024 * 1024],
            },
        );
        let mut packet = Vec::new();
        for _ in 0..17 {
            packet.extend(segment(0x80, &[]));
        }
        let error = d.push_packet(&packet, 0).err().unwrap();
        assert!(
            error
                .to_string()
                .contains("packet displays exceed memory limit")
        );
        assert!(d.retained_bytes() >= 1024 * 1024);
    }
}
