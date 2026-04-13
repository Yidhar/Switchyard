use std::collections::HashMap;
use std::fmt;
use std::path::PathBuf;

use serde::{Deserialize, Serialize};
use uuid::Uuid;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Artifact {
    pub artifact_id: Uuid,
    pub turn_id: Uuid,
    pub artifact_type: ArtifactType,
    pub title: String,
    pub summary: Option<String>,
    pub path: Option<PathBuf>,
    pub metadata: HashMap<String, serde_json::Value>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
#[non_exhaustive]
pub enum ArtifactType {
    FileChange,
    CommandOutput,
    ReviewConclusion,
    DelegateResult,
    RawProviderOutput,
}

impl fmt::Display for ArtifactType {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::FileChange => write!(f, "file_change"),
            Self::CommandOutput => write!(f, "command_output"),
            Self::ReviewConclusion => write!(f, "review_conclusion"),
            Self::DelegateResult => write!(f, "delegate_result"),
            Self::RawProviderOutput => write!(f, "raw_provider_output"),
        }
    }
}

impl Artifact {
    pub fn new(turn_id: Uuid, artifact_type: ArtifactType, title: impl Into<String>) -> Self {
        Self {
            artifact_id: Uuid::now_v7(),
            turn_id,
            artifact_type,
            title: title.into(),
            summary: None,
            path: None,
            metadata: HashMap::new(),
        }
    }
}
