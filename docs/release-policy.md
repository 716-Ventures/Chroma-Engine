# Release and Compatibility Policy

## Capability Status Language

Chroma Engine uses the same status terms in documentation and runtime DTOs:

- `designed`: the engine has a model and roadmap entry, but no executable implementation in the current build.
- `detected`: host hardware or runtime presence was detected, but Chroma Engine has not wired an executable backend for it.
- `available`: Chroma Engine can open or initialize the backend, but the codec/profile has not completed a warmup smoke probe.
- `verified`: Chroma Engine completed a real warmup or smoke probe for the codec/profile on the current host.

Planning may only select `available` or `verified` backends. Linux and Windows hardware paths are currently designed/modeled, not executable.

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

## Client Migration Notes

GenusServer, tvOS, and web-player changes should be listed per release when they depend on a new engine schema, capability state, track-selection behavior, or error code. The engine should keep adapter code outside core parser/muxer modules so client migration does not freeze the Rust API shape.
