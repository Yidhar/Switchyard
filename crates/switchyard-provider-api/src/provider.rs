use async_trait::async_trait;
use std::collections::HashMap;
use std::sync::Arc;
use tokio::sync::mpsc;
use tokio_util::sync::CancellationToken;
use uuid::Uuid;

use crate::{
    ArtifactBundle, ContextBundle, ExecutionPolicy, ProbeResult, ProviderError, ProviderEvent,
    TurnInput, TurnResult,
};

#[async_trait]
pub trait Provider: Send + Sync {
    async fn probe(&self) -> Result<ProbeResult, ProviderError>;

    /// Start a turn. The caller provides the canonical `turn_id` so that
    /// all ProviderEvents emitted through `event_tx` can be correlated
    /// back to the correct turn — even when multiple turns run concurrently
    /// across core and peer providers.
    ///
    /// `cancel` is signalled when the user requests cancellation (e.g. Esc).
    /// Implementations should select on `cancel.cancelled()` alongside their
    /// work and abort promptly (kill subprocesses, etc.) when triggered.
    async fn start_turn(
        &self,
        turn_id: Uuid,
        input: TurnInput,
        policy: ExecutionPolicy,
        context: ContextBundle,
        event_tx: mpsc::Sender<ProviderEvent>,
        cancel: CancellationToken,
    ) -> Result<(), ProviderError>;

    /// Finalize the turn identified by `turn_id`.
    /// The provider must use this to collect the final result and artifacts
    /// for the specific turn, not rely on hidden internal state.
    async fn finalize_turn(
        &self,
        turn_id: Uuid,
    ) -> Result<(TurnResult, ArtifactBundle), ProviderError>;

    fn as_persistent(&self) -> Option<&dyn PersistentProvider> {
        None
    }
}

#[async_trait]
pub trait PersistentProvider: Send + Sync {
    async fn start_persistent_instance(
        &self,
        envs: HashMap<String, String>,
    ) -> Result<Box<dyn LiveInstance>, ProviderError>;
}

#[async_trait]
pub trait LiveInstance: Send + Sync {
    async fn send_message(&mut self, text: &str) -> Result<mpsc::Receiver<ProviderEvent>, ProviderError>;
    async fn update_context(&mut self, context: ContextBundle) -> Result<(), ProviderError>;
    async fn terminate(&mut self) -> Result<(), ProviderError>;
    fn is_healthy(&mut self) -> bool {
        true
    }
}

pub trait LiveInstanceRegistry: Send + Sync {
    fn has_live_instance(&self, provider: &str) -> bool;
    fn checkout_instance(&self, provider: &str) -> Option<Arc<tokio::sync::Mutex<dyn LiveInstance>>>;
    fn release_instance(&self, provider: &str, instance: Arc<tokio::sync::Mutex<dyn LiveInstance>>);
    fn register_instance(&self, provider: &str, instance: Box<dyn LiveInstance>);
}

#[async_trait]
impl<T: ?Sized + LiveInstance> LiveInstance for Box<T> {
    async fn send_message(&mut self, text: &str) -> Result<mpsc::Receiver<ProviderEvent>, ProviderError> {
        (**self).send_message(text).await
    }
    async fn update_context(&mut self, context: ContextBundle) -> Result<(), ProviderError> {
        (**self).update_context(context).await
    }
    async fn terminate(&mut self) -> Result<(), ProviderError> {
        (**self).terminate().await
    }
    fn is_healthy(&mut self) -> bool {
        (**self).is_healthy()
    }
}

