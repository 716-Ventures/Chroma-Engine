# Chroma patch

Source: https://github.com/nyxways/mediaway/tree/ad056ebf139a624a024016727030365eca9cfa08/crates/mediaway-encoder

The source is retained from Mediaway 0.1.4. The standalone manifest resolves
workspace dependencies to the same pinned revision and preserves upstream licenses.

NVENC's dependency and implementation are restricted to Windows x86/x64.
Windows ARM64 uses the existing unsupported-backend stub, allowing automatic
encoder selection to continue to Media Foundation instead of failing to compile
the x86-only nvenc crate. Other encoder backends are unchanged.

The host WebAudio configuration import is gated by the audio feature so the
video-only dependency remains warning-free when compiled from a local path.
