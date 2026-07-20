use std::path::Path;

use thiserror::Error;

use crate::{
    container::{ContainerKind, sniff_container},
    error::EngineErrorCode,
};

/// Error returned while remuxing a source into MP4.
#[derive(Debug, Error)]
pub enum RemuxError {
    /// Source I/O failed.
    #[error(transparent)]
    Io(#[from] std::io::Error),
    /// The source container is not supported.
    #[error("unsupported container: {0}")]
    UnsupportedContainer(&'static str),
    /// The remux writer for this container is not implemented yet.
    #[error("remux writer is not implemented for {0}")]
    NotImplemented(&'static str),
}

impl RemuxError {
    /// Returns the stable Chroma Engine error code for this remux failure.
    pub fn code(&self) -> EngineErrorCode {
        match self {
            Self::Io(_) => EngineErrorCode::SourceReadFailed,
            Self::UnsupportedContainer(_) => EngineErrorCode::UnsupportedContainer,
            Self::NotImplemented(_) => EngineErrorCode::OperationNotImplemented,
        }
    }
}

/// Remuxes a supported source into MP4 without decoding.
pub fn remux_mp4(input: &Path, _output: &Path) -> Result<(), RemuxError> {
    let mut file = std::fs::File::open(input)?;
    let mut head = [0_u8; 4096];
    let n = std::io::Read::read(&mut file, &mut head)?;
    let kind = sniff_container(&head[..n]);
    match kind {
        ContainerKind::Matroska | ContainerKind::Webm | ContainerKind::Mp4 | ContainerKind::Mov => {
            Err(RemuxError::NotImplemented(kind.public_name()))
        }
        ContainerKind::Unknown => Err(RemuxError::UnsupportedContainer(kind.public_name())),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn remux_errors_expose_stable_codes() {
        assert_eq!(
            RemuxError::UnsupportedContainer("unknown").code(),
            EngineErrorCode::UnsupportedContainer
        );
        assert_eq!(
            RemuxError::NotImplemented("mp4").code(),
            EngineErrorCode::OperationNotImplemented
        );
    }
}
