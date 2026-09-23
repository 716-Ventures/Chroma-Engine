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
tracked separately below. Physical appliance access was deferred by the owner.

## Phase status

| Phase | Status | Evidence and next action |
| --- | --- | --- |
| N0 inventory/baseline/WD preflight | blocked | Matrix and read-only script exist; owner deferred WD access. Firmware/ABI and server playback baseline on an appliance remain unavailable. |
| N1 server worker boundary | in-progress | Shared admission, finite queue, deadlines/output caps, small-NAS child policy, bounded scanner enumeration, cross-process data-directory lock, graceful SIGTERM, diagnostics, and targeted/workspace tests pass locally. Explicit queued/running cancellation and fault matrix remain. |
| N2 x86-64/ARM64 images | blocked | Server Docker candidate pins Engine SHA and base digests, removes redundant patches, carries notices/provenance, fixes discovery metadata, and defines synthetic probe/remux plus non-root/persistence/stop CI gates. Server Actions billing and local builder storage failures prevent either final architecture image from executing. |
| N3 WD ARMv7 | blocked | [Dependency inventory](armv7-investigation.md) and sparse >4 GiB test exist; toolchain/build/load choice requires measured firmware ABI and device access. |
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

## Open dependencies

- Physical WD access or sanitized output from `scripts/nas-preflight.sh` and
  firmware version from its dashboard. Do not request a password in chat.
- Availability of a representative Intel/AMD NAS and ARM64 NAS, or an agreed
  substitute for build testing with appliance qualification left open.
- User-authorized test fixtures and physical browser/tvOS client for N5.
- Permission/credentials for publishing any private server image or native app.
- GitHub Actions billing/spending-limit resolution for private server CI.
- Sufficient free space on a compatible local builder, or another approved
  amd64/arm64 builder; do not delete existing Colima data to make room.
