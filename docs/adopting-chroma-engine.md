# Adopting Chroma Engine

Chroma Engine is a native Rust media-processing library and command-line worker. It
probes media, plans playback, extracts compressed packets, remuxes, packages HLS,
and executes supported native audio/video conversions. It is not a media server,
player, or FFmpeg command-line compatibility layer.

This guide describes the current source API. The package is version `0.1.0`, is
not published to crates.io (`publish = false`), and is licensed under the
[Apache License, Version 2.0](../LICENSE) (`Apache-2.0`). Third-party dependencies
and vendored components retain their own licenses; dependency notices are in
[THIRD_PARTY_NOTICES.md](../THIRD_PARTY_NOTICES.md).

## Philosophy

For a dated summary of CI coverage, remaining qualification work, and publication
state, see [current status](status.md). API availability alone is not a guarantee
of real-time performance or client playback correctness.

- **Modern media, deliberately bounded scope.** Focus on finalized MP4/M4V/MOV
  and MKV/WebM, with H.264, HEVC Main/Main10, and AV1 video. Supporting every
  historical format is not a goal. Container, profile, audio, and player support
  must be considered together; see the [media support contract](modern-media-support.md).
- **Copy before converting.** Preserve compatible compressed streams. Decode and
  encode only when the chosen output requires it. This reduces CPU use, startup
  work, power consumption, and quality loss, especially on NAS hardware.
- **Keep useful state alive.** Retain indexes and codec state across sequential
  work instead of reopening a movie and rebuilding a pipeline for every segment.
- **Bound work and fail explicitly.** Enforce parsing, memory-reservation, and
  concurrency limits. Unsupported media is not silently sent to FFmpeg/ffprobe.
  A predictable rejection is preferable to exhausting the host.
- **Select capabilities, not platform labels.** An OS name or detected GPU is
  not proof that a particular codec/profile can execute. Probe actual capabilities
  and qualify playback on the intended devices.
- **Keep application policy in the host.** The engine owns media processing; the
  application owns authorization, storage, delivery, user experience, and deployment.

These are design priorities, not a promise of real-time 4K transcoding on every
device. Packet copy is the primary route for underpowered systems.

## Where it fits

Good fits include a media server preparing playback for browsers and native
clients, a desktop application needing native metadata and packet access, or a
bounded background worker preparing supported VOD renditions.

It is not currently a general-purpose archival converter, live-ingest platform,
DRM system, browser/WASM SDK, or public C ABI. APIs accept filesystem paths, not
remote streaming URLs. A NAS-mounted path can be an input, but filesystem latency
and availability remain deployment concerns. Known HDR-to-SDR conversions are
rejected until verified tone mapping exists; compatible HDR copy is supported.
Bitmap subtitle burn-in is not implemented.

| Engine responsibility | Host application responsibility |
| --- | --- |
| Probe tracks, source facts, and capability hints | Authorize access and resolve trusted input paths |
| Plan supported playback and conversion paths | Determine client requirements and choose a viable plan |
| Read, decode, encode, remux, and package media | Schedule work, manage session lifetime, and apply backpressure |
| Publish output artifacts without replacing conflicts | Own cache namespaces, disk quotas, eviction, and HTTP delivery |
| Expose cancellation, resource policies, errors, and stats | Supervise workers, enforce OS limits, and collect operational telemetry |
| Produce timing and media metadata | Own playback progress, accounts, library indexing, and player UI |

## Choose an integration model

| Model | Use when | Important tradeoff |
| --- | --- | --- |
| Rust library | You need retained sessions and direct access to typed APIs | Native codecs run inside the host process; cooperative cancellation cannot stop every blocked native call |
| CLI child process | Your host uses another language, or needs process isolation | Each invocation starts fresh; use contiguous segment windows to amortize setup |
| Rust `WorkerSupervisor` | A Rust host wants the CLI isolation model with shared admission and supervision | Still a fresh process per `run`; not a persistent RPC service |

There is no built-in HTTP listener or persistent language-neutral session service.
A non-Rust application that needs long-lived engine sessions can implement its
own Rust worker service around the library; that protocol and lifecycle are host work.

## Add the engine to a project

### Rust dependency

Use a Git dependency or a local checkout. This example selects `main`; Cargo.lock
records the resolved revision. For releases, replace `branch = "main"` with
`rev = "<tested-commit-sha>"` using a tested commit that includes the Apache-2.0
license. Check the license in the selected revision before distributing it.

```toml
[dependencies]
chroma-engine = { git = "https://github.com/716-Ventures/Chroma-Engine.git", branch = "main" }
```

For local development, replace that dependency with:

```toml
[dependencies]
chroma-engine = { path = "../Chroma-Engine" }
```

Imports use `chroma_engine`; the high-level types below are re-exported from the
crate root. Source builds require Rust 1.90+, a native C/C++ toolchain, and libdav1d
1.3+ discoverable through `pkg-config`. The Rust dav1d wrapper does not eliminate
the native library dependency. See [build instructions](../README.md#build-and-run)
and [platform support](platform-support.md) for optional GPU features and packaging.

### CLI executable

From a repository checkout:

```sh
cargo build --locked --release
./target/release/chroma-engine --help
./target/release/chroma-engine probe /media/movie.mkv
./target/release/chroma-engine transcode-plan /media/movie.mkv --target apple-native
./target/release/chroma-engine cache-probe /cache/chroma
./target/release/chroma-engine transcode-fmp4-segments /media/movie.mkv /cache/chroma/window-001 --start-index 0 --count 2 --video-mode copy
```

These are POSIX shell examples; substitute your paths and use `chroma-engine.exe`
on Windows. The segment window must exist in the source. Choose `h264` instead of
`copy` only when conversion is required and supported by the source and policy.
Run `<command> --help` for its complete arguments; CLI and library defaults are
not necessarily identical, so set important output parameters explicitly.

## Public API overview

| Area | Main entrypoints | What you get |
| --- | --- | --- |
| Metadata | `probe_media_source`, `probe_media_source_with_runtime` | `MediaProbe`, including track IDs, source facts, and capabilities |
| Playback planning | `plan_playback`, `PlaybackConstraints`, `PlaybackTarget` | A `PlaybackPlan` describing the playback stages |
| HLS conversion planning | `plan_hls_transcode`, `HlsTranscodeRequest` | A `TranscodeExecutionPlan` with selected tracks, output choices, executability, and missing capabilities |
| Retained packet access | `PlaybackSession`, `PlaybackSessionOptions` | Indexed chunk access and compressed payloads; these are not automatically playable fMP4 files |
| Native playback metadata | `build_mp4_playback_manifest`, `build_matroska_playback_manifest` | Track configuration and native chunk-window manifests |
| Packet-copy packaging | `HlsVodPlan`, `write_hls_vod`, `write_hls_fmp4_vod`, `remux_mp4` | Supported HLS or MP4 outputs without a general decode/encode step |
| Retained conversion | `NativeFmp4TranscodeSession`, `NativeFmp4TranscodeOptions` | fMP4 initialization, media segments, playlists, and session stats |
| Host controls | `EngineRuntime`, `ResourcePolicy`, `WorkControl`, `WorkerSupervisor` | Admission, budgets, cancellation/deadlines, and child-process supervision |
| Capabilities | `decoder_backend_plan`, `encoder_backend_plan`, `encoder_probe`, `warmup` | Backend availability and runtime qualification information |
| Text subtitles | `try_segment_webvtt`, `try_build_webvtt_sidecars`, `write_webvtt_sidecars` | Supported text-subtitle sidecars |

`Engine::new()` is a convenience handle for opening sessions. The sessions retain
state; the handle itself is not a playback scheduler or shared resource pool.
Generate the complete API reference with `cargo doc --locked --no-deps --open`.

### Probe and plan in Rust

This complete example takes a media path as its first argument. In a server,
create the runtime once at application startup and share its `Arc` across work;
do not create an independent admission pool for every request.

```rust,no_run
use chroma_engine::{
    EngineRuntime, HlsTranscodeRequest, PlaybackTarget, ResourcePolicy,
    WorkControl, plan_hls_transcode, probe_media_source_with_runtime,
};
use std::{path::PathBuf, time::{Duration, Instant}};

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let input = PathBuf::from(std::env::args_os().nth(1).ok_or("provide a media path")?);
    let runtime = EngineRuntime::new(ResourcePolicy::small_nas())?;
    let control = WorkControl::with_deadline(Instant::now() + Duration::from_secs(30));
    let probe = probe_media_source_with_runtime(&input, runtime, control)?;
    let plan = plan_hls_transcode(&probe, HlsTranscodeRequest {
        target: PlaybackTarget::AppleNative,
        audio_track_id: None,
        segment_target_ms: 4_000,
        force_video_transcode: false,
    });
    if !plan.engine_executable {
        return Err(format!("unsupported playback plan: {:?}", plan.missing_capabilities).into());
    }
    println!("{plan:#?}");
    Ok(())
}
```

Use track IDs returned by the probe when selecting a language or alternate track;
do not treat IDs such as `a0` as identities that survive replacing the source.
The lightweight `probe_media_source` convenience function is also available, but
use the runtime-aware variant when probes must participate in shared admission.

A plan does not perform I/O delivery or execute a conversion. There is no generic
`execute(plan)` API: the host maps the selected tracks and output choices to the
appropriate session options. `engine_executable` is not a resource reservation,
a guarantee of hardware availability, or a real-time performance guarantee.
`Browser`, `AppleNative`, and `NativeChroma` are target families, not substitutes
for testing the actual client and codec/profile combination.

### Produce the first fMP4 segment in Rust

This complete example accepts an input path and a new output directory. It
demonstrates a copy-compatible video path; it is not an automatic implementation
of the preceding plan. Audio may still require conversion. Reject incompatible
inputs or select a validated H.264 path before opening the session.

```rust,no_run
use chroma_engine::{
    EngineRuntime, NativeFmp4TranscodeOptions, NativeFmp4TranscodeSession,
    NativeFmp4VideoMode, ResourcePolicy, WorkControl,
};
use std::path::PathBuf;

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let mut args = std::env::args_os().skip(1);
    let input = PathBuf::from(args.next().ok_or("provide a media path")?);
    let output = PathBuf::from(args.next().ok_or("provide a new output directory")?);
    std::fs::create_dir(&output)?;
    let runtime = EngineRuntime::new(ResourcePolicy::small_nas())?;
    let control = WorkControl::default();
    let session = NativeFmp4TranscodeSession::open_with_runtime(
        &input,
        NativeFmp4TranscodeOptions {
            video_mode: NativeFmp4VideoMode::Copy,
            segment_ms: 4_000,
            audio_bitrate: 384_000,
            ..Default::default()
        },
        control.clone(),
        runtime,
    )?;
    session.write_start(&output.join("init.mp4"), &output.join("seg-00000.m4s"), 0)?;
    session.write_media_playlist(&output.join("index.m3u8"), "init.mp4", 0, 1)?;
    println!("{:?}", session.stats());
    Ok(())
}
```

This writes only the first window, not a full-movie VOD package. Keep the session
alive and use `write_segment` or `write_segments` for subsequent windows. Generate
playlists for valid chunk ranges; the engine's planned chunks, not a guessed
`duration / 4` calculation, define the indices. A playlist window gets an end tag
only when it reaches the final planned chunk.

Playlist files also use no-replace publication. Write changed windows to new
paths, or let the host manage a separately generated dynamic playlist response;
do not repeatedly write different playlist contents to the example's `index.m3u8`.

Use `write_start` to produce initialization and first-media artifacts in one
render. Separately rendering the same first segment to obtain each artifact can
repeat work and disrupt sequential state reuse. Publication is per artifact,
not a transaction over the entire output directory: a later failure can leave
earlier complete files. Only expose a window after its required artifacts exist.

## Integrating with a server or application

1. **Initialize once.** Configure a shared runtime, cache root, resource policy,
   and backend checks. Validate cache storage with `cache-probe` or
   `validate_cache_directory` before accepting playback work.
2. **Authorize and probe.** Resolve an input under an allowed media root, probe
   with bounded work, and select tracks using returned metadata.
3. **Choose output.** Match client requirements against the plan. Prefer copy;
   reject or explain unsupported profiles rather than repeatedly attempting them.
4. **Allocate a session and cache namespace.** Include source identity, selected
   tracks, output settings, and engine build identity in the cache key. Coordinate
   duplicate requests so they do not render the same missing window concurrently.
5. **Produce and deliver.** Render the startup window, then bounded sequential
   windows. Serve playlists, initialization data, segments, and subtitle sidecars
   through the host's authenticated delivery routes. The engine does not serve HTTP.
6. **Handle seeks and cancellation.** Serialize work on each retained session.
   Discontinuous seeks reset affected codec state; abandoned work must be cancelled.
   Drop idle sessions to release admission and evict unused artifacts under host policy.

The APIs are synchronous. Construct and operate a retained session on one owning
worker thread or process; do not assume it can be shared concurrently across async
handlers. Keep blocking media work off an async executor's event-loop threads,
and bound the work queue as well as the number of running sessions.

### CLI integration from any language

Spawn the bundled executable directly with an argument array, not a shell command
constructed from filenames. Use its absolute path, a controlled environment, and
a host-owned output directory. Successful probe/plan commands emit JSON on stdout;
parse it only after successful exit. Other commands have command-specific output
contracts. Failures use a nonzero exit and stderr, not a guaranteed JSON error envelope.

Every invocation needs a timeout, cancellation that kills and reaps the child,
bounded stdout/stderr collection, and cleanup of abandoned cache files. Consume
both output streams without pipe backpressure deadlocks. Apply shared admission
before spawning; separate processes do not coordinate their budgets automatically.

Rust hosts can use the supplied supervisor instead of implementing that plumbing:

```rust,no_run
use chroma_engine::{EngineRuntime, ResourcePolicy, WorkControl, WorkerSupervisor};
use std::{ffi::OsString, path::PathBuf, time::Duration};

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let mut args = std::env::args_os().skip(1);
    let executable = PathBuf::from(args.next().ok_or("provide the engine executable path")?);
    let input = args.next().ok_or("provide a media path")?;
    let runtime = EngineRuntime::new(ResourcePolicy::small_nas())?;
    let supervisor = WorkerSupervisor::new(runtime, executable, Duration::from_secs(30))?;
    let output = supervisor.run(&[OsString::from("probe"), input], &WorkControl::default())?;
    println!("{}", std::str::from_utf8(&output.stdout)?);
    Ok(())
}
```

`WorkerSupervisor` reserves host capacity, passes a single-session policy to the
child, bounds stdout/stderr to 4 MiB each, and kills/reaps on timeout or cancellation.
Share the same runtime across supervisors and in-process work. It does not impose
an OS memory limit. A `transcode-fmp4-segments` call retains codecs within its
contiguous window, but another invocation cannot reuse that process's state.

## Resource limits, cancellation, and failures

The small-NAS policy reserves 192 MiB per session and 384 MiB aggregate, permits
two simultaneous sessions, uses one configurable software codec thread, and
disables software video fallback. These reservations are not measured RSS limits.
Opaque codec/driver allocations and OS caches need additional headroom and an
external process/container memory ceiling. See [resource policy](resource-policy.md)
for exact budgets, telemetry, and policy customization.

For CLI/default-runtime configuration, set `CHROMA_RESOURCE_POLICY=small-nas` or
a validated JSON policy before first use. Explicit runtime construction is clearer
for embedded hosts. A policy in one process is not a fleet-wide admission system.

`WorkControl::cancel()` permanently cancels all clones of that control.
`with_deadline` uses an absolute monotonic deadline; it does not reset for each
segment. Use a separate short-lived probe control and a deliberately chosen
playback-session lifetime, rather than accidentally applying a 30-second probe
deadline to a two-hour movie. Cooperative checks cannot interrupt every blocked
filesystem or native driver call; use supervised processes for hard deadlines.

Error shapes differ across APIs: `PlaybackSession` exposes `EngineSessionError`
and engine error codes, while native conversion, runtime-aware probing, and worker
supervision return `anyhow::Result`. Do not assume every failure has the same
serializable envelope or classify failures solely by matching human-readable text.

| Condition | Host response |
| --- | --- |
| `ResourceError::Busy` | Apply backpressure or a bounded delayed retry |
| Resource limit exceeded | Reduce work or choose a validated larger policy; do not retry unchanged in a loop |
| Unsupported container/profile or missing capability | Explain the limitation or choose a supported output path |
| Source changed | Invalidate its probe/cache/session and reopen after refresh |
| Cancellation or deadline | Release work; create a new control for a later attempt |
| Worker crash, backend failure, or output I/O failure | Record diagnostics, inspect host/device/storage state, and use bounded recovery |

Eligible hardware failures have at most one policy-permitted software retry inside
the engine. Do not add an unbounded host retry loop around that behavior.

## Packaging and keeping integrations current

Bundle the engine for the actual OS and CPU architecture with required native
libraries, build provenance, and third-party notices. macOS, Windows, Linux, and
Linux-based NAS are targets, but their hardware acceleration and minimum runtime
requirements differ. The supplied Linux bundle targets glibc 2.36, not every NAS
distribution. See [platform support](platform-support.md) and
[release policy](release-policy.md).

Use an explicit bundled executable path; an unrelated `chroma-engine` found on
`PATH` is not evidence of a compatible build. Record the source commit, enabled
features, and artifact checksum. The package version alone does not distinguish
all source revisions. Pin releases reproducibly, then have CI check newer upstream
commits, rebuild, run integration tests, and promote a verified update. The engine
does not automatically update a host's binary or Rust dependency.

Keep a host compatibility test for JSON schemas and command arguments. Where an
output exposes `schemaVersion`, validate it; do not assume every command has an
identical envelope. Invalidate derived caches when the engine build or output
contract changes, and retain a rollback artifact for deployments.

Before shipping:

- Test supported containers, codecs, profiles, alternate audio tracks, and subtitles
  on the actual browser/native clients, including startup and resumed playback.
- Measure cold-cache startup, distant seeks, concurrent sessions, long-run A/V sync,
  cancellation, malformed/truncated input, source replacement, and disk-full behavior.
- Verify cache hard-link/no-replace publication on the deployment filesystem. Prefer
  a compatible local cache if an SMB/NFS share cannot provide it; bound disk usage.
- Test under real CPU/memory limits on representative NAS hardware. Do not infer
  sustained throughput from a tiny synthetic fixture or an encoder capability flag.
- Check native library loading, hardware absence, permissions, signing where needed,
  and artifact provenance on every supported OS/architecture.

The [stability/performance audit ledger](audits/2026-09-stability-performance-plan.md)
tracks remaining qualification and implementation work. In particular, Linux/Windows
native-surface pipelines and end-to-end planar processing are not complete. Use
that evidence alongside the [README](../README.md) and
[native architecture](native-architecture.md) when setting product requirements.
