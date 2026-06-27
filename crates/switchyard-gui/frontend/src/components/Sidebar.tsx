import React from 'react';
import { Plus, Trash2, Edit2, Check, X } from 'lucide-react';
import type { Session, SwitchyardConfig } from '../types';

interface SidebarProps {
  sessions: Session[];
  selectedSession: Session | null;
  newSessionProvider: string;
  setNewSessionProvider: (provider: string) => void;
  config: SwitchyardConfig | null;
  onCreateSession: () => void;
  onSelectSession: (session: Session) => void;
  onDeleteSession: (sessionId: string) => void;
  onRenameSession: (sessionId: string, newName: string) => void;
}

/// Render a timestamp the way Claude Code's task list does: "2分钟前",
/// "3小时前", "1周". Falls back to a YYYY-MM-DD style for items older
/// than 30 days so the row stays tight. We deliberately don't pull
/// `date-fns` for one helper — under 30 lines is cheaper than the dep.
function formatRelativeTime(iso: string): string {
  const then = new Date(iso).getTime();
  if (!Number.isFinite(then)) return '';
  const diff = Date.now() - then;
  const SEC = 1_000;
  const MIN = 60 * SEC;
  const HOUR = 60 * MIN;
  const DAY = 24 * HOUR;
  const WEEK = 7 * DAY;
  if (diff < MIN) return '刚刚';
  if (diff < HOUR) return `${Math.floor(diff / MIN)} 分钟前`;
  if (diff < DAY) return `${Math.floor(diff / HOUR)} 小时前`;
  if (diff < WEEK) return `${Math.floor(diff / DAY)} 天前`;
  if (diff < 30 * DAY) return `${Math.floor(diff / WEEK)} 周前`;
  // Older — collapse to a fixed date so the column doesn't read like
  // "47 周前".
  const d = new Date(then);
  const pad = (n: number) => n.toString().padStart(2, '0');
  return `${d.getFullYear()}-${pad(d.getMonth() + 1)}-${pad(d.getDate())}`;
}

export const Sidebar: React.FC<SidebarProps> = ({
  sessions,
  selectedSession,
  newSessionProvider,
  setNewSessionProvider,
  config,
  onCreateSession,
  onSelectSession,
  onDeleteSession,
  onRenameSession,
}) => {
  const providers = config ? Object.keys(config.providers || {}) : [];
  const defaults = ['codex', 'claude', 'gemini', 'antigravity', 'kohaku'];
  const allProviders = Array.from(new Set([...providers, ...defaults]));

  const [editingSessionId, setEditingSessionId] = React.useState<string | null>(null);
  const [editName, setEditName] = React.useState('');

  const handleStartEdit = (s: Session, e: React.MouseEvent) => {
    e.stopPropagation();
    setEditingSessionId(s.session_id);
    setEditName(s.name || s.session_id.substring(0, 8));
  };

  const handleSaveEdit = (s: Session, e: React.MouseEvent) => {
    e.stopPropagation();
    if (editName.trim()) {
      onRenameSession(s.session_id, editName.trim());
    }
    setEditingSessionId(null);
  };

  const handleCancelEdit = (e: React.MouseEvent) => {
    e.stopPropagation();
    setEditingSessionId(null);
  };

  return (
    <div className="sidebar glass-panel">
      {/* Brand mark + settings live in the IconRail now — Sidebar is
          a pure session-management surface inside the active workspace. */}
      <div className="sidebar-actions">
        <div style={{ display: 'flex', flexDirection: 'column', gap: '8px', marginBottom: '12px' }}>
          <label style={{ fontSize: '10px', fontWeight: 700, color: 'var(--text-muted)', textTransform: 'uppercase', letterSpacing: '0.5px' }}>
            Core Provider
          </label>
          <select 
            className="settings-select" 
            style={{ width: '100%', textTransform: 'capitalize', cursor: 'pointer', height: '38px' }}
            value={newSessionProvider}
            onChange={(e) => setNewSessionProvider(e.target.value)}
          >
            {allProviders.map((p) => (
              <option key={p} value={p}>
                {p}
              </option>
            ))}
          </select>
          <button
            className="btn-new-session"
            onClick={onCreateSession}
            style={{ height: '38px', display: 'flex', alignItems: 'center', justifyContent: 'center', gap: '6px' }}
          >
            <Plus size={16} />
            New Chat
          </button>
        </div>
      </div>

      <div className="session-list">
        {sessions.map((s) => {
          const isSelected = selectedSession?.session_id === s.session_id;
          const isEditing = editingSessionId === s.session_id;

          return (
            <div
              key={s.session_id}
              className={`session-item glass-panel-interactive ${isSelected ? 'active' : ''}`}
              onClick={() => onSelectSession(s)}
              style={{ position: 'relative', paddingRight: '12px' }}
            >
              {isEditing ? (
                <div style={{ display: 'flex', alignItems: 'center', gap: '4px', width: '100%', margin: '2px 0' }} onClick={(e) => e.stopPropagation()}>
                  <input
                    type="text"
                    value={editName}
                    onChange={(e) => setEditName(e.target.value)}
                    className="settings-input"
                    style={{ flex: 1, height: '24px', fontSize: '12px', padding: '0 4px', background: 'rgba(0,0,0,0.3)', minWidth: 0 }}
                    autoFocus
                    onKeyDown={(e) => {
                      if (e.key === 'Enter') {
                        if (editName.trim()) {
                          onRenameSession(s.session_id, editName.trim());
                        }
                        setEditingSessionId(null);
                      } else if (e.key === 'Escape') {
                        setEditingSessionId(null);
                      }
                    }}
                  />
                  <button 
                    onClick={(e) => handleSaveEdit(s, e)}
                    style={{ background: 'none', border: 'none', padding: '2px', cursor: 'pointer', color: 'var(--color-success)', display: 'flex', alignItems: 'center' }}
                  >
                    <Check size={14} />
                  </button>
                  <button 
                    onClick={handleCancelEdit}
                    style={{ background: 'none', border: 'none', padding: '2px', cursor: 'pointer', color: 'var(--text-muted)', display: 'flex', alignItems: 'center' }}
                  >
                    <X size={14} />
                  </button>
                </div>
              ) : (
                <div className="session-item-row">
                  {/* Title — flexes to fill, ellipsis on overflow. The
                      provider tag was previously shown here as a colored
                      badge; that signal lives in the bottom StatusBar
                      ("Core: codex") so the row stays uncluttered. */}
                  <span
                    className="session-item-name"
                    title={s.name || s.session_id}
                  >
                    {s.name || `${s.session_id.substring(0, 8)}...`}
                  </span>

                  {/* Timestamp — always visible, fades out on hover so
                      the action cluster can claim its slot without
                      shifting the row's geometry. */}
                  <span className="session-item-date">
                    {formatRelativeTime(s.updated_at)}
                  </span>

                  {/* Action cluster — CSS hover-reveal. Rename + Delete
                      each stop event propagation so clicks don't also
                      select the row. Trace import/export was removed from
                      this high-frequency history surface to keep it focused
                      on session navigation. */}
                  <div
                    className="session-item-actions"
                    onClick={(e) => e.stopPropagation()}
                  >
                    <button
                      onClick={(e) => handleStartEdit(s, e)}
                      title="Rename"
                      className="session-item-action-btn"
                    >
                      <Edit2 size={13} />
                    </button>
                    <button
                      onClick={(e) => { e.stopPropagation(); onDeleteSession(s.session_id); }}
                      title="Delete Session"
                      className="session-item-action-btn session-item-action-danger"
                    >
                      <Trash2 size={13} />
                    </button>
                  </div>
                </div>
              )}
            </div>
          );
        })}
      </div>

      {/* SESSION PEERS section removed — peer toggles are session-level
          config, not workspace-list navigation, and they cluttered the
          panel. Future home is the session settings flow (via slash
          commands or per-session menu). The Settings modal still
          surfaces per-provider config. */}
    </div>
  );
};
