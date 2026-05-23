pub mod error;
pub mod event_mapper;
pub mod fake_provider;
pub mod instance;
pub mod ipc;
pub mod policy;
pub mod proxy;
pub mod registry;
pub mod router;
pub mod runtime_events;
pub mod turn_runner;

pub use error::CoreError;
pub use instance::InstancePool;
pub use proxy::PersistentProviderProxy;
// Compatibility re-exports — WorkerSupervisor moved to switchyard-orchestrator
// to avoid a cyclic dependency (orchestrator needs core to use supervisor in
// execute_delegate; core depends on orchestrator for the call site).
pub use event_mapper::map_provider_event;
pub use fake_provider::FakeProvider;
pub use policy::{execution_policy_from_config, execution_policy_from_config_with_overrides};
pub use registry::{ProviderRegistry, build_peer_catalog, build_peer_catalog_probed};
pub use router::{
    RoutedTurnOutput, RouterPromptInjection, run_routed_turn, run_routed_turn_observable,
    run_routed_turn_observable_with_policy, run_routed_turn_observable_with_policy_and_attachments,
    run_routed_turn_observable_with_policy_attachments_and_prompt_injection,
    run_routed_turn_with_archive, run_routed_turn_with_archive_and_policy,
};
pub use runtime_events::RuntimeEvent;
pub use switchyard_orchestrator::{RetryPolicy, SpawnRecipe, SupervisedOutcome, WorkerSupervisor};
pub use turn_runner::{
    TurnOutput, TurnPhase, run_turn, run_turn_full, run_turn_full_with_policy, run_turn_phased,
    run_turn_phased_with_policy, run_turn_with_archive, run_turn_with_policy,
};
