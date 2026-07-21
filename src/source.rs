use std::{
    fs::{self, File},
    io::{self, Read},
    path::{Path, PathBuf},
    time::SystemTime,
};

use anyhow::{Context, Result};

/// Immutable identity captured when a media source is opened.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MediaSourceIdentity {
    len: u64,
    modified: Option<SystemTime>,
    #[cfg(unix)]
    device: u64,
    #[cfg(unix)]
    inode: u64,
}

impl MediaSourceIdentity {
    fn from_metadata(metadata: &fs::Metadata) -> Self {
        #[cfg(unix)]
        use std::os::unix::fs::MetadataExt;

        Self {
            len: metadata.len(),
            modified: metadata.modified().ok(),
            #[cfg(unix)]
            device: metadata.dev(),
            #[cfg(unix)]
            inode: metadata.ino(),
        }
    }
}

/// Owned immutable media source bytes plus the filesystem identity they came from.
#[derive(Debug)]
pub struct MediaSource {
    path: PathBuf,
    identity: MediaSourceIdentity,
    bytes: Vec<u8>,
    len: u64,
}

impl MediaSource {
    /// Opens a source file and snapshots its bytes for parser-safe access.
    pub fn open(path: &Path) -> Result<Self> {
        let mut file = File::open(path).with_context(|| format!("open {}", path.display()))?;
        let metadata = file
            .metadata()
            .with_context(|| format!("stat {}", path.display()))?;
        let identity = MediaSourceIdentity::from_metadata(&metadata);
        let len = metadata.len();
        let capacity = usize::try_from(len).with_context(|| {
            format!(
                "source too large to index on this platform: {}",
                path.display()
            )
        })?;
        let mut bytes = Vec::new();
        bytes
            .try_reserve_exact(capacity)
            .with_context(|| format!("reserve source buffer {}", path.display()))?;
        file.read_to_end(&mut bytes)
            .with_context(|| format!("read {}", path.display()))?;
        if bytes.len() as u64 != len {
            anyhow::bail!("source changed while reading {}", path.display());
        }
        Ok(Self {
            path: path.to_path_buf(),
            identity,
            bytes,
            len,
        })
    }

    /// Returns the source length captured at open time.
    pub fn len(&self) -> u64 {
        self.len
    }

    /// Returns a stable byte snapshot of the source.
    pub fn as_bytes(&self) -> &[u8] {
        let _ = self.validate_current();
        self.read_at(0, self.bytes.len())
            .unwrap_or(self.bytes.as_slice())
    }

    /// Reads a bounded positional window from the immutable source snapshot.
    pub fn read_at(&self, offset: u64, len: usize) -> io::Result<&[u8]> {
        let start = usize::try_from(offset)
            .map_err(|_| io::Error::new(io::ErrorKind::InvalidInput, "offset is too large"))?;
        let end = start.checked_add(len).ok_or_else(|| {
            io::Error::new(io::ErrorKind::InvalidInput, "read range overflows usize")
        })?;
        self.bytes.get(start..end).ok_or_else(|| {
            io::Error::new(io::ErrorKind::UnexpectedEof, "read range is outside source")
        })
    }

    /// Verifies that the path still points at the same source identity.
    pub fn validate_current(&self) -> Result<()> {
        let current =
            fs::metadata(&self.path).with_context(|| format!("stat {}", self.path.display()))?;
        let current = MediaSourceIdentity::from_metadata(&current);
        if current != self.identity {
            anyhow::bail!("source changed: {}", self.path.display());
        }
        Ok(())
    }
}

impl AsRef<[u8]> for MediaSource {
    fn as_ref(&self) -> &[u8] {
        self.as_bytes()
    }
}

/// Compatibility wrapper for callers that still use the old mapped-source name.
pub type MappedMediaFile = MediaSource;

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn snapshots_bytes_and_detects_same_path_replacement() {
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join("sample.mp4");
        fs::write(&path, b"first").expect("write source");
        let source = MediaSource::open(&path).expect("open source");

        fs::write(&path, b"second").expect("replace source");

        assert_eq!(source.as_bytes(), b"first");
        assert!(source.validate_current().is_err());
    }

    #[test]
    fn read_at_bounds_positional_windows() {
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join("sample.mp4");
        fs::write(&path, b"abcdef").expect("write source");
        let source = MediaSource::open(&path).expect("open source");

        assert_eq!(source.read_at(2, 3).expect("read"), b"cde");
        assert_eq!(
            source.read_at(5, 2).expect_err("out of range").kind(),
            io::ErrorKind::UnexpectedEof
        );
    }
}
