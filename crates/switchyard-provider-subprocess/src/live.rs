use std::sync::Arc;
use async_trait::async_trait;
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::process::{Child, ChildStdin};
use tokio::sync::{Mutex, mpsc};
use uuid::Uuid;

use switchyard_provider_api::{
    ContextBundle, EventType, LiveInstance, ProviderError, ProviderEvent,
};

pub struct SubprocessLiveInstance {
    pub provider: String,
    pub child: Child,
    pub stdin: ChildStdin,
    pub stdout_rx: Arc<Mutex<mpsc::Receiver<String>>>,
}

impl SubprocessLiveInstance {
    pub fn new(provider: &str, mut child: Child) -> Result<Self, ProviderError> {
        let stdin = child.stdin.take().ok_or_else(|| {
            ProviderError::ExecutionFailed("Failed to open stdin for live instance".into())
        })?;
        let stdout = child.stdout.take().ok_or_else(|| {
            ProviderError::ExecutionFailed("Failed to open stdout for live instance".into())
        })?;
        let stderr = child.stderr.take();

        if let Some(se) = stderr {
            tokio::spawn(async move {
                let mut reader = BufReader::new(se).lines();
                while let Ok(Some(_line)) = reader.next_line().await {}
            });
        }

        let (stdout_tx, stdout_rx) = mpsc::channel(1024);
        tokio::spawn(async move {
            let mut reader = BufReader::new(stdout).lines();
            while let Ok(Some(line)) = reader.next_line().await {
                if stdout_tx.send(line).await.is_err() {
                    break;
                }
            }
        });

        Ok(Self {
            provider: provider.to_string(),
            child,
            stdin,
            stdout_rx: Arc::new(Mutex::new(stdout_rx)),
        })
    }
}

#[async_trait]
impl LiveInstance for SubprocessLiveInstance {
    async fn send_message(
        &mut self,
        text: &str,
    ) -> Result<mpsc::Receiver<ProviderEvent>, ProviderError> {
        self.stdin
            .write_all(text.as_bytes())
            .await
            .map_err(|e| ProviderError::ExecutionFailed(e.to_string()))?;
        self.stdin
            .write_all(b"\n")
            .await
            .map_err(|e| ProviderError::ExecutionFailed(e.to_string()))?;
        self.stdin
            .flush()
            .await
            .map_err(|e| ProviderError::ExecutionFailed(e.to_string()))?;

        let (event_tx, event_rx) = mpsc::channel(256);
        let provider_name = self.provider.clone();
        let rx_lock = Arc::clone(&self.stdout_rx);
        let turn_id = Uuid::now_v7();

        tokio::spawn(async move {
            let mut rx = rx_lock.lock().await;
            while let Some(line) = rx.recv().await {
                if line.contains("__HYARD_TURN_FINISHED__") {
                    break;
                }

                if let Ok(json) = serde_json::from_str::<serde_json::Value>(&line) {
                    if let Some(event_type) = json.get("event_type").and_then(|t| t.as_str()) {
                        if event_type == "turn.completed" || event_type == "turn.failed" {
                            break;
                        }
                    }
                    let pe = ProviderEvent::new(turn_id, EventType::ItemUpdated, &provider_name, json);
                    if event_tx.send(pe).await.is_err() {
                        break;
                    }
                } else {
                    let pe = ProviderEvent::text_message(turn_id, &provider_name, &line);
                    if event_tx.send(pe).await.is_err() {
                        break;
                    }
                }
            }
        });

        Ok(event_rx)
    }

    async fn update_context(&mut self, context: ContextBundle) -> Result<(), ProviderError> {
        let serialized = serde_json::to_string(&context)
            .map_err(|e| ProviderError::ExecutionFailed(format!("Failed to serialize context: {e}")))?;
        // Send as context update signal
        self.stdin
            .write_all(format!("__HYARD_CONTEXT_UPDATE__ {}\n", serialized).as_bytes())
            .await
            .map_err(|e| ProviderError::ExecutionFailed(e.to_string()))?;
        self.stdin
            .flush()
            .await
            .map_err(|e| ProviderError::ExecutionFailed(e.to_string()))?;
        Ok(())
    }

    async fn terminate(&mut self) -> Result<(), ProviderError> {
        self.child
            .kill()
            .await
            .map_err(|e| ProviderError::ExecutionFailed(e.to_string()))?;
        Ok(())
    }
}
