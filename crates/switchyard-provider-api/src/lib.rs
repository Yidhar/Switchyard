pub mod capability;
pub mod delegate;
pub mod error;
pub mod event;
pub mod host_surface;
pub mod identity;
pub mod peer;
pub mod pool;
pub mod provider;
pub mod role;
pub mod sentinel;
pub mod types;

pub use capability::ProviderCapability;
pub use delegate::{
    DelegateRequest, DelegateResponse, DelegateStatus, DelegateTask, DelegateTaskResult,
};
pub use error::{LabelConflict, ProviderError};
pub use event::{
    EventType, HyardJobObservation, ItemType, ProviderEvent, TerminalOutput, extract_display_text,
    extract_execution_telemetry, extract_hyard_job_observation, extract_terminal_output,
    is_empty_reasoning_payload,
};
pub use host_surface::{HostSurfaceKind, HostSurfaceProbe};
pub use identity::{ProbeResult, ProviderIdentity};
pub use peer::{PeerCatalog, PeerDescriptor, PromptMode, render_delegate_result_block};
pub use pool::InstancePool;
pub use provider::{
    InstanceKind, InstanceMetadata, InstanceState, LiveInstance, LiveInstanceRegistry,
    PersistentProvider, Provider,
};
pub use role::ProviderRole;
pub use sentinel::{extract_sentinel_blocks, parse_sentinel_json, strip_sentinel_blocks};
pub use tokio_util::sync::CancellationToken;
pub use types::{
    ARTIFACT_TYPE_RAW_OUTPUT, ArtifactBundle, ArtifactEntry, ContextBundle, EffectiveSandboxMode,
    ExecutionPolicy, ExecutionTelemetry, InputAttachment, TurnInput, TurnResult,
};
