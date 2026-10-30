//! Native, redistributable fixture: no media server, network share or TV needed.
#[path = "support/modern_fixture.rs"]
mod fixture;
use assert_cmd::Command;
use serde_json::Value;

#[test]
fn opus_fingerprints_and_visual_evidence_are_reused_across_processes() {
    let dir = tempfile::tempdir().unwrap();
    let data = fixture::modern_mkv(120);
    let sources = (0..3)
        .map(|i| {
            let p = dir.path().join(format!("episode-{i}.mkv"));
            std::fs::write(&p, &data).unwrap();
            p
        })
        .collect::<Vec<_>>();
    let cache = dir.path().join("evidence");
    let audio = Command::cargo_bin("chroma-engine")
        .unwrap()
        .arg("analyze-playback-markers")
        .arg(&sources[0])
        .arg("--cache-dir")
        .arg(&cache)
        .arg("--audio-only")
        .arg("--reference")
        .arg(&sources[1])
        .arg("--reference")
        .arg(&sources[2])
        .assert()
        .success()
        .get_output()
        .stdout
        .clone();
    let output: Value = serde_json::from_slice(&audio).unwrap();
    assert_eq!(output["notices"], serde_json::json!([]));
    assert_eq!(output["markers"], serde_json::json!([])); // Silence cannot become an intro.
    assert_eq!(
        std::fs::read_dir(cache.join("chromaprint-v2"))
            .unwrap()
            .count(),
        3
    );
    fn visual(path: &std::path::Path, cache: &std::path::Path) -> std::process::Output {
        Command::cargo_bin("chroma-engine")
            .unwrap()
            .arg("analyze-playback-markers")
            .arg(path)
            .arg("--cache-dir")
            .arg(cache)
            .assert()
            .success()
            .get_output()
            .clone()
    }
    let first = visual(&sources[0], &cache);
    let second = visual(&sources[0], &cache);
    let a: Value = serde_json::from_slice(&first.stdout).unwrap();
    let b: Value = serde_json::from_slice(&second.stdout).unwrap();
    assert_eq!(a, b);
    assert_eq!(a["notices"], serde_json::json!([]));
    assert!(String::from_utf8_lossy(&first.stderr).contains("Visual evidence:"));
    assert!(!String::from_utf8_lossy(&second.stderr).contains("Visual evidence:"));
}
