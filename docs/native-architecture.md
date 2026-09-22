# Native Chroma Engine Architecture

Chroma Engine is not a command-compatible FFmpeg replacement. It is a media engine with its own model:

- **Probe emits facts, not legacy process output.** `MediaProbe` is the source of truth: source identity, container family, typed tracks, codec families, flags, attachments, and engine capability hints.
- **Playback is session-first.** The core engine models packet sources, decode stages, encode stages, muxers, and native Chroma transports. Legacy transports must not shape the core.
- **Metadata probing must be bounded.** Probing reads container metadata sections from a file-backed source view. It must not scan packet clusters or decode frames unless a caller explicitly asks for deep analysis.
- **Matroska startup uses available indexes.** SeekHead and Cues accelerate access; cue-less sources can require a bounded header/index pass. Retained sessions reuse the cluster index rather than rescanning prefixes per segment.
- **Track IDs identify tracks within a probed source.** Use `v0`, `a0`, and `s0` from the probe rather than container stream indexes. Re-probe and invalidate selections when the source changes; IDs are not permanent cross-file identities.
- **Playback planning is selective by default.** A session plan selects the primary video and primary audio track unless the caller explicitly asks for broader work, such as all audio tracks. This keeps startup fast and avoids waste.
- **Copy paths stay separate from decode paths.** Remuxing and segmenting copy-compatible streams should avoid decoders, frame allocation, and encoder scheduling entirely.
- **Chunks are packet windows.** The engine plans native chunk ranges from compressed packet indexes first; delivery protocols can adapt after that.
- **Extraction is byte-range native.** Copy paths read planned compressed packet ranges through bounded positional I/O without opening video decoders or encoders.
- **Source reads are bounded and validated.** The source layer retains a read handle, owns bounded metadata, and performs positional packet reads. It does not map media or create a whole-file heap copy. Source identity checks detect replacement or mutation around work; host storage isolation and worker deadlines remain necessary.
- **Generated artifacts are immutable publications.** Segment, init, playlist, subtitle, and remux writers complete a unique same-directory temporary file before atomically linking it into place. Concurrent writers may accept identical bytes but cannot replace a different completed artifact.
- **HLS is an adapter over native packet plans.** The engine plans keyframe windows once, then emits playlists, individual segments, or contiguous read-ahead batches from that plan. It should not rebuild container indexes for every future segment.
- **Multi-output work should share stages.** Multiple audio/subtitle outputs should not duplicate video demux/decode/encode work.
- **Compatibility is outside the core.** If a deployment later needs a legacy transport, that layer must adapt from the native session model. It should not dictate the engine core or public CLI.
- **The crate root is the supported API.** Parser, muxer, codec, and source modules are implementation details. Server/client integrations should import re-exported root symbols so internals can be split or replaced without changing host code.
- **Performance work must be measurable.** Hot paths should get a benchmark before or with major rewrites. The current baseline lives in the `engine_hot_paths` Criterion bench and covers probe, planning, packet windows, and subtitle segmentation.
- **Transcode work is session-first.** `NativeFmp4TranscodeSession` retains its source handle, selected tracks, keyframe plan, and selected native codec state. Each request parses only its bounded packet window; sequential requests reuse codec state, while discontinuous requests reset the codec pair. Separate CLI invocations cannot share this state. Rust hosts should retain sessions and use runtime-aware constructors with shared admission; non-Rust hosts can amortize setup with contiguous CLI windows.
- **Portable fallback is a real backend, not a capability claim.** Source-built OpenH264 provides retained CPU H.264 decode and encode, while Rust backends provide retained HEVC Main/Main10, TrueHD, and DTS Core decode plus AAC-LC/AC-3/E-AC-3 encode on macOS, Windows, Linux, and Linux-based NAS systems. They use the same BGRA, length-prefixed video, PCM, and compressed-audio contracts as hardware paths, while platform hardware remains preferred where executable. TrueHD and standard big-endian DTS Core carried by DTS/DTS-HD packets can bridge to the portable audio encoders.
- **Linux GPU integrations remain runtime optional.** VA-API and NVIDIA NVENC/NVDEC backends are built into Linux releases without link-time driver dependencies. Intel Quick Sync uses the Intel iHD/i965 VA-API driver, binds sessions to the vendor-verified render node, and remains separately identifiable as `qsv` without requiring a oneVPL dispatcher at link time. Startup probes execute a small real encode before advertising a hardware encoder; decode probes require a codec context on the selected device, and NVDEC queries the active device for its codec and bit-depth capability. The audited NVDEC bridge selects NV12 for 8-bit streams and P016 for Main10, copies padded device planes without truncation, and converts them through the checked BGRA boundary. Fallback to portable codecs is subject to the resource policy; small-NAS policy disables software video fallback. This keeps headless NAS and non-GPU Linux installations on the same release artifact.
- **Windows native encoder selection is executable and measured.** Retained H.264/HEVC Main sessions accept the same BGRA boundary as other encoders, perform a checked NV12 upload, and try hardware NVENC, hardware Intel Quick Sync, then Media Foundation. A backend enters the advertised encoder profile only after a real frame produces valid elementary-stream output and decoder configuration; portable H.264 remains the last fallback.
- **Windows capability output names executable APIs, not vendor aliases.** AMD hardware is reached through the same hardware-only Media Foundation/D3D11 transforms as other Windows adapters, so Chroma does not add AMF SDK surface area merely to relabel an equivalent path. Likewise, D3D12VA, DXVA2, QSV, AMF, and NVDEC decoder enum values remain reserved for wire compatibility but are omitted from runtime matrices until they represent distinct executable implementations.
- **Windows H.264/HEVC Main/Main10 hardware decode crosses a checked D3D11 boundary.** The retained Media Foundation session requests a hardware-only decoder on a Chroma-owned D3D11 video device. HEVC hvcC parameter sets and length-prefixed access units are converted to Annex B before submission; Main requests NV12 and Main10 requests P010. Each returned texture is validated against that negotiated format, copied to a same-device staging resource, mapped with its reported row pitch, converted to BGRA through the shared checked converter, and matched back to source timing with internal tokens. A generated Main frame must survive encode, decode, readback, and conversion before D3D11VA is advertised; Main10 is accepted only when its session negotiates P010, and the portable decoder remains the fallback. Chroma vendors the narrow Mediaway decoder facade needed for this path at v0.1.4 so the crate retains its Rust 1.90 minimum.
- **Raw-frame queues are bounded.** The VideoToolbox path decodes and encodes small packet batches inside each segment instead of retaining an entire keyframe span as BGRA. On the 720p HEVC/AAC smoke fixture this reduced observed maximum resident size from roughly 1.09 GB to 238 MB.
- **Real media smoke tests are separate from fixtures.** Scripts may inspect mounted local media, but committed tests use generated or sanitized fixtures so the repo stays small and deterministic.

## Current Integration Boundary

The original native-engine implementation sequence is complete: probe, playback manifests,
bounded packet readers, reusable fMP4 muxing, retained codec sessions, and executable platform
backends now exist. GenusServer's native-engine integration supersedes the original
FFmpeg migration experiments. Hosts still own authenticated delivery, cache lifecycle,
shared admission, worker deadlines, and deployed binary updates. Verify the actual
server/client versions together; engine CI does not certify a running installation.
Unsupported-capability errors must remain explicit, without legacy-tool fallback.

## Engineering Baseline

Chroma Engine targets Rust 2024 and pins the local toolchain. The required gates are:

```sh
cargo fmt --check
cargo clippy --all-targets --all-features -- -D warnings
cargo test --all-features
RUSTDOCFLAGS="-D warnings" cargo doc --no-deps --all-features
cargo bench --bench engine_hot_paths
scripts/smoke-real-media.sh /path/to/media.mkv
scripts/smoke-native-transcode-session.sh /path/to/fixture.mkv
scripts/benchmark-native-transcode.sh /path/to/fixture.mkv
```

The default release profile uses thin LTO, one codegen unit, stripped symbols, and abort-on-panic. NAS packaging applies distinct low-build-memory overrides recorded in `BUILD-BASELINE.txt`; do not assume every artifact uses identical settings. Profiling builds retain debug symbols.

## Web Player Test Milestone

The initial short-window web-player milestone has been demonstrated. It is not the
current completion gate: [production qualification](status.md) requires cold starts,
resumes, track changes, full-length A/V validation, and representative browser/tvOS
and NAS hardware tests. Use the [player test contract](web-player-test-contract.md).

The retained transcode path has also crossed this milestone for Matroska: a real 720p HEVC/AAC
source was decoded and re-encoded to an H.264/AAC fMP4 HLS window, then played through hls.js in
Chromium at 1280x720 beyond 4.3 seconds with no media error.
