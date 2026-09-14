# Chroma Engine stability and performance review

Date: 2026-09-13  
Reviewed revision: `e8a3e3700c9609f08f96fa3ef1956cd24f16b543`  
Scope: Chroma Engine, including retained sessions, parsers, codecs, platform adapters, output publication, packaging, tests, and benchmarks. Server and client changes below are integration requirements, not reviewed implementations.

## Assessment

The engine has useful foundations: copy-first planning, retained video sessions, borrowed compressed packet inputs, checked plane geometry, immutable output publication, portable codec fallbacks, and a multi-platform CI matrix. These are worth preserving.

It is not yet ready for a claim of stable, predictable operation across low-memory NAS hardware. The largest remaining problems are source-mapping safety, incomplete resource limits, unnecessary Matroska audio scans, audio state discontinuities, and incomplete failure recovery. The current hardware paths also spend considerable work moving decoded pixels through CPU BGRA buffers.

Keep the modern-media scope. Broad hardware coverage means reliably probing and copying supported media on small machines, using hardware acceleration when available, and admitting only sustainable transcodes. It does not mean promising software 4K HEVC/AV1 transcoding on every NAS. Keep x86-64 and ARM64 as the initial architecture contract; 32-bit NAS support is a separate product decision.

Implementation was authorized after this review. The findings below describe the reviewed revision; this ledger distinguishes subsequent changes from work still open.

## Execution ledger

Implementation continued beyond the initial partial batch. The original findings below remain historical evidence, not a description of the current code. **The complete fleet/performance plan is not closed.** Server/client code has not been deployed or modified by this implementation.

| Finding | Implemented and checked locally | Remaining acceptance work |
| --- | --- | --- |
| CE-SP-01 | Removed source mmap and memmap2 entirely, including HLS and CLI consumers. Owned bounded metadata, positional payloads, retained-handle/path identity checks, truncation/replacement subprocess regressions. | SMB/NFS read-failure and concurrent-mutation appliance qualification. |
| CE-SP-02 | Shared metadata preflight, cumulative expanded-index accounting, depth/track/element limits, bounded Matroska indexes, configurable source budgets. Fuzzing additionally found and fixed subtitle timestamp overflow; subtitle work/output expansion is now bounded with checked APIs. | Complete dependency/opaque-driver allocation accounting and larger adversarial corpus qualification. |
| CE-SP-03 | Fallback uses source dimensions; only actual hardware decode scaling reduces them. Explicit software thread ceilings and AV1 fallback regression. | Scaled 4K codec matrix on real Linux/Windows devices. |
| CE-SP-04 | Cached track-independent Matroska index shared with HLS; video/audio windows extracted in one local scan with independent preroll; adjacent physical packet reads coalesced; read/cluster counters; seek-reference tests assert one local cluster at beginning/middle/end. | Large network-storage startup/seek measurements; initial cue-less index still requires one bounded header pass. |
| CE-SP-05 | Vendored, attributed streaming AAC extension; retained TrueHD/DTS/Opus decoders, PCM residuals and sample clock; EOS-only flush and edit-list priming. Uneven PCM pushes tested at 44.1/48 kHz, stereo/six channels; retained DTS and Opus segments match continuous encoding. | Independent audio-quality/priming/player validation and long-form TrueHD/DTS/Opus drift qualification. |
| CE-SP-06 | Poison/rebuild state covers audio and video; one policy-eligible runtime hardware fallback; failed recovery closes video rendering until reopen; retry/range/lifecycle tests. | Real driver-loss injection and publication-retry behavior on all target players/devices. |
| CE-SP-07 | ResourcePolicy and shared EngineRuntime admission, small-NAS profile, byte-bounded decode/compressed/output work, CPU-thread ceilings, parser/audio/video cancellation. WorkerSupervisor supplies host-side CLI admission plus hard kill/reap deadlines; watchdog and admission-release tests. | Wire the supervisor/shared runtime into GenusServer; enforce OS/container limits and coordinate separate server processes. Reservations are not a hard RSS guarantee. |
| CE-SP-08 | Stage/back-end/I/O counters; retained CPU scaler coordinates/storage; streamed fragment publication; retained Core Video surfaces for matching VideoToolbox decode/encode; explicit HDR tone-map rejection reflected in planner/probe. | Linux/Windows native-surface and portable planar optimization remain implementation work, gated on representative stage profiles and correctness/device tests. Measure TrueHD build/runtime optimization variants. |
| CE-SP-09 | Present malformed timing/sync tables fail rather than default; strict range/sample-count checks also cover fragment validation. | Broader malformed real-media corpus. |
| CE-SP-10 | Bounded existing-output comparison, early cached filesystem capability checks, Debian 12/glibc 2.36 baseline-specific Docker/CI artifact with no-graphics and 512 MiB worker tests, AAC license distribution. | Execute the new Linux container jobs; this Mac has no running Docker daemon. Actual appliance/CPU/libc/driver qualification remains required. |
| CE-SP-11 | Four 60-second local fuzz campaigns (subtitle crash fixed and rerun); full ARM64 CI suites, generated redistributable H.264/Opus smoke fixture, debug/release lifecycle and mutation tests, portable JSON benchmark with optional worker RSS/CPU/I/O sampling, CI structured fuzz seeds. | Target-device corpus, independent frame/sample validation, 24/72-hour soak/fault/concurrency runs, measured release thresholds. |

See [resource policy and host integration](../resource-policy.md) for APIs, limits, compatibility changes, and what the host must enforce. AAC streaming calls can return no frames while lookahead fills and require a final `finish()`; this is an intentional behavioral change.

Local evidence: container fuzzing completed 2,089,608 iterations, the initial fMP4 campaign 2,132,207; the later campaign additionally validates arbitrary input fragments. Codec fuzzing passed. Subtitle fuzzing exposed an arithmetic-overflow panic in timestamp parsing; the reduced regression passes and a subsequent 60-second campaign passed. All use a 512 MiB fuzzer ceiling. These bounded campaigns do not certify codec libraries or fleet stability.

The final codec campaign additionally exercises the HEVC access-unit classifier:
1,742,656 runs in 61 seconds, no crash, 132 MiB reported peak fuzzer RSS. Portable
H.264 now reuses its intermediate YUV allocation; the CPU encoder regression
checks plane-address reuse across three batches and a forced sync frame after
an intervening predictive frame. End-to-end planar conversion remains open.

Recorded synthetic measurements: [copy/AAC](2026-09-macos-synthetic-copy.json) and [VideoToolbox H.264/AAC](2026-09-macos-synthetic-h264.json). Five fresh-process runs per start index on a six-second, 32×32 original fixture gave copy p50 around 31–32 ms and H.264 p50 around 186–207 ms. Reports include exact binary hash and sampled worker metrics. Caches were uncontrolled; these are tool/lifecycle smoke measurements, **not movie performance, NAS qualification, or a before/after speedup**.

To close phases 4–5 requires representative Linux/Windows/NAS hardware and media, a usable container runtime, GenusServer integration, and independent player/long-run measurements. Do not mark the remaining implementation or qualification as complete merely because local checks pass.

Local verification after the additional Matroska random-access fixes: 262 unit tests and 18 integration/property/snapshot tests in debug and release; nine Criterion benchmark smoke cases; strict all-target/all-feature Clippy; Rust 1.90 compatibility check; warnings-as-errors documentation build; formatting and script syntax checks. The ignored test is an intentionally sleeping child fixture executed by the watchdog test, not skipped coverage.

Actual-file copy measurements: [media fixture 01](2026-09-media-fixture-01-small-nas.json) and [media fixture 02](2026-09-media-fixture-02-small-nas.json) passed all 18 fresh-process beginning/middle/late-window runs under the revised small-NAS profile. Median window generation was 34–65 ms, with maximum observed worker RSS about 73/93 MiB respectively. These are macOS engine generation measurements with uncontrolled caches, not tvOS startup or NAS-hardware results. Real segments exceeded the original 16 MiB compressed-window limit; the profile now reserves 192 MiB/session with 32 MiB compressed windows and 64 MiB output ceilings. MP4 timing normalization no longer allocates a raw i128 tuple array; accounting charges all retained track indexes but only the largest sequential scratch expansion. Actual probing also exposed incorrect hvcC chroma/depth offsets, now corrected with regressions.

The [Matroska copy report](2026-09-matroska-small-nas.json) passed nine beginning/middle/end tests. Follow-up hardware transcodes exposed a distinct open-GOP random-access failure. Video packet windows now partition decode-order GOPs instead of selecting by PTS, and fresh HEVC decode skips initial RASL access units that can reference unavailable pre-seek pictures. Subsequent continuous GOPs retain those pictures. This follows [H.265 random-access semantics](https://www.itu.int/epublications/publication/itu-t-h-265-1-2018-10-conformance-specification-for-itu-t-h-265-high-efficiency-video-coding). A reordered-picture regression additionally found and fixed eager timestamp-subtraction overflow in nominal frame-duration inference. These changes require independent player/timing validation, not just decoder success.

The [post-fix hardware-transcode report](2026-09-matroska-h264-verified.json) passes beginning/middle/end seeks with two consecutive fragments each, retaining VideoToolbox decoder/encoder sessions. A sync frame is explicitly requested for the first encoded presentation frame of each fragment; relying on the source picture's keyframe flag failed after reordering. Observed worker peaks were 82–103 MiB. These runs overlapped local builds, so use them as correctness evidence, not a throughput baseline. The earlier `matroska-h264`, `-gop-fix`, and `-random-access` reports deliberately preserve the failing progression for comparison.

Follow-up [copy high-water report](2026-09-macos-copy-highwater.json) and [H.264 high-water report](2026-09-macos-h264-highwater.json) add per-child POSIX RSS high-water/CPU accounting. They measured about 10 MiB and 29 MiB peak RSS respectively on this tiny fixture. This corrects the sampling blind spot for short-lived workers; do not interpret sub-megabyte sampled values in the earlier copy report as actual peaks.

## Evidence and limits

- `cargo test --locked --all-features` passed locally: 238 unit tests and 14 integration/property/snapshot tests; no failures. The doc-test target contains no tests.
- Two temporary targeted reproduction tests also passed after correcting the harness imports: the existing 32×32 AV1 fixture fails with `dav1d decoded 32x32, expected 16x16` when requested at reduced dimensions; two CPU AAC calls each taking 192,000 samples per channel at 48 kHz emit 193,536 coded samples each. The latter is 32 ms excess per four-second call, confirming that codec delay/padding needs explicit handling. This does not establish the audible effect in a player. The temporary harness was removed after recording the results.
- Source review covered macOS, Windows, VA-API, and NVIDIA boundaries; Linux/Windows hardware execution was not performed on this Mac.
- The existing Criterion suite exercises small synthetic probe/index/subtitle workloads. It does not establish real movie startup, sustained transcode throughput, or a NAS memory ceiling.
- Findings explicitly distinguish demonstrated code paths from performance hypotheses. Memory arithmetic below describes buffer sizes, not measured resident memory. Acceptance thresholds are proposed targets, not achieved results.
- Historical audit summaries saying parser limits were enforced and source safety was remediated are too broad: the current code only partially implements those protections.

## Findings

### CE-SP-01 — P0: portable source mappings remain unsafe under external mutation

Evidence: `src/source.rs:104`, `src/source.rs:155`, `src/source.rs:228`, `src/source.rs:250`.

When a private macOS clone is unavailable, the source maps a retained handle to the original file. On Linux and Windows, `clone_file` always returns false. `validate_current` checks pathname metadata separately from later memory access; it cannot prevent an in-place write or truncation between validation and use. The safe API returns borrowed slices over that mapping. The installed memmap2 source explicitly warns that subsequent in-process or external file modification can cause undefined behavior. A retained read-only handle does not enforce immutability.

Impact: a download, media replacement process, or administrator modifying a playing file can crash a worker or an embedding server; before/after metadata checks do not close the race.

Action: introduce bounded positional reads with owned buffers as the portable default. Preserve mapping only for a genuinely private, immutable snapshot under an explicit invariant. Check source identity around reads and publication; return a typed source-change/read error. Add subprocess tests for truncation, in-place mutation, replacement, and network read failure. Do not reintroduce whole-file heap copies.

### CE-SP-02 — P0: parser limits do not bound total work, memory, or recursion

Evidence: `src/container/mod.rs:5`, `src/container/mp4.rs:748`, `src/container/matroska.rs:693`, `src/container/matroska/ebml.rs:8`.

`max_tracks` and `max_boxes` are declared but never enforced. Matroska does not use `ParseLimits`; nested ChapterAtom elements recurse without a depth ceiling. Its strings, codec-private data, tracks, cues, and packet vectors have no shared allocation budget. MP4 limits individual tables/counts, but `max_index_bytes` is not an aggregate accounting mechanism for all expanded tables and packet indexes. Its limits are hard-coded internally and unavailable to a host resource policy.

Impact: malformed or unusually large media can exhaust stack, memory, or CPU. Small per-table limits do not protect a multi-track file or concurrent sessions. Release uses `panic = "abort"`, so a parser panic is process-fatal.

Action: pass one parser budget through both containers, tracking cumulative bytes, samples, elements, tracks, nesting depth, and codec-private/string lengths. Use fallible reservations and checked arithmetic before allocations. Prefer iterative chapter traversal. Return explicit malformed-input versus resource-limit errors instead of partial success. Add structured adversarial fixtures and resource-constrained subprocess tests, including many individually valid tables.

### CE-SP-03 — P1: macOS decode scaling breaks portable fallback

Evidence: `src/transcode/segment.rs:554`, `src/transcode/segment.rs:1342`, `src/transcode/video_decode.rs:672`, `src/transcode/video_decode.rs:918`, `src/transcode/video_decode.rs:975`, `src/transcode/video_decode.rs:1190`.

`native_decoder_output_format` chooses the reduced encode dimensions for every macOS session before the backend is selected. If VideoToolbox cannot open, `BgraDecoderSession::new` passes those dimensions to OpenH264, rust_h265, or dav1d. Those decoders produce source-sized pictures and explicitly reject a dimension mismatch. Therefore a source requiring downscaling fails when software fallback is selected. AV1 is particularly relevant because VideoToolbox availability varies by hardware and codec configuration.

Action: negotiate decoded output format after selecting the backend. Only a backend that actually supports decode scaling receives reduced output dimensions; software paths retain source dimensions and scale afterward. Make backend choice injectable for tests. Exercise 4K-to-1080p H.264, HEVC Main10, and AV1 with hardware disabled, unavailable, and initialization failure.

### CE-SP-04 — P1: Matroska audio windows can rescan the movie from the beginning

Evidence: `src/transcode/segment.rs:795`, `src/transcode/segment.rs:836`, `src/container/matroska.rs:570`, `src/container/matroska.rs:931`.

The retained transcode index stores only `PreparedSourceIndex::Matroska`, without retained cue positions or a cluster cursor. Each segment parses video and audio windows separately. Cue lookup requires the requested track number; an audio request without audio cues falls back to offset zero even when video cues could locate the nearby interleaved clusters. The parser then walks prior clusters and blocks to reach the requested audio time. Cue lookup itself reparses and linearly searches the cue list per request.

Impact: late resumes incur avoidable reads and CPU work; sequential playback repeats prefix scans. This is a code-level explanation for potential slow resumes, not a measured attribution of the earlier media fixture 02 playback incident.

Action: retain a sorted cluster/cue index shared by tracks, use binary search for seeks, and retain a forward cursor for sequential playback. Use a safe earlier cluster anchor for audio preroll even when only video cues exist, accounting for interleaving and codec seek requirements. For cue-less files, build a bounded sparse index once. Extract selected audio/video windows in one pass where practical. Reuse parsed MP4 tables as well; MP4 preparation currently parses the same track through several helpers.

Acceptance: beginning, middle, and end seeks on video-only-cued and cue-less MKV fixtures produce identical packets to a reference scan; indexed seek scan work is bounded by local clusters/preroll rather than elapsed movie length. Record bytes read and clusters visited, not only wall time.

### CE-SP-05 — P1: retained video does not imply retained audio codec state

Evidence: `src/transcode/segment.rs:1440`, `src/transcode/segment.rs:1515`, `src/transcode/audio_decode.rs:196`, `src/transcode/audio_decode.rs:355`, `src/transcode/audio_decode.rs:496`, `src/transcode/audio_encode.rs:402`, `src/transcode/audio_encode.rs:472`.

The native segment path calls one-shot audio decoder and AAC encoder helpers each segment. In addition, `CpuAacEncoderSession` retains a clock but constructs and finishes a new `rusty_aac::AacEncoder` on every encode call. Partial AAC frames are therefore finalized at intermediate boundaries. The segment path rebases each independently encoded run to its source start without carrying residual PCM or encoder delay into the next run.

Impact: unnecessary setup/preroll and PCM copies; padding and startup-delay handling can introduce gaps or overlaps at adjacent segment boundaries. Audible behavior and long-run sync need output decoding tests, not just frame-count assertions.

Action: retain audio decoder, encoder, sample clock, and residual PCM across sequential segments. Flush only at actual end of stream; explicitly reset and preroll on discontinuous seeks. Track valid samples, codec delay, and padding separately from coded frame duration. Ensure adjacent segments partition the audio timeline without duplicated or missing content. Validate at 44.1 and 48 kHz, stereo and six-channel, with non-AAC-frame-aligned video segment lengths.

Minimal reproduction for the measured CPU AAC behavior (using the public crate exports):

```rust
use chroma_engine::{CpuAacEncoderSession, PcmAudioFormat};
let mut encoder = CpuAacEncoderSession::new(
    PcmAudioFormat { channels: 2, sample_rate: 48_000 }, 128_000,
).unwrap();
for _ in 0..2 {
    let output = encoder.encode(&vec![0_i16; 192_000 * 2]).unwrap();
    let samples: u64 = output.frames.iter()
        .map(|frame| u64::from(frame.timing.sample_count)).sum();
    assert_eq!(samples, 193_536); // observed at the reviewed revision
}
```

### CE-SP-06 — P1: failures can leave a retained video session contaminated

Evidence: `src/transcode/segment.rs:263`, `src/transcode/segment.rs:573`, `src/transcode/video_decode.rs:672`, `src/transcode/video_encode.rs:257`.

A render can advance decode/encode state and then fail, including during later audio work. Only success changes `last_completed_index`; a retry of the same next segment can therefore satisfy the sequential-reuse condition and feed packets into partially advanced state. If the first render fails, the `None` state also permits reuse. Backend fallback handles constructor errors, but runtime decode/encode failures are simply returned; the retained session has no explicit poisoned/recovery state.

Action: model ready, rendering, completed, poisoned, and closed states. Mark codec state dirty before feeding packets; rebuild it after a failed render before accepting another request. Distinguish a publication failure from a codec failure and define retry semantics for each. Retry a failed hardware backend at most once from a valid keyframe on an eligible alternate backend, subject to the resource policy; never mix partially emitted output. Test injected failures after decode, encode, audio, and publication. Fix `stats()`'s assertion that decoder and encoder batch counts always match: decode batches with no output legitimately skip encoding.

### CE-SP-07 — P1: no enforceable per-session or process resource policy

Evidence: `src/engine.rs:27`, `src/transcode/segment.rs:49`, `src/transcode/segment.rs:331`, `src/transcode/segment.rs:1257`, `src/transcode/video_decode.rs:232`, `src/transcode/video_decode.rs:1439`.

The engine handle is stateless; transcode options expose no memory, thread, deadline, or cancellation budget. A fixed batch of 16 packets is not a byte budget: sixteen 3840×2160 BGRA frames alone occupy about 506 MiB, before decoder surfaces, compressed payloads, scaled frames, and encoder state. Sixteen 1920×1080 scaled BGRA frames add about 127 MiB when scaling allocates. Decoder format validation checks nonzero dimensions, not a maximum pixel count. `write_segments` reserves its caller-supplied count before validating the complete planned range. dav1d gets a frame-delay setting but no host-selected thread budget.

Action: add a validated resource policy covering aggregate engine/session memory, frame pixels, decoded bytes in flight, compressed window bytes, output bytes, codec threads, and session admission. Validate segment ranges before reserving. Use byte-based backpressure and small reusable frame pools. Add cooperative cancellation/deadlines to parser and codec loops. Native driver calls that cannot be interrupted require a host worker watchdog/process boundary, not a promise that a cancellation flag can interrupt them.

Expose admission estimates and measured backend/throughput to the server. A CPU fallback must not silently convert a sustainable hardware workload into an unsustainable software one. The server owns cross-process admission if it launches multiple CLI workers; an engine-local limiter alone cannot control those workers.

### CE-SP-08 — P2: hardware pipelines repeatedly cross the CPU pixel boundary

Evidence: `src/transcode/video_decode/windows_decode.rs:215`, `src/transcode/video_decode/vaapi_decode.rs:443`, `src/transcode/video_encode/vaapi_encode.rs:138`, `src/transcode/yuv.rs:20`, `src/transcode/scaler.rs:29`, `src/fmp4/mod.rs:110`.

Windows reads decoded textures back to CPU memory; Linux paths expose/read decoded planes and convert to BGRA. Encoding converts BGRA back into NV12 and allocates/uploads surfaces. Scaling operates in CPU BGRA on portable paths, with floating-point bilinear work for non-half-size ratios. Encoded payload assembly and final fragment construction also copy buffers.

Action: profile stage time and bytes copied first. Introduce an owned frame abstraction for CPU YUV planes and platform surfaces, preserving stride, pixel depth, color metadata, and lifetime. Keep same-device decode/scale/encode on native surfaces where supported; pool surfaces and CPU buffers. Prefer planar YUV through software decode/scale/encode as well. Retain a checked BGRA adapter for callers that need it. Stream fragment payloads through the atomic output writer instead of building avoidable whole-fragment copies.

Verify output color and timing while changing this boundary. Current portable YUV conversion uses a fixed limited-range matrix and reduces higher bit depths to 8-bit; it is not an HDR-to-SDR tone-mapping implementation. Make HDR transcode behavior explicit rather than treating bit truncation as conversion. Preserve compatible compressed HDR video through copy paths.

### CE-SP-09 — P1: malformed MP4 timing tables can silently become valid-looking defaults

Evidence: `src/container/mp4.rs:748`.

`parse_sample_table_with_limits` treats a missing table and a present-but-invalid table identically. Failed `stts`/`ctts` parsing substitutes zero durations/offsets; failed `stss` parsing marks every sample as a keyframe. Defaults appropriate for an absent optional table should not conceal rejected table contents.

Impact: corrupted timing and false random-access points can reach chunk planning and playback instead of failing at parse time.

Action: distinguish absence, valid content, malformed content, and resource-limit rejection. Require valid mandatory timing, and allow specification-defined defaults only for absent optional boxes. Test malformed sync tables at the enclosing track/session level, not just by calling the leaf parser.

### CE-SP-10 — P2: publication and packaging need a concrete NAS compatibility contract

Evidence: `src/output.rs:35`, `src/output.rs:106`, `src/output.rs:141`, `packaging/Dockerfile.linux:6`, `.github/workflows/rust.yml:123`, `docs/platform-support.md`.

Output publication always uses hard links, with no supported alternative for a destination that does not provide that operation. Existing-output comparison reads the whole matching-length file into memory. The Linux Docker build uses a Bookworm base and different optimization settings from the hosted release build; the hosted Linux build uses its runner's userspace. Bundling dav1d alone does not establish compatibility with older NAS userspaces. NVIDIA features also differ between the x64 hosted bundle and the Docker/ARM64 paths.

Action: choose and document minimum OS/libc/CPU baselines and the cache-filesystem requirements per artifact. Test the actual bundle on those baselines, with missing graphics libraries and inaccessible devices. Either require a compatible local cache with an early capability check or implement a verified no-replace atomic publication primitive for supported destinations. Compare existing output in bounded chunks. Benchmark release optimization variants, including the unoptimized TrueHD override, before retaining claims about performance; do not remove the build-memory workaround without measuring builder memory too.

### CE-SP-11 — P2: tests and release gates do not yet qualify stability or performance

Evidence: `benches/engine_hot_paths.rs:11`, `scripts/benchmark-native-transcode.sh:6`, `scripts/smoke-native-transcode-session.sh:6`, `.github/workflows/rust.yml:90`, `.github/workflows/rust.yml:268`.

Fuzz jobs run each target once. Native-session smoke and benchmark scripts default to a developer's mounted movie and the benchmark uses macOS-specific timing flags. CI does not invoke those end-to-end scripts. ARM64 explicitly tests the CPU H.264 subset, not the whole codec/transcode suite. Hosted compile success does not verify GPU paths or actual player output.

Action: build a redistributable fixture corpus and portable benchmark runner; run meaningful bounded fuzz campaigns with structured seeds. Qualify CPU and real GPU devices, multi-hour output, random seeks, repeated open/close, cancellation, disk-full/read failure, and concurrent admission. Report frame/sample continuity, first-playable latency, p50/p95/p99 segment time, RSS peak/slope, CPU, bytes read/copied, and selected backend. Add regression thresholds only after measuring stable baselines.

## Execution plan

Implement one coherent concern per change. Each phase ends with recorded evidence and a rollback point; avoid a single rewrite of parsers, surfaces, and session state.

| Phase | Work | Exit gate | Dependencies |
| --- | --- | --- | --- |
| 0 — Baseline and regression fixtures | Reproduce CE-SP-03/04/05/06/09; add immutable media fixture manifests and portable measurement tooling. Capture current cold/warm opens, beginning/middle/end seeks, sequential playback, and peak memory. | Reproductions fail for the intended reason; benchmark outputs identify revision, hardware, backend, build flags, and cache conditions. | None |
| 1 — Crash and correctness boundaries | CE-SP-01/02/03/06/09: positional source access, cumulative parser budgets, negotiated fallback dimensions, poisoned-session recovery, strict table errors. | Mutation/malformed inputs return typed errors in debug and release; software fallback downscales correctly; injected failures recover deterministically. | Phase 0 |
| 2 — Resource control | CE-SP-07 plus bounded output comparison from CE-SP-10: memory/thread budgets, pixel/window bounds, cancellation, admission contracts, byte-based frame queues. | Work fits configured budgets; over-budget work is rejected early; cancellation releases resources; concurrent workers cannot bypass the host's admission policy. | Phase 1 |
| 3 — Seek and audio continuity | CE-SP-04/05: retained shared cue/index state, one-pass selected-track extraction, audio codec/PCM retention, correct reset and delay handling. | Seek I/O is independent of prior movie duration for indexed fixtures; continuous versus segmented decoded audio matches within declared codec-delay rules; no accumulating A/V drift. | Phases 1–2 |
| 4 — Throughput optimization | CE-SP-08: stage profiles, buffer reuse, native surface paths, planar software paths, streaming mux writes. Evaluate TrueHD and build-profile tradeoffs. | Demonstrated improvement on target hardware at equivalent output quality/timing, with no memory or stability regression. | Phases 2–3 |
| 5 — Fleet qualification and release gates | CE-SP-10/11: baseline-specific bundles, actual device tests, sustained fault/load tests, reproducible benchmark reports, release-blocking regression thresholds. | Every advertised hardware tier passes its explicit workload matrix; unsupported acceleration and filesystems fail or fall back as documented. | Phases 1–4 |

Phase 0 should be small enough to support immediate fixes. Do not delay crash prevention until all fleet hardware is available. Core correctness fixes should land before native surface optimizations.

## Proposed hardware/workload matrix

| Tier | Representative environment | Required workload |
| --- | --- | --- |
| Small NAS | ARM64, 2–4 low-power cores, 1–2 GiB RAM; container capped at 512 MiB; no GPU | Probe, index, packet-copy/remux, bounded audio conversion; early rejection of video work that cannot fit or sustain playback |
| Accelerated NAS | Low-power x86-64 Intel GPU, 2–4 GiB RAM, exposed render node | Supported H.264/HEVC hardware decode/scale/encode, fallback admission, two competing requests |
| Desktop Linux | x86-64 VA-API and a separate NVIDIA machine | Both accelerator paths, device loss/unavailable-driver behavior, CPU fallback |
| Windows | x86-64 Intel/NVIDIA plus ARM64 | Media Foundation initialization/readback, software fallback, packaging and repeated sessions |
| macOS | Intel and Apple Silicon, with differing AV1 hardware support | VideoToolbox and forced software paths, scaled output, simultaneous playback workloads |

Exercise local SSD, rotating storage, and media accessed over SMB/NFS where available. Measure network/media reads separately from local output-cache writes. Validate the chosen older NAS userspace images even when development occurs on a newer Linux host.

Fixtures should cover supported H.264, HEVC Main/Main10, and AV1; 1080p and 4K; 23.976/24/29.97/60 fps; B-frames, long GOPs, VFR, multiple audio tracks, video-only cues/no cues, MP4 moov at either end, truncated files, oversized metadata, and partial last segments. Audio should include supported AAC/AC-3/E-AC-3 copy and TrueHD/DTS/Opus conversion. Include HDR copy and explicitly supported transcode behavior. Do not add legacy codecs merely to enlarge the matrix.

## Proposed acceptance targets

These are starting targets to validate during Phase 0, not current guarantees:

- Zero crashes, deadlocks, malformed published fragments, or unbounded growth in a 24-hour repeated playback/seek/cancel test; extend release-candidate soak to 72 hours.
- On the small-NAS profile, target at most 128 MiB private working memory for a copy/remux session, and a validated configurable budget for transcode. Account for mapped/file cache separately and enforce the container's total memory ceiling too. If a source cannot fit, return a resource error before allocation pressure kills the process.
- For admitted real-time transcodes, target p95 processing time at most half the segment's media duration, with p99 below its duration; this leaves playback headroom. Advertise concurrency based on measured capacity, not CPU core count alone.
- Target first playable output/resume within 2 seconds warm and 3 seconds cold on the agreed reference fixtures/storage. Record source seek, indexing, codec startup, and publication separately. Slow network storage needs its own measured service envelope; it must not conceal avoidable full-prefix scans.
- Cooperative operations should acknowledge cancellation within 250 ms at the next bounded work checkpoint. Set a separate measured watchdog deadline for native calls and ensure workers can be replaced safely.
- No cumulative audio/video drift or repeated access units across a two-hour segmented output, with explicit codec priming/padding accounting and independently checked timestamps.
- After warmup, repeated equivalent session lifecycles must have stable resource counts and no persistent upward private-memory trend. Set numerical regression tolerances per device after baseline variance is known.

## Handoff and scope control

The remaining executable work is host integration and the profile/device-gated items in the execution ledger. Changes to resource policy, source APIs, decoder format negotiation, and audio session behavior need focused server contract tests before deployment. The shipping Apple TV target remains `GenusServer/native/chroma-tvos/ChromaTV.xcodeproj`; integration fixes must be verified there.

Keep final reports explicit about what was built, what was executed on real hardware, and what was only inspected. A healthy server process and a passing codec unit suite do not establish smooth sustained playback.
