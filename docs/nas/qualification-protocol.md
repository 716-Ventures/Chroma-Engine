# NAS qualification protocol

The measured gates and safety requirements are defined in
[N5 of the execution plan](../nas-deployment-execution-plan.md#n5--media-correctness-and-performance-qualification).
Freeze fixture hashes, device firmware, server and engine revisions, client
versions, network topology, cold/warm method, and thresholds before collecting
results. This file tracks fixture selection and amendments without rewriting a
failed threshold after a run.

| Fixture ID | Required coverage | Duration | Hash | Rights confirmed | Status |
| --- | --- | --- | --- | --- | --- |
| `media-fixture-01` | MP4, H.264, copy-compatible audio, direct play | >= 2 h | pending | pending | unassessed |
| `media-fixture-02` | MKV, HEVC Main/Main10, copy/remux, useful cues | >= 2 h | pending | pending | unassessed |
| `media-fixture-03` | MKV, no useful cues, long GOP, high bitrate | pending | pending | pending | unassessed |
| `media-fixture-04` | AV1 and representative audio conversion | pending | pending | pending | unassessed |
| `media-fixture-05` | supported HDR copy and text subtitles | pending | pending | pending | unassessed |
| `media-fixture-06` | large source (>4 GiB) and seek near end | pending | pending | pending | unassessed |

These are coverage slots, not invented files or claims of supported codec
combinations. Record `sha256`, container/stream probe facts, license or ownership
confirmation, and the expected mode/rejection before running a trial. Never
record a private title-to-fixture mapping in this repository.

## Trial record

For each device and mode, retain raw timing/telemetry outside Git and publish
only a sanitized summary with: firmware, architecture, RAM, server/engine SHAs,
image digest/ABI, resource settings, fixture IDs and hashes, browser/tvOS client
versions, wired network/storage headroom, warm/cold definition, 30 independent
start/resume/seek samples, p50/p95, first response/segment/frame timestamps,
full-length rebuffer and A/V sync observations, RSS/PSS/container high-water,
queue depth, swap, and failures. Keep disk-spin-up and cue-less indexing as
separate strata. Record failed thresholds unchanged before optimization.

Fault injection must use temporary test volumes and authorized fixtures only.
The plan's 24/72-hour soak and physical player gates cannot be replaced by
synthetic CI. A benchmark-only result is `build-tested` at most.

No physical appliance or full-length qualification result is recorded yet.
