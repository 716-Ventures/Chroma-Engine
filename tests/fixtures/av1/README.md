# AV1 Decoder Fixtures

These packet fixtures were generated from FFmpeg's synthetic `testsrc2` source with SVT-AV1. They contain no third-party audiovisual content and may be redistributed with Chroma Engine's tests.

Each file starts with `width height timescale`. Remaining lines contain `index pts duration keyframe payload_hex`. The fixtures deliberately use multiple packets so tests exercise retained decoder state and end-of-stream draining.
