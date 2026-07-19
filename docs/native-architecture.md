# Native Chroma Engine Architecture

Chroma Engine is not a command-compatible FFmpeg replacement. It is a media engine with its own model:

- **Probe emits facts, not legacy process output.** `MediaProbe` is the source of truth: source identity, container family, typed tracks, codec families, flags, attachments, and engine capability hints.
- **Playback is session-first.** The core engine should model packet sources, decode stages, encode stages, muxers, and transport adapters. HLS is one adapter, not the internal architecture.
- **Metadata probing must be bounded.** Probing maps the file and reads container metadata sections. It must not scan packet clusters or decode frames unless a caller explicitly asks for deep analysis.
- **Track IDs are stable semantic handles.** Video, audio, and subtitle tracks get IDs such as `v0`, `a0`, and `s0`; server/client APIs should use those IDs rather than container stream indexes.
- **Playback planning is selective by default.** A session plan selects the primary video and primary audio track unless the caller explicitly asks for broader work, such as all audio tracks. This keeps startup fast and avoids waste.
- **Copy paths stay separate from decode paths.** Remuxing and segmenting copy-compatible streams should avoid decoders, frame allocation, and encoder scheduling entirely.
- **Multi-output work should share stages.** Multiple audio/subtitle outputs should not duplicate video demux/decode/encode work.
- **Compatibility is an adapter layer.** If clients need HLS/fMP4 or another transport, that layer adapts from the native session model. It should not dictate the engine core.

Near-term implementation order:

1. Deepen native probe until it covers real library files across MP4/MOV and Matroska/WebM.
2. Deepen the Chroma playback session manifest with concrete packet/decode/encode stage contracts.
3. Build zero-copy packet readers for MP4 and Matroska.
4. Build ISO-BMFF/fMP4 writers as reusable muxers.
5. Add HLS as a transport adapter on top of the native segment model.
6. Add platform encode/decode backends only where stream copy cannot satisfy the requested session.
