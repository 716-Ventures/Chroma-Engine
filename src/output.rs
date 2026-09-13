use std::{
    fs::{self, OpenOptions},
    io::{self, Read, Write},
    path::{Path, PathBuf},
    process,
    time::{SystemTime, UNIX_EPOCH},
};

/// Checks the cache filesystem's required no-replace hard-link operation.
/// Run once when the host selects a cache directory, before expensive work.
/// The probe creates and removes only files in its own temporary directory.
pub fn validate_cache_directory(directory: &Path) -> io::Result<()> {
    fs::create_dir_all(directory)?;
    let probe = tempfile::Builder::new()
        .prefix(".chroma-cache-check-")
        .tempdir_in(directory)?;
    let original = probe.path().join("original");
    let published = probe.path().join("published");
    fs::write(&original, b"chroma-cache-probe")?;
    fs::hard_link(&original, &published).map_err(|error| {
        io::Error::new(error.kind(), format!("cache {} does not support required atomic hard-link publication: {error}; select a compatible local cache filesystem", directory.display()))
    })?;
    match fs::hard_link(&original, &published) {
        Err(error) if error.kind() == io::ErrorKind::AlreadyExists => {}
        Err(error) => return Err(error),
        Ok(()) => {
            return Err(io::Error::other(
                "cache filesystem did not enforce no-replace publication",
            ));
        }
    }
    probe.close()
}

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

/// Streams an immutable output through a unique same-directory temporary file.
pub(crate) fn publish_file<E, F>(path: &Path, write: F) -> Result<(), E>
where
    E: From<io::Error>,
    F: FnOnce(&mut fs::File) -> Result<(), E>,
{
    if path.exists() {
        return Err(io::Error::new(
            io::ErrorKind::AlreadyExists,
            format!("output already exists: {}", path.display()),
        )
        .into());
    }

    let parent = path.parent().unwrap_or_else(|| Path::new("."));
    fs::create_dir_all(parent)?;
    let (mut file, tmp_path) = create_unique_temp(parent, path)?;
    let publish_result = (|| {
        write(&mut file)?;
        file.flush()?;
        drop(file);
        fs::hard_link(&tmp_path, path)?;
        fs::remove_file(&tmp_path)?;
        Ok(())
    })();

    if publish_result.is_err() {
        let _ = fs::remove_file(&tmp_path);
    }
    publish_result
}

/// Publishes bytes through a unique same-directory temporary file using explicit options.
pub(crate) fn publish_bytes_with_options(
    path: &Path,
    bytes: &[u8],
    options: PublishOptions,
) -> io::Result<()> {
    publish_parts_with_options(path, &[bytes], options)
}

pub(crate) fn publish_parts(path: &Path, parts: &[&[u8]]) -> io::Result<()> {
    publish_parts_with_options(path, parts, PublishOptions::default())
}

fn publish_parts_with_options(
    path: &Path,
    parts: &[&[u8]],
    options: PublishOptions,
) -> io::Result<()> {
    if options.allow_existing_identical && existing_file_matches_parts(path, parts)? {
        return Ok(());
    }
    if path.exists() {
        return Err(io::Error::new(
            io::ErrorKind::AlreadyExists,
            "output already exists with different content",
        ));
    }
    let parent = path.parent().unwrap_or_else(|| Path::new("."));
    fs::create_dir_all(parent)?;
    let (mut file, temporary) = create_unique_temp(parent, path)?;
    let result = (|| {
        for part in parts {
            file.write_all(part)?;
        }
        file.flush()?;
        if options.sync_data {
            file.sync_data()?;
        }
        drop(file);
        match fs::hard_link(&temporary, path) {
            Ok(()) => {}
            Err(error)
                if error.kind() == io::ErrorKind::AlreadyExists
                    && options.allow_existing_identical
                    && existing_file_matches_parts(path, parts)? => {}
            Err(error) => return Err(error),
        }
        fs::remove_file(&temporary)
    })();
    if result.is_err() {
        let _ = fs::remove_file(&temporary);
    }
    result
}

#[cfg(test)]
fn existing_file_matches(path: &Path, bytes: &[u8]) -> io::Result<bool> {
    existing_file_matches_parts(path, &[bytes])
}

fn existing_file_matches_parts(path: &Path, parts: &[&[u8]]) -> io::Result<bool> {
    let mut file = match fs::File::open(path) {
        Ok(file) => file,
        Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(false),
        Err(error) => return Err(error),
    };
    let metadata = file.metadata()?;
    if !metadata.is_file()
        || metadata.len()
            != parts.iter().try_fold(0u64, |sum, part| {
                sum.checked_add(part.len() as u64)
                    .ok_or_else(|| io::Error::other("output size overflow"))
            })?
    {
        return Ok(false);
    }
    let mut buffer = [0_u8; 16 * 1024];
    for chunk in parts.iter().flat_map(|part| part.chunks(16 * 1024)) {
        match file.read_exact(&mut buffer[..chunk.len()]) {
            Ok(()) => {}
            Err(error) if error.kind() == io::ErrorKind::UnexpectedEof => return Ok(false),
            Err(error) => return Err(error),
        }
        if &buffer[..chunk.len()] != chunk {
            return Ok(false);
        }
    }
    Ok(file.read(&mut buffer[..1])? == 0)
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
    fn cache_capability_probe_cleans_up_its_files() {
        let dir = tempfile::tempdir().unwrap();
        validate_cache_directory(dir.path()).unwrap();
        assert_eq!(fs::read_dir(dir.path()).unwrap().count(), 0);
    }

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

    #[test]
    fn concurrent_different_publishers_cannot_replace_the_winner() {
        use std::sync::{Arc, Barrier};

        let dir = tempfile::tempdir().expect("tempdir");
        let output = Arc::new(dir.path().join("segment.m4s"));
        let barrier = Arc::new(Barrier::new(3));
        let handles = [b"first".as_slice(), b"second".as_slice()].map(|bytes| {
            let output = Arc::clone(&output);
            let barrier = Arc::clone(&barrier);
            std::thread::spawn(move || {
                barrier.wait();
                publish_bytes(&output, bytes)
            })
        });
        barrier.wait();

        let results = handles.map(|handle| handle.join().expect("publisher thread"));
        assert_eq!(results.iter().filter(|result| result.is_ok()).count(), 1);
        let published = fs::read(&*output).expect("published output");
        assert!(published == b"first" || published == b"second");
    }

    #[test]
    fn streaming_output_is_invisible_until_complete() {
        use std::sync::{Arc, mpsc};

        let dir = tempfile::tempdir().expect("tempdir");
        let output = Arc::new(dir.path().join("movie.mp4"));
        let worker_output = Arc::clone(&output);
        let (prefix_ready_tx, prefix_ready_rx) = mpsc::channel();
        let (finish_tx, finish_rx) = mpsc::channel();
        let worker = std::thread::spawn(move || {
            publish_file::<io::Error, _>(&worker_output, |file| {
                file.write_all(b"prefix")?;
                prefix_ready_tx.send(()).expect("signal prefix");
                finish_rx.recv().expect("finish signal");
                file.write_all(b"-suffix")?;
                Ok(())
            })
        });

        prefix_ready_rx.recv().expect("prefix signal");
        assert!(!output.exists());
        finish_tx.send(()).expect("finish signal");
        worker.join().expect("publisher thread").expect("publish");
        assert_eq!(fs::read(&*output).expect("read output"), b"prefix-suffix");
    }
}
#[test]
fn comparison_checks_every_bounded_chunk_and_the_tail() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("segment.m4s");
    let mut bytes = vec![7; 32 * 1024 + 3];
    fs::write(&path, &bytes).unwrap();
    assert!(existing_file_matches(&path, &bytes).unwrap());
    bytes[32 * 1024 + 2] = 8;
    assert!(!existing_file_matches(&path, &bytes).unwrap());
}
