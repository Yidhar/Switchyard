import { invoke } from '@tauri-apps/api/core';
import type { FileSnapshot } from './Canvas';

/// Read a file via the workspace-scoped `read_file` Tauri command.
/// Lives in a separate module from `Canvas.tsx` so React Fast Refresh
/// can hot-reload the component without invalidating this helper.
export async function fetchSnapshot(path: string): Promise<FileSnapshot> {
  return await invoke<FileSnapshot>('read_file', { path });
}

/// Persist a file via the workspace-scoped `write_file` Tauri command.
/// Returns the fresh snapshot (with updated size + path) so callers can
/// reset their dirty-state immediately.
export async function saveFile(
  path: string,
  content: string,
): Promise<FileSnapshot> {
  return await invoke<FileSnapshot>('write_file', { path, content });
}
