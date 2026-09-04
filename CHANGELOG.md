# Changelog

## Unreleased

- Added retained, source-built OpenH264 CPU decode and encode sessions that exchange BGRA frames,
  AVCC samples, and avcC decoder configuration on macOS, Windows, Linux, and Linux-based NAS
  targets.
- Added a safe scalar Rust AAC-LC encoder fallback with raw access-unit output, MPEG-4 decoder
  configuration, retained sample clocks, runtime capability reporting, and startup warmup on macOS,
  Windows, Linux, and Linux-based NAS targets.
- Added retained safe-Rust HEVC Main/Main10 software decoding with hvcC packet conversion, 8-bit and
  10-bit YUV420-to-BGRA output, source timing preservation, and executable capability reporting on
  macOS, Windows, Linux, and Linux-based NAS targets.
- Added portable TrueHD decoding and a TrueHD-to-AAC fMP4 bridge with six-channel presentation
  selection and source-anchored audio timing on macOS, Windows, Linux, and Linux-based NAS targets.
- Replaced eager whole-file Matroska transcode indexes with cue-derived plans and bounded
  per-segment packet windows, and normalized packet-copy composition offsets before fMP4 muxing.
- Unified audio selection across playback planning, transcode planning, and Matroska execution so
  copyable tracks remain preferred and portable TrueHD bridges outrank unsupported DTS defaults.
- Added native Ubuntu ARM64 and Windows ARM64 CI checks, portable-codec tests, and release builds.
- Added retained VideoToolbox H.264/HEVC decoder and H.264 encoder sessions plus retained
  AudioToolbox AAC converter sessions, with explicit batch counters and discontinuity resets.
- Added `transcode-fmp4-segments` and `Engine::open_native_fmp4_transcode_session` for host-driven,
  contiguous native HLS transcode windows, including an atomically published media playlist.
- Bounded the BGRA decode/encode pump to 16 packets per batch. The 720p HEVC/AAC real-media
  benchmark reduced maximum resident size from about 1.09 GB to 238 MB while preserving output.
- Normalized target H.264 DTS to presentation order when frame reordering is disabled, fixing
  B-frame sources that previously failed the encoder's monotonic-DTS validation.
- Replaced file-sized heap snapshots with file-backed media views. macOS uses an APFS-compatible
  private copy-on-write clone when available; other filesystems retain a read handle and validate
  source identity around operations without an eager disk copy.
- Added reusable native fMP4 transcode sessions that retain the source snapshot, selected tracks,
  and cue-derived chunk plan while loading bounded packet windows for each segment request.
- Routed HLS, subtitle, transcode, manifest-derived init, and remux outputs through a single
  race-safe immutable publisher; concurrent writers can no longer replace the winning artifact.
- Removed duplicate MP4 source opens from stateless HLS adapters.
- Replaced nearest-neighbor BGRA scaling with averaged half-scaling and bilinear general scaling,
  while borrowing unchanged-size frames without an additional full-frame copy.
- Split MPEG-TS muxing and video scaling into focused implementation modules.
- Made the mounted-library smoke test select a native-HLS-compatible fixture instead of the first
  media file it encounters.
- Migrated Apple video bindings from the legacy `block` graph to maintained `objc2` framework
  crates, eliminating the Rust future-incompatibility warning.
- Added stateful playback sessions with lifecycle metrics for source opens, index parses, source validations, segment requests, and served payload bytes.
- Hardened parser limits, source identity validation, and atomic output publishing.
- Removed the experimental oxideav DTS/AC-3/E-AC-3 bridge path from production routing.
- Added exact signed timing support and normalized MP4/Matroska timing paths.
- Hardened VideoToolbox decode/encode callback error accounting and batched decode draining.
- Added generated fMP4 media-fragment validation for sample counts, sync starts, decode timing, payload offsets, and payload sizes.
- Added multi-platform Rust CI, dependency hygiene checks, cargo-deny scheduling, fuzz target smoke checks, and a private security policy.
- Added structured `EngineError` envelopes with stable codes, operation context, retry advice, and Rust error chaining.
- Added auditable release-build CI scaffolding for macOS, Linux, and Windows binaries.
