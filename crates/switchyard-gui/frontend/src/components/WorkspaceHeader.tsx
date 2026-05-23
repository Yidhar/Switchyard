import React, { useState, useEffect } from 'react';
import { ChevronDown, FolderOpen, FolderPlus, Check, Edit2, X, Folder } from 'lucide-react';
import { open as openDialog } from '@tauri-apps/plugin-dialog';
import type { Workspace } from '../types';

interface WorkspaceHeaderProps {
  current: Workspace | null;
  workspaces: Workspace[];
  onSwitch: (workspaceId: string) => void;
  onCreate: (primaryRoot: string, name: string | null) => void;
  onRename: (workspaceId: string, name: string) => void;
  /// Replace the active workspace's extra_roots list. Backend's
  /// `update_workspace` Tauri command accepts the full list to set.
  onUpdateExtraRoots: (workspaceId: string, extraRoots: string[]) => void;
}

/// Compact header that sits at the top of the second column (the
/// Workspace panel). Shows the currently-active workspace's name +
/// path, plus a dropdown for switching, opening a new folder, or
/// renaming. The "Open Folder…" affordance launches the OS-native
/// directory picker via the Tauri dialog plugin — same mental model
/// as VS Code's File → Open Folder.
export const WorkspaceHeader: React.FC<WorkspaceHeaderProps> = ({
  current,
  workspaces,
  onSwitch,
  onCreate,
  onRename,
  onUpdateExtraRoots,
}) => {
  const [menuOpen, setMenuOpen] = useState(false);
  const [renaming, setRenaming] = useState(false);
  const [renameDraft, setRenameDraft] = useState('');

  // Close the dropdown when the user clicks anywhere outside it.
  // Bound at the window level so a click on any other panel dismisses
  // the menu cleanly (matches VS Code's command-palette UX).
  useEffect(() => {
    if (!menuOpen) return;
    const onDocClick = (e: MouseEvent) => {
      const target = e.target as HTMLElement | null;
      if (target && target.closest('.workspace-header')) return;
      setMenuOpen(false);
    };
    document.addEventListener('mousedown', onDocClick);
    return () => document.removeEventListener('mousedown', onDocClick);
  }, [menuOpen]);

  /// Launch the native folder picker. Returns the selected absolute
  /// path or `null` if the user cancelled. Errors fall through as
  /// alerts — they're usually permission denials and worth showing.
  const pickFolder = async (title: string): Promise<string | null> => {
    try {
      const picked = await openDialog({
        directory: true,
        multiple: false,
        title,
      });
      if (typeof picked !== 'string') return null;
      return picked;
    } catch (e) {
      alert(`Folder picker failed: ${e}`);
      return null;
    }
  };

  const handleOpenFolder = async () => {
    const path = await pickFolder('Open Folder as Workspace');
    if (!path) return;
    // The display name defaults to the path's leaf segment, matching
    // VS Code's "Open Folder" behavior. The user can rename later.
    const name = leafName(path);
    onCreate(path, name || null);
    setMenuOpen(false);
  };

  const handleAddExtraRoot = async () => {
    if (!current) return;
    const path = await pickFolder('Add Folder to Workspace');
    if (!path) return;
    if (current.extra_roots.includes(path)) return;
    onUpdateExtraRoots(current.workspace_id, [...current.extra_roots, path]);
  };

  const startRename = () => {
    setRenameDraft(current?.name ?? '');
    setRenaming(true);
  };

  const commitRename = () => {
    const trimmed = renameDraft.trim();
    if (trimmed && current) {
      onRename(current.workspace_id, trimmed);
    }
    setRenaming(false);
  };

  return (
    <div
      className="workspace-header"
      style={{
        position: 'relative',
        height: 64,
        padding: '0 14px',
        borderBottom: '1px solid var(--border-muted)',
        background: 'rgba(255, 255, 255, 0.015)',
        display: 'flex',
        flexDirection: 'column',
        justifyContent: 'center',
        gap: 4,
        boxSizing: 'border-box',
        flexShrink: 0,
      }}
    >
      {/* Row 1 — section label + dropdown trigger. Mirrors VS Code's
          "EXPLORER" / "SWITCHYARD" headers. */}
      <div style={{ display: 'flex', alignItems: 'center', gap: 8 }}>
        <div
          style={{
            fontSize: 10,
            fontWeight: 700,
            color: 'var(--text-muted)',
            letterSpacing: '0.5px',
            textTransform: 'uppercase',
          }}
        >
          Workspace
        </div>
        <div style={{ flex: 1 }} />
        <button
          type="button"
          onClick={() => setMenuOpen((v) => !v)}
          title="Manage workspaces"
          style={{
            background: 'transparent',
            border: '1px solid var(--border-muted)',
            borderRadius: 4,
            padding: '2px 6px',
            color: 'var(--text-secondary)',
            cursor: 'pointer',
            display: 'inline-flex',
            alignItems: 'center',
            gap: 2,
            fontSize: 11,
          }}
        >
          <ChevronDown size={12} />
        </button>
      </div>

      {/* Row 2 — workspace name + rename pencil. Path is no longer
          rendered inline; the bottom StatusBar shows it (Default ▸
          path) and the dropdown menu has full path per entry. Keeping
          the header at 64px lets it line up exactly with the chat
          column's title bar and the Canvas's tab + file-header rows. */}
      <div style={{ display: 'flex', alignItems: 'center', gap: 8 }}>
        <FolderOpen size={14} color="var(--text-secondary)" />
        {renaming ? (
          <div style={{ display: 'flex', alignItems: 'center', gap: 4, flex: 1 }}>
            <input
              type="text"
              value={renameDraft}
              onChange={(e) => setRenameDraft(e.target.value)}
              autoFocus
              onKeyDown={(e) => {
                if (e.key === 'Enter') commitRename();
                else if (e.key === 'Escape') setRenaming(false);
              }}
              style={{
                flex: 1,
                background: 'rgba(0, 0, 0, 0.3)',
                border: '1px solid var(--border-muted)',
                borderRadius: 3,
                color: 'var(--text-primary)',
                padding: '2px 6px',
                fontSize: 13,
                minWidth: 0,
              }}
            />
            <button
              type="button"
              onClick={commitRename}
              style={iconBtnStyle}
              title="Save"
            >
              <Check size={14} />
            </button>
            <button
              type="button"
              onClick={() => setRenaming(false)}
              style={{ ...iconBtnStyle, color: 'var(--text-muted)' }}
              title="Cancel"
            >
              <X size={14} />
            </button>
          </div>
        ) : (
          <>
            <div
              style={{
                fontWeight: 600,
                fontSize: 14,
                color: 'var(--text-primary)',
                flex: 1,
                whiteSpace: 'nowrap',
                overflow: 'hidden',
                textOverflow: 'ellipsis',
              }}
              title={current ? `${current.name} · ${current.primary_root}` : 'No workspace'}
            >
              {current?.name ?? 'No workspace'}
            </div>
            {current && (
              <button
                type="button"
                onClick={startRename}
                title="Rename workspace"
                style={{ ...iconBtnStyle, opacity: 0.6 }}
              >
                <Edit2 size={12} />
              </button>
            )}
          </>
        )}
      </div>

      {menuOpen && (
        <div
          style={{
            position: 'absolute',
            top: '100%',
            right: 12,
            zIndex: 20,
            background: 'rgba(20, 20, 25, 0.98)',
            border: '1px solid var(--border-muted)',
            borderRadius: 6,
            boxShadow: '0 4px 12px rgba(0, 0, 0, 0.5)',
            minWidth: 280,
            maxHeight: 420,
            overflow: 'auto',
            padding: 4,
          }}
        >
          {/* Top action — equivalent to VS Code's "Open Folder…".
              Launches the OS-native folder picker; on success creates
              + switches to the new workspace in one motion. */}
          <button
            type="button"
            onClick={() => void handleOpenFolder()}
            style={menuActionStyle}
          >
            <FolderPlus size={14} color="var(--color-primary)" />
            <span style={{ flex: 1 }}>Open Folder…</span>
            <span style={{ fontSize: 10, color: 'var(--text-muted)' }}>
              Ctrl+K Ctrl+O
            </span>
          </button>

          {workspaces.length > 0 && (
            <>
              <div style={menuSectionLabelStyle}>Recent Workspaces</div>
              {workspaces.map((w) => (
                <button
                  key={w.workspace_id}
                  type="button"
                  onClick={() => {
                    onSwitch(w.workspace_id);
                    setMenuOpen(false);
                  }}
                  style={{
                    display: 'flex',
                    alignItems: 'flex-start',
                    width: '100%',
                    padding: '6px 8px',
                    background:
                      w.workspace_id === current?.workspace_id
                        ? 'rgba(99, 102, 241, 0.12)'
                        : 'transparent',
                    border: 'none',
                    borderRadius: 4,
                    color: 'var(--text-primary)',
                    cursor: 'pointer',
                    textAlign: 'left',
                    gap: 8,
                  }}
                >
                  <Folder
                    size={14}
                    style={{ flexShrink: 0, marginTop: 2, opacity: 0.8 }}
                  />
                  <div style={{ flex: 1, minWidth: 0 }}>
                    <div style={{ fontSize: 13, fontWeight: 500 }}>{w.name}</div>
                    <div
                      style={{
                        fontSize: 11,
                        color: 'var(--text-muted)',
                        whiteSpace: 'nowrap',
                        overflow: 'hidden',
                        textOverflow: 'ellipsis',
                        direction: 'rtl',
                        textAlign: 'left',
                      }}
                      title={w.primary_root}
                    >
                      {w.primary_root}
                    </div>
                  </div>
                </button>
              ))}
            </>
          )}

          {/* Extra roots editor — VS Code calls these "Folders" in a
              multi-root workspace. We use the same native folder picker
              instead of a text input. */}
          {current && (
            <>
              <div
                style={{
                  ...menuSectionLabelStyle,
                  borderTop: '1px solid var(--border-muted)',
                  marginTop: 4,
                  paddingTop: 10,
                }}
              >
                Additional Folders
              </div>
              {current.extra_roots.length === 0 && (
                <div
                  style={{
                    padding: '2px 8px 6px',
                    fontSize: 11,
                    color: 'var(--text-muted)',
                  }}
                >
                  None — only the primary folder is in scope.
                </div>
              )}
              {current.extra_roots.map((root, idx) => (
                <div
                  key={`${root}-${idx}`}
                  style={{
                    display: 'flex',
                    alignItems: 'center',
                    gap: 4,
                    padding: '2px 4px 2px 8px',
                  }}
                >
                  <Folder
                    size={12}
                    style={{ opacity: 0.7, flexShrink: 0 }}
                  />
                  <span
                    style={{
                      flex: 1,
                      fontSize: 11,
                      color: 'var(--text-primary)',
                      overflow: 'hidden',
                      textOverflow: 'ellipsis',
                      whiteSpace: 'nowrap',
                      direction: 'rtl',
                      textAlign: 'left',
                    }}
                    title={root}
                  >
                    {root}
                  </span>
                  <button
                    type="button"
                    onClick={() => {
                      const next = current.extra_roots.filter((_, i) => i !== idx);
                      onUpdateExtraRoots(current.workspace_id, next);
                    }}
                    title="Remove this folder"
                    style={iconBtnStyle}
                  >
                    <X size={12} />
                  </button>
                </div>
              ))}
              <button
                type="button"
                onClick={() => void handleAddExtraRoot()}
                style={{
                  ...menuActionStyle,
                  color: 'var(--color-primary)',
                  marginTop: 4,
                }}
              >
                <FolderPlus size={13} />
                <span style={{ flex: 1 }}>Add Folder to Workspace…</span>
              </button>
            </>
          )}
        </div>
      )}
    </div>
  );
};

const iconBtnStyle: React.CSSProperties = {
  background: 'transparent',
  border: 'none',
  color: 'var(--color-success)',
  cursor: 'pointer',
  padding: 2,
  display: 'inline-flex',
  alignItems: 'center',
};

const menuActionStyle: React.CSSProperties = {
  display: 'inline-flex',
  alignItems: 'center',
  gap: 8,
  width: '100%',
  padding: '8px 10px',
  background: 'transparent',
  border: 'none',
  borderRadius: 4,
  color: 'var(--text-primary)',
  cursor: 'pointer',
  fontSize: 13,
  textAlign: 'left',
};

const menuSectionLabelStyle: React.CSSProperties = {
  fontSize: 10,
  fontWeight: 700,
  color: 'var(--text-muted)',
  textTransform: 'uppercase',
  letterSpacing: '0.5px',
  padding: '10px 8px 4px',
};

function leafName(p: string): string {
  if (!p) return '';
  const n = p.replace(/\\/g, '/').replace(/\/+$/, '');
  const idx = n.lastIndexOf('/');
  return idx >= 0 ? n.slice(idx + 1) : n;
}

export default WorkspaceHeader;

// Hover affordance for the menu's clickable rows. Injected once so
// every menu instance picks it up without inline `onMouseEnter`
// gymnastics in the JSX.
if (typeof document !== 'undefined') {
  const id = 'switchyard-workspace-header-styles';
  if (!document.querySelector(`style[data-${id}]`)) {
    const s = document.createElement('style');
    s.setAttribute(`data-${id}`, 'true');
    s.textContent = `
      .workspace-header button:hover {
        background: rgba(255, 255, 255, 0.05) !important;
      }
    `;
    document.head.appendChild(s);
  }
}
