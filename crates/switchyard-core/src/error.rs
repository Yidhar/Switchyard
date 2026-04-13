use thiserror::Error;

#[derive(Debug, Error)]
pub enum CoreError {
    #[error("provider error: {0}")]
    Provider(#[from] switchyard_provider_api::ProviderError),

    #[error("store error: {0}")]
    Store(#[from] switchyard_store::StoreError),

    #[error("turn runner error: {0}")]
    Runner(String),
}
