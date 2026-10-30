# Preparation throughput verification

The slow network-source path combined small EBML lookahead reads, separate reads
for packets divided by mux headers, repeated per-cue identity checks, and full
audio indexing that HLS does not consume. Bounded adaptive lookahead, packet gap
coalescing and whole-pass subtitle validation reduce these operations. The
explicit streaming manifest retains video seek points and audio configuration,
while leaving audio extraction to the existing time-window pipeline.

On a private 500 MB Matroska fixture, complete-manifest positional reads fell
from 16,775 (8 KiB control) to 996 with adaptive lookahead. Streaming preparation
needed 444 operations, 38,007,269 logical bytes and 27 cluster visits, versus
486,363,621 logical bytes and 402 visits for the adaptive complete manifest.
These are logical Engine counters, not physical network traffic. The streaming
run took 0.305 seconds with uncontrolled/warm caches; this is not a cold-storage
latency guarantee or a controlled wall-clock speedup claim.

All video chunks and decoder metadata matched the complete manifest. The HLS
initialization output was byte identical. Subtitle metadata and extracted asset
hashes matched the prior implementation. Full manifest JSON remains unchanged.
Regression tests cover cache bounds, gap amplification, packet ordering,
source-change detection and streaming/full manifest equivalence. Full native
Engine tests and strict Clippy were run with macOS codec access.

No preparation compatibility versions change. No client or page design changes.
