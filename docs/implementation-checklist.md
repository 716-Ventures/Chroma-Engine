# Implementation and Qualification Checklist

Status reviewed: 2026-09-22, against engine commit `5db5081`.

This is a current inventory, not a completion percentage. Checked items mean an
implementation exists; they do not certify every codec/profile, device, or client.
See [current status](status.md) for evidence and the next qualification milestone.
Historical findings and measurements remain in the [audit ledger](audits/2026-09-stability-performance-plan.md).

## Implemented engine capabilities

- [x] Rust library and CLI with public APIs re-exported from the crate root.
- [x] Native probe manifests, typed tracks, codec configuration, chapters, timing, and capability hints.
- [x] MP4/M4V/MOV and MKV/WebM source parsing within the [modern media profile](modern-media-support.md).
- [x] Playback and HLS conversion planning with explicit unsupported-capability reporting.
- [x] Bounded owned metadata and positional packet reads; no source memory mappings or whole-file heap copies.
- [x] Source identity validation, parser/index ceilings, and checked packet ranges.
- [x] Retained packet/chunk planning, Matroska cluster indexes, and bounded random-access windows.
- [x] Packet-copy remux and HLS/fMP4 packaging for supported track combinations.
- [x] Demand-driven segments and contiguous windows, playlists, initialization data, and text-subtitle sidecars.
- [x] Multiple audio renditions without duplicating video work in supported packaging paths.
- [x] H.264 CPU decode/encode, HEVC Main/Main10 CPU decode, and AV1 decode through dav1d.
- [x] Copy-compatible audio paths and retained Opus/DTS Core/TrueHD-to-AAC bridges within supported layouts.
- [x] Portable AAC-LC and AC-3/E-AC-3 encoder APIs; availability does not imply every planner selects every encoder.
- [x] Runtime-gated macOS VideoToolbox, Windows Media Foundation/D3D11 and supported encoders, and optional Linux VA-API/NVIDIA backends.
- [x] Sequential codec-state reuse, seek/reset handling, and one policy-permitted hardware-failure fallback.
- [x] Shared admission, configurable resource budgets, cooperative cancellation, and supervised CLI workers.
- [x] No-replace artifact publication and cache filesystem preflight.
- [x] Typed errors for supported interfaces; native conversion and some other APIs still return `anyhow::Result`.
- [x] Session timing/I/O/backend metrics, generated-media smoke fixtures, and benchmark tooling.
- [x] DTS PCM scaling regression coverage and corrected HEVC color signaling: bit depth alone is not an HDR10 declaration.

## Engineering and distribution evidence

- [x] Rust 2024 with Rust 1.90 minimum, formatting, strict Clippy, tests, and rustdoc gates.
- [x] Container/codec/subtitle/fMP4 fuzz smoke jobs and regression/property tests.
- [x] Dependency auditing, generated license inventory, and attribution checks.
- [x] Auditable release-bundle builds with native dav1d libraries and checksums.
- [x] Hosted macOS ARM64 and Windows/Linux x86-64 CI, plus native Windows/Linux ARM64 jobs.
- [x] Debian 12/glibc 2.36 NAS bundle jobs on x86-64 and ARM64, including a synthetic workload under 512 MiB.
- [ ] Renew Intel macOS CI/release coverage if shipping that target. Source portability is not current hosted qualification.
- [ ] Publish the first GitHub release after reviewing the exact tagged commit, artifacts, notes, and release gates.

All 15 jobs in [the reviewed CI run](https://github.com/716-Ventures/Chroma-Engine/actions/runs/35770722559)
passed. This is bounded automated evidence, not a sustained appliance or player test.

## Host integration

- [x] Engine-side APIs and CLI outputs required for native probe, remux, and playback without FFmpeg/ffprobe.
- [x] Document Rust embedding, CLI supervision, cache ownership, deployment, and update responsibilities in the [adoption guide](adopting-chroma-engine.md).
- [x] Earlier integration work demonstrated short MP4 and Matroska H.264/AAC playback windows in Chromium.
- [ ] Revalidate the current GenusServer build and deployed engine identity together; an engine commit does not update a running server automatically.
- [ ] Verify host-wide admission, timeouts, cancellation, cache quotas, and external process limits in the deployed server.
- [ ] Qualify the actual browser and tvOS playback contracts, including audio selection and resume behavior.

GenusServer's current native-engine integration supersedes the old opt-in/FFmpeg
migration experiments. Those historical environment flags and fallback recipes
are not current integration instructions. See the host repository for its
deployment controls; this checklist does not certify a running installation.

## Remaining production qualification

- [ ] Build a neutral-ID media matrix covering supported containers, profiles, audio layouts, HDR copy, and text subtitles.
- [ ] Measure cold starts, distant resumes, repeated seeks, track changes, and concurrent sessions through the real server and players.
- [ ] Validate decoded audio levels/channel order, video color, and long-run A/V synchronization independently of engine metadata.
- [ ] Run 24/72-hour soak tests and track memory growth, resource peaks, and throughput.
- [ ] Test network-storage latency/disconnection, source replacement, disk exhaustion, cancellation, and device loss.
- [ ] Measure real GPU and NAS workloads before prioritizing Linux/Windows native-surface and portable planar optimizations.
- [ ] Define and enforce startup/resume/stall/resource acceptance thresholds per device class.

## Feature decisions, not completed work

- [ ] Decide whether HDR-to-SDR tone mapping is required for the initial dependable release; implement and qualify it if so. Currently rejected, with compatible HDR copy preferred.
- [ ] Decide whether bitmap-subtitle rendering/burn-in is required for target clients. Currently not implemented.

Legacy-format expansion and universal real-time 4K software transcoding are not
completion requirements. Preserve the focused modern-media scope and fail
explicitly when a requested conversion cannot be supported within policy.
