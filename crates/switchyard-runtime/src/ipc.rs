//! JSON-line protocol for the runtime IPC bus.
//!
//! SQLite remains the durable authority. IPC messages only advertise committed
//! event records so receivers can apply them idempotently and recover missed
//! messages by replaying `runtime_events` from the database.

use std::{io, time::Duration};

use serde::{Deserialize, Serialize};
use tokio::io::{AsyncWrite, AsyncWriteExt};
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

/// Best-effort publication timeout used by CLI/HYARD producers.
///
/// Publishing is deliberately outside the SQLite transaction: if this timeout
/// trips the durable row and event are still valid, and receivers recover via
/// snapshot/replay.
pub const DEFAULT_RUNTIME_IPC_PUBLISH_TIMEOUT: Duration = Duration::from_millis(500);

/// Publish committed runtime events to the local runtime IPC endpoint.
///
/// This function only advertises events that are already durable in SQLite. It
/// does not retry and it does not imply delivery; callers should treat failures
/// as a live-update miss and rely on the database snapshot path for recovery.
pub async fn publish_runtime_events(
    endpoint: &str,
    events: Vec<RuntimeEventRecord>,
) -> io::Result<()> {
    if events.is_empty() {
        return Ok(());
    }
    write_runtime_ipc_request(endpoint, RuntimeIpcRequest::Publish { events }).await
}

/// Publish committed runtime events with a short timeout.
///
/// This is the preferred helper for producer hot paths. It bounds the IPC path
/// so a missing or slow GUI/runtime owner cannot delay HYARD compact-JSON
/// responses or provider turn completion.
pub async fn publish_runtime_events_with_timeout(
    endpoint: &str,
    events: Vec<RuntimeEventRecord>,
    timeout: Duration,
) -> io::Result<()> {
    match tokio::time::timeout(timeout, publish_runtime_events(endpoint, events)).await {
        Ok(result) => result,
        Err(_) => Err(io::Error::new(
            io::ErrorKind::TimedOut,
            "runtime IPC publish timed out",
        )),
    }
}

async fn write_runtime_ipc_request(endpoint: &str, request: RuntimeIpcRequest) -> io::Result<()> {
    #[cfg(windows)]
    {
        let pipe = tokio::net::windows::named_pipe::ClientOptions::new().open(endpoint)?;
        write_json_line(pipe, &request).await
    }

    #[cfg(unix)]
    {
        let stream = tokio::net::UnixStream::connect(endpoint).await?;
        write_json_line(stream, &request).await
    }

    #[cfg(not(any(windows, unix)))]
    {
        let _ = (endpoint, request);
        Err(io::Error::new(
            io::ErrorKind::Unsupported,
            "runtime IPC is only implemented for windows/unix targets",
        ))
    }
}

async fn write_json_line<W>(mut writer: W, request: &RuntimeIpcRequest) -> io::Result<()>
where
    W: AsyncWrite + Unpin,
{
    let mut line = serde_json::to_vec(request).map_err(|err| {
        io::Error::new(
            io::ErrorKind::InvalidData,
            format!("failed to encode runtime IPC request: {err}"),
        )
    })?;
    line.push(b'\n');
    writer.write_all(&line).await?;
    writer.flush().await?;
    writer.shutdown().await
}

#[cfg(test)]
mod tests {
    use chrono::Utc;
    use serde_json::json;

    use super::*;

    fn event(event_id: i64) -> RuntimeEventRecord {
        RuntimeEventRecord {
            event_id,
            workspace_id: Some("workspace".to_string()),
            session_id: Some(Uuid::now_v7()),
            aggregate_type: "host_job".to_string(),
            aggregate_id: Uuid::now_v7().to_string(),
            aggregate_version: event_id,
            event_type: "host_job.running".to_string(),
            payload: json!({ "job_id": Uuid::now_v7(), "status": "running" }),
            occurred_at: Utc::now(),
            source: "test".to_string(),
        }
    }

    #[test]
    fn ipc_publish_request_round_trips_json_line() {
        let request = RuntimeIpcRequest::Publish {
            events: vec![event(7)],
        };
        let encoded = serde_json::to_string(&request).expect("encode request");

        assert!(encoded.contains(r#""type":"publish""#));
        let decoded: RuntimeIpcRequest = serde_json::from_str(&encoded).expect("decode request");
        match decoded {
            RuntimeIpcRequest::Publish { events } => {
                assert_eq!(events.len(), 1);
                assert_eq!(events[0].event_id, 7);
            }
            other => panic!("unexpected request: {other:?}"),
        }
    }

    #[test]
    fn ipc_events_message_tracks_max_event_id() {
        let message = RuntimeIpcMessage::events(vec![event(3), event(11), event(5)]);
        match message {
            RuntimeIpcMessage::Events {
                events,
                max_event_id,
            } => {
                assert_eq!(events.len(), 3);
                assert_eq!(max_event_id, 11);
            }
            other => panic!("unexpected message: {other:?}"),
        }
    }
}
