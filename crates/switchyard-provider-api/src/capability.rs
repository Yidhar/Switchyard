use serde::{Deserialize, Serialize};
use std::fmt;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
#[non_exhaustive]
pub enum ProviderCapability {
    HeadlessTurn,
    StreamingOutput,
    StructuredOutput,
    SessionResume,
    WorktreeHint,
    SubagentHint,
    ToolUseSignal,
    McpSupport,
    SdkBackend,
}

impl fmt::Display for ProviderCapability {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::HeadlessTurn => write!(f, "headless_turn"),
            Self::StreamingOutput => write!(f, "streaming_output"),
            Self::StructuredOutput => write!(f, "structured_output"),
            Self::SessionResume => write!(f, "session_resume"),
            Self::WorktreeHint => write!(f, "worktree_hint"),
            Self::SubagentHint => write!(f, "subagent_hint"),
            Self::ToolUseSignal => write!(f, "tool_use_signal"),
            Self::McpSupport => write!(f, "mcp_support"),
            Self::SdkBackend => write!(f, "sdk_backend"),
        }
    }
}
