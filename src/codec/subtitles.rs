#[derive(Debug, Clone, PartialEq, Eq)]
/// A normalized text subtitle cue using millisecond timing.
pub struct TextSubtitleCue {
    /// Inclusive cue start timestamp in milliseconds.
    pub start_ms: u64,
    /// Exclusive cue end timestamp in milliseconds.
    pub end_ms: u64,
    /// Cue text with original line breaks preserved.
    pub text: String,
}

/// Parses SubRip text into normalized subtitle cues.
pub fn parse_subrip(input: &str) -> Vec<TextSubtitleCue> {
    let normalized = input.replace("\r\n", "\n").replace('\r', "\n");
    normalized
        .split("\n\n")
        .filter_map(parse_subrip_block)
        .collect()
}

/// Renders normalized subtitle cues as a complete WebVTT document.
pub fn render_webvtt(cues: &[TextSubtitleCue]) -> String {
    let mut out = String::from("WEBVTT\n\n");
    for cue in cues {
        out.push_str(&format!(
            "{} --> {}\n{}\n\n",
            format_timestamp(cue.start_ms),
            format_timestamp(cue.end_ms),
            cue.text.trim()
        ));
    }
    out
}

/// Splits cues into fixed-duration WebVTT segment documents.
pub fn segment_webvtt(
    cues: &[TextSubtitleCue],
    segment_ms: u64,
    uri_prefix: &str,
) -> Vec<WebVttSegment> {
    if segment_ms == 0 {
        return Vec::new();
    }

    let max_end = cues.iter().map(|cue| cue.end_ms).max().unwrap_or(0);
    if max_end == 0 {
        return Vec::new();
    }

    let segment_count = max_end.div_ceil(segment_ms);
    (0..segment_count)
        .map(|idx| {
            let start = idx * segment_ms;
            let end = start + segment_ms;
            let segment_cues = cues
                .iter()
                .filter(|cue| cue.start_ms < end && cue.end_ms > start)
                .cloned()
                .collect::<Vec<_>>();
            WebVttSegment {
                index: idx as u32,
                start_ms: start,
                duration_ms: segment_ms.min(max_end.saturating_sub(start)),
                uri: format!("{uri_prefix}/seg-{idx:05}.vtt"),
                body: render_webvtt(&segment_cues),
            }
        })
        .collect()
}

#[derive(Debug, Clone, PartialEq, Eq)]
/// A generated WebVTT media segment.
pub struct WebVttSegment {
    /// Zero-based segment index.
    pub index: u32,
    /// Segment start timestamp in milliseconds.
    pub start_ms: u64,
    /// Segment duration in milliseconds.
    pub duration_ms: u64,
    /// Relative URI for the segment body.
    pub uri: String,
    /// Complete WebVTT body for this segment.
    pub body: String,
}

fn parse_subrip_block(block: &str) -> Option<TextSubtitleCue> {
    let mut lines = block
        .lines()
        .map(str::trim_end)
        .filter(|line| !line.is_empty());
    let first = lines.next()?;
    let timing = if first.contains("-->") {
        first
    } else {
        lines.next()?
    };
    let (start, end) = parse_timing_line(timing)?;
    let text = lines.collect::<Vec<_>>().join("\n");
    if text.trim().is_empty() || end <= start {
        return None;
    }
    Some(TextSubtitleCue {
        start_ms: start,
        end_ms: end,
        text,
    })
}

fn parse_timing_line(line: &str) -> Option<(u64, u64)> {
    let (start, rest) = line.split_once("-->")?;
    let end = rest.split_whitespace().next()?;
    Some((
        parse_subrip_timestamp(start.trim())?,
        parse_subrip_timestamp(end.trim())?,
    ))
}

fn parse_subrip_timestamp(raw: &str) -> Option<u64> {
    let normalized = raw.replace(',', ".");
    let mut parts = normalized.split(':');
    let hours = parts.next()?.parse::<u64>().ok()?;
    let minutes = parts.next()?.parse::<u64>().ok()?;
    let seconds_ms = parts.next()?;
    if parts.next().is_some() {
        return None;
    }
    let (seconds, millis) = seconds_ms.split_once('.')?;
    let seconds = seconds.parse::<u64>().ok()?;
    let millis = parse_millis(millis)?;
    Some((((hours * 60 + minutes) * 60) + seconds) * 1000 + millis)
}

fn parse_millis(raw: &str) -> Option<u64> {
    let digits = raw.chars().take(3).collect::<String>();
    if digits.is_empty() || !digits.chars().all(|c| c.is_ascii_digit()) {
        return None;
    }
    let padded = format!("{digits:0<3}");
    padded.parse::<u64>().ok()
}

fn format_timestamp(ms: u64) -> String {
    let total_seconds = ms / 1000;
    let millis = ms % 1000;
    let seconds = total_seconds % 60;
    let total_minutes = total_seconds / 60;
    let minutes = total_minutes % 60;
    let hours = total_minutes / 60;
    format!("{hours:02}:{minutes:02}:{seconds:02}.{millis:03}")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_subrip() {
        let cues = parse_subrip(
            "1\r\n00:00:01,500 --> 00:00:03,000\r\nHello\r\nworld\r\n\r\n2\n00:00:04,000 --> 00:00:05,250\nAgain\n",
        );
        assert_eq!(cues.len(), 2);
        assert_eq!(cues[0].start_ms, 1500);
        assert_eq!(cues[0].end_ms, 3000);
        assert_eq!(cues[0].text, "Hello\nworld");
    }

    #[test]
    fn renders_webvtt() {
        let out = render_webvtt(&[TextSubtitleCue {
            start_ms: 1500,
            end_ms: 3000,
            text: "Hello".to_string(),
        }]);
        assert!(out.starts_with("WEBVTT"));
        assert!(out.contains("00:00:01.500 --> 00:00:03.000"));
    }

    #[test]
    fn segments_webvtt() {
        let cues = vec![
            TextSubtitleCue {
                start_ms: 1000,
                end_ms: 3000,
                text: "A".to_string(),
            },
            TextSubtitleCue {
                start_ms: 6500,
                end_ms: 7000,
                text: "B".to_string(),
            },
        ];
        let segments = segment_webvtt(&cues, 6000, "s0");
        assert_eq!(segments.len(), 2);
        assert_eq!(segments[0].uri, "s0/seg-00000.vtt");
        assert!(segments[0].body.contains("A"));
        assert!(segments[1].body.contains("B"));
    }
}
