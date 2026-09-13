//! Host-side admission and hard deadlines for CLI workers. Use one shared
//! runtime across supervisors; independent server processes still require an
//! external admission coordinator and OS/container memory limits.
use std::{
    ffi::OsString,
    io::Read,
    path::PathBuf,
    process::{Child, Command, Stdio},
    sync::Arc,
    time::{Duration, Instant},
};

/// Supervises CLI workers outside uninterruptible native codec calls.
#[derive(Debug)]
pub struct WorkerSupervisor {
    runtime: Arc<crate::EngineRuntime>,
    executable: PathBuf,
    timeout: Duration,
}

/// Bounded stdout/stderr and successful worker timing.
#[derive(Debug)]
pub struct WorkerOutput {
    /// Machine-readable worker output, limited to 4 MiB.
    pub stdout: Vec<u8>,
    /// Worker diagnostics, limited to 4 MiB.
    pub stderr: Vec<u8>,
    /// Total supervised wall time.
    pub elapsed: Duration,
}

struct ReapedChild(Child);
impl Drop for ReapedChild {
    fn drop(&mut self) {
        if self.0.try_wait().ok().flatten().is_none() {
            let _ = self.0.kill();
        }
        let _ = self.0.wait();
    }
}

impl WorkerSupervisor {
    /// Creates a supervisor sharing the host's worker reservations.
    pub fn new(
        runtime: Arc<crate::EngineRuntime>,
        executable: PathBuf,
        timeout: Duration,
    ) -> anyhow::Result<Self> {
        if timeout.is_zero() {
            anyhow::bail!("worker timeout must be positive");
        }
        Ok(Self {
            runtime,
            executable,
            timeout,
        })
    }

    /// Runs one CLI command, killing and reaping it on cancellation/deadline.
    /// Output is spooled to temporary files to avoid pipe backpressure deadlocks;
    /// oversized diagnostics terminate the child at the next 10 ms checkpoint.
    pub fn run(
        &self,
        arguments: &[OsString],
        control: &crate::WorkControl,
    ) -> anyhow::Result<WorkerOutput> {
        control.check()?;
        let _lease = self.runtime.admit()?;
        let start = Instant::now();
        let mut stdout = tempfile::tempfile()?;
        let mut stderr = tempfile::tempfile()?;
        let mut policy = self.runtime.policy().clone();
        policy.aggregate_memory_bytes = policy.session_memory_bytes;
        policy.max_sessions = 1;
        let child = Command::new(&self.executable)
            .args(arguments)
            .env("CHROMA_RESOURCE_POLICY", serde_json::to_string(&policy)?)
            .stdin(Stdio::null())
            .stdout(stdout.try_clone()?)
            .stderr(stderr.try_clone()?)
            .spawn()?;
        let mut child = ReapedChild(child);
        const OUTPUT_LIMIT: u64 = 4 * 1024 * 1024;
        let status = loop {
            control.check()?;
            if start.elapsed() >= self.timeout {
                anyhow::bail!("engine worker exceeded hard deadline");
            }
            if stdout.metadata()?.len() > OUTPUT_LIMIT || stderr.metadata()?.len() > OUTPUT_LIMIT {
                anyhow::bail!("engine worker diagnostic output exceeded 4 MiB");
            }
            if let Some(status) = child.0.try_wait()? {
                break status;
            }
            std::thread::sleep(Duration::from_millis(10));
        };
        fn read_output(file: &mut std::fs::File) -> std::io::Result<Vec<u8>> {
            use std::io::{Seek, SeekFrom};
            file.seek(SeekFrom::Start(0))?;
            let mut bytes = Vec::new();
            file.take(OUTPUT_LIMIT + 1).read_to_end(&mut bytes)?;
            if bytes.len() as u64 > OUTPUT_LIMIT {
                return Err(std::io::Error::other("worker output limit exceeded"));
            }
            Ok(bytes)
        }
        let stdout = read_output(&mut stdout)?;
        let stderr = read_output(&mut stderr)?;
        if !status.success() {
            anyhow::bail!(
                "engine worker failed ({status}): {}",
                String::from_utf8_lossy(&stderr)
            );
        }
        Ok(WorkerOutput {
            stdout,
            stderr,
            elapsed: start.elapsed(),
        })
    }
}
