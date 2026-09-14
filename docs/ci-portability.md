# CI platform builds

The test matrix explicitly selects Rust 1.90.0 (MSRV) and 1.97.1 using
`RUSTUP_TOOLCHAIN`. This overrides the developer toolchain file. Cache keys
separate operating systems, architectures, and compiler versions.

The macOS CI tests and release bundles target macOS 15.5 or later to match the
Homebrew native codec libraries on the runners. This is the binary distribution
baseline, not a claim that every source dependency requires that OS version.
Building for an older OS requires native codecs built for that deployment target.

The root `build.rs` links the Swift runtime and Darwin/CoreAudio overlays needed
by the static AudioToolbox bridge. It embeds `/usr/lib/swift` as a runtime search
path in executables, examples, and tests; a dependency build script's linker
arguments alone do not configure those final targets.

Windows ARM64 retains Media Foundation encoding. The vendored Mediaway encoder
excludes the x86/x64-only NVENC dependency on ARM64 and uses its existing
unsupported-backend implementation so automatic selection can fall through.
See `third_party/mediaway-encoder/CHROMA-PATCHES.md` for the upstream revision.
