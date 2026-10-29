# Matroska playback failure and startup qualification

The reported 1080p HEVC Main 10/EAC3 episode exposed two Engine issues: a
high-bitrate H.264 conversion could exceed its retained encoded-payload budget,
and Matroska indexing performed thousands of small metadata reads over SMB.
The Server's unnecessary conversion of compatible video with missing probe
bitrate is a separate planning fix; this report covers Engine changes and
offline qualification and the final production replay.

## Bounded H.264 target

At a requested 100 Mbps, the original worker published initialization and the
first fragment, then failed on the second fragment: `encoded video requires
67269177 but the configured limit is 67108864`. This was not a failure before
first-fragment publication. The source has keyframe-aligned fragments longer
than the requested four-second target, including a 13.764-second fragment.

The session now caps its H.264 bitrate against the longest fragment in the full
immutable plan. It reserves 25% for rate-control variation, rounds duration up
to whole seconds, and includes one burst second. The smaller of that ceiling
and the requested rate applies across seek windows and software fallback.
The default policy makes this source's effective ceiling 26,843,545 bps.
The existing 64 MiB retained encoded-payload guard and final-fragment limits
remain enforced; no resource budget is raised.

The budget-fix build produced both initial fragments in 6.276 seconds: 965,051
and 15,041,325 bytes. A canonical local HLS master played through 8.199 seconds
with 34 sampled 1920×1080 frames, zero stalls/drops, and no player error. This
crossed the previously failing second fragment. That codec qualification ran
before the metadata-only change; the final build contains the same encoder fix.

## Bounded metadata lookahead

Each `MediaSource` now retains one reusable lookahead window of at most 8 KiB
for EBML headers, cluster timestamps, and block prefixes of at most 64 bytes.
Its capacity also respects the compressed-window budget, remaining metadata
budget, and source length. A smaller custom budget falls back to the original
exact header read. Packet payload reads and larger lacing-prefix reads bypass
the cache. Parent bounds, source identity boundaries, cancellation checks,
and actual positional-read accounting are preserved.

Same-source offline comparisons produced byte-identical copied initialization
and first fragments in both cases:

| Source | Index open before → after | Total before → after | Positional reads before → after | Counted bytes before → after |
| --- | --- | --- | --- | --- |
| 1080p AVC/DTS | 20.288 → 13.856 s | 21.505 → 15.132 s | 11,537 → 4,425 | 11.30 → 45.46 MB |
| 4K HEVC/EAC3 | 29.939 → 11.743 s | 30.563 → 12.041 s | 12,805 → 4,384 | 7.16 → 42.00 MB |

The tradeoff is more bytes read in exchange for fewer SMB round trips. Counters
measure Engine positional reads, exclude the initial metadata reader, and do
not measure physical disk traffic. These were sequential real-source runs
with uncontrolled OS/SMB caches, not a controlled cold-cache benchmark. Earlier
retries remained slow; later six-fragment tests opened their now-warm sources
in approximately 20–23 ms. Those later timings do not establish cold startup
latency or a production percentile.

## One pass for uncued audio plans

Complete playback manifests retain exact audio chunk plans for browser-native
playback. Previously, each audio track without its own Cues caused another
whole-source block-header scan and retained every packet reference for that
track. The manifest builder now plans all uncued audio tracks in one pass. A
shared incremental chunk planner retains one pending packet per audio track
and a reusable scratch buffer for one laced block; packet payloads are not read.
The existing lacing expansion supplies exact rescaling and frame timestamps.
Per-track logical packet/sample limits still apply even though whole-track
packet vectors are no longer retained. Cued plans and uncued-video behavior
remain unchanged.

Regression fixtures compare exact chunk timing, packet ranges, and keyframe
flags against the individual-track path, including all lacing modes, fractional
default durations, one-packet tracks, equal/backward timestamps, mixed time
scales, empty tracks, independent custom budgets, and cancellation. With two
uncued audio tracks, measured fixture reads fall from eight to four and cluster
visits from six to three. With one cued and one uncued audio track, both paths
use four reads and three visits.

The complete real-source AVC/DTS and 4K HEVC manifests are byte-identical to
the saved pre-batch build, including every track's chunk plan. These two files
each have only one audio track. Sequential pre-batch/new timings were
20.072/0.302 seconds and 73.372/0.188 seconds respectively; the new runs followed
the old source scans and were warm. They do not establish an algorithmic speedup
or a cold-start guarantee. A global header scan remains necessary for uncued
audio; this change removes repeated scans when multiple such tracks exist.
The reported episode's complete 396,952-byte manifest is also byte-identical:
201 video chunks and 343 chunks/42,874 logical packets for each of its two EAC3
tracks. Its sequential pre-batch/new runs took 5.125/0.071 seconds under the same
uncontrolled cache conditions; only the fixture counters isolate scan reduction.

## Native playback matrix

All playback below used a muted Mac `AVPlayer`, canonical HLS masters, actual
sampled video frames, and external playback disabled. Prepared local assets
separate codec qualification from server startup timing. Pixel-format labels
from cached probes were not treated as independently verified chroma facts.

| Source | Qualification | Playback evidence |
| --- | --- | --- |
| Reported 1080p HEVC/EAC3 episode | Existing raw Copy route | 4.109 s, 18 sampled 1080p frames, no error |
| Same episode, H.264 conversion | Resource-aware bitrate fix | 8.199 s, 34 sampled 1080p frames, zero stalls/drops, no error |
| 640×480 AVC/AAC | Existing raw Copy route | More than 4 s with video frames and no error |
| 1080p AVC/AC3 | Existing raw Copy route | More than 4 s with video frames and no error |
| 1872×1080 AVC/DTS → AAC | Metadata-lookahead build, two six-fragment Copy windows | 38.385 s, 154 sampled frames, zero stalls/drops, no error |
| 3840×2160 HEVC/EAC3 | Metadata-lookahead build, two six-fragment Copy windows | 27.149 s, 110 sampled frames, zero stalls/drops, no error |

In those warm-source six-fragment tests, the AVC/DTS worker published init
at 0.299 s and its first fragment at 0.329 s; the complete window took 2.362 s
and covered 35.285 s. The 4K HEVC worker published init and its first fragment
by 0.050 s; the complete window took 0.900 s and covered 24.024 s. Each window
used one source open/index parse and zero video decoder or encoder sessions.
These are publication times, not times until all six fragments are ready.

The Mac tests crossed both six-fragment boundaries. AAC clocks were exactly
contiguous across the AVC/DTS windows; video boundaries differed by at most
1.3 ms from source timestamp rounding. The 4K copied audio/video clocks were
contiguous within 32 ns of video tick rounding. Temporary loopback servers
were closed after testing. No TV operation or watch-history write was used.

## Verification and artifact

The final source passes `cargo test --workspace --all-features`: 345 top-level
tests passed and six were intentionally ignored. A nested subprocess adds one
duplicate passing test line. Strict all-target/all-feature Clippy with
`-D warnings`, formatting, and Release build pass. Five new metadata-reader
tests cover read coalescing/reload, EOF and parent bounds, failed-fill cleanup,
payload bypass and source invalidation, custom budgets, and cancelled cache
hits. Four bitrate-ceiling regressions cover long fragments, full-plan
selection, lower requested rates, and small custom budgets. Four additional
audio-plan regressions cover batch/individual equivalence and measured scan
counts, timing edges, per-track budgets, and cancellation/empty tracks.

Final Engine SHA-256:
`e0036f5c63d4f744727294748490808f34297d97fd250e71efa56d57ec74a72d`.

Local proof files are retained under the private temporary diagnostic folder:
`metadata-cache-before-after.json`, `copy-six-summary.json`,
`copy-six-mac-and-clocks-summary.json`,
`audio-plan-batch-benchmark-summary.json`,
`audio-plan-batch-apothecary-benchmark-summary.json`, and associated clock/player logs.
They are diagnostic working files, not portable repository fixtures. The
results establish the tested Mac behavior; physical tvOS playback remains
unverified.

## Final production replay

The final Engine above and Server
`9384e03387a2707b1ea22da9869c6d39a97312a0e8ad6413dfd4ec18fa97de09`
were verified in the managed live service. Eight Mac canonical-HLS tests passed:
all five representative MKVs, the reported episode's Japanese audio, explicit
Maximum H264 conversion, and a seek to 600 seconds. Each produced actual video
frames, zero access-log stalls/drops and no player error. The DTS and 4K tests
crossed six-fragment window boundaries, and H264 playback crossed the previously
failing second fragment. The seek advanced to 615.475 seconds.

The reported episode became ready in 0.595 seconds and played at 0.629 seconds
with its original HEVC video. Japanese audio became ready in 0.995 seconds;
H264 fallback in 4.825 seconds. These were previously read sources with fresh
playback sessions. The AVC/AAC sample still took 9.115 seconds, so the results
do not establish universally immediate network-volume startup. The initial
pre-batch large-file analysis and eager cluster indexing remain relevant cold
latency limits.

The temporary Mac diagnostic client was revoked, its sessions removed and
retired masters returned 404. Matching metadata and watch history remained
byte-for-byte identical to their database snapshots. The server remains healthy
over LAN under launchd. No physical-TV operation was performed.
