# NAS acceleration decision record

Status: pre-measurement design, 2026-09-23. No NAS acceleration is certified.
The N5 fixture/timing protocol is frozen before optimization. No hardware
backend is added solely because a CPU family or vendor name suggests a GPU.

## Existing and missing paths

- Linux VA-API is a build feature and runtime capability probe. Intel model
  names alone do not establish a working render node, driver, codec/profile,
  permissions, or sustained conversion rate. Start the image with **no** GPU
  mapping; qualify a narrowly granted render node separately.
- ARM64 Rockchip or other NAS media blocks are not VA-API by default. The
  current engine has no RKMPP/Rockchip backend. ARM64 copy/remux and audio
  capabilities must be reported independently of hardware video conversion.
- Small-NAS policy disables software video fallback; an unsupported video
  conversion must reject promptly. This is a capability limitation, not a
  reason to revive FFmpeg or claim acceleration.

## Gate before any new backend

Record on physical hardware: device nodes and permissions, kernel/firmware,
vendor API and userspace library versions, license/redistribution obligations,
codec/profile/color signaling support, output ownership and zero-copy behavior,
cancel/kill/reap semantics, memory ceilings, a capability-probe false-positive
test, and N5 before/after sustained playback results. Review ABI stability and
cross-build reproducibility. Obtain an explicit backend scope decision before
implementation; a host-specific success does not certify all ARM64 appliances.
