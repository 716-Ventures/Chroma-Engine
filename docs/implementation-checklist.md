# Implementation Checklist

## Contract

- [x] Create Rust repo and library/CLI skeleton.
- [x] Define native `MediaProbe` JSON with typed tracks, source facts, and capability hints.
- [x] Define command surface: `probe`, `plan`, `chunks`, `extract-chunk`, `remux-mp4`, `encoder-probe`, `warmup`.
- [x] Define native playback `plan` command with target-specific pipeline stages.
- [ ] Add real-library probe fixtures from `/Volumes/Movies` and `/Volumes/TVShows`.
- [ ] Add Chroma-native snapshot tests for representative MP4/MKV/HDR/audio/subtitle combinations.

## Probe

- [x] File existence and basic container sniffing.
- [x] MP4/MOV box parser: `ftyp`, `moov`, `trak`, `mdia`, `minf`, `stbl`, `stsd`, `mvhd`, `tkhd`, `mdhd`.
- [x] Matroska/WebM EBML parser: Segment Info, Tracks, Attachments.
- [ ] Matroska/WebM EBML parser: Chapters.
- [x] Extract video fields: codec, dimensions.
- [ ] Extract video fields: frame rate, bitrate, pixel format, HDR/DV hints.
- [ ] Extract audio fields: codec, channels, language/title, default/forced disposition.
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
- [ ] H.264 AVC configuration conversion.
- [ ] HEVC configuration conversion.
- [ ] AAC/AC-3/E-AC-3/MP3/FLAC/ALAC sample entries.
- [ ] Text subtitle to `mov_text`.
- [ ] Metadata and chapter copy.

## Native Playback

- [ ] Define Chroma-native playback session manifest.
- [x] Add initial Chroma-native playback plan: selected tracks, shared demux, copy/decode/encode, mux, transport adapters.
- [x] Default playback planning selects primary video/audio and excludes target-unusable bitmap subtitles for browser/Apple targets.
- [x] Remove legacy transport assumptions from the public core command/module surface.
- [x] Define native compressed packet references and keyframe-aligned chunk planning.
- [x] Add MP4/MOV sample-table chunk planning without per-packet materialization.
- [x] Add Matroska/WebM Cues-based chunk planning with bounded cluster fallback.
- [x] Emit native MP4/MOV compressed chunk payloads from packet byte ranges.
- [ ] Emit stream chunks through reusable packet/decode/encode stages.
- [ ] H.264/AAC native stream-copy chunks.
- [ ] HEVC native stream-copy chunks.
- [ ] AC-3/E-AC-3/MP3/FLAC/ALAC native copy paths.
- [x] Keyframe-aligned chunk planning.
- [ ] Keyframe-aligned chunk emission.
- [ ] Input-side seek anchoring and copy-path coarse seek behavior.
- [x] WebVTT parsing/rendering foundation.
- [ ] Single-process WebVTT sidecar generation for all selected text subtitles.
- [ ] Multi-audio output without duplicating video encode.

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

- [ ] Replace host-server playback/probe contracts with Chroma Engine contracts.
- [ ] Run Chroma Engine behind an env flag.
- [ ] Add side-by-side diagnostics for Chroma Engine vs legacy media path during migration.
- [ ] Switch playback sessions to Chroma Engine.
- [ ] Switch offline MKV remux to Rust engine.
- [ ] Remove vendored legacy media binaries once Chroma Engine covers the required native paths.
