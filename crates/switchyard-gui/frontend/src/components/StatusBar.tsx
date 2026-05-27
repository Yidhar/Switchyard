import React from 'react';
import { Terminal as TerminalIcon, Activity, Cpu, FolderGit2, AlertCircle } from 'lucide-react';
import type { Workspace } from '../types';

interface StatusBarProps {
  workspace: Workspace | null;
  coreProvider: string | null;
  workerCount: number;
  isGenerating: boolean;
  terminalOpen: boolean;
  onToggleTerminal: () => void;
  onOpenDiagnostics: () => void;
  /// Optional banner content (e.g. last error). Renders right-aligned
  /// before the diagnostics slot. Hidden when null.
  errorBanner?: string | null;
}

/// VS Code-style bottom status bar. It starts after the fixed icon rail so
/// the rail can own its full-height vertical rhythm; clickable items cover
/// Terminal / Diagnostics / workspace info. Items are intentionally minimal —
/// branch / lint counters land when their backends do.
export const StatusBar: React.FC<StatusBarProps> = ({
  workspace,
  coreProvider,
  workerCount,
  isGenerating,
  terminalOpen,
  onToggleTerminal,
  onOpenDiagnostics,
  errorBanner,
}) => {
  return (
    <div
      className="status-bar"
      style={{
        gridColumn: '2 / -1',
        gridRow: 3,
        display: 'flex',
        alignItems: 'center',
        height: 22,
        background: 'var(--color-primary, #3b82f6)',
        // Slightly darker than full --color-primary so it's distinct
        // from buttons. VS Code uses a similar derivation.
        backgroundColor: 'rgba(59, 130, 246, 0.85)',
        color: '#fff',
        fontSize: 11,
        userSelect: 'none',
        flexShrink: 0,
        borderTop: '1px solid rgba(0, 0, 0, 0.2)',
      }}
    >
      {/* Left cluster — workspace + core info. */}
      <StatusItem title={workspace ? `Workspace: ${workspace.primary_root}` : 'No workspace'}>
        <FolderGit2 size={11} />
        <span style={{ marginLeft: 4 }}>
          {workspace?.name ?? '—'}
        </span>
      </StatusItem>

      {coreProvider && (
        <StatusItem title={`Active core provider: ${coreProvider}`}>
          <Cpu size={11} />
          <span style={{ marginLeft: 4 }}>{coreProvider}</span>
        </StatusItem>
      )}

      {isGenerating && (
        <StatusItem
          title="A turn is currently running"
          style={{ background: 'rgba(0, 0, 0, 0.15)' }}
        >
          <Activity size={11} className="spin" />
          <span style={{ marginLeft: 4 }}>Running</span>
        </StatusItem>
      )}

      {errorBanner && (
        <StatusItem
          title={errorBanner}
          style={{
            background: 'rgba(239, 68, 68, 0.4)',
            color: '#fff',
          }}
        >
          <AlertCircle size={11} />
          <span
            style={{
              marginLeft: 4,
              maxWidth: 320,
              overflow: 'hidden',
              textOverflow: 'ellipsis',
              whiteSpace: 'nowrap',
            }}
          >
            {errorBanner}
          </span>
        </StatusItem>
      )}

      <div style={{ flex: 1 }} />

      {/* Right cluster — opens drawers / panels. Terminal is the
          highlight: clicking toggles the bottom panel just like in
          VS Code's status bar (the "Ports", "Problems", "Output" etc
          items there work the same way). */}
      <StatusItem
        title="Workers — live team workers plus active background delegates (open diagnostics for details)"
        onClick={onOpenDiagnostics}
      >
        <span>Workers:</span>
        <span
          style={{
            marginLeft: 4,
            fontWeight: workerCount > 0 ? 700 : 400,
          }}
        >
          {workerCount}
        </span>
      </StatusItem>
      <StatusItem
        title={terminalOpen ? 'Hide terminal panel' : 'Show terminal panel'}
        onClick={onToggleTerminal}
        active={terminalOpen}
      >
        <TerminalIcon size={11} />
        <span style={{ marginLeft: 4 }}>Terminal</span>
      </StatusItem>
    </div>
  );
};

const StatusItem: React.FC<{
  children: React.ReactNode;
  title?: string;
  onClick?: () => void;
  active?: boolean;
  style?: React.CSSProperties;
}> = ({ children, title, onClick, active, style }) => {
  const clickable = !!onClick;
  return (
    <div
      role={clickable ? 'button' : undefined}
      onClick={onClick}
      title={title}
      style={{
        display: 'inline-flex',
        alignItems: 'center',
        height: '100%',
        padding: '0 10px',
        cursor: clickable ? 'pointer' : 'default',
        background: active ? 'rgba(0, 0, 0, 0.25)' : 'transparent',
        transition: 'background 100ms ease',
        ...style,
      }}
      onMouseEnter={(e) => {
        if (clickable && !active) {
          e.currentTarget.style.background = 'rgba(255, 255, 255, 0.12)';
        }
      }}
      onMouseLeave={(e) => {
        if (clickable && !active) {
          e.currentTarget.style.background = 'transparent';
        }
      }}
    >
      {children}
    </div>
  );
};

export default StatusBar;
