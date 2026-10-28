//! Portable CLI-worker lifecycle tests using this test executable as the child.
use chroma_engine::{EngineRuntime, ResourcePolicy, WorkControl, WorkerSupervisor};
use std::{
    ffi::OsString,
    io::Write,
    path::PathBuf,
    sync::Arc,
    time::{Duration, Instant},
};

fn supervisor(runtime: Arc<EngineRuntime>) -> WorkerSupervisor {
    WorkerSupervisor::new(
        runtime,
        std::env::current_exe().unwrap(),
        Duration::from_secs(10),
    )
    .unwrap()
}

fn child_args(name: &str) -> Vec<OsString> {
    ["--exact", name, "--ignored", "--nocapture"]
        .into_iter()
        .map(OsString::from)
        .collect()
}

#[test]
fn successful_worker_returns_both_outputs_and_a_single_session_policy() {
    let runtime = EngineRuntime::new(ResourcePolicy::small_nas()).unwrap();
    let output = supervisor(runtime.clone())
        .run(&child_args("child_success"), &WorkControl::default())
        .unwrap();
    assert!(String::from_utf8_lossy(&output.stdout).contains("worker stdout"));
    assert!(String::from_utf8_lossy(&output.stderr).contains("worker stderr"));
    assert_eq!(runtime.active_sessions(), 0);
}

#[test]
fn failed_worker_returns_diagnostics_and_releases_admission() {
    let runtime = EngineRuntime::new(ResourcePolicy::small_nas()).unwrap();
    let error = supervisor(runtime.clone())
        .run(&child_args("child_failure"), &WorkControl::default())
        .unwrap_err();
    assert!(error.to_string().contains("engine worker failed"));
    assert!(error.to_string().contains("worker rejected input"));
    assert_eq!(runtime.active_sessions(), 0);
}

#[test]
fn spawn_failure_releases_admission_and_zero_timeout_is_rejected() {
    let runtime = EngineRuntime::new(ResourcePolicy::small_nas()).unwrap();
    let dir = tempfile::tempdir().unwrap();
    assert!(WorkerSupervisor::new(runtime.clone(), dir.path().into(), Duration::ZERO).is_err());
    let worker = WorkerSupervisor::new(
        runtime.clone(),
        dir.path().join("missing-worker"),
        Duration::from_secs(1),
    )
    .unwrap();
    assert!(worker.run(&[], &WorkControl::default()).is_err());
    assert_eq!(runtime.active_sessions(), 0);
}

#[test]
fn cancellation_stops_a_running_child_and_releases_admission() {
    let runtime = EngineRuntime::new(ResourcePolicy::small_nas()).unwrap();
    let worker = supervisor(runtime.clone());
    let control = WorkControl::default();
    let dir = tempfile::tempdir().unwrap();
    let marker = dir.path().join("worker-started");
    let mut args = child_args("child_wait");
    // libtest accepts an unrelated skip filter; it also carries this child's
    // unique marker path without mutating process-wide environment variables.
    args.extend([OsString::from("--skip"), marker.as_os_str().into()]);
    let cancellation = {
        let marker = marker.clone();
        let control = control.clone();
        let runtime = runtime.clone();
        std::thread::spawn(move || {
            let deadline = Instant::now() + Duration::from_secs(5);
            while !marker.is_file() && Instant::now() < deadline {
                std::thread::sleep(Duration::from_millis(5));
            }
            let started = marker.is_file();
            let admitted = runtime.active_sessions();
            control.cancel();
            (started, admitted)
        })
    };
    let error = worker.run(&args, &control).unwrap_err();
    assert_eq!(cancellation.join().unwrap(), (true, 1));
    assert!(error.to_string().contains("cancelled"));
    assert_eq!(runtime.active_sessions(), 0);
    assert!(
        worker
            .run(&child_args("child_success"), &WorkControl::default())
            .is_ok()
    );
}

#[test]
fn oversized_stdout_or_stderr_terminates_the_worker_and_releases_admission() {
    let runtime = EngineRuntime::new(ResourcePolicy::small_nas()).unwrap();
    let worker = supervisor(runtime.clone());
    for child in ["child_oversized_stdout", "child_oversized_stderr"] {
        let error = worker
            .run(&child_args(child), &WorkControl::default())
            .unwrap_err();
        assert!(error.to_string().contains("exceeded 4 MiB"), "{error}");
        assert_eq!(runtime.active_sessions(), 0);
    }
}

#[test]
#[ignore = "subprocess fixture invoked by WorkerSupervisor tests"]
fn child_success() {
    let policy: ResourcePolicy =
        serde_json::from_str(&std::env::var("CHROMA_RESOURCE_POLICY").unwrap()).unwrap();
    assert_eq!(policy.max_sessions, 1);
    assert_eq!(policy.aggregate_memory_bytes, policy.session_memory_bytes);
    std::io::stdout().write_all(b"worker stdout\n").unwrap();
    std::io::stderr().write_all(b"worker stderr\n").unwrap();
}

#[test]
#[ignore = "subprocess fixture invoked by WorkerSupervisor tests"]
fn child_failure() {
    std::io::stderr()
        .write_all(b"worker rejected input\n")
        .unwrap();
    std::process::exit(7);
}

#[test]
#[ignore = "subprocess fixture invoked by WorkerSupervisor tests"]
fn child_wait() {
    let marker = std::env::args_os()
        .skip_while(|arg| arg != "--skip")
        .nth(1)
        .map(PathBuf::from)
        .unwrap();
    std::fs::write(marker, b"started").unwrap();
    std::thread::sleep(Duration::from_secs(30));
}

#[test]
#[ignore = "subprocess fixture invoked by WorkerSupervisor tests"]
fn child_oversized_stdout() {
    std::io::stdout()
        .write_all(&vec![b'x'; 4 * 1024 * 1024 + 1])
        .unwrap();
    std::thread::sleep(Duration::from_secs(30));
}

#[test]
#[ignore = "subprocess fixture invoked by WorkerSupervisor tests"]
fn child_oversized_stderr() {
    std::io::stderr()
        .write_all(&vec![b'x'; 4 * 1024 * 1024 + 1])
        .unwrap();
    std::thread::sleep(Duration::from_secs(30));
}
