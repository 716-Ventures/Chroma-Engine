use serde::{Deserialize, Serialize};
use std::{error::Error as StdError, fmt};

/// Stable Chroma Engine error code for logs, API responses, and client routing.
#[non_exhaustive]
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
    /// No matching track exists for the requested operation.
    NoMatchingTrack,
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
    /// Playback pipeline stage routing failed.
    PipelineFailed,
    /// Stateful playback session setup or segment extraction failed.
    SessionFailed,
    /// A source changed after the engine opened a session for it.
    SourceChanged,
    /// Input was malformed or internally inconsistent.
    MalformedInput,
    /// Work exceeded an engine resource limit.
    ResourceLimitExceeded,
    /// Native backend execution failed.
    BackendFailed,
}

/// Retry guidance attached to a structured engine error.
#[non_exhaustive]
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum RetryAdvice {
    /// Retrying the same operation without changed inputs is expected to fail.
    DoNotRetry,
    /// Retrying may succeed after source or cache state changes.
    RetryAfterRefresh,
    /// Retrying may succeed when a backend or device resource becomes available.
    RetryLater,
}

/// Structured Chroma Engine error envelope for library callers and machine JSON output.
#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct EngineError {
    code: EngineErrorCode,
    operation: String,
    message: String,
    source_identity: Option<String>,
    track_id: Option<String>,
    segment_index: Option<u32>,
    retry: RetryAdvice,
    #[serde(skip)]
    cause: Option<Box<dyn StdError + Send + Sync + 'static>>,
}

impl EngineError {
    /// Creates a structured error with a stable code and operation label.
    pub fn new(
        code: EngineErrorCode,
        operation: impl Into<String>,
        message: impl Into<String>,
    ) -> Self {
        Self {
            code,
            operation: operation.into(),
            message: message.into(),
            source_identity: None,
            track_id: None,
            segment_index: None,
            retry: RetryAdvice::DoNotRetry,
            cause: None,
        }
    }

    /// Attaches a best-effort source identity, such as a path or content fingerprint.
    pub fn with_source_identity(mut self, source_identity: impl Into<String>) -> Self {
        self.source_identity = Some(source_identity.into());
        self
    }

    /// Attaches the selected Chroma track ID.
    pub fn with_track_id(mut self, track_id: impl Into<String>) -> Self {
        self.track_id = Some(track_id.into());
        self
    }

    /// Attaches the requested segment index.
    pub fn with_segment_index(mut self, segment_index: u32) -> Self {
        self.segment_index = Some(segment_index);
        self
    }

    /// Attaches retry guidance for callers and orchestration layers.
    pub fn with_retry(mut self, retry: RetryAdvice) -> Self {
        self.retry = retry;
        self
    }

    /// Attaches the underlying typed cause for Rust error chaining.
    pub fn with_cause<E>(mut self, cause: E) -> Self
    where
        E: StdError + Send + Sync + 'static,
    {
        self.cause = Some(Box::new(cause));
        self
    }

    /// Returns the stable machine-readable error code.
    pub fn code(&self) -> EngineErrorCode {
        self.code
    }

    /// Returns the operation label where the error occurred.
    pub fn operation(&self) -> &str {
        &self.operation
    }

    /// Returns the human-readable message.
    pub fn message(&self) -> &str {
        &self.message
    }

    /// Returns the optional source identity.
    pub fn source_identity(&self) -> Option<&str> {
        self.source_identity.as_deref()
    }

    /// Returns the optional Chroma track ID.
    pub fn track_id(&self) -> Option<&str> {
        self.track_id.as_deref()
    }

    /// Returns the optional segment index.
    pub fn segment_index(&self) -> Option<u32> {
        self.segment_index
    }

    /// Returns retry guidance for this failure.
    pub fn retry(&self) -> RetryAdvice {
        self.retry
    }
}

impl fmt::Display for EngineError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{} failed: {}", self.operation, self.message)
    }
}

impl StdError for EngineError {
    fn source(&self) -> Option<&(dyn StdError + 'static)> {
        self.cause
            .as_deref()
            .map(|cause| cause as &(dyn StdError + 'static))
    }
}
