#[path = "support/modern_fixture.rs"]
mod fixture;
use chroma_engine::{
    EngineRuntime, NativeFmp4TranscodeOptions, NativeFmp4TranscodeSession, NativeFmp4VideoMode,
    PlaybackSession, PlaybackSessionOptions, ResourcePolicy, WorkControl,
};

#[test]
fn native_video_pipeline_encodes_generated_media() {
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("video.mkv");
    std::fs::write(&path, fixture::modern_mkv(3)).unwrap();
    let session = NativeFmp4TranscodeSession::open(
        &path,
        NativeFmp4TranscodeOptions {
            segment_ms: 1000,
            video_mode: NativeFmp4VideoMode::H264,
            ..Default::default()
        },
    )
    .unwrap();
    let outputs = session
        .write_segments(&directory.path().join("output"), 0, 3)
        .unwrap();
    assert_eq!(
        outputs
            .iter()
            .map(|segment| segment.encoded_video_frames)
            .sum::<usize>(),
        6
    );
    assert_eq!(session.stats().video_decoder_sessions_created, 1);
    assert_eq!(session.stats().codec_session_reuses, 2);
}

#[test]
fn retained_native_copy_audio_seek_cancel_and_admission() {
    let directory = tempfile::tempdir().unwrap();
    let source = directory.path().join("synthetic.mkv");
    std::fs::write(&source, fixture::modern_mkv(6)).unwrap();
    let runtime = EngineRuntime::new(ResourcePolicy::small_nas()).unwrap();
    let options = NativeFmp4TranscodeOptions {
        segment_ms: 1000,
        video_mode: NativeFmp4VideoMode::Copy,
        ..Default::default()
    };
    let control = WorkControl::default();
    let session = NativeFmp4TranscodeSession::open_with_runtime(
        &source,
        options,
        control.clone(),
        runtime.clone(),
    )
    .unwrap();
    assert_eq!(runtime.active_sessions(), 1);
    assert_eq!(session.plan().chunks.chunks.len(), 6);
    let outputs = session
        .write_segments(&directory.path().join("sequential"), 0, 6)
        .unwrap();
    assert_eq!(outputs.len(), 6);
    let seek = session
        .write_segment(&directory.path().join("seek.m4s"), 4)
        .unwrap();
    assert_eq!(seek.first_video_pts_ms, Some(4000));
    assert!(seek.first_audio_pts_ms.unwrap() >= 4000);
    let before = session.stats().segment_requests;
    assert!(
        session
            .write_segments(directory.path(), u32::MAX, u32::MAX)
            .is_err()
    );
    assert_eq!(session.stats().segment_requests, before);
    control.cancel();
    assert!(
        session
            .write_segment(&directory.path().join("cancelled.m4s"), 5)
            .is_err()
    );
    assert!(!directory.path().join("cancelled.m4s").exists());
    drop(session);
    assert_eq!(runtime.active_sessions(), 0);
    for _ in 0..20 {
        let session = PlaybackSession::open_with_runtime(
            &source,
            PlaybackSessionOptions::default(),
            WorkControl::default(),
            runtime.clone(),
        )
        .unwrap();
        assert!(!session.segment(0).unwrap().1.is_empty());
        drop(session);
        assert_eq!(runtime.active_sessions(), 0);
    }
}

// Isolate source mutation checks: a SIGBUS/abort is a failing child exit, not
// something the parent test suite can accidentally mistake for a Rust error.
#[test]
fn mutation_subprocess_returns_errors_without_signals() {
    if std::env::var_os("CHROMA_MUTATION_TEST_CHILD").is_some() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("source.mkv");
        for replacement in [false, true] {
            std::fs::write(&path, fixture::modern_mkv(2)).unwrap();
            let session = PlaybackSession::open(&path, PlaybackSessionOptions::default()).unwrap();
            if replacement {
                std::fs::remove_file(&path).unwrap();
                std::fs::write(&path, b"replacement").unwrap();
            } else {
                std::fs::OpenOptions::new()
                    .write(true)
                    .open(&path)
                    .unwrap()
                    .set_len(1)
                    .unwrap();
            }
            assert!(session.segment(0).is_err());
        }
        return;
    }
    let status = std::process::Command::new(std::env::current_exe().unwrap())
        .args([
            "--exact",
            "mutation_subprocess_returns_errors_without_signals",
            "--nocapture",
        ])
        .env("CHROMA_MUTATION_TEST_CHILD", "1")
        .status()
        .unwrap();
    assert!(status.success(), "mutation worker failed: {status}");
}

#[test]
#[ignore = "fixture child invoked only by the supervisor deadline regression"]
fn supervised_sleep_child() {
    std::thread::sleep(std::time::Duration::from_secs(5));
}

#[test]
fn worker_deadline_kills_reaps_and_releases_admission() {
    let runtime = EngineRuntime::new(ResourcePolicy::small_nas()).unwrap();
    let supervisor = chroma_engine::WorkerSupervisor::new(
        runtime.clone(),
        std::env::current_exe().unwrap(),
        std::time::Duration::from_millis(50),
    )
    .unwrap();
    let error = supervisor
        .run(
            &[
                "--exact".into(),
                "supervised_sleep_child".into(),
                "--ignored".into(),
            ],
            &WorkControl::default(),
        )
        .unwrap_err();
    assert!(error.to_string().contains("hard deadline"));
    assert_eq!(runtime.active_sessions(), 0);
}
