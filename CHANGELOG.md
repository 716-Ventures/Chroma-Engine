# Changelog

## Unreleased

- Added checksummed, self-contained release archives for macOS, Linux, Windows, Linux ARM64/NAS,
  and Windows ARM64. ARM64 CI now bundles dav1d, smoke-tests the packaged executable, and uploads
  the resulting archive; rustdoc warnings are explicitly denied in the platform test matrix.
- Added retained Windows HEVC Main hardware decoding through the existing Media Foundation/D3D11
  path. Chroma converts hvcC parameter sets and length-prefixed samples to Annex B, reads back
  NV12 textures with checked row pitches, and advertises the backend only after a generated HEVC
  frame completes encode, hardware decode, and BGRA conversion. The narrowly patched decoder
  facade is vendored to preserve Rust 1.90 support.
- Extended that Windows HEVC decoder to Main10 with explicit P010 transform negotiation, native
  texture-format reporting, checked padded-plane readback, and the shared 10-bit-to-BGRA boundary.
- Removed non-executable Windows AMF, D3D12VA, DXVA2, QSV, and NVDEC decoder aliases plus the
  modeled AMF encoder candidates. AMD hardware remains covered by the executable hardware-only
  Media Foundation/D3D11 path, and reserved public enum values are retained for compatibility.
- Added retained Linux Intel Quick Sync H.264/HEVC decode and Main encode sessions through the
  runtime-loaded iHD/i965 VA-API driver. QSV selection now requires an Intel vendor render node and
  real codec probes, remains distinct from generic VA-API, and adds no oneVPL link dependency.
- Added checked NVDEC HEVC Main10 output by vendoring the audited MIT-licensed bridge, selecting
  NVIDIA P016 surfaces for high-bit-depth streams, preserving MSB-aligned 16-bit planes through
  CUDA readback, and converting them to BGRA with explicit geometry validation.

- Added retained Windows hardware H.264 decoding through Media Foundation and D3D11, including
  a hardware-only transform, NV12 texture staging readback, checked padded-plane conversion to
  BGRA, source timing restoration, runtime decode smoke verification, and portable fallback.
- Added retained Windows H.264 and HEVC Main native encoding with BGRA-to-NV12 upload and
  runtime selection across hardware NVENC, hardware Intel Quick Sync, and Media Foundation. The
  sessions preserve source timing, emit fMP4-ready length-prefixed samples and avcC/hvcC
  configuration, require a real encode before advertising availability, and retain the portable
  H.264 fallback.
- Added retained Linux NVIDIA H.264 and HEVC Main encoding through runtime-loaded CUDA/NVENC
  libraries, with checked BGRA-to-I420 conversion, AVCC/hvcC output, timing preservation, real
  encode capability smoke probes, and automatic fallback when NVIDIA hardware or drivers are
  unavailable.
- Added retained Linux NVIDIA NVDEC H.264 and HEVC Main/Main10 decoding with runtime codec-capability
  queries, length-prefixed-to-Annex-B packet conversion, decode-order-safe timing, checked
  planar 8/10-bit-to-BGRA output, and portable fallback.
- Replaced placeholder Windows DirectX decode detection with real Media Foundation hardware-MFT
  enumeration and activation probes for H.264 and HEVC decode/encode transforms.
- Added a retained portable DTS Core decoder from a pinned `oxideav-dts` revision, with DTS-HD
  core extraction, checked channel ordering, continuous sample-clock timing, DTS-to-AAC fMP4
  routing, and real-media latency validation.
- Added retained, safe-Rust AC-3 and E-AC-3 encoders with continuous sample clocks, checked PCM
  input, dac3/dec3 extraction, partial-frame flushing, and executable platform warmups.
- Added an opt-in, release-enabled Linux VA-API H.264 decoder using retained stateless packet
  submission, page-aligned NV12 user-pointer surfaces, synchronized BGRA output, and automatic
  OpenH264 fallback when libva or a suitable DRM render node is unavailable.
- Extended the retained Linux VA-API decoder to HEVC Main/Main10 with direct P010 surfaces and the
  same safe-Rust fallback used by portable and NAS deployments.
- Added retained Linux VA-API H.264 encoding from the BGRA pipeline boundary, including checked
  NV12 upload, Annex-B-to-AVCC conversion, avcC extraction, runtime smoke probing, and OpenH264
  fallback.
- Added retained Linux VA-API HEVC Main encoding with the same runtime-only libva contract,
  checked NV12 upload, length-prefixed sample conversion, hvcC extraction, and smoke probing.
- Extended the runtime-loaded Linux VA-API probe through synchronized NV12/P010 image derivation,
  plane-layout validation, CPU buffer mapping, and balanced image cleanup without link-time GPU
  dependencies.
- Added stride-aware, bounds-checked NV12/P010-to-BGRA conversion paths for mapped Linux hardware
  decode surfaces, including odd display dimensions and padded Y/UV planes.
- Added headerless, runtime-loaded Linux VA-API probing that opens DRM render nodes, reports the
  driver vendor, and verifies H.264/HEVC VLD profiles by creating real configs, decode surfaces,
  and contexts without making libva a link-time dependency.
- Added retained, source-built OpenH264 CPU decode and encode sessions that exchange BGRA frames,
  AVCC samples, and avcC decoder configuration on macOS, Windows, Linux, and Linux-based NAS
  targets.
- Added a safe scalar Rust AAC-LC encoder fallback with raw access-unit output, MPEG-4 decoder
  configuration, retained sample clocks, runtime capability reporting, and startup warmup on macOS,
  Windows, Linux, and Linux-based NAS targets.
- Added retained safe-Rust HEVC Main/Main10 software decoding with hvcC packet conversion, 8-bit and
  10-bit YUV420-to-BGRA output, source timing preservation, and executable capability reporting on
  macOS, Windows, Linux, and Linux-based NAS targets.
- Added portable TrueHD decoding and a TrueHD-to-AAC fMP4 bridge with six-channel presentation
  selection and source-anchored audio timing on macOS, Windows, Linux, and Linux-based NAS targets.
- Replaced eager whole-file Matroska transcode indexes with cue-derived plans and bounded
  per-segment packet windows, and normalized packet-copy composition offsets before fMP4 muxing.
- Unified audio selection across playback planning, transcode planning, and Matroska execution so
  copyable tracks remain preferred and portable TrueHD/DTS bridges remain executable when no
  copyable alternate exists.
- Added native Ubuntu ARM64 and Windows ARM64 CI checks, portable-codec tests, and release builds.
- Added retained VideoToolbox H.264/HEVC decoder and H.264 encoder sessions plus retained
  AudioToolbox AAC converter sessions, with explicit batch counters and discontinuity resets.
- Added `transcode-fmp4-segments` and `Engine::open_native_fmp4_transcode_session` for host-driven,
  contiguous native HLS transcode windows, including an atomically published media playlist.
- Bounded the BGRA decode/encode pump to 16 packets per batch. The 720p HEVC/AAC real-media
  benchmark reduced maximum resident size from about 1.09 GB to 238 MB while preserving output.
- Normalized target H.264 DTS to presentation order when frame reordering is disabled, fixing
  B-frame sources that previously failed the encoder's monotonic-DTS validation.
- Replaced file-sized heap snapshots with file-backed media views. macOS uses an APFS-compatible
  private copy-on-write clone when available; other filesystems retain a read handle and validate
  source identity around operations without an eager disk copy.
- Added reusable native fMP4 transcode sessions that retain the source snapshot, selected tracks,
  and cue-derived chunk plan while loading bounded packet windows for each segment request.
- Routed HLS, subtitle, transcode, manifest-derived init, and remux outputs through a single
  race-safe immutable publisher; concurrent writers can no longer replace the winning artifact.
- Removed duplicate MP4 source opens from stateless HLS adapters.
- Replaced nearest-neighbor BGRA scaling with averaged half-scaling and bilinear general scaling,
  while borrowing unchanged-size frames without an additional full-frame copy.
- Split MPEG-TS muxing and video scaling into focused implementation modules.
- Made the mounted-library smoke test select a native-HLS-compatible fixture instead of the first
  media file it encounters.
- Migrated Apple video bindings from the legacy `block` graph to maintained `objc2` framework
  crates, eliminating the Rust future-incompatibility warning.
- Added stateful playback sessions with lifecycle metrics for source opens, index parses, source validations, segment requests, and served payload bytes.
- Hardened parser limits, source identity validation, and atomic output publishing.
- Removed the experimental oxideav DTS/AC-3/E-AC-3 bridge path from production routing.
- Added exact signed timing support and normalized MP4/Matroska timing paths.
- Hardened VideoToolbox decode/encode callback error accounting and batched decode draining.
- Added generated fMP4 media-fragment validation for sample counts, sync starts, decode timing, payload offsets, and payload sizes.
- Added multi-platform Rust CI, dependency hygiene checks, cargo-deny scheduling, fuzz target smoke checks, and a private security policy.
- Added structured `EngineError` envelopes with stable codes, operation context, retry advice, and Rust error chaining.
- Added auditable release-build CI scaffolding for macOS, Linux, and Windows binaries.
