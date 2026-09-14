# Audit and benchmark reporting

Use neutral fixture identifiers such as `media-fixture-01` for real-media inputs
in public documentation, report labels, filenames, test data, and release notes.
Do not include movie titles, library folder names, or identifying source paths.
Do not commit a mapping from fixture identifiers to titles.

Describe relevant technical properties instead: container, codecs, profiles,
resolution, bit depth, duration, and the workload being measured. Keep identifiers
consistent across reports discussing the same input. Distinguish real-media
fixtures from original synthetic fixtures; anonymizing a name does not make the
underlying media redistributable.

Before publishing a report, inspect command lines, stderr, embedded metadata,
labels, and output paths for identifying text. Use neutral filenames and labels
when invoking benchmark tools; automatic path removal is not a substitute for
reviewing the output. Preserve measurements and qualification caveats when
anonymizing existing reports.
