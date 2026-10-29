//! Native repeated-audio boundaries require agreement from two peer episodes.
//! The detector stores compact spectral fingerprints, never source PCM or text.
mod fingerprint;
mod source;

use super::{DetectedMarker, MarkerKind};
use anyhow::{Context, Result, ensure};
use fingerprint::{MAX_FRAMES, Window};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::{
    fs::{self, OpenOptions},
    io::Read,
    path::{Path, PathBuf},
    time::UNIX_EPOCH,
};

const VERSION: u32 = 1;
const CACHE_LIMIT: usize = 2 * 1024 * 1024;
#[derive(Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
struct Cached {
    version: u32,
    stamp: String,
    duration_ms: u64,
    windows: Vec<Window>,
}
fn identity(path: &Path, spec: &source::Spec) -> Result<String> {
    let path = path.canonicalize()?;
    let meta = path.metadata()?;
    let modified = meta.modified()?.duration_since(UNIX_EPOCH)?.as_nanos();
    let mut hash = Sha256::new();
    hash.update(format!(
        "audio-repeat-{VERSION}:{}:{}:{modified}:{}:{}:",
        path.display(),
        meta.len(),
        spec.id,
        spec.codec
    ));
    hash.update(&spec.config);
    #[cfg(unix)]
    {
        use std::os::unix::fs::MetadataExt;
        hash.update(meta.dev().to_le_bytes());
        hash.update(meta.ino().to_le_bytes());
    }
    Ok(format!("{:x}", hash.finalize()))
}
fn valid(cached: &Cached, stamp: &str, duration: u64) -> bool {
    if cached.version != VERSION
        || cached.stamp != stamp
        || cached.duration_ms != duration
        || cached.windows.len() != 2
    {
        return false;
    }
    let mut count = 0;
    for window in &cached.windows {
        if window.start_ms >= window.end_ms
            || window.end_ms > duration
            || window.end_ms - window.start_ms > 360_000
        {
            return false;
        }
        count += window.frames.len();
        if count > MAX_FRAMES {
            return false;
        }
        let mut previous = None;
        for frame in &window.frames {
            if frame.t < window.start_ms
                || frame.t.saturating_add(128) > window.end_ms
                || previous.is_some_and(|last| last >= frame.t)
            {
                return false;
            }
            previous = Some(frame.t);
        }
    }
    true
}
fn cached(path: &Path) -> Result<Cached> {
    let metadata = path.symlink_metadata()?;
    ensure!(
        metadata.is_file()
            && !metadata.file_type().is_symlink()
            && metadata.len() <= CACHE_LIMIT as u64,
        "Acoustic cache entry exceeds bounds"
    );
    let mut options = OpenOptions::new();
    options.read(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.custom_flags(libc::O_NOFOLLOW);
    }
    let file = options.open(path)?;
    let mut bytes = Vec::new();
    file.take((CACHE_LIMIT + 1) as u64)
        .read_to_end(&mut bytes)?;
    ensure!(
        bytes.len() <= CACHE_LIMIT,
        "Acoustic cache entry exceeds bounds"
    );
    Ok(serde_json::from_slice(&bytes)?)
}
fn fingerprints(
    path: &Path,
    wanted: Option<&str>,
    cache: &Path,
) -> Result<(Vec<Window>, Option<String>)> {
    let (source, spec) = source::open(path, wanted)?;
    let stamp = identity(path, &spec)?;
    let file = cache.join(format!("{stamp}.json"));
    if let Ok(cached) = cached(&file)
        && valid(&cached, &stamp, spec.duration_ms)
    {
        source.validate_current()?;
        return Ok((cached.windows, spec.language));
    }
    let windows = source::capture(&source, &spec)?;
    let cached = Cached {
        version: VERSION,
        stamp,
        duration_ms: spec.duration_ms,
        windows,
    };
    ensure!(
        valid(&cached, &cached.stamp, spec.duration_ms),
        "Acoustic fingerprint bounds are invalid"
    );
    let bytes = serde_json::to_vec(&cached)?;
    ensure!(
        bytes.len() <= CACHE_LIMIT,
        "Acoustic fingerprint cache exceeds bounds"
    );
    source.validate_current()?;
    crate::output::publish_bytes(&file, &bytes)?;
    source.validate_current()?;
    Ok((cached.windows, spec.language))
}
fn prune(cache: &Path) -> Result<()> {
    let mut entries = Vec::new();
    for (index, item) in fs::read_dir(cache)?.enumerate() {
        ensure!(index < 4096, "Acoustic cache directory exceeds entry limit");
        let item = item?;
        let path = item.path();
        let Some(stem) = path.file_stem().and_then(|s| s.to_str()) else {
            continue;
        };
        if path.extension().and_then(|s| s.to_str()) != Some("json")
            || stem.len() != 64
            || !stem.bytes().all(|b| b.is_ascii_hexdigit())
        {
            continue;
        }
        let meta = path.symlink_metadata()?;
        if !meta.is_file() || meta.file_type().is_symlink() {
            continue;
        }
        entries.push((meta.modified()?, meta.len(), path));
    }
    entries.sort_by_key(|entry| entry.0);
    let mut bytes = entries.iter().map(|entry| entry.1).sum::<u64>();
    let mut count = entries.len();
    for (_, size, path) in entries {
        if count <= 64 && bytes <= 64 * 1024 * 1024 {
            break;
        }
        if fs::remove_file(path).is_ok() {
            bytes = bytes.saturating_sub(size);
            count -= 1;
        }
    }
    Ok(())
}

pub(super) fn detect(
    input: &Path,
    references: &[PathBuf],
    cache_dir: &Path,
) -> Result<Vec<DetectedMarker>> {
    ensure!(
        references.len() == 2,
        "Reliable audio repetition analysis requires two reference episodes"
    );
    let input_identity = input.canonicalize()?;
    let mut peer_paths = Vec::new();
    for reference in references {
        let path = reference.canonicalize()?;
        ensure!(
            path != input_identity && !peer_paths.contains(&path),
            "Audio analysis references must be distinct sources"
        );
        peer_paths.push(path);
    }
    let cache = cache_dir.join("acoustic-v1");
    fs::create_dir_all(&cache)?;
    let metadata = cache.symlink_metadata()?;
    ensure!(
        metadata.is_dir() && !metadata.file_type().is_symlink(),
        "Acoustic cache must be a regular directory"
    );
    let (target, language) =
        fingerprints(input, None, &cache).context("Target acoustic fingerprint unavailable")?;
    let (first, _) = fingerprints(&peer_paths[0], language.as_deref(), &cache)
        .context("First peer acoustic fingerprint unavailable")?;
    let (second, _) = fingerprints(&peer_paths[1], language.as_deref(), &cache)
        .context("Second peer acoustic fingerprint unavailable")?;
    let output = consensus_windows(&target, &first, &second)
        .into_iter()
        .map(|(index, found)| DetectedMarker {
            kind: if index == 0 {
                MarkerKind::Intro
            } else {
                MarkerKind::Credits
            },
            start_time_offset: found.start_ms,
            end_time_offset: found.end_ms,
            confidence: found.quality,
            method: "native-audio-repetition-v1".into(),
            reference_count: 2,
        })
        .collect();
    // Cached fingerprints are complete in memory before trimming. Concurrent
    // readers can always regenerate an evicted immutable source identity.
    if let Err(error) = prune(&cache) {
        eprintln!("Acoustic cache pruning deferred: {error}");
    }
    Ok(output)
}

fn consensus_windows(
    target: &[Window],
    first: &[Window],
    second: &[Window],
) -> Vec<(usize, fingerprint::Match)> {
    let mut output = Vec::new();
    for (index, window) in target.iter().enumerate() {
        // A premiere may use the regular opening theme as its ending. Match
        // measured content across both bounded peer windows; the target's
        // actual source position determines the contextual skip control.
        let matches = |peers: &[Window]| {
            peers
                .iter()
                .enumerate()
                .flat_map(|(peer_index, peer)| {
                    fingerprint::compare(window, peer)
                        .into_iter()
                        .filter(move |found| {
                            (peer_index == 0 || terminal(found, peer.end_ms))
                                && (index == 0 || terminal(found, window.end_ms))
                        })
                })
                .collect::<Vec<_>>()
        };
        let a = matches(first);
        let b = matches(second);
        let mut matches = fingerprint::consensus(&a, &b);
        matches.sort_by(|left, right| {
            (right.end_ms - right.start_ms)
                .cmp(&(left.end_ms - left.start_ms))
                .then(right.quality.total_cmp(&left.quality))
        });
        if let Some(found) = matches.into_iter().next() {
            output.push((index, found));
        }
    }
    output
}

// Position is a conservative eligibility gate on an independently measured
// match, never a fixed-duration credits guess. Late story reprises stay intact.
fn terminal(found: &fingerprint::Match, source_end: u64) -> bool {
    found.start_ms >= source_end.saturating_sub(240_000)
        && found.end_ms >= source_end.saturating_sub(90_000)
        && found.end_ms <= source_end
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn cache_validation_rejects_unknown_versions_bad_clocks_and_oversized_windows() {
        let mut value = Cached {
            version: VERSION,
            stamp: "identity".into(),
            duration_ms: 600_000,
            windows: vec![
                Window {
                    start_ms: 0,
                    end_ms: 300_000,
                    frames: vec![fingerprint::Feature {
                        t: 100,
                        b: 3,
                        v: true,
                    }],
                },
                Window {
                    start_ms: 300_000,
                    end_ms: 600_000,
                    frames: Vec::new(),
                },
            ],
        };
        assert!(valid(&value, "identity", 600_000));
        assert!(!valid(&value, "other", 600_000));
        value.windows[0].frames.push(fingerprint::Feature {
            t: 99,
            b: 4,
            v: true,
        });
        assert!(!valid(&value, "identity", 600_000));
        value.windows[0].frames.pop();
        value.version += 1;
        assert!(!valid(&value, "identity", 600_000));
        value.version = VERSION;
        value.windows[0].end_ms = 400_000;
        assert!(!valid(&value, "identity", 600_000));
    }
    #[test]
    fn premiere_ending_can_repeat_peer_openings_without_inventing_an_early_intro() {
        fn window(seed: u64, start: u64) -> Window {
            let mut state = seed;
            Window {
                start_ms: start,
                end_ms: start + 64_000,
                frames: (0..2000)
                    .map(|i| {
                        state ^= state << 13;
                        state ^= state >> 7;
                        state ^= state << 17;
                        fingerprint::Feature {
                            t: start + i * 32,
                            b: state,
                            v: true,
                        }
                    })
                    .collect(),
            }
        }
        let target = vec![window(43, 0), window(44, 1_000_000)];
        let mut first = vec![window(45, 0), window(46, 1_000_000)];
        let mut second = vec![window(47, 0), window(48, 1_000_000)];
        for i in 200..1500 {
            first[0].frames[i + 200].b = target[1].frames[i].b;
            second[0].frames[i - 100].b = target[1].frames[i].b;
        }
        let found = consensus_windows(&target, &first, &second);
        assert_eq!(found.len(), 1);
        assert_eq!(found[0].0, 1);
        assert!(found[0].1.start_ms > 1_006_000 && found[0].1.end_ms < 1_049_000);
        // The exact same repeated song in late story, followed by several
        // minutes of video, must not be relabeled as a credits range.
        let mut late_story = target.clone();
        late_story[1].end_ms += 240_000;
        assert!(consensus_windows(&late_story, &first, &second).is_empty());
        // Likewise a nonterminal peer's late-story reprise is not ending
        // evidence, even when the target happens to place it near its end.
        for peer in [&mut first, &mut second] {
            for i in 200..1500 {
                peer[0].frames[i].b ^= 0xa55a_1234_8765_fedc;
                peer[1].frames[i].b = target[1].frames[i].b;
            }
            peer[0].frames.clear();
            peer[1].end_ms += 240_000;
        }
        assert!(consensus_windows(&target, &first, &second).is_empty());
    }

    #[test]
    fn duplicate_reference_sources_cannot_create_consensus() {
        let temp = tempfile::tempdir().unwrap();
        let file = temp.path().join("input");
        fs::write(&file, b"not media").unwrap();
        assert!(detect(&file, &[file.clone(), file.clone()], temp.path()).is_err());
        assert!(detect(&file, std::slice::from_ref(&file), temp.path()).is_err());
    }
}
