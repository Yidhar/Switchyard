use thiserror::Error;

#[derive(Debug, Error)]
pub enum RuntimeError {
    #[error("io error: {0}")]
    Io(#[from] std::io::Error),
    #[error("sqlite error: {0}")]
    Sqlite(#[from] rusqlite::Error),
    #[error("json error: {0}")]
    Json(#[from] serde_json::Error),
    #[error("invalid uuid '{value}' in column {column}")]
    InvalidUuid { column: &'static str, value: String },
    #[error("invalid timestamp '{value}' in column {column}")]
    InvalidTimestamp { column: &'static str, value: String },
    #[error("invalid host job status '{0}'")]
    InvalidHostJobStatus(String),
    #[error("host job '{0}' not found")]
    HostJobNotFound(uuid::Uuid),
    #[error("invalid host job transition from {from} to {to}")]
    InvalidHostJobTransition { from: String, to: String },
}
