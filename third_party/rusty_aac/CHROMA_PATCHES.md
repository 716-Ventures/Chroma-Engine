# Chroma AAC streaming extension

Upstream: crates.io `rusty_aac` 0.5.0, Apache-2.0, copyright Mata Network.
Original source and license are retained; line endings normalized to LF.

`src/encode.rs` exposes the added `encode::stream` module. Its bounded streaming
adapter retains MDCT overlap, partial PCM, causal transient detector energy and
one block of lookahead. It uses the existing frame quantizer/rate loop with sine
windows. Whole-clip population-relative transient analysis is not streamable and
is deliberately replaced with causal EWMA analysis in this API only. The upstream
one-shot API is unchanged. Streaming work is serial (one caller-owned thread).

Final padding and the 1024-sample codec delay are generated once at EOS. Containers
must represent that delay separately from coded access-unit duration.
