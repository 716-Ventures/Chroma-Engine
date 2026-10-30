# Resource policy and worker integration

Use one shared `Arc<EngineRuntime>` for all engine sessions in a host process.
`PlaybackSession::open_with_runtime`, `HlsVodPlan::open_with_runtime`,
`NativeFmp4TranscodeSession::open_with_runtime`, and
`probe_media_source_with_runtime` accept that runtime and a `WorkControl`.
Dropping a source session releases admission. Busy admission is a retryable
`ResourceError::Busy`; an exceeded limit needs a smaller workload or a different
validated policy, not an immediate retry loop.

Session convenience entrypoints share a process-local runtime. Use
`probe_media_source_with_runtime` to use the host's shared admission pool; the
`probe_media_source` convenience helper uses the process-global runtime. Set `CHROMA_RESOURCE_POLICY`
**before its first use**: `small-nas` selects the copy-first profile, or provide a
JSON object using the camelCase field names in `ResourcePolicy`. Missing fields
use desktop defaults. Inconsistent or zero budgets fail before media opens.

| Ceiling | Desktop default | Small NAS |
| --- | --- | --- |
| Session reservation | 1536 MiB | 224 MiB |
| Aggregate reservations | 3072 MiB | 384 MiB |
| Simultaneous sessions | 2 | 1 |
| Metadata / expanded index | 64 / 256 MiB | 24 / 96 MiB |
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
The wider index ceiling is a bounded interim accommodation for long MP4 sample
tables, not a guarantee that all large files fit. Some real files exceed the
metadata, index, or per-track sample ceilings and require a lower-memory parser.
An MP4 track above the per-track sample ceiling is rejected if selected for
packet indexing; it no longer makes unrelated video or alternate audio tracks
fail source probing. This matters for files with very large lossless-audio or
PCM tracks alongside a smaller compatible track.

The portable H.264 encoder retains its intermediate YUV allocation across frames,
and the scaler retains coordinates/output storage. These pools reduce allocator
traffic; portable decode-to-encode still crosses BGRA and is not an end-to-end
planar pipeline.

Native H.264 fragments remain aligned with source keyframes, so an individual
fragment can be longer than the requested segment target. The encoder target is
capped against every source fragment in the immutable session plan and the
existing half-output retained-payload ceiling. The calculation reserves 25% for
rate-control variability, rounds each duration upward to whole seconds, and
allows one additional burst second. The smallest resulting rate or the requested
rate, whichever is lower, applies to the entire session, including new seek
windows and software fallback. Lower requested rates are preserved. Existing
encoded-payload and final-fragment byte checks remain authoritative; no resource
ceiling is increased. For example, a 13.764-second source fragment lowers a
100 Mbps request to approximately 26.84 Mbps under the default output policy.

The current source-reader hard safety ceiling remains 64 MiB per owned read even
if a custom policy requests more. Finalized modern containers are required;
unknown-sized Matroska Segments are accepted, unknown-sized children are not.
Media is never memory-mapped. Cooperative checks occur around positional reads,
element scans and codec batches, but cannot interrupt a blocked filesystem or
native driver call.

Large multi-track MP4 probes retain essential headers first, then at most 4 MiB
of video-only STSZ/STTS statistics when the remaining metadata budget permits.
This preserves measured video bitrate and frame rate for quality decisions
without loading unrelated audio packet tables. Optional statistics yield to
essential headers under smaller budgets; missing bitrate remains explicit.

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

MP4 metadata limits apply to the owned source view, rather than the sum of every
track's packet tables on disk. When a multi-track movie exceeds that ceiling, the
engine retains bounded track headers for probing and loads only requested tracks'
original timing/offset tables for packet work. Selected tables still obey the same
metadata, parser-depth, track, sample, and index budgets. A single oversized selected
track is rejected; the limit is not raised. Optional probe fields derived from packet
tables can be absent in this header-only view. Source media is never rewritten.

## Timing, color and measurements

MP4 HEVC packet-copy initialization preserves the original visual sample entry,
including explicit HDR/Dolby metadata, within a 256 KiB entry limit. Oversized or
malformed entries fail explicitly. The output data-reference index is normalized
to its own local media data. Manifest video dimensions and maximum frame rate
come from source metadata and bounded timing-table scans; absent facts are not
invented. Hosts must keep HDR range declarations truthful and account for the
display's playback eligibility when selecting an HLS variant.

Sequential TrueHD/DTS/Opus conversion retains decoder, PCM residuals, AAC overlap,
and sample clock. CPU AAC `encode` now means **push**, not end-of-stream: call
`finish` exactly once to flush the final tail. Output may be empty while initial
lookahead fills. Fresh bounded workers use aligned codec/encoder preroll and
lookahead and publish the same global 1024-sample AAC grid as retained workers.
Preroll from later workers is discarded, so restarting a worker does not insert
another priming interval or lose the previous worker's residual audio.
AAC priming is represented in the init segment edit list; coded
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

### Network-source read coalescing

Dense forward EBML walks start with an 8 KiB look-ahead and grow up to 1 MiB;
sparse seek/cluster jumps reset to 8 KiB. The remaining metadata and compressed
window budgets cap every fill. `CHROMA_METADATA_LOOKAHEAD_BYTES` optionally limits
the adaptive ceiling to 8192–1048576 bytes; 8192 retains the small-window profile
for diagnostics or bandwidth-constrained storage. No media is memory-mapped.

Packet reads can combine gaps of at most 256 bytes (such as Matroska mux headers)
within a 64 KiB span and at most 2× selected-payload amplification. Output contains
only selected payload bytes, in decode order. The temporary span plus output
must fit the existing compressed-window budget; larger gaps, overlapping or
backward spans fall back to separate reads. Cancellation and source identity
checks remain active. `CHROMA_PROBE_DIAGNOSTICS` includes manifest positional
read bytes, operations and visited clusters on stderr without changing JSON.


Subtitle extraction validates source identity before and after each complete
pass, then uses bounded cached positional reads inside that pass. Public random
reads retain their per-call identity checks; cancellation and byte limits still
apply to internal reads.

`manifest --streaming-index` prepares Matroska HLS video seek points and audio
decoder metadata without a global audio packet index. Audio tracks explicitly
carry `timeWindowAddressed: true` and empty chunks; they are suitable for source
time-window extraction, not packet-indexed clients. The default manifest command
keeps its complete packet index. Missing initial audio samples fall back to the
complete manifest path. MP4 sample-table indexing is unchanged. This optional
representation does not invalidate completed library preparation.


### Marker evidence

Marker analysis streams standard Chromaprint algorithm-2 fingerprints into a
source/configuration-bound v2 cache (20,000 entries / 512 MiB). Visual analysis
retains only tail packet metadata, one bounded decoded GOP and numeric keyframe /
boundary observations. Limits are 100,000 retained packet references, 4,000 tail
keyframes, 60,000 decoded frames, 4,500 decode requests and 2 GiB logical source
reads per sampler. Every decoded batch stays within the selected resource policy.
Evidence checkpoints are at most 4 MiB each and validate the source around atomic
publication. Complete cached boundaries require no native decoder. Server scheduling
limits concurrent marker work to two independent seasons (one in the small-NAS
profile) and foreground playback retains cancellation/admission priority.
