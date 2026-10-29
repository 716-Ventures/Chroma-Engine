# Hell Mode MP4 subtitle compatibility

The first episode of *Hell Mode* contains a default English `mov_text` (tx3g)
subtitle track alongside H.264 video and Japanese AAC audio. It was discovered
correctly by probing, but native subtitle-resource extraction rejected the
entire track. HLS video/audio copy does not retain that embedded subtitle track,
so rejection left the client without a usable caption rendition.

## Cause and correction

The compatibility parser treated normal tx3g defaults as unsupported layout:
centered horizontal justification, signed bottom justification (`-1`), and an
opaque black background. After accepting these defaults, extraction encountered
ordinary font-family and size changes in the sample style records and rejected
those too.

The extractor now preserves representable tx3g presentation in source-timed
WebVTT:

- Horizontal and vertical justification become cue alignment/line settings.
- Default foreground/background RGBA values become cue CSS colors.
- A bounded `ftab` parser resolves authored font IDs. Font-family, relative size,
  and RGBA changes use generated numeric cue classes; bold, italic and underline
  retain their existing cue tags. Size percentages are relative to the nonzero
  default size. Stylesheets precede all cues.
- Original text stays on the packet presentation timeline. UTF-8/UTF-16 decoding,
  UTF-16 character-range boundaries, surrogate-pair checks and overlap rejection
  remain enforced.

The sample's five font names and 39/51/63/75 size records become nine used CSS
classes with 52%, 68%, 84% and 100% sizes. Native caption rendering can substitute
fonts unavailable on the device and apply viewer accessibility preferences;
this conversion does not install fonts or establish pixel-identical typography.
The standard cue-class and font properties are described by
[WebVTT's cue CSS rules](https://www.w3.org/TR/webvtt1/#the-cue-pseudo-element).

## Bounds and remaining compatibility limits

Font tables are limited to 256 unique, nonempty UTF-8 font records and 64 KiB.
Font names are quoted CSS strings with every non-alphanumeric/non-space
character hex-escaped. Names never enter selectors or property names; classes
are generated as `tx3gN`. A track can use at most 4,096 unique style classes;
identical styles reuse a class. Missing referenced fonts, malformed/duplicate
font records, undefined size baselines, invalid ranges and unsupported face
bits remain explicit errors.

Karaoke/effects modifiers, authored text boxes and unsupported display flags
remain reported as requiring native playback. Resource limits remain unchanged:
8 MiB per subtitle payload/document and 32 MiB aggregate. The final document
check includes its header and CSS. The parser does not read compressed video or
audio payloads and does not modify the media file.

## Verification

The actual episode extracts successfully with no unavailable tracks:

| Field | Result |
| --- | --- |
| Output | Default English WebVTT track `s0` |
| Nonempty cues | 470 |
| Document size | 44,702 bytes |
| Used CSS classes | 9 |
| Resource duration | 1,328,330 ms |
| Observed extraction time | 0.177 s, previously read source; uncontrolled OS/SMB caches |

Five new regression tests cover ordinary justification/background defaults,
unsupported layout, style/font/size/color preservation, safe CSS strings,
malformed font tables, style limits, and existing Unicode-range protections.
The complete workspace passes **350 top-level tests**, with six intentional
ignores. One nested child adds a duplicate raw passing result. Strict workspace
all-target/all-feature Clippy, formatting, diff checks and Release build pass.
An initial sandboxed full test run could not access existing native Apple codec
services; the complete run with normal codec-service access passed.

Final Engine SHA-256:
`5242ff164e833e3266c6e686a8b408d264fa970abbaa3b829f54e3d5e8944909`.

Private extraction evidence is retained in
`/private/tmp/chroma-hell-mode-style-fixed/`; logs are
`/private/tmp/chroma-hell-mode-font-{full-tests,clippy,release}.log`.
## Native playback and deployment

An isolated loopback Server served the actual episode with this final Engine.
Mac AVPlayer exposed one native legible option and selected it by default.
Seven nonempty caption callbacks matched the extracted source text hashes:
three during initial playback, two after seeking to 600 seconds and two after
turning captions back on. The Off interval produced no nonempty caption output.
The player produced 258 sampled video frames and no error; external playback
was disabled. Initial HLS item times differed from source cue starts by 83 ms;
post-seek callbacks matched the source starts. This is measured native delivery,
not a claim of sample-exact subtitle timing on every platform.

The actual Chroma tvOS simulator also displayed the English dialogue caption
while playing and paused. The opt-in native-player test passed. Its disposable
fixture placed the selected episode first in the lazy recent shelf and reset
watch progress before the active-cue capture. Screenshots were inspected during
the known first dialogue cue, rather than inferring rendering from a menu.
Font substitution and accessibility preferences remain native-player behavior.

This Engine and Server
`e941209bec40a85b5a98f89d5499e7d9e806601fb56577e77ea9aa2f43c2e442`
are deployed in the managed production service. LAN/loopback health return 200,
unauthenticated playback returns 401, and library matches/watch history retain
their original fingerprints. No production diagnostic device was created.
The isolated sessions were retired and their server stopped. The physical TV
was not accessed; no client application change or installation is required.
