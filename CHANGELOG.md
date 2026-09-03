# Changelog

## Unreleased

- Replaced file-sized heap snapshots with file-backed media views. macOS uses an APFS-compatible
  private copy-on-write clone when available; other filesystems retain a read handle and validate
  source identity around operations without an eager disk copy.
- Added reusable native fMP4 transcode sessions that retain the source snapshot, selected tracks,
  packet indexes, and chunk plan across segment requests.
- Routed HLS, subtitle, transcode, manifest-derived init, and remux outputs through a single
  race-safe immutable publisher; concurrent writers can no longer replace the winning artifact.
- Removed duplicate MP4 source opens from stateless HLS adapters.
- Replaced nearest-neighbor BGRA scaling with averaged half-scaling and bilinear general scaling,
  while borrowing unchanged-size frames without an additional full-frame copy.
- Split MPEG-TS muxing and video scaling into focused implementation modules.
- Made the mounted-library smoke test select a native-HLS-compatible fixture instead of the first
  media file it encounters.
- Patched the legacy `block` ABI declaration used by the current Apple media crates, eliminating
  the Rust future-incompatibility warning while migration to `objc2` remains future work.
- Added stateful playback sessions with lifecycle metrics for source opens, index parses, source validations, segment requests, and served payload bytes.
- Hardened parser limits, source identity validation, and atomic output publishing.
- Removed the experimental oxideav DTS/AC-3/E-AC-3 bridge path from production routing.
- Added exact signed timing support and normalized MP4/Matroska timing paths.
- Hardened VideoToolbox decode/encode callback error accounting and batched decode draining.
- Added generated fMP4 media-fragment validation for sample counts, sync starts, decode timing, payload offsets, and payload sizes.
- Added multi-platform Rust CI, dependency hygiene checks, cargo-deny scheduling, fuzz target smoke checks, and a private security policy.
- Added structured `EngineError` envelopes with stable codes, operation context, retry advice, and Rust error chaining.
- Added auditable release-build CI scaffolding for macOS, Linux, and Windows binaries.
