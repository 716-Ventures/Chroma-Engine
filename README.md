# Chroma Engine

Rust media engine for Chroma playback, probing, remuxing, segmentation, and transcoding.

The target is a native media architecture, not an FFmpeg-compatible facade. Chroma Engine owns its API shape, track model, session model, and output strategy; host servers and clients should adapt to the engine when that gives us better speed, stability, or flexibility.

- `probe`: emit a Chroma-native `MediaProbe` manifest with typed tracks, source facts, and capability hints.
- `plan`: produce Chroma playback sessions optimized around reusable packet/decode/encode stages.
- `transcode-plan`: emit the Chroma-native HLS transcode execution contract, including selected tracks, output codecs, executable stages, and missing native capabilities.
- `manifest`: emit a native playback manifest with selected tracks, decoder config, and chunk windows.
- `chunks`: produce keyframe-aligned native chunk windows from compressed packet indexes.
- `codec-config`: emit decoder initialization facts for a compressed MP4/MOV track.
- `h264-nalus`: emit AVC/H.264 NAL-unit layout for a native chunk.
- `h264-annex-b`: write a start-code framed H.264 payload for a native chunk.
- `aac-adts`: write an ADTS-framed AAC payload for a native chunk.
- `extract-chunk`: write a native compressed chunk payload and emit its manifest.
- `remux-mp4`: remux supported sources into an efficient ISO-BMFF output path.
- `encoder-probe`: report platform encoder capabilities.
- `decoder-probe`: report platform decoder backend capabilities and status states, including native hardware surface families for macOS plus designed Linux/Windows targets.
- `warmup`: initialize selected hardware/software backends before the first playback session.

This crate is intentionally not a general FFmpeg clone. It implements the container, codec, muxing, scheduling, and encoding behavior Chroma actually needs, with room to expose new capabilities instead of inheriting old command-line constraints.

## Current Status

The native probe path parses MP4/MOV and Matroska/WebM structure, emits typed tracks, and has integration tests against generated fixtures. macOS VideoToolbox encode/decode probes are executable today. Linux and Windows hardware backend contracts are modeled with explicit capability states, but those backends are not executable yet.

Media inputs are held through file-backed views rather than file-sized heap buffers. Clone-capable
macOS filesystems get a private copy-on-write snapshot; the portable fallback retains the open read
handle and validates source identity around work without eagerly copying the file. A reusable
`HlsVodPlan`, `PlaybackSession`, or `NativeFmp4TranscodeSession` keeps that view and its
parsed/indexed state alive across segment requests. Generated artifacts are published atomically
and treated as immutable; a conflicting concurrent writer receives an error.

Capability status terms are:

- `designed`: modeled, not executable.
- `detected`: hardware/runtime signal found, backend still not executable.
- `available`: backend can initialize locally.
- `verified`: backend completed a real warmup/smoke probe.

The detailed implementation checklist lives in [docs/implementation-checklist.md](docs/implementation-checklist.md). Release and compatibility rules live in [docs/release-policy.md](docs/release-policy.md).

Rust is required to build:

```sh
cargo test
cargo run -- probe /path/to/media.mkv
cargo run -- manifest /path/to/media.mp4 --target-ms 4000
cargo run -- transcode-plan /path/to/media.mkv --target apple-native --force-video-transcode
cargo run -- chunks /path/to/media.mp4 --target-ms 4000
cargo run -- codec-config /path/to/media.mp4 --track a0
cargo run -- h264-nalus /path/to/media.mp4 --chunk-index 0
cargo run -- h264-annex-b /path/to/media.mp4 /tmp/chunk0.h264 --chunk-index 0
cargo run -- aac-adts /path/to/media.mp4 /tmp/chunk0.aac --track a0 --chunk-index 0
cargo run -- extract-chunk /path/to/media.mp4 /tmp/chunk0.bin --chunk-index 0
cargo run -- decoder-probe
```

Quality gates:

```sh
cargo fmt --check
cargo clippy --all-targets --all-features -- -D warnings
cargo test --all-features
RUSTDOCFLAGS="-D warnings" cargo doc --no-deps --all-features
cargo bench --bench engine_hot_paths
cargo machete
cargo +nightly fuzz run containers -- -runs=1
scripts/smoke-real-media.sh
```
