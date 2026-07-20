use serde::{Deserialize, Serialize};

/// Stable Chroma Engine error code for logs, API responses, and client routing.
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum EngineErrorCode {
    /// A requested source path does not exist.
    FileNotFound,
    /// The engine could not open a source file.
    SourceOpenFailed,
    /// The engine could not read source bytes.
    SourceReadFailed,
    /// The engine could not memory-map a source file.
    SourceMapFailed,
    /// Filesystem I/O failed while producing output.
    OutputIoFailed,
    /// A source container is unsupported for the requested operation.
    UnsupportedContainer,
    /// A requested operation is intentionally not implemented yet.
    OperationNotImplemented,
    /// Native HLS cannot support the requested source or option.
    HlsUnsupported,
    /// Native HLS failed inside a lower-level parser or muxer.
    HlsInternal,
    /// AAC parsing or framing failed.
    AacFailed,
    /// H.264 parsing or conversion failed.
    H264Failed,
    /// HEVC parsing or conversion failed.
    HevcFailed,
    /// Encoder warmup failed.
    EncoderWarmupFailed,
}
