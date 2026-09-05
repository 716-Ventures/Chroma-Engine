# Chroma Engine Mediaway decoder patch

This directory vendors the narrow Windows Media Foundation decoder portion of Mediaway 0.1.4
under its MIT license. It routes HEVC Main/Main10 through the existing hardware-only Media Foundation
and D3D11 zero-copy path in addition to H.264. Chroma converts hvcC configuration and
length-prefixed HEVC samples to Annex-B before submission and performs checked NV12/P010 texture
readback.

The local package remains pinned to the matching Mediaway common API so it can be replaced by an
upstream release once that public decoder dispatch supports the same hardware path.
