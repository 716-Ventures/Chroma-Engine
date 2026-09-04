# Platform Support

Chroma Engine separates portable container/session code, software codecs, and platform hardware adapters. A successful build does not imply that every transcode stage is executable on that host.

| Target | Build and core tests | Software video codecs | AAC-LC software encode | Hardware video encode/decode | Full native HLS transcode |
| --- | --- | --- | --- | --- | --- |
| macOS x86-64 / ARM64 | Supported | H.264 decode/encode; HEVC Main/Main10 decode; AV1 decode via dav1d | Safe Rust fallback; AudioToolbox preferred | VideoToolbox executable | H.264/HEVC/AV1 with copy-compatible audio or TrueHD-to-AAC |
| Windows x86-64 / ARM64 | Supported | H.264 decode/encode; HEVC Main/Main10 decode; AV1 decode via dav1d | Safe Rust scalar backend | NVENC, QSV, AMF, and DirectX adapters modeled, not executable | H.264/HEVC/AV1 with copy-compatible audio or TrueHD-to-AAC; DTS decode remains a gap |
| Linux x86-64 / ARM64 | Supported | H.264 decode/encode; HEVC Main/Main10 decode; AV1 decode via dav1d | Safe Rust scalar backend | VA-API, NVENC/NVDEC, and QSV adapters modeled, not executable | H.264/HEVC/AV1 with copy-compatible audio or TrueHD-to-AAC; DTS decode remains a gap |
| Linux-based NAS x86-64 / ARM64 | Supported under the Linux contract | H.264 decode/encode; HEVC Main/Main10 decode; AV1 decode via dav1d | Safe Rust scalar backend | Depends on a future adapter and exposed device/runtime | H.264/HEVC/AV1 with copy-compatible audio or TrueHD-to-AAC; DTS decode remains a gap |

The portable H.264 codec is source-built into the Rust binary dependency graph. Building from source therefore requires a working C/C++ toolchain; deployed release binaries do not need an external OpenH264 installation. OpenH264 is BSD-2-Clause licensed. Distributors remain responsible for evaluating codec patent and royalty obligations for their products and territories.

Portable HEVC Main/Main10 decoding is implemented in safe Rust and converts 8-bit or 10-bit YUV420 frames into the engine's BGRA boundary. It has no system-library or native-toolchain dependency. HEVC can carry additional profiles and chroma layouts outside this backend's declared scope; those fail explicitly instead of silently producing incompatible output.

Portable AV1 decoding uses the safe `dav1d-rs` API over VideoLAN's native libdav1d and accepts 8-, 10-, and 12-bit monochrome, 4:2:0, 4:2:2, and 4:4:4 output at the engine's BGRA boundary. Source builds require libdav1d 1.3 or newer plus `pkg-config`: Homebrew `dav1d` on macOS, vcpkg `dav1d` on Windows, or the distribution's dav1d development package on Linux/NAS. CI installs this dependency on every supported hosted architecture. Release packaging must ship the matching dav1d shared library beside Chroma Engine, or link it statically where the target toolchain provides a static archive; users of packaged releases should not need a separate system installation.

The portable AAC-LC encoder is built from safe Rust with architecture-specific SIMD disabled, so it does not require a system codec library or native build toolchain. It emits raw access units and MPEG-4 AudioSpecificConfig for one to six channels. The safe Rust TrueHD decoder selects the format-defined six-channel presentation (or the closest smaller presentation) and feeds this encoder without a system codec dependency. DTS and other compressed formats still require a future portable decode stage.

CI runs the complete check, Clippy, test, documentation, and release-build gates on hosted macOS, Ubuntu, and Windows x86-64-class runners. Additional native Ubuntu ARM64 and Windows ARM64 jobs compile all targets, exercise the CPU codec, and build the release binary. NAS vendors with custom libc versions or older kernels should validate the release binary on the oldest supported appliance image before publishing it.

Hardware capability states stay deliberately conservative: `modeled` and `detected` backends cannot be selected. Only executable or verified implementations may enter a runtime plan.
