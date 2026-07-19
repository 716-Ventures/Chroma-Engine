# Chroma Engine

Rust media engine for replacing the narrow FFmpeg surface used by GenusServer.

The first target is behavioral parity with the current `jellyfin-ffmpeg` process usage:

- `probe`: emit the metadata shape currently supplied by `ffprobe`.
- `hls`: emit fMP4 HLS session output (`master.m3u8`, variant playlists, init segments, media segments).
- `remux-mp4`: remux MKV/MP4-family sources into faststart MP4.
- `encoder-probe`: report platform encoder capabilities.
- `warmup`: initialize the chosen encoder path before the first playback session.

This crate is intentionally not a general FFmpeg clone. It only implements the container, codec, muxing, and encoding behavior needed by ChromaServer playback and conversion.

## Current Status

Initial scaffold. The CLI/API contract is in place, probe path has MP4/Matroska container sniffing, and the detailed implementation checklist lives in [docs/implementation-checklist.md](docs/implementation-checklist.md).

Rust is required to build:

```sh
cargo test
cargo run -- probe /path/to/media.mkv
```
