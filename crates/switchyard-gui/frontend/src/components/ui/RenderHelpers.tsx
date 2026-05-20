import React from 'react';
import { Terminal } from 'lucide-react';
import type { Turn } from '../../types';
import { ToolCard } from './ToolCard';

export function renderMessageBody(text: string | null, style?: React.CSSProperties) {
  if (!text) return null;

  // Check if it contains a delegate JSON block
  const jsonMatch = text.match(/<<<SWITCHYARD_JSON_BEGIN>>>([\s\S]*?)<<<SWITCHYARD_JSON_END>>>/);
  let delegateCard: React.ReactNode = null;
  let remainingText = text;

  if (jsonMatch && jsonMatch[1]) {
    try {
      const parsed = JSON.parse(jsonMatch[1]);
      if (parsed && parsed.type === 'delegate') {
        const requests = Array.isArray(parsed.requests)
          ? parsed.requests
          : [{
              id: parsed.id,
              provider: parsed.provider || 'peer',
              role: parsed.role,
              task: parsed.task || '',
              timeout_sec: parsed.timeout_sec,
              write_access: parsed.write_access,
            }];
        delegateCard = (
          <div 
            className="delegate-request-card" 
            style={{ 
              marginTop: '10px', 
              padding: '12px', 
              background: 'rgba(6, 182, 212, 0.05)', 
              borderLeft: '4px solid var(--color-secondary)',
              borderRadius: '0 4px 4px 0',
              display: 'flex',
              flexDirection: 'column',
              gap: '6px'
            }}
          >
            <div style={{ fontWeight: 'bold', display: 'flex', alignItems: 'center', gap: '6px', color: 'var(--color-secondary)' }}>
              <span>Switchyard Delegation Request</span>
            </div>
            {requests.map((request: any, idx: number) => (
              <div
                key={request.id || `${request.provider || 'peer'}-${idx}`}
                style={{
                  padding: requests.length > 1 ? '8px 0 0 0' : 0,
                  borderTop: requests.length > 1 && idx > 0 ? '1px solid var(--border-muted)' : 'none',
                  display: 'flex',
                  flexDirection: 'column',
                  gap: '5px'
                }}
              >
                <div style={{ display: 'flex', gap: '6px', flexWrap: 'wrap', alignItems: 'center' }}>
                  <span style={{ fontSize: '12px', color: 'var(--text-secondary)' }}>
                    委托给 <strong style={{ color: 'var(--text-primary)' }}>{request.provider || 'peer'}</strong>
                  </span>
                  {request.role && (
                    <span className="status-badge status-pending">{request.role}</span>
                  )}
                  {request.id && (
                    <code style={{ fontSize: '11px' }}>{request.id}</code>
                  )}
                  {request.timeout_sec && (
                    <span style={{ fontSize: '11px', color: 'var(--text-muted)' }}>{request.timeout_sec}s</span>
                  )}
                  {request.write_access === true && (
                    <span style={{ fontSize: '11px', color: 'var(--color-highlight)' }}>write access</span>
                  )}
                </div>
                <div style={{ whiteSpace: 'pre-wrap', fontSize: '13px', color: 'var(--text-primary)' }}>
                  {request.task || '(empty task)'}
                </div>
              </div>
            ))}
          </div>
        );
        // Strip the JSON block from the text so we don't render the raw JSON
        remainingText = text.replace(/<<<SWITCHYARD_JSON_BEGIN>>>[\s\S]*?<<<SWITCHYARD_JSON_END>>>/, '');
      }
    } catch (e) {
      // Ignore parsing error
    }
  }

  // Split by code blocks first
  const parts = remainingText.split(/(```[\s\S]*?```)/g);

  return (
    <div className="message-body" style={style}>
      {parts.map((part, idx) => {
        if (part.startsWith('```') && part.endsWith('```')) {
          // It's a code block
          const match = part.match(/^```(\w*)\n?([\s\S]*?)```$/);
          const language = match ? match[1] : '';
          const codeContent = match ? match[2] : part.slice(3, -3);
          
          return (
            <div key={idx} className="code-block-container" style={{ margin: '8px 0', position: 'relative' }}>
              {language && (
                <div style={{
                  position: 'absolute',
                  top: '8px',
                  right: '12px',
                  fontSize: '11px',
                  color: 'var(--text-muted)',
                  textTransform: 'uppercase',
                  fontWeight: 'bold',
                  letterSpacing: '0.5px',
                  userSelect: 'none'
                }}>
                  {language}
                </div>
              )}
              <pre style={{ margin: 0 }}>
                <code>{codeContent.trim()}</code>
              </pre>
            </div>
          );
        } else {
          // Regular text block. Render with inline formatting (inline code, bold)
          const inlineParts = part.split(/(`[^`\n]+`)/g);
          return (
            <span key={idx}>
              {inlineParts.map((subPart, subIdx) => {
                if (subPart.startsWith('`') && subPart.endsWith('`')) {
                  return (
                    <code key={subIdx}>
                      {subPart.slice(1, -1)}
                    </code>
                  );
                }

                // Check for bold parts
                const boldParts = subPart.split(/(\*\*[^*]+\*\*)/g);
                return boldParts.map((boldPart, boldIdx) => {
                  if (boldPart.startsWith('**') && boldPart.endsWith('**')) {
                    return (
                      <strong key={boldIdx}>
                        {boldPart.slice(2, -2)}
                      </strong>
                    );
                  }
                  return boldPart;
                });
              })}
            </span>
          );
        }
      })}
      {delegateCard}
    </div>
  );
}

export function isSystemStatusText(text: string): boolean {
  const trimmed = text.trim();
  if (trimmed.startsWith('[')) {
    const statusPrefixes = [
      '[会话]', '[回合]', '[系统]', '[助手]', '[结果]', '[限额]', 
      '[思考]', '[工具]', '[文件]', '[Diff]', '[待办]', '[委托]', 
      '[错误]', '[执行]', '[exec]', '[HTTP]', '[STDIO]', '[hyard]', '[error]', '[命令]'
    ];
    if (statusPrefixes.some(prefix => trimmed.startsWith(prefix))) {
      return true;
    }
    if (/^\[[a-zA-Z0-9_\-:/]+\]/.test(trimmed)) {
      return true;
    }
  }
  return false;
}

export function renderTurnEvents(
  turnId: string, 
  events: any[], 
  turns: Turn[], 
  realtimeLines?: string[], 
  hyardJobs?: Record<string, any>
) {
  // Filter events for this turn
  const turnEvents = events.filter((e) => e.turn_id === turnId);
  
  // Find execution telemetry event
  const telemetryEvent = turnEvents.find(
    (e) => e.event_type === 'item_updated' && e.payload?.item_type === 'execution_telemetry'
  );
  let commandLine = telemetryEvent?.payload?.execution?.command;
  let commandArgs = telemetryEvent?.payload?.execution?.args;
  
  // Gather terminal outputs from db events
  const dbTerminalLines = turnEvents
    .filter((e) => e.event_type === 'item_updated' && e.payload?.item_type === 'terminal_output')
    .map((e) => e.payload?.line)
    .filter(Boolean);

  let combinedTerminal = [...dbTerminalLines, ...(realtimeLines || [])];

  if (hyardJobs && hyardJobs[turnId]) {
    const job = hyardJobs[turnId];
    if (job.last_output_preview && combinedTerminal.length === 0) {
      combinedTerminal = job.last_output_preview.split('\n');
    }
    if (job.execution && !commandLine) {
      commandLine = job.execution.command;
      commandArgs = job.execution.args;
    }
  }

  // Extract tool calls and delegate sub-agents
  const toolCalls: any[] = [];

  // 1. Gather child delegate turns
  const delegates = turns.filter((t) => t.delegated_by === turnId);
  delegates.forEach((d) => {
    toolCalls.push({
      id: d.turn_id,
      name: `Delegation to ${d.provider} (${d.role})`,
      input: d.user_message,
      status: d.status,
      output: d.provider_response || d.error_message,
    });
  });

  // 2. Gather standard tool use and result events, plus Codex specific protocol items
  turnEvents.forEach((e) => {
    if (e.event_type === 'item_updated') {
      const payload = e.payload;
      if (!payload) return;

      const item = payload.item || payload;
      const type = item.type || payload.type;
      const id = item.id || payload.id || Math.random().toString();

      if (type === 'tool_use' || type === 'tool_call') {
        const name = payload.name || item.name;
        if (name && name.trim()) {
          toolCalls.push({
            id,
            name,
            input: payload.input || item.input || payload.arguments || item.arguments,
            status: 'running',
            output: null,
          });
        }
      } else if (type === 'tool_result' || type === 'tool_response') {
        const name = payload.name || item.name;
        const existing = toolCalls.find((tc) => tc.id === id || (name && tc.name === name && tc.status === 'running'));
        if (existing) {
          existing.status = 'completed';
          existing.output = payload.output || item.output || payload.content || item.content;
        } else if (name && name.trim()) {
          toolCalls.push({
            id,
            name,
            status: 'completed',
            output: payload.output || item.output || payload.content || item.content,
          });
        }
      } else if (type === 'todo_list') {
        const existing = toolCalls.find((tc) => tc.id === id);
        if (!existing) {
          toolCalls.push({
            id,
            name: 'Task Planning (Codex)',
            input: item.items,
            status: 'completed',
            output: null,
          });
        }
      } else if (type === 'command_execution') {
        const existing = toolCalls.find((tc) => tc.id === id);
        const statusMap: Record<string, string> = {
          'in_progress': 'running',
          'completed': 'completed',
          'failed': 'failed',
        };
        const status = statusMap[item.status] || 'running';
        const output = item.aggregated_output || item.error || (item.exit_code !== undefined ? `Exit Code: ${item.exit_code}` : null);
        
        if (existing) {
          existing.status = status;
          if (output) existing.output = output;
        } else {
          toolCalls.push({
            id,
            name: 'Execute Command',
            input: item.command,
            status,
            output,
          });
        }
      } else if (type === 'file_change') {
        const existing = toolCalls.find((tc) => tc.id === id);
        const status = (payload.type === 'item.completed' || item.status === 'completed') ? 'completed' : 'running';
        if (existing) {
            existing.status = status;
            if (item.diff) existing.output = item.diff;
        } else {
            toolCalls.push({
                id,
                name: 'Edit File',
                input: item.path,
                status,
                output: item.diff || null,
            });
        }
      }
    }
  });

  if (!commandLine && toolCalls.length === 0 && combinedTerminal.length === 0) {
    return null;
  }

  return (
    <div className="execution-details-accordion" style={{ marginTop: '10px', fontSize: '12px' }}>
      <details style={{ background: 'rgba(0,0,0,0.15)', borderRadius: '4px', border: '1px solid var(--border-muted)', overflow: 'hidden' }}>
        <summary style={{ padding: '8px 12px', cursor: 'pointer', fontWeight: 'bold', display: 'flex', alignItems: 'center', gap: '6px', userSelect: 'none', background: 'rgba(255,255,255,0.02)' }}>
          <Terminal size={14} style={{ color: 'var(--color-secondary)' }} />
          <span>Execution Details & Logs</span>
          {toolCalls.length > 0 && (
            <span style={{ fontSize: '11px', padding: '1px 5px', background: 'rgba(6, 182, 212, 0.1)', color: 'var(--color-secondary)', borderRadius: '3px', marginLeft: 'auto' }}>
              {toolCalls.length} Tool{toolCalls.length > 1 ? 's' : ''}
            </span>
          )}
        </summary>
        <div style={{ padding: '12px', display: 'flex', flexDirection: 'column', gap: '10px', borderTop: '1px solid var(--border-muted)' }}>
          {commandLine && (
            <div>
              <div style={{ fontWeight: 'bold', color: 'var(--text-secondary)', marginBottom: '4px' }}>Subprocess Command:</div>
              <code style={{ display: 'block', padding: '6px 10px', background: 'rgba(0, 0, 0, 0.3)', borderRadius: '4px', wordBreak: 'break-all', fontFamily: 'monospace' }}>
                {commandLine} {commandArgs ? commandArgs.join(' ') : ''}
              </code>
            </div>
          )}

          {toolCalls.length > 0 && (
            <div>
              <div style={{ fontWeight: 'bold', color: 'var(--text-secondary)', marginBottom: '6px' }}>Tool Executions:</div>
              <div style={{ display: 'flex', flexDirection: 'column', gap: '6px' }}>
                {toolCalls.map((tc, idx) => (
                  <ToolCard key={tc.id || idx} tool={tc} />
                ))}
              </div>
            </div>
          )}

          {combinedTerminal.length > 0 && (
            <div>
              <div style={{ fontWeight: 'bold', color: 'var(--text-secondary)', marginBottom: '4px' }}>Subprocess Logs & Console Output:</div>
              <pre style={{ margin: 0, padding: '8px 12px', background: '#0c0f1d', borderRadius: '4px', color: '#38bdf8', fontFamily: 'monospace', fontSize: '11px', maxHeight: '180px', overflowY: 'auto', border: '1px solid #1e293b', whiteSpace: 'pre-wrap', wordBreak: 'break-all' }}>
                {combinedTerminal.join('\n')}
              </pre>
            </div>
          )}
        </div>
      </details>
    </div>
  );
}
