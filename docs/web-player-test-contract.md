# Web Player Test Contract

This is the minimum Chroma Engine contract required before GenusServer should wire a real web-player playback test to the native engine path.

## Readiness Gate

Chroma Engine is ready for the first web-player integration test when it can produce, without FFmpeg:

- A probe document for a real MP4 source from `/Volumes/Movies`, `/Volumes/TVShows`, or `/Volumes/TV Shows`.
- A native playback manifest for the selected video track and at least one selected audio track.
- Browser-usable codec configuration for the selected tracks.
- Keyframe-aligned chunk metadata with monotonically increasing DTS and stable PTS offsets.
- Chunk payload endpoints or files that can be served as either WebCodecs-ready compressed chunks or browser-playable HLS/fMP4 output.
- Stable error codes from `EngineErrorCode` for all probe, planning, chunk, and mux failures exposed through the server.

Do not check the integration milestone until a real MP4 can play video and audio from Chroma Engine output in the web player without falling back to FFmpeg.

## Server Request

The server should treat Chroma Engine as the source of truth for playback facts:

- `probe`: source path, container, tracks, codec families, languages, titles, flags, chapters, bitrate, frame rate, pixel format, dynamic range, and capability hints.
- `plan`: selected tracks, copy/decode/encode stages, target transport, and required server route family.
- `manifest`: selected track metadata, codec strings, decoder config hex, chunk target, and chunk list.
- `chunk`: selected track id plus chunk index, returning payload bytes and sample timing metadata.
- `hls-plan` / `hls-segment`: browser-playable HLS/fMP4 or TS output when the browser path uses native HLS.

## Web Player Requirements

The first web-player test must prove:

- Video and audio both render.
- Playback starts from time zero without an initial blank/black-only failure.
- Audio remains synchronized with video.
- Segment or chunk requests stay monotonic during initial playback.
- Audio track selection uses engine track ids such as `a0`, `a1`, and changing audio preserves the current playback position.
- Failures show the stable Chroma Engine error code and a concise diagnostic string.

## Error Shape

Server-facing failures should preserve this shape:

```json
{
  "engine": "chroma-engine",
  "code": "hls_unsupported",
  "message": "source track combination is not supported by native HLS",
  "context": {
    "sourcePath": "/Volumes/Movies/example.mp4",
    "trackIds": ["v0", "a0"]
  }
}
```

The `code` value must come from `EngineErrorCode`; `message` is human-readable and may change.
