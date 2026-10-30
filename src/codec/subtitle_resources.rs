//! Source-timed text, positioned bitmap subtitles, and bounded authored fonts.
use crate::{
    container::matroska::{MatroskaTrackKind, looks_like_ebml, parse_basic_metadata},
    output::publish_bytes,
    source::MediaSource,
};
use anyhow::{Context, Result, bail};
use serde::{Deserialize, Serialize};
use std::path::Path;

mod bitmap;
mod matroska;
mod mp4;
mod sidecars;
mod text_formats;
mod text_overlay;

const SCRIPT_LIMIT: usize = 8 * 1024 * 1024;
const FONT_LIMIT: usize = 8 * 1024 * 1024;
const AGGREGATE_LIMIT: usize = 32 * 1024 * 1024;

/// A subtitle script or native text rendition written beside a resource manifest.
#[derive(Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SubtitleScriptResource {
    /// Stable native track ID.
    pub id: String,
    /// Resource codec: ASS/SSA, text, or a bitmap timeline's original codec.
    pub codec: String,
    /// Safe, generated file name relative to the output folder.
    pub file_name: String,
    /// Native HLS text rendition, when an authored text track can be represented in WebVTT.
    pub web_vtt_file_name: Option<String>,
    /// Optional ASS rendering resource for simultaneous subtitle overlays.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub overlay_file_name: Option<String>,
    /// Track language from the container.
    pub language: Option<String>,
    /// Authored track name.
    pub name: Option<String>,
    /// Authored default selection.
    pub default: bool,
    /// Authored forced-caption flag.
    pub forced: bool,
    /// Last cue end on the source timeline.
    pub duration_ms: u64,
    /// Whether this script was discovered beside the source file.
    pub external: bool,
}
/// A bounded font attachment; the original name is descriptive and never a path.
#[derive(Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SubtitleFontResource {
    /// Authored attachment name.
    pub name: String,
    /// Container MIME type.
    pub mime_type: String,
    /// Safe, generated file name relative to the output folder.
    pub file_name: String,
}
/// Extracted resources for the currently opened source identity.
#[derive(Debug, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SubtitleResources {
    /// Subtitle tracks whose authored features cannot be represented by this transport.
    #[serde(default)]
    pub unavailable_tracks: Vec<UnavailableSubtitleResource>,
    /// Native scripts or text renditions preserving supported styles and source cue times.
    pub tracks: Vec<SubtitleScriptResource>,
    /// Font attachments used by authored styles.
    pub fonts: Vec<SubtitleFontResource>,
}

/// Explicit compatibility boundary for one rejected native subtitle track.
#[derive(Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct UnavailableSubtitleResource {
    /// Stable source track identifier.
    pub id: String,
    /// Concrete native extraction or styling incompatibility.
    pub reason: String,
}

/// Extracts original Matroska scripts, timed text, positioned bitmap resources,
/// and matching external subtitle files.
/// MP4 text captions produce source-timed WebVTT with compatible authored styling;
/// incompatible MP4 features are reported in `unavailable_tracks`. Compressed
/// video/audio payloads are never read.
pub fn write_subtitle_resources(input: &Path, output: &Path) -> Result<SubtitleResources> {
    write_resources(input, output, false, false)
}

/// HLS startup needs native text renditions, not episode-wide bitmap decoding.
pub(crate) fn write_native_text_resources(
    input: &Path,
    output: &Path,
    external_only: bool,
) -> Result<SubtitleResources> {
    write_resources(input, output, true, external_only)
}

fn write_resources(
    input: &Path,
    output: &Path,
    native_text_only: bool,
    external_only: bool,
) -> Result<SubtitleResources> {
    let source = MediaSource::open_packet_copy(input)?;
    let mut resources = SubtitleResources::default();
    if external_only {
        std::fs::create_dir_all(output)?;
        let first_id = if looks_like_ebml(source.as_ref()) {
            parse_basic_metadata(source.as_ref())
                .tracks
                .iter()
                .filter(|t| t.kind == MatroskaTrackKind::Subtitle)
                .count()
        } else {
            crate::container::mp4::parse_basic_metadata(source.as_ref())
                .tracks
                .iter()
                .filter(|t| t.kind == crate::container::mp4::Mp4TrackKind::Subtitle)
                .count()
        };
        sidecars::append(
            input,
            output,
            &mut resources,
            &mut 0,
            first_id,
            Some(&source),
        )?;
        source.validate_current()?;
        return Ok(resources);
    }
    if !looks_like_ebml(source.as_ref()) {
        std::fs::create_dir_all(output)?;
        let mut total = 0;
        let report = mp4::append(&source, output, &mut resources, &mut total)?;
        resources.unavailable_tracks = report
            .rejected
            .into_iter()
            .map(|(id, reason)| UnavailableSubtitleResource { id, reason })
            .collect();
        sidecars::append(
            input,
            output,
            &mut resources,
            &mut total,
            report.next_id,
            Some(&source),
        )?;
        if !native_text_only {
            bitmap::append(&source, input, output, &mut resources)?;
            text_overlay::append(&source, output, &mut resources, &mut total)?;
        }
        source.validate_current()?;
        return Ok(resources);
    }
    let mut metadata = parse_basic_metadata(source.as_ref());
    let compression = bitmap::compression::parse(&source)?;
    let mut invalid_private = std::collections::HashSet::new();
    let mut private_bytes = 0usize;
    for (index, track) in metadata
        .tracks
        .iter_mut()
        .filter(|t| t.kind == MatroskaTrackKind::Subtitle)
        .enumerate()
    {
        if !["ass", "ssa", "webvtt"].contains(&track.codec.as_str())
            || (native_text_only && ["ass", "ssa"].contains(&track.codec.as_str()))
        {
            continue;
        }
        if let (Some(encoding), Some(private)) =
            (compression.get(&track.number), track.codec_private.as_ref())
        {
            match encoding.decode(private, 2, SCRIPT_LIMIT.min(source.policy().metadata_bytes)) {
                Ok(decoded)
                    if private_bytes.saturating_add(decoded.len())
                        <= AGGREGATE_LIMIT.min(source.policy().metadata_bytes) =>
                {
                    private_bytes += decoded.len();
                    track.codec_private = Some(decoded);
                }
                _ => {
                    invalid_private.insert(track.number);
                    resources
                        .unavailable_tracks
                        .push(UnavailableSubtitleResource {
                            id: format!("s{index}"),
                            reason: "Unsupported compressed subtitle initialization".into(),
                        });
                }
            }
        }
    }
    let mut total = 0_usize;
    std::fs::create_dir_all(output)?;
    let selected = metadata
        .tracks
        .iter()
        .filter(|track| {
            track.kind == MatroskaTrackKind::Subtitle
                && ["ass", "ssa", "subrip", "webvtt"].contains(&track.codec.as_str())
                && (!native_text_only || !["ass", "ssa"].contains(&track.codec.as_str()))
                && !invalid_private.contains(&track.number)
        })
        .collect::<Vec<_>>();
    if selected.len() > 128 {
        bail!("subtitle track limit exceeded");
    }
    let mut by_track = if selected.is_empty() {
        Default::default()
    } else {
        matroska::cues(&source, &selected)?
    };
    for (subtitle_index, track) in metadata
        .tracks
        .iter()
        .filter(|track| track.kind == MatroskaTrackKind::Subtitle)
        .enumerate()
    {
        let id = format!("s{subtitle_index}");
        let codec = track.codec.to_ascii_lowercase();
        if !["ass", "ssa", "subrip", "webvtt"].contains(&codec.as_str()) {
            continue;
        }
        let Some(cues) = by_track.remove(&track.number) else {
            continue;
        };
        if cues.is_empty() {
            continue;
        }
        let duration_ms = cues
            .iter()
            .map(|cue| cue.start_ms.saturating_add(cue.duration_ms))
            .max()
            .unwrap_or(0);
        let (file_name, script, web_vtt) = if codec == "ass" || codec == "ssa" {
            let Some(private) = track.codec_private.as_deref() else {
                continue;
            };
            let Ok(header) = std::str::from_utf8(private) else {
                continue;
            };
            if header.len() > SCRIPT_LIMIT {
                bail!("subtitle header size limit exceeded");
            }
            let Ok(mut events) = cues
                .iter()
                .map(|cue| ass_dialogue(&cue.text, cue.start_ms, cue.duration_ms))
                .collect::<Result<Vec<_>>>()
            else {
                continue;
            };
            events.sort_by_key(|(read_order, _)| *read_order);
            (
                format!("{id}.{codec}"),
                assemble_ass(header, &codec, events.into_iter().map(|(_, event)| event)),
                None,
            )
        } else {
            if codec == "webvtt"
                && let Some(private) = track.codec_private.as_deref()
            {
                let Ok(header) = std::str::from_utf8(private) else {
                    continue;
                };
                if !header.is_empty()
                    && !header.trim_start_matches('\u{feff}').starts_with("WEBVTT")
                {
                    continue;
                }
            }
            let vtt = assemble_webvtt(track.codec_private.as_deref(), &cues)?;
            if codec == "subrip" {
                let mut srt = String::new();
                for (index, cue) in cues.iter().enumerate() {
                    srt.push_str(&format!(
                        "{}\n{} --> {}\n{}\n\n",
                        index + 1,
                        text_time(cue.start_ms).replace('.', ","),
                        text_time(cue.start_ms.saturating_add(cue.duration_ms)).replace('.', ","),
                        cue.text
                    ));
                }
                (format!("{id}.srt"), srt, Some(vtt))
            } else {
                (format!("{id}.vtt"), vtt.clone(), Some(vtt))
            }
        };
        if script.len() > SCRIPT_LIMIT {
            bail!("subtitle script size limit exceeded");
        }
        total = total
            .checked_add(script.len() + web_vtt.as_ref().map_or(0, String::len))
            .context("resource size overflow")?;
        if total > AGGREGATE_LIMIT {
            bail!("subtitle resources size limit exceeded");
        }
        publish_bytes(&output.join(&file_name), script.as_bytes())?;
        let web_vtt_file_name = if let Some(vtt) = web_vtt {
            if vtt.len() > SCRIPT_LIMIT {
                bail!("WebVTT script size limit exceeded");
            }
            let name = format!("{id}.vtt");
            publish_bytes(&output.join(&name), vtt.as_bytes())?;
            Some(name)
        } else {
            None
        };
        resources.tracks.push(SubtitleScriptResource {
            id,
            codec,
            file_name,
            web_vtt_file_name,
            overlay_file_name: None,
            language: track.language.clone(),
            name: track.name.clone(),
            default: track.default,
            forced: track.forced,
            duration_ms,
            external: false,
        });
    }
    if !native_text_only {
        extract_fonts(&source, output, &mut resources, &mut total)?;
    }
    let next_id = metadata
        .tracks
        .iter()
        .filter(|track| track.kind == MatroskaTrackKind::Subtitle)
        .count();
    sidecars::append(
        input,
        output,
        &mut resources,
        &mut total,
        next_id,
        Some(&source),
    )?;
    if !native_text_only {
        bitmap::append(&source, input, output, &mut resources)?;
        text_overlay::append(&source, output, &mut resources, &mut total)?;
    }
    source.validate_current()?;
    Ok(resources)
}

fn ass_dialogue(packet: &str, start_ms: u64, duration_ms: u64) -> Result<(u64, String)> {
    // Matroska stores ReadOrder, Layer, Style, Name, margins, Effect, Text;
    // start and end belong to the block, rather than the text payload.
    let fields = packet
        .trim_end_matches('\0')
        .splitn(9, ',')
        .collect::<Vec<_>>();
    if fields.len() != 9 {
        bail!("invalid Matroska ASS event");
    }
    let read_order = fields[0]
        .trim()
        .parse::<u64>()
        .context("invalid ASS read order")?;
    let start = ass_time(start_ms);
    let end = ass_time(start_ms.saturating_add(duration_ms.max(10)));
    Ok((
        read_order,
        format!(
            "Dialogue: {},{start},{end},{}\n",
            fields[1],
            fields[2..].join(",")
        ),
    ))
}

fn assemble_ass(header: &str, codec: &str, events: impl Iterator<Item = String>) -> String {
    let normalized = header.replace("\r\n", "\n");
    let before_events = normalized
        .lines()
        .take_while(|line| !line.trim().eq_ignore_ascii_case("[Events]"))
        .collect::<Vec<_>>()
        .join("\n");
    let layer = if codec == "ssa" { "Marked" } else { "Layer" };
    let mut script = format!(
        "{}\n\n[Events]\nFormat: {layer}, Start, End, Style, Name, MarginL, MarginR, MarginV, Effect, Text\n",
        before_events.trim_end_matches('\0').trim_end()
    );
    for event in events {
        if codec == "ssa" {
            script.push_str(&event.replacen("Dialogue: ,", "Dialogue: Marked=0,", 1));
        } else {
            script.push_str(&event);
        }
    }
    script
}
fn ass_time(ms: u64) -> String {
    format!(
        "{}:{:02}:{:02}.{:02}",
        ms / 3_600_000,
        ms / 60_000 % 60,
        ms / 1000 % 60,
        ms % 1000 / 10
    )
}

fn text_time(ms: u64) -> String {
    format!(
        "{:02}:{:02}:{:02}.{:03}",
        ms / 3_600_000,
        ms / 60_000 % 60,
        ms / 1000 % 60,
        ms % 1000
    )
}
fn assemble_webvtt(private: Option<&[u8]>, cues: &[matroska::Cue]) -> Result<String> {
    let header = private
        .map(std::str::from_utf8)
        .transpose()?
        .unwrap_or("WEBVTT");
    if header.len() > SCRIPT_LIMIT {
        bail!("WebVTT header size limit exceeded");
    }
    let header = header.trim_start_matches('\u{feff}').replace("\r\n", "\n");
    let mut lines = header.lines();
    let signature = lines
        .next()
        .filter(|line| line.starts_with("WEBVTT"))
        .unwrap_or("WEBVTT");
    let mut vtt = format!("{signature}\nX-TIMESTAMP-MAP=LOCAL:00:00:00.000,MPEGTS:0\n");
    for line in lines.filter(|line| !line.starts_with("X-TIMESTAMP-MAP")) {
        vtt.push_str(line);
        vtt.push('\n');
    }
    vtt.push('\n');
    for cue in cues {
        let mut additions = cue.addition.as_deref().unwrap_or("").splitn(3, '\n');
        let settings = additions.next().unwrap_or("").trim_end_matches('\r');
        let identifier = additions.next().unwrap_or("").trim_end_matches('\r');
        let comments = additions.next().unwrap_or("");
        if !comments.is_empty() {
            vtt.push_str(comments);
            vtt.push_str("\n\n");
        }
        if !identifier.is_empty() {
            vtt.push_str(identifier);
            vtt.push('\n');
        }
        let settings = if settings.is_empty() {
            String::new()
        } else {
            format!(" {settings}")
        };
        vtt.push_str(&format!(
            "{} --> {}{settings}\n{}\n\n",
            text_time(cue.start_ms),
            text_time(cue.start_ms.saturating_add(cue.duration_ms)),
            absolute_inline_timestamps(&cue.text, cue.start_ms)
        ));
        if vtt.len() > SCRIPT_LIMIT {
            bail!("WebVTT script size limit exceeded");
        }
    }
    Ok(vtt)
}
// WebVTT timestamps embedded in a Matroska cue are relative to its block.
fn absolute_inline_timestamps(text: &str, start_ms: u64) -> String {
    let mut result = String::new();
    let mut rest = text;
    while let Some(open) = rest.find('<') {
        result.push_str(&rest[..open]);
        let tail = &rest[open + 1..];
        if let Some(close) = tail.find('>') {
            let candidate = &tail[..close];
            let parts = candidate.split(':').collect::<Vec<_>>();
            let timestamp = (|| {
                if parts.len() != 3 {
                    return None;
                }
                let hours = parts[0].parse::<u64>().ok()?;
                let minutes = parts[1].parse::<u64>().ok()?;
                let (seconds, millis) = parts[2].split_once('.')?;
                if minutes >= 60 || seconds.parse::<u64>().ok()? >= 60 || millis.len() != 3 {
                    return None;
                }
                hours
                    .checked_mul(3600000)?
                    .checked_add(minutes.checked_mul(60000)?)?
                    .checked_add(seconds.parse::<u64>().ok()?.checked_mul(1000)?)?
                    .checked_add(millis.parse::<u64>().ok()?)
            })();
            if let Some(timestamp) = timestamp {
                result.push('<');
                result.push_str(&text_time(start_ms.saturating_add(timestamp)));
                result.push('>');
                rest = &tail[close + 1..];
                continue;
            }
        }
        result.push('<');
        rest = tail;
    }
    result.push_str(rest);
    result
}

fn extract_fonts(
    source: &MediaSource,
    output: &Path,
    resources: &mut SubtitleResources,
    total: &mut usize,
) -> Result<()> {
    let mut offset = 0;
    let mut visits = 0;
    let segment = loop {
        visits += 1;
        if visits > source.policy().parse_limits().max_boxes {
            bail!("subtitle element budget exceeded");
        }
        let element = source.element_at(offset, source.len())?;
        if element.id == 0x1853_8067 {
            break element;
        }
        offset = element.end;
        if offset >= source.len() {
            return Ok(());
        }
    };
    let indexed =
        crate::container::matroska::indexed_metadata_elements(source, segment, 0x1941a469)?;
    let mut indexed = indexed.map(|items| items.into_iter());
    offset = segment.payload;
    while offset < segment.end {
        visits += 1;
        if visits > source.policy().parse_limits().max_boxes {
            bail!("subtitle element budget exceeded");
        }
        let element = if let Some(elements) = indexed.as_mut() {
            let Some(element) = elements.next() else {
                break;
            };
            element
        } else {
            source.element_at(offset, segment.end)?
        };
        if element.id == 0x1941_a469 {
            let mut attachment_offset = element.payload;
            while attachment_offset < element.end {
                visits += 1;
                if visits > source.policy().parse_limits().max_boxes {
                    bail!("attachment element budget exceeded");
                }
                let attachment = source.element_at(attachment_offset, element.end)?;
                if attachment.id == 0x61a7 {
                    let mut field_offset = attachment.payload;
                    let mut name = String::new();
                    let mut mime = String::new();
                    let mut data = None;
                    while field_offset < attachment.end {
                        visits += 1;
                        if visits > source.policy().parse_limits().max_boxes {
                            bail!("attachment field budget exceeded");
                        }
                        let field = source.element_at(field_offset, attachment.end)?;
                        match field.id {
                            0x466e if field.end - field.payload <= 512 => {
                                name =
                                    String::from_utf8_lossy(&source.read_at(
                                        field.payload,
                                        (field.end - field.payload) as usize,
                                    )?)
                                    .into()
                            }
                            0x4660 if field.end - field.payload <= 128 => {
                                mime =
                                    String::from_utf8_lossy(&source.read_at(
                                        field.payload,
                                        (field.end - field.payload) as usize,
                                    )?)
                                    .into()
                            }
                            0x465c => data = Some(field),
                            _ => {}
                        }
                        field_offset = field.end;
                    }
                    let lower = name.to_ascii_lowercase();
                    if [".ttf", ".otf", ".ttc"]
                        .iter()
                        .any(|suffix| lower.ends_with(suffix))
                    {
                        if resources.fonts.len() >= 16 {
                            bail!("subtitle font count limit exceeded");
                        }
                        if let Some(data) = data {
                            let len = usize::try_from(data.end - data.payload)?;
                            *total = total
                                .checked_add(len)
                                .context("font resource size overflow")?;
                            if len > FONT_LIMIT || *total > AGGREGATE_LIMIT {
                                bail!("subtitle font resource limit exceeded");
                            }
                            let bytes = source.read_at(data.payload, len)?;
                            let file_name = format!("font-{}.bin", resources.fonts.len());
                            publish_bytes(&output.join(&file_name), &bytes)?;
                            resources.fonts.push(SubtitleFontResource {
                                name,
                                mime_type: mime,
                                file_name,
                            });
                        }
                    }
                }
                attachment_offset = attachment.end;
            }
        }
        offset = element.end;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn ass_reconstruction_preserves_styles_layers_override_tags_and_commas() {
        let header = "[Script Info]\nScriptType: v4.00+\nPlayResX: 1920\n[V4+ Styles]\nStyle: Sign,Authored Font,48\n[Events]\nFormat: old\n";
        let (order, event) = ass_dialogue(
            "42,3,Sign,Actor,0010,0020,0030,Banner,{\\pos(100,200)\\fad(200,300)}Hello, world!",
            3723450,
            2500,
        )
        .unwrap();
        assert_eq!(order, 42);
        assert!(
            event
                .starts_with("Dialogue: 3,1:02:03.45,1:02:05.95,Sign,Actor,0010,0020,0030,Banner,")
        );
        assert!(event.ends_with("{\\pos(100,200)\\fad(200,300)}Hello, world!\n"));
        let script = assemble_ass(header, "ass", [event].into_iter());
        assert!(script.contains("Style: Sign,Authored Font,48"));
        assert_eq!(script.matches("[Events]").count(), 1);
        assert!(!script.contains("Format: old"));
    }
    #[test]
    fn malformed_ass_events_are_rejected() {
        assert!(ass_dialogue("1,2,3", 0, 100).is_err());
        assert!(ass_dialogue("bad,0,Default,,0,0,0,,Text", 0, 100).is_err());
    }
}

#[cfg(test)]
mod extraction_tests {
    use super::*;
    fn element(id: u32, payload: &[u8]) -> Vec<u8> {
        let bytes = id.to_be_bytes();
        let first = bytes.iter().position(|byte| *byte != 0).unwrap_or(3);
        let mut out = bytes[first..].to_vec();
        let len = (1..=8)
            .find(|len| (payload.len() as u64) < ((1_u64 << (7 * len)) - 1))
            .unwrap();
        let encoded = (payload.len() as u64) | (1_u64 << (7 * len));
        out.extend_from_slice(&encoded.to_be_bytes()[8 - len..]);
        out.extend_from_slice(payload);
        out
    }
    fn uint(id: u32, value: u64) -> Vec<u8> {
        element(id, &value.to_be_bytes())
    }
    fn track(number: u64, codec: &str, private: &[u8]) -> Vec<u8> {
        let mut fields = uint(0xd7, number);
        fields.extend(uint(0x83, 17));
        fields.extend(element(0x86, codec.as_bytes()));
        fields.extend(element(0x63a2, private));
        fields.extend(element(0x0022_b59c, b"eng"));
        element(0xae, &fields)
    }
    fn block(number: u8, start: i16, duration: u64, text: &str, addition: Option<&str>) -> Vec<u8> {
        let mut body = vec![0x80 | number];
        body.extend(start.to_be_bytes());
        body.push(0);
        body.extend(text.as_bytes());
        let mut group = element(0xa1, &body);
        group.extend(uint(0x9b, duration));
        if let Some(addition) = addition {
            let mut more = uint(0xee, 1);
            more.extend(element(0xa5, addition.as_bytes()));
            group.extend(element(0x75a1, &element(0xa6, &more)));
        }
        element(0xa0, &group)
    }
    fn fixture(font_size: usize) -> Vec<u8> {
        fixture_with_video_blocks(font_size, 0)
    }
    fn fixture_with_video_blocks(font_size: usize, video_blocks: usize) -> Vec<u8> {
        let header = b"[Script Info]\nScriptType: v4.00+\nPlayResX: 1920\n[V4+ Styles]\nFormat: Name,Fontname,Fontsize\nStyle: Default,Authored Font,48\n";
        let mut tracks = track(1, "S_TEXT/ASS", header);
        tracks.extend(track(2, "S_TEXT/UTF8", b""));
        tracks.extend(track(
            3,
            "S_TEXT/WEBVTT",
            b"WEBVTT\n\nSTYLE\n::cue { color: lime; }\n",
        ));
        let mut video_track = uint(0xd7, 4);
        video_track.extend(uint(0x83, 1));
        video_track.extend(element(0x86, b"V_MPEG4/ISO/AVC"));
        tracks.extend(element(0xae, &video_track));
        let mut segment = element(0x1654_ae6b, &tracks);
        let mut cluster = uint(0xe7, 0);
        for _ in 0..video_blocks {
            cluster.extend(element(0xa3, &[0x84, 0, 0, 0, 0x65]));
        }
        cluster.extend(block(
            1,
            1000,
            2500,
            "2,3,Default,Actor,0,0,0,,{\\pos(20,30)}Sign, text",
            None,
        ));
        cluster.extend(block(
            1,
            2000,
            500,
            "1,0,Default,,0,0,0,,Overlapping dialogue",
            None,
        ));
        cluster.extend(block(2, 10000, 1500, "Hello <i>world</i>\nNext line", None));
        cluster.extend(block(
            3,
            5000,
            2000,
            "WebVTT <00:00:01.000>word",
            Some("position:90% align:right\ncue-id\nNOTE authored comment"),
        ));
        segment.extend(element(0x1f43_b675, &cluster));
        let mut font = element(0x466e, b"../../Authored.ttf");
        font.extend(element(0x4660, b"font/ttf"));
        font.extend(element(0x465c, &vec![0; font_size]));
        segment.extend(element(0x1941_a469, &element(0x61a7, &font)));
        let mut bytes = element(0x1a45_dfa3, &element(0x4282, b"matroska"));
        bytes.extend(element(0x1853_8067, &segment));
        bytes
    }
    fn indexed_fixture(missing_track: bool, invalid_position: bool) -> Vec<u8> {
        use crate::source::probe_reader::{Element, element_from_header};
        fn children(bytes: &[u8], start: u64, end: u64) -> Vec<(u64, Element)> {
            let mut offset = start;
            let mut out = Vec::new();
            while offset < end {
                let e = element_from_header(&bytes[offset as usize..], offset, end).unwrap();
                out.push((offset, e));
                offset = e.end;
            }
            out
        }
        let bytes = fixture_with_video_blocks(32, 2000);
        let (_, segment) = children(&bytes, 0, bytes.len() as u64)
            .into_iter()
            .find(|(_, e)| e.id == 0x18538067)
            .unwrap();
        let items = children(&bytes, segment.payload, segment.end);
        let (tracks_start, tracks) = items
            .iter()
            .find(|(_, e)| e.id == 0x1654ae6b)
            .copied()
            .unwrap();
        let (cluster_start, cluster) = items
            .iter()
            .find(|(_, e)| e.id == 0x1f43b675)
            .copied()
            .unwrap();
        let (font_start, font) = items
            .iter()
            .find(|(_, e)| e.id == 0x1941a469)
            .copied()
            .unwrap();
        let info = element(0x1549a966, &[]);
        let track_bytes = &bytes[tracks_start as usize..tracks.end as usize];
        let cluster_bytes = &bytes[cluster_start as usize..cluster.end as usize];
        let font_bytes = &bytes[font_start as usize..font.end as usize];
        let head = |positions: [u64; 4]| {
            element(
                0x114d9b74,
                &[0x1549a966_u32, 0x1654ae6b, 0x1941a469, 0x1c53bb6b]
                    .into_iter()
                    .zip(positions)
                    .flat_map(|(id, pos)| {
                        element(
                            0x4dbb,
                            &[element(0x53ab, &id.to_be_bytes()), uint(0x53ac, pos)].concat(),
                        )
                    })
                    .collect::<Vec<_>>(),
            )
        };
        let info_pos = head([0; 4]).len();
        let tracks_pos = info_pos + info.len();
        let cluster_pos = tracks_pos + track_bytes.len();
        let font_pos = cluster_pos + cluster_bytes.len();
        let cues_pos = font_pos + font_bytes.len();
        let mut cues = Vec::new();
        for (offset, child) in children(&bytes, cluster.payload, cluster.end)
            .into_iter()
            .filter(|(_, e)| e.id == 0xa0)
        {
            let fields = children(&bytes, child.payload, child.end);
            let (_, block) = fields.iter().find(|(_, e)| e.id == 0xa1).copied().unwrap();
            let number = bytes[block.payload as usize] & 0x7f;
            if missing_track && number == 3 {
                continue;
            }
            let time = i16::from_be_bytes(
                bytes[block.payload as usize + 1..block.payload as usize + 3]
                    .try_into()
                    .unwrap(),
            ) as u64;
            let (_, duration) = fields.iter().find(|(_, e)| e.id == 0x9b).copied().unwrap();
            let duration = u64::from_be_bytes(
                bytes[duration.payload as usize..duration.end as usize]
                    .try_into()
                    .unwrap(),
            );
            let relative = if invalid_position && number == 2 {
                u64::MAX
            } else {
                offset - cluster.payload
            };
            cues.push((
                time,
                element(
                    0xbb,
                    &[
                        uint(0xb3, time),
                        element(
                            0xb7,
                            &[
                                uint(0xf7, u64::from(number)),
                                uint(0xf1, cluster_pos as u64),
                                uint(0xf0, relative),
                                uint(0xb2, duration),
                            ]
                            .concat(),
                        ),
                    ]
                    .concat(),
                ),
            ));
        }
        cues.sort_by_key(|(t, _)| *t);
        let cues = element(
            0x1c53bb6b,
            &cues.into_iter().flat_map(|(_, b)| b).collect::<Vec<_>>(),
        );
        [
            element(0x1a45dfa3, &element(0x4282, b"matroska")),
            element(
                0x18538067,
                &[
                    head([
                        info_pos as u64,
                        tracks_pos as u64,
                        font_pos as u64,
                        cues_pos as u64,
                    ]),
                    info,
                    track_bytes.to_vec(),
                    cluster_bytes.to_vec(),
                    font_bytes.to_vec(),
                    cues,
                ]
                .concat(),
            ),
        ]
        .concat()
    }
    #[test]
    fn subtitle_seek_index_preserves_all_styles_additions_fonts_and_falls_back_on_bad_references() {
        let dir = tempfile::tempdir().unwrap();
        let original = dir.path().join("original.mkv");
        std::fs::write(&original, fixture_with_video_blocks(32, 2000)).unwrap();
        let expected = dir.path().join("expected");
        let old = write_subtitle_resources(&original, &expected).unwrap();
        for (name, missing, invalid) in [
            ("indexed", false, false),
            ("missing-track", true, false),
            ("invalid-position", false, true),
        ] {
            let input = dir.path().join(format!("{name}.mkv"));
            std::fs::write(&input, indexed_fixture(missing, invalid)).unwrap();
            let output = dir.path().join(name);
            let current = write_subtitle_resources(&input, &output).unwrap();
            assert_eq!(
                serde_json::to_value(&current).unwrap(),
                serde_json::to_value(&old).unwrap(),
                "{name}"
            );
            for entry in std::fs::read_dir(&expected).unwrap() {
                let entry = entry.unwrap();
                assert_eq!(
                    std::fs::read(entry.path()).unwrap(),
                    std::fs::read(output.join(entry.file_name())).unwrap(),
                    "{name}"
                );
            }
        }
    }

    #[test]
    fn positional_resources_preserve_authored_duration_styles_webvtt_metadata_and_font_paths() {
        let dir = tempfile::tempdir().unwrap();
        let input = dir.path().join("test.mkv");
        let output = dir.path().join("output");
        std::fs::write(&input, fixture(32)).unwrap();
        let resources = write_subtitle_resources(&input, &output).unwrap();
        assert_eq!(resources.tracks.len(), 3);
        let ass = std::fs::read_to_string(output.join("s0.ass")).unwrap();
        assert!(ass.contains("0:00:01.00,0:00:03.50"));
        assert!(ass.find("Overlapping dialogue").unwrap() < ass.find("Sign, text").unwrap());
        assert!(ass.contains("{\\pos(20,30)}Sign, text"));
        let srt = std::fs::read_to_string(output.join("s1.srt")).unwrap();
        assert!(srt.contains("00:00:10,000 --> 00:00:11,500"));
        let vtt = std::fs::read_to_string(output.join("s2.vtt")).unwrap();
        assert!(vtt.contains("X-TIMESTAMP-MAP=LOCAL:00:00:00.000,MPEGTS:0"));
        assert!(vtt.contains("::cue { color: lime; }"));
        assert!(vtt.contains("cue-id\n00:00:05.000 --> 00:00:07.000 position:90% align:right"));
        assert!(vtt.contains("<00:00:06.000>word"));
        assert!(vtt.contains("NOTE authored comment"));
        assert_eq!(resources.fonts[0].file_name, "font-0.bin");
        assert!(output.join("font-0.bin").is_file());
        assert!(!dir.path().join("Authored.ttf").exists());
    }
    #[test]
    fn malformed_ass_track_does_not_remove_valid_native_text_tracks() {
        let dir = tempfile::tempdir().unwrap();
        let input = dir.path().join("malformed-ass.mkv");
        let mut bytes = fixture(32);
        let offset = bytes
            .windows(11)
            .position(|window| window == b"2,3,Default")
            .unwrap();
        bytes[offset] = b'x';
        std::fs::write(&input, bytes).unwrap();
        let resources = write_subtitle_resources(&input, &dir.path().join("output")).unwrap();
        assert_eq!(
            resources
                .tracks
                .iter()
                .map(|track| track.id.as_str())
                .collect::<Vec<_>>(),
            ["s1", "s2"]
        );
    }

    #[test]
    fn malformed_webvtt_header_and_zero_srt_duration_are_isolated() {
        let dir = tempfile::tempdir().unwrap();
        let input = dir.path().join("mixed.mkv");
        let mut bytes = fixture(32);
        let header = bytes
            .windows(6)
            .position(|window| window == b"WEBVTT")
            .unwrap();
        bytes[header] = 0xff;
        let duration = uint(0x9b, 1500);
        let offset = bytes
            .windows(duration.len())
            .position(|window| window == duration)
            .unwrap();
        bytes[offset + 2..offset + duration.len()].fill(0);
        std::fs::write(&input, bytes).unwrap();
        let resources = write_subtitle_resources(&input, &dir.path().join("output")).unwrap();
        assert_eq!(
            resources
                .tracks
                .iter()
                .map(|track| track.id.as_str())
                .collect::<Vec<_>>(),
            ["s0"]
        );
    }

    #[test]
    fn feature_length_video_does_not_consume_the_metadata_box_budget() {
        let dir = tempfile::tempdir().unwrap();
        let input = dir.path().join("feature.mkv");
        std::fs::write(&input, fixture_with_video_blocks(32, 130_000)).unwrap();
        let resources = write_subtitle_resources(&input, &dir.path().join("output")).unwrap();
        assert_eq!(resources.tracks.len(), 3);
    }

    #[test]
    fn oversized_font_is_rejected_before_attachment_read() {
        let dir = tempfile::tempdir().unwrap();
        let input = dir.path().join("large-font.mkv");
        std::fs::write(&input, fixture(FONT_LIMIT + 1)).unwrap();
        let error = write_subtitle_resources(&input, &dir.path().join("output")).unwrap_err();
        assert!(error.to_string().contains("font resource limit"));
    }
}
