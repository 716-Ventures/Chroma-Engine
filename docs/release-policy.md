# Release and Compatibility Policy

## Capability Status Language

Chroma Engine uses the same status terms in documentation and runtime DTOs:

- `modeled`: the engine has a model and roadmap entry, but no executable implementation or local runtime signal in the current build.
- `detected`: host hardware or runtime presence was detected, but Chroma Engine has not wired an executable backend for it.
- `opened`: Chroma Engine opened the backend/device and found the codec/profile, but has not wired or completed a decode/encode smoke probe.
- `executable`: Chroma Engine has an implementation that can run for the codec/profile.
- `verified`: Chroma Engine completed a real warmup or smoke probe for the codec/profile on the current host.

Planning may only select `executable` or `verified` backends. Linux builds with `linux-vaapi` report H.264 and HEVC VA-API as executable after the runtime probe opens a matching VLD context; release builds enable that feature. Windows hardware paths are not executable yet. The portable H.264 and HEVC decoders remain executable on every supported operating system and provide the Linux/NAS fallback.

## Schema Compatibility

Wire contracts must include an explicit schema version when they are consumed by GenusServer, tvOS, or the web player. Consumers must ignore unknown additive fields and reject unknown required enum values with a typed `EngineErrorCode`.

Breaking schema changes require:

- a new schema version,
- migration notes for GenusServer, tvOS, and web clients,
- compatibility tests for the previous supported schema when the old consumer remains deployed.

## Rust API Compatibility

Before a 1.0 release, public Rust APIs may evolve, but changes must be documented in `CHANGELOG.md`. Public enums intended to evolve should be `#[non_exhaustive]`, and new validated domain types should prefer constructors/builders over all-public invalid states.

After a baseline release exists, API changes should run `cargo-semver-checks` against that baseline before release.

## Release Gates

A release candidate must pass:

- `cargo fmt --check`
- `cargo clippy --locked --all-targets --all-features -- -D warnings`
- `cargo test --locked --all-targets --all-features`
- `RUSTDOCFLAGS="-D warnings" cargo doc --locked --no-deps --all-features`
- `cargo deny check`
- `cargo machete`
- fuzz target smoke checks for containers, codecs, subtitles, and fMP4

Release artifacts must be built from a clean commit and published with checksums. Platform artifacts should include dependency metadata or an SBOM once release packaging is added.

The CI `release-build` job uses `cargo auditable build --locked --release --bin chroma-engine` on macOS, Linux, and Windows so release binaries carry dependency metadata before packaging is formalized. Dedicated native ARM64 jobs compile, test the portable codec, and produce release binaries on Ubuntu and Windows; Linux ARM64 is the baseline for ARM-based NAS deployments.

The Apple video boundary uses the maintained `objc2` framework crates and `block2`; the legacy
`block 0.1.6` graph and its local compatibility patch are absent. Keep
`cargo check --future-incompat-report` at zero, and preserve the unified ownership rules documented
in [`apple-objc2-migration.md`](apple-objc2-migration.md) when extending native codec callbacks.

## Client Migration Notes

GenusServer, tvOS, and web-player changes should be listed per release when they depend on a new engine schema, capability state, track-selection behavior, or error code. The engine should keep adapter code outside core parser/muxer modules so client migration does not freeze the Rust API shape.
