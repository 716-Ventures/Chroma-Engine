use std::{fs::create_dir_all, path::Path};

use crate::{output::publish_bytes, packet::ChunkSample};

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

#[derive(Debug, Clone, PartialEq, Eq)]
/// One MP4 timed-text (`mov_text`) sample derived from a normalized cue.
pub struct MovTextSample {
    /// Inclusive cue start timestamp in milliseconds.
    pub start_ms: u64,
    /// Exclusive cue end timestamp in milliseconds.
    pub end_ms: u64,
    /// MP4 timed-text sample payload: 16-bit text length followed by UTF-8 text bytes.
    pub payload: Vec<u8>,
}

/// Parses SubRip text into normalized subtitle cues.
pub fn parse_subrip(input: &str) -> Vec<TextSubtitleCue> {
    let normalized = input.replace("\r\n", "\n").replace('\r', "\n");
    normalized
        .split("\n\n")
        .filter_map(parse_subrip_block)
        .collect()
}

/// Converts normalized text cues into MP4 timed-text (`mov_text`) samples.
pub fn cues_to_mov_text_samples(cues: &[TextSubtitleCue]) -> Vec<MovTextSample> {
    cues.iter()
        .filter_map(|cue| {
            encode_mov_text_sample(&cue.text).map(|payload| MovTextSample {
                start_ms: cue.start_ms,
                end_ms: cue.end_ms,
                payload,
            })
        })
        .collect()
}

/// Encodes one plain-text subtitle body as an MP4 timed-text sample.
pub fn encode_mov_text_sample(text: &str) -> Option<Vec<u8>> {
    let sanitized = sanitize_subtitle_text(text);
    if sanitized.is_empty() || sanitized.len() > usize::from(u16::MAX) {
        return None;
    }
    let mut out = Vec::with_capacity(2 + sanitized.len());
    out.extend_from_slice(&(sanitized.len() as u16).to_be_bytes());
    out.extend_from_slice(sanitized.as_bytes());
    Some(out)
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
                uri: segment_uri(uri_prefix, idx),
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

#[derive(Debug, Clone, PartialEq, Eq)]
/// Input cues for one selected text subtitle track.
pub struct WebVttSidecarInput {
    /// Stable subtitle track identifier.
    pub track_id: String,
    /// Subtitle language when available.
    pub language: Option<String>,
    /// Human-readable subtitle track name when available.
    pub name: Option<String>,
    /// Normalized cues for this text subtitle track.
    pub cues: Vec<TextSubtitleCue>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
/// WebVTT sidecar output for all selected text subtitle tracks.
pub struct WebVttSidecarSet {
    /// One WebVTT rendition per selected text subtitle track.
    pub tracks: Vec<WebVttSidecarTrack>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
/// WebVTT sidecar output for one text subtitle track.
pub struct WebVttSidecarTrack {
    /// Stable subtitle track identifier.
    pub track_id: String,
    /// Subtitle language when available.
    pub language: Option<String>,
    /// Human-readable subtitle track name when available.
    pub name: Option<String>,
    /// Relative URI for this track's WebVTT media playlist.
    pub playlist_uri: String,
    /// Generated WebVTT media segments.
    pub segments: Vec<WebVttSegment>,
    /// Complete WebVTT media playlist body.
    pub playlist_body: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
/// Borrowed native text subtitle packet data for one selected subtitle track.
pub struct NativeTextSubtitleTrack<'a> {
    /// Stable subtitle track identifier.
    pub track_id: String,
    /// Normalized subtitle codec identifier.
    pub codec: String,
    /// Subtitle language when available.
    pub language: Option<String>,
    /// Human-readable subtitle track name when available.
    pub name: Option<String>,
    /// Sample metadata for the subtitle packet payload.
    pub samples: &'a [ChunkSample],
    /// Contiguous subtitle packet payload bytes.
    pub payload: &'a [u8],
}

/// Builds WebVTT sidecar playlists and segments for all selected text subtitle tracks.
pub fn build_webvtt_sidecars(tracks: &[WebVttSidecarInput], segment_ms: u64) -> WebVttSidecarSet {
    let tracks = tracks
        .iter()
        .map(|track| {
            let safe_id = safe_path_component(&track.track_id);
            let segments = segment_webvtt(&track.cues, segment_ms, "");
            let playlist_body = render_webvtt_media_playlist(&segments);
            WebVttSidecarTrack {
                track_id: track.track_id.clone(),
                language: track.language.clone(),
                name: track.name.clone(),
                playlist_uri: format!("subs/{safe_id}/index.m3u8"),
                segments,
                playlist_body,
            }
        })
        .collect();

    WebVttSidecarSet { tracks }
}

/// Parses native text subtitle packets and builds WebVTT sidecars for every selected track.
pub fn build_webvtt_sidecars_from_native_text_tracks(
    tracks: &[NativeTextSubtitleTrack<'_>],
    segment_ms: u64,
) -> WebVttSidecarSet {
    let inputs = tracks
        .iter()
        .map(|track| WebVttSidecarInput {
            track_id: track.track_id.clone(),
            language: track.language.clone(),
            name: track.name.clone(),
            cues: parse_native_text_subtitle_cues(&track.codec, track.samples, track.payload),
        })
        .collect::<Vec<_>>();
    build_webvtt_sidecars(&inputs, segment_ms)
}

/// Writes WebVTT sidecar playlists and segment files for all selected text subtitle tracks.
pub fn write_webvtt_sidecars(
    output_dir: &Path,
    tracks: &[WebVttSidecarInput],
    segment_ms: u64,
) -> std::io::Result<WebVttSidecarSet> {
    let sidecars = build_webvtt_sidecars(tracks, segment_ms);
    for track in &sidecars.tracks {
        let safe_id = safe_path_component(&track.track_id);
        let track_dir = output_dir.join("subs").join(safe_id);
        create_dir_all(&track_dir)?;
        publish_bytes(
            &track_dir.join("index.m3u8"),
            track.playlist_body.as_bytes(),
        )?;
        for segment in &track.segments {
            publish_bytes(&track_dir.join(&segment.uri), segment.body.as_bytes())?;
        }
    }

    Ok(sidecars)
}

/// Parses one native text subtitle payload chunk into normalized cues.
pub fn parse_native_text_subtitle_cues(
    codec: &str,
    samples: &[ChunkSample],
    payload: &[u8],
) -> Vec<TextSubtitleCue> {
    samples
        .iter()
        .filter_map(|sample| {
            let start = sample.payload_offset as usize;
            let end = start.checked_add(sample.byte_count as usize)?;
            let bytes = payload.get(start..end)?;
            let text = decode_native_text_payload(codec, bytes)?;
            Some(TextSubtitleCue {
                start_ms: sample.pts.as_millis(),
                end_ms: sample
                    .pts
                    .as_millis()
                    .saturating_add(sample.duration.as_millis())
                    .max(sample.pts.as_millis().saturating_add(1)),
                text,
            })
        })
        .collect()
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

fn sanitize_subtitle_text(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    let mut brace_depth = 0_u32;
    let mut in_tag = false;
    for ch in text.chars() {
        match ch {
            '{' if !in_tag => brace_depth = brace_depth.saturating_add(1),
            '}' if brace_depth > 0 && !in_tag => brace_depth -= 1,
            '<' if brace_depth == 0 => in_tag = true,
            '>' if in_tag => in_tag = false,
            _ if brace_depth == 0 && !in_tag => out.push(ch),
            _ => {}
        }
    }
    out.lines()
        .map(str::trim)
        .filter(|line| !line.is_empty())
        .collect::<Vec<_>>()
        .join("\n")
}

fn decode_native_text_payload(codec: &str, payload: &[u8]) -> Option<String> {
    let raw = match codec {
        "mov_text" | "tx3g" | "text" => decode_mov_text_payload(payload)?,
        "ass" | "ssa" => decode_ass_payload(payload)?,
        "subrip" | "webvtt" | "subviewer" | "microdvd" => {
            String::from_utf8_lossy(payload).into_owned()
        }
        _ => return None,
    };
    let sanitized = sanitize_subtitle_text(&raw);
    (!sanitized.is_empty()).then_some(sanitized)
}

fn decode_mov_text_payload(payload: &[u8]) -> Option<String> {
    if payload.len() < 2 {
        return None;
    }
    let len = u16::from_be_bytes([payload[0], payload[1]]) as usize;
    let text = payload.get(2..2 + len)?;
    Some(String::from_utf8_lossy(text).into_owned())
}

fn decode_ass_payload(payload: &[u8]) -> Option<String> {
    let raw = String::from_utf8_lossy(payload);
    let body = raw
        .strip_prefix("Dialogue:")
        .map(str::trim)
        .and_then(|line| line.splitn(10, ',').nth(9))
        .or_else(|| raw.splitn(9, ',').nth(8))
        .unwrap_or(raw.as_ref());
    Some(body.replace("\\N", "\n").replace("\\n", "\n"))
}

fn render_webvtt_media_playlist(segments: &[WebVttSegment]) -> String {
    let target_duration = segments
        .iter()
        .map(|segment| segment.duration_ms.div_ceil(1000))
        .max()
        .unwrap_or(1)
        .max(1);
    let mut out = format!(
        "#EXTM3U\n#EXT-X-VERSION:3\n#EXT-X-TARGETDURATION:{target_duration}\n#EXT-X-MEDIA-SEQUENCE:0\n#EXT-X-PLAYLIST-TYPE:VOD\n"
    );
    for segment in segments {
        out.push_str(&format!(
            "#EXTINF:{:.3},\n{}\n",
            segment.duration_ms as f64 / 1000.0,
            segment.uri
        ));
    }
    out.push_str("#EXT-X-ENDLIST\n");
    out
}

fn segment_uri(uri_prefix: &str, idx: u64) -> String {
    if uri_prefix.is_empty() {
        format!("seg-{idx:05}.vtt")
    } else {
        format!("{uri_prefix}/seg-{idx:05}.vtt")
    }
}

fn safe_path_component(value: &str) -> String {
    let safe = value
        .chars()
        .map(|ch| {
            if ch.is_ascii_alphanumeric() || matches!(ch, '-' | '_') {
                ch
            } else {
                '_'
            }
        })
        .collect::<String>();
    if safe.is_empty() {
        "subtitle".to_string()
    } else {
        safe
    }
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
    fn encodes_mov_text_sample_with_length_prefix() {
        let sample = encode_mov_text_sample("{\\an8}<i>Hello</i>\nworld").unwrap();
        assert_eq!(&sample[..2], &(11_u16).to_be_bytes());
        assert_eq!(&sample[2..], b"Hello\nworld");
    }

    #[test]
    fn converts_cues_to_mov_text_samples() {
        let samples = cues_to_mov_text_samples(&[
            TextSubtitleCue {
                start_ms: 100,
                end_ms: 900,
                text: "First".to_string(),
            },
            TextSubtitleCue {
                start_ms: 1_000,
                end_ms: 1_100,
                text: "   ".to_string(),
            },
        ]);

        assert_eq!(samples.len(), 1);
        assert_eq!(samples[0].start_ms, 100);
        assert_eq!(samples[0].end_ms, 900);
        assert_eq!(&samples[0].payload[2..], b"First");
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

    #[test]
    fn builds_webvtt_sidecars_for_all_selected_text_tracks() {
        let sidecars = build_webvtt_sidecars(
            &[
                WebVttSidecarInput {
                    track_id: "s0".to_string(),
                    language: Some("eng".to_string()),
                    name: Some("English".to_string()),
                    cues: vec![TextSubtitleCue {
                        start_ms: 0,
                        end_ms: 1000,
                        text: "Hello".to_string(),
                    }],
                },
                WebVttSidecarInput {
                    track_id: "commentary/fr".to_string(),
                    language: Some("fra".to_string()),
                    name: None,
                    cues: vec![TextSubtitleCue {
                        start_ms: 1500,
                        end_ms: 2500,
                        text: "Bonjour".to_string(),
                    }],
                },
            ],
            1000,
        );

        assert_eq!(sidecars.tracks.len(), 2);
        assert_eq!(sidecars.tracks[0].playlist_uri, "subs/s0/index.m3u8");
        assert_eq!(
            sidecars.tracks[1].playlist_uri,
            "subs/commentary_fr/index.m3u8"
        );
        assert!(sidecars.tracks[0].playlist_body.contains("#EXTM3U"));
        assert_eq!(sidecars.tracks[0].segments[0].uri, "seg-00000.vtt");
    }

    #[test]
    fn writes_webvtt_sidecars_in_one_call() {
        let temp = tempfile::tempdir().unwrap();
        let sidecars = write_webvtt_sidecars(
            temp.path(),
            &[WebVttSidecarInput {
                track_id: "s0".to_string(),
                language: Some("eng".to_string()),
                name: Some("English".to_string()),
                cues: vec![TextSubtitleCue {
                    start_ms: 0,
                    end_ms: 1000,
                    text: "Hello".to_string(),
                }],
            }],
            1000,
        )
        .unwrap();

        assert_eq!(sidecars.tracks.len(), 1);
        let playlist = temp.path().join("subs").join("s0").join("index.m3u8");
        let segment = temp.path().join("subs").join("s0").join("seg-00000.vtt");
        assert!(playlist.exists());
        assert!(segment.exists());
        assert!(
            std::fs::read_to_string(playlist)
                .unwrap()
                .contains("#EXT-X-ENDLIST")
        );
        assert!(std::fs::read_to_string(segment).unwrap().contains("Hello"));
    }

    #[test]
    fn parses_native_mov_text_subtitle_packets() {
        let mut payload = Vec::new();
        payload.extend_from_slice(&5_u16.to_be_bytes());
        payload.extend_from_slice(b"Hello");
        let samples = vec![sample(0, payload.len() as u32, 1_000, 2_000)];

        let cues = parse_native_text_subtitle_cues("mov_text", &samples, &payload);

        assert_eq!(cues.len(), 1);
        assert_eq!(cues[0].start_ms, 1_000);
        assert_eq!(cues[0].end_ms, 3_000);
        assert_eq!(cues[0].text, "Hello");
    }

    #[test]
    fn parses_native_ass_subtitle_packets() {
        let payload = b"0,0,Default,,0,0,0,,{\\an8}<i>Hello\\Nworld</i>";
        let samples = vec![sample(0, payload.len() as u32, 5_000, 1_500)];

        let cues = parse_native_text_subtitle_cues("ass", &samples, payload);

        assert_eq!(cues.len(), 1);
        assert_eq!(cues[0].text, "Hello\nworld");
    }

    #[test]
    fn builds_sidecars_from_native_text_tracks() {
        let payload = b"Native line";
        let samples = vec![sample(0, payload.len() as u32, 0, 1_000)];
        let sidecars = build_webvtt_sidecars_from_native_text_tracks(
            &[NativeTextSubtitleTrack {
                track_id: "s0".to_string(),
                codec: "subrip".to_string(),
                language: Some("eng".to_string()),
                name: Some("English".to_string()),
                samples: &samples,
                payload,
            }],
            1_000,
        );

        assert_eq!(sidecars.tracks.len(), 1);
        assert!(sidecars.tracks[0].segments[0].body.contains("Native line"));
    }

    fn sample(offset: u64, size: u32, pts_ms: u64, duration_ms: u64) -> ChunkSample {
        ChunkSample {
            index: 0,
            payload_offset: offset,
            byte_count: size,
            pts: crate::packet::TimePoint::millis(pts_ms),
            dts: crate::packet::TimePoint::millis(pts_ms),
            duration: crate::packet::TimeDelta::millis(duration_ms),
            keyframe: true,
        }
    }
}
