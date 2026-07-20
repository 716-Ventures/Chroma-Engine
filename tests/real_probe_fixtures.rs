use serde_json::Value;

#[test]
fn sanitized_real_mp4_probe_fixture_preserves_playback_shape() {
    let probe = fixture("real-mp4-renoir.probe.json");

    assert_eq!(probe["source"]["path"], "fixtures://movies/renoir.mp4");
    assert_eq!(probe["source"]["sizeBytes"], 0);
    assert_eq!(probe["source"]["container"]["family"], "isoBmff");
    assert_eq!(probe["durationMs"], 6_698_275);

    let tracks = probe["tracks"].as_array().unwrap();
    assert!(probe["chapters"].as_array().unwrap().is_empty());
    assert_eq!(tracks.len(), 4);
    assert_eq!(tracks[0]["kind"], "video");
    assert_eq!(tracks[0]["codec"]["family"], "h264");
    assert_eq!(tracks[1]["kind"], "audio");
    assert_eq!(tracks[1]["codec"]["family"], "aac");
    assert_eq!(tracks[1]["audio"]["channels"], 6);
    assert!(tracks.iter().any(|track| track["kind"] == "subtitle"));
}

#[test]
fn sanitized_real_mkv_probe_fixture_preserves_multi_audio_shape() {
    let probe = fixture("real-mkv-gremlins.probe.json");

    assert_eq!(probe["source"]["path"], "fixtures://movies/gremlins.mkv");
    assert_eq!(probe["source"]["sizeBytes"], 0);
    assert_eq!(probe["source"]["container"]["family"], "matroska");
    assert_eq!(probe["durationMs"], 6_372_715);

    let tracks = probe["tracks"].as_array().unwrap();
    assert_eq!(tracks[0]["kind"], "video");
    assert_eq!(tracks[0]["codec"]["family"], "hevc");
    assert_eq!(tracks[0]["video"]["width"], 3840);
    assert_eq!(tracks[0]["video"]["height"], 2160);

    let audio_tracks = tracks
        .iter()
        .filter(|track| track["kind"] == "audio")
        .collect::<Vec<_>>();
    assert!(audio_tracks.len() >= 8);
    assert!(
        audio_tracks
            .iter()
            .any(|track| track["codec"]["family"] == "dts")
    );
    assert!(audio_tracks.iter().any(|track| track["language"] == "fre"));
    assert!(audio_tracks.iter().any(|track| track["language"] == "spa"));
    assert!(all_titles_are_sanitized(tracks));

    let chapters = probe["chapters"].as_array().unwrap();
    assert_eq!(chapters.len(), 27);
    assert_eq!(chapters[0]["startMs"], 0);
    assert_eq!(chapters[0]["language"], "eng");
    assert!(all_titles_are_sanitized(chapters));
}

fn fixture(name: &str) -> Value {
    serde_json::from_str(match name {
        "real-mp4-renoir.probe.json" => {
            include_str!("fixtures/probes/real-mp4-renoir.probe.json")
        }
        "real-mkv-gremlins.probe.json" => {
            include_str!("fixtures/probes/real-mkv-gremlins.probe.json")
        }
        _ => unreachable!("unknown fixture"),
    })
    .unwrap()
}

fn all_titles_are_sanitized(tracks: &[Value]) -> bool {
    tracks.iter().all(|track| track["title"].is_null())
}
