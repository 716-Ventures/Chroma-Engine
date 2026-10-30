# Indexed preparation on network sources

Live profiling found all four active preparation children blocked in positional
reads, rather than CPU-bound decoding. Repeated Matroska Cluster header walks
made short analysis windows expensive on SMB storage. Streaming manifest audio
presence checks also indexed packets beyond the required sample.

The file index now uses validated SeekHead/Cues positions to enter a bounded
window, retaining uncued clusters and signed relative timestamp margins. Audio
presence stops once the requested tracks appear. Subtitle extraction reads
independent blocks addressed by authored subtitle Cues; referenced/stateful
blocks, missing selected-track indexes and invalid references retain the full
scanner. Optional CueDuration is not required: original BlockDuration, styles,
WebVTT additions and bitmap decoder ordering remain authoritative. Font
attachments use SeekHead when present. Index allocations remain bounded by the
resource policy; source identity and cancellation are checked around each pass.

On the private 500 MB fixture, streaming-manifest reads fell from 444 operations,
38,007,269 logical bytes and 27 Cluster visits to 5 operations, 44,282 bytes and
one visit. Manifest JSON was identical. A three-source audio comparison retained
exact intro/credits boundaries and confidence. Real subtitle comparisons retained
all metadata and every asset byte: ten ASS tracks (ten assets) and two PGS tracks
(407 assets). Diagnostics confirmed both indexed extraction paths executed.

Sequential wall-clock comparisons were 17.638 versus 0.182 seconds for ASS,
1.653 versus 1.482 seconds for PGS, and 33.769 versus 13.943 seconds for the
three-source audio analysis. Storage caches were uncontrolled; these timings are
not cold-cache throughput guarantees. The logical read reduction is the stronger
measurement. Files without usable indexes still need a full scan, and authored
subtitle indexes are expected to enumerate their independent subtitle frames.

Validation: full Engine test targets, 407 library tests, strict all-target Clippy,
formatting and real extracted-byte comparisons. Regressions cover indexed versus
full packet windows, uncued clusters, negative relative times, early presence
exit, subtitle styles/timing/fonts and malformed/missing-index fallback.

No preparation compatibility version changed. Previously completed files require
no invalidation. No client, page layout or Apple TV changes are included.

## Interleaved audio analysis

The live post-index check exposed an additional MP4 audio bottleneck: the packet
copy reader intentionally permits only tiny gaps and 2x read amplification,
which produces many remote reads when video is interleaved with audio. Background
audio analysis now has a separate bounded sequential policy: at most 256 KiB per
gap, 1 MiB per physical span, 128x selected payload amplification, and the existing
compressed-window memory limit. Playback retains its original stricter policy.
Only selected packet bytes reach the decoder, in their original order.

A synthetic interleaved regression reduced 128 reads to three while preserving
every selected byte. It also checks out-of-order packets, excessive gaps, tight
memory budgets and changed-source rejection. A real three-source MP4 comparison
completed all original ten-minute intro/five-minute tail windows in 88.288 seconds,
with a two-reference intro match and no notices. This is an uncontrolled storage
measurement, not a promise for every source. Detection windows, fingerprint
versions and completed preparation rows were not changed to obtain the result.

## Subtitle-heavy Cues validation

A live HEVC/EAC3 file with 32 subtitle tracks contained 187,092 Cue elements in
only 720,475 bytes. Applying the generic 100,000-container-element limit to this
sample index rejected valid input. The new indexed path fell back, and the old
full Cluster scan ultimately failed on the same validation. Both paths now
validate Cues using a work ceiling derived from the configured index budget
(one element per 32 budgeted bytes), retaining metadata-size, nesting-depth and
expanded-index allocation limits. Ordinary container hierarchy limits remain
unchanged. A regression verifies acceptance beyond 100,000 Cue elements and
rejection under a tight index budget.

The previously failing 3.9 GB source then produced its streaming manifest in
0.370 seconds: four source reads, 745,063 logical bytes and one Cluster visit.
This run used uncontrolled caches. Crucially, it succeeded without the rejected
index/full-scan retry path; the earlier live attempt failed after minutes.
