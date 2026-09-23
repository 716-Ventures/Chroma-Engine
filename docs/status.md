# Current implementation and release status

Reviewed 2026-09-22 against `5db50813b9ddf8c19c416ebc1fe43845d48e279e`.
This is a dated evidence snapshot; consult subsequent CI runs and release notes
before assuming it describes a newer revision.

## Assessment

Chroma Engine is a working pre-1.0 engine entering production qualification.
The core modern-media replacement functions are substantially implemented:
probing, packet access, remuxing, HLS/fMP4 packaging, native conversion, and
resource controls. It does not require or silently invoke FFmpeg/ffprobe.

Implementation is not a guarantee of trouble-free playback on every browser,
tvOS device, GPU, or NAS. Do not calculate readiness from checked feature counts.

## Verified automated evidence

All 15 jobs in [CI run 35770722559](https://github.com/716-Ventures/Chroma-Engine/actions/runs/35770722559)
passed for the reviewed commit:

- macOS ARM64 and Windows/Linux x86-64 test matrices using Rust 1.90 and 1.97.1.
- Windows/Linux ARM64 portable jobs and platform release-bundle builds.
- Security/dependency hygiene and bounded fuzz smoke tests.
- x86-64 and ARM64 Debian 12/glibc 2.36 NAS bundle jobs, including synthetic
  execution under a 512 MiB container limit.

Intel macOS jobs were removed; x86-64 source support is not current hosted CI
or release-artifact coverage. Hosted GPU availability also does not qualify all
hardware backends. Synthetic memory-limit tests do not establish full-length
4K playback performance on actual appliances.

The reviewed commit fixes DTS output scaling and stops treating all high-bit-depth
HEVC streams as HDR10. Those corrections still need independent player/audio/video
validation across the representative media matrix.

## Known functional limits

- HDR-to-SDR tone mapping is not implemented; compatible HDR packet copy remains available.
- Bitmap subtitle burn-in is not implemented; supported text subtitles use sidecars/renditions.
- Container, profile, channel-layout, and target restrictions remain explicit; see the [media profile](modern-media-support.md).
- Linux/Windows native-surface optimization and end-to-end portable planar processing remain open.
- NAS software-video fallback can be disabled by policy. Unsupported or oversized work must fail rather than exceed the host budget.

## Next milestone: production qualification

For appliance installation and qualification work across Engine and Server, follow
the [NAS deployment execution plan](nas-deployment-execution-plan.md).

Use neutral fixture IDs and test the actual server-to-browser/tvOS path. Establish
device-specific startup, resume, stall, A/V drift, memory, and concurrency targets;
then measure cold starts, distant seeks, track changes, full-length playback,
network-storage failures, disk exhaustion, cancellation, and hardware loss.
Run multi-day soak tests on representative NAS and GPU hardware. Optimize measured
bottlenecks rather than expanding the codec list.

Engine worker supervision and resource admission must be wired into the host;
neither is automatically applied to arbitrary subprocesses. A new source commit
does not prove that the running server or client uses the new engine binary.

## Release state

At review time, `v0.1.0` exists as a draft, not a published release. Successful
build artifacts do not publish it automatically. Review the exact target commit,
checksums, dependency notices, documented upstream licensing limitations, and
[release gates](release-policy.md) before publication. Codec patents and SDK terms
remain separate from Apache-2.0 software licensing.

See the [current checklist](implementation-checklist.md), [platform guide](platform-support.md),
[adoption guide](adopting-chroma-engine.md), and [historical audit ledger](audits/2026-09-stability-performance-plan.md).
