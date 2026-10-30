# Progressive transport for recovered Dolby Vision MP4

The prior recovery qualified the configuration and offline fragment but did not
qualify the physical TV's HLS selection. A user-driven retry, with permission to
read logs only, records CoreMedia -12927 / AVFoundation -11800. The API response
decodes using actual tvOS DTOs, and the isolated HLS master is served successfully.
The corrected offline fragment decodes twelve frames in Apple's native reader.
Those checks establish that metadata parsing alone did not qualify playback.

A native `mp4-dolby-overlay` command now publishes only the repaired movie header.
The read-only progressive view labels the old moov as free space and appends the
new header, retaining every original packet address and byte. It neither copies
the full movie nor rewrites the source. Unbounded top-level atoms, multiple moovs,
source replacement and compact/incomplete metadata are rejected. Header size is
bounded by the existing metadata policy and 64 MiB. Native atomic publication
also rejects attempts to overwrite the source.

The server uses this transport for native tvOS playback when Profile 5 recovery
is required and the default audio is natively compatible. Other sources retain
the existing transport/bridge. The usual scoped playback session owns the virtual
MP4; source/header byte ranges remain seekable. Header caches are keyed by source
path/size/mtime and Engine identity. Old inactive entries are pruned with a
two-minute grace; active HTTP responses retain their open file descriptors.

The full Engine workspace passes 418 top-level tests (419 with its nested child),
eight intentional ignores, strict Clippy, formatting and an optimized build.
The new source-immutability and header tests pass. No dependency change is made
in this follow-up. Real-display output remains user verification; no TV app was
launched, stopped or installed, and no display setting was changed by the agent.
