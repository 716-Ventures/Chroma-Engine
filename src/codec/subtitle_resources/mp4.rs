//! Native MP4 text compatibility. Timings and Unicode text remain on the source
//! timeline; WebVTT cue markup/settings and ordinary tx3g bold/italic/underline
//! runs are retained. Unsupported tx3g layout/effects are reported per track.
use super::{AGGREGATE_LIMIT, SCRIPT_LIMIT, SubtitleResources, SubtitleScriptResource, text_time};
use crate::{
    container::mp4::{Mp4TrackKind, looks_like_mp4, parse_basic_metadata, parse_packet_track},
    output::publish_bytes,
    source::MediaSource,
};
use anyhow::{Result, bail};
use std::{collections::BTreeMap, path::Path};

pub(super) struct AppendReport {
    pub next_id: usize,
    pub rejected: Vec<(String, String)>,
}

pub(super) fn append(
    source: &MediaSource,
    output: &Path,
    resources: &mut SubtitleResources,
    total: &mut usize,
) -> Result<AppendReport> {
    let mut report = AppendReport {
        next_id: 0,
        rejected: Vec::new(),
    };
    if !looks_like_mp4(source.as_ref()) {
        return Ok(report);
    }
    source.validate_current()?;
    let metadata = parse_basic_metadata(source.as_ref());
    let source_canvas = metadata
        .tracks
        .iter()
        .find(|track| track.kind == Mp4TrackKind::Video)
        .and_then(|track| track.width.zip(track.height))
        .filter(|(width, height)| *width > 0 && *height > 0);
    let tracks = metadata
        .tracks
        .iter()
        .filter(|track| track.kind == Mp4TrackKind::Subtitle)
        .collect::<Vec<_>>();
    if tracks.len() > 128 {
        bail!("MP4 subtitle track limit exceeded");
    }
    report.next_id = tracks.len();
    for (index, track) in tracks.into_iter().enumerate() {
        let id = format!("s{index}");
        if !["mov_text", "webvtt"].contains(&track.codec.as_str()) {
            continue;
        }
        let result = (|| -> std::result::Result<(String, u64), String> {
            let selected = source
                .mp4_metadata_for_tracks(&[&id])
                .map_err(|e| e.to_string())?;
            let packets = parse_packet_track(&selected, Some(&id))
                .ok_or("MP4 subtitle packet index unavailable")?;
            if packets.packets.len() > 100_000 {
                return Err("MP4 subtitle cue count exceeds limit".into());
            }
            let entry = sample_entry(source.as_ref(), packets.track_index as usize)?;
            let tx3g = if track.codec == "mov_text" {
                // A full-frame BoxRecord has nonzero bottom/right bounds and
                // is ordinary tx3g placement, not an authored inset/effect.
                let canvas = tx3g_canvas(source_canvas, track.width.zip(track.height));
                Some(Tx3gDefaults::parse(entry.0, entry.1, canvas)?)
            } else {
                None
            };
            let header = if track.codec == "webvtt" {
                webvtt_header(entry.0, entry.1)?
            } else {
                "WEBVTT".to_owned()
            };
            let mut document = String::new();
            let mut styles = Tx3gStyles::default();
            let mut raw_bytes = 0_usize;
            let mut duration_ms = 0;
            for packet in &packets.packets {
                raw_bytes = raw_bytes
                    .checked_add(packet.size as usize)
                    .ok_or("MP4 subtitle size overflow")?;
                if raw_bytes > SCRIPT_LIMIT {
                    return Err("MP4 subtitle payload exceeds limit".into());
                }
                let bytes = source
                    .read_at(packet.source_offset, packet.size as usize)
                    .map_err(|error| format!("MP4 subtitle packet unavailable: {error}"))?;
                let start = packet.pts.as_millis();
                let end = start.saturating_add(packet.duration.as_millis());
                if end <= start {
                    continue;
                }
                if let Some(defaults) = &tx3g {
                    if let Some(text) = mov_text_sample_with_styles(&bytes, defaults, &mut styles)?
                    {
                        document.push_str(&format!(
                            "{} --> {} {}\n{text}\n\n",
                            text_time(start),
                            text_time(end),
                            defaults.cue_settings()
                        ));
                    }
                } else {
                    document.push_str(&webvtt_sample(&bytes, start, end)?);
                }
                duration_ms = duration_ms.max(end);
                if document.len() > SCRIPT_LIMIT {
                    return Err("MP4 WebVTT document exceeds limit".into());
                }
            }
            let mut complete = hls_header(&header)?;
            if let Some(defaults) = &tx3g {
                complete.push_str(&defaults.stylesheet(&styles));
            }
            complete.push_str(&document);
            if complete.len() > SCRIPT_LIMIT {
                return Err("MP4 WebVTT document exceeds limit".into());
            }
            Ok((complete, duration_ms))
        })();
        let (document, duration_ms) = match result {
            Ok(result) => result,
            Err(reason) => {
                report.rejected.push((id, reason));
                continue;
            }
        };
        *total = total
            .checked_add(document.len())
            .ok_or_else(|| anyhow::anyhow!("MP4 subtitle aggregate overflow"))?;
        if *total > AGGREGATE_LIMIT {
            bail!("subtitle resource aggregate limit exceeded");
        }
        let file_name = format!("{id}.vtt");
        publish_bytes(&output.join(&file_name), document.as_bytes())?;
        resources.tracks.push(SubtitleScriptResource {
            id,
            codec: "webvtt".into(),
            file_name: file_name.clone(),
            web_vtt_file_name: Some(file_name),
            overlay_file_name: None,
            language: track.language.clone(),
            name: track.title.clone(),
            default: track.default,
            forced: track.forced,
            duration_ms,
            external: false,
        });
    }
    source.validate_current()?;
    Ok(report)
}

#[derive(Clone, Copy)]
struct Atom<'a> {
    kind: [u8; 4],
    payload: &'a [u8],
}
fn boxes(mut bytes: &[u8]) -> std::result::Result<Vec<Atom<'_>>, String> {
    let mut result = Vec::new();
    while !bytes.is_empty() {
        if result.len() >= 4096 || bytes.len() < 8 {
            return Err("Malformed or excessive MP4 subtitle boxes".into());
        }
        let size = u32::from_be_bytes(bytes[..4].try_into().map_err(|_| "Invalid MP4 box")?);
        let kind = bytes[4..8].try_into().map_err(|_| "Invalid MP4 box kind")?;
        let (header, size) = if size == 1 {
            if bytes.len() < 16 {
                return Err("Truncated extended MP4 box".into());
            }
            (
                16,
                usize::try_from(u64::from_be_bytes(
                    bytes[8..16]
                        .try_into()
                        .map_err(|_| "Invalid MP4 box size")?,
                ))
                .map_err(|_| "MP4 box size exceeds platform")?,
            )
        } else if size == 0 {
            (8, bytes.len())
        } else {
            (8, size as usize)
        };
        if size < header || size > bytes.len() {
            return Err("MP4 subtitle box outside sample".into());
        }
        result.push(Atom {
            kind,
            payload: &bytes[header..size],
        });
        bytes = &bytes[size..];
    }
    Ok(result)
}
fn child<'a>(bytes: &'a [u8], kind: &[u8; 4]) -> std::result::Result<&'a [u8], String> {
    boxes(bytes)?
        .into_iter()
        .find(|atom| &atom.kind == kind)
        .map(|atom| atom.payload)
        .ok_or_else(|| "MP4 subtitle sample description missing".into())
}
fn sample_entry(bytes: &[u8], track_index: usize) -> std::result::Result<([u8; 4], &[u8]), String> {
    let movie = child(bytes, b"moov")?;
    let tracks = boxes(movie)?
        .into_iter()
        .filter(|atom| atom.kind == *b"trak")
        .collect::<Vec<_>>();
    let track = tracks
        .get(track_index)
        .ok_or("MP4 subtitle track description missing")?
        .payload;
    let table = child(child(child(track, b"mdia")?, b"minf")?, b"stbl")?;
    let description = child(table, b"stsd")?;
    if description.len() < 8
        || u32::from_be_bytes(
            description[4..8]
                .try_into()
                .map_err(|_| "MP4 stsd entry count invalid")?,
        ) != 1
    {
        return Err("Multiple or missing subtitle sample descriptions are unsupported".into());
    }
    let entries = boxes(&description[8..])?;
    let entry = entries.first().ok_or("MP4 subtitle sample entry missing")?;
    Ok((entry.kind, entry.payload))
}
fn hls_header(header: &str) -> std::result::Result<String, String> {
    if header.len() > SCRIPT_LIMIT {
        return Err("WebVTT configuration exceeds limit".into());
    }
    let normalized = header.trim_start_matches('\u{feff}').replace("\r\n", "\n");
    let mut lines = normalized.lines();
    let first = lines
        .next()
        .filter(|line| line.starts_with("WEBVTT"))
        .ok_or("Invalid MP4 WebVTT configuration")?;
    let mut result = format!("{first}\nX-TIMESTAMP-MAP=LOCAL:00:00:00.000,MPEGTS:0\n");
    for line in lines.filter(|line| !line.starts_with("X-TIMESTAMP-MAP")) {
        result.push_str(line);
        result.push('\n');
    }
    result.push('\n');
    Ok(result)
}
fn webvtt_header(kind: [u8; 4], payload: &[u8]) -> std::result::Result<String, String> {
    if kind != *b"wvtt" || payload.len() < 8 {
        return Err("Unsupported MP4 WebVTT sample entry".into());
    }
    let config = child(&payload[8..], b"vttC")?;
    std::str::from_utf8(config)
        .map(str::to_string)
        .map_err(|_| "Invalid UTF-8 WebVTT configuration".into())
}

#[derive(Clone)]
struct Tx3gDefaults {
    font_id: u16,
    font_size: u8,
    face: u8,
    color: [u8; 4],
    background: [u8; 4],
    horizontal: i8,
    vertical: i8,
    canvas: Option<(u32, u32)>,
    fonts: BTreeMap<u16, String>,
}
fn tx3g_canvas(video: Option<(u32, u32)>, text: Option<(u32, u32)>) -> Option<(u32, u32)> {
    let positive = |(width, height): &(u32, u32)| *width > 0 && *height > 0;
    match (video.filter(positive), text.filter(positive)) {
        (Some(video), Some(text)) if video != text => None,
        (Some(video), _) => Some(video),
        (None, text) => text,
    }
}
impl Tx3gDefaults {
    fn parse(
        kind: [u8; 4],
        payload: &[u8],
        canvas: Option<(u32, u32)>,
    ) -> std::result::Result<Self, String> {
        if kind != *b"tx3g" || payload.len() < 38 {
            return Err("Legacy QuickTime text sample descriptions require native playback".into());
        }
        if payload[8..12].iter().any(|byte| *byte != 0) {
            return Err("Authored tx3g display effects require native playback".into());
        }
        full_frame_text_box(&payload[18..26], canvas)?;
        let horizontal = payload[12] as i8;
        let vertical = payload[13] as i8;
        if ![-1, 0, 1].contains(&horizontal) || ![-1, 0, 1].contains(&vertical) {
            return Err("Authored tx3g justification requires native playback".into());
        }
        let face = payload[32];
        if face & !0x07 != 0 {
            return Err("Unsupported tx3g default face style".into());
        }
        let color: [u8; 4] = payload[34..38]
            .try_into()
            .map_err(|_| "Invalid tx3g default color")?;
        let fonts = font_table(&payload[38..])?;
        Ok(Self {
            font_id: u16::from_be_bytes([payload[30], payload[31]]),
            font_size: payload[33],
            face,
            color,
            background: payload[14..18]
                .try_into()
                .map_err(|_| "Invalid tx3g background")?,
            horizontal,
            vertical,
            canvas,
            fonts,
        })
    }

    fn cue_settings(&self) -> String {
        let align = match self.horizontal {
            -1 => "right",
            1 => "center",
            _ => "left",
        };
        let line = match self.vertical {
            -1 => "100%,end",
            1 => "50%,center",
            _ => "0%,start",
        };
        format!("align:{align} line:{line}")
    }

    fn stylesheet(&self, styles: &Tx3gStyles) -> String {
        let mut result = format!(
            "STYLE\n::cue {{ background-color: {}; color: {};",
            css_color(self.background),
            css_color(self.color)
        );
        if let Some(font) = self.fonts.get(&self.font_id) {
            result.push_str(&format!(" font-family: {}, sans-serif;", css_string(font)));
        }
        result.push_str(" }\n");
        for (style, index) in &styles.classes {
            result.push_str(&format!("::cue(.tx3g{index}) {{"));
            if let Some(font) = self.fonts.get(&style.font_id) {
                result.push_str(&format!(" font-family: {}, sans-serif;", css_string(font)));
            }
            if style.font_size != 0 && self.font_size != 0 {
                result.push_str(&format!(
                    " font-size: {:.6}%;",
                    f64::from(style.font_size) / f64::from(self.font_size) * 100.0
                ));
            }
            result.push_str(&format!(" color: {}; }}\n", css_color(style.color)));
        }
        result.push('\n');
        result
    }
}

/// 3GPP TS 26.245 sections 5.16/5.17: signed top/left/bottom/right
/// coordinates are relative to the track's text region, not effect flags.
/// Zero bounds are the common unspecified muxer default. Other authored
/// geometry must remain explicit until the output can preserve its clipping.
fn full_frame_text_box(
    bytes: &[u8],
    canvas: Option<(u32, u32)>,
) -> std::result::Result<(), String> {
    let bytes: &[u8; 8] = bytes
        .try_into()
        .map_err(|_| "Invalid tx3g text-box length")?;
    let values: [i16; 4] =
        std::array::from_fn(|index| i16::from_be_bytes([bytes[index * 2], bytes[index * 2 + 1]]));
    if values == [0; 4] {
        return Ok(());
    }
    let [top, left, bottom, right] = values;
    if top < 0 || left < 0 || bottom <= top || right <= left {
        return Err("Invalid tx3g text-box coordinates".into());
    }
    if top == 0 && left == 0 && canvas == Some((right as u32, bottom as u32)) {
        return Ok(());
    }
    Err("Authored tx3g text-box positioning requires native playback".into())
}

fn font_table(bytes: &[u8]) -> std::result::Result<BTreeMap<u16, String>, String> {
    let mut fonts = BTreeMap::new();
    let mut seen_table = false;
    for atom in boxes(bytes)? {
        if atom.kind != *b"ftab" {
            continue;
        }
        if seen_table || atom.payload.len() < 2 || atom.payload.len() > 64 * 1024 {
            return Err("Invalid tx3g font table".into());
        }
        seen_table = true;
        let count = usize::from(u16::from_be_bytes([atom.payload[0], atom.payload[1]]));
        if count > 256 {
            return Err("tx3g font count exceeds limit".into());
        }
        let mut remaining = &atom.payload[2..];
        for _ in 0..count {
            if remaining.len() < 3 {
                return Err("Truncated tx3g font record".into());
            }
            let id = u16::from_be_bytes([remaining[0], remaining[1]]);
            let length = usize::from(remaining[2]);
            let name = remaining
                .get(3..3 + length)
                .ok_or("Truncated tx3g font name")?;
            let name = std::str::from_utf8(name).map_err(|_| "Invalid UTF-8 tx3g font name")?;
            if name.is_empty() || fonts.insert(id, name.to_string()).is_some() {
                return Err("Empty or duplicate tx3g font record".into());
            }
            remaining = &remaining[3 + length..];
        }
        if !remaining.is_empty() {
            return Err("Trailing tx3g font table bytes".into());
        }
    }
    Ok(fonts)
}

fn css_string(value: &str) -> String {
    let mut result = String::from("\"");
    for ch in value.chars() {
        if ch.is_ascii_alphanumeric() || ch == ' ' {
            result.push(ch);
        } else {
            result.push_str(&format!("\\{:x} ", u32::from(ch)));
        }
    }
    result.push('"');
    result
}

fn css_color([r, g, b, alpha]: [u8; 4]) -> String {
    format!("rgba({r}, {g}, {b}, {:.6})", f64::from(alpha) / 255.0)
}

#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
struct Tx3gStyle {
    font_id: u16,
    font_size: u8,
    color: [u8; 4],
}

#[derive(Default)]
struct Tx3gStyles {
    classes: BTreeMap<Tx3gStyle, usize>,
}

impl Tx3gStyles {
    fn text(
        &mut self,
        text: &str,
        face: u8,
        style: Tx3gStyle,
        defaults: &Tx3gDefaults,
    ) -> std::result::Result<String, String> {
        let text = styled(text, face);
        if style.font_id == defaults.font_id
            && style.font_size == defaults.font_size
            && style.color == defaults.color
        {
            return Ok(text);
        }
        if style.font_id != defaults.font_id && !defaults.fonts.contains_key(&style.font_id) {
            return Err("tx3g style references an unavailable font".into());
        }
        if style.font_size != 0 && defaults.font_size == 0 {
            return Err("tx3g relative font size requires a nonzero default size".into());
        }
        let next = self.classes.len() + 1;
        if next > 4096 && !self.classes.contains_key(&style) {
            return Err("tx3g style count exceeds limit".into());
        }
        let class = self.classes.entry(style).or_insert(next);
        Ok(format!("<c.tx3g{class}>{text}</c>"))
    }
}
fn unicode_text(bytes: &[u8]) -> std::result::Result<String, String> {
    let (bytes, little) = if let Some(bytes) = bytes.strip_prefix(&[0xff, 0xfe]) {
        (bytes, Some(true))
    } else if let Some(bytes) = bytes.strip_prefix(&[0xfe, 0xff]) {
        (bytes, Some(false))
    } else {
        (bytes, None)
    };
    if let Some(little) = little {
        let (units, remainder) = bytes.as_chunks::<2>();
        if !remainder.is_empty() {
            return Err("Incomplete tx3g UTF-16 code unit".into());
        }
        let units = units
            .iter()
            .map(|unit| {
                if little {
                    u16::from_le_bytes(*unit)
                } else {
                    u16::from_be_bytes(*unit)
                }
            })
            .collect::<Vec<_>>();
        String::from_utf16(&units).map_err(|_| "Invalid tx3g UTF-16".into())
    } else {
        std::str::from_utf8(bytes)
            .map(str::to_string)
            .map_err(|_| "Invalid tx3g UTF-8".into())
    }
}
fn escaped(text: &str) -> String {
    text.replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
}
fn styled(text: &str, face: u8) -> String {
    let mut result = escaped(text);
    if face & 0x04 != 0 {
        result = format!("<u>{result}</u>");
    }
    if face & 0x02 != 0 {
        result = format!("<i>{result}</i>");
    }
    if face & 0x01 != 0 {
        result = format!("<b>{result}</b>");
    }
    result
}
#[cfg(test)]
fn mov_text_sample(
    bytes: &[u8],
    defaults: &Tx3gDefaults,
) -> std::result::Result<Option<String>, String> {
    mov_text_sample_with_styles(bytes, defaults, &mut Tx3gStyles::default())
}

fn mov_text_sample_with_styles(
    bytes: &[u8],
    defaults: &Tx3gDefaults,
    styles: &mut Tx3gStyles,
) -> std::result::Result<Option<String>, String> {
    if bytes.len() < 2 {
        return Err("Truncated tx3g sample".into());
    }
    let length = usize::from(u16::from_be_bytes([bytes[0], bytes[1]]));
    let end = 2_usize
        .checked_add(length)
        .filter(|end| *end <= bytes.len())
        .ok_or("tx3g text length exceeds sample")?;
    let text = unicode_text(&bytes[2..end])?;
    if text.is_empty() {
        return Ok(None);
    }
    let utf16_length = text.encode_utf16().count();
    let mut runs = Vec::new();
    let mut text_box = false;
    for atom in boxes(&bytes[end..])? {
        if atom.kind == *b"tbox" {
            if text_box {
                return Err("Duplicate tx3g text-box override".into());
            }
            full_frame_text_box(atom.payload, defaults.canvas)?;
            text_box = true;
            continue;
        }
        if atom.kind != *b"styl" {
            return Err(format!(
                "tx3g {} modifier requires native playback",
                String::from_utf8_lossy(&atom.kind)
            ));
        }
        if atom.payload.len() < 2 {
            return Err("Truncated tx3g style table".into());
        }
        let count = usize::from(u16::from_be_bytes([atom.payload[0], atom.payload[1]]));
        if count > 1024 || runs.len() + count > 1024 || atom.payload.len() != 2 + count * 12 {
            return Err("Invalid tx3g style table length".into());
        }
        for record in atom.payload[2..].as_chunks::<12>().0 {
            let start = usize::from(u16::from_be_bytes([record[0], record[1]]));
            let end = usize::from(u16::from_be_bytes([record[2], record[3]]));
            let font = u16::from_be_bytes([record[4], record[5]]);
            if start >= end || end > utf16_length || record[6] & !0x07 != 0 {
                return Err("Invalid tx3g style character range".into());
            }
            runs.push((
                start,
                end,
                record[6],
                Tx3gStyle {
                    font_id: font,
                    font_size: record[7],
                    color: record[8..12]
                        .try_into()
                        .map_err(|_| "Invalid tx3g style color")?,
                },
            ));
        }
    }
    runs.sort_by_key(|run| run.0);
    if runs.windows(2).any(|pair| pair[0].1 > pair[1].0) {
        return Err("Overlapping tx3g style runs are unsupported".into());
    }
    let mut boundaries = Vec::new();
    let mut utf16 = 0;
    for (byte, ch) in text.char_indices() {
        boundaries.push((utf16, byte));
        utf16 += ch.len_utf16();
    }
    boundaries.push((utf16, text.len()));
    let mut result = String::new();
    let mut previous = 0;
    for (start, end, face, style) in runs {
        let start = boundaries
            .binary_search_by_key(&start, |entry| entry.0)
            .ok()
            .map(|index| boundaries[index].1)
            .ok_or("tx3g style splits a Unicode surrogate pair")?;
        let end = boundaries
            .binary_search_by_key(&end, |entry| entry.0)
            .ok()
            .map(|index| boundaries[index].1)
            .ok_or("tx3g style splits a Unicode surrogate pair")?;
        if start > previous {
            result.push_str(&styled(&text[previous..start], defaults.face));
        }
        result.push_str(&styles.text(&text[start..end], face, style, defaults)?);
        previous = end;
    }
    if previous < text.len() {
        result.push_str(&styled(&text[previous..], defaults.face));
    }
    Ok(Some(result))
}
fn webvtt_sample(bytes: &[u8], start: u64, end: u64) -> std::result::Result<String, String> {
    let mut result = String::new();
    for atom in boxes(bytes)? {
        match &atom.kind {
            b"vtte" => {}
            b"vtta" => {
                let text = std::str::from_utf8(atom.payload)
                    .map_err(|_| "Invalid UTF-8 WebVTT comment")?;
                result.push_str(text);
                result.push_str("\n\n");
            }
            b"vttc" => {
                let mut payload = None;
                let mut settings = "";
                let mut identifier = "";
                for field in boxes(atom.payload)? {
                    let text = || {
                        std::str::from_utf8(field.payload)
                            .map_err(|_| "Invalid UTF-8 MP4 WebVTT cue".to_string())
                    };
                    match &field.kind {
                        b"payl" => payload = Some(text()?),
                        b"sttg" => settings = text()?,
                        b"iden" => identifier = text()?,
                        b"ctim" | b"vsid" => {}
                        _ => return Err("Unsupported MP4 WebVTT cue extension".into()),
                    }
                }
                let payload = payload.ok_or("MP4 WebVTT cue payload missing")?;
                if settings.contains(['\r', '\n']) || identifier.contains(['\r', '\n']) {
                    return Err("Invalid MP4 WebVTT cue metadata".into());
                }
                if !identifier.is_empty() {
                    result.push_str(identifier);
                    result.push('\n');
                }
                result.push_str(&format!(
                    "{} --> {}{}{}\n{payload}\n\n",
                    text_time(start),
                    text_time(end),
                    if settings.is_empty() { "" } else { " " },
                    settings
                ));
            }
            _ => return Err("Unsupported MP4 WebVTT sample box".into()),
        }
    }
    Ok(result)
}

#[cfg(test)]
mod tests {
    use super::*;
    fn atom(kind: &[u8; 4], payload: &[u8]) -> Vec<u8> {
        let mut result = ((payload.len() + 8) as u32).to_be_bytes().to_vec();
        result.extend(kind);
        result.extend(payload);
        result
    }
    fn defaults() -> Tx3gDefaults {
        Tx3gDefaults {
            font_id: 1,
            font_size: 18,
            face: 0,
            color: [255; 4],
            background: [0; 4],
            horizontal: 1,
            vertical: -1,
            canvas: Some((720, 404)),
            fonts: BTreeMap::new(),
        }
    }
    fn centered_bottom_entry() -> Vec<u8> {
        let mut entry = vec![0; 38];
        entry[7] = 1;
        entry[12] = 1;
        entry[13] = 255;
        entry[17] = 255;
        entry[31] = 1;
        entry[33] = 75;
        entry[34..38].fill(255);
        entry
    }
    #[test]
    fn ordinary_centered_bottom_mp4_captions_preserve_alignment_and_background() {
        // Common muxer defaults, matching the reported MP4's tx3g description.
        let entry = centered_bottom_entry();
        let defaults = Tx3gDefaults::parse(*b"tx3g", &entry, None).unwrap();
        assert_eq!(defaults.cue_settings(), "align:center line:100%,end");
        assert!(
            defaults
                .stylesheet(&Tx3gStyles::default())
                .contains("background-color: rgba(0, 0, 0, 1.000000);")
        );
        assert_eq!(
            mov_text_sample(&sample(b"Caption"), &defaults)
                .unwrap()
                .as_deref(),
            Some("Caption")
        );
        for (horizontal, align) in [(255, "right"), (0, "left"), (1, "center")] {
            for (vertical, line) in [(255, "100%,end"), (0, "0%,start"), (1, "50%,center")] {
                let mut entry = entry.clone();
                entry[12] = horizontal;
                entry[13] = vertical;
                assert_eq!(
                    Tx3gDefaults::parse(*b"tx3g", &entry, None)
                        .unwrap()
                        .cue_settings(),
                    format!("align:{align} line:{line}")
                );
            }
        }
        let mut colored = entry.clone();
        colored[14..18].copy_from_slice(&[10, 20, 30, 128]);
        assert!(
            Tx3gDefaults::parse(*b"tx3g", &colored, None)
                .unwrap()
                .stylesheet(&Tx3gStyles::default())
                .contains("rgba(10, 20, 30, 0.501961)")
        );
    }
    #[test]
    fn ordinary_full_frame_text_box_matches_actual_720_by_404_track() {
        // The real corpus track has an identity 720x404 text region, flags=0,
        // transparent background and BoxRecord {0,0,404,720} (TS26.245 5.16).
        let mut entry = centered_bottom_entry();
        entry[14..18].fill(0);
        entry[18..26].copy_from_slice(&[0, 0, 0, 0, 1, 148, 2, 208]);
        entry[33] = 18;
        let defaults = Tx3gDefaults::parse(*b"tx3g", &entry, Some((720, 404))).unwrap();
        // A zero track region is an unspecified size; the source video still
        // supplies the full-frame coordinates used by ordinary captions.
        for region in [None, Some((0, 0)), Some((0, 404)), Some((720, 0))] {
            let canvas = tx3g_canvas(Some((720, 404)), region);
            assert!(Tx3gDefaults::parse(*b"tx3g", &entry, canvas).is_ok());
        }
        assert!(
            Tx3gDefaults::parse(
                *b"tx3g",
                &entry,
                tx3g_canvas(Some((720, 404)), Some((640, 360)))
            )
            .is_err()
        );
        assert_eq!(defaults.cue_settings(), "align:center line:100%,end");
        assert!(
            defaults
                .stylesheet(&Tx3gStyles::default())
                .contains("background-color: rgba(0, 0, 0, 0.000000);")
        );
        let plain = sample(b"Caption");
        let mut boxed = plain.clone();
        boxed.extend(atom(b"tbox", &entry[18..26]));
        assert_eq!(
            mov_text_sample(&plain, &defaults).unwrap(),
            mov_text_sample(&boxed, &defaults).unwrap()
        );
        assert_eq!(
            mov_text_sample(&boxed, &defaults).unwrap().as_deref(),
            Some("Caption")
        );
        assert!(Tx3gDefaults::parse(*b"tx3g", &entry, None).is_err());
        assert!(Tx3gDefaults::parse(*b"tx3g", &entry, Some((1920, 1080))).is_err());
        boxed.extend(atom(b"tbox", &entry[18..26]));
        assert!(
            mov_text_sample(&boxed, &defaults)
                .unwrap_err()
                .contains("Duplicate")
        );
    }
    #[test]
    fn authored_insets_and_invalid_text_box_overrides_remain_explicit() {
        for coordinates in [
            [10i16, 20, 380, 700],
            [-1, 0, 404, 720],
            [0, 0, 0, 720],
            [0, 20, 404, 10],
        ] {
            let record = coordinates
                .into_iter()
                .flat_map(i16::to_be_bytes)
                .collect::<Vec<_>>();
            let mut entry = centered_bottom_entry();
            entry[18..26].copy_from_slice(&record);
            assert!(Tx3gDefaults::parse(*b"tx3g", &entry, Some((720, 404))).is_err());
            let mut cue = sample(b"Text");
            cue.extend(atom(b"tbox", &record));
            assert!(mov_text_sample(&cue, &defaults()).is_err());
        }
        let mut truncated = sample(b"Text");
        truncated.extend(atom(b"tbox", &[0; 7]));
        assert!(mov_text_sample(&truncated, &defaults()).is_err());
    }
    #[test]
    fn unsupported_layout_and_malformed_justification_remain_explicit() {
        let entry = centered_bottom_entry();
        for index in [8, 18, 25] {
            let mut invalid = entry.clone();
            invalid[index] = 1;
            assert!(Tx3gDefaults::parse(*b"tx3g", &invalid, None).is_err());
        }
        for index in [12, 13] {
            let mut invalid = entry.clone();
            invalid[index] = 2;
            assert!(Tx3gDefaults::parse(*b"tx3g", &invalid, None).is_err());
        }
        assert!(Tx3gDefaults::parse(*b"tx3g", &entry[..37], None).is_err());
    }
    fn sample(text: &[u8]) -> Vec<u8> {
        let mut result = (text.len() as u16).to_be_bytes().to_vec();
        result.extend(text);
        result
    }
    fn font_atom(fonts: &[(u16, &str)]) -> Vec<u8> {
        let mut table = (fonts.len() as u16).to_be_bytes().to_vec();
        for (id, name) in fonts {
            table.extend(id.to_be_bytes());
            table.push(name.len() as u8);
            table.extend(name.as_bytes());
        }
        atom(b"ftab", &table)
    }
    fn styled_sample(
        text: &str,
        start: u16,
        end: u16,
        font: u16,
        face: u8,
        size: u8,
        color: [u8; 4],
    ) -> Vec<u8> {
        let mut data = sample(text.as_bytes());
        let mut styles = 1_u16.to_be_bytes().to_vec();
        styles.extend(start.to_be_bytes());
        styles.extend(end.to_be_bytes());
        styles.extend(font.to_be_bytes());
        styles.extend([face, size]);
        styles.extend(color);
        data.extend(atom(b"styl", &styles));
        data
    }
    #[test]
    fn tx3g_font_size_color_and_face_runs_become_bounded_vtt_classes() {
        let mut entry = centered_bottom_entry();
        entry.extend(font_atom(&[(1, "Alte Haas Grotesk"), (2, "Swiss721 BT")]));
        let defaults = Tx3gDefaults::parse(*b"tx3g", &entry, None).unwrap();
        let mut styles = Tx3gStyles::default();
        let data = styled_sample("One 😀", 4, 6, 2, 3, 39, [10, 20, 30, 128]);
        assert_eq!(
            mov_text_sample_with_styles(&data, &defaults, &mut styles)
                .unwrap()
                .as_deref(),
            Some("One <c.tx3g1><b><i>😀</i></b></c>")
        );
        let stylesheet = defaults.stylesheet(&styles);
        assert!(stylesheet.contains("font-family: \"Alte Haas Grotesk\", sans-serif;"));
        assert!(stylesheet.contains("::cue(.tx3g1) { font-family: \"Swiss721 BT\", sans-serif; font-size: 52.000000%; color: rgba(10, 20, 30, 0.501961); }"));
        mov_text_sample_with_styles(&data, &defaults, &mut styles).unwrap();
        assert_eq!(
            styles.classes.len(),
            1,
            "Identical styles must reuse their class"
        );
        assert!(
            mov_text_sample_with_styles(
                &styled_sample("😀", 0, 1, 2, 0, 39, [255; 4]),
                &defaults,
                &mut styles
            )
            .unwrap_err()
            .contains("surrogate")
        );
        assert!(
            mov_text_sample_with_styles(
                &styled_sample("Text", 0, 4, 99, 0, 39, [255; 4]),
                &defaults,
                &mut styles
            )
            .unwrap_err()
            .contains("unavailable font")
        );
    }
    #[test]
    fn tx3g_font_names_cannot_inject_vtt_css_or_header_blocks() {
        let hostile = "Font\";}\n\nSTYLE\n::cue {color:red}\\<x>";
        let mut entry = centered_bottom_entry();
        entry.extend(font_atom(&[(1, hostile)]));
        let defaults = Tx3gDefaults::parse(*b"tx3g", &entry, None).unwrap();
        let stylesheet = defaults.stylesheet(&Tx3gStyles::default());
        assert_eq!(stylesheet.matches("STYLE\n").count(), 1);
        assert_eq!(stylesheet.matches("::cue").count(), 1);
        assert!(!stylesheet.contains("<x>"));
        assert!(stylesheet.contains("\\22 "));
        assert!(stylesheet.contains("\\a "));
        assert!(stylesheet.contains("\\5c "));
        assert_eq!(css_string("東京"), "\"\\6771 \\4eac \"");
    }
    #[test]
    fn tx3g_font_table_and_global_style_budgets_remain_enforced() {
        for table in [
            atom(b"ftab", &[]),
            atom(b"ftab", &[0, 1, 0, 1, 5, b'A']),
            font_atom(&[(1, "A"), (1, "B")]),
            font_atom(&[(1, "")]),
            atom(b"ftab", &[1, 1]),
        ] {
            assert!(font_table(&table).is_err());
        }
        let mut extra = font_atom(&[(1, "A")]);
        extra.extend(font_atom(&[(2, "B")]));
        assert!(font_table(&extra).is_err());
        assert!(font_table(&atom(b"ftab", &[0; 65537])).is_err());
        let mut styles = Tx3gStyles::default();
        let defaults = defaults();
        for index in 0..4096_u32 {
            let mut color = index.to_be_bytes();
            color[3] = color[3].wrapping_add(1);
            styles
                .text(
                    "x",
                    0,
                    Tx3gStyle {
                        font_id: 1,
                        font_size: 18,
                        color,
                    },
                    &defaults,
                )
                .unwrap();
        }
        assert_eq!(styles.classes.len(), 4096);
        assert!(
            styles
                .text(
                    "x",
                    0,
                    Tx3gStyle {
                        font_id: 1,
                        font_size: 18,
                        color: [1, 2, 3, 4]
                    },
                    &defaults
                )
                .unwrap_err()
                .contains("style count")
        );
    }
    #[test]
    fn mov_text_preserves_unicode_and_escapes_markup() {
        assert_eq!(
            mov_text_sample(&sample("A😀 <b>&".as_bytes()), &defaults())
                .unwrap()
                .as_deref(),
            Some("A😀 &lt;b&gt;&amp;")
        );
        assert_eq!(
            mov_text_sample(&sample(&[0xfe, 0xff, 0, 65, 0, 66]), &defaults())
                .unwrap()
                .as_deref(),
            Some("AB")
        );
        assert!(mov_text_sample(&[0, 5, b'x'], &defaults()).is_err());
    }
    #[test]
    fn mov_text_preserves_style_runs_and_rejects_unrepresentable_effects() {
        let mut data = sample(b"One Two");
        let mut styles = 1_u16.to_be_bytes().to_vec();
        styles.extend(4_u16.to_be_bytes());
        styles.extend(7_u16.to_be_bytes());
        styles.extend(1_u16.to_be_bytes());
        styles.extend([3, 18, 255, 255, 255, 255]);
        data.extend(atom(b"styl", &styles));
        assert_eq!(
            mov_text_sample(&data, &defaults()).unwrap().as_deref(),
            Some("One <b><i>Two</i></b>")
        );
        let mut unsupported = sample(b"Text");
        unsupported.extend(atom(b"krok", &[0; 8]));
        assert!(
            mov_text_sample(&unsupported, &defaults())
                .unwrap_err()
                .contains("requires native playback")
        );
    }
    #[test]
    fn webvtt_keeps_cue_identifier_settings_and_source_packet_times() {
        let mut cue = atom(b"iden", b"cue-a");
        cue.extend(atom(b"sttg", b"position:90% align:right"));
        cue.extend(atom(b"payl", b"<b>Authored</b> cue"));
        let document = webvtt_sample(&atom(b"vttc", &cue), 1234, 5678).unwrap();
        assert!(document.contains("cue-a\n00:00:01.234 --> 00:00:05.678 position:90% align:right"));
        assert!(document.contains("<b>Authored</b> cue"));
        assert!(webvtt_sample(&[0, 0, 0, 99, b'v', b't', b't', b'c'], 0, 1).is_err());
    }
    #[test]
    fn webvtt_global_styles_survive_hls_header_generation() {
        let header = hls_header("WEBVTT\n\nSTYLE\n::cue { color: lime; }\n").unwrap();
        assert!(header.contains("X-TIMESTAMP-MAP=LOCAL:00:00:00.000,MPEGTS:0"));
        assert!(header.contains("::cue { color: lime; }"));
    }
}
