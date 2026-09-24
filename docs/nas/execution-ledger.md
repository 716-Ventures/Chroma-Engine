# NAS execution ledger

Started 2026-09-23. This is execution evidence for the
[NAS deployment plan](../nas-deployment-execution-plan.md), not a certification.
Update this file with each milestone. Never publish private media names,
serial numbers, share paths, addresses, or credentials.

## Baseline

| Repository | Starting revision | Branch | Initial status |
| --- | --- | --- | --- |
| Chroma-Engine | `7f766edea5352ad5b366cd0e21cf6f1eaafaf058` | `main` | clean |
| GenusServer | `c996d2d66ffa0b881aedd7ffed337747a983d822` | `feat/apple-inspired-admin` | clean |

The server's current branch differs from `main`; preserve that branch's work.
The server initially pinned engine `3ac906705158154a7c4f9b7f1649ef716990912d`
with two local patches. Both patches were verified redundant against the
current Engine checkout with `git apply --reverse --check`; their SHA-256
values were `6f3a47b2…` (DTS) and `1dfe5b3b…` (HEVC). The server update is
tracked separately below. Physical appliance access was initially deferred; a
read-only WD preflight was supplied by the owner later on 2026-09-23.

## Phase status

| Phase | Status | Evidence and next action |
| --- | --- | --- |
| N0 inventory/baseline/WD preflight | in-progress | Owner-supplied WD preflight confirms ARMv7, OS 5 firmware 5.33.102, glibc 2.31, 1 GiB RAM, no container binary or render node. Host inspection of the NAS `getconf` executable confirms ELF32 ARM EABI5, hard-float, and `/lib/ld-linux-armhf.so.3`. Server playback baseline on an appliance remains unavailable. |
| N1 server worker boundary | in-progress | Shared admission, finite queue, deadlines/output caps, small-NAS child policy, bounded scanner enumeration, cross-process data-directory lock, graceful SIGTERM, diagnostics, and targeted/workspace tests pass locally. Explicit queued/running cancellation and fault matrix remain. |
| N2 x86-64/ARM64 images | blocked | Server Docker candidate pins Engine SHA and base digests, removes redundant patches, carries notices/provenance, fixes discovery metadata, and defines synthetic probe/remux plus non-root/persistence/stop CI gates. Server Actions billing and local builder storage failures prevent either final architecture image from executing. |
| N3 WD ARMv7 | in-progress | [Dependency inventory and cross-build](armv7-investigation.md), sparse >4 GiB test, hard-float binaries, and a Linux OS 5 tool-produced package exist. [EX2 Ultra device evidence](wd-os5-install-recovery-evidence.md) confirms WD-managed 0.1.5 install and 0.1.6 upgrade, `/ready`, Configure redirect, UI assets, persistence, and Chroma hook stop/start. Authenticated dashboard Off/On, media load/performance, reboot, and Server clean-build reproducibility remain open. |
| N4 appliance installers | blocked | Provisional vendor routes exist in the server checkout; no published image or accessible appliance. |
| N5 playback qualification | blocked | Fixture/measurement protocol expanded; fixture rights/hashes and physical browser/tvOS/NAS runs unavailable. |
| N6 measured optimization | blocked | [Acceleration decision record](hardware-acceleration.md) exists; no N5 baseline, so no hardware backend or optimization claim. |
| N7 release/upgrade | blocked | Provisional upgrade/rollback procedure exists in server checkout; no tested images, migration restore, or publication authority. |

## Evidence log

- 2026-09-23: Read both repository baselines, Docker/Compose, server engine
  integration, engine resource/release/player contracts. The server has a fixed
  four-process async semaphore and two separate blocking engine invocations.
  `WorkerSupervisor` is synchronous; wrapping it in `spawn_blocking` requires
  explicit cancellation and queueing design.
- 2026-09-23: [WD's OS 5 matrix](https://support-en.wd.com/app/answers/detailweb/a_id/29230/~/devices-available-and-supported-for-my-cloud-os-5-firmware-upgrade)
  includes the EX2 Ultra `WDBVBZ*` model family. [WD's manual app guide](https://support-en.wd.com/app/answers/detailweb/a_id/29960/~/steps-to-download-and-install-third-party-apps-manually-on-my-cloud-os-5)
  provides a possible packaging route. The installed firmware and ABI remain
  unknown; OS 5 eligibility is not confirmation that OS 5 is installed.
- 2026-09-23: Engine N0 ledger/inventory/preflight commit `8f8f262` pushed to
  `origin/main`. `sh -n scripts/nas-preflight.sh` passed. No preflight was run
  against a user device.
- 2026-09-23: Server `cargo test -p chroma-media --lib` baseline passed 26
  tests. Docker CLI exists but daemon connection failed at
  `/var/run/docker.sock`; this is an environment block, not an image failure.
- 2026-09-23: Engine `cargo tree --locked -p chroma-engine --target
  armv7-unknown-linux-gnueabihf --depth 2` resolved its Rust graph. The new
  sparse >4 GiB MP4 metadata test passed locally. No ARMv7 compilation occurred.
- 2026-09-23: Engine `cargo fmt --all -- --check`, `cargo test --locked`,
  `cargo clippy --locked --all-targets -- -D warnings`, and
  `cargo doc --locked --no-deps` passed on local macOS ARM64. These do not
  certify Linux images, the WD, or client playback.
- 2026-09-23: Server targeted tests passed for bounded process timeout, output
  flood, permit reuse, and single data-directory lock; integration provisioning
  tests passed 6/6. Final server image and appliance tests remain pending.
- 2026-09-23: Engine milestone `526db7a529f8c58b2f89852355dd964f9e4ca8dc`
  pushed to `origin/main`. Server worker commit `4628966` and image candidate
  commit `d91330b87a197380b5ee0624db723bd1477de93b` pushed to
  `origin/feat/apple-inspired-admin`. Server `cargo test --locked --workspace`,
  `cargo clippy --locked --workspace --all-targets -- -D warnings`,
  `cargo fmt --all -- --check`, `npm run test:engine-integration`, and admin SPA
  typecheck/build passed locally on macOS ARM64.
- 2026-09-23: Server [CI run 35876466774](https://github.com/716-Ventures/GenusServer/actions/runs/35876466774)
  failed before any runner started. GitHub's job annotation says recent account
  payments failed or the spending limit must be increased. This is a billing
  block, **not** a code/test result; both architecture image jobs have no
  execution evidence. Engine [CI run 35876158547](https://github.com/716-Ventures/Chroma-Engine/actions/runs/35876158547)
  subsequently completed successfully with all 15 jobs, including x86-64 and
  ARM64 Linux NAS bundle jobs, macOS, Windows, security, fuzz, and release
  builds. Those are Engine-only gates, not complete server-image results.
- 2026-09-23: Local ARM64 server image build attempted in an existing Colima
  profile. Package extraction, npm, Cargo certificate loading, and BuildKit
  metadata all failed with `Input/output error`; the VM was returned to its
  original stopped state without deleting images/volumes. A fresh isolated
  Colima profile could not download its base image because the macOS system
  volume had about 118 MiB free (`No space left on device`). Only its 29 MiB
  incomplete download was removed; the existing profile was untouched.
  Neither attempt produced a usable image or an image-test result.
- 2026-09-23: Server follow-up commit
  `42f0dd1e3792cee654c628d1c6133154b5430d15` pushed to
  `origin/feat/apple-inspired-admin`. It adds a bounded scan cap, explicit
  lock release and second-process test, SIGTERM handling, pinned multiarch
  base indexes, a synthetic `nas-smoke` Docker target, and CI checks for
  read-only media, resource limits, persistence, and graceful restart. The
  base digests were checked to include amd64 and arm64 with `docker manifest
  inspect`. Server workspace tests and Clippy passed locally; the final image
  checks are still unexecuted.
- 2026-09-23: Owner supplied sanitized output from the physical WD EX2 Ultra
  using preflight version 1: `armv7l`, kernel `4.14.22-armada-18.09.3`,
  firmware `5.33.102`, glibc `2.31`, 1,036,672 KiB RAM (306,496 KiB available
  at capture), 1,529,792 KiB swap (1,411,008 KiB free), and 32,514 KiB free
  on the 54,637 KiB root filesystem. Neither Docker nor Podman was present;
  no render nodes were found. ELF class, loader, and float ABI were unavailable.
  The preflight made no software, media, or configuration changes. Script
  version 2 adds a BusyBox `od` fallback for the remaining ABI evidence.
- 2026-09-23: Version 2 still could not inspect ELF because the firmware has
  no `readelf`, `file`, `od`, or `hexdump` command. The owner copied the NAS's
  `getconf` executable to the development Mac via read-only SSH streaming.
  Host `file` identified a 32-bit little-endian PIE ARM EABI5 executable,
  dynamically linked with interpreter `/lib/ld-linux-armhf.so.3`, built for
  GNU/Linux 3.2. Host `xxd` read ELF `e_flags` bytes `00 04 00 05` at offset
  36, or `0x05000400` in little-endian order; bit `0x400` is ARM hard-float.
  This establishes a target candidate but is not a successful cross-build or
  physical Chroma execution.
- 2026-09-23: Installed the Rust 1.97.1 standard library for
  `armv7-unknown-linux-gnueabihf` on the development Mac. A locked
  `CARGO_BUILD_JOBS=2 cargo check --target armv7-unknown-linux-gnueabihf`
  progressed through common dependencies but made extremely slow progress
  before reaching codec/native build steps; it was interrupted with exit 130.
  This is an incomplete check, not an ARMv7 compiler failure or proof of
  support. No matching C cross compiler or glibc sysroot is installed yet.
- 2026-09-23: Zig 0.16 cross-built pinned dav1d 1.5.3 (assembly disabled)
  statically, then linked locked release Engine and Server binaries for
  ARMv7 hard-float with a glibc 2.31 baseline. Both have EABI5 hard-float
  flag `0x05000400`, loader `/lib/ld-linux-armhf.so.3`, and no required glibc
  symbol above 2.30. The Server link required reusing a previously valid
  macOS host SQLx macro dylib after a newly generated one failed to load;
  clean-build reproducibility remains open. Admin SPA was rebuilt. The pilot
  archive contains both binaries, SPA, Engine notices, and checksums; its
  two binary hashes verify after assembly. No NAS upload or execution occurred.
- 2026-09-23: An owner-supplied EX2 Ultra OS 5 package was inspected as data,
  not executed. It established the 204-byte `GrandTeZ` header, model fields,
  payload length/XOR checksum, encrypted app-name signature, archive layout,
  and app lifecycle interface. Chroma's own `.bin` was built from Chroma-only
  files and its header, signature, archive, binary ABI, and shell syntax passed
  local checks. The owner subsequently installed Chroma packages 0.1.0 and
  0.1.1 and 0.1.2; the WD dashboard reported Chroma On, but Configure returned WD's
  HTTP 404 and the API port refused connections. No Engine/Server runtime or
  rollback result exists. Version 0.1.2 relaxes the hard-coded volume mount
  assumption and exposes a minimal startup-status page if Configure resolves.
  WD's documented install hook passes an `_install` source directory; our
  earlier hook rejected that source. Version 0.1.3 accepted it but still
  required the destination not to exist. Read-only SSH inspection after
  installation found that the NAS had created the destination with only
  `apkg.xml`; it had no Chroma payload, web link, or process. Version 0.1.4
  copies into the existing destination and resolves no-argument lifecycle
  hooks from their script paths. Local tests passed at that time; that
  artifact was not a successful device installation.
- 2026-09-24: Replaced the manual WD package serializer with the OS 5 tool,
  verified its format against the known-good reference, and installed 0.1.5
  through WD's manager. Hook traces established the actual staged-app and
  destination arguments. The Server and Engine loaded and `/ready` returned
  200. Configure initially failed because PHP URL streams were disabled on
  the NAS; 0.1.6 used PHP cURL for readiness. WD-managed upgrade to 0.1.6
  completed, preserved server identity, and Configure redirected to the
  working administration UI. Details and remaining gates are in
  [the sanitized evidence report](wd-os5-install-recovery-evidence.md).
- 2026-09-24: The first 0.1.6 owner-password submission was rejected because
  LAN-enabled setup requires a one-time secret, but the NAS hook did not
  provide one and the SPA had no input. A 0.1.7 package candidate now generates
  a private per-device secret and carries the matching SPA form. Local tests
  pass; device installation and owner creation remain unverified.

## Open dependencies

- A test window for authorized-library playback/performance, authenticated
  dashboard Off/On and reboot persistence, plus clean Server build
  reproducibility. The OS 5 install/upgrade/Configure path is proven for this
  EX2 Ultra only. Do not record passwords, addresses, serial numbers, or
  share paths in public reports.
- Availability of a representative Intel/AMD NAS and ARM64 NAS, or an agreed
  substitute for build testing with appliance qualification left open.
- User-authorized test fixtures and physical browser/tvOS client for N5.
- Permission/credentials for publishing any private server image or native app.
- GitHub Actions billing/spending-limit resolution for private server CI.
- Sufficient free space on a compatible local builder, or another approved
  amd64/arm64 builder; do not delete existing Colima data to make room.
