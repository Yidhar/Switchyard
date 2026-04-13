use std::collections::HashSet;

use serde::{Deserialize, Serialize};

use crate::{capability::ProviderCapability, host_surface::HostSurfaceProbe};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ProviderIdentity {
    pub provider_id: String,
    pub backend_id: String,
    pub display_name: String,
    pub capabilities: HashSet<ProviderCapability>,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct ProbeResult {
    pub version: Option<String>,
    pub available: bool,
    pub capabilities: HashSet<ProviderCapability>,
    pub issues: Vec<String>,
    pub host_surface: HostSurfaceProbe,
}
