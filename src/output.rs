use std::{
    fs::{self, OpenOptions},
    io::{self, Write},
    path::{Path, PathBuf},
    process,
    time::{SystemTime, UNIX_EPOCH},
};

/// Options controlling atomic publication of generated engine outputs.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct PublishOptions {
    /// Force file data to stable storage before publishing.
    pub(crate) sync_data: bool,
    /// Treat an already published byte-identical file as success.
    pub(crate) allow_existing_identical: bool,
}

impl Default for PublishOptions {
    fn default() -> Self {
        Self {
            sync_data: false,
            allow_existing_identical: true,
        }
    }
}

/// Publishes bytes through a unique same-directory temporary file.
pub(crate) fn publish_bytes(path: &Path, bytes: &[u8]) -> io::Result<()> {
    publish_bytes_with_options(path, bytes, PublishOptions::default())
}

/// Publishes bytes through a unique same-directory temporary file using explicit options.
pub(crate) fn publish_bytes_with_options(
    path: &Path,
    bytes: &[u8],
    options: PublishOptions,
) -> io::Result<()> {
    if options.allow_existing_identical && existing_file_matches(path, bytes)? {
        return Ok(());
    }

    if path.exists() {
        return Err(io::Error::new(
            io::ErrorKind::AlreadyExists,
            format!(
                "output already exists with different content: {}",
                path.display()
            ),
        ));
    }

    let parent = path.parent().unwrap_or_else(|| Path::new("."));
    fs::create_dir_all(parent)?;
    let (mut file, tmp_path) = create_unique_temp(parent, path)?;
    let publish_result = (|| {
        file.write_all(bytes)?;
        file.flush()?;
        if options.sync_data {
            file.sync_data()?;
        }
        drop(file);
        fs::rename(&tmp_path, path)
    })();

    if publish_result.is_err() {
        let _ = fs::remove_file(&tmp_path);
    }
    publish_result
}

fn existing_file_matches(path: &Path, bytes: &[u8]) -> io::Result<bool> {
    let Ok(metadata) = fs::metadata(path) else {
        return Ok(false);
    };
    if !metadata.is_file() || metadata.len() != bytes.len() as u64 {
        return Ok(false);
    }
    Ok(fs::read(path)? == bytes)
}

fn create_unique_temp(parent: &Path, target: &Path) -> io::Result<(fs::File, PathBuf)> {
    let stem = target
        .file_name()
        .and_then(|name| name.to_str())
        .unwrap_or("chroma-output");
    for attempt in 0..128_u32 {
        let now = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map(|duration| duration.as_nanos())
            .unwrap_or(0);
        let tmp_path = parent.join(format!(".{stem}.{}.{}.{}.tmp", process::id(), now, attempt));
        match OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&tmp_path)
        {
            Ok(file) => return Ok((file, tmp_path)),
            Err(error) if error.kind() == io::ErrorKind::AlreadyExists => continue,
            Err(error) => return Err(error),
        }
    }
    Err(io::Error::new(
        io::ErrorKind::AlreadyExists,
        format!(
            "could not create unique temporary output beside {}",
            target.display()
        ),
    ))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn publishes_complete_bytes_and_leaves_no_temp_file() {
        let dir = tempfile::tempdir().expect("tempdir");
        let output = dir.path().join("segment.m4s");

        publish_bytes(&output, b"complete segment").expect("publish");

        assert_eq!(fs::read(&output).expect("read output"), b"complete segment");
        let temp_count = fs::read_dir(dir.path())
            .expect("read dir")
            .filter_map(Result::ok)
            .filter(|entry| entry.file_name().to_string_lossy().ends_with(".tmp"))
            .count();
        assert_eq!(temp_count, 0);
    }

    #[test]
    fn accepts_existing_identical_immutable_output() {
        let dir = tempfile::tempdir().expect("tempdir");
        let output = dir.path().join("init.mp4");

        publish_bytes(&output, b"same").expect("initial publish");
        publish_bytes(&output, b"same").expect("identical publish");

        assert_eq!(fs::read(&output).expect("read output"), b"same");
    }

    #[test]
    fn refuses_to_replace_different_immutable_output() {
        let dir = tempfile::tempdir().expect("tempdir");
        let output = dir.path().join("init.mp4");

        publish_bytes(&output, b"old").expect("initial publish");
        let error = publish_bytes(&output, b"new").expect_err("different output rejected");

        assert_eq!(error.kind(), io::ErrorKind::AlreadyExists);
        assert_eq!(fs::read(&output).expect("read output"), b"old");
    }
}
