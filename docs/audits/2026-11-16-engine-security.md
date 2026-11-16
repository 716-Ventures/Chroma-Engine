# Chroma Engine security review

Date: 2026-11-16  
Base: `c8ce752`, plus the working-tree fixes described here.  
Scope: Engine container/source parsing, codec and subtitle normalization, platform and vendored FFI boundaries, worker supervision, output publication, dependencies, CI, and regression/performance checks. No deployment, live-server restart, or physical-TV access was performed.

## Confirmed findings and fixes

These findings require a crafted media subtitle or matching sidecar to reach Engine extraction. They do not establish an unauthenticated network endpoint in Engine. Severity is medium: denial of service through excessive memory or CPU consumption, without demonstrated code execution or disclosure.

| Finding | Input and failure | Fix |
| --- | --- | --- |
| TTML expansion checked too late | `write_subtitle_resources` accepts bounded TTML sidecars. A paragraph with timed spans is rendered at every timing boundary. All resulting cue strings were retained before the normalized-document size check, allowing a small source to retain thousands of copies of a large paragraph. Referenced styles could multiply temporary copying and traversal. | Enforce the existing 32 MiB text allowance before escaping, joining, wrapping, and retaining cues. Independently cap cumulative copy/style work at 512 MiB and tree/style visits at 1,000,000. Check the cue count at insertion. Escape in one output pass and reject oversized color tokens before allocating a lowercase copy. |
| WebVTT markup suffix rescanning and expansion | WebVTT overlay normalization searched the entire remaining cue for `>` or `;`, then checked whether the delimiter was within the permitted lookahead. Repeated unmatched `<` or `&` therefore caused quadratic searches. Repeated flat style tags could expand a large ASS string before the outer size check. | Search only the permitted 1,024/16 bytes, with UTF-8-safe delimiter offsets. Bound every output append, using the remaining whole-document allowance after dialogue metadata. Copy ordinary dialogue in runs to keep the normal path efficient. |
| SAMI metadata suffix copying | Each skipped script/style block lowercased the entire remaining document. Many short metadata blocks repeatedly copied/scanned the same suffix; skipped blocks also bypassed the tag counter. | Reuse the already-created ASCII-lowercase document at matching byte offsets. Count skipped metadata against the existing 200,000-tag budget. Mixed-case tags and Unicode content remain supported. |

The TTML XML parser continues to reject DTDs, cap XML nodes and depth, and preserve source timing, frame/tick timebases, languages, referenced styles, and timed spans. No decoder, playback planner, HLS timing, Dolby Vision conversion, or native-app interface was removed or changed.

## Dependencies and other reviewed boundaries

- Updated `rustix` 1.1.4 to 1.1.6 in both lockfiles for [RUSTSEC-2026-0486](https://rustsec.org/advisories/RUSTSEC-2026-0486.html). This is an upstream unsoundness remediation; no Engine extended-attribute call path was found. The separate fuzz workspace lockfile also needed its dependency graph brought up to date with the current Engine manifest.
- Both lockfiles pass `cargo audit audit --deny unsound`: zero known vulnerabilities and no unsoundness warnings. RustSec database commit `02e2517f457a359fc6d3440ea4560c8ea188fee8`, updated 2026-11-15. This is a dated dependency result, not a guarantee about all native code.
- The existing `paste` 1.0.15 maintenance warning, [RUSTSEC-2024-0436](https://rustsec.org/advisories/RUSTSEC-2024-0436.html), remains through the optional Windows encoding dependency graph. It has no patched release and reports maintenance status rather than a vulnerability. Its narrow existing waiver was not expanded.
- Local dav1d is 1.5.4. The bundled OpenH264 headers report 2.6.0, which includes the fix identified in [Cisco's decoder advisory](https://github.com/cisco/openh264/security/advisories/GHSA-m99q-5j7x-7m9x). System decoder versions on other machines remain deployment-specific; see [VideoLAN's dav1d releases](https://downloads.videolan.org/pub/videolan/dav1d/).
- Rechecked container entry/count/index limits, bounded source reads and identity checks, compressed subtitle expansion limits, bitmap region/object budgets, sidecar symlink and directory restrictions, immutable output publication, and worker admission/timeout/output handling. No additional high-confidence exploitable defect was established in those paths.
- Rechecked native buffer dimensional/length guards and scoped FFI ownership. macOS execution is covered below; Windows/Linux driver paths received static review only. Native libraries/drivers retain their own allocation and correctness risks; Engine budgets do not constitute a hard process RSS limit.

## Verification

- `cargo fmt --all --check` and `git diff --check`.
- `cargo clippy --locked --all-targets --all-features -- -D warnings`.
- `cargo test --locked --all-targets --all-features`: **467 passed**, six intentional subprocess-fixture skips across the suite. Includes macOS codec execution, resource extraction, property tests, source/index behavior, and worker supervision. Sandbox-only codec failures were resolved by rerunning with access to this Mac's media frameworks.
- New regressions cover retained TTML expansion across timed intervals, exact byte boundaries, Unicode escaping, style wrapping, copy/visit/reference budgets, repeated malformed markup, flat style expansion, mixed-case SAMI metadata, and metadata work limits. Existing subtitle and playback behavior tests continue to pass.
- `RUSTDOCFLAGS='-D warnings' cargo doc --locked --no-deps --all-features` passes. Corrected one pre-existing bare URL in the SCC documentation so this required check passes without suppressing a warning.
- `python3 scripts/test-check-licenses.py`: eight checks passed. Updated the rustix version in the checked-in attribution inventory after confirming that its COPYRIGHT and all three license files are byte-identical between 1.1.4 and 1.1.6. The full cargo-about inventory generator was not run locally.
- Synthetic MKV generation and two fMP4 copy segments succeeded. The reported copy path opened the source once and created zero video decoder/encoder sessions. This is a packaging smoke test, not a long-duration real-media playback qualification.
- Extended the subtitle fuzz entrypoint to include TTML, SAMI, MicroDVD/SubViewer, SCC, and WebVTT-to-ASS normalization. Added source-timed/style/Unicode seeds and wired them into the existing nightly CI corpus.
- All four local fuzz harnesses built with locked dependencies. Container, codec, subtitle, and fMP4 targets each completed **10,000 smoke inputs** with a 512 MiB RSS limit and no crash. This Mac's stable compiler runs were **not sanitizer-instrumented or coverage-guided**; the existing nightly sanitizer CI has not been executed against this unpushed working tree.

## Performance

An initial 10-sample comparison showed timing shifts even in untouched paths. The final control exported the original source, used the same updated production lockfile and benchmark fixture, and compared it with a freshly rebuilt patched implementation. Both final runs used 20 samples, a one-second warmup and a two-second measurement per benchmark. Values below are Criterion mean point estimates.

| Benchmark | Original control | Patched | Mean change |
| --- | ---: | ---: | ---: |
| `packet_plan/12000_packets` | 62.746 µs | 63.361 µs | +1.0% |
| `plan/browser_primary_audio` | 643.982 ns | 600.872 ns | -6.7% |
| `probe/minimal_mp4` | 21.908 µs | 20.752 µs | -5.3% |
| `session/first_segment_indexed_mp4` | 8.706 µs | 8.828 µs | +1.4% |
| `session/open_indexed_mp4` | 32.279 µs | 33.616 µs | +4.1% |
| `session/sequential_segments_indexed_mp4` | 17.802 µs | 17.801 µs | -0.0% |
| `subtitle_resources/1000_unrelated_siblings` | 1.379 ms | 1.331 ms | -3.5% |
| `subtitle_resources/ttml_timed_spans` | 1.175 ms | 1.109 ms | -5.6% |
| `subtitles/parse_subrip_2000` | 698.773 µs | 695.431 µs | -0.5% |
| `subtitles/render_webvtt_2000` | 768.822 µs | 755.895 µs | -1.7% |
| `subtitles/segment_webvtt_2000` | 918.296 µs | 895.049 µs | -2.5% |

The new benchmark exercises 100 styled TTML paragraphs with timed spans through the public resource-extraction API. It improved by approximately 5.6%; sequential segment generation was statistically unchanged. The unchanged session-open microbenchmark rose approximately 4.1% (about 1.3 µs). Repeated runs also varied on unchanged code, so this review does not attribute every difference to the patches or claim a universal speedup. No video/audio decode, packaging, or playback-plan implementation was modified. These short local checks do not establish end-to-end player startup, network throughput, or sustained hardware performance.

## Qualification limits

No deployment, physical-TV playback, Linux/Windows GPU execution, sanitizer campaign, long-duration malformed-media soak, or independent player/frame comparison was completed. Existing CI definitions and successful local smoke checks do not establish those results. Supported format behavior is preserved for inputs within resource limits; deliberately excessive normalization work now fails with a bounded error rather than consuming unbounded resources.
