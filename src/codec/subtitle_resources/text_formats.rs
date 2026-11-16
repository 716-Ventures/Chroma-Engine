//! Bounded normalization of external timed text. XML has no DTD/entity/network resolver.
pub(super) mod encoding;
mod scc;

use super::{SCRIPT_LIMIT, text_overlay::timestamp_ms};
use anyhow::{Context, Result, bail};
use std::collections::BTreeMap;

pub(super) struct ConvertedTrack {
    pub language: Option<String>,
    pub label: Option<String>,
    pub webvtt: String,
    pub duration_ms: u64,
}
#[derive(Clone)]
struct Cue {
    start: u64,
    end: u64,
    text: String,
}

pub(super) fn convert(
    extension: &str,
    text: &str,
    duration: Option<u64>,
    fps: Option<f64>,
) -> Result<Vec<ConvertedTrack>> {
    if text.len() > SCRIPT_LIMIT {
        bail!("text subtitle size limit exceeded");
    }
    match extension {
        "ttml" | "dfxp" | "xml" => ttml(text),
        "smi" | "sami" => sami(text, duration),
        "sub" | "txt" => microdvd(text, fps).or_else(|_| subviewer(text)),
        "sbv" => subviewer(text),
        "scc" => scc(text, duration),
        _ => Ok(Vec::new()),
    }
}
fn escaped(text: &str) -> String {
    text.replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
}
fn clock(ms: u64) -> String {
    format!(
        "{:02}:{:02}:{:02}.{:03}",
        ms / 3_600_000,
        ms / 60_000 % 60,
        ms / 1000 % 60,
        ms % 1000
    )
}
fn document(
    cues: Vec<Cue>,
    language: Option<String>,
    label: Option<String>,
) -> Result<ConvertedTrack> {
    if cues.len() > 100_000 {
        bail!("text subtitle cue count exceeded");
    }
    let mut output = String::from("WEBVTT\nX-TIMESTAMP-MAP=LOCAL:00:00:00.000,MPEGTS:0\n\n");
    let mut colors = std::collections::BTreeSet::new();
    for cue in &cues {
        for part in cue.text.split("<c.ttmlc").skip(1) {
            if let Some(color) = part.split('>').next().filter(|v| {
                (v.len() == 6 || v.len() == 8) && v.bytes().all(|b| b.is_ascii_hexdigit())
            }) {
                colors.insert(color.to_owned());
            }
        }
    }
    if colors.len() > 4096 {
        bail!("TTML color style limit exceeded");
    }
    if !colors.is_empty() {
        output.push_str("STYLE\n");
        for color in colors {
            output.push_str(&format!("::cue(.ttmlc{color}) {{ color: #{color}; }}\n"));
        }
        output.push('\n');
    }
    let mut end = 0;
    for cue in cues {
        if cue.end <= cue.start || cue.text.trim().is_empty() {
            continue;
        }
        if cue.end > 30 * 24 * 3600 * 1000 {
            bail!("subtitle timestamp exceeds limit");
        }
        end = end.max(cue.end);
        output.push_str(&format!(
            "{} --> {}\n{}\n\n",
            clock(cue.start),
            clock(cue.end),
            cue.text
        ));
        if output.len() > SCRIPT_LIMIT {
            bail!("normalized subtitle exceeds size limit");
        }
    }
    if end == 0 {
        bail!("text subtitle contains no timed cues");
    }
    Ok(ConvertedTrack {
        language,
        label,
        webvtt: output,
        duration_ms: end,
    })
}

#[derive(Clone, Copy)]
struct Timing {
    frames: f64,
    ticks: f64,
}
fn time(value: &str, timing: Timing) -> Option<u64> {
    let value = value.trim();
    if value.split(':').count() == 4 {
        let parts = value.split(':').collect::<Vec<_>>();
        let base = timestamp_ms(&format!("{}:{}:{}", parts[0], parts[1], parts[2]))?;
        let frames = parts[3].parse::<f64>().ok()?;
        if !frames.is_finite() || frames < 0.0 || frames >= timing.frames.ceil() {
            return None;
        }
        return base.checked_add((frames / timing.frames * 1000.0).round() as u64);
    }
    if value.contains(':') {
        return timestamp_ms(value);
    }
    for (suffix, scale) in [
        ("ms", 1.0),
        ("h", 3_600_000.0),
        ("m", 60_000.0),
        ("s", 1000.0),
        ("f", 1000.0 / timing.frames),
        ("t", 1000.0 / timing.ticks),
    ] {
        if let Some(number) = value
            .strip_suffix(suffix)
            .and_then(|v| v.parse::<f64>().ok())
        {
            let ms = number * scale;
            return (ms.is_finite() && (0.0..=2_592_000_000.0).contains(&ms))
                .then(|| ms.round() as u64);
        }
    }
    None
}
fn attr<'a>(node: roxmltree::Node<'a, '_>, name: &str) -> Option<&'a str> {
    node.attributes()
        .find(|a| a.name() == name)
        .map(|a| a.value())
}

// Cap retained expansion separately from temporary copies and tree visits. Timed
// spans may render the same paragraph many times, even for a small source file.
struct TtmlContext<'a> {
    timing: Timing,
    styles: BTreeMap<&'a str, roxmltree::Node<'a, 'a>>,
    remaining: usize,
    copies: usize,
    visits: usize,
}
impl TtmlContext<'_> {
    fn visit(&mut self) -> Result<()> {
        self.visits = self
            .visits
            .checked_sub(1)
            .context("TTML work limit exceeded")?;
        Ok(())
    }
    fn allocation(&mut self, size: usize) -> Result<()> {
        if size > self.remaining {
            bail!("TTML expanded text exceeds limit");
        }
        self.copies = self
            .copies
            .checked_sub(size)
            .context("TTML copy work limit exceeded")?;
        Ok(())
    }
    fn wrap(&mut self, text: String, prefix: &str, suffix: &str) -> Result<String> {
        let size = text
            .len()
            .checked_add(prefix.len())
            .and_then(|n| n.checked_add(suffix.len()))
            .context("TTML text size overflow")?;
        self.allocation(size)?;
        Ok(format!("{prefix}{text}{suffix}"))
    }
}
fn ttml(text: &str) -> Result<Vec<ConvertedTrack>> {
    ttml_with_limits(text, SCRIPT_LIMIT, 512 * 1024 * 1024, 1_000_000)
}
fn ttml_with_limits(
    text: &str,
    retained: usize,
    copies: usize,
    visits: usize,
) -> Result<Vec<ConvertedTrack>> {
    let options = roxmltree::ParsingOptions {
        allow_dtd: false,
        nodes_limit: 100_000,
        ..Default::default()
    };
    let doc =
        roxmltree::Document::parse_with_options(text.trim_start_matches('\u{feff}'), options)?;
    let root = doc.root_element();
    if root.tag_name().name() != "tt" {
        bail!("not a TTML document");
    }
    if doc
        .descendants()
        .any(|n| n.ancestors().take(66).count() > 64)
    {
        bail!("TTML depth limit exceeded");
    }
    let mut frames = attr(root, "frameRate").unwrap_or("30").parse::<f64>()?;
    if let Some(multiplier) = attr(root, "frameRateMultiplier") {
        let numbers = multiplier
            .split_whitespace()
            .map(str::parse::<f64>)
            .collect::<std::result::Result<Vec<_>, _>>()?;
        if numbers.len() != 2 || numbers[0] <= 0.0 || numbers[1] <= 0.0 {
            bail!("invalid TTML frame-rate multiplier");
        }
        frames *= numbers[0] / numbers[1];
    }
    let ticks = attr(root, "tickRate")
        .map(str::parse::<f64>)
        .transpose()?
        .unwrap_or(frames);
    if !frames.is_finite()
        || !(1.0..=1000.0).contains(&frames)
        || !ticks.is_finite()
        || !(0.001..=1e9).contains(&ticks)
    {
        bail!("invalid TTML timebase");
    }
    let timing = Timing { frames, ticks };
    let styles = doc
        .descendants()
        .filter(|n| n.has_tag_name("style"))
        .filter_map(|n| attr(n, "id").map(|id| (id, n)))
        .collect::<BTreeMap<_, _>>();
    let mut cues = Vec::new();
    let mut context = TtmlContext {
        timing,
        styles,
        remaining: retained,
        copies,
        visits,
    };
    walk_ttml(root, 0, 2_592_000_000, &mut context, &mut cues, 0)?;
    Ok(vec![document(
        cues,
        attr(root, "lang").map(str::to_string),
        None,
    )?])
}
fn interval(
    node: roxmltree::Node<'_, '_>,
    base: u64,
    limit: u64,
    timing: Timing,
) -> Result<(u64, u64)> {
    let start = base
        .checked_add(
            attr(node, "begin")
                .map(|v| time(v, timing).context("invalid TTML begin"))
                .transpose()?
                .unwrap_or(0),
        )
        .context("TTML begin overflow")?;
    let explicit_end = attr(node, "end")
        .map(|v| {
            time(v, timing)
                .and_then(|v| base.checked_add(v))
                .context("invalid TTML end")
        })
        .transpose()?;
    let duration = attr(node, "dur")
        .map(|v| {
            time(v, timing)
                .and_then(|v| start.checked_add(v))
                .context("invalid TTML duration")
        })
        .transpose()?;
    Ok((
        start,
        explicit_end
            .unwrap_or(limit)
            .min(duration.unwrap_or(limit))
            .min(limit),
    ))
}
fn walk_ttml<'a>(
    node: roxmltree::Node<'a, 'a>,
    base: u64,
    limit: u64,
    context: &mut TtmlContext<'a>,
    cues: &mut Vec<Cue>,
    depth: usize,
) -> Result<()> {
    context.visit()?;
    if depth > 64 || cues.len() > 100_000 {
        bail!("TTML work limit exceeded");
    }
    if !node.is_element() {
        return Ok(());
    }
    let (start, end) = interval(node, base, limit, context.timing)?;
    if end <= start {
        return Ok(());
    }
    if node.has_tag_name("p") {
        let mut boundaries = vec![start, end];
        for child in node.descendants().filter(|n| n.is_element() && n != &node) {
            if attr(child, "begin").is_some()
                || attr(child, "end").is_some()
                || attr(child, "dur").is_some()
            {
                let (s, e) = interval(child, start, end, context.timing)?;
                boundaries.extend([s.max(start).min(end), e.max(start).min(end)]);
            }
            if boundaries.len() > 4096 {
                bail!("TTML timed-span work limit exceeded");
            }
        }
        boundaries.sort_unstable();
        boundaries.dedup();
        for pair in boundaries.windows(2) {
            let body = ttml_text(node, start, end, pair[0], context, 0)?;
            if !body.trim().is_empty() {
                if cues.len() >= 100_000 {
                    bail!("TTML cue count exceeded");
                }
                context.remaining -= body.len();
                cues.push(Cue {
                    start: pair[0],
                    end: pair[1],
                    text: body,
                });
            }
        }
        return Ok(());
    }
    if matches!(
        node.tag_name().name(),
        "head" | "metadata" | "styling" | "layout"
    ) {
        return Ok(());
    }
    let sequential = attr(node, "timeContainer") == Some("seq");
    let mut cursor = start;
    for child in node.children().filter(|n| n.is_element()) {
        let child_base = if sequential { cursor } else { start };
        walk_ttml(child, child_base, end, context, cues, depth + 1)?;
        if sequential {
            cursor = interval(child, child_base, end, context.timing)?.1;
        }
    }
    Ok(())
}
fn ttml_text<'a>(
    node: roxmltree::Node<'a, 'a>,
    base: u64,
    limit: u64,
    at: u64,
    context: &mut TtmlContext<'a>,
    depth: usize,
) -> Result<String> {
    context.visit()?;
    if depth > 64 {
        bail!("TTML text depth limit exceeded");
    }
    if node.is_text() {
        let text = node.text().unwrap_or("");
        let mut size = text.len();
        for byte in text.bytes() {
            size = size
                .checked_add(match byte {
                    b'&' => 4,
                    b'<' | b'>' => 3,
                    _ => 0,
                })
                .context("TTML escaped text size overflow")?;
        }
        context.allocation(size)?;
        if size == text.len() {
            return Ok(text.to_owned());
        }
        let mut output = String::new();
        output
            .try_reserve_exact(size)
            .context("TTML text allocation failed")?;
        for ch in text.chars() {
            match ch {
                '&' => output.push_str("&amp;"),
                '<' => output.push_str("&lt;"),
                '>' => output.push_str("&gt;"),
                _ => output.push(ch),
            }
        }
        return Ok(output);
    }
    if node.has_tag_name("br") {
        context.allocation(1)?;
        return Ok("\n".into());
    }
    let (start, end) = if node.has_tag_name("p") {
        (base, limit)
    } else {
        interval(node, base, limit, context.timing)?
    };
    if at < start || at >= end {
        return Ok(String::new());
    }
    let mut result = String::new();
    for child in node.children() {
        let child = ttml_text(child, start, end, at, context, depth + 1)?;
        let size = result
            .len()
            .checked_add(child.len())
            .context("TTML text size overflow")?;
        if size > context.remaining {
            bail!("TTML expanded text exceeds limit");
        }
        context.copies = context
            .copies
            .checked_sub(child.len())
            .context("TTML copy work limit exceeded")?;
        result.push_str(&child);
    }
    let mut references = Vec::new();
    resolve_styles(
        node,
        &context.styles,
        &mut references,
        0,
        &mut context.copies,
    )?;
    context.visits = context
        .visits
        .checked_sub(references.len())
        .context("TTML work limit exceeded")?;
    references.push(node);
    for style in references {
        if attr(style, "fontWeight") == Some("bold") {
            result = context.wrap(result, "<b>", "</b>")?;
        }
        if attr(style, "fontStyle") == Some("italic") {
            result = context.wrap(result, "<i>", "</i>")?;
        }
        if attr(style, "textDecoration")
            .is_some_and(|s| s.split_whitespace().any(|s| s == "underline"))
        {
            result = context.wrap(result, "<u>", "</u>")?;
        }
        if let Some(color) = attr(style, "color").and_then(ttml_color) {
            result = context.wrap(result, &format!("<c.ttmlc{color}>"), "</c>")?;
        }
    }
    Ok(result)
}
fn ttml_color(value: &str) -> Option<String> {
    // Supported names and #RRGGBB[AA] never exceed nine bytes.
    if value.len() > 9 {
        return None;
    }
    if let Some(hex) = value
        .strip_prefix('#')
        .filter(|v| (v.len() == 6 || v.len() == 8) && v.bytes().all(|b| b.is_ascii_hexdigit()))
    {
        return Some(hex.to_ascii_lowercase());
    }
    Some(
        match value.to_ascii_lowercase().as_str() {
            "white" => "ffffff",
            "black" => "000000",
            "red" => "ff0000",
            "green" => "008000",
            "lime" => "00ff00",
            "blue" => "0000ff",
            "yellow" => "ffff00",
            "cyan" => "00ffff",
            "magenta" => "ff00ff",
            "transparent" => "00000000",
            _ => return None,
        }
        .into(),
    )
}
fn resolve_styles<'a>(
    node: roxmltree::Node<'a, 'a>,
    styles: &BTreeMap<&str, roxmltree::Node<'a, 'a>>,
    out: &mut Vec<roxmltree::Node<'a, 'a>>,
    depth: usize,
    work: &mut usize,
) -> Result<()> {
    if depth > 16 || out.len() > 256 {
        bail!("cyclic or excessive TTML style references");
    }
    let references = attr(node, "style").unwrap_or("");
    *work = work
        .checked_sub(references.len())
        .context("TTML style work limit exceeded")?;
    for id in references.split_whitespace() {
        if let Some(style) = styles.get(id) {
            resolve_styles(*style, styles, out, depth + 1, work)?;
            out.push(*style);
        }
    }
    Ok(())
}

fn microdvd(text: &str, fps: Option<f64>) -> Result<Vec<ConvertedTrack>> {
    let mut rate = fps;
    let mut cues = Vec::new();
    for line in text.lines().filter(|s| !s.trim().is_empty()) {
        let line = line.trim();
        let rest = line.strip_prefix('{').context("not MicroDVD")?;
        let (start, rest) = rest.split_once('}').context("invalid MicroDVD start")?;
        let rest = rest.strip_prefix('{').context("invalid MicroDVD end")?;
        let (end, body) = rest.split_once('}').context("invalid MicroDVD end")?;
        let start = start.parse::<u64>()?;
        let end = end.parse::<u64>()?;
        if start == end && matches!(start, 0 | 1) && cues.is_empty() {
            rate = Some(body.trim().parse()?);
            continue;
        }
        let rate = rate
            .filter(|r| r.is_finite() && (1.0..=1000.0).contains(r))
            .context("MicroDVD requires a declared or source frame rate")?;
        if end <= start {
            continue;
        }
        let mut body = body.to_string();
        let mut italic = false;
        let mut bold = false;
        let mut underline = false;
        while body.starts_with('{') {
            let Some(index) = body.find('}') else { break };
            let tag = &body[1..index];
            if let Some(styles) = tag.strip_prefix("y:").or_else(|| tag.strip_prefix("Y:")) {
                italic |= styles.contains('i');
                bold |= styles.contains('b');
                underline |= styles.contains('u');
            }
            body = body[index + 1..].into();
        }
        let mut body = escaped(&body.replace('|', "\n"));
        if italic {
            body = format!("<i>{body}</i>");
        }
        if bold {
            body = format!("<b>{body}</b>");
        }
        if underline {
            body = format!("<u>{body}</u>");
        }
        cues.push(Cue {
            start: (start as f64 * 1000.0 / rate).round() as u64,
            end: (end as f64 * 1000.0 / rate).round() as u64,
            text: body,
        });
        if cues.len() > 100_000 {
            bail!("MicroDVD cue limit exceeded");
        }
    }
    Ok(vec![document(cues, None, None)?])
}
fn subviewer(text: &str) -> Result<Vec<ConvertedTrack>> {
    let normalized = text.replace("\r\n", "\n");
    let mut cues = Vec::new();
    let mut lines = normalized.lines();
    while let Some(line) = lines.next() {
        let Some((left, right)) = line.trim().split_once(',') else {
            continue;
        };
        let (Some(start), Some(end)) = (timestamp_ms(left), timestamp_ms(right)) else {
            continue;
        };
        let mut body = Vec::new();
        for line in lines.by_ref() {
            if line.trim().is_empty() {
                break;
            }
            body.push(line.replace("[br]", "\n"));
        }
        cues.push(Cue {
            start,
            end,
            text: escaped(&body.join("\n")),
        });
        if cues.len() > 100_000 {
            bail!("SubViewer cue limit exceeded");
        }
    }
    Ok(vec![document(cues, None, None)?])
}

pub(super) fn attributes(tag: &str) -> BTreeMap<String, String> {
    let mut out = BTreeMap::new();
    let mut rest = tag
        .split_once(char::is_whitespace)
        .map(|v| v.1)
        .unwrap_or("")
        .trim();
    while !rest.is_empty() {
        let n = rest
            .find(|c: char| c == '=' || c.is_whitespace())
            .unwrap_or(rest.len());
        let key = rest[..n].to_ascii_lowercase();
        rest = rest[n..].trim_start();
        if !rest.starts_with('=') {
            break;
        }
        rest = rest[1..].trim_start();
        let (value, left) = if let Some(q) = rest.chars().next().filter(|c| matches!(c, '\'' | '"'))
        {
            let end = rest[1..].find(q).map(|i| i + 1).unwrap_or(rest.len());
            (&rest[1..end], rest.get(end + 1..).unwrap_or(""))
        } else {
            let end = rest.find(char::is_whitespace).unwrap_or(rest.len());
            (&rest[..end], &rest[end..])
        };
        out.insert(key, value.into());
        rest = left.trim_start();
        if out.len() > 32 {
            break;
        }
    }
    out
}
fn sami(text: &str, duration: Option<u64>) -> Result<Vec<ConvertedTrack>> {
    let lower = text.to_ascii_lowercase();
    if !lower.contains("<sami") {
        bail!("not a SAMI document");
    }
    let start = lower
        .find("<body")
        .and_then(|i| lower[i..].find('>').map(|n| i + n + 1))
        .context("SAMI body missing")?;
    let mut rest = &text[start..];
    let mut at = None;
    let mut class = String::from("default");
    let mut frame = BTreeMap::<String, String>::new();
    let mut tracks = BTreeMap::<String, Vec<Cue>>::new();
    let mut tags = 0;
    while !rest.is_empty() {
        if rest.starts_with('<') {
            tags += 1;
            if tags > 200_000 {
                bail!("SAMI work limit exceeded");
            }
            let end = rest
                .find('>')
                .filter(|n| *n <= 4096)
                .context("malformed SAMI tag")?;
            let tag = &rest[1..end];
            let name = tag
                .split_whitespace()
                .next()
                .unwrap_or("")
                .to_ascii_lowercase();
            if name == "sync" {
                let new = attributes(tag)
                    .get("start")
                    .context("SAMI sync start missing")?
                    .parse::<u64>()?;
                if let Some(old) = at {
                    if new < old {
                        bail!("nonmonotonic SAMI time");
                    }
                    flush_sami(&mut tracks, &mut frame, old, new)?;
                }
                at = Some(new);
                class = "default".into();
            } else if name == "p" {
                class = attributes(tag)
                    .get("class")
                    .cloned()
                    .unwrap_or_else(|| "default".into());
                if class.len() > 128 {
                    bail!("SAMI class exceeds limit");
                }
            } else if matches!(name.as_str(), "br" | "br/") {
                frame.entry(class.clone()).or_default().push('\n');
            } else if matches!(name.as_str(), "b" | "/b" | "i" | "/i" | "u" | "/u") {
                frame
                    .entry(class.clone())
                    .or_default()
                    .push_str(&format!("<{name}>"));
            } else if matches!(name.as_str(), "script" | "style") {
                let close = format!("</{name}>");
                let tail = &rest[end + 1..];
                let offset = text.len() - tail.len();
                let n = lower[offset..]
                    .find(&close)
                    .context("unclosed SAMI metadata block")?;
                rest = &tail[n + close.len()..];
                continue;
            }
            rest = &rest[end + 1..];
        } else {
            let end = rest.find('<').unwrap_or(rest.len());
            if at.is_some() {
                let value = rest[..end].trim();
                if !value.is_empty() {
                    let body = frame.entry(class.clone()).or_default();
                    if !body.is_empty() && !body.ends_with(['\n', '>', ' ']) {
                        body.push(' ');
                    }
                    body.push_str(value);
                }
            }
            rest = &rest[end..];
        }
    }
    if let (Some(at), Some(end)) = (at, duration)
        && end > at
    {
        flush_sami(&mut tracks, &mut frame, at, end)?;
    }
    if tracks.len() > 128 {
        bail!("SAMI track limit exceeded");
    }
    tracks
        .into_iter()
        .map(|(class, cues)| {
            let mut language =
                if class.eq_ignore_ascii_case("encc") || class.eq_ignore_ascii_case("english") {
                    Some("eng".into())
                } else if class.eq_ignore_ascii_case("jpcc") {
                    Some("jpn".into())
                } else {
                    None
                };
            let mut label = class.clone();
            let selector = format!(".{}", class.to_ascii_lowercase());
            if let Some(start) = lower
                .find(&selector)
                .and_then(|i| text[i..].find('{').map(|n| i + n + 1))
                && let Some(end) = text[start..].find('}').filter(|n| *n < 4096)
            {
                for declaration in text[start..start + end].split(';') {
                    if let Some((key, value)) = declaration.split_once(':') {
                        let value = value.trim().trim_matches(['\'', '"']);
                        match key.trim().to_ascii_lowercase().as_str() {
                            "lang"
                                if value.len() <= 35
                                    && value
                                        .bytes()
                                        .all(|b| b.is_ascii_alphanumeric() || b == b'-') =>
                            {
                                language = Some(value.into())
                            }
                            "name" => {
                                label = value
                                    .chars()
                                    .filter(|c| !c.is_control())
                                    .take(100)
                                    .collect()
                            }
                            _ => {}
                        }
                    }
                }
            }
            document(cues, language, Some(label))
        })
        .collect()
}
fn flush_sami(
    tracks: &mut BTreeMap<String, Vec<Cue>>,
    frame: &mut BTreeMap<String, String>,
    start: u64,
    end: u64,
) -> Result<()> {
    for (class, text) in std::mem::take(frame) {
        let visible = text.replace("&nbsp;", "").replace("&#160;", "");
        if visible.trim().is_empty() {
            continue;
        }
        if tracks.len() >= 128 && !tracks.contains_key(&class) {
            bail!("SAMI track limit exceeded");
        }
        let cues = tracks.entry(class).or_default();
        if cues.len() >= 100_000 {
            bail!("SAMI resource limit exceeded");
        }
        cues.push(Cue { start, end, text });
    }
    Ok(())
}

fn scc(text: &str, duration: Option<u64>) -> Result<Vec<ConvertedTrack>> {
    scc::parse(text, duration)?
        .into_iter()
        .map(|(channel, cues)| document(cues, None, Some(channel)))
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn ttml_bounds_retained_timed_span_expansion_before_collecting_cues() {
        let text =
            "<tt><body><p dur='4s'>abcdefgh<span begin='1s' dur='1s'>x</span></p></body></tt>";
        let error = ttml_with_limits(text, 24, 4096, 1000).err().unwrap();
        assert!(error.to_string().contains("expanded text"));
        let tracks = ttml_with_limits(text, 25, 4096, 1000).unwrap();
        assert!(
            tracks[0]
                .webvtt
                .contains("00:00:01.000 --> 00:00:02.000\nabcdefghx")
        );
    }
    #[test]
    fn ttml_bounds_escaping_styling_and_repeated_tree_work() {
        let escaped = "<tt><body><p dur='1s'>日本&amp;&lt;&gt;</p></body></tt>";
        assert!(ttml_with_limits(escaped, 18, 4096, 1000).is_err());
        assert!(
            ttml_with_limits(escaped, 19, 4096, 1000).unwrap()[0]
                .webvtt
                .contains("日本&amp;&lt;&gt;")
        );
        let styled = "<tt><body><p dur='1s' fontWeight='bold'>abc</p></body></tt>";
        assert!(ttml_with_limits(styled, 9, 4096, 1000).is_err());
        assert!(ttml_with_limits(styled, 10, 4096, 1000).is_ok());
        assert!(
            ttml_with_limits(styled, 100, 5, 1000)
                .err()
                .unwrap()
                .to_string()
                .contains("copy work")
        );
        assert!(
            ttml_with_limits(styled, 100, 4096, 2)
                .err()
                .unwrap()
                .to_string()
                .contains("work limit")
        );
    }
    #[test]
    fn ttml_bounds_unknown_style_reference_work_too() {
        let text =
            "<tt><body><p dur='1s' style='unknown unknown unknown unknown'>abc</p></body></tt>";
        assert!(
            ttml_with_limits(text, 100, 30, 1000)
                .err()
                .unwrap()
                .to_string()
                .contains("style work")
        );
        assert!(ttml_with_limits(text, 100, 1000, 1000).is_ok());
        assert!(ttml_color(&"red".repeat(1000)).is_none());
    }
    #[test]
    fn sami_skips_repeated_metadata_without_copying_the_remaining_document() {
        let mut text = String::from("<SAMI><BODY>");
        text.push_str(&"<ScRiPt>日本語</sCrIpT><STYLE></style>".repeat(10_000));
        text.push_str("<SYNC Start=0><P Class=ENCC>Dialogue<SYNC Start=1000></BODY></SAMI>");
        let tracks = sami(&text, Some(2000)).unwrap();
        assert!(
            tracks[0]
                .webvtt
                .contains("00:00:00.000 --> 00:00:01.000\nDialogue")
        );
        let text = format!("<SAMI><BODY>{}", "<script></script>".repeat(200_001));
        assert!(
            sami(&text, None)
                .err()
                .unwrap()
                .to_string()
                .contains("work limit")
        );
    }
    #[test]
    fn ttml_parent_timing_frames_ticks_and_styles_are_source_timed() {
        let text = r#"<tt xmlns="http://www.w3.org/ns/ttml" xmlns:ttp="http://www.w3.org/ns/ttml#parameter" xmlns:tts="http://www.w3.org/ns/ttml#styling" xml:lang="en" ttp:frameRate="25" ttp:tickRate="100"><head><styling><style xml:id="bold" tts:fontWeight="bold"/></styling></head><body begin="1s"><div><p begin="25f" dur="100t" style="bold">A &amp; B<br/>line</p></div></body></tt>"#;
        let tracks = ttml(text).unwrap();
        assert_eq!(tracks[0].language.as_deref(), Some("en"));
        assert!(tracks[0].webvtt.contains("00:00:02.000 --> 00:00:03.000"));
        assert!(tracks[0].webvtt.contains("<b>A &amp; B\nline</b>"));
    }
    #[test]
    fn ttml_rejects_entities_cycles_and_supports_timed_spans() {
        assert!(ttml("<!DOCTYPE tt [<!ENTITY x SYSTEM 'file:///etc/passwd'>]><tt><body><p begin='0s' dur='1s'>&x;</p></body></tt>").is_err());
        assert!(ttml("<tt><head><style xml:id='a' style='a'/></head><body><p begin='0s' dur='1s' style='a'>text</p></body></tt>").is_err());
        let result=ttml("<tt><body><p begin='1s' end='4s'>one<span begin='1s' dur='1s'>two</span></p></body></tt>").unwrap();
        assert!(
            result[0]
                .webvtt
                .contains("00:00:02.000 --> 00:00:03.000\nonetwo")
        );
    }
    #[test]
    fn microdvd_uses_source_or_authored_frame_rate_and_line_styles() {
        let result = microdvd("{1}{1}25\n{25}{75}{y:i}A|B", None).unwrap();
        assert!(
            result[0]
                .webvtt
                .contains("00:00:01.000 --> 00:00:03.000\n<i>A\nB</i>")
        );
        assert!(microdvd("{25}{75}Text", None).is_err());
        assert!(microdvd("{25}{75}Text", Some(25.0)).is_ok());
    }
    #[test]
    fn sami_keeps_language_tracks_and_clear_sync_boundaries() {
        let result=sami("<SAMI><BODY><SYNC Start=1000><P Class=ENCC>One<br>two<P Class=JPCC>日本語<SYNC Start=3000><P Class=ENCC>&nbsp;<P Class=JPCC>&nbsp;</BODY></SAMI>",Some(5000)).unwrap();
        assert_eq!(result.len(), 2);
        assert_eq!(result[0].language.as_deref(), Some("eng"));
        assert!(
            result[0]
                .webvtt
                .contains("00:00:01.000 --> 00:00:03.000\nOne\ntwo")
        );
        assert!(result[1].webvtt.contains("日本語"));
    }
    #[test]
    fn subviewer_and_sbv_normalize_intervals_and_scc_pop_on_clears() {
        let result = subviewer("00:00:01.000,00:00:02.500\nA[br]B\n\n").unwrap();
        assert!(result[0].webvtt.contains("A\nB"));
        let cc = scc(
            "Scenarist_SCC V1.0\n\n00:00:01:00\t9420 94ae c849 942f\n00:00:03:00\t942c\n",
            Some(5000),
        )
        .unwrap();
        assert!(cc[0].webvtt.contains("HI"));
        assert!(cc[0].duration_ms >= 3000);
    }
}
