# WD EX2 Ultra ARMv7 investigation

Status: off-device ARMv7 cross-build plus physical read-only preflight,
2026-09-23. This is not a successful device load or a declaration of support.
The owner-supplied preflight measured OS 5 firmware `5.33.102`, ARMv7 kernel
`4.14.22-armada-18.09.3`, and glibc 2.31. The firmware lacks `readelf`,
`file`, `od`, and `hexdump`, so both preflight versions could not read the
executable ABI locally. A read-only copy of the NAS `getconf` executable was
inspected on the development Mac: ELF32 little-endian ARM EABI5, hard-float
flag `0x400`, dynamic loader `/lib/ld-linux-armhf.so.3`. No Chroma upload or
firmware modification was attempted.

## Build boundary

- Engine declares Rust 1.90 and edition 2024. Its unconditional media dependencies
  include `dav1d`/`dav1d-sys`, `openh264`/`openh264-sys2`, `rust_h265`, AAC,
  Opus, AC-3/DTS, and TrueHD crates. The dav1d library and any C/C++
  build steps must be produced for the measured WD ABI, not copied from the
  Debian 12 ARM64 or x86-64 bundle.
- The server additionally depends on Tokio, SQLx/SQLite, TLS and native build
  steps. The complete server binary and SPA assets must be built off-device.
- `cargo tree --locked -p chroma-engine --target armv7-unknown-linux-gnueabihf
  --depth 2` resolves the Rust graph. This does **not** prove that the C code
  compiles, links, loads, or runs on the appliance.
- The Rust 1.97.1 ARMv7 hard-float standard library and Zig 0.16 toolchain
  cross-built dav1d 1.5.3 as a static library and linked locked release Engine
  and Server binaries for `armv7-unknown-linux-gnueabihf.2.31`. Both are
  ELF32 little-endian ARM EABI5 hard-float with the measured NAS loader
  `/lib/ld-linux-armhf.so.3` and flag `0x05000400`. Their highest required
  glibc symbol version is 2.30, below the NAS's measured glibc 2.31. This is
  link/ABI evidence only; no ARM binary has executed on the NAS.
- The Server build is not yet cleanly reproducible: a newly generated macOS
  host-side SQLx macro dylib failed to load with a `mis-aligned LINKEDIT string
  pool` error. Reusing the same revision's previously built, loadable macro
  dylib allowed the ARMv7 Server link to finish. Resolve this host build issue
  before treating the result as a releasable package.
- The current `linux-vaapi` feature is not a Rockchip or ARMADA hardware-video
  backend. The safe initial claim, if the full build and device tests pass, is
  copy/remux plus separately measured audio conversion, not video conversion.

## Offset and memory checks

- `source::probe_reader` uses `u64` file lengths/offsets and bounded positional
  reads. A sparse MP4 test now places an extended-size payload above 4 GiB and
  confirms discovery of tail metadata without copying the payload.
- Container parsers use `usize` for in-memory windows. Examples include MP4
  atom size conversion and Matroska block-prefix offsets. These conversions
  require 32-bit-target execution tests; a 64-bit test alone cannot certify
  ARMv7 large-file handling. Rejecting an oversized in-memory atom is safer
  than truncating it, but usability of real >4 GiB sources remains unproven.
- Engine `small-nas` reserves 192 MiB per session and 384 MiB aggregate; these
  are reservations, not process RSS or a server-wide cap. Test stack usage,
  allocator behavior, SQLite/cache I/O, and whole-server memory on the WD
  before scanning a real library. Preflight captured only 306 MiB available RAM
  and 1.4 GiB free swap. Swap is not a substitute for RAM or a performance
  qualification. The root filesystem had only 32 MiB free, so a future pilot
  must use an approved data-volume location, not root.

## Next gate

The off-device binaries are in a 9.3 MiB pilot archive at
`target/wd-ex2-ultra-armv7-pilot.tar.gz`. A separate
`target/wd-os5/MyCloudEX2Ultra_chromaserver_0.1.2.bin` was assembled for the
WD dashboard using the EX2 Ultra OS 5 package format and lifecycle hooks.
Its header, signature, tar payload, ARM ABI, and scripts passed local checks
against an owner-supplied known-good package. The owner installed earlier
versions and the WD dashboard reported the app On, but the Configure path
returned WD's HTTP 404 and the Chroma API port refused connections. That is
not a successful Engine or Server load. Version 0.1.2 removes a hard-coded
volume path and adds a minimal Configure startup-status page; it has not yet
been tried on the NAS. Attempt a minimal load spike for **both** Engine and Server
on the device; isolate any codec failure with its exact command and loader
output. Then measure startup, `/ready`, RSS, swap, and small authorized
fixture playback. A reduced
copy-first media contract requires a separate scope decision; no codec is
silently removed here.
