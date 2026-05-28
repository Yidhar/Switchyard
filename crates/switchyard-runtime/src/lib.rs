//! Runtime authority primitives for Switchyard.
//!
//! This crate is the durable half of the runtime bus design: every lifecycle
//! change is committed to SQLite together with an ordered runtime event before
//! any IPC/UI broadcast happens.  IPC delivery can therefore be at-least-once
//! while this event log remains the exactly-once source of truth.

mod error;
pub mod protocol;
pub mod schema;
pub mod store;

pub use error::RuntimeError;
pub use protocol::{
    CreateHostJob, HostJobMutation, HostJobRecord, HostJobStatus, RuntimeEventRecord,
    RuntimeSnapshot, RuntimeWrite, WorkerInstanceState,
};
pub use store::RuntimeDb;
