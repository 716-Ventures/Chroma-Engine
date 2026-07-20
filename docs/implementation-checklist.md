# Implementation Checklist

## Contract

- [x] Create Rust repo and library/CLI skeleton.
- [x] Move CLI implementation behind the library entrypoint so the binary is only a launcher.
- [x] Narrow public API to an explicit crate-root facade; keep parser/muxer implementation modules crate-private.
- [x] Define native `MediaProbe` JSON with typed tracks, source facts, and capability hints.
- [x] Define command surface: `probe`, `plan`, `manifest`, `chunks`, `codec-config`, `h264-nalus`, `h264-annex-b`, `aac-adts`, `extract-chunk`, `extract-window`, `hls`, `hls-plan`, `hls-segment`, `hls-segments`, `remux-mp4`, `encoder-probe`, `warmup`.
- [x] Define native playback `plan` command with target-specific pipeline stages.
- [x] Add real-library smoke script for mounted media under `/Volumes/Movies`, `/Volumes/TV Shows`, or `/Volumes/TVShows`.
- [ ] Add sanitized real-library probe fixtures from `/Volumes/Movies` and `/Volumes/TVShows`.
- [ ] Add Chroma-native snapshot tests for representative MP4/MKV/HDR/audio/subtitle combinations.

## Engineering Quality

- [x] Migrate crate to Rust 2024 edition.
- [x] Pin Rust toolchain and formatting policy.
- [x] Enforce `cargo fmt --check`.
- [x] Enforce `cargo clippy --all-targets --all-features -- -D warnings`.
- [x] Enforce `cargo test --all-features`.
- [x] Enforce `RUSTDOCFLAGS="-D warnings" cargo doc --no-deps --all-features`.
- [x] Centralize memory-map `unsafe` usage behind an audited source wrapper.
- [x] Add release profile policy for optimized binaries.
- [x] Add Criterion benchmark harness for initial hot paths.
- [ ] Add dependency audit policy once `cargo-deny` is installed or CI can install it reproducibly.
- [ ] Add missing-docs policy for the public facade.
- [ ] Split oversized implementation modules: HLS, MP4, Matroska, CLI.
- [ ] Replace broad `anyhow` use in library-facing APIs with typed engine errors.
- [ ] Add fuzz/property tests for EBML, MP4 atoms, packet range math, and timestamp repair.

## Probe

- [x] File existence and basic container sniffing.
- [x] MP4/MOV box parser: `ftyp`, `moov`, `trak`, `mdia`, `minf`, `stbl`, `stsd`, `mvhd`, `tkhd`, `mdhd`.
- [x] Matroska/WebM EBML parser: Segment Info, Tracks, Attachments.
- [ ] Matroska/WebM EBML parser: Chapters.
- [x] Extract video fields: codec, dimensions.
- [ ] Extract video fields: frame rate, bitrate, pixel format, HDR/DV hints.
- [x] Extract Matroska audio fields: codec, channels, language/title, default/forced disposition.
- [x] Extract MP4 audio fields: codec, channels, sample rate.
- [ ] Extract MP4 audio language/title/default/forced disposition.
- [ ] Extract audio fields: bitrate, Atmos/JOC hints.
- [x] Extract subtitles: text vs bitmap classification, language/title, default/forced disposition.
- [ ] Extract chapters.
- [x] Extract duration.
- [x] Use memory-mapped probing instead of whole-file reads.
- [ ] Define Chroma-native error taxonomy.

## Remux MP4

- [ ] Zero-copy packet path for supported stream-copy remux.
- [ ] Matroska block reader.
- [ ] MP4 writer with faststart `moov` before media data.
- [x] H.264 AVC configuration parsing and Annex-B conversion.
- [ ] HEVC configuration conversion.
- [x] Extract MP4/MOV H.264, HEVC, and AAC codec configuration records.
- [ ] AAC/AC-3/E-AC-3/MP3/FLAC/ALAC sample entries.
- [ ] Text subtitle to `mov_text`.
- [ ] Metadata and chapter copy.

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
- [ ] Emit stream chunks through reusable packet/decode/encode stages.
- [x] H.264/AAC native stream-copy chunks.
- [ ] HEVC native stream-copy chunks.
- [ ] AC-3/E-AC-3/MP3/FLAC/ALAC native copy paths.
- [x] Keyframe-aligned chunk planning.
- [x] Keyframe-aligned MP4/MOV chunk emission.
- [ ] Input-side seek anchoring and copy-path coarse seek behavior.
- [x] WebVTT parsing/rendering foundation.
- [ ] Single-process WebVTT sidecar generation for all selected text subtitles.
- [ ] Multi-audio output without duplicating video encode.

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
- [x] Sanitize per-stream output timestamps to keep DTS monotonic.
- [x] Collapse pathological sub-second segment windows.
- [x] Signal AC-3/E-AC-3 in PMT descriptors for more reliable player detection.
- [x] Add native TS inspection tests for PMT signaling, PCR placement, and continuity counters without external FFmpeg tools.
- [x] Add benchmark coverage for probe, playback planning, packet-window planning, and subtitle segmentation.
- [ ] Add native fMP4 inspection tests that validate fragment timing without external FFmpeg tools.
- [ ] Add multi-audio HLS outputs without duplicating video work.
- [ ] Add subtitle sidecar/rendition generation from native text subtitle parsing.

## Transcode

- [ ] Platform capability probe without shelling out to FFmpeg.
- [ ] macOS VideoToolbox H.264 encode.
- [ ] macOS VideoToolbox HEVC encode.
- [ ] AAC audio encode.
- [ ] AC-3/E-AC-3 bridge encode.
- [ ] CPU fallback decision.
- [ ] Linux VAAPI/NVENC/QSV path.
- [ ] Windows NVENC/QSV/AMF path.
- [ ] Warm encoder session initialization.

## Integration

- [ ] **Milestone: ready to update GenusServer and test in the web player.** Check this on every build-out pass; do not declare it ready until Chroma Engine can produce a browser-playable or WebCodecs-ready native stream for at least one real MP4 from `/Volumes/Movies` or `/Volumes/TVShows` without FFmpeg.
- [ ] Define minimum web-player test contract: Chroma-native manifest, selected tracks, codec config, chunk URLs, timestamp model, and error shape.
- [ ] Emit browser-playable or WebCodecs-ready H.264/AAC output for one real MP4.
- [ ] Add a GenusServer env-flagged route that serves the Chroma-native manifest and chunks.
- [ ] Run one end-to-end web player test using Chroma Engine output.
- [ ] Replace host-server playback/probe contracts with Chroma Engine contracts.
- [ ] Run Chroma Engine behind an env flag.
- [ ] Add side-by-side diagnostics for Chroma Engine vs legacy media path during migration.
- [ ] Switch playback sessions to Chroma Engine.
- [ ] Switch offline MKV remux to Rust engine.
- [ ] Remove vendored legacy media binaries once Chroma Engine covers the required native paths.
