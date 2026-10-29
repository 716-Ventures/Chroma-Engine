# Native HDR HLS copy qualification

The owner's continued Pacific Rim buffering report exposed a transport difference:
Chroma used progressive MP4 while the shipping Genus client requested HLS. Before
changing delivery, local validation found the existing HLS init changed the source
`hvc1` tag to `hev1` and omitted its explicit color metadata. That HLS defect is
separate from the previous progressive response.

Packet-copy initialization now preserves the validated original HEVC visual sample
entry, bounded to 256 KiB, including color/HDR/Dolby metadata; data_reference_index
is normalized to 1. Generic Matroska HEVC remains consistently `hev1`. Codec strings
retain HEVC constraint bytes and derive native Dolby Vision profile/level from
bounded dvcC/dvvC records. Manifest videoRange follows explicit transfer facts or
the defined Dolby Vision profile 5 PQ transfer; unknown facts remain absent.

The actual source around 569–584 seconds is fast-start and tightly interleaved.
Ten copy segments have continuous audio/video clocks, exact EAC3 packets and an
exact original first video payload. The original visual metadata, hvcC and dec3
are byte-identical. Mac decode of both original and copied local files produces
3840×2160 BT.2020/PQ output without errors. No video decode/encode or audio encode
is used by the packaging worker.

The final suite passes 332 top-level tests, with 6 intentional ignores; 333 raw
passing lines include a nested subprocess duplicate. Strict all-feature Clippy,
formatting and independent source review pass. The Server uses analyzer 11,
plan schema 6 and asset revision `hdr-copy-v6`; its master preserves the exact
codec string and explicit VIDEO-RANGE. Final production-stream evidence belongs
in the adjacent retained JSON. Physical TV behavior is not established by Mac
tests and remains for an explicitly authorized owner-controlled playback test.

Manifest video facts also include positive source dimensions and maximum frame
rate, derived from nonempty MP4 STTS durations or explicit Matroska DefaultDuration.
They permit truthful HLS RESOLUTION/FRAME-RATE attributes. The final master remains
PQ: this SDR Mac rejects that variant with a display-eligibility error while its
media playlist decodes correctly. No false SDR tag is used to force selection.

[Retained proof](2026-10-28-pacific-hls.json) records both final 180-second production media/original-file Mac replays around 569 seconds, exact format/clock checks, diagnostic revocation and unchanged catalog/watch data. The canonical HDR master is not physically qualified: this SDR Mac returns the typed display-eligibility error. No physical TV was accessed for these diagnostics.
