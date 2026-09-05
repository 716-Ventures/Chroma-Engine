# Implementation Checklist

## Contract

- [x] Create Rust repo and library/CLI skeleton.
- [x] Move CLI implementation behind the library entrypoint so the binary is only a launcher.
- [x] Narrow public API to an explicit crate-root facade; keep parser/muxer implementation modules crate-private.
- [x] Define native `MediaProbe` JSON with typed tracks, source facts, and capability hints.
- [x] Define command surface: `probe`, `plan`, `manifest`, `chunks`, `codec-config`, `h264-nalus`, `h264-annex-b`, `hevc-annex-b`, `aac-adts`, `extract-chunk`, `extract-window`, `hls`, `hls-plan`, `hls-segment`, `hls-segments`, `transcode-fmp4-segments`, `remux-mp4`, `encoder-probe`, `warmup`.
- [x] Define native playback `plan` command with target-specific pipeline stages.
- [x] Add real-library smoke script for mounted media under `/Volumes/Movies`, `/Volumes/TV Shows`, or `/Volumes/TVShows`.
- [x] Add sanitized real-library probe fixtures from `/Volumes/Movies` and `/Volumes/TVShows`.
- [x] Add Chroma-native snapshot tests for representative MP4/MKV/HDR/audio/subtitle combinations.

## Engineering Quality

- [x] Migrate crate to Rust 2024 edition.
- [x] Pin Rust toolchain and formatting policy.
- [x] Enforce `cargo fmt --check`.
- [x] Enforce `cargo clippy --all-targets --all-features -- -D warnings`.
- [x] Enforce `cargo test --all-features`.
- [x] Enforce `RUSTDOCFLAGS="-D warnings" cargo doc --no-deps --all-features`.
- [x] Replace file-sized heap snapshots with a file-backed source wrapper. The only mapping `unsafe` block is documented inside that wrapper; macOS uses a private clone where supported, while sessions validate identity before work on the portable retained-handle path.
- [x] Add release profile policy for optimized binaries.
- [x] Add Criterion benchmark harness for initial hot paths.
- [x] Add dependency audit policy once `cargo-deny` is installed or CI can install it reproducibly.
- [x] Add missing-docs policy for the public facade.
- [x] Split oversized implementation modules: HLS, MP4, Matroska, CLI.
- [x] Split HLS MPEG-TS muxing and transcode scaling into focused modules.
- [x] Replace broad `anyhow` use in library-facing APIs with typed engine errors.
- [x] Add fuzz/property tests for EBML, MP4 atoms, packet range math, and timestamp repair.

## Probe

- [x] File existence and basic container sniffing.
- [x] MP4/MOV box parser: `ftyp`, `moov`, `trak`, `mdia`, `minf`, `stbl`, `stsd`, `mvhd`, `tkhd`, `mdhd`.
- [x] Matroska/WebM EBML parser: Segment Info, Tracks, Attachments.
- [x] Matroska/WebM EBML parser: Chapters.
- [x] Extract video fields: codec, dimensions.
- [x] Extract video fields: frame rate, bitrate, HDR/DV hints.
- [x] Extract video fields: pixel format.
- [x] Extract Matroska audio fields: codec, channels, language/title, default/forced disposition.
- [x] Extract MP4 audio fields: codec, channels, sample rate.
- [x] Extract MP4 audio title/default/forced disposition.
- [x] Extract audio fields: bitrate and MP4 E-AC-3 Atmos/JOC hints.
- [x] Extract Matroska E-AC-3 Atmos/JOC hints.
- [x] Extract subtitles: text vs bitmap classification, language/title, default/forced disposition.
- [x] Extract chapters.
- [x] Extract duration.
- [x] Keep source bytes behind `MediaSource` and validate source identity before segment reads. The wrapper uses a sealed copy-on-write clone where supported and a retained read handle elsewhere, avoiding a file-sized heap allocation.
- [x] Define Chroma-native error taxonomy.

## Remux MP4

- [x] Zero-copy packet path for supported stream-copy remux.
- [x] Matroska block reader.
- [x] MP4 writer with faststart `moov` before media data.
- [x] H.264 AVC configuration parsing and Annex-B conversion.
- [x] HEVC configuration conversion.
- [x] Extract MP4/MOV H.264, HEVC, and AAC codec configuration records.
- [x] AAC/AC-3/E-AC-3/MP3/FLAC/ALAC sample entries.
- [x] Text subtitle to `mov_text`.
- [x] Metadata and chapter copy.

## Native Playback

- [x] Define initial Chroma-native MP4/MOV playback manifest with selected tracks, decoder config, and chunk windows.
- [x] Emit all indexed audio tracks in Chroma-native playback manifests when audio is included.
- [x] Include audio metadata in Chroma-native playback manifests: language, title, channels, sample rate, default, forced.
- [x] Add initial Chroma-native playback plan: selected tracks, shared demux, copy/decode/encode, mux, transport adapters.
- [x] Default playback planning selects primary video/audio and excludes target-unusable bitmap subtitles for browser/Apple targets.
- [x] Remove legacy transport assumptions from the public core command/module surface.
- [x] Define native compressed packet references and keyframe-aligned chunk planning.
- [x] Add MP4/MOV sample-table chunk planning without per-packet materialization.
- [x] Add Matroska/WebM Cues-based chunk planning with bounded cluster fallback.
- [x] Emit native MP4/MOV compressed chunk payloads from packet byte ranges.
- [x] Extract MP4/MOV decoder initialization facts for compressed tracks.
- [x] Emit sample-level timing and payload layout for native MP4/MOV chunks.
- [x] Parse AVC/H.264 NAL-unit layout from native MP4/MOV chunks.
- [x] Convert native MP4/MOV AVC chunks to Annex-B H.264 without decode.
- [x] Wrap native MP4/MOV AAC chunks as ADTS without decode.
- [x] Emit stream chunks through reusable packet/decode/encode stages.
- [x] Publish HLS, subtitle, remux, and transcode artifacts through the race-safe immutable output publisher.
- [x] H.264/AAC native stream-copy chunks.
- [x] HEVC native stream-copy chunks.
- [x] AC-3/E-AC-3/MP3/FLAC/ALAC native copy paths.
- [x] Keyframe-aligned chunk planning.
- [x] Keyframe-aligned MP4/MOV chunk emission.
- [x] Input-side seek anchoring and copy-path coarse seek behavior.
- [x] WebVTT parsing/rendering foundation.
- [x] Single-process WebVTT sidecar generation for all selected text subtitles.
- [x] Multi-audio output without duplicating video encode.

## Native HLS

- [x] Build keyframe-aligned HLS VOD plans from MP4/MOV packet tables.
- [x] Build keyframe-aligned HLS VOD plans from Matroska/WebM Cues with bounded fallback.
- [x] Write MPEG-TS HLS segments for H.264/AAC without decode.
- [x] Write MPEG-TS HLS segments for HEVC with AC-3/E-AC-3 without decode.
- [x] Emit HLS master and media playlists from the native segment plan.
- [x] Estimate HLS variant bandwidth from peak packet-window bitrate instead of a fixed constant.
- [x] Support demand-driven single segment emission.
- [x] Support contiguous batch segment emission for read-ahead.
- [x] Support explicit native HLS audio track selection by stable `aN` track ID.
- [x] Prefer native-HLS streamable MKV audio over unsupported default/first tracks when no explicit audio track is requested. Verified with Gremlins 4K MKV: native HLS plan selects `v0` + `a1` AC-3 instead of the first DTS track.
- [x] Sanitize per-stream output timestamps to keep DTS monotonic.
- [x] Collapse pathological sub-second segment windows.
- [x] Signal AC-3/E-AC-3 in PMT descriptors for more reliable player detection.
- [x] Add native TS inspection tests for PMT signaling, PCR placement, and continuity counters without external FFmpeg tools.
- [x] Add benchmark coverage for probe, playback planning, packet-window planning, and subtitle segmentation.
- [x] Add native fMP4 inspection tests that validate fragment timing without external FFmpeg tools.
- [x] Add multi-audio HLS outputs without duplicating video work.
- [x] Add subtitle sidecar/rendition generation from native text subtitle parsing.

## Transcode

- [x] Platform capability probe without shelling out to FFmpeg.
- [x] Sample-accurate audio output clock for native decode/encode paths. `AudioSampleClock` anchors on source PTS only at startup/discontinuity and advances encoded output timestamps by sample count to avoid container timebase jitter.
- [x] Encoded audio output contract. `EncodedAudioStream` and `EncodedAudioFrame` define the stable codec/timing/payload boundary native backends must emit before fMP4/HLS muxing.
- [x] fMP4 adapter for encoded audio frames. Native audio backend output can be packed into fMP4 fragment tracks with contiguous sample-clock validation before player-visible muxing.
- [x] macOS VideoToolbox H.264 encode. Native BGRA frames encode through VideoToolbox to length-prefixed AVC access units with avcC decoder config and stable frame timing.
- [x] macOS VideoToolbox HEVC encode. Native BGRA frames encode through VideoToolbox to length-prefixed HEVC access units with hvcC decoder config and stable frame timing.
- [x] AAC audio encode. macOS AudioToolbox remains preferred, with a safe scalar Rust fallback for Windows, Linux, macOS, and Linux-based NAS hosts. Both encode interleaved i16 PCM to raw AAC-LC access units with MPEG-4 AudioSpecificConfig and sample-clocked `EncodedAudioFrame` payloads.
- [x] AC-3/E-AC-3 bridge encode. Retained safe-Rust sessions accept interleaved i16 PCM, preserve a continuous sample clock across batches, emit parse-validated syncframes plus dac3/dec3 decoder configuration, and explicitly flush a padded final partial syncframe.
- [x] Portable CPU H.264 encoder. A retained OpenH264 session accepts BGRA frames and emits AVCC access units plus avcC decoder configuration on macOS, Windows, Linux, and Linux-based NAS hosts.
- [x] Portable CPU H.264 decoder. A retained OpenH264 session converts AVCC packets into timestamped BGRA frames and can feed the same bounded decode/scale/encode pipeline as VideoToolbox.
- [x] Portable HEVC Main/Main10 video decoder. A retained safe-Rust session accepts hvcC/length-prefixed packets, decodes 8-bit and 10-bit YUV420, and emits BGRA through the same bounded pipeline on macOS, Windows, Linux, and Linux-based NAS hosts.
- [x] Portable AV1 video decoder. A retained `dav1d-rs` session accepts low-overhead AV1 packets, converts 8/10/12-bit planar output to BGRA, and feeds the bounded H.264 transcode pipeline. Redistributable synthetic 8-bit and 10-bit fixtures gate one-shot and retained multi-batch decode/encode behavior.
- [x] Portable TrueHD audio decoder. Safe Rust TrueHD decoding selects the format-defined six-channel-or-smaller presentation and feeds the portable AAC/fMP4 bridge on macOS, Windows, Linux, and Linux-based NAS hosts.
- [ ] Portable DTS audio decoder. DTS remains a deterministic missing capability; the rejected oxideav implementation must not return without real-media latency evidence.
- [ ] Linux executable VAAPI/NVENC/QSV path. H.264 and HEVC Main/Main10 VA-API decode plus H.264 and HEVC Main VA-API encode are executable behind the release-enabled `linux-vaapi` feature. Retained stateless sessions use page-aligned NV12/P010 user-pointer surfaces, preserve timing, produce fMP4-ready length-prefixed samples and avcC/hvcC, and fall back to portable codecs. NVENC/NVDEC and QSV remain.
- [ ] Windows executable NVENC/QSV/AMF/D3D path. Capability models exist, but no Windows decode/encode backend is executable or verified yet.
- [x] Warm encoder session initialization. AAC AudioToolbox, portable CPU AAC/AC-3/E-AC-3, VideoToolbox H.264/HEVC, OpenH264, and executable VA-API backends run tiny real encodes before playback so startup failures surface before the first segment request.
- [x] Chroma-native HLS transcode execution planning. `transcode-plan` now emits selected tracks, output codecs, stage readiness, and missing native capabilities so hosts can ask the engine what it can run without inheriting FFmpeg command/API shape.
- [x] Native video decode frame/pump contract. Chroma Engine now has zero-copy compressed packet input, decoded BGRA frame output, and a bounded send/receive/drain action sequence for future HEVC/H.264/AV1 decoder backends.
- [x] Native video decoder backend matrix. `decoder-probe` now reports H.264/HEVC decode backends and BGRA output format separately from encoder capabilities, with macOS VideoToolbox availability backed by native hardware decode support checks.
- [x] Native VideoToolbox decoder session probes. H.264 and HEVC probes parse avcC/hvcC decoder config into parameter sets, create CoreMedia format descriptions, and open real VTDecompressionSession instances before the engine trusts a source for native decode.
- [x] Stateful native fMP4 transcode session. Source snapshot, track selection, and the cue-derived keyframe plan are retained across segment requests; packet parsing is bounded to each requested window instead of retaining a whole-file index. Lifecycle stats expose source opens, plan parses, requests, and validations.
- [x] Retained native codec sessions. Sequential fMP4 requests reuse one VideoToolbox decoder and H.264 encoder; random seeks reset both, and session stats expose creations, reuse, and resets.
- [x] Bounded decode/encode pump. Long keyframe spans are processed in small packet batches rather than accumulating every decoded BGRA frame before encoding.
- [x] Cross-platform hardware decode contract. Decoder probes now model macOS VideoToolbox, Linux VAAPI/NVDEC/QSV, and Windows D3D11VA/D3D12VA/DXVA2/QSV/AMF/NVDEC paths with explicit `designed`, `detected`, `available`, and `verified` capability states instead of marking modeled placeholders as executable.
- [x] macOS VideoToolbox BGRA decode primitive. Chroma Engine can now accept H.264/HEVC compressed packet batches plus avcC/hvcC config and return owned decoded BGRA frames through a Rust-first API.
- [x] Native compressed video decode backend for HEVC/H.264 Matroska sources that need target-native fMP4 HLS output. VideoToolbox feeds a bounded BGRA-to-H.264 pump owned by the retained transcode session.
- [x] Native TrueHD decode bridge for MKV audio tracks that have no AAC/AC-3/E-AC-3 alternate. The portable decoder feeds AAC-LC access units into fMP4 while preserving the source sample-clock anchor.
- [x] Keep audio routing executable across planning and segment generation. Copy-compatible tracks rank first, portable TrueHD-to-AAC ranks ahead of unsupported defaults, and explicit track selection remains authoritative. Verified without an audio override on the DTS-first Gremlins remux: native execution selected `a1` AC-3 and emitted a valid HEVC/AC-3 fMP4 segment in 0.97 seconds at about 11.4 MB maximum RSS.
- [ ] Native DTS decode bridge for MKV audio tracks that have no copy-compatible alternate. Follow the FFmpeg send/receive/drain state-machine shape and AetherEngine's copy-first/bridge-only policy, but keep the API Chroma-native.

## Integration

- [x] Transcode real 4K HEVC/TrueHD 7.1 Atmos Matroska segments through the portable TrueHD-to-AAC bridge. Segment zero completed in 0.29 seconds at about 25.9 MB maximum RSS; a random segment at index 100 also completed successfully using cue seeking and bounded major-sync preroll. Fixture: `/Volumes/Movies/Caught Stealing (2025)/Caught.Stealing.2025.2160p.UHD.Blu-ray.REMUX.HEVC.Atmos.TrueHD7.1-HDH.mkv`.
- [x] Transcode a real 720p HEVC/AAC Matroska source to H.264/AAC fMP4 with retained native codec sessions and play the generated two-segment HLS window in Chromium with hls.js. Verified 1280x720 decode, ready state 4, playback beyond 4.3 seconds, and no media error using `scripts/smoke-native-transcode-session.sh` output.
- [x] **Milestone: ready to update GenusServer and test in the web player.** Verified with `/Volumes/TVShows/Big Fat Quiz/Season 2026/Big.Fat.Quiz.S2026E01.The.Big.Fat.Quiz.of.Telly.1080p.ALL4.WEB-DL.AAC2.0.H.264-RAWR.mp4`: Chroma Engine emits an fMP4 HLS playlist with `#EXT-X-MAP`, `init.mp4`, and `.m4s` media fragments without FFmpeg. Test artifact: `/tmp/chroma-engine-milestone-aac-20260720-111252`. `/Volumes/Movies/42.mp4` is now correctly rejected for native browser HLS because its `mp4a` object type is DTS (`mp4a.a9`), not AAC.
- [x] Define minimum web-player test contract: Chroma-native manifest, selected tracks, codec config, chunk URLs, timestamp model, and error shape. See `docs/web-player-test-contract.md`.
- [x] Emit browser-playable or WebCodecs-ready H.264/AAC output for one real MP4. Covered by `scripts/smoke-web-player-output.sh`.
- [x] Add a GenusServer env-flagged route that serves the Chroma-native manifest and chunks. GenusServer now proxies `/v1/chroma/{sessionId}/...` and the stream sidecar serves Chroma Engine manifests, fMP4 HLS playlists, `init.mp4`, and `.m4s` fragments; smoke-covered by `npm run smoke:chroma-hls --workspace @chroma-server/admin-web -- "/Volumes/TVShows/Big Fat Quiz/Season 2026/Big.Fat.Quiz.S2026E01.The.Big.Fat.Quiz.of.Telly.1080p.ALL4.WEB-DL.AAC2.0.H.264-RAWR.mp4"`.
- [x] Run one end-to-end web player test using Chroma Engine output. GenusServer `npm run e2e:chroma-web-player --workspace @chroma-server/admin-web -- media_c75bb70a81774a35964021aacb41f675` fetches the live `/v1/playback` contract, asserts Chroma Engine `hls-fmp4` with H.264/AAC codecs, and verifies Chromium + hls.js decodes 1920x1080 video and advances playback beyond 3 seconds with no fatal HLS or media errors. Latest screenshot artifact: `/tmp/chroma-hls-js-e2e.png`.
- [x] Replace host-server playback/probe contracts with Chroma Engine contracts. GenusServer analyzer version `5` stores Chroma Engine manifest/facts, exposes targeted per-file refresh, and invalidates stale rows after the corrected `mp4a` codec classification.
- [x] Run Chroma Engine behind an env flag. GenusServer gates playback selection through `CHROMA_ENGINE_PLAYBACK`; the stream/analyzer binary path is overridable with `CHROMA_ENGINE_BIN`.
- [x] Add side-by-side diagnostics for Chroma Engine vs legacy media path during migration. GenusServer diagnostics now show FFmpeg/ffprobe, Chroma Engine binary/enabled state, analyzer version, analysis ready/stale/failed/missing counts, and active Chroma playback sessions.
- [x] Switch playback sessions to Chroma Engine. Verified `media_c75bb70a81774a35964021aacb41f675` starts as `directStream`/`hls-fmp4` with `transcodeReasons=["chroma-engine-hls"]`; DTS-in-`mp4a` movie `media_5995bdbc01e74246be61afcf6a986ed6` no longer receives a fake Chroma-native HLS path.
- [x] Switch offline MKV remux to Rust engine. Chroma Engine `remux-mp4` now writes fragmented MP4 from supported MKV/WebM sources without FFmpeg; GenusServer MKV convert jobs prefer this path and fall back to FFmpeg only when native remux fails and fallback is enabled. Validated with `/Volumes/TVShows/Is It Wrong to Try to Pick Up Girls in a Dungeon!/Season 5/Is.It.Wrong.to.Try.to.Pick.Up.Girls.in.a.Dungeon.S05E10.720p.HEVC.x265-MeGusta.mkv`.
- [ ] Remove vendored legacy media binaries once Chroma Engine covers the required native paths. Reopened after server testing: GenusServer still requires ffprobe for some MP4 analysis paths and ffmpeg for MKV/movie fallback paths, so vendored binaries must remain until those calls are replaced by Chroma Engine-native probe/transcode coverage.
