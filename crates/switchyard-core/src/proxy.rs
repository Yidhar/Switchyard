use async_trait::async_trait;
use std::collections::HashMap;
use std::sync::Arc;
use tokio::sync::{Mutex, mpsc};
use uuid::Uuid;
use tokio_util::sync::CancellationToken;

use switchyard_provider_api::{
    ArtifactBundle, ContextBundle, ExecutionPolicy, LiveInstanceRegistry, ProbeResult,
    Provider, ProviderError, ProviderEvent, TurnInput, TurnResult, PersistentProvider,
};

pub struct PersistentProviderProxy {
    provider_name: String,
    inner: Box<dyn Provider>,
    registry: Option<Arc<dyn LiveInstanceRegistry>>,
    results: Arc<Mutex<HashMap<Uuid, (TurnResult, ArtifactBundle)>>>,
}

impl PersistentProviderProxy {
    pub fn new(
        provider_name: impl Into<String>,
        inner: Box<dyn Provider>,
        registry: Option<Arc<dyn LiveInstanceRegistry>>,
    ) -> Self {
        Self {
            provider_name: provider_name.into(),
            inner,
            registry,
            results: Arc::new(Mutex::new(HashMap::new())),
        }
    }
}

#[async_trait]
impl Provider for PersistentProviderProxy {
    async fn probe(&self) -> Result<ProbeResult, ProviderError> {
        self.inner.probe().await
    }

    async fn start_turn(
        &self,
        turn_id: Uuid,
        input: TurnInput,
        policy: ExecutionPolicy,
        context: ContextBundle,
        event_tx: mpsc::Sender<ProviderEvent>,
        cancel: CancellationToken,
    ) -> Result<(), ProviderError> {
        if let Some(ref reg) = self.registry
            && let Some(inst_lock) = reg.checkout_instance(&self.provider_name)
        {
            let mut inst = inst_lock.lock().await;
            if let Err(e) = inst.update_context(context).await {
                reg.release_instance(&self.provider_name, inst_lock.clone());
                return Err(ProviderError::ExecutionFailed(format!(
                    "Failed to sync context to persistent instance: {e}"
                )));
            }

            let mut event_rx = match inst.send_message(&input.user_message).await {
                Ok(rx) => rx,
                Err(e) => {
                    reg.release_instance(&self.provider_name, inst_lock.clone());
                    return Err(ProviderError::ExecutionFailed(format!(
                        "Failed to execute on persistent instance: {e}"
                    )));
                }
            };
            drop(inst); // Unlock early

            let mut response_text = String::new();
            let mut failed = false;
            let provider_name = self.provider_name.clone();

            loop {
                tokio::select! {
                    _ = cancel.cancelled() => {
                        failed = true;
                        break;
                    }
                    pe_opt = event_rx.recv() => {
                        if let Some(mut pe) = pe_opt {
                            pe.provider = provider_name.clone();
                            pe.turn_id = turn_id;
                            if pe.event_type == switchyard_provider_api::EventType::TurnFailed {
                                failed = true;
                            }
                            if let Some(text) = pe.payload.get("text").and_then(|t| t.as_str()) {
                                response_text.push_str(text);
                            } else if let Some(result_text) = pe.payload.get("result").and_then(|r| r.as_str()) {
                                response_text.push_str(result_text);
                            }
                            if event_tx.send(pe).await.is_err() {
                                break;
                            }
                        } else {
                            break;
                        }
                    }
                }
            }

            reg.release_instance(&self.provider_name, inst_lock.clone());

            let mut metadata = HashMap::new();
            metadata.insert("raw_stdout".to_string(), serde_json::Value::String(response_text.clone()));

            let turn_result = TurnResult {
                response_text,
                exit_code: if failed { Some(1) } else { Some(0) },
                stderr: None,
                metadata,
            };
            let artifact_bundle = ArtifactBundle { artifacts: vec![] };

            self.results.lock().await.insert(turn_id, (turn_result, artifact_bundle));
            Ok(())
        } else {
            self.inner.start_turn(turn_id, input, policy, context, event_tx, cancel).await
        }
    }

    async fn finalize_turn(
        &self,
        turn_id: Uuid,
    ) -> Result<(TurnResult, ArtifactBundle), ProviderError> {
        if let Some(res) = self.results.lock().await.remove(&turn_id) {
            Ok(res)
        } else {
            self.inner.finalize_turn(turn_id).await
        }
    }

    fn as_persistent(&self) -> Option<&dyn PersistentProvider> {
        self.inner.as_persistent()
    }
}
