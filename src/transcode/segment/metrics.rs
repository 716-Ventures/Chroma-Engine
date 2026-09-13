//! Monotonic, allocation-free stage timings, including failed operations.
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::Instant;

#[derive(Debug, Default)]
pub(super) struct StageCounters {
    pub open: AtomicU64,
    pub read: AtomicU64,
    pub video: AtomicU64,
    pub audio: AtomicU64,
    pub mux: AtomicU64,
    pub publish: AtomicU64,
}

pub(super) struct StageTimer<'a> {
    start: Instant,
    counter: &'a AtomicU64,
}
impl<'a> StageTimer<'a> {
    pub fn new(counter: &'a AtomicU64) -> Self {
        Self {
            start: Instant::now(),
            counter,
        }
    }
}
impl Drop for StageTimer<'_> {
    fn drop(&mut self) {
        self.counter.fetch_add(
            self.start.elapsed().as_micros().min(u128::from(u64::MAX)) as u64,
            Ordering::Relaxed,
        );
    }
}
