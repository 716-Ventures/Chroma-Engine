# Native Chroma Engine Architecture

Chroma Engine is not a command-compatible FFmpeg replacement. It is a media engine with its own model:

- **Probe emits facts, not legacy process output.** `MediaProbe` is the source of truth: source identity, container family, typed tracks, codec families, flags, attachments, and engine capability hints.
- **Playback is session-first.** The core engine models packet sources, decode stages, encode stages, muxers, and native Chroma transports. Legacy transports must not shape the core.
- **Metadata probing must be bounded.** Probing reads container metadata sections from a file-backed source view. It must not scan packet clusters or decode frames unless a caller explicitly asks for deep analysis.
- **Matroska startup uses SeekHead and Cues.** Large Matroska files must not be planned by walking every cluster; Cues provide the bounded keyframe map for native chunk startup.
- **Track IDs are stable semantic handles.** Video, audio, and subtitle tracks get IDs such as `v0`, `a0`, and `s0`; server/client APIs should use those IDs rather than container stream indexes.
- **Playback planning is selective by default.** A session plan selects the primary video and primary audio track unless the caller explicitly asks for broader work, such as all audio tracks. This keeps startup fast and avoids waste.
- **Copy paths stay separate from decode paths.** Remuxing and segmenting copy-compatible streams should avoid decoders, frame allocation, and encoder scheduling entirely.
- **Chunks are packet windows.** The engine plans native chunk ranges from compressed packet indexes first; delivery protocols can adapt after that.
- **Extraction is byte-range native.** Copy-compatible chunk emission should copy planned packet byte ranges directly from mapped source data before any decode, encode, or mux work is considered.
- **Source views must not scale heap use with media size.** On macOS the source layer first attempts a sealed same-filesystem copy-on-write clone. When cloning is unavailable, it retains the original read handle and maps it on demand without an eager disk copy. Sessions validate path identity before serving work; deployments must not modify an actively leased inode in place.
- **Generated artifacts are immutable publications.** Segment, init, playlist, subtitle, and remux writers complete a unique same-directory temporary file before atomically linking it into place. Concurrent writers may accept identical bytes but cannot replace a different completed artifact.
- **HLS is an adapter over native packet plans.** The engine plans keyframe windows once, then emits playlists, individual segments, or contiguous read-ahead batches from that plan. It should not rebuild container indexes for every future segment.
- **Multi-output work should share stages.** Multiple audio/subtitle outputs should not duplicate video demux/decode/encode work.
- **Compatibility is outside the core.** If a deployment later needs a legacy transport, that layer must adapt from the native session model. It should not dictate the engine core or public CLI.
- **The crate root is the supported API.** Parser, muxer, codec, and source modules are implementation details. Server/client integrations should import re-exported root symbols so internals can be split or replaced without changing host code.
- **Performance work must be measurable.** Hot paths should get a benchmark before or with major rewrites. The current baseline lives in the `engine_hot_paths` Criterion bench and covers probe, planning, packet windows, and subtitle segmentation.
- **Transcode work is session-first.** `NativeFmp4TranscodeSession` retains its source snapshot, selected tracks, cue-derived keyframe plan, and VideoToolbox decoder/encoder objects. Each request parses only its bounded packet window; sequential requests reuse codec state, while discontinuous requests reset the codec pair. Stateless CLI helpers are adapters over a one-operation session; servers should open sessions through `Engine` and retain them across requests.
- **Portable fallback is a real backend, not a capability claim.** Source-built OpenH264 provides retained CPU H.264 decode and encode, while Rust backends provide retained HEVC Main/Main10, TrueHD, and DTS Core decode plus AAC-LC/AC-3/E-AC-3 encode on macOS, Windows, Linux, and Linux-based NAS systems. They use the same BGRA, length-prefixed video, PCM, and compressed-audio contracts as hardware paths, while platform hardware remains preferred where executable. TrueHD and standard big-endian DTS Core carried by DTS/DTS-HD packets can bridge to the portable audio encoders.
- **Linux GPU integrations remain runtime optional.** VA-API and NVIDIA NVENC/NVDEC backends are built into Linux releases without link-time driver dependencies. Startup probes execute a small real encode before advertising a hardware encoder; NVDEC queries the active device for its codec capability. Session construction falls back to portable codecs when the device, codec profile, or runtime libraries are unavailable. NVDEC is currently restricted to checked 8-bit YUV420 output, leaving HEVC Main10 on VA-API or the CPU decoder. This keeps headless NAS and non-GPU Linux installations on the same release artifact.
- **Windows native encoder selection is executable and measured.** Retained H.264/HEVC Main sessions accept the same BGRA boundary as other encoders, perform a checked NV12 upload, and try hardware NVENC, hardware Intel Quick Sync, then Media Foundation. A backend enters the advertised encoder profile only after a real frame produces valid elementary-stream output and decoder configuration; portable H.264 remains the last fallback.
- **Raw-frame queues are bounded.** The VideoToolbox path decodes and encodes small packet batches inside each segment instead of retaining an entire keyframe span as BGRA. On the 720p HEVC/AAC smoke fixture this reduced observed maximum resident size from roughly 1.09 GB to 238 MB.
- **Real media smoke tests are separate from fixtures.** Scripts may inspect mounted local media, but committed tests use generated or sanitized fixtures so the repo stays small and deterministic.

Near-term implementation order:

1. Deepen native probe until it covers real library files across MP4/MOV and Matroska/WebM.
2. Deepen the Chroma playback session manifest with concrete packet/decode/encode stage contracts.
3. Build zero-copy packet readers for MP4 and Matroska.
4. Build ISO-BMFF/fMP4 writers as reusable muxers.
5. Add platform encode/decode backends only where stream copy cannot satisfy the requested session.
6. Add any legacy transport adapters only after the native Chroma transport is stable.

## Engineering Baseline

Chroma Engine targets Rust 2024 and pins the local toolchain. The required gates are:

```sh
cargo fmt --check
cargo clippy --all-targets --all-features -- -D warnings
cargo test --all-features
RUSTDOCFLAGS="-D warnings" cargo doc --no-deps --all-features
cargo bench --bench engine_hot_paths
scripts/smoke-real-media.sh
scripts/smoke-native-transcode-session.sh
scripts/benchmark-native-transcode.sh
```

Release binaries use the repository release profile: thin LTO, one codegen unit, stripped symbols, and abort-on-panic. Profiling builds inherit release settings but keep debug symbols.

## Web Player Test Milestone

Every build-out pass should check whether Chroma Engine has reached the point where GenusServer can be updated for a real web-player test. Do not move this milestone forward just to force an early integration. It is ready only when Chroma Engine can produce a browser-playable or WebCodecs-ready native stream for at least one real MP4 from `/Volumes/Movies` or `/Volumes/TVShows` without FFmpeg, including manifest shape, codec configuration, chunk URLs, timing, and clear errors.

The retained transcode path has also crossed this milestone for Matroska: a real 720p HEVC/AAC
source was decoded and re-encoded to an H.264/AAC fMP4 HLS window, then played through hls.js in
Chromium at 1280x720 beyond 4.3 seconds with no media error.
