use std::fs;
use std::path::Path;

use anyhow::{bail, Result};

use crate::codec::subtitles::WebVttSegment;

pub fn run_hls_session(
    input: &Path,
    work_dir: &Path,
    anchor_seconds: f64,
    segment_seconds: f64,
) -> Result<()> {
    if !input.exists() {
        bail!("input not found");
    }
    if !anchor_seconds.is_finite() || anchor_seconds < 0.0 {
        bail!("anchor_seconds must be finite and >= 0");
    }
    if !segment_seconds.is_finite() || segment_seconds <= 0.0 {
        bail!("segment_seconds must be finite and > 0");
    }

    fs::create_dir_all(work_dir)?;
    bail!("hls fMP4 writer not implemented yet")
}

pub fn render_webvtt_media_playlist(segments: &[WebVttSegment]) -> String {
    let target_duration = segments
        .iter()
        .map(|segment| segment.duration_ms.div_ceil(1000))
        .max()
        .unwrap_or(0)
        .max(1);
    let mut out = format!(
        "#EXTM3U\n#EXT-X-VERSION:7\n#EXT-X-TARGETDURATION:{target_duration}\n#EXT-X-MEDIA-SEQUENCE:0\n"
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

pub fn render_master_playlist(
    variants: &[HlsVariant],
    subtitles: &[HlsSubtitleRendition],
) -> String {
    let mut out = String::from("#EXTM3U\n#EXT-X-VERSION:7\n");
    for sub in subtitles {
        out.push_str(&format!(
            "#EXT-X-MEDIA:TYPE=SUBTITLES,GROUP-ID=\"subs\",NAME=\"{}\",DEFAULT={},FORCED={},URI=\"{}\"\n",
            sub.name,
            yes_no(sub.default),
            yes_no(sub.forced),
            sub.uri
        ));
    }
    for variant in variants {
        let subtitles_attr = if subtitles.is_empty() {
            String::new()
        } else {
            ",SUBTITLES=\"subs\"".to_string()
        };
        out.push_str(&format!(
            "#EXT-X-STREAM-INF:BANDWIDTH={},CODECS=\"{}\"{}\n{}\n",
            variant.bandwidth, variant.codecs, subtitles_attr, variant.uri
        ));
    }
    out
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HlsVariant {
    pub bandwidth: u64,
    pub codecs: String,
    pub uri: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HlsSubtitleRendition {
    pub name: String,
    pub uri: String,
    pub default: bool,
    pub forced: bool,
}

fn yes_no(value: bool) -> &'static str {
    if value {
        "YES"
    } else {
        "NO"
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn renders_master_playlist() {
        let playlist = render_master_playlist(
            &[HlsVariant {
                bandwidth: 1_000_000,
                codecs: "avc1,mp4a.40.2".to_string(),
                uri: "0/playlist.m3u8".to_string(),
            }],
            &[],
        );
        assert!(playlist.contains("#EXTM3U"));
        assert!(playlist.contains("#EXT-X-STREAM-INF"));
    }

    #[test]
    fn renders_webvtt_media_playlist() {
        let playlist = super::render_webvtt_media_playlist(&[WebVttSegment {
            index: 0,
            start_ms: 0,
            duration_ms: 6000,
            uri: "s0/seg-00000.vtt".to_string(),
            body: "WEBVTT\n\n".to_string(),
        }]);
        assert!(playlist.contains("#EXT-X-TARGETDURATION:6"));
        assert!(playlist.contains("#EXTINF:6.000,"));
        assert!(playlist.contains("s0/seg-00000.vtt"));
        assert!(playlist.ends_with("#EXT-X-ENDLIST\n"));
    }
}
