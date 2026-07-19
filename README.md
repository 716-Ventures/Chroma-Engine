# Chroma Engine

Rust media engine for Chroma playback, probing, remuxing, segmentation, and transcoding.

The target is a native media architecture, not an FFmpeg-compatible facade. Chroma Engine owns its API shape, track model, session model, and output strategy; host servers and clients should adapt to the engine when that gives us better speed, stability, or flexibility.

- `probe`: emit a Chroma-native `MediaProbe` manifest with typed tracks, source facts, and capability hints.
- `plan`: produce Chroma playback sessions optimized around reusable packet/decode/encode stages.
- `manifest`: emit a native playback manifest with selected tracks, decoder config, and chunk windows.
- `chunks`: produce keyframe-aligned native chunk windows from compressed packet indexes.
- `codec-config`: emit decoder initialization facts for a compressed MP4/MOV track.
- `extract-chunk`: write a native compressed chunk payload and emit its manifest.
- `remux-mp4`: remux supported sources into an efficient ISO-BMFF output path.
- `encoder-probe`: report platform encoder capabilities.
- `warmup`: initialize selected hardware/software backends before the first playback session.

This crate is intentionally not a general FFmpeg clone. It implements the container, codec, muxing, scheduling, and encoding behavior Chroma actually needs, with room to expose new capabilities instead of inheriting old command-line constraints.

## Current Status

The native probe path parses MP4/MOV and Matroska/WebM structure, emits typed tracks, and has integration tests against generated fixtures. The detailed implementation checklist lives in [docs/implementation-checklist.md](docs/implementation-checklist.md).

Rust is required to build:

```sh
cargo test
cargo run -- probe /path/to/media.mkv
cargo run -- manifest /path/to/media.mp4 --target-ms 4000
cargo run -- chunks /path/to/media.mp4 --target-ms 4000
cargo run -- codec-config /path/to/media.mp4 --track a0
cargo run -- extract-chunk /path/to/media.mp4 /tmp/chunk0.bin --chunk-index 0
```
