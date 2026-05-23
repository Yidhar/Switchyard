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

export type SandboxMode = 'read-only' | 'workspace-write' | 'danger-full-access';

export interface SandboxConfig {
  mode: SandboxMode;
  allowed_paths: string[];
}

export interface SwitchyardConfig {
  core: CoreConfig;
  providers: Record<string, ProviderConfig>;
  sandbox: SandboxConfig;
  store: StoreConfig;
}

export type AttachmentKind = 'image' | 'file';

export interface InputAttachment {
  path: string;
  name: string;
  kind: AttachmentKind;
  mimeType?: string | null;
}

export type ImageAttachment = InputAttachment;

export interface SendPayload {
  text: string;
  imagePaths: string[];
  filePaths?: string[];
}

export interface Session {
  session_id: string;
  /// Workspace this session belongs to. May be the nil UUID for legacy
  /// sessions that predate the workspace concept — the GUI lazily binds
  /// those to the current workspace on first access.
  workspace_id: string;
  created_at: string;
  updated_at: string;
  active_core: string;
  enabled_peers: string[];
  mode: string;
  summary: string | null;
  name?: string | null;
  native_bindings?: Record<string, string>;
}

/// Mirrors `switchyard_session::Workspace`. A workspace owns a list of
/// sessions plus its own roots and (eventually) per-workspace settings.
/// Persisted at `~/.switchyard/workspaces.json`.
export interface Workspace {
  workspace_id: string;
  name: string;
  primary_root: string;
  extra_roots: string[];
  created_at: string;
  updated_at: string;
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

/// Snapshot of one persistent instance bound to a session. Matches the
/// `InstanceMetadataView` shape returned by the `list_session_workers`
/// Tauri command. `kind = 'core'` is the session's primary provider;
/// `kind = 'worker'` is a team worker spawned by the Core.
export interface InstanceMetadata {
  instance_id: string;
  provider: string;
  session_id: string;
  label: string | null;
  kind: 'core' | 'worker';
  spawned_at: string;
  state: 'spawning' | 'idle' | 'busy' | 'retrying' | 'dying' | 'dead';
  in_flight_turn_id: string | null;
}
