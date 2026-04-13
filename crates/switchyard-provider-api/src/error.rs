use thiserror::Error;

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
