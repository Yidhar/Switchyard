use std::collections::HashMap;

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use uuid::Uuid;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Session {
    pub session_id: Uuid,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
    pub active_core: String,
    pub enabled_peers: Vec<String>,
    pub mode: SessionMode,
    pub summary: Option<String>,
    pub native_bindings: HashMap<String, String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
#[non_exhaustive]
pub enum SessionMode {
    Interactive,
    Headless,
}

impl Session {
    pub fn new(active_core: String) -> Self {
        let now = Utc::now();
        Self {
            session_id: Uuid::now_v7(),
            created_at: now,
            updated_at: now,
            active_core,
            enabled_peers: Vec::new(),
            mode: SessionMode::Interactive,
            summary: None,
            native_bindings: HashMap::new(),
        }
    }
}
