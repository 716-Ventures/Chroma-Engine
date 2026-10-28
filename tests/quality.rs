#[path = "support/modern_fixture.rs"]
mod fixture;

use assert_cmd::Command;
use serde_json::Value;

#[test]
fn native_cli_quality_limits_reach_the_encoded_init_segment() {
    let dir = tempfile::tempdir().unwrap();
    let source = dir.path().join("source.mkv");
    let init = dir.path().join("init.mp4");
    let segment = dir.path().join("segment.m4s");
    std::fs::write(&source, fixture::modern_mkv(1)).unwrap();
    Command::cargo_bin("chroma-engine")
        .unwrap()
        .arg("transcode-fmp4-start")
        .arg(&source)
        .arg(&init)
        .arg(&segment)
        .args([
            "--index",
            "0",
            "--segment-ms",
            "1000",
            "--video-mode",
            "h264",
            "--max-width",
            "16",
            "--max-height",
            "16",
            "--video-bitrate",
            "1000000",
        ])
        .assert()
        .success();
    let output = Command::cargo_bin("chroma-engine")
        .unwrap()
        .arg("probe")
        .arg(&init)
        .assert()
        .success()
        .get_output()
        .stdout
        .clone();
    let json: Value = serde_json::from_slice(&output).unwrap();
    let video = json["tracks"]
        .as_array()
        .unwrap()
        .iter()
        .find(|track| track["kind"] == "video")
        .unwrap();
    assert_eq!(video["video"]["width"], 16);
    assert_eq!(video["video"]["height"], 16);
    assert!(std::fs::metadata(segment).unwrap().len() > 0);
}

#[test]
fn native_cli_rejects_unbounded_dimensions_before_reading_media() {
    let stderr = Command::cargo_bin("chroma-engine")
        .unwrap()
        .args([
            "transcode-fmp4-init",
            "missing.mkv",
            "unused.mp4",
            "--max-width",
            "1000000",
        ])
        .assert()
        .failure()
        .get_output()
        .stderr
        .clone();
    assert!(String::from_utf8_lossy(&stderr).contains("3840"));
}
