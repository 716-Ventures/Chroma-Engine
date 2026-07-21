# Security Policy

## Supported Versions

Chroma Engine is pre-1.0 and private to 716 Ventures. Security fixes are applied to `main` and to any actively deployed release branch.

## Reporting a Vulnerability

Report suspected vulnerabilities privately to the repository owner in the 716 Ventures GitHub organization. Do not open a public issue for parser crashes, crafted media hangs, memory exhaustion, or playback-cache path issues.

Include the smallest reproducer you can share, the affected platform, the Chroma Engine commit SHA, and whether the input came from MP4, Matroska, HLS/fMP4, codec headers, subtitles, or output publishing.

## Response Targets

Critical parser, path traversal, arbitrary write, or crash-on-playback issues should receive initial triage within 2 business days. High-severity denial-of-service and malformed-media issues should receive initial triage within 5 business days.

## Release Hardening

Release artifacts must be built from a clean commit, include checksums, and pass `cargo test --locked --all-targets --all-features`, `cargo clippy --locked --all-targets --all-features -- -D warnings`, `cargo deny check`, rustdoc warnings, and fuzz-target smoke checks before distribution.
