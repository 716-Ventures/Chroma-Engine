# Modern media support profile

Chroma Engine is intentionally a focused media pipeline, not a general-purpose codec archive.
The accepted source-container boundary is:

- ISO BMFF: MP4, M4V, and MOV;
- Matroska: MKV and WebM.

AVI, MPEG-PS/VOB, source MPEG-TS/M2TS, ASF/WMV, Ogg, FLV, RealMedia, and other legacy or
special-purpose containers are rejected. Applications should report that boundary directly rather
than silently invoking an external compatibility transcoder.

## Codec profile

| Media | Accepted inputs | Playback behavior |
|---|---|---|
| Video | H.264/AVC | Packet copy when the target accepts it; otherwise native H.264 encode |
| Video | HEVC Main/Main10 | Packet copy for compatible targets or native decode to H.264 |
| Video | AV1, 8/10/12-bit | Native `dav1d-rs` decode to H.264 |
| Audio | AAC-LC, AC-3, E-AC-3 | Packet copy when the target accepts it |
| Audio | FLAC, ALAC, MP3 | Packet copy in supported fMP4/native paths |
| Audio | Opus | Pure-Rust decode and AAC-LC bridge |
| Audio | DTS/DTS-HD core, TrueHD | Native decode and AAC-LC bridge, up to six output channels |
| Subtitles | WebVTT and supported text subtitle tracks | Native sidecar conversion/rendition |

VP8/VP9, MPEG-1/2/4 Part 2, VC-1, Theora, and other legacy video codecs are outside the
transcoding profile. Bitmap subtitles are identified by probing but are not burned in. Opus
mapping families other than 0 and 1, nonstandard channel maps, and layouts above eight input
channels are rejected explicitly.

Codec presence alone does not guarantee playback: malformed configuration records, encrypted
tracks, unsupported profiles, and container/codec combinations that cannot be represented in the
target output are rejected with a typed engine error.

## Platform policy

The same portable H.264, HEVC, AV1, Opus, DTS, TrueHD, AAC, AC-3, and E-AC-3 implementations are
available on macOS, Windows, Linux, and Linux-based NAS systems on x86-64 and ARM64. Hardware paths
are optional accelerators:

- macOS: VideoToolbox;
- Windows: Media Foundation, Intel Quick Sync, or NVIDIA where runtime probes succeed;
- Linux/NAS: VA-API, Intel Quick Sync, or NVIDIA where enabled and runtime probes succeed.

When a hardware probe fails, the engine uses the portable implementation. Release bundles must
ship the correct native Chroma Engine artifact and its declared runtime libraries for the target;
they must not add FFmpeg or ffprobe as a fallback.
