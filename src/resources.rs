//! Shared admission and explicit per-session allocation ceilings.
use serde::{Deserialize, Serialize};
#[cfg(not(test))]
use std::sync::OnceLock;
use std::sync::{Arc, Mutex};

/// Limits for one session and the engine runtime sharing its admission pool.
/// Reservations bound admitted work, not allocations inside an opaque driver;
/// hosts must enforce a worker process/container ceiling for those allocations.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default, rename_all = "camelCase")]
pub struct ResourcePolicy {
    /// Memory reserved for each admitted session.
    pub session_memory_bytes: usize,
    /// Sum of simultaneous session reservations allowed by this runtime.
    pub aggregate_memory_bytes: usize,
    /// Maximum simultaneous source sessions, including copies.
    pub max_sessions: usize,
    /// Maximum owned container metadata bytes.
    pub metadata_bytes: usize,
    /// Maximum aggregate expanded packet index bytes.
    pub index_bytes: usize,
    /// Maximum compressed payload bytes in one read operation.
    pub compressed_window_bytes: usize,
    /// Maximum decoded/scaled frame bytes submitted together.
    pub decoded_batch_bytes: usize,
    /// Maximum complete output fragment bytes.
    pub output_bytes: usize,
    /// Maximum source frame pixel count.
    pub frame_pixels: u64,
    /// Maximum threads requested from configurable software codecs.
    pub codec_threads: u32,
    /// Whether software video fallback may be selected.
    pub allow_software_video: bool,
}

impl Default for ResourcePolicy {
    fn default() -> Self {
        Self {
            session_memory_bytes: 1536 * 1024 * 1024,
            aggregate_memory_bytes: 3 * 1024 * 1024 * 1024,
            max_sessions: 2,
            metadata_bytes: 64 * 1024 * 1024,
            index_bytes: 256 * 1024 * 1024,
            compressed_window_bytes: 64 * 1024 * 1024,
            decoded_batch_bytes: 64 * 1024 * 1024,
            output_bytes: 128 * 1024 * 1024,
            frame_pixels: 8192 * 4320,
            codec_threads: 2,
            allow_software_video: true,
        }
    }
}

/// Structured policy or admission failure, suitable for host retry decisions.
#[derive(Debug, Clone, thiserror::Error, PartialEq, Eq)]
pub enum ResourceError {
    /// The configured ceilings are inconsistent or zero.
    #[error("invalid engine resource policy: {0}")]
    InvalidPolicy(&'static str),
    /// An operation cannot fit its explicitly configured ceiling.
    #[error("{resource} requires {requested} but the configured limit is {limit}")]
    Exceeded {
        /// Allocation or workload category.
        resource: &'static str,
        /// Requested amount.
        requested: u64,
        /// Configured limit.
        limit: u64,
    },
    /// Other sessions own the available capacity; retry after they close.
    #[error("engine admission capacity is in use")]
    Busy,
}

impl ResourcePolicy {
    /// Low-memory, copy-first policy. Software video fallback is disabled.
    pub fn small_nas() -> Self {
        Self {
            session_memory_bytes: 224 * 1024 * 1024,
            aggregate_memory_bytes: 384 * 1024 * 1024,
            metadata_bytes: 24 * 1024 * 1024,
            index_bytes: 96 * 1024 * 1024,
            compressed_window_bytes: 32 * 1024 * 1024,
            decoded_batch_bytes: 8 * 1024 * 1024,
            output_bytes: 64 * 1024 * 1024,
            codec_threads: 1,
            allow_software_video: false,
            ..Self::default()
        }
    }
    /// Validates allocation ceilings before creating any media worker state.
    pub fn validate(&self) -> Result<(), ResourceError> {
        if self.max_sessions == 0
            || self.codec_threads == 0
            || self.codec_threads > 64
            || self.frame_pixels == 0
        {
            return Err(ResourceError::InvalidPolicy(
                "zero capacity or invalid thread/pixel count",
            ));
        }
        let mut total = 0usize;
        for value in [
            self.metadata_bytes,
            self.index_bytes,
            self.compressed_window_bytes,
            self.decoded_batch_bytes,
            self.output_bytes,
        ] {
            if value == 0 {
                return Err(ResourceError::InvalidPolicy(
                    "allocation ceilings must be positive",
                ));
            }
            total = total
                .checked_add(value)
                .ok_or(ResourceError::InvalidPolicy("memory accounting overflow"))?;
        }
        if total > self.session_memory_bytes
            || self.session_memory_bytes > self.aggregate_memory_bytes
        {
            return Err(ResourceError::InvalidPolicy(
                "allocation/session/aggregate ceilings are inconsistent",
            ));
        }
        Ok(())
    }
    pub(crate) fn check(
        &self,
        resource: &'static str,
        requested: usize,
        limit: usize,
    ) -> Result<(), ResourceError> {
        if requested > limit {
            return Err(ResourceError::Exceeded {
                resource,
                requested: requested as u64,
                limit: limit as u64,
            });
        }
        Ok(())
    }
    pub(crate) fn parse_limits(&self) -> crate::container::ParseLimits {
        crate::container::ParseLimits {
            max_index_bytes: self.index_bytes,
            ..Default::default()
        }
    }
}

/// Shared process-local admission pool. Every source session holds a lease.
#[derive(Debug)]
pub struct EngineRuntime {
    policy: ResourcePolicy,
    sessions: Mutex<usize>,
}

impl EngineRuntime {
    /// Creates an independent pool; share its Arc across all host engine handles.
    pub fn new(policy: ResourcePolicy) -> Result<Arc<Self>, ResourceError> {
        policy.validate()?;
        Ok(Arc::new(Self {
            policy,
            sessions: Mutex::new(0),
        }))
    }
    /// Returns the validated policy used by this pool.
    pub fn policy(&self) -> &ResourcePolicy {
        &self.policy
    }
    /// Returns currently admitted session count.
    pub fn active_sessions(&self) -> usize {
        *self
            .sessions
            .lock()
            .unwrap_or_else(|error| error.into_inner())
    }
    pub(crate) fn global() -> anyhow::Result<Arc<Self>> {
        #[cfg(test)]
        {
            Ok(Self::new(ResourcePolicy::default())?)
        }
        #[cfg(not(test))]
        {
            static GLOBAL: OnceLock<Result<Arc<EngineRuntime>, String>> = OnceLock::new();
            GLOBAL
                .get_or_init(|| {
                    let policy = match std::env::var("CHROMA_RESOURCE_POLICY") {
                        Ok(value) if value == "small-nas" => ResourcePolicy::small_nas(),
                        Ok(value) => serde_json::from_str(&value)
                            .map_err(|error| format!("CHROMA_RESOURCE_POLICY: {error}"))?,
                        Err(std::env::VarError::NotPresent) => ResourcePolicy::default(),
                        Err(error) => return Err(error.to_string()),
                    };
                    Self::new(policy).map_err(|error| error.to_string())
                })
                .clone()
                .map_err(anyhow::Error::msg)
        }
    }
    pub(crate) fn admit(self: &Arc<Self>) -> Result<SessionLease, ResourceError> {
        let mut sessions = self
            .sessions
            .lock()
            .unwrap_or_else(|error| error.into_inner());
        let limit = self
            .policy
            .max_sessions
            .min(self.policy.aggregate_memory_bytes / self.policy.session_memory_bytes);
        if *sessions >= limit {
            return Err(ResourceError::Busy);
        }
        *sessions += 1;
        Ok(SessionLease(self.clone()))
    }
}

#[derive(Debug)]
pub(crate) struct SessionLease(Arc<EngineRuntime>);
impl Drop for SessionLease {
    fn drop(&mut self) {
        let mut count = self
            .0
            .sessions
            .lock()
            .unwrap_or_else(|error| error.into_inner());
        *count = count.saturating_sub(1);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn shared_admission_releases_on_drop_and_rejects_invalid_limits() {
        let runtime = EngineRuntime::new(ResourcePolicy::small_nas()).unwrap();
        let first = runtime.admit().unwrap();
        assert!(matches!(runtime.admit(), Err(ResourceError::Busy)));
        drop(first);
        assert_eq!(runtime.active_sessions(), 0);
        let replacement = runtime.admit().unwrap();
        drop(replacement);
        assert_eq!(runtime.active_sessions(), 0);
        assert!(
            EngineRuntime::new(ResourcePolicy {
                codec_threads: 0,
                ..Default::default()
            })
            .is_err()
        );
    }
}
