use serde::{Deserialize, Serialize};
use std::fmt;

/// Describes how a provider exposes the HYARD surface to the core.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum HostSurfaceKind {
    NativeSlash,
    NativeCustomCommand,
    Skill,
    Plugin,
    ShellFallback,
    Unknown,
}

impl fmt::Display for HostSurfaceKind {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::NativeSlash => write!(f, "native_slash"),
            Self::NativeCustomCommand => write!(f, "native_custom_command"),
            Self::Skill => write!(f, "skill"),
            Self::Plugin => write!(f, "plugin"),
            Self::ShellFallback => write!(f, "shell_fallback"),
            Self::Unknown => write!(f, "unknown"),
        }
    }
}

/// Probe details for the host-native surface.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct HostSurfaceProbe {
    pub kind: HostSurfaceKind,
    pub installed: bool,
    pub configured: bool,
    pub discoverable: bool,
    pub notes: Vec<String>,
}

impl HostSurfaceProbe {
    /// Surface is fully ready.
    pub fn ready(kind: HostSurfaceKind) -> Self {
        Self {
            kind,
            installed: true,
            configured: true,
            discoverable: true,
            notes: vec![],
        }
    }

    /// Surface exists but requires additional notes.
    pub fn flagged(kind: HostSurfaceKind, notes: Vec<String>) -> Self {
        Self {
            kind,
            installed: true,
            configured: true,
            discoverable: true,
            notes,
        }
    }

    /// Surface is unavailable or still unknown.
    pub fn unavailable(notes: Vec<String>) -> Self {
        Self {
            kind: HostSurfaceKind::Unknown,
            installed: false,
            configured: false,
            discoverable: false,
            notes,
        }
    }

    /// Whether the host surface is fully ready for user/model discovery.
    pub fn is_ready(&self) -> bool {
        self.installed && self.configured && self.discoverable
    }
}

impl Default for HostSurfaceProbe {
    fn default() -> Self {
        Self {
            kind: HostSurfaceKind::Unknown,
            installed: false,
            configured: false,
            discoverable: false,
            notes: Vec::new(),
        }
    }
}
