//! Fake provider for testing. Produces deterministic event sequences.
//!
//! Three modes:
//! - `Success` — emits turn_started, item_updated (text), turn_completed
//! - `Failure` — emits turn_started, turn_failed
//! - `Timeout` — emits turn_started, then hangs until cancelled

use std::collections::HashMap;
use std::sync::Arc;
use std::time::Duration;

use async_trait::async_trait;
use tokio::sync::{Mutex, mpsc};
use uuid::Uuid;

use switchyard_provider_api::*;

/// Behavior mode for the fake provider.
#[derive(Debug, Clone)]
pub enum FakeMode {
    /// Emit a successful turn with the given response text.
    Success(String),
    /// Emit a failed turn with the given error message.
    Failure(String),
    /// Hang for the given duration (simulates timeout).
    Timeout(Duration),
}

/// A fully controllable fake provider for testing turn lifecycle.
#[derive(Debug, Clone)]
pub struct FakeProvider {
    pub provider_id: String,
    mode: Arc<Mutex<FakeMode>>,
    results: Arc<Mutex<HashMap<Uuid, (TurnResult, ArtifactBundle)>>>,
}

impl FakeProvider {
    pub fn new(provider_id: impl Into<String>, mode: FakeMode) -> Self {
        Self {
            provider_id: provider_id.into(),
            mode: Arc::new(Mutex::new(mode)),
            results: Arc::new(Mutex::new(HashMap::new())),
        }
    }

    pub fn success(response: impl Into<String>) -> Self {
        Self::new("fake", FakeMode::Success(response.into()))
    }

    pub fn failure(error: impl Into<String>) -> Self {
        Self::new("fake", FakeMode::Failure(error.into()))
    }

    pub fn timeout(duration: Duration) -> Self {
        Self::new("fake", FakeMode::Timeout(duration))
    }

    /// Change mode at runtime (useful for multi-turn test scenarios).
    pub async fn set_mode(&self, mode: FakeMode) {
        *self.mode.lock().await = mode;
    }
}

#[async_trait]
impl Provider for FakeProvider {
    async fn probe(&self) -> Result<ProbeResult, ProviderError> {
        let mut caps = std::collections::HashSet::new();
        caps.insert(ProviderCapability::HeadlessTurn);
        caps.insert(ProviderCapability::StreamingOutput);
        Ok(ProbeResult {
            version: Some("0.0.0-fake".to_string()),
            available: true,
            capabilities: caps,
            issues: vec![],
            ..Default::default()
        })
    }

    async fn start_turn(
        &self,
        turn_id: Uuid,
        input: TurnInput,
        _policy: ExecutionPolicy,
        _context: ContextBundle,
        event_tx: mpsc::Sender<ProviderEvent>,
        cancel: CancellationToken,
    ) -> Result<(), ProviderError> {
        let mode = self.mode.lock().await.clone();
        let provider = self.provider_id.clone();

        match mode {
            FakeMode::Success(response_text) => {
                event_tx
                    .send(ProviderEvent::turn_started(turn_id, &provider))
                    .await
                    .ok();

                event_tx
                    .send(ProviderEvent::text_message(
                        turn_id,
                        &provider,
                        &response_text,
                    ))
                    .await
                    .ok();

                event_tx
                    .send(ProviderEvent::turn_completed(turn_id, &provider))
                    .await
                    .ok();

                self.results.lock().await.insert(
                    turn_id,
                    (
                        TurnResult {
                            response_text: response_text.clone(),
                            exit_code: Some(0),
                            stderr: None,
                            metadata: HashMap::new(),
                        },
                        ArtifactBundle {
                            artifacts: vec![ArtifactEntry {
                                artifact_type: ARTIFACT_TYPE_RAW_OUTPUT.to_string(),
                                title: format!("fake response to: {}", input.user_message),
                                summary: Some(response_text),
                                path: None,
                                metadata: HashMap::new(),
                            }],
                        },
                    ),
                );
            }
            FakeMode::Failure(error_msg) => {
                event_tx
                    .send(ProviderEvent::turn_started(turn_id, &provider))
                    .await
                    .ok();

                event_tx
                    .send(ProviderEvent::turn_failed(turn_id, &provider, &error_msg))
                    .await
                    .ok();

                self.results.lock().await.insert(
                    turn_id,
                    (
                        TurnResult {
                            response_text: String::new(),
                            exit_code: Some(1),
                            stderr: Some(error_msg.clone()),
                            metadata: HashMap::new(),
                        },
                        ArtifactBundle { artifacts: vec![] },
                    ),
                );
            }
            FakeMode::Timeout(duration) => {
                event_tx
                    .send(ProviderEvent::turn_started(turn_id, &provider))
                    .await
                    .ok();

                tokio::select! {
                    _ = tokio::time::sleep(duration) => {}
                    _ = cancel.cancelled() => {}
                }

                let msg = if cancel.is_cancelled() {
                    "cancelled"
                } else {
                    "timed out"
                };
                event_tx
                    .send(ProviderEvent::turn_failed(turn_id, &provider, msg))
                    .await
                    .ok();

                self.results.lock().await.insert(
                    turn_id,
                    (
                        TurnResult {
                            response_text: String::new(),
                            exit_code: None,
                            stderr: Some("timed out".to_string()),
                            metadata: HashMap::new(),
                        },
                        ArtifactBundle { artifacts: vec![] },
                    ),
                );
            }
        }

        Ok(())
    }

    async fn finalize_turn(
        &self,
        turn_id: Uuid,
    ) -> Result<(TurnResult, ArtifactBundle), ProviderError> {
        self.results
            .lock()
            .await
            .remove(&turn_id)
            .ok_or_else(|| ProviderError::ExecutionFailed(format!("no result for turn {turn_id}")))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn success_emits_three_events() {
        let provider = FakeProvider::success("hello world");
        let turn_id = Uuid::now_v7();
        let (tx, mut rx) = mpsc::channel(16);

        provider
            .start_turn(
                turn_id,
                TurnInput {
                    user_message: "hi".to_string(),
                    system_prompt: None,
                },
                ExecutionPolicy {
                    timeout_secs: 30,
                    write_access: false,
                    cwd: std::path::PathBuf::from("."),
                    allowed_paths: vec![],
                },
                ContextBundle {
                    summary: None,
                    recent_turns: vec![],
                    peer_state: vec![],
                    artifacts: vec![],
                },
                tx,
                CancellationToken::new(),
            )
            .await
            .unwrap();

        let mut events = Vec::new();
        while let Ok(e) = rx.try_recv() {
            events.push(e);
        }
        assert_eq!(events.len(), 3);
        assert_eq!(events[0].event_type, EventType::TurnStarted);
        assert_eq!(events[1].event_type, EventType::ItemUpdated);
        assert_eq!(events[1].payload["text"], "hello world");
        assert_eq!(events[2].event_type, EventType::TurnCompleted);

        let (result, bundle) = provider.finalize_turn(turn_id).await.unwrap();
        assert_eq!(result.response_text, "hello world");
        assert_eq!(result.exit_code, Some(0));
        assert_eq!(bundle.artifacts.len(), 1);
    }

    #[tokio::test]
    async fn failure_emits_started_then_failed() {
        let provider = FakeProvider::failure("segment fault");
        let turn_id = Uuid::now_v7();
        let (tx, mut rx) = mpsc::channel(16);

        provider
            .start_turn(
                turn_id,
                TurnInput {
                    user_message: "crash".to_string(),
                    system_prompt: None,
                },
                ExecutionPolicy {
                    timeout_secs: 30,
                    write_access: false,
                    cwd: std::path::PathBuf::from("."),
                    allowed_paths: vec![],
                },
                ContextBundle {
                    summary: None,
                    recent_turns: vec![],
                    peer_state: vec![],
                    artifacts: vec![],
                },
                tx,
                CancellationToken::new(),
            )
            .await
            .unwrap();

        let mut events = Vec::new();
        while let Ok(e) = rx.try_recv() {
            events.push(e);
        }
        assert_eq!(events.len(), 2);
        assert_eq!(events[0].event_type, EventType::TurnStarted);
        assert_eq!(events[1].event_type, EventType::TurnFailed);

        let (result, _) = provider.finalize_turn(turn_id).await.unwrap();
        assert_eq!(result.exit_code, Some(1));
        assert_eq!(result.stderr.as_deref(), Some("segment fault"));
    }

    #[tokio::test]
    async fn timeout_hangs_then_fails() {
        let provider = FakeProvider::timeout(Duration::from_millis(50));
        let turn_id = Uuid::now_v7();
        let (tx, mut rx) = mpsc::channel(16);

        provider
            .start_turn(
                turn_id,
                TurnInput {
                    user_message: "slow".to_string(),
                    system_prompt: None,
                },
                ExecutionPolicy {
                    timeout_secs: 1,
                    write_access: false,
                    cwd: std::path::PathBuf::from("."),
                    allowed_paths: vec![],
                },
                ContextBundle {
                    summary: None,
                    recent_turns: vec![],
                    peer_state: vec![],
                    artifacts: vec![],
                },
                tx,
                CancellationToken::new(),
            )
            .await
            .unwrap();

        let mut events = Vec::new();
        while let Ok(e) = rx.try_recv() {
            events.push(e);
        }
        assert_eq!(events.len(), 2);
        assert_eq!(events[0].event_type, EventType::TurnStarted);
        assert_eq!(events[1].event_type, EventType::TurnFailed);
    }

    #[tokio::test]
    async fn probe_returns_available() {
        let provider = FakeProvider::success("x");
        let probe = provider.probe().await.unwrap();
        assert!(probe.available);
        assert!(
            probe
                .capabilities
                .contains(&ProviderCapability::HeadlessTurn)
        );
    }
}
