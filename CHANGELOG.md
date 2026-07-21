# Changelog

## Unreleased

- Added stateful playback sessions with lifecycle metrics for source opens, index parses, source validations, segment requests, and served payload bytes.
- Hardened parser limits, source identity validation, and atomic output publishing.
- Removed the experimental oxideav DTS/AC-3/E-AC-3 bridge path from production routing.
- Added exact signed timing support and normalized MP4/Matroska timing paths.
- Hardened VideoToolbox decode/encode callback error accounting and batched decode draining.
- Added generated fMP4 media-fragment validation for sample counts, sync starts, decode timing, payload offsets, and payload sizes.
- Added multi-platform Rust CI, dependency hygiene checks, cargo-deny scheduling, fuzz target smoke checks, and a private security policy.
- Added structured `EngineError` envelopes with stable codes, operation context, retry advice, and Rust error chaining.
- Added auditable release-build CI scaffolding for macOS, Linux, and Windows binaries.
