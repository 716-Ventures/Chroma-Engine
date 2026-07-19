# Implementation Checklist

## Contract

- [x] Create Rust repo and library/CLI skeleton.
- [x] Define JSON response types matching GenusServer `SourceProbe`.
- [x] Define command surface: `probe`, `hls`, `remux-mp4`, `encoder-probe`, `warmup`.
- [ ] Add golden fixtures from representative GenusServer media files.
- [ ] Add parity snapshots against current `ffprobe` and `ffmpeg` outputs.

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
- [ ] Match GenusServer error envelopes.

## Remux MP4

- [ ] Zero-copy packet path for supported stream-copy remux.
- [ ] Matroska block reader.
- [ ] MP4 writer with faststart `moov` before media data.
- [ ] H.264 AVC configuration conversion.
- [ ] HEVC configuration conversion.
- [ ] AAC/AC-3/E-AC-3/MP3/FLAC/ALAC sample entries.
- [ ] Text subtitle to `mov_text`.
- [ ] Metadata and chapter copy.

## fMP4 HLS

- [ ] Emit `master.m3u8` immediately from plan metadata.
- [ ] Emit variant playlists, `init_*.mp4`, and `seg-%05d.m4s`.
- [ ] H.264/AAC stream-copy HLS.
- [ ] HEVC stream-copy HLS.
- [ ] AC-3/E-AC-3/MP3/FLAC/ALAC copy paths.
- [ ] Keyframe-aligned segmentation.
- [ ] Input-side seek anchoring and copy-path coarse seek behavior.
- [ ] Optional program date time tags.
- [x] WebVTT sidecar parsing/rendering and media playlist generation foundation.
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

- [ ] Add GenusServer `MediaTool` abstraction.
- [ ] Run Chroma Engine behind an env flag.
- [ ] Dual-run probe parity in diagnostics.
- [ ] Switch HLS sessions to Rust engine.
- [ ] Switch offline MKV remux to Rust engine.
- [ ] Remove vendored `jellyfin-ffmpeg` once parity passes.
