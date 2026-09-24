# Complete, efficient library scanning: execution plan for GPT-6 Sol

Prepared 2026-09-24. Status: **planned; implementation and device acceptance pending**.

This plan supersedes the small-NAS probe-skipping behavior introduced in WD
candidate 0.1.9. It retains the live TV count and durable Activity fixes from
that candidate. It is a subplan of [NAS deployment](../nas-deployment-execution-plan.md)
and follows the already implemented [WD installation recovery](wd-os5-install-recovery-plan.md).

## Outcome and execution contract

Deliver a staged scanner that discovers, probes, and matches the entire supported
library efficiently on the WD EX2 Ultra, while preserving macOS, Windows, Linux,
x86-64 NAS, and ARM64 NAS behavior. Every new or changed supported file receives
a technical probe automatically. Playback reuses the resulting full metadata.
No hardware profile may silently omit probing, and no user must play a file to
finish cataloging it. Deliver a fresh dashboard-installable WD `.bin` containing
the changed Engine, Server, and administration UI.

Read this document completely and execute S0–S8. An instruction to implement
this plan authorizes implementation, tests, off-device builds, and scoped commits
and pushes in both repositories. Continue through all independent work without
asking for routine implementation choices. Preserve unrelated changes. Use
existing authorized NAS access for scoped diagnostics; the owner can upload the
final `.bin`. Do not make physical acceptance depend on the owner using SSH.
Do not reset the owner's library, install Jellyfin, modify Plex, change firmware,
or reboot the appliance to obtain a benchmark.

Maintain the execution ledger below. A failed hardware or build gate remains
open while independent work continues. A runnable package is a deliverable even
when physical testing must wait; it is then explicitly an unqualified candidate.
Never mark this plan complete from local tests or simulated NAS results alone.

## Evidence and starting points

Revalidate paths, instructions, branches, and revisions before editing:

| Repository | Reviewed checkout and revision | Main responsibility |
| --- | --- | --- |
| Engine | `/Volumes/Overflow/development/Chroma-Engine`, `main`, `89eb4d0` | Native probe I/O, metadata correctness, resource limits, WD packaging |
| Server | `/Volumes/Overflow/development/GenusServer`, `codex/first-public-release`, `318cb07` | Discovery, durable work, probe cache, metadata providers, API/UI |

Both worktrees were clean when this plan was written. The owner reported a
0.1.8 scan advancing from 166 to 170 out of 1,507 files, with metadata matching
waiting and misleading TV/Activity state. This observation has no reliable
elapsed-time measurement. It does not establish a probe latency distribution.
0.1.9 was built with probes disabled during small-NAS scans; its installation
and completed library scan have not been confirmed.

Measured appliance facts from the installation work: ARMv7 hard-float, firmware
5.33.102, glibc 2.31, approximately 1 GiB RAM, with about 300 MiB available at a
previous idle snapshot. Refresh memory and workload observations before testing.
See [installation evidence](wd-os5-install-recovery-evidence.md) and
[ARMv7 investigation](armv7-investigation.md).

### Confirmed code facts to address

- Server `crates/chroma-jobs/src/scan.rs` collects the file list, then awaits a
  per-file probe inline. `probe_during_scan` disables this for `small`.
- `crates/chroma-jobs/src/lib.rs` awaits a complete library job and only then
  enqueues the `library-metadata` follow-up. Adding a second job to that same
  serial worker does not create an independent pipeline.
- `crates/chroma-jobs/src/probe.rs` applies a 30-second outer timeout and saves
  compact technical fields. The media layer already obtains a full `SourceProbe`
  before reducing it. `streams_json` and `chapters_json` are not saved by scanning.
- `crates/chroma-api/src/routes/playback.rs` has a separate cache reader/writer;
  its persistence updates by media ID without a source-fingerprint condition.
- Scan updates clear dimensions and `probed_at` for changed files, but retain
  old duration, codecs, streams, chapters, and error fields. Cache invalidation
  must cover the complete technical snapshot.
- Server `crates/chroma-media/src/chroma_native.rs` supplies shared subprocess
  admission: one Engine slot on `small`, four on standard, and bounded waiters.
  These slots also serve playback. Preserve a global bound when adding stages.
- Engine `src/source/probe_reader.rs::mkv_metadata` walks every Segment child,
  skipping media payload by offset but reading each top-level element header.
  It does not use SeekHead. Many small reads across clusters are a plausible
  spinning-disk bottleneck; this is a hypothesis until timed and counted.
- Engine MP4 probing reads `ftyp` and `moov`; it does not load the whole `mdat`.
  Small-NAS metadata is capped at 8 MiB. Do not describe existing probing as
  full-file decoding or remove allocation limits without evidence.

### Research used, and its limits

Jellyfin's [scanner proposal](https://github.com/jellyfin/jellyfin-meta/discussions/125)
separates discovery, identification, enrichment, persistence, and optional
post-processing, with centralized scheduling and progress. It is a proposal,
not proof of deployed performance. Its current
[probe provider](https://github.com/jellyfin/jellyfin/blob/master/MediaBrowser.Providers/MediaInfo/ProbeProvider.cs)
checks for changed files and missing media information. Its
[video probe](https://github.com/jellyfin/jellyfin/blob/master/MediaBrowser.Providers/MediaInfo/FFProbeVideoInfo.cs)
persists streams and chapters and treats chapter-image extraction separately.
Apply those architectural lessons through Chroma's existing interfaces; no
Jellyfin runtime dependency or FFmpeg/ffprobe fallback is part of this plan.

## Required architecture

```text
Discover and classify -> persist file identity and pending work
                              |                    |
                    mandatory technical probe     metadata matching
                              |                    |
                         shared bounded database write path
                              |
                     reconcile run and publish final counts
                              |
                     optional derived-image work, if enabled
```

The arrows represent streaming batches. Probing and metadata start from committed
items before discovery finishes. A slow probe must not block metadata for an
already discovered item. Both stages remain accountable to the same scan run.
Do not introduce a requirement to finish all probes before matching metadata.

Use SQLite as the durable work source and bounded in-memory windows for execution.
Start the small-NAS policy with one active background probe, at most two metadata
HTTP requests, one serialized scanner write path, work windows of at most 64
items, and short commit batches capped at 64 items or 100 ms of accumulated work.
These are initial implementation settings, to be verified in S7. No transaction
may remain open during a subprocess, network request, filesystem traversal, or
image download. Existing request handlers may still use their normal database
connections; this is not a mandate to rewrite all Server persistence.

Larger machines may use bounded additional workers. Use configuration and the
existing resource policy, not CPU architecture as a proxy for available memory.
Background jobs, playback, and repair tools must share the process/memory limits.
Foreground playback gets the next available Engine slot before more background
probes. Avoid a backlog of probe tasks already waiting inside the Engine semaphore.

### Completion semantics

- Each file/stage has durable `pending`, `running`, `succeeded`, `retry_wait`,
  `failed`, or `cancelled` state. A valid cached probe counts as satisfied work.
  A missing optional field is not automatically a failed probe: validate required
  fields against the supported container/codec contract.
- Each run records discovery completion separately from probe and metadata
  completion. Counts must distinguish discovered, analyzed, cached, pending,
  failed, matched, unmatched, and metadata unavailable. An unsupported file is
  explicit, not silently successful or endlessly retried.
- `completed` requires successful enumeration and all required work settled,
  with no probe failures. `completed_with_errors` is terminal but visibly reports
  failed/unavailable work. Pending retries keep the run active. An unmatched
  provider result can be a legitimate completed metadata attempt; transport
  errors or missing credentials must be distinguishable from no match.
- Partial results are usable while scanning. Do not label the library fully
  analyzed or stop showing its active work at the end of discovery alone.
- Optional image/preview processing does not determine scan completion. Do not
  add new thumbnail or trickplay features as part of this implementation.

## S0 — Establish baseline and execution records

1. Inspect instructions and current changes in both repositories. Read the
   Engine resource policy, modern-media support contract, WD build recipe, and
   Server engine-integration documentation. Follow the installed working WD
   packaging contract; do not reopen the resolved installer design.
2. Create `docs/nas/staged-scanner-execution-evidence.md` with source revisions,
   environment, fixture characteristics, commands, results, and open gates.
   Keep raw traces in ignored `target/scanner-evidence/<run-id>/` directories.
   Public material uses neutral `media-fixture-*` IDs and never records a
   media-title mapping, credentials, or an identifying NAS library path.
3. Inspect the live Chroma version/process if existing access works. If access
   fails, record the actual error, continue local implementation and packaging,
   and leave physical measurement pending. Do not reuse a pasted password in
   logs or ask the owner to repeat already established dashboard checks.
4. Record the scanner, probe, metadata, API, UI, and package baseline independently.
   Keep the good 0.1.9 reporting changes. Replace its test that asserts zero
   probes with the mandatory-probe acceptance in S7.

**Gate S0:** ledger and source baseline exist; accepted behavior is explicit.

## S1 — Measure the probe and scan costs

1. Add opt-in structured timings across queue wait, process startup, Engine
   metadata reads/parsing, JSON transfer, persistence, and metadata HTTP work.
   Count read operations, bytes requested, visited container elements, timeouts,
   retries, cache hits, and child processes. Report peak resident memory where
   available. Diagnostic fields must not corrupt the CLI's JSON stdout contract.
2. Use at least 20 representative valid files where available: MKV/WebM and
   MP4/MOV, large files above 4 GiB, several supported audio/video combinations,
   multiple audio/subtitle tracks, chapters, attachments, and late metadata.
   Add synthetic malformed/timeout cases separately. Obtain a baseline with
   the pre-change native probe and a clean scratch catalog, without resetting
   the owner's database or modifying source files.
3. Benchmark isolated probes and the complete Server path. Separate the first
   observed read from repeat reads; do not label cache state cold unless verified.
   Do not drop the appliance's global caches or disable other applications.
   Record competing load and available memory. A local SSD or emulator result
   is useful but does not substitute for the WD disk and CPU.
4. Use these initial WD performance goals: valid representative file probes at
   p95 <= 1 second, discovery of the approximately 1,507-file library within
   60 seconds, and discovery plus required technical analysis within 15 minutes
   on an otherwise idle NAS. Record provider/network time separately from local
   analysis. These are engineering targets, not measured claims. If missed,
   identify the cost and optimize; do not quietly relax them or omit work.
5. Resource acceptance: no OOM, process leak, unbounded queue, or sustained
   scanner-induced swap churn. Target combined Chroma Server/Engine RSS <=
   192 MiB during scan-only work, with at least 64 MiB system memory available
   under the recorded idle baseline. Include child processes and metadata/image
   work. Existing allocated virtual-memory budgets are not RSS measurements.
6. Responsiveness goal: lightweight authenticated status requests at p95 <=
   500 ms on the LAN while scanning. Cached playback startup under scan should
   regress by no more than the larger of 500 ms or 20% of its no-scan p95.
   Record stalls and Engine admission waits separately. Full playback quality
   qualification remains a broader milestone, but scanning must not introduce
   observed stalls into the sample playback test.

**Gate S1:** useful component measurements and a repeatable harness exist.
Unavailable device evidence does not prevent S2–S6, but S8 remains unqualified.

## S2 — Reduce native probe I/O while preserving metadata

Primary files: Engine `src/source/probe_reader.rs`, `src/probe/mod.rs`,
`src/source.rs`, `src/resources.rs`, and their existing tests.

1. Measure the Matroska element walk. Implement validated SeekHead-based access
   to required metadata where a usable index exists, avoiding a read at every
   Cluster on the indexed path. Reuse existing EBML parsing primitives.
2. Validate segment-relative offsets, IDs, sizes, bounds, overflow, duplicate
   references, cycles, and maximum seek/element counts. Handle multiple/chained
   SeekHeads and metadata stored after media clusters. Keep a bounded fallback
   for absent, incomplete, or malformed indexes. An index is not proof that an
   unlisted optional section is absent; define and test the completeness rule
   before taking an early exit. Preserve chapters and attachment counts.
3. Count I/O in tests: increasing media payload/cluster count with an otherwise
   equivalent valid index must not make indexed metadata reads grow linearly
   with cluster count. Compare optimized results with known fixture facts and
   the existing parser, not just with snapshots regenerated from new output.
4. Investigate MP4 `moov` allocation and repeated parsing if measurements identify
   them as material costs. Preserve support for end-of-file `moov`, extended
   sizes and >4 GiB offsets on 32-bit ARM. Preserve tracks, languages, duration,
   chapters, profile/color information, and other currently supported fields.
5. Retain explicit memory, work, and deadline limits. A limit error must remain
   visible and classified. Optional extras must not cause otherwise readable
   core information to be reported complete when it was never analyzed.
6. Optimize process startup/serialization only if measured. Keep the supervised
   subprocess boundary initially; a persistent worker or in-process embedding
   is not required. If a persistent worker becomes necessary, add cancellation,
   crash recovery, request isolation, and RSS-growth tests before using it.

**Gate S2:** metadata correctness and bounded-read regression tests pass; before/
after measurements show what improved. Full validation needs the physical NAS.

## S3 — Unify scanner and playback probe persistence

Primary files: Server `crates/chroma-media/src/probe.rs`,
`crates/chroma-media/src/lib.rs`, `crates/chroma-jobs/src/probe.rs`,
`crates/chroma-api/src/routes/playback.rs`, and new additive DB migrations.

1. Move reusable cache validation/persistence into a shared layer without
   creating a Jobs/API dependency cycle. Persist the full typed `SourceProbe`,
   chapters, summary fields, success/error state, source fingerprint, and probe
   schema/compatibility version together. Do not reduce the result before saving.
2. Use the same cache validity rules for scan, playback, and the existing
   `crates/chroma-jobs/examples/backfill_probes.rs`. A successful scan must let
   playback start without another probe subprocess for the same valid snapshot.
3. Fingerprint at least the media identity/path, size, and high-resolution mtime;
   verify again before committing. Include portable file identity where useful,
   without requiring Unix inode support on Windows. Document the residual case
   of externally replaced bytes with an identical fingerprint. Do not hash an
   entire movie on each scan merely to validate cached metadata.
4. Reject late results using a conditional database update on the fingerprint
   and work generation. A changed/deleted file or newer completed attempt cannot
   be overwritten by an old worker. Playback cache writes need the same fencing.
5. Invalidate all technical fields on a source change, including serialized
   streams/chapters, duration, codecs, probe errors, and any derived playback
   analysis cache. Audit the playback-manifest cache's identity/version rules.
   Keep manual descriptive metadata and watch progress unless their own rules
   require an update.
6. Deduplicate concurrent scan/playback requests for the same fingerprint.
   Playback may use/promote an in-flight probe; cancellation of one caller must
   not corrupt another caller's result or leak the child. Bound waiting callers.
7. Add an upgrade-safe selector for missing/incomplete probe work, including
   0.1.9 imports. Wire automatic scheduling into the orchestrator in S4. Full but
   invalid-version snapshots require a new probe; valid successes are reused.

**Gate S3:** tests prove scan-to-playback reuse, invalidation, stale-write rejection,
old-catalog backfill selection, and same-file request deduplication. Automatic
execution of selected backfill work is an S4/S7 gate.

## S4 — Implement durable streaming scan orchestration

Primary files: Server `crates/chroma-jobs/src/{scan,queue,lib,watch}.rs`,
`crates/chroma-server/src/main.rs`, and `crates/chroma-db/src/migrations/`.

1. Extend existing scan/queue tables through additive migrations. Persist run ID,
   source generation, enumeration state, stage counters, and per-file work with
   a unique identity for run/file/stage/generation. Use atomic claims, attempt
   IDs or leases, bounded retries/backoff, and terminal error records. Index
   runnable work queries. Do not use an unbounded task per file.
2. Stream discovery into short batches. Preserve filename classification,
   unresolved entries, supported-extension rules, symlink policy, and existing
   watcher exclusions. Persist file identity and downstream work atomically.
   Enforce existing configured scan limits with a visible error, not truncation.
3. Start independent probe and metadata consumers from committed work. Replace
   the single whole-library follow-up barrier in `lib.rs`. Merely adding more
   job types to its existing serial execution loop cannot satisfy this phase.
4. Route scan-generated mutations through a bounded writer. Coalesce progress
   updates to at most a few writes/events per second plus durable batch/terminal
   boundaries. Keep transactions short; do not hold SQL locks while probing.
5. Maintain one active run per library. Coalesce duplicate manual/watcher scan
   requests, retaining a rescan-needed marker when changes arrive during a run.
   Restart or cancellation must not lose that marker. Fairly schedule work
   across libraries without multiplying the global worker limits.
6. On restart, reclaim interrupted attempts, preserving committed results and
   counters. On cancellation, stop and reap active children, settle leases, and
   retain resumable progress. On transient failure, retry at most three attempts
   by default with backoff; classify malformed/unsupported media as permanent
   for that fingerprint. A failed file must not abort unrelated files.
7. Account for queue wait separately from execution deadlines. Fix the current
   outer timeout if it can charge playback contention as a corrupt-file failure.
   Integrate foreground priority with existing Engine admission; prove a failed
   or cancelled child releases capacity. Do not allow starvation indefinitely.
8. Reconcile removals only after complete, successful enumeration of the affected
   roots. An offline share, partial traversal, permission error, or cancelled scan
   must not prune unseen records. Preserve watch history; use root-specific seen
   generations and the existing identity behavior. Pruning and finalization must
   be crash-safe and must not erase newly discovered concurrent generations.
9. Schedule S3's missing/incomplete backfill automatically for existing libraries
   after upgrade, with coalescing so restart does not enqueue it repeatedly.
   Restore mandatory probes for `small`; remove `probe_during_scan`'s bypass only
   when the new pipeline is connected. Persist explicit technical errors and
   honor completion semantics above. Bound retention of old work/history rows
   without deleting active work or the latest actionable errors.

**Gate S4:** slow/hung probe tests show continued discovery and metadata progress;
restart, cancellation, duplicate requests, and source changes preserve correctness.

## S5 — Adapt metadata matching to incremental work

Primary files: Server `crates/chroma-jobs/src/metadata/{mod,provider,artwork}.rs`.

1. Factor the existing matching logic into per-item or bounded-series-batch work.
   Do not repeatedly call the whole-library matcher for every discovered file.
   Preserve scoring, manual locks, provider selection, season/episode mapping,
   and explicit force-refresh behavior.
2. Cache/deduplicate series and season lookups within a bounded scope. Avoid a
   provider search for every episode of the same identified series. Keep the
   cache bounded and treat conflicting identification hints correctly.
3. Respect provider rate limits, Retry-After, network timeouts, and cancellation.
   A failed HTTP request for one item cannot end metadata work for the library.
   Missing provider configuration must produce a visible actionable state, with
   technical scanning continuing and a way to retry after configuration changes.
4. Make artwork fetches bounded and separately observable. Slow downloads must
   not occupy the only probe slot or hold database transactions. Continue to
   supply normal posters/backdrops; do not eliminate existing artwork behavior
   to pass a performance test. Keep optional generated previews outside the
   required scan lifecycle.
5. Prevent late matching results from overwriting a newer manual match, locked
   fields, or a replaced file. Persist meaningful matched/unmatched/error counts
   and reconcile parent TV series consistently as episodes arrive.

**Gate S5:** first committed items can gain metadata while discovery/probing
continues; provider failures and manual changes are isolated and recoverable.

## S6 — Make progress and completeness visible

Server touchpoints: `crates/chroma-contracts/src/{lib,realtime}.rs`,
`crates/chroma-api/src/activity.rs`, routes `system`, `libraries`, `misc`, `admin`,
and Server event forwarding. UI touchpoints: `apps/admin-spa/src/pages/activity.tsx`,
library management/detail, dashboard activity, onboarding, scan button, and
`apps/admin-spa/src/lib/types.ts`. Inspect shared TypeScript contracts as well.

1. Extend contracts additively where possible; update all producers/consumers.
   Do not redefine an existing discovered-file counter to mean analyzed files.
   Preserve distinct TV-series counts and existing client compatibility.
2. Show discovery, analysis, and metadata progress plus per-stage errors and retry
   state. Avoid a misleading fixed total while discovery is unfinished. Show
   technical properties as pending until known; never fabricate resolution or
   a zero runtime to stand in for unavailable data.
3. Restore progress from durable API state on page load, then apply ordered live
   updates. Reconnects and stale events cannot regress counters or resurrect a
   completed run. Page refresh must not temporarily claim no active job because
   the frontend has not received a realtime event yet.
4. Show terminal success, completion with errors, and cancellation accurately.
   Provide an authenticated retry-failed-work operation using the same durable
   queue, deduplication, resource limits, and fingerprint checks.
5. Keep ordinary UI wording understandable: files found, files analyzed, metadata
   matched, and failures. Detailed durations/admission diagnostics belong in
   Diagnostics. Preserve the working owner setup and Configure flow.

**Gate S6:** browser tests cover refresh, reconnect, out-of-order events, partial
completion, retry, correct TV counts, and catalog technical fields after analysis.

## S7 — Verify correctness, resilience, and performance

Add tests for behavior at stage boundaries, not just helpers returning expected
constants. Use controllable fake probes/providers for scheduling tests and real
native fixtures for metadata and I/O tests. Keep these evidence categories distinct.

| Case | Required observation |
| --- | --- |
| New small-NAS library | Every supported file is automatically probed or has an explicit terminal error; metadata starts before all probes finish. |
| Unchanged rescan | Zero unnecessary probe subprocesses; valid metadata and manual locks remain. |
| 0.1.8/0.1.9 database upgrade | Existing files gain missing full probe snapshots without losing matches, ownership, identity, or watch progress. |
| Playback after scan | Full valid snapshot reused; no duplicate probe launch. |
| Replacement during probe | Stale result rejected; current generation analyzed; old manifests invalidated. |
| Bad file / timeout / child crash | Bounded failure/retry, process reaped, next file progresses, no false success. |
| Restart / cancellation / duplicate scan | Committed work retained; no duplicated claims, stranded jobs, or incorrect completion. |
| Offline or partially unreadable root | No destructive pruning based on incomplete discovery. |
| Metadata outage / rate limit / absent key | Technical work continues; metadata status accurate and retryable. |
| Scan plus playback | Global resource bounds honored; foreground admission and measured responsiveness pass. |
| Large catalog | Memory, task count, HTTP concurrency, and DB batches remain bounded. |
| UI refresh / reconnect | Persisted stage progress and series counts remain consistent. |
| Matroska and MP4 edge cases | Indexed and fallback paths preserve fields; malformed indexes cannot loop or escape bounds. |

Run the relevant package suites and existing CI gates. Starting commands:

```sh
# Engine checkout; retain repository-specific environment/toolchain requirements.
cargo test --locked
git diff --check

# Server checkout.
cargo test --locked -p chroma-db -p chroma-media -p chroma-jobs -p chroma-api -p chroma-contracts
npm run test:engine-integration
npm run test:contracts-rust
npm run typecheck -w @chroma-server/admin-spa
npm run test:admin
npm run test:e2e -w @chroma-server/admin-spa
npm run build -w @chroma-server/admin-spa
git diff --check
```

Update focused regression tests in `scan_small_nas.rs`, `scan_import.rs`,
`metadata_job.rs`, playback cache tests, and `apps/admin-spa/e2e/smoke.spec.ts`.
Verify on native macOS and CI-supported Windows/Linux targets; cross-build the
WD ARMv7 target and retain x86-64/ARM64 coverage. No Unix-only shell test double
may make the portable suite unusable on Windows.

Re-run S1 measurements on identical fixtures and record p50/p95/max, read counts,
peak RSS, queue wait, metadata throughput, subprocess count, and cache-hit rate.
Successful faster discovery alone is insufficient. Complete technical analysis
must meet the target without suppressing errors or fields. A benchmark failure
requires further diagnosis and a recorded result, not another probe-disable flag.

**Gate S7:** required automated checks pass and performance evidence is recorded;
device checks not yet available are visibly pending.

## S8 — Build, deliver, and qualify the WD package

1. Commit tested scoped changes in both repositories. Update the Server's Engine
   integration/pin using its established mechanism and confirm the build uses
   the intended Engine revision. Never label an old binary with a new revision.
2. Follow [off-device ARMv7 build instructions](../../packaging/wd/BUILD.md) and
   [OS 5 packaging](../../packaging/wd/os5/README.md). Rebuild Engine and Server
   for the measured ARMv7 hard-float/glibc 2.31 target and rebuild the admin SPA.
   Inspect the documented SQLx host-macro build issue if it recurs; obtain a clean
   build through a verified toolchain/builder. Do not silently reuse a stale
   Server binary or claim a workaround is reproducible.
3. Select the next unused WD version after checking current artifacts and device
   state; likely 0.1.10 after the reviewed 0.1.9 candidate. Preserve existing
   artifacts. Use the pinned OS 5 packager, verify payload/version/hash agreement,
   run `test_build.py` and `test_install.py`, and record both source revisions,
   binary/SPA hashes, toolchain, target ABI, and package SHA-256.
4. Push scoped commits and verify remote heads and relevant CI results. Supply
   the `.bin` as a clickable local file with its hash and concise WD upload steps.
   Build off-device; the appliance needs no compiler, Rust, Node, or containers.
5. For the owner-installed candidate, verify the deployed version and actual
   binary hashes through available access. Exercise normal rescan/backfill,
   count/probe completeness, metadata matching, page refresh, and sample playback.
   Preserve the database and app data. Use a consistent backup if a rollback
   could require the pre-migration schema; an older binary alone is not always
   sufficient for database rollback.
6. Record physical results against S1 goals for the whole roughly 1,507-file
   library and representative isolated probes. Network metadata availability
   and failed files must be accounted for individually. Do not equate seeing
   posters or a completed progress bar with full probe coverage.
7. Update package README, deployment plan/ledger, and evidence with implemented,
   tested, and pending states. Retire 0.1.9 probe-skipping guidance. Keep broader
   NAS/playback qualification distinct from this scanner acceptance.

**Gate S8:** fresh `.bin` delivered, source/payload provenance verified, scoped
work pushed, and physical functional/performance acceptance recorded. If the
owner has not installed/tested it, record candidate delivered / device pending.

## Execution ledger

Update this table during implementation. Evidence must name commands/results,
revisions, and artifacts; do not replace a failed gate with narrative reassurance.

| Phase | Status | Evidence / remaining work |
| --- | --- | --- |
| S0 Baseline | Not started | Planning source review is recorded above; execution baseline pending. |
| S1 Measurement | Not started | No timed physical probe baseline yet. |
| S2 Native probe | Not started | Indexed metadata-read implementation and validation pending. |
| S3 Shared cache | Not started | Full snapshot persistence/invalidation pending. |
| S4 Staged execution | Not started | Durable independent consumers and mandatory probes pending. |
| S5 Metadata | Not started | Incremental matching and failure isolation pending. |
| S6 Progress | Not started | Stage-aware contracts, UI, and retry operation pending. |
| S7 Verification | Not started | Automated and physical measurements pending. |
| S8 Delivery | Not started | New binaries, package, push, and device acceptance pending. |

## Handoff instruction

> Implement `docs/nas/staged-scanner-execution-plan.md` completely in phase order.
> Read the whole plan and inspect both repositories first. Keep mandatory probing
> on every supported hardware profile, optimize measured native probe costs, and
> persist one full result for scan and playback. Complete implementation, relevant
> tests, documentation, scoped commits/pushes, and a fresh WD dashboard `.bin`.
> Maintain the ledger and distinguish local verification from physical NAS results.
> Continue independent work if device access is unavailable; deliver the package
> with the exact remaining acceptance gate. Do not stop after only introducing
> stages, improving progress reporting, or making discovery appear faster.
