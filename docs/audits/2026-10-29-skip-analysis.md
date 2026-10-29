# Native skip-range analysis

`analyze-playback-markers INPUT --cache-dir DIRECTORY` emits measured source-time
ranges and source identity without rewriting video or using FFmpeg. Up to two
`--reference EPISODE` arguments enable repeated-audio analysis; `--audio-only`
allows a server to publish an opening immediately before independent visual work.
The output includes confidence/method/reference count, duration and chapter facts.
The server decides scheduling, source-generation persistence and client delivery.

## Evidence and limits

Portable AAC/AC3/EAC3 decoding covers bounded first-six-minute and last-five-minute
windows. Streaming downmix/resampling feeds gain-insensitive spectral features;
full decoded audio is never retained. A candidate needs at least 20 seconds of
coherent nonconstant/nonsilent matches confirmed by two independent same-season
peers. Both peer windows are compared, covering a premiere that places its opening
theme at the end. Only measured terminal ranges qualify as credits: they start
within the last 240 seconds and end within the last 90 seconds. Those limits are
eligibility gates, not invented boundaries or fixed song lengths. Late-story
reprises outside those gates and unrelated-program evidence are rejected.

Visual analysis looks for sustained aligned glyph-like rows on dark backgrounds
and upward movement stronger than downward correlation. Frames are natively
decoded/scaled to at most 640×360. Observed boundaries are refined; uncertain gaps
and post-credit scenes remain outside the ranges. Static/artistic credits and
unsupported decoders may return no marker. Commercials and recaps are not guessed
by these detectors; authored/imported ranges are handled by the Server.

Source/cancellation checks and bounded packet windows remain active. Acoustic
cache entries are source/configuration/version bound and at most 2 MiB each, with
64 entries / 64 MiB aggregate limits. Visual work bounds source I/O to 2 GiB, decoded
frames to 60,000, frame requests to 2,000, wall time to 240 seconds and one cached GOP
to 128 grayscale frames (roughly 30 MiB at the analysis scale). Empty or uncertain
results are valid, and independent detector failures remain explicit notices.

## Decoder and chapter corrections

The vendored AAC decoder's standard 128/1024-coefficient inverse MDCT used direct
quadratic cosine evaluation. A cached safe FFT/DCT-IV path replaces it while
retaining the original mathematical transform as reference/fallback. Independent
impulse/random comparisons, codec-size TDAC overlap and known access-unit PCM
synthesis tests check equivalence. The full vendored suite passes 94 tests, with
four intentional ignores. Licensing and prior streaming patches are retained in
`third_party/rusty_aac/CHROMA_PATCHES.md`.

MP4 parsing now recognizes Nero chapter tables under `moov/udta/chpl`, as well as
direct `moov/chpl`. This restores chapter metadata without classifying generic
scene names as skip ranges. The Server can refresh only the analyzed source's
chapter snapshot, avoiding a whole-library probe or metadata rematch.

## Qualification

Two real episode fixtures use two independent peers each. The premiere has no
fabricated early Intro; measured terminal credits are 1,331,220–1,419,700ms at
confidence 0.97140. A second real episode has Intro 1,728–89,408ms and
Credits 1,280,384–1,354,624ms at confidence 0.97884/0.98228. An unrelated-program
negative produces no ranges. Final-binary cache reuse completes these checks in
roughly 0.37–0.44seconds; this does not measure cold NAS access.

A first full AAC target-plus-two-peer workload completed in 39.258 seconds after
the transform correction; the old decoder exceeded 240 seconds before completing
one source. EAC3 took 16.707 seconds initially and 6.011 seconds on previously
accessed storage. These observations use uncontrolled NAS/OS caches and are not
appliance performance guarantees.

Two actual movies produce six/two conservative rolling-credit ranges with
confidence 0.937–0.945. A locally generated native H264 positive fixture contains
rolling text 75–108 seconds and a post-credit scene 108–120 seconds: detection returns
78–107.5seconds, excluding the scene. Starfield, static logo, dark scenery,
silence/constant audio and misplaced reprise negatives are covered.

The complete Engine workspace passes 414 top-level tests (415 including a nested
child), with eight intentional ignores including private-media diagnostics.
Strict all-target/all-feature Clippy, formatting and the optimized build pass.
Public evidence uses neutral fixture labels and technical counts only; private
source paths, caption text and source frames are not committed.

Sanitized measurements, per-build provenance and the final Server API priority
regression are in [skip-analysis-evidence.json](skip-analysis-evidence.json).
