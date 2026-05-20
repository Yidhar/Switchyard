import React from 'react';
import { Plus, Settings, Trash2, Download, Edit2, Check, X } from 'lucide-react';
import type { Session, SwitchyardConfig } from '../types';

interface SidebarProps {
  sessions: Session[];
  selectedSession: Session | null;
  newSessionProvider: string;
  setNewSessionProvider: (provider: string) => void;
  config: SwitchyardConfig | null;
  onCreateSession: () => void;
  onSelectSession: (session: Session) => void;
  onTogglePeer: (peerName: string) => void;
  onOpenSettings: () => void;
  onDeleteSession: (sessionId: string) => void;
  onRenameSession: (sessionId: string, newName: string) => void;
  onExportSessionTrace: (sessionId: string) => void;
  onImportSessionTrace: (traceJson: string) => void;
}

export const Sidebar: React.FC<SidebarProps> = ({
  sessions,
  selectedSession,
  newSessionProvider,
  setNewSessionProvider,
  config,
  onCreateSession,
  onSelectSession,
  onTogglePeer,
  onOpenSettings,
  onDeleteSession,
  onRenameSession,
  onExportSessionTrace,
  onImportSessionTrace,
}) => {
  const providers = config ? Object.keys(config.providers || {}) : [];
  const defaults = ['codex', 'claude', 'gemini'];
  const allProviders = Array.from(new Set([...providers, ...defaults]));

  // Get available peers (all registered providers except current active core)
  const availablePeers = selectedSession
    ? allProviders.filter((p) => p !== selectedSession.active_core)
    : [];

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
      <div className="sidebar-header">
        <div className="logo-icon">S</div>
        <div className="logo-text">Switchyard</div>
      </div>

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
          <div style={{ display: 'flex', gap: '8px', width: '100%' }}>
            <button 
              className="btn-new-session" 
              onClick={onCreateSession} 
              style={{ flex: 1, height: '38px', display: 'flex', alignItems: 'center', justifyContent: 'center', gap: '6px' }}
            >
              <Plus size={16} />
              New
            </button>
            <button 
              className="btn-new-session" 
              onClick={() => {
                const input = document.createElement('input');
                input.type = 'file';
                input.accept = '.json';
                input.onchange = (e) => {
                  const file = (e.target as HTMLInputElement).files?.[0];
                  if (!file) return;
                  const reader = new FileReader();
                  reader.onload = (ev) => {
                    const text = ev.target?.result as string;
                    onImportSessionTrace(text);
                  };
                  reader.readAsText(file);
                };
                input.click();
              }}
              style={{ flex: 1, height: '38px', display: 'flex', alignItems: 'center', justifyContent: 'center', gap: '6px', background: 'rgba(255, 255, 255, 0.03)', border: '1px solid var(--border-muted)', color: 'var(--text-secondary)' }}
            >
              Import
            </button>
          </div>
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
              style={{ position: 'relative', paddingRight: isSelected ? '72px' : '12px' }}
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
                <>
                  <div className="session-item-title">
                    <span 
                      style={{ textOverflow: 'ellipsis', overflow: 'hidden', whiteSpace: 'nowrap', fontWeight: 500, fontSize: '13px', flex: 1, minWidth: 0 }}
                      title={s.name || s.session_id}
                    >
                      {s.name || `${s.session_id.substring(0, 8)}...`}
                    </span>
                    <span className={`session-item-badge badge-${s.active_core}`} style={{ flexShrink: 0 }}>
                      {s.active_core}
                    </span>
                  </div>
                  <div className="session-item-date">
                    {new Date(s.updated_at).toLocaleString()}
                  </div>

                  {isSelected && (
                    <div style={{ position: 'absolute', right: '8px', top: '50%', transform: 'translateY(-50%)', display: 'flex', gap: '6px' }} className="session-item-actions" onClick={(e) => e.stopPropagation()}>
                      <button 
                        onClick={(e) => handleStartEdit(s, e)}
                        title="Rename"
                        style={{ background: 'none', border: 'none', padding: '2px', cursor: 'pointer', color: 'var(--text-secondary)', display: 'flex', alignItems: 'center' }}
                      >
                        <Edit2 size={13} />
                      </button>
                      <button 
                        onClick={(e) => { e.stopPropagation(); onExportSessionTrace(s.session_id); }}
                        title="Export Trace"
                        style={{ background: 'none', border: 'none', padding: '2px', cursor: 'pointer', color: 'var(--text-secondary)', display: 'flex', alignItems: 'center' }}
                      >
                        <Download size={13} />
                      </button>
                      <button 
                        onClick={(e) => { e.stopPropagation(); onDeleteSession(s.session_id); }}
                        title="Delete Session"
                        style={{ background: 'none', border: 'none', padding: '2px', cursor: 'pointer', color: 'var(--color-error)', display: 'flex', alignItems: 'center' }}
                      >
                        <Trash2 size={13} />
                      </button>
                    </div>
                  )}
                </>
              )}
            </div>
          );
        })}
      </div>

      {selectedSession && availablePeers.length > 0 && (
        <div className="peer-configurator" style={{ padding: '12px 16px', borderTop: '1px solid var(--border-muted)', background: 'rgba(255, 255, 255, 0.01)' }}>
          <label style={{ fontSize: '10px', fontWeight: 700, color: 'var(--text-muted)', textTransform: 'uppercase', letterSpacing: '0.5px', display: 'block', marginBottom: '8px' }}>
            Session Peers
          </label>
          <div style={{ display: 'flex', flexDirection: 'column', gap: '6px' }}>
            {availablePeers.map((peer) => {
              const isEnabled = selectedSession.enabled_peers.includes(peer);
              return (
                <div 
                   key={peer} 
                   onClick={() => onTogglePeer(peer)}
                   style={{
                     display: 'flex',
                     alignItems: 'center',
                     justifyContent: 'space-between',
                     padding: '6px 10px',
                     borderRadius: '4px',
                     background: isEnabled ? 'rgba(59, 130, 246, 0.08)' : 'rgba(255, 255, 255, 0.02)',
                     border: isEnabled ? '1px solid rgba(59, 130, 246, 0.3)' : '1px solid transparent',
                     cursor: 'pointer',
                     transition: 'all 0.2s ease',
                   }}
                   className="peer-switch-item"
                >
                  <span style={{ fontSize: '12px', color: isEnabled ? 'var(--text-primary)' : 'var(--text-secondary)', textTransform: 'capitalize' }}>
                    {peer}
                  </span>
                  <div 
                    style={{
                      width: '28px',
                      height: '16px',
                      borderRadius: '8px',
                      background: isEnabled ? 'var(--color-primary)' : 'rgba(255, 255, 255, 0.15)',
                      position: 'relative',
                      transition: 'background-color 0.2s ease',
                    }}
                  >
                    <div 
                      style={{
                        width: '12px',
                        height: '12px',
                        borderRadius: '50%',
                        background: '#fff',
                        position: 'absolute',
                        top: '2px',
                        left: isEnabled ? '14px' : '2px',
                        transition: 'left 0.2s cubic-bezier(0.4, 0, 0.2, 1)',
                      }}
                    />
                  </div>
                </div>
              );
            })}
          </div>
        </div>
      )}

      <div className="sidebar-footer">
        <button className="btn-settings" onClick={onOpenSettings}>
          <Settings size={16} />
          Settings
        </button>
        <div style={{ fontSize: '11px', color: 'var(--text-muted)' }}>
          v0.1.0-tauri
        </div>
      </div>
    </div>
  );
};
