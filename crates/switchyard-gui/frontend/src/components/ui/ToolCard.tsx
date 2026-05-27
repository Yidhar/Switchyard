import React, { useMemo, useState } from 'react';
import { Terminal, Code, CheckCircle2, AlertTriangle, Eye, EyeOff } from 'lucide-react';
import { VirtualizedDiffBlock } from './VirtualizedDiffBlock';

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

const TOOL_CARD_DATA_MAX_CHARS = 160_000;
const TOOL_CARD_MAX_OBJECT_KEYS = 48;
const TOOL_CARD_MAX_ARRAY_ITEMS = 80;
const TOOL_CARD_STRUCTURED_SCAN_MAX_DEPTH = 3;
const TOOL_CARD_STRUCTURED_SCAN_MAX_NODES = 96;
const TOOL_CARD_STRUCTURED_SCAN_MAX_KEYS = 36;
const TOOL_CARD_STRUCTURED_SCAN_MAX_ITEMS = 36;
const TOOL_CARD_LARGE_LINE_THRESHOLD = 40;
const TOOL_CARD_LARGE_CHAR_THRESHOLD = 2_000;
const TOOL_CARD_COMMAND_LARGE_LINE_THRESHOLD = 12;
const LIKELY_OUTPUT_TEXT_KEYS = new Set([
  'output',
  'stdout',
  'stderr',
  'diff',
  'patch',
  'content',
  'text',
  'message',
  'result',
  'data',
]);

interface StructuredTextScan {
  charCount: number;
  lineCount: number;
  stringCount: number;
  maxStringChars: number;
  diffText: string | null;
  truncated: boolean;
  largeShape: boolean;
  visited: WeakSet<object>;
  nodes: number;
}

interface OutputSummaryMeta {
  hasOutput: boolean;
  lineCount: number | null;
  charCount: number | null;
  isDiff: boolean;
  stats: { additions: number; deletions: number } | null;
  isLarge: boolean;
  approximate: boolean;
  shapeLabel: string | null;
  diffText: string | null;
}

function truncateMiddle(text: string, maxChars = TOOL_CARD_DATA_MAX_CHARS): string {
  const value = text.trim();
  if (value.length <= maxChars) return value;
  const headChars = Math.floor(maxChars * 0.72);
  const tailChars = Math.floor(maxChars * 0.18);
  return `${value.slice(0, headChars)}\n… 已截断 ${Math.max(0, value.length - headChars - tailChars).toLocaleString()} 个字符 …\n${value.slice(-tailChars)}`;
}

function simplifyData(data: any, depth = 0): any {
  if (data === undefined || data === null) return data;
  if (typeof data === 'string') return truncateMiddle(data);
  if (typeof data !== 'object') return data;
  if (depth >= 5) return '[Object]';
  if (Array.isArray(data)) {
    const items = data.slice(0, TOOL_CARD_MAX_ARRAY_ITEMS).map((item) => simplifyData(item, depth + 1));
    if (data.length > TOOL_CARD_MAX_ARRAY_ITEMS) {
      items.push(`… ${data.length - TOOL_CARD_MAX_ARRAY_ITEMS} more items …`);
    }
    return items;
  }

  const entries = Object.entries(data);
  const out: Record<string, any> = {};
  for (const [key, value] of entries.slice(0, TOOL_CARD_MAX_OBJECT_KEYS)) {
    out[key] = simplifyData(value, depth + 1);
  }
  if (entries.length > TOOL_CARD_MAX_OBJECT_KEYS) {
    out['…'] = `${entries.length - TOOL_CARD_MAX_OBJECT_KEYS} more keys`;
  }
  return out;
}

function formatData(data: any): string {
  if (data === undefined || data === null) return '';
  if (typeof data === 'string') return truncateMiddle(data);
  try {
    return truncateMiddle(JSON.stringify(simplifyData(data), null, 2));
  } catch {
    return truncateMiddle(String(data));
  }
}

function hasData(data: any): boolean {
  if (data === undefined || data === null) return false;
  if (typeof data === 'string') return data.trim().length > 0;
  if (Array.isArray(data)) return data.length > 0;
  if (typeof data === 'object') return Object.keys(data).length > 0;
  return true;
}

function countLines(text: string): number {
  if (!text) return 0;
  let count = 1;
  for (let i = 0; i < text.length; i += 1) {
    if (text.charCodeAt(i) === 10) count += 1;
  }
  return count;
}

function isDiffContent(text: string): boolean {
  if (typeof text !== 'string') return false;
  let hasAdd = false;
  let hasSub = false;
  let lineStart = 0;
  let inspected = 0;
  for (let i = 0; i <= text.length && inspected < 80; i += 1) {
    if (i !== text.length && text.charCodeAt(i) !== 10) continue;
    const line = text.slice(lineStart, i).trim();
    if (line.startsWith('diff --git') || line.startsWith('@@')) return true;
    if (line.startsWith('+')) hasAdd = true;
    if (line.startsWith('-')) hasSub = true;
    if (hasAdd && hasSub) return true;
    lineStart = i + 1;
    inspected += 1;
  }
  return false;
}

function diffStats(diffText: string): { additions: number; deletions: number } {
  let additions = 0;
  let deletions = 0;
  let lineStart = 0;
  for (let i = 0; i <= diffText.length; i += 1) {
    if (i !== diffText.length && diffText.charCodeAt(i) !== 10) continue;
    const line = diffText.slice(lineStart, i);
    if (!(line.startsWith('+++') || line.startsWith('---'))) {
      if (line.startsWith('+')) additions += 1;
      if (line.startsWith('-')) deletions += 1;
    }
    lineStart = i + 1;
  }
  return { additions, deletions };
}

function hasMetricOver(value: number | null, threshold: number): boolean {
  return value !== null && value > threshold;
}

function shapeLabelForTopLevel(count: number, kind: 'array' | 'object'): string {
  const unit = kind === 'array' ? 'item' : 'key';
  return `${count.toLocaleString()} ${unit}${count === 1 ? '' : 's'}`;
}

function scanStructuredTextValue(value: any, state: StructuredTextScan, depth = 0): void {
  if (state.nodes >= TOOL_CARD_STRUCTURED_SCAN_MAX_NODES) {
    state.truncated = true;
    return;
  }
  if (value === undefined || value === null) return;

  if (typeof value === 'string') {
    const text = value.trim();
    if (!text) return;
    state.stringCount += 1;
    state.charCount += text.length;
    state.lineCount += countLines(text);
    state.maxStringChars = Math.max(state.maxStringChars, text.length);
    if (state.diffText === null && isDiffContent(text)) {
      state.diffText = text;
    }
    return;
  }

  if (typeof value !== 'object' || depth >= TOOL_CARD_STRUCTURED_SCAN_MAX_DEPTH) {
    return;
  }

  if (state.visited.has(value)) {
    return;
  }
  state.visited.add(value);
  state.nodes += 1;

  if (Array.isArray(value)) {
    if (value.length > TOOL_CARD_STRUCTURED_SCAN_MAX_ITEMS) {
      state.largeShape = true;
      state.truncated = true;
    }
    for (let index = 0; index < Math.min(value.length, TOOL_CARD_STRUCTURED_SCAN_MAX_ITEMS); index += 1) {
      scanStructuredTextValue(value[index], state, depth + 1);
      if (state.nodes >= TOOL_CARD_STRUCTURED_SCAN_MAX_NODES) break;
    }
    return;
  }

  const keys = Object.keys(value);
  if (keys.length > TOOL_CARD_STRUCTURED_SCAN_MAX_KEYS) {
    state.largeShape = true;
    state.truncated = true;
  }

  const preferredKeys: string[] = [];
  const fallbackKeys: string[] = [];
  for (const key of keys) {
    if (LIKELY_OUTPUT_TEXT_KEYS.has(key.toLowerCase())) {
      preferredKeys.push(key);
    } else {
      fallbackKeys.push(key);
    }
  }

  const keysToScan = preferredKeys.concat(fallbackKeys).slice(0, TOOL_CARD_STRUCTURED_SCAN_MAX_KEYS);
  for (const key of keysToScan) {
    scanStructuredTextValue(value[key], state, depth + 1);
    if (state.nodes >= TOOL_CARD_STRUCTURED_SCAN_MAX_NODES) break;
  }
}

function scanStructuredText(data: any): StructuredTextScan {
  const state: StructuredTextScan = {
    charCount: 0,
    lineCount: 0,
    stringCount: 0,
    maxStringChars: 0,
    diffText: null,
    truncated: false,
    largeShape: false,
    visited: new WeakSet<object>(),
    nodes: 0,
  };
  scanStructuredTextValue(data, state);
  return state;
}

function summarizeOutputData(data: any, commandLike: boolean): OutputSummaryMeta {
  if (!hasData(data)) {
    return {
      hasOutput: false,
      lineCount: null,
      charCount: null,
      isDiff: false,
      stats: null,
      isLarge: false,
      approximate: false,
      shapeLabel: null,
      diffText: null,
    };
  }

  if (typeof data === 'string') {
    const text = data.trim();
    const lineCount = countLines(text);
    const charCount = text.length;
    const isDiff = isDiffContent(text);
    const stats = isDiff ? diffStats(text) : null;
    return {
      hasOutput: text.length > 0,
      lineCount,
      charCount,
      isDiff,
      stats,
      isLarge:
        isDiff ||
        lineCount > TOOL_CARD_LARGE_LINE_THRESHOLD ||
        charCount > TOOL_CARD_LARGE_CHAR_THRESHOLD ||
        (commandLike && lineCount > TOOL_CARD_COMMAND_LARGE_LINE_THRESHOLD),
      approximate: false,
      shapeLabel: null,
      diffText: isDiff ? text : null,
    };
  }

  if (typeof data !== 'object') {
    const text = String(data).trim();
    const lineCount = countLines(text);
    const charCount = text.length;
    return {
      hasOutput: text.length > 0,
      lineCount,
      charCount,
      isDiff: false,
      stats: null,
      isLarge: charCount > TOOL_CARD_LARGE_CHAR_THRESHOLD,
      approximate: false,
      shapeLabel: null,
      diffText: null,
    };
  }

  const topLevelCount = Array.isArray(data) ? data.length : Object.keys(data).length;
  const scan = scanStructuredText(data);
  const shapeLabel = shapeLabelForTopLevel(topLevelCount, Array.isArray(data) ? 'array' : 'object');
  const lineCount = scan.stringCount > 0 ? scan.lineCount : null;
  const charCount = scan.stringCount > 0 ? scan.charCount : null;
  const isDiff = scan.diffText !== null;
  const stats = scan.diffText ? diffStats(scan.diffText) : null;
  const topLevelLarge = topLevelCount > TOOL_CARD_STRUCTURED_SCAN_MAX_KEYS;

  return {
    hasOutput: true,
    lineCount,
    charCount,
    isDiff,
    stats,
    isLarge:
      isDiff ||
      scan.largeShape ||
      scan.truncated ||
      topLevelLarge ||
      hasMetricOver(lineCount, TOOL_CARD_LARGE_LINE_THRESHOLD) ||
      hasMetricOver(charCount, TOOL_CARD_LARGE_CHAR_THRESHOLD) ||
      (commandLike && hasMetricOver(lineCount, TOOL_CARD_COMMAND_LARGE_LINE_THRESHOLD)),
    approximate: scan.stringCount > 1 || scan.truncated,
    shapeLabel,
    diffText: scan.diffText,
  };
}

function formatOutputSummary(meta: OutputSummaryMeta): string {
  const approximatePrefix = meta.approximate ? '≈' : '';
  const lineLabel = meta.lineCount === null
    ? meta.shapeLabel ?? 'structured'
    : `${approximatePrefix}${meta.lineCount.toLocaleString()} line${meta.lineCount === 1 ? '' : 's'}`;
  const charLabel = meta.charCount !== null && meta.charCount > TOOL_CARD_LARGE_CHAR_THRESHOLD
    ? `, ${approximatePrefix}${meta.charCount.toLocaleString()} chars`
    : '';
  const shapeLabel = meta.shapeLabel && meta.lineCount !== null ? `, ${meta.shapeLabel}` : '';

  if (meta.isDiff) {
    const additions = meta.stats?.additions;
    const deletions = meta.stats?.deletions;
    const statsLabel = additions === undefined || deletions === undefined
      ? '+? -?'
      : `+${additions.toLocaleString()} -${deletions.toLocaleString()}`;
    return `Diff (${statsLabel}, ${lineLabel}${shapeLabel})`;
  }

  return `Output (${lineLabel}${charLabel}${shapeLabel})`;
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

  const inputText = useMemo(() => (showInput ? formatData(tool.input) : ''), [showInput, tool.input]);
  const hasInput = hasData(tool.input);
  const lowerName = tool.name.toLowerCase();
  const commandLike = lowerName.includes('command') || lowerName.includes('execute') || lowerName.includes('run') || lowerName.includes('shell');
  const outputMeta = useMemo(() => summarizeOutputData(tool.output, commandLike), [commandLike, tool.output]);
  const hasOutput = outputMeta.hasOutput;
  const outputIsDiff = outputMeta.isDiff;
  const outputIsLarge = outputMeta.isLarge;
  const showOutput = outputVisibilityOverride ?? !outputIsLarge;
  const toggleOutput = () => setOutputVisibilityOverride(!showOutput);
  const outputText = useMemo(() => {
    if (!showOutput || !hasOutput) return '';
    if (outputIsDiff && outputMeta.diffText) {
      return truncateMiddle(outputMeta.diffText);
    }
    return formatData(tool.output);
  }, [hasOutput, outputIsDiff, outputMeta.diffText, showOutput, tool.output]);
  const outputSummary = formatOutputSummary(outputMeta);
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
                <VirtualizedDiffBlock
                  text={outputText}
                  maxHeight={260}
                  rowHeight={20}
                  lineNumberWidth={38}
                  style={{
                    margin: 0,
                    borderRadius: 4,
                    border: '1px solid var(--border-muted)',
                  }}
                />
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
