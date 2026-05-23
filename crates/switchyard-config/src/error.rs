use thiserror::Error;

#[derive(Debug, Error)]
pub enum ConfigError {
    #[error("config I/O error: {0}")]
    Io(#[from] std::io::Error),

    #[error("config parse error: {0}")]
    Parse(#[from] toml::de::Error),

    #[error("config serialization error: {0}")]
    Serialize(#[from] toml::ser::Error),
}
