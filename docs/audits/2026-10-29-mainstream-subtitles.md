# Mainstream subtitle resources

`subtitle-resources` now exports source-timed resources for text and bitmap
subtitles without burning captions into the video or transcoding video for them.
Resources retain stable source-track identifiers, language/default/forced track
flags, authored ASS fonts, and bitmap source coordinates.

| Format | Input | Rendering resource |
| --- | --- | --- |
| SubRip, WebVTT | Matroska or matching sidecar | WebVTT and optional ASS overlay |
| ASS, SSA | Matroska or matching sidecar | Authored script and attached fonts |
| MP4 timed text (`tx3g` / `mov_text`) | MP4 | Styled WebVTT and ASS overlay |
| Blu-ray PGS | Matroska or matching SUP sidecar | Positioned PNG tiles and timed JSON |
| DVD / VobSub | Matroska or paired IDX/SUB sidecars | Positioned PNG tiles and timed JSON |
| DVB bitmap subtitles | Matroska | Positioned PNG tiles and timed JSON |
| TTML / DFXP | Matching TTML, DFXP or XML sidecar | Normalized WebVTT and ASS overlay |
| SAMI | Matching SMI/SAMI sidecar | Separate language resources |
| MicroDVD, SubViewer, SBV | Matching SUB/TXT/SBV sidecar | Normalized WebVTT and ASS overlay |
| Scenarist SCC / CEA-608 | Matching SCC sidecar | Separate CC1/CC2 timed text |

This describes supported transports, not every extension of every specification.
Embedded video SEI broadcast CEA-608/708, MP4 `stpp`, DVD per-line COLCON palette
changes, DVB character/ROM/progressive objects, and separately selecting only
forced objects within an otherwise unforced PGS track are not implemented.
Recognized unsupported bitmap features produce per-track unavailability. Manual
PGS selection includes all of the track's authored objects. ASS remains authored;
normalizing TTML/SAMI/SCC preserves supported text styles rather than promising
pixel-identical broadcast layouts.

## Resource and startup boundaries

`--native-text-only` excludes episode-wide bitmap/font work. The mutually exclusive
`--external-text-only` mode additionally avoids scanning embedded Matroska text;
new overlay-capable clients request those tracks independently of video startup.
Legacy clients retain native text rendition behavior.

Bitmap decoders bound each canvas to 16 million pixels / 64 MiB RGBA, one PNG
to 8 MiB, timed indexes to 100,000 cues / 8 MiB, and the combined extraction
archive to 512 MiB / 50,000 files across all tracks; each track remains limited
to 256 MiB. Failed tracks release their
archive reservations and generated files. Source identity/cancellation checks
remain active during packet scans and image publishing. Matroska compression
supports bounded zlib and header stripping in reverse encoding order; encrypted
or unsupported encoding chains fail the individual track.

XML parsing disables DTDs and external entities, limits nodes/depth/style chains,
and never evaluates content. Sidecar discovery is bounded, uses same-basename
regular files, rejects symlinks, and publishes generated resource filenames.
Text output is limited to 32 MiB per script, font attachments to 16 MiB each
and 128 attachments, and aggregate imported font/script resources to 128 MiB.
Retained cue data also respects the configured index memory budget. Markup depth
and per-track cue counts remain bounded.
UTF-8 and BOM-marked UTF-16 are validated strictly; declared XML/SAMI charsets
are honored. Legacy text can use Windows-1252, while WebVTT remains UTF-8 and
SCC remains ASCII. Invalid individual sidecars do not discard valid tracks.

## Qualification

The actual PGS fixture `subtitle-fixture-01` produces two English PGS resources:
307 cues / 312 PNG tiles and 37 cues / 37 tiles. Both first cues cover
5,590–16,434 ms on a 1920×1080 source canvas. A real DVB episode produces
1,585 cues / 2,505 tiles. Neither source reports an unavailable subtitle track.
The previous MP4 timed-text regression remains part of qualification.

Portable decoder tests cover palette/RLE/state/timing behavior, fragmented
objects, clear displays, malformed sizes, compression bombs/checksums, external
SUP and IDX/SUB delivery, and extraction rollback/aggregate bounds. Timed-text
tests cover style/timing normalization, multilingual SAMI, timed TTML spans,
SCC display memories/channel controls/Unicode/parity, and ASS literal escaping.
The corresponding Server audit records end-to-end API and simulator evidence.

All six real-media fixtures pass with the final optimized Engine, including a
second MP4 whose ordinary full-frame tx3g box previously caused rejection.
The complete workspace passes 401 top-level tests (402 including a nested child),
with six existing intentional ignores. Formatting, strict all-target/all-feature
Clippy and the optimized build pass. No high-confidence vulnerability was found
in the reviewed resource authorization, file discovery or decoder bounds.

Sanitized counts, timing caveats and binary identity are recorded in
[mainstream-subtitle-evidence.json](mainstream-subtitle-evidence.json).
