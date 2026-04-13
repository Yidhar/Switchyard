use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use std::fmt;
use uuid::Uuid;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Event {
    pub event_id: Uuid,
    pub turn_id: Uuid,
    pub event_type: EventType,
    pub provider: String,
    pub timestamp: DateTime<Utc>,
    pub payload: serde_json::Value,
}

/// Canonical event types. Identical to switchyard-provider-api::EventType
/// so that serialized forms are wire-compatible.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
#[non_exhaustive]
pub enum EventType {
    ThreadStarted,
    TurnStarted,
    ItemStarted,
    ItemUpdated,
    ItemCompleted,
    ArtifactReady,
    DelegateRequested,
    DelegateCompleted,
    TurnCompleted,
    TurnFailed,
}

impl fmt::Display for EventType {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::ThreadStarted => write!(f, "thread_started"),
            Self::TurnStarted => write!(f, "turn_started"),
            Self::ItemStarted => write!(f, "item_started"),
            Self::ItemUpdated => write!(f, "item_updated"),
            Self::ItemCompleted => write!(f, "item_completed"),
            Self::ArtifactReady => write!(f, "artifact_ready"),
            Self::DelegateRequested => write!(f, "delegate_requested"),
            Self::DelegateCompleted => write!(f, "delegate_completed"),
            Self::TurnCompleted => write!(f, "turn_completed"),
            Self::TurnFailed => write!(f, "turn_failed"),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
#[non_exhaustive]
pub enum ItemType {
    AgentMessage,
    Reasoning,
    CommandExecution,
    FileChange,
    DiffReady,
    ToolCall,
    TodoList,
    DelegateRequest,
    DelegateResult,
    Error,
}

impl fmt::Display for ItemType {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::AgentMessage => write!(f, "agent_message"),
            Self::Reasoning => write!(f, "reasoning"),
            Self::CommandExecution => write!(f, "command_execution"),
            Self::FileChange => write!(f, "file_change"),
            Self::DiffReady => write!(f, "diff_ready"),
            Self::ToolCall => write!(f, "tool_call"),
            Self::TodoList => write!(f, "todo_list"),
            Self::DelegateRequest => write!(f, "delegate_request"),
            Self::DelegateResult => write!(f, "delegate_result"),
            Self::Error => write!(f, "error"),
        }
    }
}

impl Event {
    pub fn new(
        turn_id: Uuid,
        event_type: EventType,
        provider: impl Into<String>,
        payload: serde_json::Value,
    ) -> Self {
        Self {
            event_id: Uuid::now_v7(),
            turn_id,
            event_type,
            provider: provider.into(),
            timestamp: Utc::now(),
            payload,
        }
    }
}
