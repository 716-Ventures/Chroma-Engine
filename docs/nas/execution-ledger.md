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
Current server Dockerfile pins engine `3ac906705158154a7c4f9b7f1649ef716990912d`
and contains two engine patches. The engine plan was pushed at the starting
revision. Physical appliance access has not yet been established.

## Phase status

| Phase | Status | Evidence and next action |
| --- | --- | --- |
| N0 inventory/baseline/WD preflight | in-progress | Source-backed starting matrix and read-only preflight script; obtain WD firmware/ABI output and establish available test hosts. |
| N1 server worker boundary | not-started | Audit direct subprocess launches in `probe.rs` and `plan.rs` plus the async path in `chroma_native.rs`; implement shared limits. |
| N2 x86-64/ARM64 images | not-started | Reconcile engine pin/patches, validate final image on each architecture. |
| N3 WD ARMv7 | blocked | Requires the device's actual firmware/ABI and a physical run; do independent dependency audit meanwhile. |
| N4 appliance installers | not-started | Depends on built images and accessible vendor devices. |
| N5 playback qualification | blocked | Requires physical NAS and browser/tvOS test path. |
| N6 measured optimization | not-started | Depends on N5 evidence. |
| N7 release/upgrade | not-started | Depends on verified images and qualification. |

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

## Open dependencies

- Physical WD access or sanitized output from `scripts/nas-preflight.sh` and
  firmware version from its dashboard. Do not request a password in chat.
- Availability of a representative Intel/AMD NAS and ARM64 NAS, or an agreed
  substitute for build testing with appliance qualification left open.
- User-authorized test fixtures and physical browser/tvOS client for N5.
- Permission/credentials for publishing any private server image or native app.
