use serde::{Deserialize, Serialize};
use std::fmt;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
#[non_exhaustive]
pub enum ProviderRole {
    Core,
    Worker,
    Reviewer,
    Analyst,
}

impl fmt::Display for ProviderRole {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Core => write!(f, "core"),
            Self::Worker => write!(f, "worker"),
            Self::Reviewer => write!(f, "reviewer"),
            Self::Analyst => write!(f, "analyst"),
        }
    }
}
