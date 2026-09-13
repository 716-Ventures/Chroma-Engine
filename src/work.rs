use std::{
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
    },
    time::Instant,
};

/// Cooperative cancellation and deadline for a sequence of engine operations.
/// Native driver calls are not interruptible; hosts must additionally supervise
/// their worker process when enforcing a hard wall-clock deadline.
#[derive(Debug, Clone, Default)]
pub struct WorkControl {
    cancelled: Arc<AtomicBool>,
    deadline: Option<Instant>,
}

/// A cooperative operation was cancelled or exceeded its deadline.
#[derive(Debug, thiserror::Error, PartialEq, Eq)]
pub enum WorkStopped {
    /// The owner cancelled the operation.
    #[error("engine work cancelled")]
    Cancelled,
    /// The configured deadline elapsed.
    #[error("engine work deadline exceeded")]
    Deadline,
}

impl WorkControl {
    /// Creates a control with an absolute monotonic deadline.
    pub fn with_deadline(deadline: Instant) -> Self {
        Self {
            deadline: Some(deadline),
            ..Self::default()
        }
    }

    /// Cancels all work sharing this control. Cancellation is permanent.
    pub fn cancel(&self) {
        self.cancelled.store(true, Ordering::Release);
    }

    /// Checks cancellation and deadline before the next bounded unit of work.
    pub fn check(&self) -> Result<(), WorkStopped> {
        if self.cancelled.load(Ordering::Acquire) {
            return Err(WorkStopped::Cancelled);
        }
        if self
            .deadline
            .is_some_and(|deadline| Instant::now() >= deadline)
        {
            return Err(WorkStopped::Deadline);
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn cancellation_is_shared_and_deadlines_are_monotonic() {
        let owner = WorkControl::default();
        let worker = owner.clone();
        assert!(worker.check().is_ok());
        owner.cancel();
        assert_eq!(worker.check(), Err(WorkStopped::Cancelled));
        assert_eq!(
            WorkControl::with_deadline(Instant::now()).check(),
            Err(WorkStopped::Deadline)
        );
    }
}
