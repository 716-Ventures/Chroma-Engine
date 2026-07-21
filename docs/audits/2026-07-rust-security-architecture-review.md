# Chroma Engine Rust, Security, and Architecture Review

Date: 2026-07-21  
Reviewed revision: `362324a` (`main`)  
Target implementer: GPT-5.5  
Scope: `/Users/chrisjdavis/development/Chroma-Engine`

## Executive Summary

Chroma Engine has a sound high-level direction and a stronger baseline than its age suggests: Rust 2024, a pinned current compiler, `unsafe_code = "deny"` with narrowly scoped exceptions, strict Clippy, rustfmt, rustdoc checks, `cargo-deny`, property tests, snapshots, Criterion benchmarks, and a small 1.8 MiB stripped release binary. The current test suite passes (170 library tests, 5 CLI tests, 3 property tests, 2 sanitized real-probe tests, and 4 snapshots), and `cargo deny check` reports no known advisory, license, source, or ban failures.

The engine is not yet ready to be treated as a hardened media-processing library or a stable cross-platform transcoder. The highest-risk problems are structural:

1. `MappedMediaFile` exposes a safe byte slice over a mapping whose safety depends on another process never modifying or truncating the media file. That invariant cannot be enforced by the safe API.
2. MP4 table parsers allocate from attacker-controlled counts before proving those counts fit the containing box or a configured resource budget.
3. The internal time model loses sub-millisecond precision, cannot represent negative media timestamps, and derives ordering lexicographically across incompatible time scales.
4. Matroska video timing is reconstructed in integer milliseconds, causing systematic drift for fractional frame rates such as 24000/1001.
5. The VideoToolbox path serializes asynchronous decoding one packet at a time, clones a complete timing map for every packet, copies every decoded frame through BGRA, and recreates sessions per segment.
6. Encoder callbacks silently discard per-frame failures and label output keyframes from input intent rather than encoded sample metadata.
7. Segment, init, and playlist outputs are generally written directly to final paths. Concurrent readers can observe partial files; the one "atomic" helper uses a deterministic temporary name and is unsafe under concurrent writers.
8. Linux hardware decoders can be reported as `available` based on device-node presence even though no executable Linux decoder exists. The CPU H.264 fallback is also advertised as a profile despite having no implementation.
9. `oxideav-dts` remains vendored, compiled, publicly exposed, and used in the production transcode path even though it was explicitly rejected for real-world performance.
10. CI runs only on macOS, does not test the declared Rust 1.90 MSRV, has no fuzz/sanitizer/Miri coverage, and has no playback-output correctness gate.

GPT-5.5 should implement the remediation in the numbered phases below. Do not combine all phases into one commit. Each phase has required tests and exit criteria.

## Review Baseline

Commands run successfully on Rust 1.97.1 (2026-07-16 patch release):

```text
cargo fmt --check
cargo clippy --all-targets --all-features -- -D warnings
cargo test --all-targets --all-features
cargo deny check
RUSTDOCFLAGS="-D warnings" cargo doc --no-deps --all-features
```

Observed dependency/toolchain issue:

- `block 0.1.6`, pulled through the current Apple media crates, triggers Rust's future-incompatibility warning for an uninhabited static and may become a hard compiler error.
- `cargo-audit` is not installed locally, but `cargo-deny` checked the RustSec advisory database successfully.
- `bitflags` and `bytes` appear to be unused direct dependencies and should be confirmed with an unused-dependency gate before removal.

## Severity Definitions

- **P0:** Can violate memory safety, permit trivial denial of service from a media file, corrupt output, or make capability routing select a nonexistent backend.
- **P1:** Can cause playback instability, dropped frames, drift, incomplete output, severe avoidable latency, or production crashes.
- **P2:** Weakens maintainability, API evolution, observability, portability, or supply-chain assurance.
- **P3:** Cleanup or documentation work that should follow functional hardening.

## P0 Findings

### CE-001: Safe mmap API relies on an unenforceable external invariant

Location: `src/source.rs:12-24`

`MappedMediaFile::open` calls unsafe `Mmap::map` and then exposes `&[u8]` through safe methods. Its safety comment requires callers to keep the source read-only while an operation is active. A library caller cannot prevent a scanner, file replacement, download process, administrator, or another process from truncating or mutating the file. Rust's own mmap documentation notes that a safe mmap abstraction must take responsibility for external mutation and that common wrappers which do not enforce this are likely unsound.

Required implementation:

1. Introduce a `MediaSource` abstraction with bounded positional reads and immutable source identity: file ID/inode where available, length, and modification/change token.
2. Use positional reads (`pread`/`FileExt`) for parser windows and packet spans. Do not return borrowed references into a mutable external mapping from safe APIs.
3. If mmap remains as an opt-in fast path, keep it behind an explicitly unsafe/internal constructor, validate source identity before and after operations, and never expose it as a generally safe guarantee. Prefer a copied immutable metadata window and direct positional packet reads.
4. Detect truncation/replacement as `SourceChanged` and fail the current segment cleanly. Never continue with stale offsets.
5. Keep source handles open for a playback session rather than reopening by path for every segment.

Acceptance tests:

- Concurrently truncate and replace a source while probing and extracting. The engine must return a typed error, not panic, abort, SIGBUS, or emit corrupt output.
- Verify source replacement at the same path is detected.
- Run under Miri for non-OS source abstractions and under AddressSanitizer on supported targets.

### CE-002: MP4 sample-table counts can force excessive allocation

Locations: `src/container/mp4.rs:807-993`, `src/container/mp4/atom.rs:19-50`

Examples:

- `parse_stsz` returns `vec![fixed_size; sample_count]` before proving `sample_count` is sane.
- `parse_stss` allocates `sample_count` booleans from file input.
- `parse_stts` and `parse_ctts` expand run-length entries before bounding each run against the declared sample count.
- `parse_stsc`, `parse_stco`, and `parse_co64` reserve attacker-provided entry counts before proving `header + count * entry_width <= payload.len()`.
- Atom offsets use unchecked `start + 16` and `start + size`; extended 64-bit sizes are cast to `usize` before checked conversion.

A small malicious MP4 can request multi-gigabyte allocations or trigger arithmetic overflow. The existing arbitrary-byte property test only generates up to 4096 bytes and does not assert allocation/time budgets.

Required implementation:

1. Add a `ParseLimits` value passed through all container parsers. Include maximum tracks, boxes/elements, nesting depth, metadata bytes, samples per track, table entries, chapters, attachments, codec-private bytes, and total index memory.
2. Before allocating or looping, prove table geometry with checked arithmetic: `entry_count <= (payload.len() - header) / entry_width`.
3. For run-length tables, append at most `sample_count - current_len`; reject any run whose count would exceed the declared sample count or the configured limit.
4. Replace all lossy `as usize/u32` conversions at trust boundaries with `try_from` and typed parse errors.
5. Use `try_reserve` and convert allocation failure to `ResourceLimit` or `AllocationFailed`; do not rely on allocator abort behavior.
6. Change `AtomIter` and EBML offset calculations to checked addition and explicit oversized/unknown-size handling.
7. Return typed errors with byte offsets and box/element names instead of collapsing malformed input to `None`.

Acceptance tests:

- Add regression fixtures with `sample_count = u32::MAX`, `entry_count = u32::MAX`, oversized 64-bit atom sizes, overflowing nested offsets, and RLE counts larger than the sample count.
- Every fixture must fail within a fixed memory and time budget.
- Fuzz all MP4 and Matroska parser entrypoints continuously; seed with the regression corpus.

### CE-003: Capability reporting can claim nonexistent executable backends

Locations: `src/platform/mod.rs:398-760`, `src/platform/mod.rs:763-935`

Linux VAAPI/QSV/NVDEC decode is marked available from `/dev/dri` or `/dev/nvidiactl` presence, but all actual video decode implementations outside macOS return `BackendUnavailable`. `default_cpu_profile` advertises `chroma-cpu-h264`, but no CPU H.264 encoder exists. Device presence is not proof that a codec/profile, pixel format, driver, or executable path works.

Required implementation:

1. Replace boolean `available` with an explicit capability state: `Modeled`, `Detected`, `Opened`, `Executable`, and `Verified` (or an equivalent enum).
2. Playback/transcode planning may select only `Executable` or `Verified` backends.
3. Perform a real create/decode/encode smoke probe for each advertised codec/profile and surface format. Cache the result by device/driver identity.
4. Remove the CPU profile until a CPU encoder exists. Never use a placeholder as a fallback.
5. Keep planned future backends in documentation or a separate roadmap DTO, not the runtime capability contract.

Acceptance tests:

- On Linux with a fake or inaccessible `/dev/dri`, no backend is selectable.
- On Windows/Linux builds without backend implementations, transcode planning returns a typed `NoExecutableBackend` error before segment work starts.
- Contract tests assert every selectable backend has a callable implementation and successful warmup path.

### CE-004: Rejected DTS implementation is still in production

Locations: `Cargo.toml:28-29`, `vendor/oxideav-dts-0.0.1`, `src/transcode/audio_decode.rs:239-330`, `src/transcode/segment.rs:699-760`, `src/lib.rs:68-89`

`oxideav-dts` and `oxideav-core` remain direct dependencies. `decode_dts_core_to_interleaved_i16` instantiates the vendored decoder, the Matroska transcode path routes DTS through it, and the API is re-exported publicly. This contradicts the explicit requirement to remove the crate because it is too slow for real-world playback.

Required implementation:

1. Remove both dependencies and delete `vendor/oxideav-dts-0.0.1`.
2. Remove the decoder implementation and public exports.
3. Retain the lightweight Chroma-owned DTS header parser only if it is used for probe/routing.
4. Route DTS to a typed unsupported capability until a measured native decoder exists. Do not silently fall back to FFmpeg or another process.
5. Update execution plans so unsupported DTS cannot reach a segment-generation path.

Acceptance tests:

- `cargo tree` contains no `oxideav-*` packages.
- DTS media produces a deterministic planning error with the selected track ID and required capability.
- Existing AAC/AC-3/E-AC-3 copy paths remain unchanged.

## P1 Findings

### CE-005: The time model cannot preserve real container timing

Locations: `src/packet/mod.rs:149-224`, `src/container/mp4.rs:772-777`, `src/container/matroska.rs:1051-1075`, `src/container/matroska.rs:1214-1259`

Problems:

- `TimePoint` uses `u64`, so negative PTS/DTS, edit-list offsets, and preroll cannot be represented.
- Negative MP4 composition offsets are saturated at zero.
- Matroska timestamps and frame durations are rounded to integer milliseconds before packet planning.
- 24000/1001 video becomes approximately 42 ms per frame in Matroska packet timing, producing cumulative drift.
- Derived `Ord` compares `(units, scale)` lexicographically, not by represented time. Two values in different scales can sort incorrectly.
- `to_millis` uses saturating `u64` multiplication, which can silently return a numerically wrong value instead of an overflow error.

Required implementation:

1. Replace the public-field time structs with validated rational timestamp types using signed ticks (`i64`) and non-zero time bases.
2. Preserve native track time bases through demux, decode, encode, and mux. Convert only at protocol serialization boundaries.
3. Implement checked rescaling with `i128/u128`, an explicit rounding mode, and overflow errors.
4. Remove derived cross-scale `Ord`; either normalize comparisons safely or require equal time bases.
5. Preserve negative timestamps internally and normalize an entire output timeline once at mux/HLS boundaries.
6. Derive Matroska decode timing without reducing fractional frame duration to milliseconds.

Acceptance tests:

- 23.976, 29.97, and 59.94 sources show less than one audio sample of A/V drift over two hours.
- B-frame fixtures preserve decode order, presentation order, negative composition offsets, and edit-list behavior.
- Property tests compare rational rescaling against a high-precision reference implementation.

### CE-006: VideoToolbox decode defeats asynchronous operation and performs O(n^2) metadata cloning

Location: `src/transcode/video_decode.rs:348-545`

For every packet, the decoder clones the full `BTreeMap` into the callback closure and immediately calls `wait_for_asynchronous_frames`. This serializes the pipeline and makes timing metadata work quadratic in packet count. Callback lookup is keyed by `pts.as_millis()`, so distinct timestamps can collide after millisecond truncation. Conversion failures become zero timestamps. Every frame is copied from `CVPixelBuffer` into a BGRA `Vec`.

Required implementation:

1. Create one persistent decoder session per playback/transcode session.
2. Submit a bounded batch of packets asynchronously, then drain according to backpressure and end-of-stream state.
3. Attach a stable per-packet context/refcon or exact native timestamp key; do not clone the timing map per packet.
4. Propagate callback timestamp/conversion errors. Never replace them with zero.
5. Return an owned hardware-surface abstraction (`CVPixelBuffer` on Apple) and keep NV12/P010 surfaces zero-copy into scaling/encoding where possible.
6. Provide cancellation and a bounded in-flight frame count.

Acceptance tests:

- Assert no per-packet wait call in the normal batch path.
- Benchmark decode throughput and allocations for 1, 96, and 1000 packets.
- Verify duplicate/sub-millisecond PTS values preserve distinct frame identities.
- Cancellation releases the session and all surfaces without deadlock or leaked callback owners.

### CE-007: Encoder callback failures can become missing frames

Locations: `src/transcode/video_encode.rs:397-469`, `src/transcode/video_encode.rs:473-613`, `src/transcode/video_encode.rs:711-778`

Callbacks return silently when VideoToolbox reports an error, a sample buffer is null, or sample/config extraction fails. Batch encoding can therefore succeed with fewer output frames than input frames. Output keyframe flags are copied from input intent rather than read from the encoded sample attachments. Expected frame rate is set using integer division (`24000 / 1001 == 23`). Single-frame H.264/HEVC entrypoints ignore their bitrate arguments.

Required implementation:

1. Capture the first callback error in shared session state and fail the operation after drain.
2. Track submitted and completed frame IDs; require an output or explicit dropped-frame event for every submitted frame.
3. Read actual PTS, DTS, duration, sync/dependency flags, and codec configuration from each encoded `CMSampleBuffer`.
4. Set expected frame rate using a numeric value that preserves the rational rate.
5. Apply bitrate/profile/level/keyframe interval settings consistently in all encode entrypoints.
6. Replace output `Mutex<Vec<_>>` cloning with ownership transfer or a channel/bounded collector.

Acceptance tests:

- Inject callback failure/null/malformed samples and assert a typed error.
- Assert encoded frame accounting for B-frame and forced-keyframe fixtures.
- Inspect every segment's first output access unit and sync-sample metadata.

### CE-008: Transcoding is segment-stateless and repeats expensive setup

Locations: `src/transcode/segment.rs:152-220`, `src/transcode/segment.rs:267-337`, `src/transcode/video_decode.rs:348-545`, `src/transcode/video_encode.rs:473-613`

Every public segment call reopens/remaps the source, reparses metadata and packet indexes, extracts tracks, creates decoder/encoder sessions, and materializes complete init/media segments in memory. This prevents decoder reference continuity, wastes startup work, and makes playback sensitive to segment request timing.

Required implementation:

1. Add a stateful `Engine` and `PlaybackSession`/`TranscodeSession` API.
2. Session creation opens the source once, validates identity, parses indexes once, selects tracks once, creates codec sessions once, and emits a versioned immutable plan.
3. `next_segment`, `segment(index)`, and `seek(timestamp)` operate against retained state and bounded caches.
4. Share demux/decode/video encode stages across audio renditions.
5. Define cancellation, timeout, backpressure, memory budget, and teardown behavior.
6. Keep a stateless CLI adapter, but implement it over the stateful library API.

Acceptance tests:

- Instrument and assert one source open, one index parse, and one codec-session creation for sequential segment generation.
- Generate at least 30 sequential segments while measuring bounded resident memory and no timestamp discontinuity.
- Seek repeatedly and verify deterministic decoder reset/preroll behavior.

### CE-009: Full-frame BGRA copies and nearest-neighbor scaling are the transcode hot path

Location: `src/transcode/segment.rs:486-696`

Decoded frames are copied to BGRA, each frame is allocated again for scaling, and scaling is a scalar nearest-neighbor loop. This is both expensive and visibly low quality. It also discards the advantages of NV12/P010 hardware surfaces and does not define HDR/color conversion behavior.

Required implementation:

1. Introduce typed pixel surfaces with color primaries, transfer, matrix, range, chroma location, and bit depth.
2. On Apple, use CVPixelBuffer-backed NV12/P010 and VideoToolbox/CoreImage/Metal-compatible conversion paths; avoid CPU readback where the encoder accepts the surface.
3. Implement equivalent backend surface abstractions for Linux and Windows when those backends become executable.
4. Define SDR passthrough, HDR passthrough, and tone-map policies explicitly.
5. Keep a measured CPU scaler only as a real fallback; use a production-quality filter and pooled buffers.

Acceptance tests:

- Allocation and byte-copy counters prove zero CPU frame copies for compatible Apple decode-to-encode paths.
- Pixel/color metadata survives copy paths.
- Golden image tests cover SDR, HDR10, Dolby Vision fallback policy, range conversion, and odd dimensions.

### CE-010: HLS and transcode outputs are not safely published

Locations: `src/hls/mod.rs:472-579`, `src/hls/mod.rs:731-933`, `src/hls/mod.rs:1634-1809`, `src/transcode/segment.rs:152-220`, `src/cli/mod.rs:1485-1494`

Most outputs use `std::fs::write` directly to the final path. A server can serve a partially written `.m4s`, `init.mp4`, or playlist. `write_atomic` uses a deterministic sibling temp path, so concurrent writers race on the same file. It does not create the parent, preserve/replace atomically on Windows, or optionally sync data/directory metadata.

Required implementation:

1. Create one output publisher with unique same-directory temporary files, write-all, flush, optional `sync_data`, and platform-correct atomic replacement.
2. Publish init and media segments before playlists that reference them.
3. Never overwrite an already complete immutable segment unless content identity differs and replacement is intentional.
4. Include a content length/hash in the session cache metadata and verify before serving.
5. Clean abandoned temporary files by session ownership and age, not broad path deletion.

Acceptance tests:

- Concurrent readers observe either the old complete file or the new complete file, never a prefix.
- Concurrent writers to the same segment converge deterministically.
- Windows, Linux, and macOS replacement semantics are tested in CI.

### CE-011: Panic-abort release policy conflicts with an embedded stability goal

Location: `Cargo.toml:55-63`

`panic = "abort"` minimizes binary size but makes any latent panic terminate the entire hosting process. That is acceptable only if Chroma Engine is isolated as a disposable worker process. It is not a suitable default for an in-process server library whose primary requirement is playback stability.

Required implementation:

1. Define separate profiles: an unwind-capable service/library distribution and an optional size-optimized isolated CLI worker with abort semantics.
2. Remove panic paths reachable from untrusted media and add panic-free parser/transcode tests.
3. If the engine remains subprocess-isolated, formalize worker supervision, crash diagnostics, bounded restart policy, and session recovery.

Acceptance tests:

- A deliberately panicking test backend cannot terminate the server integration process.
- Malformed corpus runs complete with typed errors and zero process aborts.

## P2 Findings

### CE-012: Error handling is fragmented and often untyped

Locations: `src/error.rs`, `src/source.rs`, `src/hls/mod.rs`, `src/transcode/segment.rs`, `src/cli/mod.rs`

`EngineErrorCode` exists, but major public paths return `anyhow::Result`, parser failures often become `Option`, and the CLI exits with formatted anyhow chains rather than a stable machine envelope. Callers cannot reliably distinguish malformed input, unsupported capability, resource exhaustion, source mutation, cancellation, or backend failure.

Required implementation:

1. Define a non-exhaustive `EngineError` with stable code, operation, source identity, track/segment context, retryability, and `source()` chaining.
2. Use typed errors in the library. Restrict `anyhow` to the CLI/application boundary.
3. Replace parser `Option` returns with `Result<Option<T>, ParseError>` where absence differs from corruption.
4. Emit versioned JSON errors on stdout/stderr for machine callers, with human rendering as an explicit CLI mode.

### CE-013: Public API and wire contracts are tightly coupled

Location: `src/lib.rs:27-90` and public structs throughout `src/`

The crate root re-exports a broad set of low-level packet, codec, platform, and backend types. Most public structs have all-public fields and derive `Deserialize`, allowing invalid states and making additive fields source-breaking for struct literals. The public `cli` module is also part of the library contract.

Required implementation:

1. Make the stateful engine/session API the primary surface.
2. Separate validated domain types from versioned serde DTOs.
3. Use private fields, constructors/builders, `NonZeroU32`, newtypes for IDs/bitrates/dimensions, and `#[non_exhaustive]` on evolvable public enums/errors.
4. Move CLI code into a separate binary crate or keep it out of supported library docs.
5. Add `cargo-semver-checks` against the last released API once a baseline release exists.

### CE-014: Files are too large and ownership boundaries are blurred

Evidence: `src/hls/mod.rs` is 3347 lines, `src/container/mp4.rs` 2622, `src/container/matroska.rs` 1897, `src/cli/mod.rs` 1505, and `src/fmp4/mod.rs` 1498.

Required implementation:

1. Split by responsibility, not arbitrary line count: parser primitives, metadata, sample tables, packet index, segment planning, mux, playlist rendering, and output publishing.
2. Keep protocol adapters separate from native timing/session logic.
3. Do not change behavior during the split; use snapshot and differential tests.

### CE-015: CI does not validate claimed platform or MSRV support

Location: `.github/workflows/rust.yml`

CI runs only `macos-latest` with Rust 1.97.1. `Cargo.toml` declares `rust-version = "1.90"`, but 1.90 is never tested. Linux/Windows code is not compiled in CI, and runtime capability tests currently exercise modeled strings rather than implementations.

Required implementation:

1. Decide the MSRV policy. Either test 1.90 on every supported feature set or raise `rust-version` to the actual minimum. Rust's `rust-version` contract applies to all package targets and features.
2. Add macOS, Ubuntu, and Windows compile/test jobs.
3. Separate compile-only backend jobs from hardware-runner integration jobs.
4. Add `cargo test --locked`, rustdoc, `cargo-deny`, future-incompatibility rejection, and unused-dependency checks.
5. Replace the future-incompatible `block 0.1.6` dependency chain with maintained Apple bindings or a patched/upgraded stack before it becomes a hard error.

### CE-016: Security testing is below the parser's risk level

There is no `fuzz/` workspace, sanitizer job, Miri job, mutation test, or parser resource-budget test. Property tests over small arbitrary buffers establish basic panic resistance but not security resilience.

Required implementation:

1. Add `cargo-fuzz` targets for container sniffing, MP4 metadata/sample tables, EBML iteration/lacing/cues, H.264/HEVC config and NAL conversion, AAC/AC-3/DTS header parsing, subtitle parsing, and fMP4 mux inspection.
2. Add structure-aware generators for valid-near-invalid MP4/Matroska.
3. Run a short fuzz budget on PRs and longer scheduled jobs; archive crashing inputs as regression tests.
4. Run Miri on pure-Rust unit tests and sanitizers on parser/mux integration tests where supported.
5. Add memory/time ceilings for every untrusted-input operation.

### CE-017: Playback correctness is not a release gate

Current real fixtures validate probe/plan JSON. They do not prove decoded frame count, HLS continuity, fMP4 conformance, A/V drift, seek behavior, or browser/tvOS playback.

Required implementation:

1. Build a redistributable media corpus covering MP4/MKV, H.264/HEVC, B-frames, fractional frame rates, AAC/AC-3/E-AC-3, multiple audio languages, subtitles, chapters, VFR, edit lists, HDR, and malformed/truncated variants.
2. Add an internal fMP4/HLS validator for monotonic DTS, composition offsets, sync starts, contiguous decode timelines, sample counts, data offsets, and declared codec configuration.
3. Gate releases on maximum A/V drift, no timestamp gaps/overlaps, deterministic seek hashes, and stable frame counts.
4. Keep real mounted-media smoke tests, but do not rely on them as the only end-to-end evidence.

### CE-018: Benchmarks do not enforce regressions or cover the real hot path

Criterion currently reports success but CI does not compare thresholds. The benchmarks cover probe/planning/subtitles, not sustained demux/decode/scale/encode/mux, seek latency, first-segment latency, allocation count, or memory high-water mark.

Required implementation:

1. Add benchmarks for first segment, sequential segments, random seek, MP4/MKV indexing, VideoToolbox batch decode/encode, and fMP4 mux.
2. Record bytes copied, allocations, peak memory, session creations, and source reads in addition to wall time.
3. Establish per-platform regression budgets and require explicit approval for meaningful regressions.

### CE-019: Supply-chain and release hardening is incomplete

`cargo-deny` is present and currently clean, which is good. Missing controls include scheduled advisory scans, automated dependency updates, auditable binary dependency metadata, release provenance/SBOM, `SECURITY.md`, and pinned GitHub Action commit SHAs.

Required implementation:

1. Add scheduled RustSec/cargo-deny checks and dependency update automation.
2. Pin third-party actions by immutable commit SHA.
3. Embed dependency metadata with `cargo-auditable` or emit an equivalent SBOM for release artifacts.
4. Publish checksums and provenance for each platform artifact.
5. Add private vulnerability-reporting instructions and a supported-version policy.

### CE-020: Direct dependencies and feature topology need cleanup

`bitflags` and `bytes` appear unused directly. Apple dependencies are always included on macOS even for probe-only builds. CLI, tracing, serde, and platform backends are not feature-separated, limiting minimum binary configurations.

Required implementation:

1. Verify and remove unused direct dependencies with a CI gate.
2. Split features/crates around actual deployment units: core timing/types, MP4, Matroska, HLS/fMP4, subtitles, CLI, and platform backends.
3. Keep defaults aligned with the shipping product, not an empty minimal build.
4. Measure size and performance for every split; do not add abstraction layers without evidence.

## P3 Findings

### CE-021: Rustdoc explains fields but not operational failure contracts

Public fallible functions generally lack `# Errors` sections, panic behavior, resource limits, concurrency guarantees, and examples. Add focused examples for session creation, planning, segment generation, seek, cancellation, and structured errors. Follow the Rust API Guidelines without mechanically documenting obvious field access.

### CE-022: Runtime claims and implementation status are mixed

`docs/implementation-checklist.md` marks cross-platform hardware decode contracts complete even though Linux/Windows execution is absent. Keep three statuses distinct: designed, detected, and executable/verified. Documentation and runtime DTOs must use the same terminology.

### CE-023: Release/change management is absent

Add a changelog, compatibility policy for schema versions, deprecation policy for the Rust API, and migration notes for GenusServer/tvOS/web consumers. Schema numbers alone are not enough; define compatibility and unknown-field behavior.

## Required Implementation Order

GPT-5.5 should use this sequence and stop only when a phase's acceptance criteria pass:

1. **Security containment:** CE-002 parser limits/checked arithmetic, CE-001 source safety, CE-010 atomic publishing.
2. **Truthful routing:** CE-003 capability states and CE-004 removal of `oxideav-dts`.
3. **Timing correctness:** CE-005 rational signed time model and output validation from CE-017.
4. **Stateful architecture:** CE-008 engine/session lifecycle with cancellation, budgets, retained indexes, and retained codec sessions.
5. **Apple hot path:** CE-006, CE-007, and CE-009 with zero-copy surfaces and callback accounting.
6. **Error/API cleanup:** CE-012 and CE-013, including versioned wire DTOs and migration adapters.
7. **Cross-platform implementation:** implement and verify Linux/Windows backends one at a time; do not advertise placeholders.
8. **Continuous assurance:** CE-015 through CE-020.
9. **Documentation/release policy:** CE-021 through CE-023.

## Commit Strategy for GPT-5.5

Use small commits that each preserve a green build. Suggested commit boundaries:

1. Add parser limits and malicious fixtures.
2. Harden atom/EBML arithmetic and error types.
3. Replace the safe mmap source abstraction.
4. Add atomic output publisher and migrate all writes.
5. Remove `oxideav-dts` and update routing.
6. Make capability reporting executable-only.
7. Introduce signed rational time types behind adapters.
8. Migrate MP4 timing.
9. Migrate Matroska timing.
10. Add stateful session/source/index lifecycle.
11. Make VideoToolbox decode batched and callback-safe.
12. Make VideoToolbox encode callback-accounted.
13. Add zero-copy Apple surfaces/scaling path.
14. Add playback correctness corpus and gates.
15. Add platform/MSRV/fuzz/security CI.
16. Split public API/wire DTOs and large modules without behavior changes.

Do not mix timing-model migration, parser hardening, and module splitting in the same commit. Those changes have different failure signatures and must be bisectable.

## Definition of Done

The remediation is complete only when all of the following are true:

- Malformed media cannot trigger unbounded allocation, unbounded parser work, panic, abort, or unsafe mmap failure.
- All selectable capabilities are executable and warmup-verified on the current host.
- No `oxideav-*` code remains.
- Internal timing is signed, rational, and precision-preserving; two-hour A/V drift tests pass.
- Sequential transcode segments reuse source/index/codec state and have continuous timestamps.
- Encoder/decoder callback failures are never silently converted into missing frames.
- Compatible hardware transcodes avoid CPU BGRA round trips.
- Segments and playlists are atomically published.
- The public library returns stable typed errors and valid-state domain objects.
- macOS, Linux, Windows, and the declared MSRV compile/test in CI.
- Fuzz, sanitizer/Miri, dependency, conformance, and performance-regression gates run automatically.
- Web and tvOS playback tests pass for the representative MP4/MKV corpus without FFmpeg fallback.

## July 2026 Reference Basis

The recommendations were checked against current primary guidance as of 2026-07-21:

- Rust 1.97.1 release notes: <https://doc.rust-lang.org/stable/releases.html>
- Cargo `rust-version`: <https://doc.rust-lang.org/stable/cargo/reference/rust-version.html>
- Rust 2024 resolver behavior: <https://doc.rust-lang.org/stable/edition-guide/rust-2024/cargo-resolver.html>
- Cargo SemVer compatibility: <https://doc.rust-lang.org/stable/cargo/reference/semver.html>
- Rust API Guidelines checklist and failure documentation: <https://rust-lang.github.io/api-guidelines/checklist.html> and <https://rust-lang.github.io/api-guidelines/documentation.html>
- Rust unsafe-code and Miri guidance: <https://doc.rust-lang.org/stable/book/ch20-01-unsafe-rust.html>
- Rust standard-library mmap safety discussion: <https://doc.rust-lang.org/stable/src/std/os/unix/io/mod.rs.html>
- Rust Fuzz Book: <https://rust-fuzz.github.io/book/>
- RustSec tooling and advisory database: <https://rustsec.org/>

## Instructions to GPT-5.5

Before editing each phase:

1. Read the complete affected modules and their callers.
2. Consult current FFmpeg behavior only to understand container/timing invariants and failure handling; do not copy code or reproduce FFmpeg's API.
3. Consult AetherEngine for architectural lessons where relevant; do not import its API shape blindly.
4. Add a failing regression/security/performance test before changing behavior.
5. Preserve Chroma Engine's Rust-first domain model and make server/client adapters follow the corrected engine contract.
6. Run all baseline gates plus the phase-specific acceptance tests.
7. Commit and push the completed phase before beginning the next one.

When a recommendation conflicts with measured performance, keep the invariant and optimize its implementation. Do not remove validation, error propagation, resource budgets, or atomic publication to win a microbenchmark.
