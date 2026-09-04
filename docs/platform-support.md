# Platform Support

Chroma Engine separates portable container/session code, software codecs, and platform hardware adapters. A successful build does not imply that every transcode stage is executable on that host.

| Target | Build and core tests | H.264 software decode/encode | Hardware video encode/decode | Full native HLS transcode |
| --- | --- | --- | --- | --- |
| macOS x86-64 / ARM64 | Supported | OpenH264 | VideoToolbox executable | Executable for the documented VideoToolbox + AudioToolbox paths |
| Windows x86-64 / ARM64 | Supported | OpenH264 | NVENC, QSV, AMF, and DirectX adapters modeled, not executable | H.264 input with copy-compatible audio; HEVC and audio transcode are not yet portable |
| Linux x86-64 / ARM64 | Supported | OpenH264 | VA-API, NVENC/NVDEC, and QSV adapters modeled, not executable | H.264 input with copy-compatible audio; HEVC and audio transcode are not yet portable |
| Linux-based NAS x86-64 / ARM64 | Supported under the Linux contract | OpenH264 | Depends on a future adapter and exposed device/runtime | H.264 input with copy-compatible audio; HEVC and audio transcode are not yet portable |

The portable H.264 codec is source-built into the Rust binary dependency graph. Building from source therefore requires a working C/C++ toolchain; deployed release binaries do not need an external OpenH264 installation. OpenH264 is BSD-2-Clause licensed. Distributors remain responsible for evaluating codec patent and royalty obligations for their products and territories.

CI runs the complete check, Clippy, test, documentation, and release-build gates on hosted macOS, Ubuntu, and Windows x86-64-class runners. Additional native Ubuntu ARM64 and Windows ARM64 jobs compile all targets, exercise the CPU codec, and build the release binary. NAS vendors with custom libc versions or older kernels should validate the release binary on the oldest supported appliance image before publishing it.

Hardware capability states stay deliberately conservative: `modeled` and `detected` backends cannot be selected. Only executable or verified implementations may enter a runtime plan.
