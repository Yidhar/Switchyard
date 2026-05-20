import React from 'react';
import { X, Plus, Trash } from 'lucide-react';
import type { SwitchyardConfig, ProviderConfig } from '../types';

interface SettingsModalProps {
  config: SwitchyardConfig;
  settingsTab: string;
  setSettingsTab: (tab: string) => void;
  onClose: () => void;
  onSave: () => void;
  onFieldChange: (section: keyof SwitchyardConfig, field: string, value: any) => void;
  onProviderFieldChange: (providerName: string, field: keyof ProviderConfig, value: any) => void;
  onAddCustomProvider: () => void;
  onAddEnvVar: (providerName: string) => void;
  onRemoveEnvVar: (providerName: string, key: string) => void;
  onDeleteProvider: (providerName: string) => void;
}

export const SettingsModal: React.FC<SettingsModalProps> = ({
  config,
  settingsTab,
  setSettingsTab,
  onClose,
  onSave,
  onFieldChange,
  onProviderFieldChange,
  onAddCustomProvider,
  onAddEnvVar,
  onRemoveEnvVar,
  onDeleteProvider,
}) => {
  return (
    <div className="settings-overlay">
      <div className="settings-modal glass-panel">
        <div className="settings-modal-header">
          <h2>Switchyard System Configurations</h2>
          <button className="btn-close" onClick={onClose}>
            <X size={20} />
          </button>
        </div>

        <div className="settings-modal-body">
          <div className="settings-tabs" style={{ display: 'flex', flexDirection: 'column', height: '100%', justifyContent: 'space-between' }}>
            <div style={{ display: 'flex', flexDirection: 'column', gap: '6px', overflowY: 'auto', flex: 1 }}>
              <button 
                className={`settings-tab-btn ${settingsTab === 'general' ? 'active' : ''}`}
                onClick={() => setSettingsTab('general')}
              >
                General Core
              </button>
              
              {/* Dynamic tabs for configured providers */}
              {Object.keys(config.providers || {}).map((pName) => (
                <button 
                  key={pName}
                  className={`settings-tab-btn ${settingsTab === pName ? 'active' : ''}`}
                  onClick={() => setSettingsTab(pName)}
                  style={{ textTransform: 'capitalize' }}
                >
                  {pName}
                </button>
              ))}

              <button 
                className={`settings-tab-btn ${settingsTab === 'store' ? 'active' : ''}`}
                onClick={() => setSettingsTab('store')}
              >
                Database Store
              </button>
            </div>

            {/* Add Custom Provider Button */}
            <div style={{ padding: '8px', borderTop: '1px solid var(--border-muted)' }}>
              <button 
                className="btn-add-row" 
                onClick={onAddCustomProvider} 
                style={{ width: '100%', display: 'flex', justifyContent: 'center', gap: '6px', padding: '10px' }}
              >
                <Plus size={14} />
                Add Provider
              </button>
            </div>
          </div>

          <div className="settings-tab-content">
            {settingsTab === 'general' && (
              <>
                <div className="settings-form-group">
                  <label>Default Core Provider</label>
                  <select 
                    className="settings-select"
                    value={config.core.default_provider}
                    onChange={(e) => onFieldChange('core', 'default_provider', e.target.value)}
                  >
                    {Object.keys(config.providers || {}).map((pName) => (
                      <option key={pName} value={pName}>{pName}</option>
                    ))}
                  </select>
                </div>

                <div className="settings-form-group">
                  <label>Default Peers</label>
                  <div style={{ display: 'flex', flexDirection: 'column', gap: '8px', marginTop: '4px' }}>
                    {Object.keys(config.providers || {}).map((peer) => (
                      <label key={peer} style={{ display: 'flex', alignItems: 'center', gap: '8px', textTransform: 'none', fontSize: '13px' }}>
                        <input 
                          type="checkbox"
                          checked={config.core.default_peers.includes(peer)}
                          onChange={(e) => {
                            let list = [...config.core.default_peers];
                            if (e.target.checked) {
                              list.push(peer);
                            } else {
                              list = list.filter((p) => p !== peer);
                            }
                            onFieldChange('core', 'default_peers', list);
                          }}
                        />
                        {peer}
                      </label>
                    ))}
                  </div>
                </div>
              </>
            )}

            {Object.keys(config.providers || {}).includes(settingsTab) && (() => {
              const pName = settingsTab;
              const prov = config.providers[pName] || { command: '', args: [], env: {}, timeout_secs: 900, backend: null };
              return (
                <>
                  <div className="settings-form-group">
                    <label>Backend Type</label>
                    <select 
                      className="settings-select"
                      value={prov.backend || ''}
                      onChange={(e) => onProviderFieldChange(pName, 'backend', e.target.value || null)}
                    >
                      <option value="codex">Codex Factory</option>
                      <option value="claude">Claude Factory</option>
                      <option value="gemini">Gemini Factory</option>
                    </select>
                  </div>

                  <div className="settings-form-group">
                    <label>Subprocess CLI Command</label>
                    <input 
                      type="text" 
                      className="settings-input settings-input-mono"
                      value={prov.command}
                      onChange={(e) => onProviderFieldChange(pName, 'command', e.target.value)}
                    />
                  </div>

                  <div className="settings-form-group">
                    <label>CLI Execution Arguments (comma separated)</label>
                    <input 
                      type="text" 
                      className="settings-input settings-input-mono"
                      value={prov.args.join(', ')}
                      onChange={(e) => {
                        const args = e.target.value.split(',').map((s) => s.trim()).filter((s) => s.length > 0);
                        onProviderFieldChange(pName, 'args', args);
                      }}
                    />
                  </div>

                  <div className="settings-form-group">
                    <label>Execution Timeout (seconds)</label>
                    <input 
                      type="number" 
                      className="settings-input"
                      value={prov.timeout_secs}
                      onChange={(e) => onProviderFieldChange(pName, 'timeout_secs', parseInt(e.target.value) || 900)}
                    />
                  </div>

                  <div className="settings-form-group">
                    <label style={{ display: 'flex', justifyContent: 'space-between', alignItems: 'center' }}>
                      <span>Environment Variables (API Keys, etc.)</span>
                      <button className="btn-add-row" onClick={() => onAddEnvVar(pName)} style={{ padding: '2px 8px' }}>
                        Add Key
                      </button>
                    </label>
                    <div style={{ maxHeight: '160px', overflowY: 'auto', marginTop: '6px' }}>
                      {Object.entries(prov.env || {}).map(([key, val]) => (
                        <div key={key} className="env-editor-row">
                          <input type="text" className="settings-input settings-input-mono" value={key} readOnly />
                          <input 
                            type="text" 
                            className="settings-input" 
                            value={val} 
                            onChange={(e) => {
                              const envCopy = { ...prov.env };
                              envCopy[key] = e.target.value;
                              onProviderFieldChange(pName, 'env', envCopy);
                            }}
                          />
                          <button className="btn-remove-row" onClick={() => onRemoveEnvVar(pName, key)}>
                            <X size={14} />
                          </button>
                        </div>
                      ))}
                    </div>
                  </div>

                  <div style={{ marginTop: '24px', display: 'flex', justifyContent: 'flex-end', borderTop: '1px solid var(--border-muted)', paddingTop: '16px' }}>
                    <button 
                      className="btn-remove-row" 
                      style={{ background: 'var(--color-error)', color: 'white', display: 'flex', alignItems: 'center', gap: '6px', padding: '8px 16px', fontSize: '13px' }}
                      onClick={() => {
                        if (confirm(`Are you sure you want to delete provider "${pName}"?`)) {
                          onDeleteProvider(pName);
                        }
                      }}
                    >
                      <Trash size={14} />
                      Delete Provider
                    </button>
                  </div>
                </>
              );
            })()}

            {settingsTab === 'store' && (
              <>
                <div className="settings-form-group">
                  <label>Store Engine Backend</label>
                  <select 
                    className="settings-select"
                    value={config.store.backend}
                    onChange={(e) => onFieldChange('store', 'backend', e.target.value)}
                  >
                    <option value="jsonl">JSONL (Plain Text Stream Files)</option>
                    <option value="sqlite">SQLite Database (Single Persistent File)</option>
                  </select>
                </div>

                <div className="settings-form-group">
                  <label>Database Storage Path</label>
                  <input 
                    type="text" 
                    className="settings-input settings-input-mono"
                    value={config.store.path}
                    onChange={(e) => onFieldChange('store', 'path', e.target.value)}
                  />
                </div>
              </>
            )}
          </div>
        </div>

        <div className="settings-modal-footer">
          <button className="btn-secondary" onClick={onClose}>
            Cancel
          </button>
          <button className="btn-primary" onClick={onSave}>
            Save & Apply Config
          </button>
        </div>
      </div>
    </div>
  );
};

export default SettingsModal;
