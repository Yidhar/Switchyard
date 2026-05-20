// Interfaces for Switchyard Config
export interface CoreConfig {
  default_provider: string;
  default_peers: string[];
}

export interface ProviderConfig {
  command: string;
  args: string[];
  env: Record<string, string>;
  timeout_secs: number;
  backend: string | null;
}

export interface StoreConfig {
  backend: 'jsonl' | 'sqlite';
  path: string;
}

export interface SwitchyardConfig {
  core: CoreConfig;
  providers: Record<string, ProviderConfig>;
  store: StoreConfig;
}

export interface Session {
  session_id: string;
  created_at: string;
  updated_at: string;
  active_core: string;
  enabled_peers: string[];
  mode: string;
  summary: string | null;
  name?: string | null;
  native_bindings?: Record<string, string>;
}

export interface Turn {
  turn_id: string;
  session_id: string;
  origin: 'user' | 'delegate' | 'system';
  provider: string;
  role: 'core' | 'worker' | 'reviewer' | 'analyst';
  user_message: string;
  provider_response: string | null;
  error_message: string | null;
  status: 'pending' | 'running' | 'completed' | 'failed' | 'cancelled';
  started_at: string;
  completed_at: string | null;
  delegated_by: string | null;
}

export interface TelemetryLog {
  timestamp: string;
  tag: 'core' | 'peer' | 'sys' | 'info';
  message: string;
}

export interface HostSurfaceProbe {
  kind: string;
  installed: boolean;
  configured: boolean;
  discoverable: boolean;
  notes: string[];
}

export interface ProviderStatus {
  provider_id: string;
  backend: string | null;
  command: string | null;
  args: string[];
  timeout_secs: number | null;
  configured: boolean;
  registered: boolean;
  is_default_core: boolean;
  is_default_peer: boolean;
  roles: string[];
  available: boolean;
  version: string | null;
  capabilities: string[];
  issues: string[];
  host_surface: HostSurfaceProbe | null;
  error: string | null;
  checked_at: string;
}
