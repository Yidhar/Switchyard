import React, { useState } from 'react';
import { Terminal, Code, CheckCircle2, AlertTriangle, Eye, EyeOff } from 'lucide-react';

interface ToolAction {
  id?: string;
  label: string;
  tone?: 'primary' | 'danger' | 'secondary';
  title?: string;
  disabled?: boolean;
  onClick: () => void | Promise<void>;
}

interface ToolCall {
  id: string;
  name: string;
  input: any;
  status: 'pending' | 'running' | 'completed' | 'failed' | 'cancelled';
  output: any;
  actions?: ToolAction[];
}

interface ToolCardProps {
  tool: ToolCall;
}

export const ToolCard: React.FC<ToolCardProps> = ({ tool }) => {
  const [showInput, setShowInput] = useState(false);
  const [outputVisibilityOverride, setOutputVisibilityOverride] = useState<boolean | null>(null);
  const [busyAction, setBusyAction] = useState<string | null>(null);

  const getStatusColor = (status: string) => {
    switch (status) {
      case 'completed':
      case 'success':
        return 'var(--color-success)';
      case 'failed':
      case 'cancelled':
        return 'var(--color-error)';
      default:
        return 'var(--color-warning)';
    }
  };

  // Helper to check if a string contains diff markers
  const isDiffContent = (text: string) => {
    if (typeof text !== 'string') return false;
    const lines = text.split('\n');
    let hasAdd = false;
    let hasSub = false;
    for (let i = 0; i < Math.min(lines.length, 30); i++) {
      const trimmed = lines[i].trim();
      if (trimmed.startsWith('+')) hasAdd = true;
      if (trimmed.startsWith('-')) hasSub = true;
    }
    return hasAdd && hasSub;
  };

  const diffStats = (diffText: string) => {
    let additions = 0;
    let deletions = 0;
    diffText.split('\n').forEach((line) => {
      if (line.startsWith('+++') || line.startsWith('---')) return;
      if (line.startsWith('+')) additions += 1;
      if (line.startsWith('-')) deletions += 1;
    });
    return { additions, deletions };
  };

  // Custom renderer for diff contents
  const renderDiff = (diffText: string) => {
    const lines = diffText.split('\n');
    return (
      <div 
        style={{
          fontFamily: 'monospace',
          fontSize: '11px',
          background: '#08090d',
          borderRadius: '4px',
          padding: '8px 12px',
          overflowX: 'auto',
          border: '1px solid var(--border-muted)',
          display: 'flex',
          flexDirection: 'column',
          gap: '2px',
          lineHeight: '1.5',
          maxHeight: '260px',
          overflowY: 'auto',
        }}
      >
        {lines.map((line, idx) => {
          let color = 'var(--text-primary)';
          let bg = 'transparent';
          if (line.startsWith('+')) {
            color = '#10b981'; // green
            bg = 'rgba(16, 185, 129, 0.08)';
          } else if (line.startsWith('-')) {
            color = '#ef4444'; // red
            bg = 'rgba(239, 68, 68, 0.08)';
          } else if (line.startsWith('@@')) {
            color = 'var(--color-secondary)'; // cyan/indigo
            bg = 'rgba(6, 182, 212, 0.05)';
          }

          return (
            <div 
              key={idx} 
              style={{ 
                color, 
                background: bg, 
                padding: '1px 6px',
                borderRadius: '3px',
                whiteSpace: 'pre-wrap',
                wordBreak: 'break-all'
              }}
            >
              {line}
            </div>
          );
        })}
      </div>
    );
  };

  // Convert input or output to string nicely
  const formatData = (data: any) => {
    if (!data) return '';
    if (typeof data === 'string') return data.trim();
    return JSON.stringify(data, null, 2);
  };

  const inputText = formatData(tool.input);
  const outputText = formatData(tool.output);
  const hasInput = Boolean(tool.input && inputText.length > 0);
  const hasOutput = Boolean(tool.output && outputText.length > 0);
  const outputLineCount = hasOutput ? outputText.split(/\r?\n/).length : 0;
  const outputCharCount = outputText.length;
  const outputIsDiff = hasOutput && isDiffContent(outputText);
  const stats = outputIsDiff ? diffStats(outputText) : null;
  const lowerName = tool.name.toLowerCase();
  const commandLike = lowerName.includes('command') || lowerName.includes('execute') || lowerName.includes('run') || lowerName.includes('shell');
  const outputIsLarge = outputIsDiff || outputLineCount > 40 || outputCharCount > 2000 || (commandLike && outputLineCount > 12);
  const showOutput = outputVisibilityOverride ?? !outputIsLarge;
  const toggleOutput = () => setOutputVisibilityOverride(!showOutput);
  const outputSummary = outputIsDiff
    ? `Diff (+${stats?.additions ?? 0} -${stats?.deletions ?? 0}, ${outputLineCount} lines)`
    : `Output (${outputLineCount} line${outputLineCount === 1 ? '' : 's'}${outputCharCount > 2000 ? `, ${outputCharCount.toLocaleString()} chars` : ''})`;
  const hasActions = Array.isArray(tool.actions) && tool.actions.length > 0;

  const actionStyle = (tone: ToolAction['tone'] = 'secondary'): React.CSSProperties => {
    if (tone === 'primary') {
      return {
        background: 'rgba(34, 197, 94, 0.16)',
        border: '1px solid rgba(34, 197, 94, 0.45)',
        color: 'var(--color-success)',
      };
    }
    if (tone === 'danger') {
      return {
        background: 'rgba(239, 68, 68, 0.14)',
        border: '1px solid rgba(239, 68, 68, 0.42)',
        color: 'var(--color-error)',
      };
    }
    return {
      background: 'rgba(255, 255, 255, 0.04)',
      border: '1px solid var(--border-muted)',
      color: 'var(--text-secondary)',
    };
  };

  const runAction = async (action: ToolAction) => {
    const id = action.id || action.label;
    setBusyAction(id);
    try {
      await action.onClick();
    } catch (error) {
      console.error('Tool action failed:', error);
    } finally {
      setBusyAction((current) => (current === id ? null : current));
    }
  };

  // Auto detect icon
  const getIcon = () => {
    if (tool.name.toLowerCase().includes('command') || tool.name.toLowerCase().includes('run')) {
      return <Terminal size={14} style={{ color: getStatusColor(tool.status) }} />;
    }
    return <Code size={14} style={{ color: getStatusColor(tool.status) }} />;
  };

  return (
    <div 
      className="tool-card glass-panel"
      style={{
        margin: '6px 0',
        borderLeft: `3px solid ${getStatusColor(tool.status)}`,
        borderRadius: '4px',
        overflow: 'hidden',
        background: 'rgba(255, 255, 255, 0.01)',
      }}
    >
      {/* Header */}
      <div 
        style={{
          display: 'flex',
          alignItems: 'center',
          justifyContent: 'space-between',
          padding: '8px 12px',
          background: 'rgba(255, 255, 255, 0.02)',
          borderBottom: '1px solid var(--border-muted)',
        }}
      >
        <div style={{ display: 'flex', alignItems: 'center', gap: '8px', fontWeight: 'bold', fontSize: '12px' }}>
          {getIcon()}
          <span style={{ color: 'var(--text-primary)' }}>{tool.name}</span>
        </div>
        <div style={{ display: 'flex', alignItems: 'center', gap: '8px', fontSize: '11px' }}>
          {tool.status === 'completed' ? (
            <CheckCircle2 size={12} style={{ color: 'var(--color-success)' }} />
          ) : tool.status === 'failed' ? (
            <AlertTriangle size={12} style={{ color: 'var(--color-error)' }} />
          ) : null}
          <span style={{ color: getStatusColor(tool.status), textTransform: 'capitalize', fontWeight: '500' }}>
            {tool.status}
          </span>
        </div>
      </div>

      {/* Body */}
      <div style={{ padding: '10px 12px', display: 'flex', flexDirection: 'column', gap: '8px', fontSize: '11px' }}>
        
        {/* Input Toggle */}
        {hasInput && (
          <div>
            <div 
              onClick={() => setShowInput(!showInput)}
              style={{
                display: 'flex',
                alignItems: 'center',
                gap: '4px',
                color: 'var(--text-muted)',
                cursor: 'pointer',
                marginBottom: '4px',
                userSelect: 'none',
              }}
            >
              {showInput ? <EyeOff size={11} /> : <Eye size={11} />}
              <span>{showInput ? 'Hide Input' : 'Show Input / Parameters'}</span>
            </div>
            {showInput && (
              <pre 
                style={{
                  margin: 0,
                  fontSize: '11px',
                  fontFamily: 'monospace',
                  background: 'rgba(0,0,0,0.2)',
                  color: 'var(--text-muted)',
                  padding: '6px 8px',
                  borderRadius: '4px',
                  maxHeight: '150px',
                  overflowY: 'auto',
                  border: '1px solid var(--border-muted)',
                  whiteSpace: 'pre-wrap',
                  wordBreak: 'break-all',
                }}
              >
                {inputText}
              </pre>
            )}
          </div>
        )}

        {/* Output Toggle */}
        {hasOutput && (
          <div>
            <div 
              onClick={toggleOutput}
              style={{
                display: 'flex',
                alignItems: 'center',
                gap: '4px',
                color: 'var(--text-secondary)',
                cursor: 'pointer',
                marginBottom: '4px',
                userSelect: 'none',
              }}
            >
              {showOutput ? <EyeOff size={11} /> : <Eye size={11} />}
              <span>{showOutput ? `Hide ${outputSummary}` : `Show ${outputSummary}`}</span>
            </div>
            {!showOutput && outputIsLarge && (
              <div
                style={{
                  color: 'var(--text-muted)',
                  background: 'rgba(255,255,255,0.025)',
                  border: '1px dashed var(--border-muted)',
                  borderRadius: '4px',
                  padding: '6px 8px',
                  marginBottom: '4px',
                }}
              >
                已折叠大块{outputIsDiff ? '文件差异' : '工具输出'}，点击上方展开查看完整内容。
              </div>
            )}
            {showOutput && (
              outputIsDiff ? (
                renderDiff(outputText)
              ) : (
                <pre 
                  style={{
                    margin: 0,
                    fontSize: '11px',
                    fontFamily: 'monospace',
                    background: '#0c0f1d',
                    color: tool.status === 'failed' ? 'var(--color-error)' : '#38bdf8',
                    padding: '8px 10px',
                    borderRadius: '4px',
                    maxHeight: '220px',
                    overflowY: 'auto',
                    border: '1px solid rgba(255,255,255,0.05)',
                    whiteSpace: 'pre-wrap',
                    wordBreak: 'break-all',
                  }}
                >
                  {outputText}
                </pre>
              )
            )}
          </div>
        )}

        {hasActions && (
          <div
            style={{
              display: 'flex',
              justifyContent: 'flex-end',
              gap: '8px',
              paddingTop: hasInput || hasOutput ? '4px' : 0,
              borderTop: hasInput || hasOutput ? '1px solid rgba(255,255,255,0.04)' : 'none',
            }}
          >
            {tool.actions!.map((action) => {
              const id = action.id || action.label;
              const disabled = Boolean(action.disabled || busyAction);
              return (
                <button
                  key={id}
                  type="button"
                  title={action.title}
                  disabled={disabled}
                  onClick={() => void runAction(action)}
                  style={{
                    ...actionStyle(action.tone),
                    cursor: disabled ? 'not-allowed' : 'pointer',
                    opacity: disabled ? 0.55 : 1,
                    borderRadius: '6px',
                    padding: '5px 10px',
                    fontSize: '11px',
                    fontWeight: 700,
                  }}
                >
                  {busyAction === id ? 'Sending…' : action.label}
                </button>
              );
            })}
          </div>
        )}
      </div>
    </div>
  );
};
