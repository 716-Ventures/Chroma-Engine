# Recovering missing Dolby Vision Profile 5 signalling

A real MP4 fixture contains CRC-valid Profile 5 RPUs in its HEVC packets but has
an ordinary `hvc1` sample entry with neither `dvcC` nor explicit colour metadata.
The previous probe reports unknown dynamic range. Packet copying preserves the
RPUs but cannot tell an Apple decoder to interpret them as Dolby Vision.

The positional metadata reader now recovers a missing description only when a
bounded packet inspection validates Profile 5 with the native MIT-licensed
`dolby_vision` parser, including CRC. It requires 10-bit 4:2:0 HEVC, a single sample
description, measured maximum frame rate, known geometry and declared maximum
bitrate. Level selection follows Dolby's width, pixel-rate and main/high-tier
limits up to level 7. Existing authored HDR/Dolby descriptions are preserved.
Profile 7/8, invalid CRC, enhancement NALs and uncertain facts are not relabelled.

At most four leading tracks, two first-chunk samples, 2 MiB of payload, 1,024 NALs
per sample and 16 KiB per RPU are inspected. Reads use the existing cancellation,
source-generation and bounds checks. Only the in-memory metadata view is changed;
source packet offsets/timing remain absolute and the original media is read-only.
A recovered `dvh1`/`dvhe` entry contains its Profile 5 record and exposes PQ and the
matching codec string to existing copy/remux/probe paths.

The reported fixture now probes as Dolby Vision, produces `dvh1.05.06`/PQ, and
both ordinary fMP4 and the server's native video-copy initialization path contain
the restored record. Apple AVFoundation recognizes the offline fragment as
`dvh1` and exposes `dvcC`. The first segment's compressed audio/video payload is
byte-identical to the prior Engine's output (767,112 bytes); this is a bounded
segment comparison, not a full-file checksum or a physical-display colour test.
No TV operation was performed. Actual TV rendering remains user verification.

The complete workspace passes 418 top-level tests (419 including a nested child),
with eight intentional ignores. Strict all-target/all-feature Clippy, formatting,
optimized build and the generated dependency-license inventory pass. Regressions
cover valid generated RPUs, corrupt CRC, ordinary HEVC, incompatible profiles,
truncation, packet-address preservation and preserving authored descriptions.

References: [Dolby levels](https://ott.dolby.com/OnDelKits/Dolby_Vision_Online_Delivery_Kit/v1/Documentation/Specs/Visio_Profiles/help_files/topics/c_dovi_levels.html),
[Apple HLS signalling](https://developer.apple.com/streaming/hls-authoring-specification-for-apple-devices.html),
[native RPU parser](https://github.com/quietvoid/dovi_tool/tree/main/dolby_vision).
