# Release and Compatibility Policy

## Capability Status Language

Chroma Engine uses the same status terms in documentation and runtime DTOs:

- `modeled`: the engine has a model and roadmap entry, but no executable implementation or local runtime signal in the current build.
- `detected`: host hardware or runtime presence was detected, but Chroma Engine has not wired an executable backend for it.
- `opened`: Chroma Engine opened the backend/device and found the codec/profile, but has not wired or completed a decode/encode smoke probe.
- `executable`: Chroma Engine has an implementation that can run for the codec/profile.
- `verified`: Chroma Engine completed a real warmup or smoke probe for the codec/profile on the current host.

Planning may only select `executable` or `verified` backends. Linux builds with `linux-vaapi` report H.264 and HEVC VA-API as executable after the runtime probe opens a matching VLD context; an Intel-identified iHD/i965 render node may additionally advertise the same retained paths as Quick Sync. Release builds enable that feature. Windows reports D3D11VA H.264 or HEVC decode as executable only after a generated compressed frame completes hardware decode, texture readback, and BGRA conversion; HEVC Main10 additionally requires successful P010 negotiation when a Main10 session opens. The portable H.264 and HEVC decoders remain executable on every supported operating system and provide the universal fallback.

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

Release artifacts must be built from a clean commit and published with checksums. Platform
artifacts include cargo-auditable dependency metadata and the native dav1d runtime required by the
executable. The checksum is for the exact `.tar.gz` or `.zip` archive uploaded by CI.

The CI `release-build` job uses `cargo auditable build --locked --release --bin chroma-engine` on
macOS, Linux, and Windows, then smoke-tests and archives a self-contained runtime bundle with a
SHA-256 checksum. Dedicated native ARM64 jobs compile all targets, test the portable codec, build
with the same cargo-auditable metadata, bundle dav1d, smoke-test the packaged executable, verify
its checksum, and upload the archive on Ubuntu and Windows; Linux ARM64 is the baseline for
ARM-based NAS deployments.

The Apple video boundary uses the maintained `objc2` framework crates and `block2`; the legacy
`block 0.1.6` graph and its local compatibility patch are absent. Keep
`cargo check --future-incompat-report` at zero, and preserve the unified ownership rules documented
in [`apple-objc2-migration.md`](apple-objc2-migration.md) when extending native codec callbacks.

## License and Attribution Gate

Chroma Engine is Apache-2.0. Dependency licenses remain in force; compatibility
does not remove attribution, redistribution, or modification-notice requirements.
The license audit includes the root crate even while `publish = false` is set.

Regenerate the checked-in Rust dependency inventory whenever Cargo.lock, features,
vendored sources, or license configuration changes:

```sh
cargo install cargo-about --locked --version 0.9.2 --features cli
python3 scripts/check-licenses.py --write
cargo deny --all-features check licenses
```

Review the generated changes and any harvesting warnings, not just the exit code.
`about.toml` selects permissive licenses, includes all supported release platforms,
optional features and build dependencies, and excludes dev-only dependencies.
The check script requires Python 3.9+ and cargo-about 0.9.2. Repository-level
licenses omitted by published crates are retrieved at their original package
revision and verified against content hashes, so generation may need network access.

Some upstream projects do not provide a full license file. Their canonical SPDX
text is explicitly labelled in the generated document, along with declared package
authors (not invented copyright holders). Known limitations are recorded by exact
package/version/license in `packaging/license-fallbacks.json`. CI rejects new or
changed fallbacks and stale generated output. A reviewed harvesting limitation is
not a legal clearance; resolve upstream uncertainties before making stronger claims.
See [upstream licensing limitations](../THIRD_PARTY_NOTICES.md#upstream-licensing-limitations).

Run `python3 scripts/test-check-licenses.py` and `python3 scripts/check-licenses.py`
before release. The release jobs depend on this gate. Keep the generator version
pinned and review the inventory diff when changing dependencies or clarifications.

Every binary bundle must include `LICENSE`, `THIRD_PARTY_NOTICES.md`,
`THIRD_PARTY_LICENSES.md`, `license-fallbacks.json`, and `rusty-aac-LICENSE`.
The handwritten notices cover
native libraries outside Cargo (including dav1d), local patch provenance, and
codec-patent considerations. Update native notices against the exact library
revision shipped when changing bundle dependencies. Source distributions must
retain vendored licenses and modification notices as well.

This inventory is not legal or patent clearance. Review applicable codec patent
obligations separately before distribution; in particular, source-built OpenH264
does not automatically receive Cisco's separately distributed binary-module
royalty coverage. See [codec redistribution notes](../THIRD_PARTY_NOTICES.md#codec-patents-and-redistribution).

## Client Migration Notes

GenusServer, tvOS, and web-player changes should be listed per release when they depend on a new engine schema, capability state, track-selection behavior, or error code. The engine should keep adapter code outside core parser/muxer modules so client migration does not freeze the Rust API shape.
