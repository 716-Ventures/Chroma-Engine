# Chroma Engine security and quality audit

Date: 2026-10-28
Reviewed revision: `342f81024f109251adbf1594110724e84fc52b91` plus the local audit fixes below.
Scope: container and codec parsing, HLS packaging, worker admission and supervision,
platform and vendored FFI boundaries, dependency advisories, CI, tests, organization,
and performance. macOS code was compiled and exercised locally; Windows and Linux
backend review was static. Chroma Server findings are recorded in its repository.

## Assessment

The review found concrete resource-expansion, reordered-video timing, and Windows
COM lifetime defects. The fixes preserve the existing container, codec, HLS, and
worker interfaces and add targeted regressions. Bounded positional source reads,
shared admission, retained sessions, and immutable output publication remain useful
boundaries to preserve.

The locked dependency audit found **zero known RustSec vulnerabilities** against
advisory database commit `40f5be6fb12ed93fc591bf4ab23b7d793b3f2239`, updated
2026-10-28. This is a dated dependency result, not a guarantee about all source,
native libraries, or driver behavior.

## Findings and remediation

| ID | Priority | Finding and implemented fix | Regression evidence |
| --- | --- | --- | --- |
| CE-QA-01 | P1 | Tiny AVC NALs could expand a bounded compressed window into excessive metadata. H.264 parsing now caps aggregate NAL metadata at 1,000,000 entries, checks offsets before addition/conversion, and uses fallible reservations. Annex-B conversion avoids constructing metadata and temporary output for every sample; aggregate output is capped at 64 MiB. | Many tiny NALs across samples, invalid offsets, truncated input, empty NALs, invalid length sizes, and output expansion at the limit. |
| CE-QA-02 | P1 | HEVC configuration counts could allocate metadata before proving the serialized records fit, and many empty units could expand metadata by roughly 12 times. Parsing now checks array/unit counts against remaining bytes and caps aggregate units at 4,096 before allocation. HEVC sample/chunk Annex-B output is capped at 64 MiB across samples. | Truncated array and unit counts, empty units distributed across arrays, aggregate output expansion, and invalid length sizes. |
| CE-QA-03 | P1 | HLS bandwidth accounting assumed monotonically increasing presentation timestamps, although packets arrive in decode order. Reordered packets now locate the correct window; processing continues after an out-of-range timestamp. The final window now uses the maximum packet presentation end, preserving a later-presented reference frame preceding the final B-frame in decode order. | Reordered timestamps across boundaries and after an out-of-range packet; PTS `[0, 80, 40]` retains all three frames and ends at 120 ms. |
| CE-QA-04 | P2 | Vendored Windows decoder and encoder enumeration stopped after selecting a transform, then freed the raw array without releasing subsequent activation objects. Every activation now drops its COM reference before array deallocation. Successful empty enumerations also free non-null arrays. | Source review of CPU decoder, DX11 decoder, and DX11 encoder paths; native Windows execution remains required. Patch scope is recorded in the vendored documentation and third-party notices. |
| CE-QA-05 | P2 | Worker supervision lacked focused coverage of several cleanup and output-boundary paths. Added five portable subprocess tests in `tests/worker_supervision.rs`. | Success and the single-session child policy, nonzero exit diagnostics, spawn failure and zero timeout, cancellation after observed child startup, admission reuse, and oversized stdout/stderr. Existing stability coverage checks hard deadlines. |
| CE-QA-06 | P2 | The Rust workflow relied on repository-default token permissions. It now explicitly requests only `contents: read`. New Clippy suggestions were addressed with fixed-size slice chunks where the existing geometry already guarantees complete chunks. | Strict Clippy on Rust 1.99; `as_chunks`/`as_chunks_mut` were stabilized in Rust 1.88 and remain compatible with the declared Rust 1.90 minimum. |

The new conversion ceilings intentionally reject output exceeding 64 MiB with a
resource-limit error. Integrations that previously relied on larger expanded
chunks must request smaller windows. These guards do not constitute a hard
process RSS limit or account for every opaque codec/driver allocation.

## Verification and coverage

Formatting, strict all-target/all-feature Clippy, the macOS suite, and
warnings-as-errors documentation builds pass on Rust 1.99.0. Native builds used
Xcode 27 with `DEVELOPER_DIR` explicitly selected and dav1d 1.5.4.

```sh
cargo fmt --all --check
cargo clippy --locked --all-targets --all-features -- -D warnings
cargo test --locked --all-targets --all-features
RUSTDOCFLAGS="-D warnings" cargo doc --locked --no-deps --all-features
python3 scripts/test-check-licenses.py
```

- **300 top-level Rust tests passed**: 275 library, 5 CLI, 5 property, 2 real-probe
  contract, 4 snapshot, 4 stability, and 5 worker lifecycle tests. An additional
  subprocess self-check passes; raw harness logs show 301 passing invocations.
- **8 license-checker Python tests passed**. The complete cargo-about/license
  inventory gate and nightly fuzz campaigns were not executed locally.
- Supported synthetic H.264/Opus MKV smoke passed probing, planning, chunk/manifest
  generation, cache capability checks, native decode capability probes, and two
  retained fMP4 segments in both copy and H.264 modes. Direct HLS/remux of Opus
  remains outside those copy paths; it returned the documented unsupported-audio
  error, so no MP4-source smoke result is claimed from that fixture.
- LLVM 23.1.2 combined test/synthetic-smoke coverage: **20,474 / 26,773 lines
  (76.47%)**, **1,762 / 2,468 functions (71.39%)**, and **29,102 / 38,500 regions
  (75.59%)**. Details: [coverage summary](2026-10-28-coverage.json).
  This includes test code inside compiled macOS source modules and excludes
  platform-gated code for other operating systems. Regions are not branches.
- Remaining measured gaps include HLS 40.35% and the CLI dispatcher 18.33% line
  coverage. Fuzzing entrypoints are 0% in this coverage run; separate CI fuzz
  campaigns are not included in these profiles.
- Five ignored worker functions are subprocess fixtures invoked explicitly by
  lifecycle tests. The existing ignored sleeping fixture serves the deadline
  regression; these are not unimplemented tests.

The dependency audit covers 288 locked packages and reports the existing
`paste 1.0.15` unmaintained advisory, `RUSTSEC-2024-0436`, through the optional
Windows `mediaway-encoder -> mediaway-sw -> rav1e` graph. Its narrow, documented
`deny.toml` waiver remains. The advisory has no fixed version and is a maintenance
warning rather than a reported vulnerability; replacement belongs in upstream
dependency maintenance.

## Organization and performance priorities

Keep parser boundary tests beside the codec implementation and process lifecycle
tests in the dedicated integration file. Keep vendored modifications narrow and
record their upstream revision and patch scope. No broad directory reshuffle or
public-interface rewrite was needed to fix these defects.

H.264 conversion now validates sizes before allocating and avoids the expanded
metadata vector and per-sample temporary buffers. HLS retains a cheap current-window
path and uses binary search when timestamps leave that window. These are code-level
allocation and complexity improvements; this audit does not claim a measured
throughput speedup.

Prioritize additional CLI/HLS failure-path tests, structured malformed-media fuzz
corpora, and measured startup/seek/concurrency workloads before further structural
refactoring. Preserve separate evidence for synthetic smoke tests, real-media
measurements, and hardware qualification. Future module extraction should follow
an actual lifecycle or validation boundary instead of moving large files solely
to reduce their line counts.

## Qualification limits

No native Windows/Linux GPU execution, NAS appliance qualification, independent
player/frame comparison, or long-duration fault/concurrency soak was completed by
this local review. The Windows COM fixes require the corresponding platform CI
and repeated-session checks. Existing fuzz and multi-platform CI definitions do
not establish that those jobs ran against this working tree. Hardware throughput,
network-storage failures, opaque dependency allocation, and audio/video timing
under sustained load remain acceptance work described in the
[stability and performance ledger](2026-09-stability-performance-plan.md).
