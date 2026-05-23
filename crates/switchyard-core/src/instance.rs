//! Compatibility re-export shim.
//!
//! `InstancePool` used to live here, but moved to `switchyard-provider-api`
//! so the default `LiveInstanceRegistry` implementation ships alongside the
//! trait — that's what unblocks `switchyard-orchestrator` using `InstancePool`
//! in its tests without depending back on `switchyard-core` (which would
//! cycle, since core already depends on orchestrator).
//!
//! External callers using `switchyard_core::InstancePool` or
//! `switchyard_core::instance::InstancePool` keep working through this
//! re-export; the actual definition is at
//! `switchyard_provider_api::pool::InstancePool`.

pub use switchyard_provider_api::InstancePool;
