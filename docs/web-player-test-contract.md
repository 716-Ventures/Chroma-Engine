# Browser and tvOS Playback Qualification Contract

The initial short-window browser integration milestone has been demonstrated.
This document defines the ongoing host/player acceptance work; it is not a claim
that every requirement already passes. See [current status](status.md).

## Engine and host boundary

The engine supplies probe facts, selected-track plans, codec configuration,
chunk timing, and supported HLS/fMP4 or native compressed output without
FFmpeg/ffprobe. Raw extracted chunks are not automatically playable MP4 files.

The host owns authentication, authorized source paths, client capability
selection, session lifetime, work scheduling, cache quotas, HTTP delivery, and
playback-progress events. A plan is not an executing session. Match the selected
plan to the appropriate packaging/conversion API.

Use engine track IDs within the current source identity. Refresh selections when
the source changes. Do not use an outdated migration feature flag or assume
unsupported media has a legacy fallback.

## Acceptance matrix

Use neutral-ID fixtures with recorded container, codec/profile, bit depth,
resolution, audio layout, subtitle type, and duration. Cover the supported
combinations that actual clients need, including compatible HDR copy and explicit
rejection of unsupported HDR conversion or subtitle rendering.

For each target browser/tvOS device and representative server/NAS class:

- Verify video and audio render, with independent checks for color, audio levels,
  channel order, priming, and A/V synchronization.
- Measure cold/warm startup, distant resume, and repeated seeks. Record the
  engine-generation time separately from HTTP delivery and player startup.
- Verify consecutive fragments preserve timing and decoder continuity.
- Change audio tracks without losing playback position or leaking abandoned work.
- Exercise text subtitles, concurrent users, and cancellation.
- Run full-length playback and multi-day soak tests; measure stalls, drift, CPU,
  memory growth, and disk use.
- Test missing segments, source mutation, network interruption, disk exhaustion,
  worker failure, and eligible backend fallback. Errors must terminate or recover
  predictably rather than produce indefinite player retries.

Define device-specific latency, stall, drift, concurrency, and resource thresholds
before measuring. Passing a few seconds of Chromium playback or a synthetic
engine test does not establish these acceptance criteria.

## Output and error contracts

For HLS/fMP4, serve the matching playlist, initialization segment, media fragments,
and subtitle renditions. A single generated window is not a complete movie.
Immutable engine publication does not overwrite a changing playlist; the host must
manage new playlist paths or a dynamic response.

Library error types differ by API, and CLI failures are not a guaranteed JSON
error envelope. `PlaybackSession` exposes engine error codes; native conversion
and some other entrypoints use `anyhow::Result`. The host should map these into
its own stable client-facing error contract, preserve a code when available, and
log sanitized diagnostics. Do not classify failures solely by matching stderr.

An illustrative host response, not a universal engine CLI schema:

```json
{
  "engine": "chroma-engine",
  "code": "hls_unsupported",
  "message": "source track combination is not supported by native HLS",
  "context": {
    "fixtureId": "media-fixture-01",
    "trackIds": ["v0", "a0"]
  }
}
```

Do not expose local source paths or identifying media titles in public reports.
Use the [audit reporting conventions](audits/README.md) and retain measurements,
build identities, device details, and explicit limitations.
