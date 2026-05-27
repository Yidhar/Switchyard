import { invoke } from '@tauri-apps/api/core';
import type { Session, Turn, SwitchyardConfig, ProviderStatus, SandboxMode } from '../types';

export const listSessions = (): Promise<Session[]> => {
  return invoke<Session[]>('list_sessions');
};

export const getSessionTurns = (sessionId: string): Promise<Turn[]> => {
  return invoke<Turn[]>('get_session_turns', { sessionId });
};

export const getSessionEvents = (sessionId: string): Promise<any[]> => {
  return invoke<any[]>('get_session_events', { sessionId });
};

export const createSession = (provider: string): Promise<Session> => {
  return invoke<Session>('create_session', { provider });
};

export const listProviderStatus = (): Promise<ProviderStatus[]> => {
  return invoke<ProviderStatus[]>('list_provider_status');
};

export const loadConfig = (): Promise<SwitchyardConfig> => {
  return invoke<SwitchyardConfig>('load_config');
};

export const saveConfig = (config: SwitchyardConfig): Promise<void> => {
  return invoke<void>('save_config', { config });
};

export const runTurn = (
  sessionId: string,
  message: string,
  provider?: string,
  sandboxMode?: SandboxMode,
  imagePaths: string[] = [],
  filePaths: string[] = [],
): Promise<string> => {
  return invoke<string>('run_turn', { sessionId, message, provider, sandboxMode, imagePaths, filePaths });
};

export const saveClipboardAttachment = (
  nameHint: string | undefined,
  mimeType: string | undefined,
  dataUrl: string,
): Promise<string> => {
  return invoke<string>('save_clipboard_attachment', { nameHint, mimeType, dataUrl });
};

export const persistAttachmentFile = (
  path: string,
  mimeType?: string | null,
): Promise<string> => {
  return invoke<string>('persist_attachment_file', { path, mimeType });
};

export const readImageAttachmentDataUrl = (
  path: string,
  mimeType?: string | null,
): Promise<string> => {
  return invoke<string>('read_image_attachment_data_url', { path, mimeType });
};

export const resolveToolApproval = (
  requestId: string,
  decision: 'approve' | 'deny',
  reason?: string,
): Promise<void> => {
  return invoke<void>('resolve_tool_approval', { requestId, decision, reason });
};

export const cancelTurn = (): Promise<void> => {
  return invoke<void>('cancel_turn');
};

export const updateSessionPeers = (sessionId: string, enabledPeers: string[]): Promise<void> => {
  return invoke<void>('update_session_peers', { sessionId, enabledPeers });
};
