//! Portable positioned bitmap subtitle resources. Decode one display at a time,
//! publish immutable bounded PNGs, and retain only the small timed cue index.
use super::{SubtitleResources, SubtitleScriptResource, UnavailableSubtitleResource};
use crate::{
    container::matroska::{
        MatroskaTrack, MatroskaTrackKind, looks_like_ebml, parse_basic_metadata,
    },
    output::publish_bytes,
    source::{MediaSource, probe_reader::Element},
};
use anyhow::{Context, Result, bail};
use serde::Serialize;
use std::{
    cell::Cell,
    fs::{File, OpenOptions},
    io::{Read, Seek, SeekFrom},
    path::{Path, PathBuf},
    rc::Rc,
};

pub(crate) mod compression;
mod dvb;
mod dvd;
mod pgs;

pub(super) const MAX_PIXELS: usize = 16 * 1024 * 1024;
pub(super) const MAX_RGBA_BYTES: usize = 64 * 1024 * 1024;
pub(super) const MAX_ENCODED_BYTES: usize = 8 * 1024 * 1024;
const MAX_OUTPUT_BYTES: usize = 256 * 1024 * 1024;
const MAX_CUES: usize = 100_000;
const MAX_INDEX_BYTES: usize = 8 * 1024 * 1024;
const MAX_ARCHIVE_FILES: usize = 50_000;

struct ArchiveBudget {
    used: Cell<usize>,
    files: Cell<usize>,
    limit: usize,
}
impl ArchiveBudget {
    fn new(limit: usize) -> Self {
        Self {
            used: Cell::new(0),
            files: Cell::new(0),
            limit,
        }
    }
    fn reserve(&self, bytes: usize) -> Result<()> {
        let next = self
            .used
            .get()
            .checked_add(bytes)
            .context("Bitmap archive size overflow")?;
        if next > self.limit || self.files.get() >= MAX_ARCHIVE_FILES {
            bail!("Aggregate bitmap subtitle archive budget exceeded");
        }
        self.used.set(next);
        self.files.set(self.files.get() + 1);
        Ok(())
    }
}

pub(super) struct BitmapRect {
    pub x: u32,
    pub y: u32,
    pub width: u32,
    pub height: u32,
    pub rgba: Vec<u8>,
}
pub(super) struct DisplaySet {
    pub time_ms: u64,
    pub canvas_width: u32,
    pub canvas_height: u32,
    pub images: Vec<BitmapRect>,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct BitmapDocument {
    schema_version: u32,
    canvas_width: u32,
    canvas_height: u32,
    cues: Vec<BitmapCue>,
}
#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct BitmapCue {
    start_ms: u64,
    end_ms: u64,
    images: Vec<BitmapImage>,
}
#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct BitmapImage {
    file_name: String,
    x: u32,
    y: u32,
    width: u32,
    height: u32,
}

pub(super) fn checked_rgba_len(width: u32, height: u32) -> Result<usize> {
    let pixels = usize::try_from(width)?
        .checked_mul(usize::try_from(height)?)
        .context("Bitmap pixel count overflow")?;
    if width == 0 || height == 0 || pixels > MAX_PIXELS {
        bail!("Bitmap dimensions exceed limit");
    }
    let len = pixels.checked_mul(4).context("Bitmap RGBA size overflow")?;
    if len > MAX_RGBA_BYTES {
        bail!("Bitmap RGBA size limit exceeded");
    }
    Ok(len)
}

struct Publisher<'a> {
    output: &'a Path,
    id: String,
    document: BitmapDocument,
    active: Option<BitmapCue>,
    last_time: Option<u64>,
    images: usize,
    written: usize,
    created: Vec<PathBuf>,
    complete: bool,
    max_display_bytes: usize,
    archive: Rc<ArchiveBudget>,
    reserved: usize,
    reserved_files: usize,
}
impl<'a> Publisher<'a> {
    fn new(output: &'a Path, id: String) -> Self {
        Self {
            output,
            id,
            document: BitmapDocument {
                schema_version: 1,
                canvas_width: 0,
                canvas_height: 0,
                cues: Vec::new(),
            },
            active: None,
            last_time: None,
            images: 0,
            written: 0,
            created: Vec::new(),
            complete: false,
            max_display_bytes: MAX_RGBA_BYTES,
            archive: Rc::new(ArchiveBudget::new(MAX_OUTPUT_BYTES)),
            reserved: 0,
            reserved_files: 0,
        }
    }
    fn with_source(
        output: &'a Path,
        id: String,
        source: &MediaSource,
        archive: Rc<ArchiveBudget>,
    ) -> Self {
        let mut publisher = Self::new(output, id);
        publisher.max_display_bytes = MAX_RGBA_BYTES.min(source.policy().decoded_batch_bytes);
        publisher.archive = archive;
        publisher
    }
    fn retained_bytes(&self) -> usize {
        let cue_bytes = |cue: &BitmapCue| {
            cue.images.capacity() * std::mem::size_of::<BitmapImage>()
                + cue
                    .images
                    .iter()
                    .map(|image| image.file_name.capacity())
                    .sum::<usize>()
        };
        self.document.cues.capacity() * std::mem::size_of::<BitmapCue>()
            + self.document.cues.iter().map(cue_bytes).sum::<usize>()
            + self.active.as_ref().map_or(0, cue_bytes)
            + self.created.capacity() * std::mem::size_of::<PathBuf>()
            + self
                .created
                .iter()
                .map(|path| path.as_os_str().len())
                .sum::<usize>()
    }
    fn push(&mut self, display: DisplaySet) -> Result<()> {
        checked_rgba_len(display.canvas_width, display.canvas_height)?;
        if self.document.canvas_width == 0 {
            self.document.canvas_width = display.canvas_width;
            self.document.canvas_height = display.canvas_height;
        } else if (self.document.canvas_width, self.document.canvas_height)
            != (display.canvas_width, display.canvas_height)
        {
            bail!("Bitmap canvas changes within track");
        }
        if self.last_time.is_some_and(|last| display.time_ms < last) {
            bail!("Nonmonotonic bitmap presentation times");
        }
        self.last_time = Some(display.time_ms);
        self.close(display.time_ms)?;
        if display.images.is_empty() {
            return Ok(());
        }
        if display.images.len() > 64 {
            bail!("Bitmap display image count limit exceeded");
        }
        let mut images = Vec::new();
        let mut rgba_bytes = 0usize;
        for image in display.images {
            let len = checked_rgba_len(image.width, image.height)?;
            rgba_bytes = rgba_bytes
                .checked_add(len)
                .context("Bitmap display size overflow")?;
            if rgba_bytes > self.max_display_bytes
                || image.rgba.len() != len
                || image
                    .x
                    .checked_add(image.width)
                    .is_none_or(|end| end > display.canvas_width)
                || image
                    .y
                    .checked_add(image.height)
                    .is_none_or(|end| end > display.canvas_height)
            {
                bail!("Bitmap image outside canvas or display budget");
            }
            if self.images >= MAX_CUES {
                bail!("Bitmap image count limit exceeded");
            }
            self.images += 1;
            let file_name = format!("{}-f{:06}.png", self.id, self.images);
            let mut bytes = Vec::new();
            {
                let mut encoder = png::Encoder::new(&mut bytes, image.width, image.height);
                encoder.set_color(png::ColorType::Rgba);
                encoder.set_depth(png::BitDepth::Eight);
                let mut writer = encoder.write_header()?;
                writer.write_image_data(&image.rgba)?;
            }
            if bytes.len() > MAX_ENCODED_BYTES {
                bail!("Bitmap PNG exceeds image size limit");
            }
            self.written = self
                .written
                .checked_add(bytes.len())
                .context("Bitmap output size overflow")?;
            if self.written > MAX_OUTPUT_BYTES {
                bail!("Bitmap output budget exceeded");
            }
            self.archive.reserve(bytes.len())?;
            self.reserved += bytes.len();
            self.reserved_files += 1;
            let path = self.output.join(&file_name);
            let existed = path.exists();
            publish_bytes(&path, &bytes)?;
            if !existed {
                self.created.push(path);
            }
            images.push(BitmapImage {
                file_name,
                x: image.x,
                y: image.y,
                width: image.width,
                height: image.height,
            });
        }
        self.active = Some(BitmapCue {
            start_ms: display.time_ms,
            end_ms: display.time_ms,
            images,
        });
        Ok(())
    }
    fn close(&mut self, end: u64) -> Result<()> {
        if let Some(mut cue) = self.active.take()
            && end > cue.start_ms
        {
            if self.document.cues.len() >= MAX_CUES {
                bail!("Bitmap cue count limit exceeded");
            }
            cue.end_ms = end;
            self.document.cues.push(cue);
        }
        Ok(())
    }
    fn finish(mut self, end: Option<u64>) -> Result<(String, u64)> {
        if let Some(cue) = &self.active {
            let end = end
                .filter(|end| *end > cue.start_ms)
                .context("Bitmap track has no final clear or source duration")?;
            self.close(end)?;
        }
        if self.document.cues.is_empty() {
            bail!("No visible bitmap subtitle cues");
        }
        let duration = self.document.cues.last().map(|cue| cue.end_ms).unwrap_or(0);
        let bytes = serde_json::to_vec(&self.document)?;
        if bytes.len() > MAX_INDEX_BYTES {
            bail!("Bitmap cue index exceeds size limit");
        }
        self.archive.reserve(bytes.len())?;
        self.reserved += bytes.len();
        self.reserved_files += 1;
        let name = format!("{}.bitmap.json", self.id);
        let path = self.output.join(&name);
        let existed = path.exists();
        publish_bytes(&path, &bytes)?;
        if !existed {
            self.created.push(path);
        }
        self.complete = true;
        Ok((name, duration))
    }
}
impl Drop for Publisher<'_> {
    fn drop(&mut self) {
        if !self.complete {
            for path in &self.created {
                let _ = std::fs::remove_file(path);
            }
            self.archive
                .used
                .set(self.archive.used.get().saturating_sub(self.reserved));
            self.archive
                .files
                .set(self.archive.files.get().saturating_sub(self.reserved_files));
        }
    }
}

enum Decoder {
    Pgs(pgs::PgsDecoder),
    Dvd(dvd::DvdDecoder),
    Dvb(dvb::DvbDecoder),
}
impl Decoder {
    fn new(track: &MatroskaTrack) -> Result<Self> {
        Ok(match track.codec.as_str() {
            "hdmv_pgs_subtitle" | "pgs" => Self::Pgs(pgs::PgsDecoder::default()),
            "dvd_subtitle" | "vobsub" => Self::Dvd(dvd::DvdDecoder::new(
                track.codec_private.as_deref().unwrap_or_default(),
            )?),
            "dvb_subtitle" | "S_DVBSUB" => Self::Dvb(dvb::DvbDecoder::new(
                track.codec_private.as_deref().unwrap_or_default(),
            )?),
            _ => bail!("Unsupported bitmap subtitle codec"),
        })
    }
    fn push(&mut self, bytes: &[u8], time: u64, duration: Option<u64>) -> Result<Vec<DisplaySet>> {
        match self {
            Self::Pgs(decoder) => decoder.push_packet(bytes, time),
            Self::Dvd(decoder) => decoder.push_packet_timed(bytes, time, duration),
            Self::Dvb(decoder) => decoder.push_packet(bytes, time),
        }
    }
    fn finish(&mut self) -> Result<Vec<DisplaySet>> {
        match self {
            Self::Pgs(decoder) => {
                decoder.finish()?;
                Ok(Vec::new())
            }
            Self::Dvd(decoder) => Ok(decoder.finish()),
            Self::Dvb(decoder) => decoder.finish(),
        }
    }
    fn retained_bytes(&self) -> usize {
        match self {
            Self::Pgs(value) => value.retained_bytes(),
            Self::Dvd(value) => value.retained_bytes(),
            Self::Dvb(value) => value.retained_bytes(),
        }
    }
}
struct TrackState<'a> {
    track: &'a MatroskaTrack,
    id: String,
    decoder: Option<Decoder>,
    publisher: Option<Publisher<'a>>,
    error: Option<String>,
    input_bytes: usize,
    compression: compression::TrackCompression,
}
impl TrackState<'_> {
    fn fail(&mut self, error: String) {
        self.error = Some(error);
        self.decoder = None;
        self.publisher = None;
    }
}

/// Append embedded PGS/VobSub/DVB and same-basename SUP/IDX bitmap resources.
pub(super) fn append(
    source: &MediaSource,
    input: &Path,
    output: &Path,
    resources: &mut SubtitleResources,
) -> Result<()> {
    source.check_work()?;
    source.validate_current()?;
    std::fs::create_dir_all(output)?;
    let archive = Rc::new(ArchiveBudget::new(MAX_OUTPUT_BYTES));
    let embedded_count = if looks_like_ebml(source.as_ref()) {
        let metadata = parse_basic_metadata(source.as_ref());
        let compressions = compression::parse(source)?;
        let subtitles = metadata
            .tracks
            .iter()
            .filter(|track| track.kind == MatroskaTrackKind::Subtitle)
            .collect::<Vec<_>>();
        let mut states = Vec::new();
        for (index, track) in subtitles.iter().enumerate() {
            if !matches!(
                track.codec.as_str(),
                "hdmv_pgs_subtitle"
                    | "pgs"
                    | "dvd_subtitle"
                    | "vobsub"
                    | "dvb_subtitle"
                    | "S_DVBSUB"
            ) {
                continue;
            }
            let id = format!("s{index}");
            let compression = compressions.get(&track.number).cloned().unwrap_or_default();
            let decoder = (|| -> Result<Decoder> {
                let mut restored = (**track).clone();
                restored.codec_private = track
                    .codec_private
                    .as_deref()
                    .map(|private| compression.decode(private, 2, MAX_ENCODED_BYTES))
                    .transpose()?;
                Decoder::new(&restored)
            })();
            let (decoder, error) = match decoder {
                Ok(decoder) => (Some(decoder), None),
                Err(error) => (None, Some(error.to_string())),
            };
            states.push(TrackState {
                track,
                id: id.clone(),
                decoder,
                publisher: Some(Publisher::with_source(
                    output,
                    id,
                    source,
                    Rc::clone(&archive),
                )),
                error,
                input_bytes: 0,
                compression,
            });
        }
        if !states.is_empty() {
            let selected_numbers = states
                .iter()
                .map(|state| state.track.number)
                .collect::<Vec<_>>();
            scan_matroska(
                source,
                |number, time, duration, bytes| {
                    if let Some(index) = states
                        .iter()
                        .position(|state| state.track.number == number && state.error.is_none())
                    {
                        let state = &mut states[index];
                        state.input_bytes = state
                            .input_bytes
                            .checked_add(bytes.len())
                            .context("Bitmap input size overflow")?;
                        if state.input_bytes > MAX_OUTPUT_BYTES {
                            state.fail("Bitmap input budget exceeded".into());
                            return Ok(());
                        }
                        let result = (|| -> Result<()> {
                            let restored = state.compression.decode(bytes, 1, MAX_ENCODED_BYTES)?;
                            for display in state
                                .decoder
                                .as_mut()
                                .context("Bitmap decoder unavailable")?
                                .push(&restored, time, duration)?
                            {
                                state
                                    .publisher
                                    .as_mut()
                                    .context("Bitmap publisher unavailable")?
                                    .push(display)?;
                            }
                            Ok(())
                        })();
                        if let Err(error) = result {
                            state.fail(error.to_string());
                        }
                        let decoder_bytes = states
                            .iter()
                            .filter_map(|state| state.decoder.as_ref())
                            .map(Decoder::retained_bytes)
                            .sum::<usize>();
                        let index_bytes = states
                            .iter()
                            .filter_map(|state| state.publisher.as_ref())
                            .map(Publisher::retained_bytes)
                            .sum::<usize>();
                        if decoder_bytes
                            > source
                                .policy()
                                .compressed_window_bytes
                                .min(128 * 1024 * 1024)
                            || index_bytes > source.policy().index_bytes.min(64 * 1024 * 1024)
                        {
                            states[index]
                                .fail("Aggregate bitmap subtitle state budget exceeded".into());
                        }
                    }
                    Ok(())
                },
                &selected_numbers,
            )?;
            for mut state in states {
                let result = (|| -> Result<(String, u64)> {
                    if let Some(error) = &state.error {
                        bail!("{error}");
                    }
                    for display in state
                        .decoder
                        .as_mut()
                        .context("Bitmap decoder unavailable")?
                        .finish()?
                    {
                        state
                            .publisher
                            .as_mut()
                            .context("Bitmap publisher unavailable")?
                            .push(display)?;
                    }
                    state
                        .publisher
                        .take()
                        .context("Bitmap publisher unavailable")?
                        .finish(metadata.duration_ms)
                })();
                match result {
                    Ok((name, duration)) => resources.tracks.push(SubtitleScriptResource {
                        id: state.id,
                        codec: state.track.codec.clone(),
                        file_name: name,
                        web_vtt_file_name: None,
                        overlay_file_name: None,
                        language: state.track.language.clone(),
                        name: state.track.name.clone(),
                        default: state.track.default,
                        forced: state.track.forced,
                        duration_ms: duration,
                        external: false,
                    }),
                    Err(error) => resources
                        .unavailable_tracks
                        .push(UnavailableSubtitleResource {
                            id: state.id,
                            reason: error.to_string(),
                        }),
                }
            }
        }
        subtitles.len()
    } else {
        crate::container::mp4::parse_basic_metadata(source.as_ref())
            .tracks
            .iter()
            .filter(|track| track.kind == crate::container::mp4::Mp4TrackKind::Subtitle)
            .count()
    };
    let next_id = resources
        .tracks
        .iter()
        .map(|track| &track.id)
        .chain(resources.unavailable_tracks.iter().map(|track| &track.id))
        .filter_map(|id| id.strip_prefix('s')?.parse::<usize>().ok())
        .max()
        .map_or(embedded_count, |id| embedded_count.max(id + 1));
    append_external(source, input, output, resources, next_id, &archive)?;
    source.validate_current()?;
    Ok(())
}

fn scan_matroska(
    source: &MediaSource,
    mut consume: impl FnMut(u64, u64, Option<u64>, &[u8]) -> Result<()>,
    selected: &[u64],
) -> Result<()> {
    let mut visits = 0usize;
    let mut offset = 0;
    let segment = loop {
        visit(source, &mut visits)?;
        let element = source.element_at(offset, source.len())?;
        if element.id == 0x1853_8067 {
            break element;
        }
        offset = element.end;
        if offset >= source.len() {
            bail!("Missing Matroska bitmap subtitle segment");
        }
    };
    let mut scale = 1_000_000u64;
    offset = segment.payload;
    while offset < segment.end {
        visit(source, &mut visits)?;
        let element = source.element_at(offset, segment.end)?;
        if element.unknown_size {
            bail!("Unknown-sized bitmap subtitle container child");
        }
        if element.id == 0x1549_a966 {
            let mut child_offset = element.payload;
            while child_offset < element.end {
                visit(source, &mut visits)?;
                let child = source.element_at(child_offset, element.end)?;
                if child.id == 0x002a_d7b1 {
                    scale = uint(source, child)?;
                }
                child_offset = child.end;
            }
        }
        offset = element.end;
    }
    if scale == 0 {
        bail!("Invalid Matroska bitmap subtitle clock");
    }
    offset = segment.payload;
    while offset < segment.end {
        visit(source, &mut visits)?;
        let cluster = source.element_at(offset, segment.end)?;
        if cluster.id == 0x1f43_b675 {
            let mut child_offset = cluster.payload;
            let mut clock = None;
            while child_offset < cluster.end {
                visit(source, &mut visits)?;
                let child = source.element_at(child_offset, cluster.end)?;
                if child.id == 0xe7 {
                    clock = Some(uint(source, child)?);
                    break;
                }
                child_offset = child.end;
            }
            let clock = clock.context("Missing bitmap subtitle cluster time")?;
            child_offset = cluster.payload;
            while child_offset < cluster.end {
                visit(source, &mut visits)?;
                let child = source.element_at(child_offset, cluster.end)?;
                let (mut block, mut duration) = (None, None);
                if child.id == 0xa3 {
                    block = Some(child);
                }
                if child.id == 0xa0 {
                    let mut field_offset = child.payload;
                    while field_offset < child.end {
                        visit(source, &mut visits)?;
                        let field = source.element_at(field_offset, child.end)?;
                        if field.id == 0xa1 {
                            block = Some(field);
                        }
                        if field.id == 0x9b {
                            duration = Some(uint(source, field)?);
                        }
                        field_offset = field.end;
                    }
                }
                if let Some(block) = block {
                    let len = usize::try_from(block.end - block.payload)?;
                    let prefix = source.read_metadata_prefix(block.payload, len.min(12))?;
                    let (number, track_len) =
                        vint(&prefix).context("Invalid bitmap subtitle block track")?;
                    if selected.contains(&number) {
                        if len < track_len + 3 || prefix.len() < track_len + 3 {
                            bail!("Truncated bitmap subtitle block");
                        }
                        if prefix[track_len + 2] & 0x06 != 0 {
                            bail!("Laced bitmap subtitle blocks are unsupported");
                        }
                        let relative =
                            i16::from_be_bytes([prefix[track_len], prefix[track_len + 1]]);
                        let ticks = (i128::from(clock) + i128::from(relative)).max(0) as u128;
                        let time = u64::try_from(
                            ticks
                                .checked_mul(u128::from(scale))
                                .context("Bitmap subtitle clock overflow")?
                                / 1_000_000,
                        )?;
                        let duration = duration
                            .map(|ticks| {
                                u64::try_from(u128::from(ticks) * u128::from(scale) / 1_000_000)
                            })
                            .transpose()?;
                        let payload_len = len - track_len - 3;
                        if payload_len > MAX_ENCODED_BYTES {
                            bail!("Bitmap subtitle packet exceeds size limit");
                        }
                        let bytes = source.read_window_unvalidated(
                            block.payload + (track_len + 3) as u64,
                            payload_len,
                        )?;
                        consume(number, time, duration, &bytes)?;
                    }
                }
                child_offset = child.end;
            }
        }
        offset = cluster.end;
    }
    Ok(())
}

fn visit(source: &MediaSource, count: &mut usize) -> Result<()> {
    source.check_work()?;
    *count += 1;
    if *count > 2_000_000 {
        bail!("Bitmap subtitle scan work budget exceeded");
    }
    Ok(())
}
fn uint(source: &MediaSource, element: Element) -> Result<u64> {
    let len = usize::try_from(element.end - element.payload)?;
    if len == 0 || len > 8 {
        bail!("Invalid bitmap subtitle metadata integer");
    }
    Ok(source
        .read_metadata_prefix(element.payload, len)?
        .into_iter()
        .fold(0, |n, byte| n << 8 | u64::from(byte)))
}
fn vint(bytes: &[u8]) -> Option<(u64, usize)> {
    let first = *bytes.first()?;
    let len = first.leading_zeros() as usize + 1;
    if len > 8 || bytes.len() < len {
        return None;
    }
    let mut value = u64::from(first & (0xff_u8 >> len));
    for byte in &bytes[1..len] {
        value = value << 8 | u64::from(*byte);
    }
    Some((value, len))
}

fn append_external(
    source: &MediaSource,
    input: &Path,
    output: &Path,
    resources: &mut SubtitleResources,
    mut next_id: usize,
    archive: &Rc<ArchiveBudget>,
) -> Result<()> {
    let directory = input
        .parent()
        .context("Bitmap media directory missing")?
        .canonicalize()?;
    let stem = input
        .file_stem()
        .and_then(|name| name.to_str())
        .context("Bitmap media basename invalid")?
        .to_lowercase();
    let prefix = format!("{stem}.");
    let mut candidates = Vec::new();
    for (index, entry) in std::fs::read_dir(&directory)?.enumerate() {
        source.check_work()?;
        if index >= 10_000 {
            bail!("Bitmap sidecar directory entry limit exceeded");
        }
        let entry = entry?;
        let path = entry.path();
        let extension = path
            .extension()
            .and_then(|value| value.to_str())
            .unwrap_or("")
            .to_ascii_lowercase();
        if !["sup", "idx", "sub"].contains(&extension.as_str()) {
            continue;
        }
        let name = path
            .file_stem()
            .and_then(|value| value.to_str())
            .unwrap_or("")
            .to_lowercase();
        if name != stem && !name.starts_with(&prefix) {
            continue;
        }
        let kind = entry.file_type()?;
        if !kind.is_file() || kind.is_symlink() {
            continue;
        }
        if path.canonicalize()?.parent() != Some(directory.as_path()) {
            continue;
        }
        if candidates.len() >= 64 {
            bail!("Bitmap sidecar file count limit exceeded");
        }
        candidates.push((path, extension, name));
    }
    candidates.sort_by(|a, b| a.0.cmp(&b.0));
    let media_duration = if looks_like_ebml(source.as_ref()) {
        parse_basic_metadata(source.as_ref()).duration_ms
    } else {
        crate::container::mp4::parse_basic_metadata(source.as_ref()).duration_ms
    };
    for (path, extension, basename) in &candidates {
        if extension == "sub" {
            continue;
        }
        if extension == "sup" {
            let id = format!("s{next_id}");
            next_id += 1;
            let result = (|| -> Result<(String, u64)> {
                let mut file = open_sidecar(path, &directory)?;
                let before = file.metadata()?;
                if before.len() > MAX_OUTPUT_BYTES as u64 {
                    bail!("PGS sidecar exceeds input limit");
                }
                let mut decoder = pgs::PgsDecoder::default();
                let mut publisher =
                    Publisher::with_source(output, id.clone(), source, Rc::clone(archive));
                let mut position = 0u64;
                let mut segments = 0usize;
                while position < before.len() {
                    source.check_work()?;
                    segments += 1;
                    if segments > 1_000_000 {
                        bail!("PGS sidecar segment count exceeded");
                    }
                    let mut header = [0u8; 13];
                    file.read_exact(&mut header)?;
                    if header[..2] != *b"PG" {
                        bail!("Invalid PGS sidecar magic");
                    }
                    let time =
                        u64::from(u32::from_be_bytes(header[2..6].try_into()?)) * 1000 / 90_000;
                    let len = usize::from(u16::from_be_bytes([header[11], header[12]]));
                    position = position
                        .checked_add(13 + len as u64)
                        .context("PGS sidecar offset overflow")?;
                    if position > before.len() {
                        bail!("PGS sidecar segment exceeds file");
                    }
                    let mut segment = Vec::with_capacity(len + 3);
                    segment.extend_from_slice(&header[10..13]);
                    segment.resize(len + 3, 0);
                    file.read_exact(&mut segment[3..])?;
                    for display in decoder.push_packet(&segment, time)? {
                        publisher.push(display)?;
                    }
                    check_retained(source, decoder.retained_bytes(), &publisher)?;
                }
                decoder.finish()?;
                validate_sidecar(&file, path, &before)?;
                publisher.finish(media_duration)
            })();
            match result {
                Ok((name, duration)) => resources.tracks.push(external_resource(
                    id,
                    "hdmv_pgs_subtitle",
                    name,
                    duration,
                    sidecar_language(basename, &prefix),
                    basename,
                )),
                Err(error) => resources
                    .unavailable_tracks
                    .push(UnavailableSubtitleResource {
                        id,
                        reason: error.to_string(),
                    }),
            }
        } else {
            let parsed = (|| -> Result<dvd::VobSubIndex> {
                let mut file = open_sidecar(path, &directory)?;
                let before = file.metadata()?;
                if before.len() > MAX_ENCODED_BYTES as u64 {
                    bail!("VobSub index exceeds size limit");
                }
                let mut bytes = Vec::new();
                file.by_ref()
                    .take((MAX_ENCODED_BYTES + 1) as u64)
                    .read_to_end(&mut bytes)?;
                if bytes.len() > MAX_ENCODED_BYTES {
                    bail!("VobSub index exceeds size limit");
                }
                validate_sidecar(&file, path, &before)?;
                dvd::parse_idx(&String::from_utf8_lossy(&bytes))
            })();
            let index = match parsed {
                Ok(index) => index,
                Err(error) => {
                    let id = format!("s{next_id}");
                    next_id += 1;
                    resources
                        .unavailable_tracks
                        .push(UnavailableSubtitleResource {
                            id,
                            reason: error.to_string(),
                        });
                    continue;
                }
            };
            let sub = candidates
                .iter()
                .filter(|(_, ext, name)| ext == "sub" && name == basename)
                .collect::<Vec<_>>();
            if sub.len() != 1 {
                for _ in &index.tracks {
                    let id = format!("s{next_id}");
                    next_id += 1;
                    resources
                        .unavailable_tracks
                        .push(UnavailableSubtitleResource {
                            id,
                            reason: "VobSub requires one matching regular SUB sidecar".into(),
                        });
                }
                continue;
            }
            let sub_path = &sub[0].0;
            for track in index.tracks {
                let id = format!("s{next_id}");
                next_id += 1;
                let result = (|| -> Result<(String, u64)> {
                    let mut sub_file = open_sidecar(sub_path, &directory)?;
                    let sub_before = sub_file.metadata()?;
                    let mut decoder =
                        dvd::DvdDecoder::with_palette(index.width, index.height, index.palette);
                    let mut publisher =
                        Publisher::with_source(output, id.clone(), source, Rc::clone(archive));
                    for entry in track.entries {
                        source.check_work()?;
                        if entry.file_position >= sub_before.len() {
                            bail!("VobSub index offset outside SUB file");
                        }
                        let max = usize::try_from(
                            (sub_before.len() - entry.file_position).min(MAX_ENCODED_BYTES as u64),
                        )?;
                        let mut len = max.min(8192);
                        let spu = loop {
                            source.check_work()?;
                            let mut data = vec![0; len];
                            sub_file.seek(SeekFrom::Start(entry.file_position))?;
                            sub_file.read_exact(&mut data)?;
                            match dvd::extract_spu(&data, 0, track.index) {
                                Ok(spu) => break spu,
                                Err(error) if len == max => return Err(error),
                                Err(_) => len = len.saturating_mul(2).min(max),
                            }
                        };
                        for display in decoder.push_packet(&spu, entry.time_ms)? {
                            publisher.push(display)?;
                        }
                        check_retained(source, decoder.retained_bytes(), &publisher)?;
                    }
                    for display in decoder.finish() {
                        publisher.push(display)?;
                    }
                    validate_sidecar(&sub_file, sub_path, &sub_before)?;
                    publisher.finish(media_duration)
                })();
                match result {
                    Ok((name, duration)) => resources.tracks.push(external_resource(
                        id,
                        "dvd_subtitle",
                        name,
                        duration,
                        Some(track.language),
                        basename,
                    )),
                    Err(error) => resources
                        .unavailable_tracks
                        .push(UnavailableSubtitleResource {
                            id,
                            reason: error.to_string(),
                        }),
                }
            }
        }
    }
    Ok(())
}

fn check_retained(
    source: &MediaSource,
    decoder_bytes: usize,
    publisher: &Publisher<'_>,
) -> Result<()> {
    if decoder_bytes
        > source
            .policy()
            .compressed_window_bytes
            .min(128 * 1024 * 1024)
        || publisher.retained_bytes() > source.policy().index_bytes.min(64 * 1024 * 1024)
    {
        bail!("Bitmap subtitle retained state budget exceeded");
    }
    Ok(())
}

fn sidecar_language(name: &str, prefix: &str) -> Option<String> {
    name.strip_prefix(prefix)?
        .split('.')
        .find(|part| {
            (2..=3).contains(&part.len()) && part.bytes().all(|byte| byte.is_ascii_alphabetic())
        })
        .map(str::to_string)
}
fn external_resource(
    id: String,
    codec: &str,
    file_name: String,
    duration_ms: u64,
    language: Option<String>,
    name: &str,
) -> SubtitleScriptResource {
    SubtitleScriptResource {
        id,
        codec: codec.into(),
        file_name,
        web_vtt_file_name: None,
        overlay_file_name: None,
        language,
        name: None,
        default: name
            .split('.')
            .any(|part| part.eq_ignore_ascii_case("default")),
        forced: name
            .split('.')
            .any(|part| part.eq_ignore_ascii_case("forced")),
        duration_ms,
        external: true,
    }
}
fn open_sidecar(path: &Path, directory: &Path) -> Result<File> {
    let mut options = OpenOptions::new();
    options.read(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.custom_flags(libc::O_NOFOLLOW);
    }
    let file = options.open(path)?;
    if !file.metadata()?.is_file()
        || path.symlink_metadata()?.file_type().is_symlink()
        || path.canonicalize()?.parent() != Some(directory)
    {
        bail!("Invalid bitmap sidecar file");
    }
    Ok(file)
}
fn validate_sidecar(file: &File, path: &Path, before: &std::fs::Metadata) -> Result<()> {
    let current = file.metadata()?;
    let named = path.symlink_metadata()?;
    if !named.is_file()
        || named.file_type().is_symlink()
        || current.len() != before.len()
        || current.modified()? != before.modified()?
        || named.len() != before.len()
        || named.modified()? != before.modified()?
    {
        bail!("Bitmap sidecar changed during extraction");
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::MetadataExt;
        if current.ino() != named.ino() || current.dev() != named.dev() {
            bail!("Bitmap sidecar was replaced during extraction");
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    fn display(time: u64, visible: bool) -> DisplaySet {
        DisplaySet {
            time_ms: time,
            canvas_width: 4,
            canvas_height: 3,
            images: if visible {
                vec![BitmapRect {
                    x: 1,
                    y: 1,
                    width: 2,
                    height: 1,
                    rgba: vec![255, 255, 255, 255, 0, 0, 0, 0],
                }]
            } else {
                Vec::new()
            },
        }
    }
    #[test]
    fn bitmap_resources_publish_source_timed_rectangles_png_alpha_and_explicit_clears() {
        let dir = tempfile::tempdir().unwrap();
        let mut publisher = Publisher::new(dir.path(), "s2".into());
        publisher.push(display(1000, true)).unwrap();
        publisher.push(display(2500, false)).unwrap();
        publisher.push(display(5000, true)).unwrap();
        publisher.push(display(6000, false)).unwrap();
        let (name, duration) = publisher.finish(None).unwrap();
        assert_eq!((name.as_str(), duration), ("s2.bitmap.json", 6000));
        let document: serde_json::Value =
            serde_json::from_slice(&std::fs::read(dir.path().join(name)).unwrap()).unwrap();
        assert_eq!(document["schemaVersion"], 1);
        assert_eq!(document["canvasWidth"], 4);
        assert_eq!(document["cues"][0]["startMs"], 1000);
        assert_eq!(document["cues"][0]["endMs"], 2500);
        assert_eq!(document["cues"][1]["startMs"], 5000);
        let image = &document["cues"][0]["images"][0];
        assert_eq!(image["x"], 1);
        assert_eq!(image["y"], 1);
        let bytes = std::fs::read(dir.path().join(image["fileName"].as_str().unwrap())).unwrap();
        let mut reader = png::Decoder::new(std::io::Cursor::new(bytes))
            .read_info()
            .unwrap();
        let mut rgba = vec![0; reader.output_buffer_size().unwrap()];
        let info = reader.next_frame(&mut rgba).unwrap();
        assert_eq!(
            &rgba[..info.buffer_size()],
            [255, 255, 255, 255, 0, 0, 0, 0]
        );
    }
    #[test]
    fn bitmap_publication_rejects_bad_geometry_clock_or_unterminated_cues_and_removes_partial_outputs()
     {
        let dir = tempfile::tempdir().unwrap();
        {
            let mut publisher = Publisher::new(dir.path(), "s0".into());
            publisher.push(display(1000, true)).unwrap();
            assert!(publisher.push(display(999, false)).is_err());
        }
        assert_eq!(std::fs::read_dir(dir.path()).unwrap().count(), 0);
        {
            let mut publisher = Publisher::new(dir.path(), "s0".into());
            publisher.push(display(1000, true)).unwrap();
            assert!(publisher.finish(None).is_err());
        }
        assert_eq!(std::fs::read_dir(dir.path()).unwrap().count(), 0);
        let mut publisher = Publisher::new(dir.path(), "s1".into());
        let mut invalid = display(0, true);
        invalid.images[0].x = 4;
        assert!(publisher.push(invalid).is_err());
        assert!(checked_rgba_len(u32::MAX, u32::MAX).is_err());
        assert!(checked_rgba_len(0, 2).is_err());
    }
    #[test]
    fn bitmap_archive_budget_is_shared_across_tracks_and_failed_track_reservations_roll_back() {
        let dir = tempfile::tempdir().unwrap();
        let budget = Rc::new(ArchiveBudget::new(MAX_OUTPUT_BYTES));
        {
            let mut failed = Publisher::new(dir.path(), "s0".into());
            failed.archive = Rc::clone(&budget);
            failed.push(display(1000, true)).unwrap();
            assert!(budget.used.get() > 0);
            assert!(failed.push(display(999, false)).is_err());
        }
        assert_eq!(budget.used.get(), 0);
        assert_eq!(budget.files.get(), 0);
        assert_eq!(std::fs::read_dir(dir.path()).unwrap().count(), 0);
        let mut complete = Publisher::new(dir.path(), "s0".into());
        complete.archive = Rc::clone(&budget);
        complete.push(display(1000, true)).unwrap();
        complete.push(display(2000, false)).unwrap();
        complete.finish(None).unwrap();
        let reserved = budget.used.get();
        assert!(reserved > 0);
        // A second track must observe archive capacity already owned by other
        // tracks rather than independently acquiring a complete 256 MiB budget.
        budget.used.set(MAX_OUTPUT_BYTES - 1);
        {
            let mut second = Publisher::new(dir.path(), "s1".into());
            second.archive = Rc::clone(&budget);
            assert!(second.push(display(1000, true)).is_err());
        }
        assert_eq!(budget.used.get(), MAX_OUTPUT_BYTES - 1);
        assert!(!dir.path().join("s1-f000001.png").exists());
        assert!(dir.path().join("s0.bitmap.json").is_file());
        budget.used.set(reserved);
        budget.files.set(MAX_ARCHIVE_FILES);
        let mut too_many = Publisher::new(dir.path(), "s2".into());
        too_many.archive = Rc::clone(&budget);
        assert!(too_many.push(display(1000, true)).is_err());
        assert_eq!(budget.used.get(), reserved);
        assert!(!dir.path().join("s2-f000001.png").exists());
    }
    fn element(id: u32, payload: &[u8]) -> Vec<u8> {
        let raw = id.to_be_bytes();
        let first = raw.iter().position(|v| *v != 0).unwrap();
        let mut bytes = raw[first..].to_vec();
        let n = (1..=8)
            .find(|n| (payload.len() as u64) < ((1u64 << (7 * n)) - 1))
            .unwrap();
        let size = (payload.len() as u64) | (1u64 << (7 * n));
        bytes.extend_from_slice(&size.to_be_bytes()[8 - n..]);
        bytes.extend(payload);
        bytes
    }
    fn pgs_segment(kind: u8, body: &[u8]) -> Vec<u8> {
        let mut bytes = vec![kind];
        bytes.extend((body.len() as u16).to_be_bytes());
        bytes.extend(body);
        bytes
    }
    fn pgs_display(visible: bool) -> Vec<u8> {
        let mut pcs = vec![
            0,
            4,
            0,
            3,
            0x10,
            0,
            1,
            0x80,
            0,
            0,
            if visible { 1 } else { 0 },
        ];
        if visible {
            pcs.extend([0, 1, 0, 0, 0, 1, 0, 1]);
        }
        let mut bytes = pgs_segment(0x16, &pcs);
        if visible {
            bytes.extend(pgs_segment(0x14, &[0, 0, 1, 235, 128, 128, 255]));
            bytes.extend(pgs_segment(
                0x15,
                &[0, 1, 0, 0xc0, 0, 0, 8, 0, 2, 0, 1, 1, 1, 0, 0],
            ));
        }
        bytes.extend(pgs_segment(0x80, &[]));
        bytes
    }
    fn block(number: u8, time: i16, packet: &[u8]) -> Vec<u8> {
        let mut data = vec![0x80 | number];
        data.extend(time.to_be_bytes());
        data.push(0);
        data.extend(packet);
        element(0xa3, &data)
    }
    #[test]
    fn embedded_pgs_needs_no_block_duration_and_keeps_distinct_track_ids() {
        for compressed in [false, true] {
            let mut tracks = Vec::new();
            for number in [1u8, 2] {
                let mut data = element(0xd7, &[number]);
                data.extend(element(0x83, &[17]));
                data.extend(element(0x86, b"S_HDMV/PGS"));
                if compressed {
                    let mut compression = element(0x5032, &[3]);
                    compression.extend(element(0x5034, &element(0x4254, &[0])));
                    data.extend(element(0x6d80, &element(0x6240, &compression)));
                }
                data.extend(element(0x0022_b59c, b"eng"));
                tracks.extend(element(0xae, &data));
            }
            let mut segment = element(0x1654_ae6b, &tracks);
            let mut cluster = element(0xe7, &[0]);
            let packet = |visible| {
                let bytes = pgs_display(visible);
                if compressed {
                    use std::io::Write;
                    let mut writer =
                        flate2::write::ZlibEncoder::new(Vec::new(), flate2::Compression::default());
                    writer.write_all(&bytes).unwrap();
                    writer.finish().unwrap()
                } else {
                    bytes
                }
            };
            for number in [1u8, 2] {
                cluster.extend(block(number, 1000, &packet(true)));
                cluster.extend(block(number, 2000, &packet(false)));
            }
            segment.extend(element(0x1f43_b675, &cluster));
            let mut bytes = element(0x1a45_dfa3, &element(0x4282, b"matroska"));
            bytes.extend(element(0x1853_8067, &segment));
            let dir = tempfile::tempdir().unwrap();
            let input = dir.path().join("episode.mkv");
            std::fs::write(&input, bytes).unwrap();
            let source = MediaSource::open_packet_copy(&input).unwrap();
            let output = dir.path().join("out");
            let mut resources = SubtitleResources::default();
            append(&source, &input, &output, &mut resources).unwrap();
            assert!(resources.unavailable_tracks.is_empty());
            assert_eq!(resources.tracks.len(), 2);
            assert_eq!(resources.tracks[0].id, "s0");
            assert_eq!(resources.tracks[1].id, "s1");
            assert_eq!(resources.tracks[0].duration_ms, 2000);
        }
    }
    #[test]
    fn external_sup_uses_90khz_clock_and_never_follows_a_subtitle_symlink() {
        let dir = tempfile::tempdir().unwrap();
        let input = dir.path().join("film.mkv");
        // Reuse a minimal finalized Matroska shell for standalone resources.
        let mut bytes = element(0x1a45_dfa3, &element(0x4282, b"matroska"));
        bytes.extend(element(0x1853_8067, &[]));
        std::fs::write(&input, bytes).unwrap();
        let mut sup = Vec::new();
        for (ms, visible) in [(1000u32, true), (3000, false)] {
            let packets = pgs_display(visible);
            let mut offset = 0;
            while offset < packets.len() {
                let len = usize::from(u16::from_be_bytes([
                    packets[offset + 1],
                    packets[offset + 2],
                ]));
                sup.extend(b"PG");
                sup.extend((ms * 90).to_be_bytes());
                sup.extend(0u32.to_be_bytes());
                sup.extend_from_slice(&packets[offset..offset + 3 + len]);
                offset += 3 + len;
            }
        }
        std::fs::write(dir.path().join("film.eng.sup"), sup).unwrap();
        #[cfg(unix)]
        std::os::unix::fs::symlink("film.eng.sup", dir.path().join("film.bad.sup")).unwrap();
        let source = MediaSource::open_packet_copy(&input).unwrap();
        let mut resources = SubtitleResources::default();
        append(&source, &input, &dir.path().join("out"), &mut resources).unwrap();
        assert_eq!(resources.tracks.len(), 1);
        assert_eq!(resources.tracks[0].language.as_deref(), Some("eng"));
        assert_eq!(resources.tracks[0].duration_ms, 3000);
    }
    #[test]
    fn external_vobsub_pair_publishes_rgba_timing_and_refuses_symlinked_sub() {
        let dir = tempfile::tempdir().unwrap();
        let input = dir.path().join("film.mkv");
        let mut shell = element(0x1a45_dfa3, &element(0x4282, b"matroska"));
        shell.extend(element(0x1853_8067, &[]));
        std::fs::write(&input, shell).unwrap();
        let palette = "000000,ffffff,ff0000,00ff00,000000,000000,000000,000000,000000,000000,000000,000000,000000,000000,000000,000000";
        std::fs::write(dir.path().join("film.en.idx"), format!("size: 720x480\npalette: {palette}\nid: en, index: 0\ntimestamp: 00:00:01:000, filepos: 00000000\n")).unwrap();
        let mut spu = vec![0, 36, 0, 6, 0x90, 0xa0];
        spu.extend([
            0, 0, 0, 30, 3, 0x32, 0x10, 4, 0xff, 0xf0, 5, 0, 0, 1, 0, 0, 1, 6, 0, 4, 0, 5, 1, 0xff,
        ]);
        spu.extend([0, 90, 0, 30, 2, 0xff]);
        let mut pes = vec![0, 0, 1, 0xbd];
        pes.extend(u16::try_from(spu.len() + 4).unwrap().to_be_bytes());
        pes.extend([0x80, 0, 0, 0x20]);
        pes.extend(spu);
        std::fs::write(dir.path().join("film.en.sub"), pes).unwrap();
        let output = dir.path().join("out");
        let resources = crate::write_subtitle_resources(&input, &output).unwrap();
        assert!(resources.unavailable_tracks.is_empty());
        assert_eq!(resources.tracks.len(), 1);
        assert_eq!(resources.tracks[0].codec, "dvd_subtitle");
        assert_eq!(resources.tracks[0].language.as_deref(), Some("en"));
        let document: serde_json::Value = serde_json::from_slice(
            &std::fs::read(output.join(&resources.tracks[0].file_name)).unwrap(),
        )
        .unwrap();
        assert_eq!(document["cues"][0]["startMs"], 1000);
        assert_eq!(document["cues"][0]["endMs"], 2024);
        let bytes = std::fs::read(
            output.join(
                document["cues"][0]["images"][0]["fileName"]
                    .as_str()
                    .unwrap(),
            ),
        )
        .unwrap();
        let mut png = png::Decoder::new(std::io::Cursor::new(bytes))
            .read_info()
            .unwrap();
        let mut rgba = vec![0; png.output_buffer_size().unwrap()];
        let info = png.next_frame(&mut rgba).unwrap();
        assert_eq!(
            &rgba[..info.buffer_size()],
            &[
                255, 255, 255, 255, 255, 255, 255, 255, 255, 0, 0, 255, 255, 0, 0, 255
            ]
        );
        #[cfg(unix)]
        {
            std::fs::rename(
                dir.path().join("film.en.sub"),
                dir.path().join("outside.sub"),
            )
            .unwrap();
            std::os::unix::fs::symlink("outside.sub", dir.path().join("film.en.sub")).unwrap();
            let rejected =
                crate::write_subtitle_resources(&input, &dir.path().join("rejected")).unwrap();
            assert!(rejected.tracks.is_empty());
            assert_eq!(rejected.unavailable_tracks.len(), 1);
        }
    }
}
