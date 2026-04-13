use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use std::fmt;
use uuid::Uuid;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Turn {
    pub turn_id: Uuid,
    pub session_id: Uuid,
    pub origin: TurnOrigin,
    pub provider: String,
    pub role: TurnRole,
    pub user_message: String,
    pub provider_response: Option<String>,
    pub error_message: Option<String>,
    pub status: TurnStatus,
    pub started_at: DateTime<Utc>,
    pub completed_at: Option<DateTime<Utc>>,
    /// For delegate turns: who initiated this delegation.
    pub delegated_by: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
#[non_exhaustive]
pub enum TurnOrigin {
    User,
    Delegate,
    System,
}

impl fmt::Display for TurnOrigin {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::User => write!(f, "user"),
            Self::Delegate => write!(f, "delegate"),
            Self::System => write!(f, "system"),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
#[non_exhaustive]
pub enum TurnStatus {
    Pending,
    Running,
    Completed,
    Failed,
    Cancelled,
}

impl fmt::Display for TurnStatus {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Pending => write!(f, "pending"),
            Self::Running => write!(f, "running"),
            Self::Completed => write!(f, "completed"),
            Self::Failed => write!(f, "failed"),
            Self::Cancelled => write!(f, "cancelled"),
        }
    }
}

/// Role the provider plays in this turn.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
#[non_exhaustive]
pub enum TurnRole {
    Core,
    Worker,
    Reviewer,
    Analyst,
}

impl fmt::Display for TurnRole {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Core => write!(f, "core"),
            Self::Worker => write!(f, "worker"),
            Self::Reviewer => write!(f, "reviewer"),
            Self::Analyst => write!(f, "analyst"),
        }
    }
}

impl Turn {
    /// Create a user-initiated turn (origin = User).
    pub fn new(
        session_id: Uuid,
        provider: impl Into<String>,
        role: TurnRole,
        user_message: impl Into<String>,
    ) -> Self {
        Self {
            turn_id: Uuid::now_v7(),
            session_id,
            origin: TurnOrigin::User,
            provider: provider.into(),
            role,
            user_message: user_message.into(),
            provider_response: None,
            error_message: None,
            status: TurnStatus::Pending,
            started_at: Utc::now(),
            completed_at: None,
            delegated_by: None,
        }
    }

    /// Create a delegate turn (origin = Delegate).
    /// `delegated_by` records which provider initiated the delegation.
    pub fn new_delegate(
        session_id: Uuid,
        provider: impl Into<String>,
        role: TurnRole,
        task: impl Into<String>,
        delegated_by: impl Into<String>,
    ) -> Self {
        Self {
            turn_id: Uuid::now_v7(),
            session_id,
            origin: TurnOrigin::Delegate,
            provider: provider.into(),
            role,
            user_message: task.into(),
            provider_response: None,
            error_message: None,
            status: TurnStatus::Pending,
            started_at: Utc::now(),
            completed_at: None,
            delegated_by: Some(delegated_by.into()),
        }
    }

    /// Create a system turn (origin = System).
    pub fn new_system(
        session_id: Uuid,
        provider: impl Into<String>,
        message: impl Into<String>,
    ) -> Self {
        Self {
            turn_id: Uuid::now_v7(),
            session_id,
            origin: TurnOrigin::System,
            provider: provider.into(),
            role: TurnRole::Core,
            user_message: message.into(),
            provider_response: None,
            error_message: None,
            status: TurnStatus::Pending,
            started_at: Utc::now(),
            completed_at: None,
            delegated_by: None,
        }
    }
}
