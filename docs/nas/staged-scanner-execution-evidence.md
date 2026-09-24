# Staged scanner execution evidence

Started 2026-09-24 against Engine `68956c66dd8800afe2d9d4a2fcbee579d91c4ea8`
and Server `318cb07fed4f1f9f7760174448da4f22dbe51f9d`.
Both worktrees were clean. No repository-specific `AGENTS.md` was found in either
checkout or its immediate parents. The implementation target is
[staged-scanner-execution-plan.md](staged-scanner-execution-plan.md).

## Baseline

- Server `crates/chroma-jobs/src/scan.rs` awaits per-file probes inside the
  discovery loop except when `CHROMA_NAS_PROFILE=small`.
- Server `crates/chroma-jobs/src/lib.rs` enqueues metadata only after the whole
  scan job completes. The current worker runs one queue job at a time.
- Scan probes save compact fields; playback uses a separate full `streams_json`
  cache. Scan cache invalidation does not clear every technical field.
- Engine `src/source/probe_reader.rs` walks Matroska Segment element headers
  across clusters. A physical timing distribution has not been captured.
- WD OS 5 0.1.9 is a candidate. Appliance install and complete scan are not
  confirmed; 0.1.8 owner setup and initial slow scan were observed previously.

## Device access

On 2026-09-24, `ssh -o BatchMode=yes -o ConnectTimeout=7
root@192.168.1.156` returned `Permission denied (publickey,password)`.
No password prompt, remote command, or device mutation occurred. Physical S1
baseline and S8 acceptance remain pending; local development continues.

## Execution ledger

| Phase | Status | Evidence / remaining work |
| --- | --- | --- |
| S0 Baseline | Complete | Source/revisions and device access result above. |
| S1 Measurement | Partial | Opt-in read/element/elapsed diagnostics and synthetic indexed-read test added. No physical or representative 20-file baseline: SSH rejects authentication. |
| S2 Native probe | Implemented locally | Indexed Matroska SeekHead reader with bounded fallback; 1,000-cluster regression shows under 80 indexed reads versus over 1,000 fallback reads. `cargo test --locked` passed on macOS. Device timing pending. |
| S3 Shared cache | Implemented locally | Server uses a single versioned full `SourceProbe` cache for scan/playback, fingerprint fencing, source-change invalidation, and same-fingerprint in-process deduplication. Focused cache test passed. |
| S4 Staged execution | Implemented locally | Bounded streaming traversal, SQLite per-file stages, independent probe/metadata consumers, restart reclaim, retry/coalesced rescan, one-time old-catalog backfill. Small-NAS integration test passed. Physical throughput pending. |
| S5 Metadata | Implemented locally | Per-file matching, bounded series candidate cache, existing manual-lock checks, missing-key unavailable state, retry path. Provider/network throughput not measured on device. |
| S6 Progress | Implemented locally | Durable stage counters in owner API and admin Activity/library UI; failed-work retry operation. Browser e2e rerun pending. |
| S7 Verification | Partial | Engine suite and Server Rust package suite passed before final streaming changes; focused staged/cache tests passed. UI typecheck/build/admin tests passed. Final full rerun and physical performance pending. |
| S8 Delivery | In progress | New ARMv7 Engine and Server cross-builds passed; final Server rebuild, package assembly, provenance, push and owner-installed qualification pending. |

## Local commands and results

- Engine: `cargo test --locked` passed all unit, CLI, fixture, snapshot, stability, and doc tests on macOS. Indexed synthetic fixture contains 1,000 Cluster elements with late Chapters/Attachments and compares against the fallback parser. This is a read-count result, not a WD timing result.
- Server: `cargo test --locked -p chroma-db -p chroma-media -p chroma-jobs -p chroma-api -p chroma-contracts` passed before the final streaming adjustments; `cargo test --locked -p chroma-media -p chroma-jobs --lib` and the revised small-NAS integration test passed afterward. A final broad rerun is still required.
- Admin: `npm run test:engine-integration`, `npm run test:contracts-rust`, `npm run typecheck -w @chroma-server/admin-spa`, `npm run test:admin`, and `npm run build -w @chroma-server/admin-spa` passed. Initial Playwright run failed because its fixture asserted old progress wording; the fixture was updated and rerun is underway.
- Off-device cross-build: `cargo +1.97.1 zigbuild --locked --release --target armv7-unknown-linux-gnueabihf.2.31 --bin chroma-engine` and the matching `-p chroma-server` build passed using the existing pinned dav1d prefix and Zig 0.16. The Engine executable is ELF32 ARM EABI5, hard-float loader `/lib/ld-linux-armhf.so.3`, SHA-256 `dc5ad047b92632f488bab75347f8627d09e04f8dd423ebbf904e0d575c69ca39`. The Server source changed after its first cross-build, so it must be rebuilt before packaging.

Keep public evidence neutral: `media-fixture-*` labels only, no identifying
media paths, titles, credentials, or title-to-fixture mapping.
