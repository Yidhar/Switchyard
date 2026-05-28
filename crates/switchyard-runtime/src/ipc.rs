//! JSON-line protocol for the runtime IPC bus.
//!
//! SQLite remains the durable authority. IPC messages only advertise committed
//! event records so receivers can apply them idempotently and recover missed
//! messages by replaying `runtime_events` from the database.

use serde::{Deserialize, Serialize};
use uuid::Uuid;

use crate::protocol::{RuntimeEventRecord, RuntimeSnapshot};

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum RuntimeIpcRequest {
    /// Publish already-committed runtime events to local subscribers.
    ///
    /// Producers must commit the SQLite transaction first, then send this
    /// notification. Receivers treat delivery as at-least-once and de-dupe by
    /// `event_id`.
    Publish { events: Vec<RuntimeEventRecord> },
    /// Ask the runtime owner for a replayable snapshot. This is used on
    /// reconnect and as a low-frequency reconcile fallback.
    Snapshot {
        session_id: Uuid,
        after_event_id: i64,
        event_limit: usize,
        job_limit: usize,
    },
    /// Subscribe to future committed events. The server should send a snapshot
    /// first, then stream `events` messages.
    Subscribe {
        session_id: Option<Uuid>,
        after_event_id: i64,
    },
    /// Lightweight liveness check.
    Heartbeat,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum RuntimeIpcMessage {
    Ack {
        accepted: usize,
    },
    Snapshot(RuntimeSnapshot),
    Events {
        events: Vec<RuntimeEventRecord>,
        max_event_id: i64,
    },
    Heartbeat {
        max_event_id: i64,
    },
    Error {
        message: String,
    },
}

impl RuntimeIpcMessage {
    pub fn events(events: Vec<RuntimeEventRecord>) -> Self {
        let max_event_id = events.iter().map(|event| event.event_id).max().unwrap_or(0);
        Self::Events {
            events,
            max_event_id,
        }
    }
}
