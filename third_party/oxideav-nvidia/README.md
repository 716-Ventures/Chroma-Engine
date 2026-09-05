# Chroma Engine oxideav-nvidia patch

This directory vendors `oxideav-nvidia` 0.0.3 under its MIT license. Chroma Engine keeps the
runtime-loaded CUDA/NVDEC/NVENC design and adds checked high-bit-depth decode output:

- NVDEC selects P016 rather than NV12 when the parser reports more than eight bits per component.
- Device readback treats pitch as bytes and preserves MSB-aligned 16-bit luma and chroma samples.
- Interleaved P016 chroma is split into bounded planar buffers for Chroma's retained decoder.

The local package remains version-pinned so a future upstream release can replace it after the
same Main10 behavior is available and verified there.
