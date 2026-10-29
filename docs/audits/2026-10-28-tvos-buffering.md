# tvOS buffering investigation

The physical tvOS High request profile exposed defects missed by the earlier
8 Mbps Simulator profile and short playback checks. All diagnostics for this
follow-up ran on the Mac. No physical TV was accessed or operated.

For Rocky, the compact large-MP4 probe omitted available video sample statistics.
An unknown bitrate caused unnecessary 50 Mbps H.264 conversion. Its soft average
rate target allowed segment 5 to reach 100.305 Mbps and segment 11 to reach
81.793 Mbps. Fresh AAC workers also introduced 39.333 ms and 32 ms decode-clock
gaps at successive worker boundaries. Fast Mac playback masked these defects:
the pre-fix 140-second Mac replay itself reported no stalls.

The compact probe now retains optional video STSZ/STTS tables within the remaining
metadata budget, capped at 4 MiB, after proving essential headers fit. The real
file probes at 24,013,640 bps and 23.976 fps while retaining 1,498,885 metadata
bytes. The host can consequently copy its original AVC video. Fresh audio
workers align preroll, encoder lookahead and emitted packets to the continuous
1024-sample AAC grid. H.264 conversion additionally installs VideoToolbox's
one-second DataRateLimits property and reports an error if installation fails.

Eighteen real forced-conversion segments across three workers contain no decode
clock gaps or dropped encoded frames; maximum segment bitrate is 36.851 Mbps.
Eighteen independently generated video-copy/DTS-to-AAC segments also have no
clock gaps and peak at 29.633 Mbps. Regression coverage checks 44.1/48 kHz worker
continuity, packet draining, bounded optional metadata and framework property
readback. The Engine suite passes 326 top-level tests, with six intentional
ignores; one nested subprocess produces an additional duplicate passing line.
Strict all-target/all-feature Clippy and formatting checks pass.

The Server uses probe schema 2 selectively, analyzer version 9 and HLS asset
revision `clocked-aac-rate-v5`, preventing reuse of older affected snapshots and
audio fragments. Existing complete snapshots remain cached. Production-profile
replay and cleanup evidence is retained in the Server's buffering audit. These
measurements establish Mac/backend behavior; physical Apple TV network and
decoder behavior still require the owner's authorized playback test.

[Retained quantitative proof](2026-10-28-tvos-buffering.json) includes the deployed 140-second Mac replays, clean audio/video clocks across all 38 actual Rocky HTTP segments, and successful diagnostic revocation/catalog-progress integrity checks. Both films report zero stalls, dropped frames and AVFoundation errors; these remain Mac observations.
