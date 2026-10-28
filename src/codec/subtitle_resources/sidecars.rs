//! Same-basename subtitle discovery never follows symlinks or uses authored names as output paths.
use super::{AGGREGATE_LIMIT, SCRIPT_LIMIT, SubtitleResources, SubtitleScriptResource};
use anyhow::{Context, Result, bail};
use std::{fs::OpenOptions, io::Read, path::Path};

pub(super) fn append(
    input: &Path,
    output: &Path,
    resources: &mut SubtitleResources,
    total: &mut usize,
    first_id: usize,
) -> Result<()> {
    let directory = input
        .parent()
        .context("media parent missing")?
        .canonicalize()?;
    let stem = input
        .file_stem()
        .and_then(|stem| stem.to_str())
        .context("media basename is invalid")?
        .to_lowercase();
    let prefix = format!("{stem}.");
    let mut candidates = Vec::new();
    for (index, entry) in std::fs::read_dir(&directory)?.enumerate() {
        if index >= 10_000 {
            bail!("sidecar directory entry limit exceeded");
        }
        let entry = entry?;
        let kind = entry.file_type()?;
        if !kind.is_file() || kind.is_symlink() {
            continue;
        }
        let path = entry.path();
        let Some(extension) = path
            .extension()
            .and_then(|extension| extension.to_str())
            .map(str::to_ascii_lowercase)
        else {
            continue;
        };
        if !["ass", "ssa", "srt", "vtt", "webvtt"].contains(&extension.as_str()) {
            continue;
        }
        let Some(name) = path.file_stem().and_then(|name| name.to_str()) else {
            continue;
        };
        let lower = name.to_lowercase();
        if lower != stem && !lower.starts_with(&prefix) {
            continue;
        }
        if path.canonicalize()?.parent() != Some(directory.as_path()) {
            continue;
        }
        if candidates.len() >= 32 {
            bail!("sidecar file count limit exceeded");
        }
        candidates.push((
            path.clone(),
            extension,
            lower.strip_prefix(&prefix).unwrap_or("").to_string(),
        ));
    }
    candidates.sort_by(|left, right| left.0.cmp(&right.0));
    for (index, (path, extension, suffix)) in candidates.into_iter().enumerate() {
        let mut options = OpenOptions::new();
        options.read(true);
        #[cfg(unix)]
        {
            use std::os::unix::fs::OpenOptionsExt;
            options.custom_flags(libc::O_NOFOLLOW);
        }
        let Ok(mut file) = options.open(&path) else {
            continue;
        };
        let metadata = file.metadata()?;
        if !metadata.is_file() || metadata.len() > SCRIPT_LIMIT as u64 {
            bail!("sidecar script size limit exceeded");
        }
        if path.symlink_metadata()?.file_type().is_symlink()
            || path.canonicalize()?.parent() != Some(directory.as_path())
        {
            continue;
        }
        let mut bytes = Vec::new();
        file.by_ref()
            .take((SCRIPT_LIMIT + 1) as u64)
            .read_to_end(&mut bytes)?;
        if bytes.len() > SCRIPT_LIMIT {
            bail!("sidecar script size limit exceeded");
        }
        let text = if let Some(data) = bytes.strip_prefix(&[0xff, 0xfe]) {
            let (pairs, remainder) = data.as_chunks::<2>();
            if !remainder.is_empty() {
                continue;
            }
            let units = pairs
                .iter()
                .map(|pair| u16::from_le_bytes(*pair))
                .collect::<Vec<_>>();
            let Ok(text) = String::from_utf16(&units) else {
                continue;
            };
            text
        } else if let Some(data) = bytes.strip_prefix(&[0xfe, 0xff]) {
            let (pairs, remainder) = data.as_chunks::<2>();
            if !remainder.is_empty() {
                continue;
            }
            let units = pairs
                .iter()
                .map(|pair| u16::from_be_bytes(*pair))
                .collect::<Vec<_>>();
            let Ok(text) = String::from_utf16(&units) else {
                continue;
            };
            text
        } else {
            let Ok(text) = String::from_utf8(bytes) else {
                continue;
            };
            text
        };
        let codec = match extension.as_str() {
            "srt" => "subrip",
            "vtt" | "webvtt" => "webvtt",
            codec => codec,
        };
        let id = format!("s{}", first_id + index);
        let (vtt, duration_ms) = if codec == "subrip" {
            let cues = crate::codec::subtitles::parse_subrip(&text);
            if cues.is_empty() {
                continue;
            }
            let duration = cues.iter().map(|cue| cue.end_ms).max().unwrap_or(0);
            let native = cues
                .into_iter()
                .map(|cue| super::matroska::Cue {
                    start_ms: cue.start_ms,
                    duration_ms: cue.end_ms.saturating_sub(cue.start_ms),
                    text: cue.text,
                    addition: None,
                })
                .collect::<Vec<_>>();
            (Some(super::assemble_webvtt(None, &native)?), duration)
        } else if codec == "webvtt" {
            let normalized = text.trim_start_matches('\u{feff}').replace("\r\n", "\n");
            if !normalized.starts_with("WEBVTT") {
                continue;
            }
            let mut lines = normalized.lines();
            let first = lines.next().unwrap_or("WEBVTT");
            let mut normalized = format!("{first}\nX-TIMESTAMP-MAP=LOCAL:00:00:00.000,MPEGTS:0\n");
            for line in lines.filter(|line| !line.starts_with("X-TIMESTAMP-MAP")) {
                normalized.push_str(line);
                normalized.push('\n');
            }
            (Some(normalized), 0)
        } else {
            (None, 0)
        };
        *total = total
            .checked_add(text.len() + vtt.as_ref().map_or(0, String::len))
            .context("sidecar aggregate overflow")?;
        if *total > AGGREGATE_LIMIT {
            bail!("sidecar aggregate size limit exceeded");
        }
        let file_extension = if codec == "webvtt" {
            "webvtt"
        } else {
            extension.as_str()
        };
        let file_name = format!("{id}.{file_extension}");
        crate::output::publish_bytes(&output.join(&file_name), text.as_bytes())?;
        let web_vtt_file_name = if let Some(vtt) = vtt {
            let name = format!("{id}.vtt");
            crate::output::publish_bytes(&output.join(&name), vtt.as_bytes())?;
            Some(name)
        } else {
            None
        };
        let parts = suffix.split(['.', '_']).collect::<Vec<_>>();
        let forced = parts
            .iter()
            .any(|part| ["forced", "foreign"].contains(part));
        let default = parts.contains(&"default");
        let language = parts
            .iter()
            .find(|part| {
                part.chars().all(|c| c.is_ascii_alphabetic()) && [2, 3].contains(&part.len())
                    || part.len() == 5 && part.as_bytes().get(2) == Some(&b'-')
            })
            .map(|part| part.to_string());
        let name = if suffix.is_empty() {
            "External Subtitles".into()
        } else {
            suffix.replace(['.', '_'], " ")
        };
        resources.tracks.push(SubtitleScriptResource {
            id,
            codec: codec.into(),
            file_name,
            web_vtt_file_name,
            language,
            name: Some(name),
            default,
            forced,
            duration_ms,
            external: true,
        });
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn sidecars_are_basename_scoped_and_keep_ass_bytes_and_native_text_timing() {
        let dir = tempfile::tempdir().unwrap();
        let input = dir.path().join("Film.mkv");
        std::fs::write(&input, b"video source is opened by caller").unwrap();
        let authored = "[Script Info]\n[V4+ Styles]\nStyle: Authored,Special Font,48\n[Events]\nDialogue: 0,0:00:01.00,0:00:02.00,Authored,,0,0,0,,{\\pos(20,20)}Text";
        std::fs::write(dir.path().join("Film.en.forced.ass"), authored).unwrap();
        std::fs::write(
            dir.path().join("Film.fr.srt"),
            "1\n00:00:05,000 --> 00:00:06,200\nBonjour\n",
        )
        .unwrap();
        std::fs::write(
            dir.path().join("Film.webvtt"),
            "WEBVTT\n\n00:00:05.000 --> 00:00:06.000\nCue\n",
        )
        .unwrap();
        std::fs::write(dir.path().join("Unrelated.srt"), "must not be exposed").unwrap();
        std::fs::write(dir.path().join("Film.zz.bad.srt"), [0xff, 0xfa, 0x00]).unwrap();
        let output = dir.path().join("output");
        std::fs::create_dir(&output).unwrap();
        let mut resources = SubtitleResources::default();
        append(&input, &output, &mut resources, &mut 0, 4).unwrap();
        assert_eq!(resources.tracks.len(), 3);
        assert!(resources.tracks.iter().all(|track| track.external));
        let ass = resources
            .tracks
            .iter()
            .find(|track| track.codec == "ass")
            .unwrap();
        assert_eq!(ass.id, "s4");
        assert!(ass.forced);
        assert_eq!(ass.language.as_deref(), Some("en"));
        assert_eq!(
            std::fs::read_to_string(output.join(&ass.file_name)).unwrap(),
            authored
        );
        let srt = resources
            .tracks
            .iter()
            .find(|track| track.codec == "subrip")
            .unwrap();
        let vtt =
            std::fs::read_to_string(output.join(srt.web_vtt_file_name.as_ref().unwrap())).unwrap();
        assert!(vtt.contains("00:00:05.000 --> 00:00:06.200"));
        assert!(vtt.contains("X-TIMESTAMP-MAP=LOCAL:00:00:00.000,MPEGTS:0"));
        assert!(!output.join("Unrelated.srt").exists());
    }
    #[cfg(unix)]
    #[test]
    fn sidecar_symlinks_are_never_followed() {
        use std::os::unix::fs::symlink;
        let dir = tempfile::tempdir().unwrap();
        let other = tempfile::tempdir().unwrap();
        let input = dir.path().join("Film.mkv");
        std::fs::write(&input, b"video").unwrap();
        let secret = other.path().join("Outside.ass");
        std::fs::write(&secret, b"outside content").unwrap();
        symlink(&secret, dir.path().join("Film.en.ass")).unwrap();
        let mut resources = SubtitleResources::default();
        append(&input, dir.path(), &mut resources, &mut 0, 0).unwrap();
        assert!(resources.tracks.is_empty());
    }
}
