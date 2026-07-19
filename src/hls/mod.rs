use std::fs;
use std::path::Path;

use anyhow::{bail, Result};

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

pub fn render_master_playlist(variants: &[HlsVariant], subtitles: &[HlsSubtitleRendition]) -> String {
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
}
