//! Same-basename subtitle discovery never follows symlinks or uses authored names as output paths.
use super::{AGGREGATE_LIMIT, SCRIPT_LIMIT, SubtitleResources, SubtitleScriptResource};
use anyhow::{Context, Result, bail};
use std::{collections::HashSet, fs::OpenOptions, io::Read, path::Path};

const TEXT_EXTENSIONS: &[&str] = &[
    "ass", "ssa", "srt", "vtt", "webvtt", "ttml", "dfxp", "xml", "smi", "sami", "sub", "txt",
    "sbv", "scc",
];

pub(super) fn append(
    input: &Path,
    output: &Path,
    resources: &mut SubtitleResources,
    total: &mut usize,
    first_id: usize,
    source: Option<&crate::source::MediaSource>,
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
    let mut siblings = Vec::new();
    for (index, entry) in std::fs::read_dir(&directory)?.enumerate() {
        if index >= 10_000 {
            bail!("sidecar directory entry limit exceeded");
        }
        if let Some(source) = source {
            source.check_work()?;
        }
        let entry = entry?;
        let path = entry.path();
        // Filter names before asking the filesystem for file type. Network
        // directories may need a remote metadata request for every such check.
        let extension = path
            .extension()
            .and_then(|value| value.to_str())
            .unwrap_or("");
        if !extension.eq_ignore_ascii_case("idx")
            && !TEXT_EXTENSIONS
                .iter()
                .any(|candidate| extension.eq_ignore_ascii_case(candidate))
        {
            continue;
        }
        let name = path
            .file_stem()
            .and_then(|value| value.to_str())
            .unwrap_or("")
            .to_lowercase();
        if name != stem && !name.starts_with(&prefix) {
            continue;
        }
        let kind = entry.file_type()?;
        if kind.is_file() && !kind.is_symlink() {
            siblings.push(path);
        }
    }
    // VobSub pairs are case-insensitive, just like subtitle extension discovery.
    // Only regular, non-symlink siblings from the bounded scan may suppress the
    // text reader: the bitmap reader validates and opens both files separately.
    let indexes = siblings
        .iter()
        .filter(|path| {
            path.extension()
                .and_then(|extension| extension.to_str())
                .is_some_and(|extension| extension.eq_ignore_ascii_case("idx"))
        })
        .filter_map(|path| path.file_name()?.to_str().map(str::to_lowercase))
        .collect::<HashSet<_>>();
    let mut candidates = Vec::new();
    for path in siblings {
        let Some(extension) = path
            .extension()
            .and_then(|extension| extension.to_str())
            .map(str::to_ascii_lowercase)
        else {
            continue;
        };
        if !TEXT_EXTENSIONS.contains(&extension.as_str()) {
            continue;
        }
        if extension == "sub" {
            let index = path.with_extension("idx");
            if index
                .file_name()
                .and_then(|name| name.to_str())
                .is_some_and(|name| indexes.contains(&name.to_lowercase()))
            {
                continue;
            }
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
    let (source_duration, source_fps) = source
        .map(|source| {
            if crate::container::matroska::looks_like_ebml(source.as_ref()) {
                let metadata = crate::container::matroska::parse_basic_metadata(source.as_ref());
                (
                    metadata.duration_ms,
                    metadata.tracks.iter().find_map(|t| {
                        t.default_duration_ns
                            .filter(|v| *v > 0)
                            .map(|v| 1e9 / v as f64)
                    }),
                )
            } else {
                let metadata = crate::container::mp4::parse_basic_metadata(source.as_ref());
                (
                    metadata.duration_ms,
                    metadata.tracks.iter().find_map(|t| t.frame_rate),
                )
            }
        })
        .unwrap_or((None, None));
    let mut next_id = first_id;
    for (path, extension, suffix) in candidates {
        if let Some(source) = source {
            source.check_work()?;
        }
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
        if !metadata.is_file() {
            continue;
        }
        if metadata.len() > SCRIPT_LIMIT as u64 {
            reject(
                resources,
                &mut next_id,
                "External subtitle script size limit exceeded",
            );
            continue;
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
            reject(
                resources,
                &mut next_id,
                "External subtitle script size limit exceeded",
            );
            continue;
        }
        let text = match super::text_formats::encoding::decode(&extension, &bytes) {
            Ok(text) => text,
            Err(_) => {
                reject(
                    resources,
                    &mut next_id,
                    "Unsupported external subtitle text encoding",
                );
                continue;
            }
        };
        if !["ass", "ssa", "srt", "vtt", "webvtt"].contains(&extension.as_str()) {
            let converted = match super::text_formats::convert(
                &extension,
                &text,
                source_duration,
                source_fps,
            ) {
                Ok(tracks) => tracks,
                Err(_) => {
                    // Binary SUB belongs to the paired IDX/bitmap reader, and
                    // ordinary same-basename XML/TXT files need not be captions.
                    if !["sub", "xml", "txt"].contains(&extension.as_str()) {
                        resources
                            .unavailable_tracks
                            .push(super::UnavailableSubtitleResource {
                                id: format!("s{next_id}"),
                                reason: "Malformed or unsupported external timed-text subtitle"
                                    .into(),
                            });
                        next_id += 1;
                    }
                    continue;
                }
            };
            for converted in converted {
                if resources.tracks.len() >= 128 {
                    bail!("subtitle track limit exceeded");
                }
                *total = total
                    .checked_add(converted.webvtt.len())
                    .context("sidecar aggregate overflow")?;
                if *total > AGGREGATE_LIMIT {
                    bail!("sidecar aggregate size limit exceeded");
                }
                let id = format!("s{next_id}");
                next_id += 1;
                let filename = format!("{id}.vtt");
                crate::output::publish_bytes(&output.join(&filename), converted.webvtt.as_bytes())?;
                let parts = suffix.split(['.', '_']).collect::<Vec<_>>();
                resources.tracks.push(SubtitleScriptResource {
                    id,
                    codec: "webvtt".into(),
                    file_name: filename.clone(),
                    web_vtt_file_name: Some(filename),
                    overlay_file_name: None,
                    language: converted.language.or_else(|| {
                        parts
                            .iter()
                            .find(|p| {
                                p.len() >= 2
                                    && p.len() <= 3
                                    && p.bytes().all(|c| c.is_ascii_alphabetic())
                            })
                            .map(|p| p.to_string())
                    }),
                    name: converted
                        .label
                        .or_else(|| Some(suffix.replace(['.', '_'], " "))),
                    default: parts.contains(&"default"),
                    forced: parts.contains(&"forced") || parts.contains(&"foreign"),
                    duration_ms: converted.duration_ms,
                    external: true,
                });
            }
            continue;
        }
        let codec = match extension.as_str() {
            "srt" => "subrip",
            "vtt" | "webvtt" => "webvtt",
            codec => codec,
        };
        let id = format!("s{next_id}");
        next_id += 1;
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
            overlay_file_name: None,
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

fn reject(resources: &mut SubtitleResources, next_id: &mut usize, reason: &str) {
    resources
        .unavailable_tracks
        .push(super::UnavailableSubtitleResource {
            id: format!("s{next_id}"),
            reason: reason.into(),
        });
    *next_id += 1;
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn cancelled_sidecar_scan_does_not_publish_resources() {
        let dir = tempfile::tempdir().unwrap();
        let input = dir.path().join("Fixture.mkv");
        std::fs::write(&input, b"fixture").unwrap();
        std::fs::write(
            dir.path().join("Fixture.en.srt"),
            "1\n00:00:01,000 --> 00:00:02,000\nText\n",
        )
        .unwrap();
        let output = dir.path().join("output");
        std::fs::create_dir(&output).unwrap();
        let control = crate::WorkControl::default();
        let source = crate::source::MediaSource::open_with_context(
            &input,
            crate::EngineRuntime::new(crate::ResourcePolicy::default()).unwrap(),
            control.clone(),
        )
        .unwrap();
        control.cancel();
        let mut resources = SubtitleResources::default();
        assert!(append(&input, &output, &mut resources, &mut 0, 0, Some(&source)).is_err());
        assert!(resources.tracks.is_empty());
        assert_eq!(std::fs::read_dir(output).unwrap().count(), 0);
    }

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
        append(&input, &output, &mut resources, &mut 0, 4, None).unwrap();
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
    #[test]
    fn uppercase_vobsub_pair_skips_binary_text_read_and_preserves_valid_sidecar() {
        let dir = tempfile::tempdir().unwrap();
        let input = dir.path().join("Film.mkv");
        std::fs::write(&input, b"video").unwrap();
        std::fs::write(dir.path().join("Film.en.IDX"), b"# VobSub index file, v7\n").unwrap();
        // Real VobSub files routinely exceed the text script budget. The text
        // reader must leave this case-insensitive pair to the bitmap reader.
        std::fs::File::create(dir.path().join("Film.en.SUB"))
            .unwrap()
            .set_len(SCRIPT_LIMIT as u64 + 1)
            .unwrap();
        std::fs::write(
            dir.path().join("Film.fr.srt"),
            "1\n00:00:01,000 --> 00:00:02,000\nValid caption\n",
        )
        .unwrap();
        let output = dir.path().join("output");
        std::fs::create_dir(&output).unwrap();
        let mut resources = SubtitleResources::default();
        append(&input, &output, &mut resources, &mut 0, 0, None).unwrap();
        assert_eq!(resources.tracks.len(), 1);
        assert!(resources.unavailable_tracks.is_empty());
        assert_eq!(resources.tracks[0].language.as_deref(), Some("fr"));
        assert_eq!(std::fs::read_dir(output).unwrap().count(), 2);
    }

    #[test]
    fn oversized_individual_sidecar_is_reported_without_disabling_valid_tracks() {
        let dir = tempfile::tempdir().unwrap();
        let input = dir.path().join("Film.mkv");
        std::fs::write(&input, b"video").unwrap();
        std::fs::File::create(dir.path().join("Film.aa.srt"))
            .unwrap()
            .set_len(SCRIPT_LIMIT as u64 + 1)
            .unwrap();
        std::fs::write(
            dir.path().join("Film.en.srt"),
            "1\n00:00:03,000 --> 00:00:04,500\nValid caption\n",
        )
        .unwrap();
        let output = dir.path().join("output");
        std::fs::create_dir(&output).unwrap();
        let mut resources = SubtitleResources::default();
        let mut total = 0;
        append(&input, &output, &mut resources, &mut total, 4, None).unwrap();
        assert_eq!(resources.unavailable_tracks.len(), 1);
        assert_eq!(resources.unavailable_tracks[0].id, "s4");
        assert!(
            resources.unavailable_tracks[0]
                .reason
                .contains("size limit")
        );
        assert_eq!(resources.tracks.len(), 1);
        assert_eq!(resources.tracks[0].id, "s5");
        let text = std::fs::read_to_string(
            output.join(resources.tracks[0].web_vtt_file_name.as_ref().unwrap()),
        )
        .unwrap();
        assert!(text.contains("00:00:03.000 --> 00:00:04.500"));
        assert!(text.contains("Valid caption"));
        assert!(total > 0 && total < SCRIPT_LIMIT);
        assert!(!output.join("s4.srt").exists());
    }

    #[test]
    fn invalid_unicode_sidecar_is_isolated_from_valid_tracks() {
        let dir = tempfile::tempdir().unwrap();
        let input = dir.path().join("Film.mkv");
        std::fs::write(&input, b"video").unwrap();
        std::fs::write(dir.path().join("Film.aa.srt"), [0xff, 0xfe, 0x41]).unwrap();
        std::fs::write(
            dir.path().join("Film.en.srt"),
            "1\n00:00:01,000 --> 00:00:02,000\nValid caption\n",
        )
        .unwrap();
        let output = dir.path().join("output");
        std::fs::create_dir(&output).unwrap();
        let mut resources = SubtitleResources::default();
        append(&input, &output, &mut resources, &mut 0, 0, None).unwrap();
        assert_eq!(resources.unavailable_tracks.len(), 1);
        assert!(resources.unavailable_tracks[0].reason.contains("encoding"));
        assert_eq!(resources.tracks.len(), 1);
        assert_eq!(resources.tracks[0].language.as_deref(), Some("en"));
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
        append(&input, dir.path(), &mut resources, &mut 0, 0, None).unwrap();
        assert!(resources.tracks.is_empty());
    }
}
