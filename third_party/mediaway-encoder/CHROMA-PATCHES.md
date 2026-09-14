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

Only the Rust `rlib` is built. The upstream combined `cdylib`/`rlib` target
produces colliding filenames when release integration tests build both abort
and unwind variants, causing mismatched Mediaway types. Chroma does not consume
or distribute the encoder as a standalone DLL.
