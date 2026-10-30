//! Source-bound reusable detection evidence, independent of segment thresholds.
use anyhow::{Result, ensure};
use serde::{Serialize, de::DeserializeOwned};
use sha2::{Digest, Sha256};
use std::{
    fs::{self, OpenOptions},
    io::{Read, Write},
    path::{Path, PathBuf},
    time::UNIX_EPOCH,
};
const LIMIT: usize = 4 * 1024 * 1024;
pub(super) struct EvidenceCache {
    source: PathBuf,
    stamp: String,
    path: PathBuf,
}
fn stamp(path: &Path) -> Result<String> {
    let meta = fs::metadata(path)?;
    ensure!(meta.is_file(), "Analysis source is not a regular file");
    let mut hash = Sha256::new();
    hash.update(path.to_string_lossy().as_bytes());
    hash.update(meta.len().to_le_bytes());
    hash.update(
        meta.modified()?
            .duration_since(UNIX_EPOCH)?
            .as_nanos()
            .to_le_bytes(),
    );
    #[cfg(unix)]
    {
        use std::os::unix::fs::MetadataExt;
        hash.update(meta.dev().to_le_bytes());
        hash.update(meta.ino().to_le_bytes());
    }
    Ok(format!("{:x}", hash.finalize()))
}
impl EvidenceCache {
    pub fn open(source: &Path, root: &Path, kind: &str) -> Result<Self> {
        let source = source.canonicalize()?;
        let stamp = stamp(&source)?;
        let directory = root.join(kind);
        fs::create_dir_all(&directory)?;
        let meta = directory.symlink_metadata()?;
        ensure!(
            meta.is_dir() && !meta.file_type().is_symlink(),
            "Evidence cache must be a regular directory"
        );
        Ok(Self {
            source,
            path: directory.join(format!("{stamp}.json")),
            stamp,
        })
    }
    pub fn validate(&self) -> Result<()> {
        ensure!(
            stamp(&self.source)? == self.stamp,
            "Analysis source changed"
        );
        Ok(())
    }
    pub fn read<T: DeserializeOwned>(&self) -> Option<T> {
        (|| -> Result<T> {
            self.validate()?;
            let meta = self.path.symlink_metadata()?;
            ensure!(
                meta.is_file() && !meta.file_type().is_symlink() && meta.len() <= LIMIT as u64,
                "Invalid evidence entry"
            );
            let mut options = OpenOptions::new();
            options.read(true);
            #[cfg(unix)]
            {
                use std::os::unix::fs::OpenOptionsExt;
                options.custom_flags(libc::O_NOFOLLOW);
            }
            let file = options.open(&self.path)?;
            let mut bytes = Vec::new();
            file.take((LIMIT + 1) as u64).read_to_end(&mut bytes)?;
            ensure!(bytes.len() <= LIMIT, "Evidence cache entry too large");
            let value = serde_json::from_slice(&bytes)?;
            self.validate()?;
            Ok(value)
        })()
        .ok()
    }
    pub fn write<T: Serialize>(&self, value: &T) -> Result<()> {
        self.validate()?;
        let bytes = serde_json::to_vec(value)?;
        ensure!(bytes.len() <= LIMIT, "Evidence cache entry too large");
        let mut temporary = tempfile::NamedTempFile::new_in(self.path.parent().unwrap())?;
        temporary.write_all(&bytes)?;
        temporary.flush()?;
        temporary.persist(&self.path)?;
        self.validate()
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn replacement_and_corrupt_evidence_never_reuse_old_data() {
        let dir = tempfile::tempdir().unwrap();
        let source = dir.path().join("source");
        fs::write(&source, b"one").unwrap();
        let cache = EvidenceCache::open(&source, dir.path(), "visual").unwrap();
        cache.write(&vec![1_u32, 2]).unwrap();
        assert_eq!(cache.read::<Vec<u32>>().unwrap(), [1, 2]);
        fs::write(&source, b"changed").unwrap();
        assert!(cache.read::<Vec<u32>>().is_none());
        assert!(cache.write(&vec![3]).is_err());
    }
    #[cfg(unix)]
    #[test]
    fn symlink_cache_is_rejected() {
        let dir = tempfile::tempdir().unwrap();
        let source = dir.path().join("source");
        fs::write(&source, b"one").unwrap();
        std::os::unix::fs::symlink(dir.path(), dir.path().join("linked")).unwrap();
        assert!(EvidenceCache::open(&source, dir.path(), "linked").is_err());
    }
}
