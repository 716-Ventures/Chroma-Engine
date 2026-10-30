# Engine throughput and test review — 2026-10-30

Reviewed the current native source, container, subtitle, Dolby Vision, retained audio/segment, resource-policy and worker-supervision paths, their tests, dependency lockfile, and CI. This is an engineering review with measured test coverage, not a claim of exhaustive correctness or hardware qualification.

## Changes

- Text and bitmap sidecar discovery now filters extensions and same-basename names before requesting file type. This avoids asking a network filesystem for metadata on unrelated directory entries. Regular-file, symlink, canonical-parent, directory-count, track-count and output-size checks remain enforced.
- Text sidecar discovery checks source cancellation during directory scanning and before processing each selected file, matching the bitmap path. A regression test cancels an opened source and verifies that extraction publishes no resources.
- The hot-path Criterion suite now includes full resource extraction beside 1,000 unrelated subtitle files. Generated media and neutral captions keep the benchmark independent of private files.
- CI now runs an instrumented all-feature Rust library/integration suite and publishes an LCOV artifact. Existing cross-platform/MSRV tests, dependency hygiene and fuzz-smoke jobs remain in place. Coverage is visible; no arbitrary cross-platform percentage threshold is claimed validated.

## Verification

On Apple Silicon macOS, Rust 1.99.0, LLVM 23.1.2, native dependencies enabled:

- `cargo test --locked --all-targets --all-features`: 391 library tests passed; integration suites passed. Harness summaries total 420 successful invocations, including subprocess fixtures; this is not a unique-test count.
- The existing two private-source visual qualification tests remain opt-in. Six ignored child entry points are invoked by supervisor tests, not independent missing scenarios.
- Strict all-target/all-feature Clippy and formatting passed. Sidecar-specific regressions were rerun after the extension-list cleanup.
- Instrumented library/integration baseline: **77.63% line coverage**, 73.35% function coverage. Dolby recovery: 89.16%; text sidecar extraction: 82.37%. Inline test bodies are included by the stable Rust coverage tooling; branch coverage was not collected. These rates do not establish complete error-path coverage.
- Current RustSec audit: zero known vulnerabilities; the transitive `paste` unmaintained warning remains. It is absent from the current host's active dependency tree; replacing the upstream platform dependency is outside this patch.

## Synthetic release measurements

Criterion: 10 samples, 100 ms warm-up, 300 ms target measurement; local storage. Values below are central estimates, not production SLAs or before/after speedup claims.

| Operation | Estimate |
| --- | ---: |
| Minimal MP4 probe | 22.3 µs |
| Browser playback planning | 0.598 µs |
| Plan 12,000 packets | 64.8 µs |
| Open indexed MP4 session | 34.1 µs |
| First indexed segment | 8.96 µs |
| Sequential indexed segments | 18.0 µs |
| Parse 2,000 SubRip cues | 747 µs |
| Render 2,000 WebVTT cues | 816 µs |
| Segment 2,000 cues | 960 µs |
| Extract subtitle resources beside 1,000 unrelated files | 1.29 ms |

Reproduce with `cargo bench --locked --bench engine_hot_paths -- --sample-size 10 --warm-up-time 0.1 --measurement-time 0.3`. The throughput fixture is deliberately tiny media; it measures inventory and native interfaces, not full-length video transcoding.

## Coverage limits and follow-up

Linux/Windows codecs, VA-API/device behavior, physical TV playback, real network-share latency and long-running concurrency were not newly qualified on this Mac. Existing CI supplies platform checks and fuzz smoke; no fresh local libFuzzer campaign was run. Native decoders and malformed-container branches still have uncovered paths. Retain explicit source-mutation, bounds, cancellation, timeout and admission tests as correctness gates; do not use a coverage percentage as their substitute.

No production runtime or TV was installed, restarted, or operated for this review.
