# WD EX2 Ultra ARMv7 investigation

Status: dependency inventory plus physical read-only preflight, 2026-09-23.
This is not a build result or a declaration of support or impossibility.
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
  Opus, AC-3/DTS, and TrueHD crates. The dav1d shared library and any C/C++
  build steps must be produced for the measured WD ABI, not copied from the
  Debian 12 ARM64 or x86-64 bundle.
- The server additionally depends on Tokio, SQLx/SQLite, TLS and native build
  steps. The complete server binary and SPA assets must be built off-device.
- `cargo tree --locked -p chroma-engine --target armv7-unknown-linux-gnueabihf
  --depth 2` resolves the Rust graph. This does **not** prove that the C code
  compiles, links, loads, or runs on the appliance.
- The Rust 1.97.1 ARMv7 hard-float standard library is installed locally, but
  no matching C cross toolchain or glibc sysroot is installed. A locked
  cross-target `cargo check` was interrupted before reaching native codec
  build steps because it progressed too slowly to yield a useful result; it
  is not a build failure. The measured ELF ABI supports
  `armv7-unknown-linux-gnueabihf` as the target candidate. Build against
  glibc 2.31 or older, not the existing Debian 12/glibc 2.36 bundle baseline.
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

Prepare a matching cross toolchain and sysroot, then attempt a locked minimal
build of both the engine and server off-device. Confirm a safe, approved
data-volume staging directory and the
actual OS 5 app/startup mechanism before any upload. Attempt a locked minimal
load spike for **both** engine and server on the device. Isolate any
codec failure with its exact command and linker or loader output. A reduced
copy-first media contract requires a separate scope decision; no codec is
silently removed here.
