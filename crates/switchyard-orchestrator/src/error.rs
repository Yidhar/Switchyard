use switchyard_store::StoreError;

#[derive(Debug, thiserror::Error)]
pub enum OrchestratorError {
    #[error("invalid delegate request: {0}")]
    InvalidRequest(String),

    #[error("peer '{0}' is not available")]
    PeerUnavailable(String),

    #[error("peer execution failed: {0}")]
    PeerExecutionFailed(String),

    #[error("store error: {0}")]
    Store(#[from] StoreError),

    #[error("provider error: {0}")]
    Provider(#[from] switchyard_provider_api::ProviderError),
}
