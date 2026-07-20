use chroma_engine::{MediaProbe, PlaybackConstraints, PlaybackTarget, plan_playback};

#[test]
fn snapshot_real_mp4_probe_contract() {
    let probe = real_mp4_probe();
    insta::assert_json_snapshot!("real_mp4_probe_contract", probe);
}

#[test]
fn snapshot_real_mkv_probe_contract() {
    let probe = real_mkv_probe();
    insta::assert_json_snapshot!("real_mkv_probe_contract", probe);
}

#[test]
fn snapshot_real_mp4_browser_playback_plan() {
    let probe = real_mp4_probe();
    let plan = plan_playback(
        &probe,
        PlaybackConstraints {
            target: PlaybackTarget::Browser,
            ..PlaybackConstraints::default()
        },
    );
    insta::assert_json_snapshot!("real_mp4_browser_playback_plan", plan);
}

#[test]
fn snapshot_real_mkv_browser_playback_plan() {
    let probe = real_mkv_probe();
    let plan = plan_playback(
        &probe,
        PlaybackConstraints {
            target: PlaybackTarget::Browser,
            ..PlaybackConstraints::default()
        },
    );
    insta::assert_json_snapshot!("real_mkv_browser_playback_plan", plan);
}

fn real_mp4_probe() -> MediaProbe {
    serde_json::from_str(include_str!("fixtures/probes/real-mp4-renoir.probe.json")).unwrap()
}

fn real_mkv_probe() -> MediaProbe {
    serde_json::from_str(include_str!("fixtures/probes/real-mkv-gremlins.probe.json")).unwrap()
}
