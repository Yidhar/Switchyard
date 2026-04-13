use async_trait::async_trait;
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
}
