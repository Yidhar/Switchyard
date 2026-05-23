import React from 'react';
import { Terminal } from 'lucide-react';
import type { Turn } from '../../types';
import { ToolCard } from './ToolCard';

/// Heuristic: does this text look enough like a file path that the Canvas
/// should offer to open it?
///
/// Keep this intentionally conservative. Chat text often contains CJK prose
/// with slashes ("工作区/HEAD", "你/上一轮"), command names, or option strings;
/// those must not become blue file chips. A reference is considered openable
/// only when the final segment is a known source/config/document file name.
const CODE_EXTENSIONS = [
  'ts', 'tsx', 'js', 'jsx', 'mjs', 'cjs',
  'rs', 'py', 'go', 'java', 'kt', 'kts',
  'rb', 'php', 'swift', 'cs',
  'c', 'h', 'cpp', 'cc', 'cxx', 'hpp', 'hh',
  'json', 'toml', 'yaml', 'yml',
  'md', 'markdown',
  'html', 'htm', 'css', 'scss', 'sass',
  'sh', 'bash', 'zsh', 'ps1',
  'sql', 'xml', 'graphql',
];

const EXTENSIONLESS_FILE_NAMES = new Set([
  'dockerfile',
  'makefile',
  'justfile',
  'license',
  'notice',
  'copying',
]);

const DOTFILE_NAMES = new Set([
  '.env',
  '.gitignore',
  '.gitattributes',
  '.dockerignore',
  '.editorconfig',
  '.npmrc',
  '.prettierrc',
  '.eslintrc',
]);

const FILE_REFERENCE_LEADING_PUNCT_RE = /^[<([{"'“‘]+/;
const FILE_REFERENCE_TRAILING_PUNCT_RE = /[>\])}"'“”‘’.,;:!?，。；：！？、]+$/;

export function normalizeFileReferenceForOpen(raw: string): string {
  return raw
    .trim()
    .replace(/^file:\/\//i, '')
    .replace(FILE_REFERENCE_LEADING_PUNCT_RE, '')
    .replace(FILE_REFERENCE_TRAILING_PUNCT_RE, '')
    .replace(/#L\d+(?:-L?\d+)?$/i, '')
    .replace(/:\d+(?::\d+)?$/, '');
}

function fileReferenceBasename(path: string): string {
  const normalized = path.replace(/\\/g, '/');
  const segments = normalized.split('/').filter(Boolean);
  return segments[segments.length - 1] || normalized;
}

function fileReferenceExtension(path: string): string {
  const basename = fileReferenceBasename(path);
  const dotIdx = basename.lastIndexOf('.');
  if (dotIdx <= 0 || dotIdx >= basename.length - 1) return '';
  return basename.slice(dotIdx + 1).toLowerCase();
}

export function looksLikeFilePath(s: string): boolean {
  const normalized = normalizeFileReferenceForOpen(s);
  if (!normalized || normalized.length < 3 || normalized.length > 260) return false;
  if (/^https?:\/\//i.test(normalized)) return false;
  if (normalized.includes('://')) return false;
  if (normalized.endsWith('/') || normalized.endsWith('\\')) return false;
  if (/[\s(){}[\];,，；]/.test(normalized)) return false;

  const basename = fileReferenceBasename(normalized);
  const lowerBasename = basename.toLowerCase();
  if (!basename || basename === '.' || basename === '..') return false;

  if (DOTFILE_NAMES.has(lowerBasename) || EXTENSIONLESS_FILE_NAMES.has(lowerBasename)) {
    return true;
  }

  const ext = fileReferenceExtension(normalized);
  const knownExt = !!ext && CODE_EXTENSIONS.includes(ext);
  return knownExt;
}

function renderFileReference(
  displayText: string,
  openPath: string,
  key: React.Key,
  asCode = false,
  onOpenFile?: (path: string) => void,
): React.ReactNode {
  const title = `Open ${openPath} in Canvas`;
  const handleClick = (e: React.MouseEvent) => {
    e.preventDefault();
    e.stopPropagation();
    onOpenFile?.(openPath);
  };
  if (asCode) {
    return (
      <code
        key={key}
        className="inline-file-reference inline-file-reference-code"
        onClick={handleClick}
        title={title}
      >
        {displayText}
      </code>
    );
  }
  return (
    <button
      key={key}
      type="button"
      className="inline-file-reference"
      onClick={handleClick}
      title={title}
    >
      {displayText}
    </button>
  );
}

function renderPlainTextWithFileReferences(
  text: string,
  keyPrefix: string,
  onOpenFile?: (path: string) => void,
): React.ReactNode[] {
  if (!onOpenFile || !text) return [text];

  return text.split(/(\s+)/).map((token, tokenIdx) => {
    if (!token || /^\s+$/.test(token)) return token;

    const leading = token.match(FILE_REFERENCE_LEADING_PUNCT_RE)?.[0] ?? '';
    const withoutLeading = token.slice(leading.length);
    const trailing = withoutLeading.match(FILE_REFERENCE_TRAILING_PUNCT_RE)?.[0] ?? '';
    const candidate = withoutLeading.slice(
      0,
      trailing ? withoutLeading.length - trailing.length : withoutLeading.length,
    );
    const openPath = normalizeFileReferenceForOpen(candidate);
    if (!looksLikeFilePath(openPath)) return token;

    return (
      <React.Fragment key={`${keyPrefix}-file-${tokenIdx}`}>
        {leading}
        {renderFileReference(candidate, openPath, 'ref', false, onOpenFile)}
        {trailing}
      </React.Fragment>
    );
  });
}

function renderInlineMarkdown(
  text: string,
  keyPrefix: string,
  onOpenFile?: (path: string) => void,
): React.ReactNode[] {
  // Inline formatting only. Block-level markdown (headings/code fences) is
  // handled by renderMarkdownTextPart so we can keep the existing lightweight
  // renderer without pulling a full markdown dependency into the Tauri UI.
  const inlineParts = text.split(/(`[^`\n]+`)/g);
  const nodes: React.ReactNode[] = [];
  inlineParts.forEach((subPart, subIdx) => {
    if (subPart.startsWith('`') && subPart.endsWith('`')) {
      const codeText = subPart.slice(1, -1);
      // When the inline-code looks like a file path AND we have a Canvas
      // open-file handler in scope, render it as a clickable link that opens a
      // tab. Otherwise render the plain `<code>` element.
      if (onOpenFile && looksLikeFilePath(codeText)) {
        nodes.push(
          renderFileReference(
            codeText,
            normalizeFileReferenceForOpen(codeText),
            `${keyPrefix}-inline-code-${subIdx}`,
            true,
            onOpenFile,
          ),
        );
        return;
      }
      nodes.push(<code key={`${keyPrefix}-inline-code-${subIdx}`}>{codeText}</code>);
      return;
    }

    // Check for bold parts. Keep the existing intentionally-small markdown
    // surface: headings, fenced code, inline code, bold, and file references.
    const boldParts = subPart.split(/(\*\*[^*]+\*\*)/g);
    boldParts.forEach((boldPart, boldIdx) => {
      if (boldPart.startsWith('**') && boldPart.endsWith('**')) {
        nodes.push(
          <strong key={`${keyPrefix}-bold-${subIdx}-${boldIdx}`}>
            {renderPlainTextWithFileReferences(
              boldPart.slice(2, -2),
              `${keyPrefix}-${subIdx}-${boldIdx}-strong`,
              onOpenFile,
            )}
          </strong>,
        );
        return;
      }
      nodes.push(
        <React.Fragment key={`${keyPrefix}-text-${subIdx}-${boldIdx}`}>
          {renderPlainTextWithFileReferences(
            boldPart,
            `${keyPrefix}-${subIdx}-${boldIdx}`,
            onOpenFile,
          )}
        </React.Fragment>,
      );
    });
  });
  return nodes;
}

function parseMarkdownHeading(line: string): { level: number; text: string } | null {
  const match = line.match(/^(#{1,6})(.*)$/);
  if (!match) return null;

  const marker = match[1];
  const rest = match[2] ?? '';
  if (!rest.trim()) return null;

  // Standard Markdown requires whitespace after the marker ("## Heading").
  // The GUI also accepts the common CJK/no-space form the user called out
  // ("##标题"). Avoid treating "#include" or "#123" as a heading by only
  // allowing no-space headings for level 2+ or a non-ASCII first character.
  if (/^[ \t]/.test(rest)) {
    return { level: marker.length, text: rest.trimStart() };
  }

  const first = rest[0] ?? '';
  const allowNoSpaceHeading = marker.length >= 2 || /[^\x00-\x7F]/.test(first);
  return allowNoSpaceHeading ? { level: marker.length, text: rest } : null;
}

function renderMarkdownTextPart(
  part: string,
  keyPrefix: string,
  onOpenFile?: (path: string) => void,
): React.ReactNode[] {
  const lines = part.split(/\r?\n/);
  return lines.flatMap((line, lineIdx) => {
    const heading = parseMarkdownHeading(line);
    const key = `${keyPrefix}-line-${lineIdx}`;
    const trailingNewline = lineIdx < lines.length - 1 ? '\n' : null;
    if (heading) {
      return [
        React.createElement(
          `h${heading.level}` as keyof React.JSX.IntrinsicElements,
          { key, className: `message-heading message-heading-${heading.level}` },
          renderInlineMarkdown(heading.text, `${key}-heading`, onOpenFile),
        ),
        // Block headings already occupy their own line; only preserve extra
        // blank lines from the source, not the structural line break itself.
      ];
    }
    return [
      <React.Fragment key={key}>
        {renderInlineMarkdown(line, key, onOpenFile)}
        {trailingNewline}
      </React.Fragment>,
    ];
  });
}

export function renderMessageBody(
  text: string | null,
  style?: React.CSSProperties,
  onOpenFile?: (path: string) => void,
  // `onProposeForCanvas` was an older affordance that copied AI chat
  // code blocks into the Canvas as a "proposed change" for the user to
  // apply. That direction was wrong — the AI applies changes itself
  // via Codex/Claude tool calls, and Canvas surfaces the resulting
  // unified diff via the PostToolUse-hook capture pipeline. The
  // parameter is kept in the signature for callsite compatibility but
  // ignored.
  _onProposeForCanvas?: (path: string, content: string) => void,
) {
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
          // Renders a plain code block. We no longer offer an
          // "Apply to Canvas" affordance on chat code blocks — the
          // AI applies its own changes via Codex/Claude tools, and
          // Canvas surfaces the resulting unified diff automatically.
          const match = part.match(/^```([^\n]*)\n?([\s\S]*?)```$/);
          const infoString = match ? match[1].trim() : '';
          const language = infoString.split(/\s+/)[0] ?? '';
          const codeContent = match ? match[2] : part.slice(3, -3);

          return (
            <div
              key={idx}
              className="code-block-container"
              style={{ margin: '8px 0', position: 'relative' }}
            >
              {language && (
                <div
                  style={{
                    position: 'absolute',
                    top: 8,
                    right: 12,
                    fontSize: 11,
                    color: 'var(--text-muted)',
                    textTransform: 'uppercase',
                    fontWeight: 'bold',
                    letterSpacing: '0.5px',
                    userSelect: 'none',
                  }}
                >
                  {language}
                </div>
              )}
              <pre style={{ margin: 0 }}>
                <code>
                  {renderPlainTextWithFileReferences(codeContent.trim(), `code-${idx}`, onOpenFile)}
                </code>
              </pre>
            </div>
          );
        }

        // Regular text block. Render a small markdown subset plus the existing
        // inline code / bold / file reference affordances.
        return (
          <React.Fragment key={idx}>
            {renderMarkdownTextPart(part, `part-${idx}`, onOpenFile)}
          </React.Fragment>
        );
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
      '[错误]', '[执行]', '[exec]', '[HTTP]', '[STDIO]', '[hyard]', '[error]', '[命令]', '[权限]'
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

type ToolStatus = 'pending' | 'running' | 'completed' | 'failed' | 'cancelled';

interface ToolDisplay {
  id: string;
  name: string;
  input: any;
  status: ToolStatus;
  output: any;
  actions?: Array<{
    id?: string;
    label: string;
    tone?: 'primary' | 'danger' | 'secondary';
    title?: string;
    disabled?: boolean;
    onClick: () => void | Promise<void>;
  }>;
}

export interface RenderTurnEventsOptions {
  onResolveApproval?: (
    requestId: string,
    decision: 'approve' | 'deny',
    reason?: string,
  ) => void | Promise<void>;
}

const CALL_ITEM_TYPES = new Set([
  'tool_use',
  'tool_call',
  'function_call',
  'custom_tool_call',
  'mcp_tool_call',
  'local_shell_call',
]);

const RESULT_ITEM_TYPES = new Set([
  'tool_result',
  'tool_response',
  'function_call_output',
  'custom_tool_call_output',
  'mcp_tool_call_output',
  'local_shell_call_output',
]);

const PROVIDER_ITEM_EVENT_TYPES = new Set([
  'item_started',
  'item_updated',
  'item_completed',
  'artifact_ready',
]);

function firstPresent<T = any>(...values: T[]): T | undefined {
  return values.find((value) => value !== undefined && value !== null && !(typeof value === 'string' && value.length === 0));
}

function hasMeaningfulDisplayValue(value: any): boolean {
  if (value === undefined || value === null) return false;
  if (typeof value === 'string') return value.trim().length > 0;
  if (Array.isArray(value)) return value.some(hasMeaningfulDisplayValue);
  if (typeof value === 'object') {
    return Object.entries(value).some(([key, nested]) => {
      if (['type', 'role', 'status', 'id', 'index', 'encrypted_content'].includes(key)) {
        return false;
      }
      return hasMeaningfulDisplayValue(nested);
    });
  }
  return false;
}

function firstMeaningful<T = any>(...values: T[]): T | undefined {
  return values.find(hasMeaningfulDisplayValue);
}

function asString(value: any): string | undefined {
  if (value === undefined || value === null) return undefined;
  return typeof value === 'string' ? value : String(value);
}

function normalizeProviderEventType(value: any): string {
  const text = asString(value)?.trim();
  if (!text) return '';
  return text
    .replace(/([a-z0-9])([A-Z])/g, '$1_$2')
    .replace(/[./\s-]+/g, '_')
    .toLowerCase();
}

function itemTypeFromLooseValue(value: any): string | undefined {
  const text = asString(value);
  if (!text) return undefined;
  // Provider protocol envelopes often use values such as `item.started`,
  // `item/started`, or `turn.completed` in `payload.type`. Those are lifecycle
  // markers, not renderable item kinds. Treat slash/dot values as protocol
  // kinds unless they came from an actual nested `item.type`.
  return text.includes('.') || text.includes('/') ? undefined : text;
}

function executionCommand(execution: any): string | undefined {
  return asString(firstPresent(
    execution?.actual_display,
    execution?.actual_command,
    execution?.resolved_command,
    execution?.original_command,
    execution?.command,
  ));
}

function commandArgsSuffix(args: any): string {
  if (args === undefined || args === null) return '';
  if (Array.isArray(args)) return args.length > 0 ? ` ${args.join(' ')}` : '';
  const text = String(args);
  return text ? ` ${text}` : '';
}

function getPayloadItem(payload: any): any {
  return (
    payload?.item ||
    payload?.params?.item ||
    payload?.event?.item ||
    payload?.msg?.item ||
    payload?.message?.item ||
    payload?.data?.item ||
    payload?.params ||
    payload?.event ||
    payload?.msg ||
    payload ||
    {}
  );
}

function getItemType(payload: any, item: any): string {
  return (
    asString(payload?.item_type) ||
    asString(payload?.params?.item_type) ||
    itemTypeFromLooseValue(item?.type) ||
    itemTypeFromLooseValue(payload?.params?.type) ||
    itemTypeFromLooseValue(payload?.type) ||
    ''
  );
}

function getProtocolKind(payload: any): string {
  return asString(payload?.method) || asString(payload?.params?.method) || asString(payload?.type) || '';
}

function isProviderItemEvent(event: any): boolean {
  return PROVIDER_ITEM_EVENT_TYPES.has(normalizeProviderEventType(event?.event_type));
}

function normalizeStatus(payload: any, item: any, eventType?: string): ToolStatus {
  const lifecycle = normalizeProviderEventType(eventType);
  const protocol = [getProtocolKind(payload), lifecycle]
    .join('.')
    .toLowerCase()
    .replace(/\//g, '.');
  const raw = asString(firstPresent(
    item?.status,
    payload?.status,
    payload?.decision?.decision,
    payload?.decision,
  ))?.toLowerCase();

  if (raw) {
    if (['failed', 'failure', 'error', 'errored', 'rejected', 'deny', 'denied'].includes(raw)) return 'failed';
    if (['cancelled', 'canceled', 'aborted', 'skipped'].includes(raw)) return 'cancelled';
  }

  const exitCode = firstPresent(item?.exit_code, payload?.exit_code);
  if (protocol.includes('failed') || protocol.includes('error')) return 'failed';
  if (protocol.includes('cancel')) return 'cancelled';
  if (protocol.includes('completed') || protocol.includes('complete') || protocol.includes('artifact_ready') || protocol.includes('output')) {
    return exitCode !== undefined && Number(exitCode) !== 0 ? 'failed' : 'completed';
  }
  if (raw) {
    if (['pending', 'queued'].includes(raw)) return 'pending';
    if (['in_progress', 'in-progress', 'running', 'started', 'streaming'].includes(raw)) return 'running';
    if (['completed', 'complete', 'success', 'succeeded', 'done', 'finished', 'approved', 'approve'].includes(raw)) return 'completed';
  }
  if (protocol.includes('started') || protocol.includes('updated')) return 'running';
  return 'running';
}

function commandInput(payload: any, item: any): any {
  const execution = firstPresent(item?.execution, payload?.execution);
  const executionDisplay = executionCommand(execution);
  if (executionDisplay) return executionDisplay;

  const command = firstPresent(item?.command, payload?.command, item?.cmd, payload?.cmd);
  if (command) return command;
  const args = firstPresent(item?.args, payload?.args, item?.arguments, payload?.arguments);
  if (Array.isArray(args)) return args.join(' ');
  return args;
}

function toolInput(payload: any, item: any, type: string): any {
  if (type === 'command_execution' || type === 'local_shell_call') {
    return commandInput(payload, item);
  }
  return firstPresent(
    item?.input,
    payload?.input,
    item?.arguments,
    payload?.arguments,
    item?.args,
    payload?.args,
    item?.params,
    payload?.params,
    item?.path,
    payload?.path,
    item?.command,
    payload?.command,
  );
}

function toolOutput(payload: any, item: any): any {
  const explicit = firstPresent(
    item?.aggregated_output,
    payload?.aggregated_output,
    item?.output,
    payload?.output,
    item?.result,
    payload?.result,
    item?.content,
    payload?.content,
    item?.error,
    payload?.error,
  );
  if (explicit !== undefined) return explicit;

  const stdout = firstPresent(item?.stdout, payload?.stdout);
  const stderr = firstPresent(item?.stderr, payload?.stderr);
  if (stdout || stderr) {
    return [stdout, stderr].filter(Boolean).join('\n');
  }

  const exitCode = firstPresent(item?.exit_code, payload?.exit_code);
  return exitCode !== undefined ? `Exit Code: ${exitCode}` : null;
}

function toolName(payload: any, item: any, type: string): string {
  const explicit = asString(firstPresent(
    item?.name,
    payload?.name,
    item?.tool_name,
    payload?.tool_name,
    item?.function?.name,
    payload?.function?.name,
  ));
  if (explicit && explicit.trim()) return explicit;

  if (type === 'command_execution' || type === 'local_shell_call') return 'Execute Command';
  if (type === 'file_change' || type === 'diff_ready') return 'Edit File';
  if (type === 'todo_list') return 'Task Planning (Codex)';
  if (type === 'approval_request') return '权限确认请求';
  if (type === 'approval_decision') return '权限处理结果';
  if (type === 'server_request') return 'Codex Server Request';
  if (type === 'reasoning') return 'Reasoning';
  if (type.includes('mcp')) {
    const server = asString(firstPresent(item?.server, payload?.server, item?.server_name, payload?.server_name));
    const mcpTool = asString(firstPresent(item?.tool, payload?.tool, item?.tool_name, payload?.tool_name));
    const combined = [server, mcpTool].filter(Boolean).join(' / ');
    return combined || 'MCP Tool';
  }
  if (type === 'function_call' || type === 'function_call_output') return 'Function Call';
  if (type === 'custom_tool_call' || type === 'custom_tool_call_output') return 'Custom Tool Call';
  if (type === 'tool_result' || type === 'tool_response') return 'Tool Result';
  return 'Tool Call';
}

function stableToolId(turnId: string, event: any, index: number, payload: any, item: any, type: string, name: string): string {
  if (type === 'reasoning') {
    return `${turnId}:reasoning`;
  }
  if (type === 'approval_request' || type === 'approval_decision') {
    const approvalRequestId = firstPresent(item?.request_id, payload?.request_id);
    if (approvalRequestId !== undefined) {
      return `${turnId}:approval:${String(approvalRequestId)}`;
    }
  }
  const natural = firstPresent(
    item?.id,
    payload?.id,
    item?.call_id,
    payload?.call_id,
    item?.tool_call_id,
    payload?.tool_call_id,
    item?.request_id,
    payload?.request_id,
    commandInput(payload, item),
    item?.path,
    payload?.path,
  );
  if (natural !== undefined) {
    return `${turnId}:${type || 'tool'}:${String(natural)}`;
  }
  return `${turnId}:${type || 'tool'}:${name}:${event.event_id || index}`;
}

function mergeTool(toolCalls: ToolDisplay[], next: ToolDisplay) {
  const existing = toolCalls.find(
    (tool) =>
      tool.id === next.id ||
      (tool.name === next.name && (tool.status === 'running' || tool.status === 'pending') && !tool.output),
  );
  if (!existing) {
    toolCalls.push(next);
    return;
  }
  existing.status = next.status || existing.status;
  if (next.input !== undefined && next.input !== null) existing.input = next.input;
  if (next.output !== undefined && next.output !== null) existing.output = next.output;
  if (next.actions !== undefined) existing.actions = next.actions;
}

export function renderTurnEvents(
  turnId: string,
  events: any[],
  turns: Turn[],
  realtimeLines?: string[],
  hyardJobs?: Record<string, any>,
  options: RenderTurnEventsOptions = {},
) {
  // Filter events for this turn
  const turnEvents = events.filter((e) => e.turn_id === turnId);

  // Find execution telemetry event
  const telemetryEvent = turnEvents.find((e) => {
    if (!isProviderItemEvent(e)) return false;
    const item = getPayloadItem(e.payload);
    return getItemType(e.payload, item) === 'execution_telemetry';
  });
  const telemetryPayload = telemetryEvent?.payload;
  const telemetryItem = telemetryPayload ? getPayloadItem(telemetryPayload) : null;
  const telemetryExecution = firstPresent(telemetryPayload?.execution, telemetryItem?.execution);
  let commandLine = executionCommand(telemetryExecution);
  let commandArgs = telemetryExecution?.args;

  // Gather terminal outputs from db events
  const dbTerminalLines = turnEvents
    .filter((e) => {
      if (!isProviderItemEvent(e)) return false;
      const item = getPayloadItem(e.payload);
      return getItemType(e.payload, item) === 'terminal_output';
    })
    .map((e) => {
      const item = getPayloadItem(e.payload);
      return firstPresent(e.payload?.line, item?.line);
    })
    .filter(Boolean);

  let combinedTerminal = [...dbTerminalLines, ...(realtimeLines || [])];

  if (hyardJobs && hyardJobs[turnId]) {
    const job = hyardJobs[turnId];
    if (job.last_output_preview && combinedTerminal.length === 0) {
      combinedTerminal = job.last_output_preview.split('\n');
    }
    if (job.execution && !commandLine) {
      commandLine = executionCommand(job.execution);
      commandArgs = job.execution.args;
    }
  }

  // Extract tool calls and delegate sub-agents
  const toolCalls: ToolDisplay[] = [];

  // 1. Gather child delegate turns
  const delegates = turns.filter((t) => t.delegated_by === turnId);
  delegates.forEach((d) => {
    mergeTool(toolCalls, {
      id: d.turn_id,
      name: `Delegation to ${d.provider} (${d.role})`,
      input: d.user_message,
      status: normalizeStatus({ status: d.status }, {}),
      output: d.provider_response || d.error_message,
    });
  });

  // 2. Gather standard tool use and result events, plus Codex specific protocol items.
  turnEvents.forEach((e, index) => {
    if (!isProviderItemEvent(e)) return;
    const payload = e.payload;
    if (!payload) return;

    const item = getPayloadItem(payload);
    const type = getItemType(payload, item);
    const protocol = getProtocolKind(payload).toLowerCase().replace(/\//g, '.');
    const itemType = type.toLowerCase();

    if (itemType === 'agent_message' || itemType === 'assistant' || itemType === 'terminal_output' || itemType === 'execution_telemetry') {
      return;
    }

    const name = toolName(payload, item, itemType);
    const id = stableToolId(turnId, e, index, payload, item, itemType, name);

    if (itemType === 'approval_request') {
      const requestId = asString(payload.request_id);
      const timeoutSecs = payload.timeout_secs ? Number(payload.timeout_secs) : null;
      const policy = payload.policy || {};
      const policyDecisionTag = asString(firstPresent(
        payload.policy_decision?.decision_tag,
        payload.policyDecision?.decisionTag,
      ));
      const input = {
        method: payload.method,
        sandbox_mode: policy.sandbox_mode,
        cwd: policy.cwd,
        allowed_paths: policy.allowed_paths,
        request: payload.request,
      };
      const outputLines = [
        '正在等待你的批准/拒绝；在你操作或超时之前不会静默发送 deny。',
        timeoutSecs ? `超时: ${timeoutSecs}s` : null,
        policyDecisionTag ? `策略预览: ${policyDecisionTag}` : null,
      ].filter(Boolean);
      const resolveApproval = options.onResolveApproval;
      mergeTool(toolCalls, {
        id,
        name,
        input,
        status: 'pending',
        output: outputLines.join('\n'),
        actions: requestId && resolveApproval
          ? [
              {
                id: `${requestId}:approve`,
                label: '批准',
                tone: 'primary',
                title: '批准这次 Codex 工具/文件权限请求',
                onClick: () => resolveApproval(requestId, 'approve'),
              },
              {
                id: `${requestId}:deny`,
                label: '拒绝',
                tone: 'danger',
                title: '拒绝这次 Codex 工具/文件权限请求',
                onClick: () => resolveApproval(requestId, 'deny'),
              },
            ]
          : undefined,
      });
    } else if (itemType === 'approval_decision') {
      const decisionTag = payload.decision_tag || payload.tag;
      const decisionText = [
        asString(decisionTag || payload.decision?.decision || payload.decision),
        asString(payload.reason),
      ].filter(Boolean).join('\n');
      mergeTool(toolCalls, {
        id,
        name,
        input: payload.request,
        status: String(decisionTag || '').startsWith('deny') ? 'failed' : 'completed',
        output: decisionText || decisionTag || payload.decision,
        actions: [],
      });
    } else if (itemType === 'server_request') {
      mergeTool(toolCalls, {
        id,
        name,
        input: firstPresent(payload.request, payload.params, item?.request, item?.params),
        status: normalizeStatus(payload, item, e.event_type),
        output: firstMeaningful(payload.summary, item?.summary, payload.result, item?.result, payload.response, payload.error),
      });
    } else if (CALL_ITEM_TYPES.has(itemType)) {
      mergeTool(toolCalls, {
        id,
        name,
        input: toolInput(payload, item, itemType),
        status: normalizeStatus(payload, item, e.event_type),
        output: toolOutput(payload, item),
      });
    } else if (RESULT_ITEM_TYPES.has(itemType)) {
      mergeTool(toolCalls, {
        id,
        name,
        input: toolInput(payload, item, itemType),
        status: normalizeStatus({ ...payload, type: payload.type || 'item.completed' }, item, e.event_type),
        output: toolOutput(payload, item),
      });
    } else if (itemType === 'command_execution') {
      mergeTool(toolCalls, {
        id,
        name,
        input: commandInput(payload, item),
        status: normalizeStatus(payload, item, e.event_type),
        output: toolOutput(payload, item),
      });
    } else if (itemType === 'file_change' || itemType === 'diff_ready') {
      mergeTool(toolCalls, {
        id,
        name,
        input: firstPresent(item?.path, payload?.path, item?.file, payload?.file),
        status: normalizeStatus(payload, item, e.event_type),
        output: firstPresent(item?.diff, payload?.diff, item?.patch, payload?.patch, item?.summary, payload?.summary),
      });
    } else if (itemType === 'todo_list') {
      mergeTool(toolCalls, {
        id,
        name,
        input: firstPresent(item?.items, payload?.items),
        status: 'completed',
        output: null,
      });
    } else if (itemType === 'reasoning') {
      const summary = firstMeaningful(
        item?.summary,
        payload?.summary,
        payload?.params?.summary,
        item?.text,
        payload?.text,
        payload?.params?.text,
        item?.content,
        payload?.content,
        payload?.params?.content,
        item?.delta?.summary,
        payload?.delta?.summary,
        payload?.params?.delta?.summary,
        item?.delta?.text,
        payload?.delta?.text,
        payload?.params?.delta?.text,
        item?.delta?.content,
        payload?.delta?.content,
        payload?.params?.delta?.content,
      );
      if (summary !== undefined) {
        mergeTool(toolCalls, {
          id,
          name,
          input: null,
          status: protocol.includes('completed') ? 'completed' : 'running',
          output: summary,
        });
      }
    }
  });

  if (!commandLine && toolCalls.length === 0 && combinedTerminal.length === 0) {
    return null;
  }

  const hasRunningTool = toolCalls.some((tool) => tool.status === 'running' || tool.status === 'pending');
  const hasLiveTerminal = (realtimeLines?.length ?? 0) > 0;

  return (
    <div className="execution-details-accordion" style={{ marginTop: '10px', fontSize: '12px' }}>
      <details
        open={hasRunningTool || hasLiveTerminal}
        style={{ background: 'rgba(0,0,0,0.15)', borderRadius: '4px', border: '1px solid var(--border-muted)', overflow: 'hidden' }}
      >
        <summary style={{ padding: '8px 12px', cursor: 'pointer', fontWeight: 'bold', display: 'flex', alignItems: 'center', gap: '6px', userSelect: 'none', background: 'rgba(255,255,255,0.02)' }}>
          <Terminal size={14} style={{ color: 'var(--color-secondary)' }} />
          <span>Execution Details & Logs</span>
          <span style={{ marginLeft: 'auto', display: 'flex', gap: '6px', alignItems: 'center' }}>
            {hasRunningTool && (
              <span style={{ fontSize: '11px', padding: '1px 5px', background: 'rgba(245, 158, 11, 0.1)', color: 'var(--color-warning)', borderRadius: '3px' }}>
                Running
              </span>
            )}
            {toolCalls.length > 0 && (
              <span style={{ fontSize: '11px', padding: '1px 5px', background: 'rgba(6, 182, 212, 0.1)', color: 'var(--color-secondary)', borderRadius: '3px' }}>
                {toolCalls.length} Tool{toolCalls.length > 1 ? 's' : ''}
              </span>
            )}
            {combinedTerminal.length > 0 && (
              <span style={{ fontSize: '11px', padding: '1px 5px', background: 'rgba(59, 130, 246, 0.1)', color: 'var(--color-primary)', borderRadius: '3px' }}>
                {combinedTerminal.length} Log{combinedTerminal.length > 1 ? 's' : ''}
              </span>
            )}
          </span>
        </summary>
        <div style={{ padding: '12px', display: 'flex', flexDirection: 'column', gap: '10px', borderTop: '1px solid var(--border-muted)' }}>
          {commandLine && (
            <div>
              <div style={{ fontWeight: 'bold', color: 'var(--text-secondary)', marginBottom: '4px' }}>Subprocess Command:</div>
              <code style={{ display: 'block', padding: '6px 10px', background: 'rgba(0, 0, 0, 0.3)', borderRadius: '4px', wordBreak: 'break-all', fontFamily: 'monospace' }}>
                {commandLine}{commandArgsSuffix(commandArgs)}
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
