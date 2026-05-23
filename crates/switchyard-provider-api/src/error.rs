use thiserror::Error;
use uuid::Uuid;

#[derive(Debug, Error)]
pub enum ProviderError {
    #[error("provider not installed: {0}")]
    NotInstalled(String),

    #[error("provider not authenticated: {0}")]
    NotAuthenticated(String),

    #[error("invalid output from provider: {0}")]
    InvalidOutput(String),

    #[error("provider timed out after {0} seconds")]
    Timeout(u64),

    #[error("provider execution failed: {0}")]
    ExecutionFailed(String),

    #[error("unsupported capability: {0}")]
    UnsupportedCapability(String),
}

/// Returned by [`LiveInstanceRegistry::register`] when an instance with the
/// same `(provider, session_id, label)` triple already exists. Label-less
/// instances never conflict.
#[derive(Debug, Error, Clone, PartialEq, Eq)]
#[error("label conflict in pool: provider={provider} session={session_id} label={label}")]
pub struct LabelConflict {
    pub provider: String,
    pub session_id: Uuid,
    pub label: String,
}
