# WD EX2 Ultra ARMv7 investigation (preflight pending)

Status: dependency inventory only, 2026-09-23. This is not a build result or a
declaration of support or impossibility. The installed firmware, executable
loader, libc, float ABI, and kernel have not been measured. The owner deferred
device access; no upload, SSH session, or firmware modification was attempted.

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
  host at this checkpoint. Do not choose `armv7-unknown-linux-gnueabihf` until
  the preflight confirms hard-float ABI and compatible libc/loader.
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
  before scanning a real library.

## Next gate

With user-approved read-only access, run `scripts/nas-preflight.sh` and record
sanitized firmware, `uname -m`, ELF interpreter, float ABI, libc, kernel,
available memory, storage, and startup/app mechanism. Choose a matching
toolchain, then attempt a locked minimal build-and-load spike for **both**
engine and server. Isolate any codec failure with its exact command and linker
or loader output. A reduced copy-first media contract requires a separate
scope decision; no codec is silently removed here.
