use std::path::Path;

use anyhow::{Context, Result};
use memmap2::Mmap;

#[derive(Debug)]
pub struct MappedMediaFile {
    bytes: Mmap,
    len: u64,
}

impl MappedMediaFile {
    pub fn open(path: &Path) -> Result<Self> {
        let file = std::fs::File::open(path).with_context(|| format!("open {}", path.display()))?;
        let len = file
            .metadata()
            .with_context(|| format!("stat {}", path.display()))?
            .len();
        #[allow(unsafe_code)]
        // SAFETY: Chroma maps immutable media inputs and only exposes shared byte slices.
        // Callers must treat source files as read-only while an engine operation is active.
        let bytes =
            unsafe { Mmap::map(&file) }.with_context(|| format!("map {}", path.display()))?;
        Ok(Self { bytes, len })
    }

    pub fn len(&self) -> u64 {
        self.len
    }

    pub fn is_empty(&self) -> bool {
        self.len == 0
    }

    pub fn as_bytes(&self) -> &[u8] {
        self.bytes.as_ref()
    }
}

impl AsRef<[u8]> for MappedMediaFile {
    fn as_ref(&self) -> &[u8] {
        self.as_bytes()
    }
}
