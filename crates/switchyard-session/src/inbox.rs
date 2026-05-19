use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use std::fmt;
use uuid::Uuid;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct InboxEntry {
    pub entry_id: Uuid,
    pub session_id: Uuid,
    pub kind: InboxItemKind,
    pub status: InboxStatus,
    pub provider: Option<String>,
    pub job_id: Option<Uuid>,
    pub turn_id: Option<Uuid>,
    pub title: String,
    pub message: String,
    pub summary: Option<String>,
    pub payload: serde_json::Value,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
    pub read_at: Option<DateTime<Utc>>,
    pub consumed_at: Option<DateTime<Utc>>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
#[non_exhaustive]
pub enum InboxItemKind {
    BackgroundJobReceipt,
}

impl fmt::Display for InboxItemKind {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::BackgroundJobReceipt => write!(f, "background_job_receipt"),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
#[non_exhaustive]
pub enum InboxStatus {
    Unread,
    Read,
    Consumed,
}

impl fmt::Display for InboxStatus {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Unread => write!(f, "unread"),
            Self::Read => write!(f, "read"),
            Self::Consumed => write!(f, "consumed"),
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
#[non_exhaustive]
pub enum InboxDeliveryMode {
    Immediate,
    Checkpoint,
    Quiet,
}

impl InboxDeliveryMode {
    pub fn parse(value: &str) -> Option<Self> {
        match value.trim().to_ascii_lowercase().as_str() {
            "immediate" => Some(Self::Immediate),
            "checkpoint" => Some(Self::Checkpoint),
            "quiet" => Some(Self::Quiet),
            _ => None,
        }
    }
}

impl fmt::Display for InboxDeliveryMode {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Immediate => write!(f, "immediate"),
            Self::Checkpoint => write!(f, "checkpoint"),
            Self::Quiet => write!(f, "quiet"),
        }
    }
}

impl InboxEntry {
    pub fn new(
        session_id: Uuid,
        kind: InboxItemKind,
        title: impl Into<String>,
        message: impl Into<String>,
    ) -> Self {
        let now = Utc::now();
        Self {
            entry_id: Uuid::now_v7(),
            session_id,
            kind,
            status: InboxStatus::Unread,
            provider: None,
            job_id: None,
            turn_id: None,
            title: title.into(),
            message: message.into(),
            summary: None,
            payload: serde_json::json!({}),
            created_at: now,
            updated_at: now,
            read_at: None,
            consumed_at: None,
        }
    }

    pub fn background_job_receipt(
        session_id: Uuid,
        provider: impl Into<String>,
        title: impl Into<String>,
        message: impl Into<String>,
    ) -> Self {
        let provider = provider.into();
        let mut entry = Self::new(
            session_id,
            InboxItemKind::BackgroundJobReceipt,
            title,
            message,
        );
        entry.provider = Some(provider);
        entry
    }

    pub fn mark_read(&mut self) {
        if matches!(self.status, InboxStatus::Consumed) {
            return;
        }
        let now = Utc::now();
        self.status = InboxStatus::Read;
        self.updated_at = now;
        self.read_at.get_or_insert(now);
    }

    pub fn mark_consumed(&mut self) {
        let now = Utc::now();
        self.status = InboxStatus::Consumed;
        self.updated_at = now;
        self.read_at.get_or_insert(now);
        self.consumed_at = Some(now);
    }

    pub fn is_unread(&self) -> bool {
        matches!(self.status, InboxStatus::Unread)
    }

    pub fn delivery_mode(&self) -> InboxDeliveryMode {
        if let Some(mode) = self
            .payload
            .get("callback_delivery")
            .and_then(|value| value.as_str())
            .and_then(InboxDeliveryMode::parse)
        {
            return mode;
        }

        match self.kind {
            InboxItemKind::BackgroundJobReceipt => match self
                .payload
                .get("job_status")
                .and_then(|value| value.as_str())
            {
                Some("failed") | Some("cancelled") => InboxDeliveryMode::Immediate,
                Some("completed") => InboxDeliveryMode::Checkpoint,
                Some("queued") | Some("running") | Some("cancel_requested") => {
                    InboxDeliveryMode::Quiet
                }
                _ => InboxDeliveryMode::Checkpoint,
            },
        }
    }
}
