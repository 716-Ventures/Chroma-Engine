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
use std::path::Path;

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
                Some(Tx3gDefaults::parse(entry.0, entry.1)?)
            } else {
                None
            };
            let header = if track.codec == "webvtt" {
                webvtt_header(entry.0, entry.1)?
            } else {
                "WEBVTT".to_owned()
            };
            let mut document = hls_header(&header)?;
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
                    if let Some(text) = mov_text_sample(&bytes, defaults)? {
                        document.push_str(&format!(
                            "{} --> {}\n{text}\n\n",
                            text_time(start),
                            text_time(end)
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
            Ok((document, duration_ms))
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

#[derive(Clone, Copy)]
struct Tx3gDefaults {
    font_id: u16,
    font_size: u8,
    face: u8,
    color: [u8; 4],
}
impl Tx3gDefaults {
    fn parse(kind: [u8; 4], payload: &[u8]) -> std::result::Result<Self, String> {
        if kind != *b"tx3g" || payload.len() < 38 {
            return Err("Legacy QuickTime text sample descriptions require native playback".into());
        }
        if payload[8..12].iter().any(|byte| *byte != 0)
            || payload[14..26].iter().any(|byte| *byte != 0)
        {
            return Err("Authored tx3g display effects, backgrounds or text-box positioning require native playback".into());
        }
        if payload[12] != 0 || ![0, 1].contains(&payload[13]) {
            return Err("Authored tx3g justification requires native playback".into());
        }
        let face = payload[32];
        if face & !0x07 != 0 {
            return Err("Unsupported tx3g default face style".into());
        }
        let color: [u8; 4] = payload[34..38]
            .try_into()
            .map_err(|_| "Invalid tx3g default color")?;
        if color != [0, 0, 0, 0] && color != [255, 255, 255, 255] {
            return Err("Authored tx3g default color requires native playback".into());
        }
        Ok(Self {
            font_id: u16::from_be_bytes([payload[30], payload[31]]),
            font_size: payload[33],
            face,
            color,
        })
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
fn mov_text_sample(
    bytes: &[u8],
    defaults: &Tx3gDefaults,
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
    for atom in boxes(&bytes[end..])? {
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
            if font != defaults.font_id
                || record[7] != defaults.font_size
                || record[8..12] != defaults.color
            {
                return Err(
                    "Authored tx3g font, size or color changes require native playback".into(),
                );
            }
            runs.push((start, end, record[6]));
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
    for (start, end, face) in runs {
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
        result.push_str(&styled(&text[start..end], face));
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
        }
    }
    fn sample(text: &[u8]) -> Vec<u8> {
        let mut result = (text.len() as u16).to_be_bytes().to_vec();
        result.extend(text);
        result
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
