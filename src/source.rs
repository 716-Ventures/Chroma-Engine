use std::{
    fs::{self, File},
    io,
    path::{Path, PathBuf},
    time::SystemTime,
};

use anyhow::{Context, Result};
use memmap2::{Mmap, MmapOptions};
use tempfile::TempDir;

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

/// File-backed media source view plus its original filesystem identity.
pub struct MediaSource {
    path: PathBuf,
    identity: MediaSourceIdentity,
    len: u64,
    bytes: SnapshotBytes,
    _mapped_file: File,
    // Keep this last so Windows closes the mapping and file before cleanup.
    snapshot_dir: Option<TempDir>,
    private_snapshot: bool,
}

enum SnapshotBytes {
    Empty,
    Mapped(Mmap),
}

impl AsRef<[u8]> for SnapshotBytes {
    fn as_ref(&self) -> &[u8] {
        match self {
            Self::Empty => &[],
            Self::Mapped(bytes) => bytes.as_ref(),
        }
    }
}

impl std::fmt::Debug for MediaSource {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("MediaSource")
            .field("path", &self.path)
            .field("identity", &self.identity)
            .field("len", &self.len)
            .field("private_snapshot", &self.private_snapshot)
            .field(
                "snapshot_dir",
                &self.snapshot_dir.as_ref().map(TempDir::path),
            )
            .finish_non_exhaustive()
    }
}

impl MediaSource {
    /// Opens a source and creates a file-backed parser view.
    ///
    /// The snapshot is mapped instead of copied into a file-sized heap buffer. On
    /// copy-on-write filesystems the platform copy is normally a cheap private
    /// clone. Other filesystems retain and validate the original read handle,
    /// avoiding both file-sized heap allocations and eager disk copies.
    pub fn open(path: &Path) -> Result<Self> {
        let file = File::open(path).with_context(|| format!("open {}", path.display()))?;
        let metadata = file
            .metadata()
            .with_context(|| format!("stat {}", path.display()))?;
        let identity = MediaSourceIdentity::from_metadata(&metadata);
        let len = metadata.len();
        usize::try_from(len).with_context(|| {
            format!(
                "source too large to map on this platform: {}",
                path.display()
            )
        })?;

        let candidate_dir = create_snapshot_dir(path).ok();
        let private_snapshot = candidate_dir
            .as_ref()
            .map(|directory| clone_file(path, &directory.path().join("source.snapshot")))
            .transpose()
            .with_context(|| format!("clone snapshot for {}", path.display()))?
            .unwrap_or(false);
        let mapped_file = if private_snapshot {
            let snapshot_path = candidate_dir
                .as_ref()
                .ok_or_else(|| anyhow::anyhow!("private snapshot has no owning directory"))?
                .path()
                .join("source.snapshot");
            seal_snapshot(&snapshot_path)
                .with_context(|| format!("seal snapshot for {}", path.display()))?;
            File::open(&snapshot_path)
                .with_context(|| format!("open snapshot for {}", path.display()))?
        } else {
            file.try_clone()
                .with_context(|| format!("retain read handle for {}", path.display()))?
        };
        let mapped_len = mapped_file
            .metadata()
            .with_context(|| format!("stat mapped source for {}", path.display()))?
            .len();
        let current = fs::metadata(path).with_context(|| format!("restat {}", path.display()))?;
        if mapped_len != len || MediaSourceIdentity::from_metadata(&current) != identity {
            anyhow::bail!("source changed while reading {}", path.display());
        }
        let bytes = if len == 0 {
            SnapshotBytes::Empty
        } else {
            SnapshotBytes::Mapped(
                map_readonly(&mapped_file)
                    .with_context(|| format!("map source for {}", path.display()))?,
            )
        };
        Ok(Self {
            path: path.to_path_buf(),
            identity,
            len,
            bytes,
            _mapped_file: mapped_file,
            snapshot_dir: private_snapshot.then_some(candidate_dir).flatten(),
            private_snapshot,
        })
    }

    /// Returns the source length captured at open time.
    pub fn len(&self) -> u64 {
        self.len
    }

    /// Returns the bytes in the retained source view.
    pub fn as_bytes(&self) -> &[u8] {
        let bytes = self.bytes.as_ref();
        self.read_at(0, bytes.len()).unwrap_or(bytes)
    }

    /// Reads a bounded positional window from the retained source view.
    pub fn read_at(&self, offset: u64, len: usize) -> io::Result<&[u8]> {
        let start = usize::try_from(offset)
            .map_err(|_| io::Error::new(io::ErrorKind::InvalidInput, "offset is too large"))?;
        let end = start.checked_add(len).ok_or_else(|| {
            io::Error::new(io::ErrorKind::InvalidInput, "read range overflows usize")
        })?;
        self.bytes.as_ref().get(start..end).ok_or_else(|| {
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

fn create_snapshot_dir(source_path: &Path) -> io::Result<TempDir> {
    if let Some(parent) = source_path.parent()
        && let Ok(directory) = tempfile::Builder::new()
            .prefix(".chroma-media-source-")
            .tempdir_in(parent)
    {
        return Ok(directory);
    }
    tempfile::Builder::new()
        .prefix("chroma-media-source-")
        .tempdir()
}

#[cfg(target_os = "macos")]
#[allow(unsafe_code)]
fn clone_file(source_path: &Path, snapshot_path: &Path) -> io::Result<bool> {
    use std::{ffi::CString, os::unix::ffi::OsStrExt};

    unsafe extern "C" {
        fn clonefile(
            source: *const std::ffi::c_char,
            destination: *const std::ffi::c_char,
            flags: u32,
        ) -> std::ffi::c_int;
    }

    let source = CString::new(source_path.as_os_str().as_bytes())
        .map_err(|_| io::Error::new(io::ErrorKind::InvalidInput, "source path contains NUL"))?;
    let destination = CString::new(snapshot_path.as_os_str().as_bytes())
        .map_err(|_| io::Error::new(io::ErrorKind::InvalidInput, "snapshot path contains NUL"))?;
    // SAFETY: both pointers reference live, NUL-terminated paths for the
    // duration of the call. The destination is inside a private new directory.
    let result = unsafe { clonefile(source.as_ptr(), destination.as_ptr(), 0) };
    if result == 0 {
        return Ok(true);
    }
    let _ = fs::remove_file(snapshot_path);
    Ok(false)
}

#[cfg(not(target_os = "macos"))]
fn clone_file(_source_path: &Path, _snapshot_path: &Path) -> io::Result<bool> {
    Ok(false)
}

fn seal_snapshot(snapshot_path: &Path) -> io::Result<()> {
    #[cfg(unix)]
    {
        let mut permissions = fs::metadata(snapshot_path)?.permissions();
        permissions.set_readonly(true);
        fs::set_permissions(snapshot_path, permissions)?;
    }
    #[cfg(not(unix))]
    let _ = snapshot_path;
    Ok(())
}

#[allow(unsafe_code)]
fn map_readonly(file: &File) -> io::Result<Mmap> {
    // SAFETY: MediaSource retains the read-only File for the mapping lifetime.
    // Clone-capable filesystems map a sealed private inode. The portable path
    // is used only by sessions that validate source identity before work;
    // callers must not mutate an actively leased source file in place.
    unsafe { MmapOptions::new().map(file) }
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
    fn detects_same_path_replacement_and_preserves_private_snapshots() {
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join("sample.mp4");
        fs::write(&path, b"first").expect("write source");
        let source = MediaSource::open(&path).expect("open source");

        fs::write(&path, b"second").expect("replace source");

        assert!(source.validate_current().is_err());
        if source.private_snapshot {
            assert_eq!(source.as_bytes(), b"first");
        }
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
