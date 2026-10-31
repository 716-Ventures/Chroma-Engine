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

## Recovery of failed preparation jobs

The recovery audit found two avoidable full indexes: a valid mux with three
linked SeekHeads exceeded the previous two-head search, and delayed initial
audio missed the one-second presence sample. Both readers now follow at most
16 distinct bounded SeekHeads, preserving cycle/offset rejection. Audio presence
uses a maximum 30-second initial window but still stops as soon as every requested
audio track appears. No complete preparation row is invalidated by these fixes.

Indexed subtitle validation and extraction now run together in physical source
order. Previously the engine validated every addressed block, retained its
location, and then reread all the same blocks to extract their contents. The
visitor consumes each block while its header window remains resident. An invalid
late reference resets text accumulation and bitmap decoder/publisher state before
full-scan fallback; partial PNGs and archive reservations are released. Resource
limit errors from the consumer propagate, rather than initiating another scan.

Library text scripts are bounded to 32 MiB each; CJK font attachments to 16 MiB
each, with at most 128 fonts. The combined script/font import budget is 128 MiB,
and retained cue allocations also respect the configured index budget. Cue
bookkeeping is charged against aggregate memory, not against the text payload's
individual script limit. Bitmap limits are unchanged. Server delivery and client
validation must use corresponding asset limits; a physical client installation is
separate from deploying the engine/server.

Failures from missing encoded source bytes cannot be repaired by retrying a job.
Fragmented MP4 input indexing remains unsupported. Metadata probing retains
its initialization metadata and stops at the first `moof`, avoiding a walk of
every fragment that would never populate traditional sample tables. This
restriction does not apply to generated HLS output fragments.
Detailed CLI causes are retained for diagnosis; server responses remain sanitized.

Font discovery is skipped when the source has no ASS/SSA subtitle tracks. SRT,
WebVTT and bitmap subtitle tracks do not consume embedded fonts. Previously a
valid SeekHead without an Attachments entry caused a full-file search for unused
fonts after successful text extraction. A no-font 2,000-Cluster regression asserts
that this step performs no source reads. Explicit zero-duration subtitle events
are omitted without discarding the track's other visible events.

Large bitmap blocks are prefetched into the existing bounded metadata window
(up to its configured ceiling, at most 1 MiB), so BlockGroup validation, trailing
duration fields and compressed payload consumption reuse one owned span. Larger
blocks retain the bounded positional fallback. Publisher memory accounting now
accumulates completed cue/path allocations rather than summing the complete cue
history after every packet; active display and vector capacities remain charged.

## Compressed bitmap subtitle recovery

Chunked zlib expansion used `FlushDecompress::Finish` with only an 8 KiB output
slice. The backend requires sufficient output space for the whole remainder in
that mode, so valid large compressed PGS packets failed. Streaming expansion now
uses `None`, preserving the expanded-size, progress, checksum and trailing-input
checks. A 200,000-byte synthetic payload fails under the previous mode and passes
under the corrected mode; truncation and expansion-limit checks still fail.

Resources report diagnostic `extractionRevision: 1`. This is not a preparation
compatibility-version change: successful older resources remain valid. Server
recovery targets only old partial outputs carrying this exact decompression
failure; new genuine source failures retain their cached diagnostic outcome.
