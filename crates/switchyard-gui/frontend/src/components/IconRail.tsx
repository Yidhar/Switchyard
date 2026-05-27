import React from 'react';
import {
  MessageSquare,
  FolderOpen,
  GitBranch,
  Activity,
  Settings as SettingsIcon,
} from 'lucide-react';

/// Modes the second column (Workspace / Files / Source Control) can show.
/// Terminal is deliberately not a rail mode — its toggle lives in the
/// bottom StatusBar (matching VS Code's status-bar-driven terminal panel).
export type RailMode = 'chat' | 'files' | 'source_control';

interface IconRailProps {
  mode: RailMode;
  onModeChange: (mode: RailMode) => void;
  /// Open the slide-in diagnostics drawer.
  onOpenDiagnostics: () => void;
  /// Open settings modal.
  onOpenSettings: () => void;
}

interface RailButtonProps {
  active: boolean;
  onClick: () => void;
  title: string;
  children: React.ReactNode;
}

const RailButton: React.FC<RailButtonProps> = ({ active, onClick, title, children }) => (
  <button
    type="button"
    onClick={onClick}
    title={title}
    className={`icon-rail-button ${active ? 'is-active' : ''}`}
    style={{
      width: 40,
      height: 40,
      display: 'inline-flex',
      alignItems: 'center',
      justifyContent: 'center',
      background: active ? 'rgba(99, 102, 241, 0.15)' : 'transparent',
      border: 'none',
      borderRadius: 6,
      color: active ? 'var(--color-primary)' : 'var(--text-muted)',
      cursor: 'pointer',
      transition: 'background 120ms ease, color 120ms ease',
      position: 'relative',
    }}
  >
    {active && (
      <span
        aria-hidden
        style={{
          position: 'absolute',
          left: -10,
          top: 8,
          bottom: 8,
          width: 3,
          borderRadius: 2,
          background: 'var(--color-primary)',
        }}
      />
    )}
    {children}
  </button>
);

export const IconRail: React.FC<IconRailProps> = ({
  mode,
  onModeChange,
  onOpenDiagnostics,
  onOpenSettings,
}) => {
  return (
    <div
      className="icon-rail"
      style={{
        gridColumn: 1,
        gridRow: '2 / 4',
        width: 56,
        minHeight: 0,
        background: 'rgba(0, 0, 0, 0.35)',
        borderRight: '1px solid var(--border-muted)',
        display: 'flex',
        flexDirection: 'column',
        alignItems: 'center',
        padding: '12px 0',
        gap: 4,
        flexShrink: 0,
      }}
    >
      {/* Brand mark */}
      <div
        style={{
          width: 32,
          height: 32,
          borderRadius: 6,
          background: 'linear-gradient(135deg, var(--color-primary), var(--color-secondary))',
          color: '#fff',
          display: 'inline-flex',
          alignItems: 'center',
          justifyContent: 'center',
          fontWeight: 700,
          fontSize: 13,
          letterSpacing: '-0.02em',
          marginBottom: 16,
        }}
        title="Switchyard"
      >
        SY
      </div>

      {/* Mode group — selects what the second column shows */}
      <RailButton
        active={mode === 'chat'}
        onClick={() => onModeChange('chat')}
        title="Chat — session history"
      >
        <MessageSquare size={18} />
      </RailButton>
      <RailButton
        active={mode === 'files'}
        onClick={() => onModeChange('files')}
        title="Files — workspace file tree"
      >
        <FolderOpen size={18} />
      </RailButton>
      <RailButton
        active={mode === 'source_control'}
        onClick={() => onModeChange('source_control')}
        title="Source Control — git status + commit"
      >
        <GitBranch size={18} />
      </RailButton>

      {/* Diagnostics drawer toggle */}
      <div style={{ marginTop: 16 }}>
        <RailButton
          active={false}
          onClick={onOpenDiagnostics}
          title="Diagnostics — telemetry, workers, provider status"
        >
          <Activity size={18} />
        </RailButton>
      </div>

      {/* Spacer pushes the next group to the bottom */}
      <div style={{ flex: 1 }} />

      {/* Bottom group: keep only wired controls. Placeholder Help/Feedback
          buttons were removed so the rail distribution stays clean. */}
      <RailButton
        active={false}
        onClick={onOpenSettings}
        title="Settings"
      >
        <SettingsIcon size={18} />
      </RailButton>
    </div>
  );
};

export default IconRail;
