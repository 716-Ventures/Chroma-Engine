# Chroma Engine

Rust media engine for Chroma playback, probing, remuxing, segmentation, and transcoding.

New integrations should start with the [adoption and integration guide](docs/adopting-chroma-engine.md),
covering the engine's philosophy, use cases, public APIs, Rust and CLI examples, and deployment responsibilities.

The target is a native media architecture, not an FFmpeg-compatible facade. Chroma Engine owns its API shape, track model, session model, and output strategy; host servers and clients should adapt to the engine when that gives us better speed, stability, or flexibility.

- `probe`: emit a Chroma-native `MediaProbe` manifest with typed tracks, source facts, and capability hints.
- `cache-probe`: check that an output directory supports the engine's hard-link/no-replace publication strategy.
- `plan`: produce Chroma playback sessions optimized around reusable packet/decode/encode stages.
- `transcode-plan`: emit the Chroma-native HLS transcode execution contract, including selected tracks, output codecs, executable stages, and missing native capabilities.
- `manifest`: emit a native playback manifest with selected tracks, decoder config, and chunk windows.
- `chunks`: produce keyframe-aligned native chunk windows from compressed packet indexes.
- `codec-config`: emit decoder initialization facts for a compressed MP4/MOV track.
- `h264-nalus`: emit AVC/H.264 NAL-unit layout for a native chunk.
- `h264-annex-b`: write a start-code framed H.264 payload for a native chunk.
- `hevc-annex-b`: write a start-code framed HEVC payload for a native chunk.
- `aac-adts`: write an ADTS-framed AAC payload for a native chunk.
- `extract-chunk`: write a native compressed chunk payload and emit its manifest.
- `hls-plan`, `hls`, and `hls-fmp4`: plan or package supported sources into native HLS output.
- `remux-mp4`: remux supported sources into an efficient ISO-BMFF output path.
- `encoder-probe`: report platform encoder capabilities.
- `decoder-probe`: report executable and runtime-verified decoder backends on macOS, Linux, and Windows, while keeping non-selectable capability states explicit.
- `transcode-fmp4-segments`: emit a contiguous MP4/MOV- or Matroska/WebM-to-fMP4 window while retaining native decoder and encoder sessions.
- `warmup`: initialize selected hardware/software backends before the first playback session.

This crate is intentionally not a general FFmpeg clone. It implements the container, codec, muxing, scheduling, and encoding behavior Chroma actually needs, with room to expose new capabilities instead of inheriting old command-line constraints.

Run `chroma-engine --help` for the complete command list and `chroma-engine <command> --help`
for arguments. The runtime does not require FFmpeg or ffprobe, and unsupported media is not
silently delegated to either tool.

## Supported Media

The source-container boundary is MP4/M4V/MOV and MKV/WebM. Video support focuses on H.264,
HEVC Main/Main10, and AV1. Compatible compressed streams are copied whenever possible;
AV1 uses `dav1d-rs` for native decode to H.264. Supported audio includes copy-compatible
AAC, AC-3, E-AC-3, FLAC, ALAC, and MP3 paths, plus Opus, DTS/DTS-HD core, and TrueHD
bridges to AAC. Exact container/codec/profile and target combinations still matter.

Legacy source containers and video codecs are outside the playback profile. Supported text
subtitles can be converted to sidecars; bitmap subtitle burn-in is not implemented.
Known HDR-to-SDR transcodes are rejected until verified tone mapping is available. Compatible
HDR packet copy remains supported; dropping bit depth is not treated as tone mapping.
See [modern media support](docs/modern-media-support.md) for the detailed boundary.

## Current Status

Portable container/session code and software codec implementations target macOS, Windows,
Linux, and Linux-based NAS systems on x86-64 and ARM64. Retained OpenH264 handles H.264
decode/encode, `rust_h265` handles HEVC decode, dav1d handles AV1 decode, and retained native
audio paths provide the supported bridges. AAC-LC uses a vendored `rusty_aac` streaming
extension; attribution and patch scope are in [third-party notices](THIRD_PARTY_NOTICES.md).

Hardware backends are selected by runtime capabilities, not operating-system names alone:

- macOS: VideoToolbox; matching decode/encode sessions pass retained Core Video buffers
  without CPU pixel readback/upload.
- Windows: Media Foundation/D3D11 decode and supported hardware encoder paths. The shared
  pipeline still uses checked CPU pixel readback/conversion.
- Linux/NAS: optional `linux-vaapi` and `linux-nvidia` features. Intel VA-API devices are
  exposed as Quick Sync only after the required vendor and codec checks succeed. libva is
  loaded dynamically; portable fallback is subject to the resource policy.

Inputs use bounded owned metadata and positional packet reads, **not memory mappings or
whole-file heap copies**. Source identity is checked around work. `HlsVodPlan`,
`PlaybackSession`, and `NativeFmp4TranscodeSession` retain parsed/indexed state across requests;
Matroska video/audio windows share a cached cluster index. Parser work, nesting, tracks, and
expanded indexes have resource ceilings.

Sequential native transcodes retain video codecs, audio decoders, PCM residuals, and AAC
encoder state. Discontinuous seeks and failed renders reset affected state; eligible runtime
hardware failures get at most one policy-permitted software retry. CPU scaler storage and
the H.264 encoder's intermediate YUV buffer are reused. Generated artifacts are published
without replacing conflicting existing output.

CPU AAC `encode` is a streaming push and may return no frames while lookahead fills. Call
`finish` once at end of stream, not after each segment. Priming is represented in the fMP4
init edit list. Independent player/audio-quality and long-run timing qualification remain
necessary.

The [stability audit execution ledger](docs/audits/2026-09-stability-performance-plan.md#execution-ledger)
separates implemented fixes from remaining work. Linux/Windows native-surface pipelines,
end-to-end planar processing, and fleet/long-run qualification are not complete. A successful
build or small synthetic benchmark does not establish stable 4K transcoding on every NAS.

Capability status terms are:

- `modeled`: a roadmap/backend model exists, with no local runtime signal.
- `detected`: hardware/runtime signal found, backend still not executable.
- `opened`: the backend/device and required codec profile opened successfully, without an executable codec smoke test.
- `executable`: an implementation can run for the codec/profile.
- `verified`: backend completed a real warmup/smoke probe.

## Host Integration and Resource Limits

For in-process use, share one `Arc<EngineRuntime>` across sessions and use the
`open_with_runtime` APIs with a `WorkControl`. Default entrypoints use a process-local shared
runtime. Set `CHROMA_RESOURCE_POLICY` before first use: `small-nas` selects a copy-first policy,
or supply a validated JSON policy. The small-NAS profile reserves 192 MiB per session,
384 MiB aggregate, permits two sessions, uses one software codec thread, and disables software
video fallback. Reservations are not measured RSS limits; hosts must leave headroom for codecs,
drivers, and OS resources and enforce process/container limits separately.

`WorkerSupervisor` provides shared admission and hard kill/reap deadlines for CLI workers.
An embedding host must wire it in explicitly; it is not automatically applied to arbitrary
CLI subprocesses. Cooperative cancellation cannot interrupt a blocked native call or filesystem
read. Separate server processes also need cross-process admission coordination.

Use `cache-probe` before choosing output storage. Cache publication requires hard links with
no replacement; prefer a compatible local cache when an SMB/NFS share cannot provide them.
See [resource policy and integration](docs/resource-policy.md) for APIs, exact ceilings,
cancellation, audio compatibility changes, and telemetry.

## Build and Run

Source builds require Rust **1.90 or newer** (edition 2024), a native C/C++ build toolchain,
and **libdav1d 1.3+** discoverable through `pkg-config`. This is a native dependency even
though AV1 is accessed through a Rust wrapper. Runtime bundles must include the matching
dav1d shared library. Optional Linux GPU features need additional build dependencies; see
[platform support](docs/platform-support.md).

Shell examples use POSIX syntax. On Windows, use Git Bash or adapt environment-variable
assignments and paths for PowerShell. Output directories and media paths below are placeholders.

```sh
cargo build --locked --release
cargo run --locked -- probe /path/to/media.mkv
cargo run --locked -- manifest /path/to/media.mp4 --target-ms 4000
cargo run --locked -- transcode-plan /path/to/media.mkv --target apple-native
cargo run --locked -- chunks /path/to/media.mp4 --target-ms 4000
cargo run --locked -- codec-config /path/to/media.mp4 --track a0
cargo run --locked -- decoder-probe
cargo run --locked -- cache-probe /path/to/chroma-cache
cargo run --locked --release -- transcode-fmp4-segments /path/to/media.mkv /path/to/chroma-window \
  --start-index 0 --count 2 --video-mode copy
```

Use `--video-mode h264` when native video encoding is needed and the source/resource policy
permits it. For a shell-based small-NAS copy run:

```sh
CHROMA_RESOURCE_POLICY=small-nas cargo run --locked --release -- \
  transcode-fmp4-segments /path/to/media.mkv /path/to/chroma-window --count 2 --video-mode copy
```

Linux distributors can enable H.264/HEVC VA-API decode and encode with
`cargo build --locked --release --features linux-vaapi`.
That feature needs Clang and libva development headers at build time, but the resulting binary
loads libva dynamically and still starts on hosts without libva.

Portable Linux and Linux-based NAS bundles can be built for either architecture with BuildKit:

```bash
docker buildx build --platform linux/amd64 --output type=local,dest=dist/linux-x64 \
  -f packaging/Dockerfile.linux .
docker buildx build --platform linux/arm64 --output type=local,dest=dist/linux-arm64 \
  -f packaging/Dockerfile.linux .
```

Each output contains `chroma-engine`, colocated `libdav1d.so.7`, license notices, and
`BUILD-BASELINE.txt`. The Dockerfile targets Debian 12/glibc 2.36, enables VA-API, and runs
generated-media checks in a minimal runtime before export. This bundle does not enable
NVIDIA and is not a musl/static or older-glibc compatibility promise. Actual appliance and
driver qualification is still required. See [release policy](docs/release-policy.md).

## Tests and Measurements

```sh
cargo fmt --all --check
cargo clippy --locked --all-targets --all-features -- -D warnings
cargo test --locked --all-features
cargo test --locked --release --test stability
RUSTDOCFLAGS="-D warnings" cargo doc --locked --no-deps --all-features
cargo bench --locked --bench engine_hot_paths
cargo machete
```

`cargo machete` is an optional separately installed development tool. Fuzzing additionally
requires nightly Rust and `cargo-fuzz`; targets are `containers`, `codecs`, `subtitles`, and
`fmp4`. For example:

```sh
cargo +nightly fuzz run containers -- -max_total_time=60 -timeout=10 -rss_limit_mb=512
```

Generate an original H.264/Opus fixture without FFmpeg, using a new output filename:

```sh
cargo run --locked --example make-test-media -- /path/to/fixture.mkv 6
```

Media-dependent scripts need a suitable input file. H.264 transcode tests require a source
that does not need unsupported HDR tone mapping:

```sh
scripts/smoke-real-media.sh /path/to/media.mkv
scripts/smoke-native-transcode-session.sh /path/to/fixture.mkv
scripts/benchmark-native-transcode.sh /path/to/fixture.mkv
python3 scripts/benchmark-session.py --engine target/release/chroma-engine \
  --media /path/to/media.mkv --start-index 0 --count 2 --repeats 5 --video-mode copy \
  --report /path/to/new-report.json
```

The portable Python runner records fresh-process window timings and available worker metrics.
Optional `psutil` enables sampled RSS/CPU/I/O; POSIX also records per-child high-water RSS.
Caches are uncontrolled, and tiny fixtures do not establish movie startup latency, NAS memory
ceilings, or tvOS playback quality. See the [audit ledger](docs/audits/2026-09-stability-performance-plan.md)
for measured evidence and remaining acceptance work, and the
[implementation checklist](docs/implementation-checklist.md) for the broader feature inventory.

## License

Chroma Engine is licensed under the [Apache License, Version 2.0](LICENSE)
(`Apache-2.0`). Third-party dependencies and vendored components retain their own
licenses; see [third-party notices](THIRD_PARTY_NOTICES.md) and the license files
in `third_party/`.
