# Apple Framework Binding Migration

Chroma Engine's video path uses the maintained `objc2` Apple framework bindings. Compression and
decompression sessions, CoreMedia buffers and format descriptions, CoreVideo pixel buffers, and
VideoToolbox callbacks now share one explicit `CFRetained` ownership model.

## Target graph

- `objc2-core-foundation`
- `objc2-core-media`
- `objc2-core-video`
- `objc2-video-toolbox`
- `block2`

The legacy `video-toolbox`, `core-media`, `core-video`, `core-foundation`, and
`core-foundation-sys` dependencies have been removed together with the local `block` patch. The
existing higher-level `audiotoolbox` dependency remains isolated to the audio encoder and does not
pull the legacy video or `block` graph back into the build.

## Ownership rules

- Objects returned by Apple `Create` functions enter Rust exactly once through
  `CFRetained::from_raw`.
- Objects returned under get rules are borrowed for the callback duration or retained by the
  generated binding when they must escape the immediate call.
- VideoToolbox output blocks own only thread-safe Rust state and copy sample or pixel bytes before
  returning.
- Retained sessions are explicitly invalidated during teardown; their `CFRetained` owners then
  balance the framework retain.
- Unsafe casts are limited to documented Core Foundation subtype relationships such as
  `CVPixelBuffer`/`CVImageBuffer` and `CMVideoFormatDescription`/`CMFormatDescription`.

## Exit gates

- The full strict test, Clippy, rustdoc, dependency, and future-incompatibility gates pass.
- Retained-session smoke stats still report one decoder, one encoder, and at least one reuse.
- The 720p HEVC/AAC browser smoke reaches ready state 4 and advances playback without an error.
- Maximum resident size does not regress beyond the established approximately 238 MB measurement.
- `cargo tree -i block` reports that `block` is absent.
