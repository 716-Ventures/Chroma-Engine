# Native marker pipeline rewrite

The prior implementation repeatedly opened GOP decoders and re-read overlapping
Matroska neighborhoods while comparing arbitrary image pairs. Profiling a live
worker found 689 of 723 main-thread samples in pread. This follow-up replaces that
architecture rather than increasing its timeout or reducing detection accuracy.

## Implementation

- Standard Chromaprint algorithm 2 and its upstream matcher, via pinned
  rusty-chromaprint 0.3.0, replace custom spectral signatures and matching.
- First min(25% of duration, 10 minutes) and last five minutes are fingerprinted.
  Two independent same-season peers corroborate ranges; silence, constant audio,
  unrelated tracks and nonterminal reprises cannot produce accepted ranges.
- Native AAC/AC3/EAC3 plus the existing Opus, DTS Core and TrueHD decoders stream
  PCM into the fingerprinter. No external FFmpeg process or retained PCM files.
- Credits screen keyframes in the bounded tail (35%, capped at 15 minutes),
  retaining the conservative text-on-dark-background verification. Static credit
  text is now eligible; upward motion is no longer mandatory. Generic logos,
  darkness and coloured scenery are not accepted as credit evidence.
- Video packet metadata for the tail is built once and the native decoder is
  retained across random-access intervals. Full GOPs are decoded only at candidate
  boundaries. Negative keyframes split runs; post-credit scenes stay outside them.
- Source-bound numeric screening observations and refined boundary observations
  are atomically checkpointed separately from segment output. Interrupted scans
  resume and completed evidence can be reused without opening a decoder/index.
- Acoustic evidence has a separate v2 namespace, bounded at 20,000 entries / 512
  MiB instead of evicting everything beyond 64 fingerprints. Existing complete
  server results remain compatible; no library-wide preparation version bump.

The architectural reference is Jellyfin Intro Skipper's public documentation and
FFmpegService implementation, inspected on 2026-10-30. No Intro Skipper source was
copied or translated. This is not a claim of identical classifiers or complete
format/visual-style coverage. Authored chapter/EDL commercial ranges remain the
commercial source; this change does not invent general advertising detection.

Sources:
- https://github.com/intro-skipper/intro-skipper/wiki
- https://github.com/intro-skipper/intro-skipper/blob/10.11/IntroSkipper/FFmpeg/FFmpegService.cs
- https://github.com/darksv/rusty-chromaprint/tree/97d67dfed02854ac5d5eaa9ac965b9abe3dff690

## Validation

Full native Engine tests and strict Clippy pass. New tests cover upstream matching,
packet-boundary-independent fingerprints, silence/unrelated rejection, independent
peer consensus, source/cache replacement, cache symlinks, positive cached boundaries
without any decodable media, static credit text, scene gaps and cross-process
Opus/visual evidence reuse. Dependency inventory and license-policy tests pass.

A synthetic original clip with credits at 75–108 seconds and a scene at 108–120
returns 78–107 seconds; cached execution took 0.013 seconds. Two private movie
fixtures produce observed credit ranges in 6.282 / 12.242 seconds with the retained
decoder; cached runs take 0.020 / 0.035 seconds. Before decoder retention, the same
new pipeline took 27.607 / 55.142 seconds with identical ranges. These are sequential
observations with uncontrolled NAS/OS caches, not controlled speedup ratios or
cold-storage guarantees. A separate original-detector run of the first movie took
13.939 seconds on warm storage; the historical cold/warm timings are not comparable.

A real episode target with two peers took 61.549 seconds to create all three
fingerprints on the NAS, then 0.308 seconds cached. A familiar 1080p anime case
produced corroborated intro and credits in 33.769 seconds for all three sources,
then 0.083 seconds cached. Initial audio demux/read costs still depend on storage;
this report does not claim every first scan is instantaneous.

Private titles, paths, library IDs, source frames and captions are excluded here.
No Apple TV or UI changes.


## HEVC open-GOP refinement

Live validation exposed one additional failure on a private 59 GB Dolby Vision
source: screening completed, but VideoToolbox rejected a boundary GOP containing
RASL leading pictures. Refinement now starts a fresh decoder when jumping back
from screening and excludes HEVC RASL pictures that can reference the preceding
GOP. The keyframe-only pass retains its decoder. Reusing the completed numeric
screening evidence, the corrected refinement completed in 3.568 seconds, decoded
152 frames, read 22,782,777 logical bytes and returned four measured credit ranges
with no unavailable notice. No full rescan was necessary. A follow-up cached run
and native regression tests verify the corrected path before its deployment.
