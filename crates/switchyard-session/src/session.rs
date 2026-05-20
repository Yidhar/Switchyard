use std::collections::HashMap;

use chrono::{DateTime, Duration, Utc};
use serde::{Deserialize, Serialize};
use uuid::Uuid;

pub const ACTIVE_TURN_LEASE_SECS: i64 = 15;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Session {
    pub session_id: Uuid,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
    pub active_core: String,
    pub enabled_peers: Vec<String>,
    pub mode: SessionMode,
    pub summary: Option<String>,
    #[serde(default)]
    pub name: Option<String>,
    pub native_bindings: HashMap<String, String>,
    #[serde(default)]
    pub active_turn_id: Option<Uuid>,
    #[serde(default)]
    pub active_turn_provider: Option<String>,
    #[serde(default)]
    pub active_turn_lease_expires_at: Option<DateTime<Utc>>,
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
            name: None,
            native_bindings: HashMap::new(),
            active_turn_id: None,
            active_turn_provider: None,
            active_turn_lease_expires_at: None,
        }
    }

    pub fn mark_turn_active(&mut self, turn_id: Uuid, provider: impl Into<String>) {
        self.active_turn_id = Some(turn_id);
        self.active_turn_provider = Some(provider.into());
        self.bump_active_turn_lease();
    }

    pub fn bump_active_turn_lease(&mut self) {
        let now = Utc::now();
        self.updated_at = now;
        self.active_turn_lease_expires_at = Some(now + Duration::seconds(ACTIVE_TURN_LEASE_SECS));
    }

    pub fn clear_active_turn(&mut self) {
        self.active_turn_id = None;
        self.active_turn_provider = None;
        self.active_turn_lease_expires_at = None;
        self.updated_at = Utc::now();
    }

    pub fn active_turn_is_live(&self) -> bool {
        self.active_turn_id.is_some()
            && self
                .active_turn_lease_expires_at
                .map(|expires_at| expires_at > Utc::now())
                .unwrap_or(false)
    }
}
