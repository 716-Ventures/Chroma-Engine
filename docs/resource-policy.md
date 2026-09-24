# Resource policy and worker integration

Use one shared `Arc<EngineRuntime>` for all engine sessions in a host process.
`PlaybackSession::open_with_runtime`, `HlsVodPlan::open_with_runtime`,
`NativeFmp4TranscodeSession::open_with_runtime`, and
`probe_media_source_with_runtime` accept that runtime and a `WorkControl`.
Dropping a source session releases admission. Busy admission is a retryable
`ResourceError::Busy`; an exceeded limit needs a smaller workload or a different
validated policy, not an immediate retry loop.

Session convenience entrypoints share a process-local runtime. Use
`probe_media_source_with_runtime` for admission-controlled probing; the lightweight
`probe_media_source` helper is not a shared admission reservation. Set `CHROMA_RESOURCE_POLICY`
**before its first use**: `small-nas` selects the copy-first profile, or provide a
JSON object using the camelCase field names in `ResourcePolicy`. Missing fields
use desktop defaults. Inconsistent or zero budgets fail before media opens.

| Ceiling | Desktop default | Small NAS |
| --- | --- | --- |
| Session reservation | 1536 MiB | 192 MiB |
| Aggregate reservations | 3072 MiB | 384 MiB |
| Simultaneous sessions | 2 | 2 |
| Metadata / expanded index | 64 / 256 MiB | 24 / 64 MiB |
| Compressed window / decoded batch | 64 / 64 MiB | 32 / 8 MiB |
| Output fragment | 128 MiB | 64 MiB |
| Configurable software codec threads | 2 | 1 |
| Software video fallback | Allowed | Disabled |

Reservations are not eager allocations or a measured RSS guarantee. Video
admission includes a conservative reference-frame working-set estimate; it can
reject a resolution below the absolute 8K pixel ceiling. Opaque codec/driver
allocations, executable pages, and OS cache require an **external process or
container memory ceiling**, with headroom. The small-NAS CI workload runs under
512 MiB; it is a tiny synthetic fixture, not 4K movie qualification.

The portable H.264 encoder retains its intermediate YUV allocation across frames,
and the scaler retains coordinates/output storage. These pools reduce allocator
traffic; portable decode-to-encode still crosses BGRA and is not an end-to-end
planar pipeline.

The current source-reader hard safety ceiling remains 64 MiB per owned read even
if a custom policy requests more. Finalized modern containers are required;
unknown-sized Matroska Segments are accepted, unknown-sized children are not.
Media is never memory-mapped. Cooperative checks occur around positional reads,
element scans and codec batches, but cannot interrupt a blocked filesystem or
native driver call.

## CLI worker supervision

`WorkerSupervisor` supplies a host-side reservation, mandatory wall-clock
deadline, kill/reap cleanup, and bounded stdout/stderr for CLI workers. Share the
same runtime across supervisors. The child receives a single-session version of
that policy. This is an integration API: existing servers do **not** use it
automatically, and independently created host pools cannot coordinate each other.
Separate server processes need an external admission coordinator.

```rust
use chroma_engine::{EngineRuntime, ResourcePolicy, WorkerSupervisor, WorkControl};
use std::{path::PathBuf, time::Duration};

let runtime = EngineRuntime::new(ResourcePolicy::small_nas())?;
let supervisor = WorkerSupervisor::new(
    runtime, PathBuf::from("/opt/chroma/chroma-engine"), Duration::from_secs(30),
)?;
let output = supervisor.run(
    &["probe".into(), "/media/movie.mkv".into()], &WorkControl::default(),
)?;
Ok::<(), anyhow::Error>(())
```

For in-process sessions, cancel permanently through `WorkControl::cancel()` or
set a monotonic deadline. After a failed render, codec state is poisoned and
recreated before reuse. Eligible hardware failures get at most one software
fallback attempt; policy-disabled software does not bypass admission. No partial
fragment is published. A conflicting existing output remains unchanged.

Run `cache-probe` when choosing storage. Native transcode write APIs also check
each destination directory before rendering and cache that result for the
session. Hard-link/no-replace support is required; use local cache storage when
SMB/NFS/appliance shares do not supply it.

## Timing, color and measurements

Sequential TrueHD/DTS/Opus conversion retains decoder, PCM residuals, AAC overlap,
and sample clock. CPU AAC `encode` now means **push**, not end-of-stream: call
`finish` exactly once to flush the final tail. Output may be empty while initial
lookahead fills. AAC priming is represented in the init segment edit list; coded
sample durations are never shortened to disguise delay.

Known HDR-to-SDR transcodes fail explicitly and are marked non-executable by the
planner until verified tone mapping is supplied. Compatible compressed HDR copy
remains the preferred route. Dolby Vision profiles and actual player behavior
still need fixture/device qualification.

On macOS, matching VideoToolbox decode/encode uses retained Core Video buffers
without CPU pixel readback/upload. Other backends retain the checked BGRA
boundary; the retained CPU scaler reuses destination storage and coordinate
tables. Linux/Windows surface pipelines and planar software codecs still need
profile-led implementation and real-device qualification.

Session stats report selected backends, open/read/video/audio/mux/publication
microseconds, positional read bytes/operations, and local clusters visited.
Read counters exclude the initial metadata reader, include repeated header
reads, and are not physical disk-byte counters.

`scripts/benchmark-session.py` records fresh-process windows and p50/p95/p99.
Install `psutil==7.2.2` for optional 20 ms worker RSS/CPU/I/O sampling and pass
`--metrics-required` in qualification jobs. Sampling misses short peaks; compare
OS high-water/container telemetry too. POSIX runs also record per-child
`wait4` high-water RSS and CPU usage, covering workers that finish between samples.
`--report PATH` creates a new JSON report.
Five samples, tiny frames, and uncontrolled caches cannot establish production
latency or a performance improvement over the reviewed revision.
