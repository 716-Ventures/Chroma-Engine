//! Text renditions have an independent ASS representation for simultaneous tracks.
//! Native single-track WebVTT remains available; this never replaces authored ASS.
use super::{AGGREGATE_LIMIT, SCRIPT_LIMIT, SubtitleResources};
use crate::{output::publish_bytes, source::MediaSource};
use anyhow::{Context, Result, bail};
use std::{collections::BTreeMap, path::Path};

#[derive(Clone)]
struct Style {
    font: String,
    size: f64,
    color: String,
    bold: bool,
    italic: bool,
    underline: bool,
}
impl Default for Style {
    fn default() -> Self {
        Self {
            font: "Arial".into(),
            size: 48.0,
            color: "&H00FFFFFF&".into(),
            bold: false,
            italic: false,
            underline: false,
        }
    }
}
impl Style {
    fn tags(&self) -> String {
        format!(
            "{{\\fn{}\\fs{:.2}\\c{}\\b{}\\i{}\\u{}}}",
            self.font,
            self.size,
            self.color,
            u8::from(self.bold),
            u8::from(self.italic),
            u8::from(self.underline)
        )
    }
}

pub(super) fn append(
    source: &MediaSource,
    directory: &Path,
    resources: &mut SubtitleResources,
    total: &mut usize,
) -> Result<()> {
    let (width, height) = if crate::container::matroska::looks_like_ebml(source.as_ref()) {
        let metadata = crate::container::matroska::parse_basic_metadata(source.as_ref());
        metadata
            .tracks
            .iter()
            .find_map(|t| Some((t.width?, t.height?)))
            .unwrap_or((1920, 1080))
    } else {
        let metadata = crate::container::mp4::parse_basic_metadata(source.as_ref());
        metadata
            .tracks
            .iter()
            .find_map(|t| Some((t.width?, t.height?)))
            .unwrap_or((1920, 1080))
    };
    for track in &mut resources.tracks {
        source.check_work()?;
        if matches!(track.codec.as_str(), "ass" | "ssa") {
            track.overlay_file_name = Some(track.file_name.clone());
            continue;
        }
        let Some(file) = &track.web_vtt_file_name else {
            continue;
        };
        let path = directory.join(file);
        if std::fs::metadata(&path)?.len() > SCRIPT_LIMIT as u64 {
            bail!("text overlay input exceeds limit");
        }
        let text = std::fs::read_to_string(path)?;
        let Ok(script) = webvtt_to_ass(&text, width, height) else {
            continue;
        };
        *total = total
            .checked_add(script.len())
            .context("text overlay aggregate overflow")?;
        if *total > AGGREGATE_LIMIT {
            bail!("subtitle resource aggregate size limit exceeded");
        }
        let name = format!("{}.overlay.ass", track.id);
        publish_bytes(&directory.join(&name), script.as_bytes())?;
        track.overlay_file_name = Some(name);
    }
    Ok(())
}

pub(super) fn webvtt_to_ass(text: &str, width: u32, height: u32) -> Result<String> {
    if text.len() > SCRIPT_LIMIT || width == 0 || height == 0 || width > 16384 || height > 16384 {
        bail!("invalid text overlay geometry or size");
    }
    let normalized = text
        .trim_start_matches('\u{feff}')
        .replace("\r\n", "\n")
        .replace('\r', "\n");
    if !normalized.starts_with("WEBVTT") {
        bail!("invalid WebVTT overlay input");
    }
    let mut base = Style::default();
    let mut classes = BTreeMap::new();
    for block in normalized
        .split("\n\n")
        .filter(|b| b.trim_start().starts_with("STYLE"))
    {
        parse_css(block, &mut base, &mut classes)?;
    }
    let mut output = format!(
        "[Script Info]\nScriptType: v4.00+\nPlayResX: {width}\nPlayResY: {height}\nScaledBorderAndShadow: yes\n\n[V4+ Styles]\nFormat: Name,Fontname,Fontsize,PrimaryColour,SecondaryColour,OutlineColour,BackColour,Bold,Italic,Underline,StrikeOut,ScaleX,ScaleY,Spacing,Angle,BorderStyle,Outline,Shadow,Alignment,MarginL,MarginR,MarginV,Encoding\nStyle: Default,{},48,&H00FFFFFF,&H00FFFFFF,&H00000000,&H80000000,0,0,0,0,100,100,0,0,1,2,0,2,40,40,40,1\n\n[Events]\nFormat: Layer,Start,End,Style,Name,MarginL,MarginR,MarginV,Effect,Text\n",
        base.font
    );
    let mut cues = 0;
    for block in normalized.split("\n\n") {
        let lines = block.lines().collect::<Vec<_>>();
        if ["WEBVTT", "NOTE", "STYLE", "REGION"]
            .iter()
            .any(|prefix| block.trim_start().starts_with(prefix))
        {
            continue;
        }
        let Some((index, timing)) = lines
            .iter()
            .enumerate()
            .find(|(_, line)| line.contains("-->"))
        else {
            continue;
        };
        let Some((left, right)) = timing.split_once("-->") else {
            continue;
        };
        let mut right = right.split_whitespace();
        let start = timestamp_ms(left.trim()).context("invalid subtitle start")?;
        let end = timestamp_ms(right.next().unwrap_or("")).context("invalid subtitle end")?;
        if end <= start {
            continue;
        }
        cues += 1;
        if cues > 100_000 {
            bail!("subtitle overlay cue limit exceeded");
        }
        let settings = right.collect::<Vec<_>>();
        let alignment = alignment(&settings);
        let body = lines[index + 1..].join("\n");
        let body = markup(&body, &base, &classes)?;
        output.push_str(&format!(
            "Dialogue: 0,{},{},Default,,0,0,0,,{{\\an{alignment}}}{}{body}\n",
            ass_time(start),
            ass_time(end),
            base.tags()
        ));
        if output.len() > SCRIPT_LIMIT {
            bail!("subtitle overlay script size limit exceeded");
        }
    }
    if cues == 0 {
        bail!("subtitle overlay has no timed cues");
    }
    Ok(output)
}

fn alignment(settings: &[&str]) -> u8 {
    let col = settings
        .iter()
        .find_map(|v| v.strip_prefix("align:"))
        .map(|v| match v {
            "left" | "start" => 0,
            "right" | "end" => 2,
            _ => 1,
        })
        .unwrap_or(1);
    let row = settings
        .iter()
        .find_map(|v| v.strip_prefix("line:"))
        .and_then(|v| v.split('%').next()?.parse::<f64>().ok())
        .map(|v| {
            if v <= 20.0 {
                6
            } else if v < 80.0 {
                3
            } else {
                0
            }
        })
        .unwrap_or(0);
    1 + row + col
}

pub(super) fn timestamp_ms(value: &str) -> Option<u64> {
    let parts = value.split(':').collect::<Vec<_>>();
    if !(2..=3).contains(&parts.len()) {
        return None;
    }
    let seconds = parts.last()?.replace(',', ".").parse::<f64>().ok()?;
    let minutes = parts[parts.len() - 2].parse::<u64>().ok()?;
    let hours = if parts.len() == 3 {
        parts[0].parse::<u64>().ok()?
    } else {
        0
    };
    if !seconds.is_finite()
        || !(0.0..60.0).contains(&seconds)
        || (parts.len() == 3 && minutes >= 60)
    {
        return None;
    }
    let result = hours
        .checked_mul(3_600_000)?
        .checked_add(minutes.checked_mul(60_000)?)?
        .checked_add((seconds * 1000.0).round() as u64)?;
    (result <= 30 * 24 * 3600 * 1000).then_some(result)
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

fn parse_css(block: &str, base: &mut Style, classes: &mut BTreeMap<String, Style>) -> Result<()> {
    for rule in block.split('}') {
        let Some((selector, declarations)) = rule.rsplit_once('{') else {
            continue;
        };
        let selector = selector.trim().trim_start_matches("STYLE").trim();
        let name = if selector == "::cue" {
            None
        } else if let Some(name) = selector
            .strip_prefix("::cue(.")
            .and_then(|s| s.strip_suffix(')'))
        {
            Some(name)
        } else {
            continue;
        };
        if name.is_some_and(|v| {
            v.len() > 128
                || !v
                    .bytes()
                    .all(|c| c.is_ascii_alphanumeric() || matches!(c, b'-' | b'_'))
        }) {
            continue;
        }
        let mut style = base.clone();
        for declaration in declarations.split(';') {
            let Some((key, value)) = declaration.split_once(':') else {
                continue;
            };
            let value = value.trim();
            match key.trim() {
                "font-family" => {
                    let family = css_font(value);
                    if !family.is_empty() {
                        style.font = family;
                    }
                }
                "font-size" => {
                    if let Some(percent) = value
                        .strip_suffix('%')
                        .and_then(|v| v.parse::<f64>().ok())
                        .filter(|v| v.is_finite() && *v > 0.0 && *v <= 1000.0)
                    {
                        style.size = base.size * percent / 100.0;
                    } else if let Some(size) = value
                        .strip_suffix("px")
                        .and_then(|v| v.parse::<f64>().ok())
                        .filter(|v| v.is_finite() && *v > 0.0 && *v <= 512.0)
                    {
                        style.size = size;
                    }
                }
                "color" => {
                    if let Some(color) = color(value) {
                        style.color = color;
                    }
                }
                "font-weight" => {
                    style.bold = value == "bold" || value.parse::<u16>().is_ok_and(|v| v >= 600)
                }
                "font-style" => style.italic = value == "italic",
                "text-decoration" => style.underline = value.contains("underline"),
                _ => {}
            }
        }
        if let Some(name) = name {
            if classes.len() >= 4096 {
                bail!("subtitle CSS class limit exceeded");
            }
            classes.insert(name.into(), style);
        } else {
            *base = style;
        }
    }
    Ok(())
}
fn css_font(value: &str) -> String {
    let value = value.trim();
    let raw = if let Some(quote) = value.chars().next().filter(|v| matches!(v, '\'' | '"')) {
        value[1..].split(quote).next().unwrap_or("")
    } else {
        value.split(',').next().unwrap_or("").trim()
    };
    let mut decoded = String::new();
    let mut chars = raw.chars().peekable();
    while let Some(ch) = chars.next() {
        if ch == '\\' {
            let hex = chars
                .by_ref()
                .take_while(|v| v.is_ascii_hexdigit())
                .take(6)
                .collect::<String>();
            if let Some(ch) = u32::from_str_radix(&hex, 16).ok().and_then(char::from_u32) {
                decoded.push(ch);
            }
        } else {
            decoded.push(ch);
        }
    }
    decoded
        .chars()
        .filter(|c| !c.is_control() && !matches!(c, '\\' | '{' | '}' | ','))
        .take(128)
        .collect()
}
fn color(value: &str) -> Option<String> {
    let lower = value.to_ascii_lowercase();
    let rgba = if let Some(hex) = lower
        .strip_prefix('#')
        .filter(|h| h.len() == 6 || h.len() == 8)
    {
        let v = u32::from_str_radix(hex, 16).ok()?;
        if hex.len() == 6 {
            [(v >> 16) as u8, (v >> 8) as u8, v as u8, 255]
        } else {
            [(v >> 24) as u8, (v >> 16) as u8, (v >> 8) as u8, v as u8]
        }
    } else if let Some(body) = lower
        .strip_prefix("rgba(")
        .and_then(|v| v.strip_suffix(')'))
    {
        let parts = body.split(',').map(str::trim).collect::<Vec<_>>();
        if parts.len() != 4 {
            return None;
        }
        let alpha = parts[3].parse::<f64>().ok()?;
        if !alpha.is_finite() || !(0.0..=1.0).contains(&alpha) {
            return None;
        }
        [
            parts[0].parse().ok()?,
            parts[1].parse().ok()?,
            parts[2].parse().ok()?,
            (alpha * 255.0).round() as u8,
        ]
    } else {
        match lower.as_str() {
            "white" => [255, 255, 255, 255],
            "black" => [0, 0, 0, 255],
            "red" => [255, 0, 0, 255],
            "lime" => [0, 255, 0, 255],
            "green" => [0, 128, 0, 255],
            "blue" => [0, 0, 255, 255],
            "yellow" => [255, 255, 0, 255],
            "cyan" => [0, 255, 255, 255],
            "magenta" => [255, 0, 255, 255],
            _ => return None,
        }
    };
    Some(format!(
        "&H{:02X}{:02X}{:02X}{:02X}&",
        255 - rgba[3],
        rgba[2],
        rgba[1],
        rgba[0]
    ))
}

fn markup(text: &str, base: &Style, classes: &BTreeMap<String, Style>) -> Result<String> {
    let mut result = String::new();
    let mut stack: Vec<(String, Style)> = Vec::new();
    let mut style = base.clone();
    let mut rest = text;
    while !rest.is_empty() {
        if rest.starts_with('<')
            && let Some(end) = rest.find('>').filter(|n| *n < 1024)
        {
            let tag = &rest[1..end];
            if let Some(close) = tag.strip_prefix('/') {
                if let Some(index) = stack.iter().rposition(|(name, _)| name == close) {
                    style = stack[index].1.clone();
                    stack.truncate(index);
                    result.push_str(&style.tags());
                }
            } else {
                let name = tag.split(['.', ' ']).next().unwrap_or("");
                if matches!(name, "b" | "i" | "u" | "c" | "font") {
                    if stack.len() >= 32 {
                        bail!("subtitle markup depth exceeded");
                    }
                    stack.push((name.into(), style.clone()));
                    match name {
                        "b" => style.bold = true,
                        "i" => style.italic = true,
                        "u" => style.underline = true,
                        "font" => {
                            for (attribute, value) in super::text_formats::attributes(tag) {
                                if attribute == "color" {
                                    if let Some(color) = color(&value) {
                                        style.color = color;
                                    }
                                } else if attribute == "face" {
                                    let name = css_font(&value);
                                    if !name.is_empty() {
                                        style.font = name;
                                    }
                                }
                            }
                        }
                        "c" => {
                            for class in tag.split('.').skip(1) {
                                if let Some(class) = classes.get(class) {
                                    let bold = style.bold;
                                    let italic = style.italic;
                                    let underline = style.underline;
                                    style = class.clone();
                                    style.bold |= bold;
                                    style.italic |= italic;
                                    style.underline |= underline;
                                }
                            }
                        }
                        _ => {}
                    }
                    result.push_str(&style.tags());
                } else if name == "br" || name == "br/" {
                    result.push_str("\\N");
                }
            }
            rest = &rest[end + 1..];
            continue;
        }
        let ch = rest.chars().next().unwrap();
        rest = &rest[ch.len_utf8()..];
        match ch {
            '\n' => result.push_str("\\N"),
            '{' => result.push_str("\\{"),
            '}' => result.push_str("\\}"),
            '\\' => result.push_str("\\\\"),
            '&' => {
                if let Some(end) = rest.find(';').filter(|v| *v < 16) {
                    let name = &rest[..end];
                    let decoded = match name {
                        "amp" => Some('&'),
                        "lt" => Some('<'),
                        "gt" => Some('>'),
                        "quot" => Some('"'),
                        "apos" | "#39" => Some('\''),
                        "nbsp" => Some(' '),
                        _ => name
                            .strip_prefix("#x")
                            .and_then(|v| u32::from_str_radix(v, 16).ok())
                            .or_else(|| name.strip_prefix('#').and_then(|v| v.parse().ok()))
                            .and_then(char::from_u32),
                    };
                    if let Some(ch) =
                        decoded.filter(|c| !c.is_control() && !matches!(c, '{' | '}' | '\\'))
                    {
                        result.push(ch);
                        rest = &rest[end + 1..];
                    } else {
                        result.push('&');
                    }
                } else {
                    result.push('&');
                }
            }
            _ => result.push(ch),
        }
    }
    Ok(result)
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn preserves_native_cue_timing_styles_and_secondary_safe_ass() {
        let vtt = "WEBVTT\n\nSTYLE\n::cue { font-family: \"Authored Font\", sans-serif; color: rgba(255, 255, 255, 1); }\n::cue(.tx3g1) { font-size: 84%; color: rgba(255, 0, 0, 1); }\n\ncue\n00:00:06.170 --> 00:00:10.380 align:center line:100%,end\n<c.tx3g1><b>A &amp; B</b></c>\nNext line\n\n";
        let ass = webvtt_to_ass(vtt, 1920, 1080).unwrap();
        assert!(ass.contains("0:00:06.17,0:00:10.38"));
        assert!(ass.contains("\\fs40.32"));
        assert!(ass.contains("\\c&H000000FF&"));
        assert!(ass.contains("\\b1"));
        assert!(ass.contains("A & B"));
        assert!(ass.contains("\\NNext line"));
        assert!(ass.contains("\\an2"));
    }
    #[test]
    fn rejects_invalid_timing_and_bounds_without_interpreting_text_as_ass() {
        assert!(webvtt_to_ass("WEBVTT\n\n00:xx --> 00:04.000\ntext", 1920, 1080).is_err());
        assert!(webvtt_to_ass("WEBVTT\n\n00:01.000 --> 00:04.000\ntext", 0, 1080).is_err());
        let result = webvtt_to_ass(
            "WEBVTT\n\n00:01.000 --> 00:04.000 line:0% align:left\n{\\pos(0,0)} &lt;literal&gt;",
            1920,
            1080,
        )
        .unwrap();
        assert!(result.contains("\\{\\\\pos(0,0)\\}"));
        assert!(result.contains("\\an7"));
        assert!(result.contains("<literal>"));
        assert_eq!(timestamp_ms("00:60.000"), None);
        assert_eq!(timestamp_ms("00:01.250"), Some(1250));
    }
}
