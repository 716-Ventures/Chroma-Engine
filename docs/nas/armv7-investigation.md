# WD EX2 Ultra ARMv7 investigation

Status: dependency inventory plus physical read-only preflight, 2026-09-23.
This is not a build result or a declaration of support or impossibility.
The owner-supplied preflight measured OS 5 firmware `5.33.102`, ARMv7 kernel
`4.14.22-armada-18.09.3`, and glibc 2.31. Executable loader and float ABI
remain unknown because version 1 of the preflight could not extract them. No upload or
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
- No ARMv7 Rust target or cross C toolchain was installed on the development
  host at the initial checkpoint. Do not choose
  `armv7-unknown-linux-gnueabihf` until the version 2 preflight confirms
  hard-float ABI and a compatible loader. Build against glibc 2.31 or older,
  not the existing Debian 12/glibc 2.36 bundle baseline.
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

Obtain sanitized version 2 `scripts/nas-preflight.sh` output to resolve ELF
class, interpreter, and float ABI with BusyBox `od`, then choose a matching
toolchain. Confirm a safe, approved data-volume staging directory and the
actual OS 5 app/startup mechanism before any upload. Attempt a locked minimal
build-and-load spike for **both** engine and server off-device. Isolate any
codec failure with its exact command and linker or loader output. A reduced
copy-first media contract requires a separate scope decision; no codec is
silently removed here.
