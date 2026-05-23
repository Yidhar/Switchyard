//! Shared subprocess infrastructure for CLI-based providers.
//!
//! Provides command resolution, subprocess lifecycle management,
//! and generic version probing.

pub mod context_render;
pub mod helpers;
pub mod live;
pub mod probe;
pub mod resolve;
pub mod runner;
pub use live::SubprocessLiveInstance;

pub use context_render::render_context_bundle;
pub use helpers::{
    build_turn_result, check_auth_error, compose_prompt, default_cli_capabilities,
    effective_timeout_secs, emit_completion_event, handle_subprocess_error,
};
pub use probe::{VersionProbe, is_available, probe_version};
pub use resolve::{find_on_path, is_windows_batch_wrapper, resolve_command, resolve_npm_entry};
pub use runner::{
    StreamingOutputLine, SubprocessConfig, SubprocessError, SubprocessInvocationPlan,
    SubprocessOutput, build_subprocess_invocation_plan, resize_registered_pty, run_subprocess,
    run_subprocess_streaming, run_subprocess_streaming_until,
};
