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
| S6 Progress | Locally verified | Durable stage counters in owner API and admin Activity/library UI; failed-work retry operation; Playwright refresh/retry smoke passed. |
| S7 Verification | Local checks passed; device pending | Final Engine and Server Rust suites, admin tests/typecheck/build and Playwright smoke passed. Physical probe and full-library performance pending. |
| S8 Delivery | Candidate delivered; device pending | ARMv7 binaries and OS 5 package inspected, 18 installer tests passed, source pushed. Owner installation and physical acceptance pending. |

## Local commands and results

- Engine: `cargo test --locked` passed all unit, CLI, fixture, snapshot, stability, and doc tests on macOS. Indexed synthetic fixture contains 1,000 Cluster elements with late Chapters/Attachments and compares against the fallback parser. This is a read-count result, not a WD timing result.
- Server: the final `cargo test --locked -p chroma-db -p chroma-media -p chroma-jobs -p chroma-api -p chroma-contracts` passed, including real stage retry/cache tests and a small-NAS integration test that waits for two invalid fixtures to reach explicit terminal probe errors while metadata is marked unavailable.
- Admin: `npm run test:engine-integration`, `npm run test:contracts-rust`, `npm run typecheck -w @chroma-server/admin-spa`, `npm run test:admin`, `npm run build -w @chroma-server/admin-spa`, and `npm run test:e2e -w @chroma-server/admin-spa` passed. Playwright covers persisted activity after refresh and the retry action. The first run caught an outdated fixture assertion; it was corrected before the passing rerun.
- Off-device cross-build: `cargo +1.97.1 zigbuild --locked --release --target armv7-unknown-linux-gnueabihf.2.31 --bin chroma-engine` and the matching `-p chroma-server` build passed using pinned dav1d and Zig 0.16. Both executables are ELF32 ARM EABI5 with the hard-float loader `/lib/ld-linux-armhf.so.3`. Engine SHA-256 `dc5ad047b92632f488bab75347f8627d09e04f8dd423ebbf904e0d575c69ca39`; Server SHA-256 `3a62152ba680a89b55174019918e7b36043433c590e7a6f744c5aa5e7ce09997`.
- Package: `target/wd-os5/MyCloudEX2Ultra_chromaserver_0.1.10.bin`, SHA-256 `6bdc95263f06b8c74b52ecdc279e938d979d71dcf4320dc932ec655b78328d9d`, 9.3 MiB. `packaging/wd/os5/build.py` independently inspected the header, signature, version, file modes and payload. `CHROMA_WD_DOCKER_CONTEXT=colima-wd-os5 python3 -m unittest packaging/wd/os5/test_build.py packaging/wd/os5/test_install.py -v` passed 18/18. Provenance in the payload names Engine `9e0d56b0d789dab4951ec00168d0e074716f87e1` and Server `8d40dd0092720cb0732dd7f37fa9d703c6cedbb4`.
- Git: Engine main and Server `codex/first-public-release` were pushed at the above revisions. The Engine Rust workflow for that revision was still running when checked; Server branch had no recent workflow run listed.
- The optional Docker `nas-smoke` image build resolved the exact Engine commit but failed during `apt`/BuildKit layer commits with `input/output error` in the local Colima content store. The macOS system volume had only 177 MiB available, while the project volume had 1.7 TiB. The generated cross-build workspace was moved intact from `/tmp` to ignored `target/scanner-evidence/` (with a symlink preserving the original path), restoring about 1.7 GiB on the system volume. `docker system df` still reports a content-store blob I/O error, so this Docker gate is environmental and remains open; do not claim an image passed. The WD `.bin` was built and independently inspected before this Docker attempt and is unaffected.

Keep public evidence neutral: `media-fixture-*` labels only, no identifying
media paths, titles, credentials, or title-to-fixture mapping.
