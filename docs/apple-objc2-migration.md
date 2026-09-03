# Apple Framework Binding Migration

Chroma Engine currently uses the older `core-foundation`, `core-media`, `core-video`, and
`video-toolbox` wrapper family. The locally patched `block` crate keeps that graph compatible with
the pinned Rust compiler, but the long-term Apple boundary should use the maintained `objc2` family.

## Target graph

- `objc2-core-foundation`
- `objc2-core-media`
- `objc2-core-video`
- `objc2-video-toolbox`
- `objc2-audio-toolbox` when the current higher-level AudioToolbox wrapper no longer covers the
  retained converter contract

All target crates are available at version `0.3.2` and support the repository MSRV. The migration
must remove `video-toolbox`, `core-media`, `core-video`, `core-foundation`,
`core-foundation-sys`, the `[patch.crates-io] block` entry, and `vendor/block` together. A partial
dependency swap would leave two incompatible Core Foundation ownership systems in one callback
path and is not acceptable.

## Migration slices

1. Add a private Apple media adapter module whose Rust-facing inputs and outputs are the existing
   Chroma packet, frame, and session types.
2. Port CoreMedia time, format-description, block-buffer, and sample-buffer construction. Prove
   create/get ownership rules with focused tests before changing VideoToolbox calls.
3. Port retained H.264 compression, its asynchronous callback, encoder properties, pixel-buffer
   creation, and avcC extraction.
4. Port retained H.264/HEVC decompression, output attributes, asynchronous callbacks, and bounded
   BGRA copying.
5. Port capability probes and one-frame warmups, then remove the legacy graph and compatibility
   patch in one commit.

## Exit gates

- The full strict test, Clippy, rustdoc, dependency, and future-incompatibility gates pass.
- Retained-session smoke stats still report one decoder, one encoder, and at least one reuse.
- The 720p HEVC/AAC browser smoke reaches ready state 4 and advances playback without an error.
- Maximum resident size does not regress beyond the current approximately 238 MB measurement.
- `cargo tree -i block` reports that `block` is absent.
