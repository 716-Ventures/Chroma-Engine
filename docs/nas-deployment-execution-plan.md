# NAS deployment execution plan

Prepared 2026-09-23 for execution by GPT-6 Sol. This document is a plan, not a
claim of completed appliance support or permission to deploy onto user hardware.

The WD EX2 Ultra OS 5 installer recovery is documented in the
[2026-09-24 installation recovery plan](nas/wd-os5-install-recovery-plan.md)
and [device evidence](nas/wd-os5-install-recovery-evidence.md). Version 0.1.6
installs and opens the administration UI on the tested appliance; media,
authenticated dashboard Off/On, reboot and broader NAS qualification remain.

## Objective and scope

Deliver installable, reproducible Chroma Server distributions containing Chroma
Engine for media-serving NAS appliances. Validate the entire server-to-client
path, not just the engine executable. Prioritize x86-64 media NAS deployments,
retain a distinct ARM64 track, and investigate and implement a safe ARMv7 path
for the user's WD My Cloud EX2 Ultra where its firmware permits it.

Keep the modern-media scope in [modern-media-support.md](modern-media-support.md).
Do not add FFmpeg/ffprobe fallback, unsupported HDR tone mapping, or bitmap
subtitle burn-in to make this effort pass. Preserve macOS and Windows behavior.

Success means an operator can install, configure, start, test, update, and roll
back the server without a Rust/Node/C compiler on the appliance. Build both
Engine and Server off-device and deliver tested, versioned artifacts. For NAS
models with a supported container manager, publish architecture-specific OCI
images and a vendor-appropriate deployment profile. For the WD EX2 Ultra,
deliver a native My Cloud OS 5 `.bin` app package as the primary installation
path; do not require Docker or an SSH-only manual pilot for normal operation.
Other native vendor packages may be added where model support and user demand
justify them. A successful
cross-build, emulator test, or constrained container is not appliance qualification.

## Read this before implementation

1. Inspect both repositories' current `AGENTS.md`, branch, status, commits, and
   workflows. Preserve unrelated changes. Paths below are starting points, not
   permission to reset or update another checkout.
2. Read this plan completely, then the engine's [resource policy](resource-policy.md),
   [platform support](platform-support.md), [release policy](release-policy.md),
   and [player test contract](web-player-test-contract.md).
3. Read the server's `deploy/Dockerfile`, `deploy/docker-compose.yml`,
   `deploy/NAS.md`, and `docs/chroma-engine-integration.md` before changing them.
4. Create the execution ledger described below. Work in phase order and keep
   each change small enough to test and review independently.
5. Never turn a missing device, secret, package-signing credential, or test fixture
   into a passing result. Complete independent local work, then name exactly
   what evidence or access is missing. Do not call the overall plan complete.

### Repository boundary and baseline

| Repository | Local starting location | Reviewed revision | Responsibility |
| --- | --- | --- | --- |
| Chroma-Engine | `/Volumes/Overflow/development/Chroma-Engine` | `fc5660f164fdc58db18b254597dabb95ca8748e6` | Engine builds, portable codecs, runtime policy, worker API, engine tests |
| GenusServer | `/Users/chrisjdavis/development/GenusServer` | `c996d2d66ffa0b881aedd7ffed337747a983d822` | Server, worker invocation, images, appliance installation, UI, end-to-end tests |

Revalidate these facts at execution time:

- Engine Linux artifacts currently target x86-64 and ARM64. The NAS bundle is
  Debian 12/glibc 2.36, not a universal native NAS binary.
- `src/resources.rs` supplies policies/admission; the small-NAS policy reserves
  up to 384 MiB across two sessions and disables software video fallback.
  Reservations are not a process RSS cap or a whole-server budget.
- Server `crates/chroma-media/src/probe.rs` and `plan.rs` contain direct
  `std::process::Command` invocations. `chroma_native.rs` has a separate Tokio
  worker path with a fixed four-process semaphore. Trace all callers before
  concluding which paths are bounded or covered by cancellation.
- Server `deploy/Dockerfile` pins engine revision
  `3ac906705158154a7c4f9b7f1649ef716990912d` and applies local engine patches.
  Server NAS/integration docs instead describe a default of main. Reconcile
  the actual patch requirements and provenance; do not blindly remove patches.
- Dockerfile metadata lists UDP 32414, while Compose publishes UDP 32514.
  Resolve the actual discovery listener and client protocol before editing either.
  Port publication alone does not prove broadcast discovery across a bridge.
- No physical NAS playback qualification is established by this plan.

## Target and evidence policy

The preliminary recommendation survey covered 22 distinct NAS models from late
2021 through 2026: 20 x86-64 and two ARM64. It was an unweighted editorial sample,
not shipment share or installed-base share; it must not become a marketing claim.
Reconstruct and preserve the rows/sources in phase N0 before relying on that count.

The preliminary 22-model set was: DS220j, DS220+, TS-253D, DS920+, TS-464,
HS-264, AS6704T, F4-423, TVS-h874, DS224+, F2-424, F4-424, F4-424 Pro,
BeeStation Plus, Minisforum N5, F4 SSD, DH4300 Plus, DXP2800, DXP4800 Pro,
AS5402T, ZimaCube 2, and DS925+. DS220j and DH4300 Plus were the ARM64 entries.
Repeated recommendations were counted once; the ZimaBoard 2 single-board server
was excluded. Additional candidates below are not part of that denominator.
Do not reproduce this sample as an exhaustive five-year product census.

Starting recommendation sources:

- [2021/2022 two-bay recommendations](https://nascompares.com/2021/12/17/recommended-2-bay-nas-to-buy-in-2022/)
- [DS920+ media-server review](https://www.androidcentral.com/synology-diskstation-ds920-plus-long-term-review)
- [2022/2023 Plex recommendations](https://nascompares.com/2022/12/29/best-plex-nas-of-2023/amp/)
- [2023/2024 Plex recommendations](https://nascompares.com/2023/12/22/the-best-plex-nas-of-2023-2024-a-buyers-guide/)
- [2025 media NAS recommendations](https://nascompares.com/2025/12/26/best-plex-jellyfin-or-emby-nas-of-2025/)
- [2026 Plex recommendations](https://www.androidcentral.com/best-nas-plex)

Starting device families, not a list of certified devices:

| Track | Representative candidates | Delivery direction | Required distinction |
| --- | --- | --- | --- |
| x86-64 Intel | Synology DS220+/DS920+/DS224+, QNAP TS-253D/TS-464, ASUSTOR AS6704T/AS5402T, TerraMaster F4-423/F4-424, UGREEN DXP2800 | Multi-arch image via vendor container manager first | Intel graphics available does not imply working Chroma acceleration |
| x86-64 AMD | Synology DS923+/DS925+, Minisforum N5 family | Container installation on the actual vendor OS | Do not assume a GPU exists or expose nonexistent acceleration |
| ARM64 | Synology DS220j/DS223j, QNAP TS-233, UGREEN DH4300 Plus | ARM64 image where vendor-supported containers exist; native package otherwise | CPU ISA, installed userspace, RAM, and package availability are separate checks |
| ARMv7 | WD My Cloud EX2 Ultra, WDBVBZ0120JCH-NESN | Firmware-compatible native package/bundle; do not assume Docker | ARMADA 385, 32-bit ARM; current 64-bit artifacts cannot execute |
| Restricted appliances | Synology BeeStation Plus and similar | Investigate documented third-party integration | Plex integration does not establish arbitrary app installation support |

Use explicit statuses: `unassessed`, `build-tested`, `container-tested`,
`appliance-qualified`, `blocked`, or `unsupported-with-evidence`. Track playback
modes separately: direct play, remux, audio conversion, video conversion, HDR
copy, and subtitles. Never inherit one device's certification across a brand.

## Execution ledger and artifacts

Create these files when implementation begins; the names below are proposed,
not existing commands or tools:

- Engine `docs/nas/execution-ledger.md`: phase status, both commit SHAs, commands,
  results, CI URLs, next action, blockers, and approved scope decisions.
- Engine `docs/nas/device-matrix.md`: model/CPU/ISA, userspace bitness, RAM,
  firmware/kernel/libc, containers/native package support, GPU/API, source URLs,
  access availability, evidence date, and qualification status.
- Engine `docs/nas/qualification-protocol.md`: frozen fixtures and thresholds.
- Server `deploy/nas/`: deployment profiles, installers, upgrade/rollback
  instructions, and vendor-specific guides, created incrementally.
- Engine `docs/nas/reports/`: sanitized summary reports and artifact references.
  Keep large media and private raw diagnostics outside Git.

Each ledger row must identify one of `not-started`, `in-progress`, `complete`,
or `blocked`, and include evidence. A limitation is not completion unless an
explicitly approved scope change removes that deliverable.

## N0 — Inventory, baseline, and WD preflight

**Changes:** evidence/diagnostic tooling only; no device mutation.

1. Populate the device matrix using manufacturer specifications and developer
   documentation. Preserve the recommendation source and year independently of
   hardware evidence. Include budget/older media-serving devices; separate
   DIY servers and storage-only appliances from packaged NAS units.
2. Ask which physical devices or remotely accessible test hosts are available.
   Do not buy hardware or assume access. The WD device is a required investigation,
   not an optional item to silently defer behind new hardware.
3. Supply a read-only preflight script. Capture `uname -m`, kernel, firmware,
   userspace ELF architecture, libc/loader, RAM/swap, free storage, mount types,
   supported container runtime and cgroups, render nodes, and existing listeners.
   Commands must tolerate BusyBox and absent utilities. Do not dump environment
   variables, tokens, full mount credentials, serial numbers, or media filenames.
4. For WD, establish firmware-specific supported app/startup mechanisms and the
   ABI before choosing a toolchain. Obtain permission before SSH access or uploads.
   Do not replace firmware, kernel, libc, or existing applications.
5. Capture current automated test results and a server playback baseline on an
   available supported host. Record unavailable NAS results as unavailable.

**Exit gate:** populated source-backed matrix; WD preflight obtained or a precise
access blocker recorded; reproducible baseline commands. Independent N1/N2 work
may proceed while hardware access is pending.

## N1 — One bounded server worker boundary

**Primary files:** server `crates/chroma-media/src/{probe,plan,chroma_native,chroma_hls,env}.rs`,
callers in `chroma-api`/`chroma-jobs`, engine `src/resources.rs`, `src/worker.rs`,
and their exports in `src/lib.rs`.

1. Inventory every engine process launch, including scan jobs, probes, plans,
   playback, warmup, and previews. Build a call-site checklist before refactoring.
2. Consolidate host admission and supervision. Reuse `WorkerSupervisor` if its
   integration fits; otherwise implement an equivalent async boundary with a
   documented reason. Do not create independently limited pools that multiply
   the allowed aggregate workload. Do not block Tokio executor threads.
3. Apply explicit whole-server profiles: constrained NAS defaults to one active
   heavy worker and one configurable software thread, bounded background scanning,
   finite queue/output sizes, and software video conversion disabled. Pass a
   validated child `CHROMA_RESOURCE_POLICY`; do not merely set it in documentation.
4. Add acquisition deadlines, execution deadlines, cancellation, kill-and-reap,
   bounded stdout/stderr, and permit release on every failure. Interactive
   playback must not sit behind an unbounded library scan queue.
5. Expose selected profile, effective limits, engine identity, admission failures,
   and selected playback mode through suitable existing diagnostics/API/UI.
   Validate untrusted config values and return stable, actionable errors.
6. Test multiple server processes explicitly: either disallow them per data
   directory with a tested lock or provide real cross-process coordination.

**Tests:** fake child succeeds/fails/hangs/floods output; cancellation during
queueing and execution; unavailable executable; concurrent scans and playback;
permit reuse; no orphan; aggregate admission; invalid policy; restart after failure.

**Exit gate:** every launch site covered by tests; existing server/engine tests
pass; no unbounded bypass and no FFmpeg fallback. Policy budgets and OS limits
must be reported separately.

## N2 — Reproducible x86-64 and ARM64 server images

**Primary files:** server `deploy/Dockerfile`, `deploy/docker-compose.yml`,
`scripts/ensure-chroma-engine*`, `scripts/vendor-chroma-engine.sh`,
`scripts/engine-provenance.mjs`, `patches/chroma-engine/`, `.github/workflows/ci.yml`;
engine `packaging/Dockerfile.linux`, `.github/workflows/rust.yml`.

1. Resolve a full engine SHA once at build/provisioning time. Development may
   resolve current main; production images must pin the tested SHA. An upstream
   update triggers a candidate rebuild/test, not a live binary replacement.
2. Compare server patches to that revision. Verify applied/redundant/conflicting
   cases with tests, preserve necessary changes, and record patch hashes. If an
   upstream fix is needed, land it in Engine before bumping Server's pin.
3. Build `linux/amd64` and `linux/arm64` on capable builders, not on the user's
   small NAS. Bundle the server, SPA, engine, native shared libraries, notices,
   and machine-readable provenance. Pin dependencies/base digests appropriately.
4. Validate the final image on each architecture: ELF dependencies, non-root
   startup, engine probe/remux, SPA, `/ready`, graceful termination, persistence,
   library permissions, and the absence of FFmpeg/ffprobe runtime dependencies.
5. Test with a read-only media mount, persistent database/cache locations,
   temporary-directory limits, effective memory/CPU/PID ceilings, no GPU devices,
   and no privileged container. GPU access must use narrow device/group grants.
6. Reconcile ports against listeners and clients. Test manual connection and
   discovery independently; document bridge/broadcast limitations. Do not expose
   WAN ports or enable host networking by default to hide a discovery failure.
7. Produce architecture-specific test artifacts and a manifest only after both
   target tests pass. Prepare registry publication; require explicit authority
   for registry visibility, credentials, or publishing the server image.

**Exit gate:** complete server images execute on both architectures; provenance
matches the actual binaries; smoke tests, restart, and persistent state pass.
Emulation is acceptable for a labeled build/smoke gate, not performance evidence.

## N3 — WD EX2 Ultra / ARMv7 implementation

**Dependency:** N0 ABI evidence and N1 supervision. This track may continue alongside
N2 once those dependencies exist; do not treat ARM64 as a substitute.

1. Create a minimal build-and-load spike for both server and engine against the
   measured WD ABI. Evaluate `armv7-unknown-linux-gnueabihf` only if it matches the
   firmware. Record float ABI, loader, libc baseline, kernel requirements, and
   applicable toolchain support; do not assume glibc 2.36 or musl solves this.
2. Audit unconditional dependencies, including dav1d, OpenH264, Rust codecs,
   SQLite/native build steps, atomics, alignment, `usize` conversions, large-file
   offsets, and thread stack requirements. Cross-build with locked dependencies
   and target-compatible C/C++ tooling; retain existing 64-bit tests.
3. Add tests for offsets above 4 GiB and very large source lengths using sparse
   synthetic files. Do not copy multi-gigabyte fixtures merely to test offsets.
4. If a codec dependency prevents a build, isolate the failure. A proposed
   copy-first feature profile must still probe/copy supported modern streams,
   advertise unavailable conversion truthfully, and fail unsupported requests.
   Do not silently delete codecs or ship stub implementations. Obtain a scope
   decision for any reduced media contract.
5. Build off-device and first run from an approved data-volume staging directory. Validate
   executable loading, probe, plan, cache publication, remux, and a small server
   test library before installing a startup service.
6. Produce a My Cloud OS 5 app `.bin` for the EX2 Ultra, containing prebuilt
   Engine and Server binaries, runtime libraries, SPA assets, notices, manifest,
   and lifecycle hooks. Verify the OS 5 package format and model identifier
   against the actual firmware. First test a clearly labeled supervised pilot
   bundle before installing the package; the pilot is a test gate, not the
   supported delivery mechanism. The package must preserve a dedicated data
   directory, integrate start/stop/restart with the WD app manager, and never
   overwrite system files or other apps. Document firmware privilege constraints.
7. Implement upgrade, restart, uninstall, and rollback without deleting media or
   user data. Qualify low-memory behavior before running a full library scan.

**Exit gate:** physical WD boots/runs the complete server and passes direct-play
and remux tests with bounded resources. Audio conversion is a separately measured
capability. If blocked, provide the exact dependency/ABI failure, reproducer,
alternatives, and required decision; do not declare ARMv7 impossible from a single
compiler error or mark this phase complete without physical execution.

## N4 — Appliance installation paths

**Dependency:** N2 images; N3 separately for WD.

For each accessible vendor platform, create a guide and a tested deployment
profile: Synology Container Manager, QNAP Container Station, ASUSTOR Docker/
Portainer, TerraMaster container tooling, and UGREEN Docker. Confirm availability
for the precise firmware/model instead of copying a brand-level assertion.

Each guide must cover image tag/digest and architecture, user/group permissions,
media path mapping, persistent database/cache, secrets generation without logging,
resource limits actually enforced by that runtime, LAN URL, discovery behavior,
health/logs, first scan, update, backup, rollback, stop, and data-preserving uninstall.
Give users a prebuilt artifact, not a source-build command.

For devices without a supported container route, provide a tested native package
where the vendor documents a third-party app path; WD OS 5 is the first required
case. For container-capable devices, native packages (for example DSM SPK or
QNAP QPKG) are a separate usability track, not a prerequisite for OCI support.
Do not fabricate vendor approval or count a package scaffold as a working
installer. Restricted appliances remain
blocked until a documented third-party path is established; do not bypass their
security controls. Native store publication/signing needs separate authorization.

**Exit gate per vendor:** fresh install and restart on a real listed appliance,
read-only media protection, correct architecture selection, update and rollback
demonstrated, no lost state. Untested guides remain marked provisional.

## N5 — Media correctness and performance qualification

**Dependency:** freeze the following protocol before optimization. These are
proposed engineering acceptance targets, not existing measured guarantees.
Changing a missed target requires an explicit decision and retained failed data.

Use licensed/user-authorized fixtures with neutral IDs and recorded hashes. Cover
MP4/MKV, H.264/HEVC Main/Main10/AV1, copy-compatible audio, representative audio
conversion, text subtitles, supported HDR copy, long GOPs, high bitrate, large
files, and Matroska with/without useful cues. Mark unsupported combinations as
expected rejection; do not count them as successful playback.

| Gate | Protocol and initial acceptance target |
| --- | --- |
| Start/resume/seek | On wired LAN with measured storage/network headroom, 30 trials per operation for representative direct-play/remux fixtures; p95 request-to-first-presented-frame <= 3 s warm and <= 5 s cold-cache with disks awake. Report disk-spin-up and cue-less indexing separately, never hide them in warm results. |
| Sustained playback | At least one full-length >= 2-hour fixture per claimed mode on browser and physical tvOS; zero server-attributable rebuffering after startup and no progressive A/V drift; sampled sync error <= 100 ms. |
| Audio conversion | >= 1.25x sustained realtime generation at the supported concurrency, correct layout/loudness, no discontinuities or cumulative drift. Otherwise do not advertise realtime conversion for that profile. |
| Video conversion | Qualify only claimed GPU/codec/profile combinations; >= 1.25x sustained realtime and correct output. CPU-disabled profiles reject video conversion promptly and visibly. |
| Memory/concurrency | Record whole-server plus all-worker peak RSS/PSS where available, container high-water, swap, queue depth, and device available memory. No OOM, no unbounded queue, no escalating swap pressure; effective cap and reserved OS headroom documented for each appliance. |
| Cancellation | Child reaped and capacity reusable within 5 s in the injected hanging-worker test; no unfinished segment published. Report uninterruptible storage/kernel behavior separately. |
| Soak | 24 hours on each claimed hardware class; 72 hours on the lowest-memory qualified device. Fixed workload, no crash/deadlock/orphans, no unexplained >10% settled RSS growth between comparable post-warmup windows, bounded cache and cleanup. |
| Faults | Missing media mount, read errors, full test-cache volume, permission denial, client disconnect, worker crash, lost GPU, restart during output, and network interruption: bounded failure, useful error, no corrupt published segment or database. |

Use engine `scripts/benchmark-session.py` and stats for diagnostics, but measure
end-to-end latency in the real server/client path too. Record session request,
probe, plan, first segment, first frame/audio, and seek completion separately.
Do not equate first HTTP response with playback start. Warm and cold-cache methods
must be explicit; do not drop system-wide caches on a user's live appliance.

Inject faults in temporary test directories/volumes only. Never fill the actual
NAS data volume, unplug a live disk, or power-cycle a production appliance as a
test. Physical tests require an agreed maintenance window and backup plan.

**Exit gate:** per-device report with firmware, both SHAs, image digest, settings,
fixture IDs, client versions, raw measurement references, thresholds, failures,
and verdict. External network/player failures must be evidenced, not presumed.

## N6 — Hardware acceleration gaps and measured optimization

Use N5 profiles to choose changes. A Rockchip ARM64 NAS does not automatically
use the existing Intel VA-API path. Inventory actual vendor device/API access;
the current engine search found no RKMPP/Rockchip backend in `src`.

- Fix proven startup/remux/audio bottlenecks before expanding video backends.
- For Intel, test actual driver/render-node permissions, codec/profile probes,
  fallback policy, and sustained output on each qualified firmware family.
- If ARM64 media hardware requires Rockchip-specific integration, produce a
  bounded backend design, dependency/license review, capability-probe tests,
  cancellation/resource plan, and physical-device benchmark before implementation.
  Until approved and verified, report ARM64 copy/audio capability separately
  from hardware video conversion; no pretend acceleration.
- Keep native-surface/planar optimizations profile-driven. No broad codec rewrite
  or unrelated architecture refactor in the NAS packaging changes.

**Exit gate:** each optimization has before/after evidence and correctness tests;
unimplemented acceleration remains an explicit limitation, not an implied feature.

## N7 — Release, upgrade, and truthful support claims

1. Refresh Linux x86-64/ARM64, macOS, Windows, security/license, and applicable
   ARMv7 gates. Inspect actual CI outcomes, not just push success.
2. Version artifacts with server SHA, engine SHA, patch identity, dependencies,
   target ABI, checksums, and notices. Verify content from the distributable.
3. Test updating an existing database and restoring a backup into a separate test
   directory. Image rollback alone is insufficient after incompatible schema
   migration. Document downtime and the full database/cache recovery procedure.
4. Promote only tested immutable images/packages. Automated newest-engine
   integration must pass gates before promotion; preserve an explicit offline pin
   and never silently reuse stale code after a requested update fails.
5. Update both repositories' documentation with the device matrix and installation
   routes. Distinguish build-tested from appliance-qualified, including exact
   firmware and supported playback modes. Remove blanket NAS guarantees.
6. Publish only with the required authorization and credentials. Do not change
   GenusServer's license/visibility or publish private server source as a side
   effect of open-source engine work.

**Exit gate:** authorized artifacts available, fresh-install/update/rollback
instructions verified, reports linked, limitations visible, all required ledger
items complete or explicitly removed by an approved scope decision.

## Verification and commit discipline

Use existing repository scripts; inspect package scripts and CI before inventing
commands. Baseline checks include:

```sh
# Chroma-Engine checkout
cargo fmt --all -- --check
cargo test --locked
cargo clippy --locked --all-targets -- -D warnings
cargo doc --locked --no-deps

# GenusServer checkout
cargo fmt --all -- --check
cargo test --locked --workspace
npm run test:engine-integration
```

Run platform-feature checks only on supported targets with their dependencies;
add targeted tests for changed paths, final-image tests, and relevant existing CI
gates. Record setup failures accurately rather than weakening a test to pass.

Make coherent commits per phase/subphase. Update the ledger in the same milestone.
Commit engine fixes first, verify them, then update the server pin and integration
tests in a separate server commit. When implementation is authorized, follow the
user's established commit/push preference, preserve unrelated edits, and report
both repository SHAs. Never force-push or publish a release to bypass a failed gate.

## Copyable execution instruction

> Implement docs/nas-deployment-execution-plan.md across Chroma-Engine and
> GenusServer. Begin with N0 and create the ledger. Preserve the modern media
> contract and existing desktop behavior. Complete the locally executable phases
> and their tests, committing and pushing coherent changes. Do not substitute
> generic Linux CI for NAS qualification, skip the WD ARMv7 investigation, or
> claim support without evidence. Request device access, deployment/publication
> authority, or consequential scope decisions when needed; continue independent
> safe work while those are pending. Keep blocked items explicit and finish with
> a complete/partial/blocked matrix, both repository SHAs, CI results, and the
> precise remaining qualification actions. Do not silently stop after a first
> image or a single successful playback.
