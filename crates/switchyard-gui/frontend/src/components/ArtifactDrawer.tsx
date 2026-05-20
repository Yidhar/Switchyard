import React, { useState, useEffect } from 'react';
import { invoke } from '@tauri-apps/api/core';
import { FileText, ChevronDown, ChevronUp, Clock, RefreshCw, Layers } from 'lucide-react';

interface ArtifactItem {
  name: string;
  path: string;
  size: number;
  is_dir: boolean;
  modified: string | null;
}

interface ArtifactDrawerProps {
  isOpen: boolean;
  onToggle: () => void;
}

export const ArtifactDrawer: React.FC<ArtifactDrawerProps> = ({ isOpen, onToggle }) => {
  const [artifacts, setArtifacts] = useState<ArtifactItem[]>([]);
  const [selectedName, setSelectedName] = useState<string | null>(null);
  const [content, setContent] = useState<string>('');
  const [loading, setLoading] = useState(false);
  const [listLoading, setListLoading] = useState(false);

  const loadList = async () => {
    setListLoading(true);
    try {
      const list = await invoke<ArtifactItem[]>('list_artifacts');
      setArtifacts(list);
      if (list.length > 0 && !selectedName) {
        // Auto select the first artifact (usually the newest plan/task)
        setSelectedName(list[0].name);
      }
    } catch (e) {
      console.error('Failed to list artifacts:', e);
    } finally {
      setListLoading(false);
    }
  };

  const loadContent = async (name: string) => {
    setLoading(true);
    try {
      const text = await invoke<string>('read_artifact', { name });
      setContent(text);
    } catch (e) {
      setContent(`Error loading artifact: ${e}`);
    } finally {
      setLoading(false);
    }
  };

  useEffect(() => {
    loadList();
  }, []);

  useEffect(() => {
    if (selectedName) {
      loadContent(selectedName);
    }
  }, [selectedName]);

  const formatSize = (bytes: number) => {
    if (bytes < 1024) return `${bytes} B`;
    if (bytes < 1024 * 1024) return `${(bytes / 1024).toFixed(1)} KB`;
    return `${(bytes / (1024 * 1024)).toFixed(1)} MB`;
  };

  const formatDate = (isoStr: string | null) => {
    if (!isoStr) return '';
    try {
      const d = new Date(isoStr);
      return d.toLocaleTimeString([], { hour: '2-digit', minute: '2-digit', second: '2-digit' });
    } catch {
      return '';
    }
  };

  // Helper to parse markdown with basic support for Headers, List Items, GitHub Alerts, and Code Blocks
  const renderArtifactMarkdown = (mdText: string) => {
    if (!mdText) return <p style={{ color: 'var(--text-muted)' }}>Empty content.</p>;

    // Split text into paragraphs/blocks
    const lines = mdText.split('\n');
    const elements: React.ReactNode[] = [];
    let inCodeBlock = false;
    let codeLanguage = '';
    let codeLines: string[] = [];
    let currentAlert: { type: string; lines: string[] } | null = null;

    const flushCodeBlock = (key: string | number) => {
      if (codeLines.length > 0) {
        elements.push(
          <div key={key} className="code-block-container" style={{ margin: '12px 0', position: 'relative' }}>
            {codeLanguage && (
              <div style={{
                position: 'absolute',
                top: '8px',
                right: '12px',
                fontSize: '11px',
                color: 'var(--text-muted)',
                textTransform: 'uppercase',
                fontWeight: 'bold',
                letterSpacing: '0.5px'
              }}>
                {codeLanguage}
              </div>
            )}
            <pre style={{ margin: 0, background: 'rgba(0, 0, 0, 0.4)', padding: '12px', borderRadius: '4px', overflowX: 'auto' }}>
              <code style={{ fontFamily: 'monospace', color: '#38bdf8' }}>{codeLines.join('\n')}</code>
            </pre>
          </div>
        );
        codeLines = [];
      }
      inCodeBlock = false;
    };

    const flushAlert = (key: string | number) => {
      if (currentAlert) {
        const type = currentAlert.type.toUpperCase();
        let borderClr = 'var(--border-muted)';
        let bgClr = 'rgba(255, 255, 255, 0.02)';
        let textClr = 'var(--text-primary)';
        
        if (type === 'IMPORTANT' || type === 'WARNING') {
          borderClr = 'var(--color-secondary)';
          bgClr = 'rgba(6, 182, 212, 0.05)';
        } else if (type === 'TIP' || type === 'NOTE') {
          borderClr = 'var(--color-primary)';
          bgClr = 'rgba(59, 130, 246, 0.05)';
        } else if (type === 'CAUTION') {
          borderClr = '#f43f5e';
          bgClr = 'rgba(244, 63, 94, 0.05)';
        }

        elements.push(
          <div key={key} style={{
            padding: '12px 16px',
            margin: '12px 0',
            borderLeft: `4px solid ${borderClr}`,
            backgroundColor: bgClr,
            borderRadius: '0 4px 4px 0',
          }}>
            <div style={{ fontWeight: 'bold', fontSize: '12px', color: borderClr, marginBottom: '6px', textTransform: 'uppercase' }}>
              {type}
            </div>
            <div style={{ fontSize: '13px', color: textClr, whiteSpace: 'pre-wrap' }}>
              {currentAlert.lines.join('\n')}
            </div>
          </div>
        );
        currentAlert = null;
      }
    };

    for (let i = 0; i < lines.length; i++) {
      const line = lines[i];

      // Handle Code Blocks
      if (line.trim().startsWith('```')) {
        if (inCodeBlock) {
          flushCodeBlock(`code-${i}`);
        } else {
          inCodeBlock = true;
          codeLanguage = line.trim().slice(3).trim();
        }
        continue;
      }

      if (inCodeBlock) {
        codeLines.push(line);
        continue;
      }

      // Handle GitHub Alerts: e.g. > [!NOTE] or > [!IMPORTANT]
      const alertMatch = line.trim().match(/^>\s*\[!(NOTE|TIP|IMPORTANT|WARNING|CAUTION)\]/i);
      if (alertMatch) {
        flushAlert(`alert-flush-${i}`);
        currentAlert = {
          type: alertMatch[1],
          lines: []
        };
        continue;
      }

      if (currentAlert) {
        if (line.trim().startsWith('>')) {
          // Add remaining alert body lines
          currentAlert.lines.push(line.trim().slice(1).trim());
          continue;
        } else {
          flushAlert(`alert-flush-${i}`);
        }
      }

      // Headers
      if (line.startsWith('# ')) {
        elements.push(<h1 key={i} style={{ fontSize: '20px', fontWeight: 'bold', margin: '18px 0 10px 0', color: 'var(--text-primary)' }}>{line.slice(2)}</h1>);
      } else if (line.startsWith('## ')) {
        elements.push(<h2 key={i} style={{ fontSize: '16px', fontWeight: 'bold', margin: '16px 0 8px 0', borderBottom: '1px solid var(--border-muted)', paddingBottom: '4px', color: 'var(--color-primary)' }}>{line.slice(3)}</h2>);
      } else if (line.startsWith('### ')) {
        elements.push(<h3 key={i} style={{ fontSize: '14px', fontWeight: 'bold', margin: '12px 0 6px 0', color: 'var(--color-secondary)' }}>{line.slice(4)}</h3>);
      } else if (line.startsWith('#### ')) {
        elements.push(<h4 key={i} style={{ fontSize: '12px', fontWeight: 'bold', margin: '10px 0 4px 0', color: 'var(--text-primary)' }}>{line.slice(5)}</h4>);
      }
      // List items
      else if (line.trim().startsWith('- ') || line.trim().startsWith('* ')) {
        const itemText = line.trim().slice(2);
        // Simple support for task list checkbox representation
        let isTask = false;
        let checked = false;
        let label = itemText;
        if (itemText.startsWith('[ ] ')) {
          isTask = true;
          checked = false;
          label = itemText.slice(4);
        } else if (itemText.startsWith('[x]') || itemText.startsWith('[X]')) {
          isTask = true;
          checked = true;
          label = itemText.slice(4);
        } else if (itemText.startsWith('[/]')) {
          isTask = true;
          // In progress helper icon or style
          isTask = true;
          label = itemText.slice(4);
        }

        elements.push(
          <div key={i} style={{ display: 'flex', alignItems: 'flex-start', gap: '8px', paddingLeft: '12px', margin: '4px 0', fontSize: '13px' }}>
            {isTask ? (
              itemText.startsWith('[/]') ? (
                <span style={{ color: 'var(--color-secondary)', fontSize: '12px', fontWeight: 'bold', marginTop: '2px' }}>🔄</span>
              ) : (
                <input type="checkbox" checked={checked} readOnly style={{ marginTop: '3px' }} />
              )
            ) : (
              <span style={{ color: 'var(--color-primary)', marginTop: '2px' }}>•</span>
            )}
            <span style={{ textDecoration: checked ? 'line-through' : 'none', color: checked ? 'var(--text-muted)' : 'var(--text-primary)' }}>{label}</span>
          </div>
        );
      }
      // Empty line
      else if (line.trim() === '') {
        // Skip or push small space
        elements.push(<div key={i} style={{ height: '8px' }}></div>);
      }
      // Standard Paragraph
      else {
        // Replace bold and inline code
        const inlineParts = line.split(/(`[^`\n]+`|\*\*[^*]+\*\*)/g);
        elements.push(
          <p key={i} style={{ fontSize: '13px', lineHeight: '1.6', margin: '6px 0', color: 'var(--text-primary)' }}>
            {inlineParts.map((part, idx) => {
              if (part.startsWith('`') && part.endsWith('`')) {
                return <code key={idx} style={{ background: 'rgba(255,255,255,0.05)', padding: '2px 4px', borderRadius: '4px', fontFamily: 'monospace', color: 'var(--color-secondary)' }}>{part.slice(1, -1)}</code>;
              }
              if (part.startsWith('**') && part.endsWith('**')) {
                return <strong key={idx} style={{ color: 'var(--text-primary)' }}>{part.slice(2, -2)}</strong>;
              }
              return part;
            })}
          </p>
        );
      }
    }

    // Flush any leftover open blocks
    flushCodeBlock('code-final');
    flushAlert('alert-final');

    return elements;
  };

  return (
    <div 
      className={`glass-panel ${isOpen ? 'open' : ''}`}
      style={{
        position: 'fixed',
        bottom: 0,
        left: isOpen ? '240px' : '0', // Align with sidebar
        right: 0,
        height: isOpen ? '360px' : '36px',
        borderTop: '1px solid var(--border-muted)',
        background: 'var(--bg-glass)',
        backdropFilter: 'blur(12px)',
        display: 'flex',
        flexDirection: 'column',
        zIndex: 50,
        transition: 'all 0.3s cubic-bezier(0.4, 0, 0.2, 1)',
        overflow: 'hidden',
        boxShadow: '0 -4px 10px rgba(0,0,0,0.3)',
      }}
    >
      {/* Header Bar */}
      <div 
        onClick={onToggle}
        style={{
          height: '36px',
          padding: '0 16px',
          display: 'flex',
          alignItems: 'center',
          justifyContent: 'space-between',
          cursor: 'pointer',
          background: 'rgba(255, 255, 255, 0.02)',
          borderBottom: isOpen ? '1px solid var(--border-muted)' : 'none',
          userSelect: 'none'
        }}
      >
        <div style={{ display: 'flex', alignItems: 'center', gap: '8px' }}>
          <Layers size={14} style={{ color: 'var(--color-primary)' }} />
          <span style={{ fontSize: '12px', fontWeight: '600', letterSpacing: '0.5px' }}>WORKSPACE ARTIFACT EXPLORER</span>
          {artifacts.length > 0 && (
            <span style={{ fontSize: '10px', padding: '1px 6px', background: 'rgba(59, 130, 246, 0.1)', color: 'var(--color-primary)', borderRadius: '3px' }}>
              {artifacts.length} File{artifacts.length > 1 ? 's' : ''}
            </span>
          )}
        </div>
        <div style={{ display: 'flex', alignItems: 'center', gap: '12px' }}>
          {isOpen && (
            <button 
              onClick={(e) => {
                e.stopPropagation();
                loadList();
              }}
              style={{
                background: 'transparent',
                border: 'none',
                color: 'var(--text-secondary)',
                cursor: 'pointer',
                display: 'flex',
                alignItems: 'center',
                padding: '4px',
                borderRadius: '3px'
              }}
              title="Refresh Files"
            >
              <RefreshCw size={12} className={listLoading ? 'spin' : ''} />
            </button>
          )}
          {isOpen ? <ChevronDown size={14} /> : <ChevronUp size={14} />}
        </div>
      </div>

      {/* Drawer Content */}
      {isOpen && (
        <div style={{ flex: 1, display: 'flex', overflow: 'hidden' }}>
          {/* Artifacts Sidebar */}
          <div style={{
            width: '220px',
            borderRight: '1px solid var(--border-muted)',
            overflowY: 'auto',
            background: 'rgba(0,0,0,0.1)',
            padding: '8px'
          }}>
            {listLoading && artifacts.length === 0 ? (
              <div style={{ display: 'flex', justifyContent: 'center', padding: '20px' }}>
                <RefreshCw size={16} className="spin" style={{ color: 'var(--text-muted)' }} />
              </div>
            ) : artifacts.length === 0 ? (
              <div style={{ padding: '12px', fontSize: '11px', color: 'var(--text-muted)', textAlign: 'center' }}>
                No workspace artifacts found.
              </div>
            ) : (
              <div style={{ display: 'flex', flexDirection: 'column', gap: '4px' }}>
                {artifacts.map((art) => {
                  const isActive = selectedName === art.name;
                  return (
                    <button
                       key={art.name}
                      onClick={() => setSelectedName(art.name)}
                      style={{
                        display: 'flex',
                        flexDirection: 'column',
                        alignItems: 'flex-start',
                        width: '100%',
                        padding: '8px 10px',
                        border: 'none',
                        borderRadius: '3px',
                        background: isActive ? 'rgba(59, 130, 246, 0.12)' : 'transparent',
                        borderLeft: isActive ? '2px solid var(--color-primary)' : '2px solid transparent',
                        color: isActive ? 'var(--color-primary)' : 'var(--text-secondary)',
                        textAlign: 'left',
                        cursor: 'pointer',
                        transition: 'all 0.2s'
                      }}
                    >
                      <div style={{ display: 'flex', alignItems: 'center', gap: '6px', width: '100%' }}>
                        <FileText size={12} style={{ flexShrink: 0 }} />
                        <span style={{ fontSize: '11px', fontWeight: isActive ? '600' : '400', overflow: 'hidden', textOverflow: 'ellipsis', whiteSpace: 'nowrap' }}>
                          {art.name}
                        </span>
                      </div>
                      <div style={{ display: 'flex', justifyContent: 'space-between', width: '100%', marginTop: '4px', fontSize: '9px', color: 'var(--text-muted)' }}>
                        <span>{formatSize(art.size)}</span>
                        <span style={{ display: 'flex', alignItems: 'center', gap: '2px' }}>
                          <Clock size={8} />
                          {formatDate(art.modified)}
                        </span>
                      </div>
                    </button>
                  );
                })}
              </div>
            )}
          </div>

          {/* Artifact Preview Area */}
          <div style={{ flex: 1, display: 'flex', flexDirection: 'column', overflow: 'hidden', padding: '16px' }}>
            {selectedName ? (
              <div style={{ flex: 1, display: 'flex', flexDirection: 'column', overflow: 'hidden' }}>
                <div style={{ display: 'flex', justifyContent: 'space-between', alignItems: 'center', borderBottom: '1px solid var(--border-muted)', paddingBottom: '8px', marginBottom: '12px' }}>
                  <h3 style={{ fontSize: '13px', fontWeight: 'bold', display: 'flex', alignItems: 'center', gap: '6px' }}>
                    <FileText size={14} style={{ color: 'var(--color-primary)' }} />
                    {selectedName}
                  </h3>
                </div>
                
                <div style={{ flex: 1, overflowY: 'auto', paddingRight: '6px' }}>
                  {loading ? (
                    <div style={{ display: 'flex', justifyContent: 'center', alignItems: 'center', height: '100%' }}>
                      <RefreshCw size={20} className="spin" style={{ color: 'var(--color-primary)' }} />
                    </div>
                  ) : (
                    renderArtifactMarkdown(content)
                  )}
                </div>
              </div>
            ) : (
              <div style={{ display: 'flex', flexDirection: 'column', justifyContent: 'center', alignItems: 'center', height: '100%', gap: '8px', color: 'var(--text-muted)' }}>
                <FileText size={28} />
                <span style={{ fontSize: '12px' }}>Select an artifact to view its content</span>
              </div>
            )}
          </div>
        </div>
      )}
    </div>
  );
};
