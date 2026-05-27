import React, { useEffect, useRef, useState } from 'react';
import { invoke } from '@tauri-apps/api/core';
import { getCurrentWindow } from '@tauri-apps/api/window';
import {
  Activity,
  AppWindow,
  Check,
  Edit2,
  FolderGit2,
  FolderOpen,
  FolderPlus,
  GitBranch,
  MessageSquare,
  Settings,
  Terminal,
  X,
} from 'lucide-react';
import type { Workspace } from '../types';
import type { RailMode } from './IconRail';

type TopBarMenu = 'file' | 'workspace' | 'view';

interface AppTopBarProps {
  current: Workspace | null;
  workspaces: Workspace[];
  railMode: RailMode;
  terminalOpen: boolean;
  onRailModeChange: (mode: RailMode) => void;
  onSwitchWorkspace: (workspaceId: string) => void;
  onRenameWorkspace: (workspaceId: string, name: string) => void;
  onOpenFolder: () => void;
  onCloseWorkspace: () => void;
  onAddFolder: () => void;
  onRemoveExtraRoot: (root: string) => void;
  onOpenSettings: () => void;
  onToggleTerminal: () => void;
  onOpenDiagnostics: () => void;
}

/// Custom Tauri title/workbench bar. With `decorations: false`, this replaces
/// the native title bar and gives us VS Code-like File / Workspace / View
/// menus without keeping the workspace header pinned inside the sidebar.
export const AppTopBar: React.FC<AppTopBarProps> = ({
  current,
  workspaces,
  railMode,
  terminalOpen,
  onRailModeChange,
  onSwitchWorkspace,
  onRenameWorkspace,
  onOpenFolder,
  onCloseWorkspace,
  onAddFolder,
  onRemoveExtraRoot,
  onOpenSettings,
  onToggleTerminal,
  onOpenDiagnostics,
}) => {
  const rootRef = useRef<HTMLDivElement | null>(null);
  const [activeMenu, setActiveMenu] = useState<TopBarMenu | null>(null);
  const [isMaximized, setIsMaximized] = useState(false);

  useEffect(() => {
    if (!activeMenu) return;
    const handlePointerDown = (event: MouseEvent) => {
      const target = event.target as Node | null;
      if (target && rootRef.current?.contains(target)) return;
      setActiveMenu(null);
    };
    const handleKeyDown = (event: KeyboardEvent) => {
      if (event.key === 'Escape') setActiveMenu(null);
    };
    document.addEventListener('mousedown', handlePointerDown);
    document.addEventListener('keydown', handleKeyDown);
    return () => {
      document.removeEventListener('mousedown', handlePointerDown);
      document.removeEventListener('keydown', handleKeyDown);
    };
  }, [activeMenu]);

  useEffect(() => {
    const appWindow = getCurrentWindow();
    let cancelled = false;
    let unlistenResize: (() => void) | null = null;

    const syncMaximizedState = async () => {
      try {
        const next = await readWindowMaximizedState();
        if (!cancelled) setIsMaximized(next);
      } catch (error) {
        // In the browser/Vite preview there is no Tauri window backend.
        // Keep the custom title bar usable there instead of throwing.
        if (import.meta.env.DEV) {
          console.debug('Unable to read Tauri maximized state:', error);
        }
      }
    };

    void syncMaximizedState();
    void appWindow
      .onResized(syncMaximizedState)
      .then((unlisten) => {
        if (cancelled) {
          unlisten();
          return;
        }
        unlistenResize = unlisten;
      })
      .catch((error) => {
        if (import.meta.env.DEV) {
          console.debug('Unable to subscribe to Tauri resize events:', error);
        }
      });

    return () => {
      cancelled = true;
      unlistenResize?.();
    };
  }, []);

  const toggleMenu = (menu: TopBarMenu) => {
    setActiveMenu((currentMenu) => (currentMenu === menu ? null : menu));
  };

  const closeAfter = (handler: () => void | Promise<void>) => {
    setActiveMenu(null);
    void handler();
  };

  const renameCurrentWorkspace = () => {
    if (!current) return;
    const nextName = prompt('Rename workspace', current.name)?.trim();
    if (!nextName || nextName === current.name) return;
    onRenameWorkspace(current.workspace_id, nextName);
  };

  const minimizeWindow = () => {
    void runWindowCommand(
      () => getCurrentWindow().minimize(),
      () => invoke('app_window_minimize'),
      'Failed to minimize Tauri window',
    );
  };

  const toggleMaximizeWindow = async () => {
    const appWindow = getCurrentWindow();
    try {
      const maximized = await readWindowMaximizedState();
      if (maximized) {
        await runWindowCommand(
          () => appWindow.unmaximize(),
          () => invoke('app_window_unmaximize'),
          'Failed to restore Tauri window',
        );
      } else {
        await runWindowCommand(
          () => appWindow.maximize(),
          () => invoke('app_window_maximize'),
          'Failed to maximize Tauri window',
        );
      }
      setIsMaximized(await readWindowMaximizedState());
    } catch (error) {
      console.warn('Failed to toggle Tauri window maximize state:', error);
    }
  };

  const closeWindow = () => {
    void runWindowCommand(
      () => getCurrentWindow().close(),
      () => invoke('app_window_close'),
      'Failed to close Tauri window',
    );
  };

  const createNewWindow = () => {
    void invoke('app_window_new').catch((error) => {
      console.warn('Failed to create new Tauri window:', error);
    });
  };

  return (
    <div ref={rootRef} className="app-top-bar">
      <div
        className="app-top-bar-brand"
        data-tauri-drag-region
        onDoubleClick={toggleMaximizeWindow}
      >
        <span className="app-top-bar-brand-mark">SY</span>
        <span className="app-top-bar-brand-name">Switchyard</span>
      </div>

      <div className="app-top-bar-menus">
        <TopBarMenuButton
          label="File"
          open={activeMenu === 'file'}
          onClick={() => toggleMenu('file')}
        >
          <MenuItem icon={<AppWindow size={14} />} onClick={() => closeAfter(createNewWindow)}>
            New Window
          </MenuItem>
          <MenuSeparator />
          <MenuItem icon={<FolderOpen size={14} />} onClick={() => closeAfter(onOpenFolder)}>
            Open Folder…
          </MenuItem>
          <MenuItem
            icon={<FolderPlus size={14} />}
            onClick={() => closeAfter(onAddFolder)}
            disabled={!current}
          >
            Add Folder to Workspace…
          </MenuItem>
          <MenuItem
            icon={<X size={14} />}
            onClick={() => closeAfter(onCloseWorkspace)}
            disabled={!current}
          >
            Close Workspace
          </MenuItem>
          <MenuSeparator />
          <MenuItem icon={<Settings size={14} />} onClick={() => closeAfter(onOpenSettings)}>
            Settings
          </MenuItem>
        </TopBarMenuButton>

        <TopBarMenuButton
          label="Workspace"
          open={activeMenu === 'workspace'}
          onClick={() => toggleMenu('workspace')}
        >
          {current ? (
            <>
              <MenuItem icon={<Edit2 size={14} />} onClick={() => closeAfter(renameCurrentWorkspace)}>
                Rename Workspace…
              </MenuItem>
              <MenuItem icon={<FolderPlus size={14} />} onClick={() => closeAfter(onAddFolder)}>
                Add Folder to Workspace…
              </MenuItem>
              <MenuSeparator />
              <div className="app-top-bar-menu-section">Folders</div>
              <div className="app-top-bar-root-row" title={current.primary_root}>
                <FolderGit2 size={13} />
                <span>{leafName(current.primary_root) || current.primary_root}</span>
                <em>primary</em>
              </div>
              {current.extra_roots.length === 0 && (
                <div className="app-top-bar-menu-note">
                  No extra folders in this workspace.
                </div>
              )}
              {current.extra_roots.map((root) => (
                <div className="app-top-bar-root-row" key={root} title={root}>
                  <FolderOpen size={13} />
                  <span>{leafName(root) || root}</span>
                  <button
                    type="button"
                    className="app-top-bar-root-remove"
                    onClick={() => closeAfter(() => onRemoveExtraRoot(root))}
                    title="Remove folder from workspace"
                  >
                    <X size={12} />
                  </button>
                </div>
              ))}
              <MenuSeparator />
            </>
          ) : (
            <>
              <MenuItem icon={<FolderOpen size={14} />} onClick={() => closeAfter(onOpenFolder)}>
                Open Folder…
              </MenuItem>
              <MenuSeparator />
            </>
          )}

          {workspaces.length > 0 && (
            <>
              <div className="app-top-bar-menu-section">Recent Workspaces</div>
              {workspaces.map((workspace) => (
                <button
                  key={workspace.workspace_id}
                  type="button"
                  className={`app-top-bar-recent ${workspace.workspace_id === current?.workspace_id ? 'is-active' : ''}`}
                  onClick={() => closeAfter(() => onSwitchWorkspace(workspace.workspace_id))}
                  title={workspace.primary_root}
                >
                  {workspace.workspace_id === current?.workspace_id ? (
                    <Check size={13} />
                  ) : (
                    <FolderOpen size={13} />
                  )}
                  <span>
                    <strong>{workspace.name}</strong>
                    <small>{workspace.primary_root}</small>
                  </span>
                </button>
              ))}
            </>
          )}
        </TopBarMenuButton>

        <TopBarMenuButton
          label="View"
          open={activeMenu === 'view'}
          onClick={() => toggleMenu('view')}
        >
          <MenuItem
            icon={<MessageSquare size={14} />}
            onClick={() => closeAfter(() => onRailModeChange('chat'))}
            active={railMode === 'chat'}
          >
            Chat
          </MenuItem>
          <MenuItem
            icon={<FolderOpen size={14} />}
            onClick={() => closeAfter(() => onRailModeChange('files'))}
            active={railMode === 'files'}
          >
            Explorer
          </MenuItem>
          <MenuItem
            icon={<GitBranch size={14} />}
            onClick={() => closeAfter(() => onRailModeChange('source_control'))}
            active={railMode === 'source_control'}
          >
            Source Control
          </MenuItem>
          <MenuSeparator />
          <MenuItem
            icon={<Terminal size={14} />}
            onClick={() => closeAfter(onToggleTerminal)}
            active={terminalOpen}
          >
            Toggle Terminal
          </MenuItem>
          <MenuItem icon={<Activity size={14} />} onClick={() => closeAfter(onOpenDiagnostics)}>
            Diagnostics
          </MenuItem>
        </TopBarMenuButton>
      </div>

      <div
        className="app-top-bar-workspace is-display"
        data-tauri-drag-region
        onDoubleClick={toggleMaximizeWindow}
        title={
          current
            ? `${current.name}\n${current.primary_root}`
            : 'No workspace opened'
        }
      >
        <FolderGit2 size={14} />
        <span className="app-top-bar-workspace-name">
          {current?.name ?? 'No Workspace'}
        </span>
        {current && (
          <span className="app-top-bar-workspace-path">
            {formatCompactPath(current.primary_root)}
          </span>
        )}
      </div>

      <div
        className="app-top-bar-drag-fill"
        data-tauri-drag-region
        onDoubleClick={toggleMaximizeWindow}
      />

      <div className="app-top-bar-window-controls">
        <button
          type="button"
          className="app-window-button"
          onClick={minimizeWindow}
          title="Minimize"
          aria-label="Minimize window"
        >
          <span className="app-window-glyph app-window-glyph-minimize" aria-hidden />
        </button>
        <button
          type="button"
          className="app-window-button"
          onClick={toggleMaximizeWindow}
          title={isMaximized ? 'Restore' : 'Maximize'}
          aria-label={isMaximized ? 'Restore window' : 'Maximize window'}
        >
          <span
            className={`app-window-glyph ${
              isMaximized ? 'app-window-glyph-restore' : 'app-window-glyph-maximize'
            }`}
            aria-hidden
          />
        </button>
        <button
          type="button"
          className="app-window-button app-window-button-close"
          onClick={closeWindow}
          title="Close"
          aria-label="Close window"
        >
          <span className="app-window-glyph app-window-glyph-close" aria-hidden />
        </button>
      </div>
    </div>
  );
};

const TopBarMenuButton: React.FC<{
  label: string;
  open: boolean;
  onClick: () => void;
  children: React.ReactNode;
}> = ({ label, open, onClick, children }) => (
  <div className="app-top-bar-menu-host">
    <button
      type="button"
      className={`app-top-bar-menu-trigger ${open ? 'is-open' : ''}`}
      onClick={onClick}
    >
      {label}
    </button>
    {open && <div className="app-top-bar-menu">{children}</div>}
  </div>
);

const MenuItem: React.FC<{
  icon?: React.ReactNode;
  children: React.ReactNode;
  onClick: () => void;
  disabled?: boolean;
  active?: boolean;
}> = ({ icon, children, onClick, disabled, active }) => (
  <button
    type="button"
    className={`app-top-bar-menu-item ${active ? 'is-active' : ''}`}
    disabled={disabled}
    onClick={() => {
      if (!disabled) onClick();
    }}
  >
    <span className="app-top-bar-menu-icon">{icon}</span>
    <span>{children}</span>
    {active && <Check size={13} className="app-top-bar-menu-check" />}
  </button>
);

const MenuSeparator = () => <div className="app-top-bar-menu-separator" />;

function leafName(path: string): string {
  const normalised = path.replace(/\\/g, '/').replace(/\/+$/, '');
  const idx = normalised.lastIndexOf('/');
  return idx >= 0 ? normalised.slice(idx + 1) : normalised;
}

function formatCompactPath(path: string): string {
  const normalised = path.replace(/\\/g, '/');
  const parts = normalised.split('/').filter(Boolean);
  if (parts.length <= 2) return normalised;
  return `${parts[parts.length - 2]}/${parts[parts.length - 1]}`;
}

async function readWindowMaximizedState(): Promise<boolean> {
  try {
    return await getCurrentWindow().isMaximized();
  } catch (primaryError) {
    try {
      return await invoke<boolean>('app_window_is_maximized');
    } catch (fallbackError) {
      if (import.meta.env.DEV) {
        console.debug('Unable to read Tauri maximized state:', {
          primaryError,
          fallbackError,
        });
      }
      return false;
    }
  }
}

async function runWindowCommand(
  primary: () => Promise<unknown>,
  fallback: () => Promise<unknown>,
  message: string,
) {
  try {
    await primary();
  } catch (primaryError) {
    try {
      await fallback();
    } catch (fallbackError) {
      console.warn(message, { primaryError, fallbackError });
    }
  }
}

export default AppTopBar;
