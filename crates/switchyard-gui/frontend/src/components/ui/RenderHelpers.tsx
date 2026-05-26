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
  const displayText = stripSystemStatusLeakText(text);
  if (!displayText) return null;

  // Check if it contains a delegate JSON block
  const jsonMatch = displayText.match(/<<<SWITCHYARD_JSON_BEGIN>>>([\s\S]*?)<<<SWITCHYARD_JSON_END>>>/);
  let delegateCard: React.ReactNode = null;
  let remainingText = displayText;

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
        remainingText = displayText.replace(/<<<SWITCHYARD_JSON_BEGIN>>>[\s\S]*?<<<SWITCHYARD_JSON_END>>>/, '');
      }
    } catch (e) {
      // Ignore parsing error
    }
  }

  if (!remainingText.trim() && !delegateCard) return null;

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

function looksLikeExecutionLeakText(text: string): boolean {
  const trimmed = text.trim();
  if (!trimmed) return false;

  // Raw terminal streams occasionally arrive as generic text before the
  // provider payload is classified. Keep those out of assistant markdown. The
  // patterns are intentionally execution-shaped (ANSI SGR, PowerShell table
  // headers, Codex bootstrap dumps, unified diffs/patches), not normal prose.
  if (/\x1b\[[0-9;?]*[ -/]*[@-~]/.test(text)) return true;
  if (/\[[0-9]{1,3}(?:;[0-9]{1,3})*m/.test(text)) return true;
  if (/^CODEX \((?:CORE|PEER)\)/im.test(trimmed)) return true;
  if (/^PWD:\s*$/im.test(trimmed) && /^ROOT:\s*$/im.test(trimmed)) return true;
  if (/^\s*(?:Mode|----)\s+.*(?:LastWriteTime|Length|Name|Path)/im.test(trimmed)) return true;
  if (/^\s*Directory:\s+/im.test(trimmed)) return true;
  if (/^(?:diff --git|--- a\/|\+\+\+ b\/|\*\*\* Begin Patch)/m.test(trimmed)) return true;
  if (/^Exit Code:\s*\d+/im.test(trimmed)) return true;
  if (/^warning:\s+in the working copy\b/im.test(trimmed)) return true;
  if (/\b(?:LF|CRLF) will be replaced by (?:CRLF|LF)\b/i.test(trimmed)) return true;
  if (/^(?:LF|CRLF) will be(?:\s|$)/i.test(trimmed)) return true;
  if (/^replaced by (?:CRLF|LF)\b/i.test(trimmed)) return true;
  if (/\bthe next time Git touches it\b/i.test(trimmed)) return true;
  if (/^\[warning\]\s*$/i.test(trimmed)) return true;

  return false;
}

export function isSystemStatusText(text: string): boolean {
  if (looksLikeExecutionLeakText(text)) return true;

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

const EXECUTION_LOG_FENCE_LANG_RE = /^(?:text|txt|log|logs|console|terminal|shell|sh|bash|zsh|powershell|ps1|cmd|bat)$/i;

function parseFenceLine(line: string): string | null {
  const match = line.trim().match(/^```([^\r\n`]*)$/);
  return match ? (match[1] ?? '') : null;
}

function isExecutionLogFenceInfo(info: string): boolean {
  const lang = info.trim().split(/\s+/)[0] ?? '';
  return !lang || EXECUTION_LOG_FENCE_LANG_RE.test(lang);
}

function lineLooksLikeRuntimeLeak(line: string): boolean {
  return Boolean(line.trim()) && isSystemStatusText(line);
}

export function stripSystemStatusLeakText(text: string): string {
  if (!text) return text;

  const lines = text.split(/\r?\n/);
  const kept: string[] = [];

  for (let i = 0; i < lines.length; i += 1) {
    const line = lines[i];
    const fenceInfo = parseFenceLine(line);

    if (fenceInfo !== null && isExecutionLogFenceInfo(fenceInfo)) {
      let closingIndex = -1;
      for (let j = i + 1; j < lines.length; j += 1) {
        if (lines[j].trim() === '```') {
          closingIndex = j;
          break;
        }
      }

      const bodyEnd = closingIndex === -1 ? lines.length : closingIndex;
      const bodyLines = lines.slice(i + 1, bodyEnd);
      const nonEmptyBodyLines = bodyLines.filter((bodyLine) => bodyLine.trim());
      const bodyIsOnlyRuntimeLeak =
        nonEmptyBodyLines.length > 0 &&
        nonEmptyBodyLines.every((bodyLine) => lineLooksLikeRuntimeLeak(bodyLine));

      if (bodyIsOnlyRuntimeLeak) {
        i = closingIndex === -1 ? lines.length : closingIndex;
        continue;
      }
    }

    if (lineLooksLikeRuntimeLeak(line)) continue;
    kept.push(line);
  }

  return kept
    .join('\n')
    // If a streamed terminal leak opened a text/log fence before the warning
    // line was filtered, don't leave an empty or dangling code block in the
    // assistant prose.
    .replace(
      /(^|\n)[ \t]*```(?:text|txt|log|logs|console|terminal|shell|sh|bash|zsh|powershell|ps1|cmd|bat)?[^\S\r\n]*(?:\n[ \t]*)?```[ \t]*(?=\n|$)/gi,
      '$1',
    )
    .replace(
      /(^|\n)[ \t]*```(?:text|txt|log|logs|console|terminal|shell|sh|bash|zsh|powershell|ps1|cmd|bat)?[^\S\r\n]*$/i,
      '$1',
    )
    .replace(/\n{3,}/g, '\n\n')
    .trim();
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

const COMMAND_ITEM_TYPES = new Set([
  'command_execution',
  'local_shell_call',
  'local_shell_call_output',
]);

const FILE_EDIT_ITEM_TYPES = new Set([
  'file_change',
  'diff_ready',
  'file_change_delta',
  'diff_delta',
  'patch_delta',
]);

const TERMINAL_OUTPUT_ITEM_TYPES = new Set([
  'terminal_output',
  'terminal_output_delta',
  'tool_output_delta',
  'command_output_delta',
  'shell_output_delta',
  'stdout_delta',
  'stderr_delta',
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

function normalizedProtocolKind(payload: any): string {
  return getProtocolKind(payload).toLowerCase().replace(/[\/_\-\s]+/g, '.');
}

function isCommandTool(tool: ToolDisplay): boolean {
  const name = String(tool.name || '').toLowerCase();
  return (
    name.includes('command') ||
    name.includes('execute') ||
    name.includes('shell') ||
    name.includes('run') ||
    COMMAND_ITEM_TYPES.has(String((tool as any).itemType || '').toLowerCase())
  );
}

function isEditTool(tool: ToolDisplay): boolean {
  const name = String(tool.name || '').toLowerCase();
  const output = typeof tool.output === 'string' ? tool.output : '';
  return (
    name.includes('edit') ||
    name.includes('diff') ||
    name.includes('patch') ||
    name.includes('file change') ||
    name.includes('changed file') ||
    output.startsWith('diff --git') ||
    output.includes('\n--- ') ||
    output.includes('\n+++ ')
  );
}

function hasFileEditShape(payload: any, item: any, itemType: string, protocol: string): boolean {
  if (FILE_EDIT_ITEM_TYPES.has(itemType)) return true;
  const diffLike = firstMeaningful(
    item?.diff,
    payload?.diff,
    item?.patch,
    payload?.patch,
    item?.changes,
    payload?.changes,
    item?.edits,
    payload?.edits,
  );
  if (diffLike === undefined) return false;
  const pathLike = firstPresent(item?.path, payload?.path, item?.file, payload?.file);
  const protocolLooksEdit =
    protocol.includes('file') ||
    protocol.includes('diff') ||
    protocol.includes('patch') ||
    protocol.includes('edit');
  return Boolean(pathLike || protocolLooksEdit);
}

function fileEditInput(payload: any, item: any): any {
  return firstPresent(
    item?.path,
    payload?.path,
    item?.file,
    payload?.file,
    item?.title,
    payload?.title,
    item?.input,
    payload?.input,
    item?.arguments,
    payload?.arguments,
    item?.args,
    payload?.args,
    item?.params,
    payload?.params,
  );
}

function fileEditOutput(payload: any, item: any): any {
  return firstPresent(
    item?.diff,
    payload?.diff,
    item?.patch,
    payload?.patch,
    item?.changes,
    payload?.changes,
    item?.edits,
    payload?.edits,
    item?.summary,
    payload?.summary,
  );
}

function terminalOutputText(payload: any, item: any, itemType: string): string | undefined {
  if (!TERMINAL_OUTPUT_ITEM_TYPES.has(itemType)) return undefined;
  const delta = firstPresent(item?.delta, payload?.delta);
  return asString(firstPresent(
    item?.line,
    payload?.line,
    item?.text,
    payload?.text,
    item?.output,
    payload?.output,
    typeof delta === 'object' ? delta?.text : delta,
  ));
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

  if (COMMAND_ITEM_TYPES.has(type)) return 'Execute Command';
  if (FILE_EDIT_ITEM_TYPES.has(type)) return 'Edit File';
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
  if (FILE_EDIT_ITEM_TYPES.has(type)) {
    const filePath = fileEditPathForTool({
      id: '',
      name,
      input: fileEditInput(payload, item),
      status: 'running',
      output: fileEditOutput(payload, item),
    });
    if (filePath) {
      return `${turnId}:${type}:file:${canonicalEditPathKey(filePath)}`;
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
      (!isEditTool(tool) &&
        !isEditTool(next) &&
        tool.name === next.name &&
        (tool.status === 'running' || tool.status === 'pending') &&
        !tool.output),
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

function isActiveTurnStatus(status?: Turn['status']): boolean {
  return status === 'pending' || status === 'running';
}

function isTerminalTurnStatus(status?: Turn['status']): boolean {
  return status === 'completed' || status === 'failed' || status === 'cancelled';
}

function settleToolStatus(status: ToolStatus, turnStatus?: Turn['status']): ToolStatus {
  if (!isTerminalTurnStatus(turnStatus)) return status;
  if (status !== 'pending' && status !== 'running') return status;
  if (turnStatus === 'failed') return 'failed';
  if (turnStatus === 'cancelled') return 'cancelled';
  return 'completed';
}

function formatToolData(value: any): string {
  if (value === undefined || value === null) return '';
  if (typeof value === 'string') return value.trim();
  try {
    return JSON.stringify(value, null, 2);
  } catch {
    return String(value);
  }
}

function collectDiffTextFragments(value: any, depth = 0, fragments: string[] = []): string[] {
  if (value === undefined || value === null || depth > 4) return fragments;

  if (typeof value === 'string') {
    if (
      value.includes('diff --git') ||
      value.includes('\n+++ ') ||
      value.includes('\n--- ') ||
      /^[+-][^+-]/m.test(value)
    ) {
      fragments.push(value);
    }
    return fragments;
  }

  if (Array.isArray(value)) {
    value.forEach((item) => collectDiffTextFragments(item, depth + 1, fragments));
    return fragments;
  }

  if (typeof value !== 'object') return fragments;

  [
    'diff',
    'patch',
    'content',
    'output',
    'result',
    'summary',
    'text',
    'changes',
    'edits',
    'files',
    'items',
    'data',
  ].forEach((key) => collectDiffTextFragments(value[key], depth + 1, fragments));

  return fragments;
}

function diffStatsFromText(value: any): { additions: number; deletions: number; lines: number } {
  const fragments = collectDiffTextFragments(value);
  const text = fragments.length > 0 ? fragments.join('\n') : formatToolData(value);
  let additions = 0;
  let deletions = 0;
  const lines = text ? text.split(/\r?\n/) : [];
  for (const line of lines) {
    if (line.startsWith('+++') || line.startsWith('---')) continue;
    if (line.startsWith('+')) additions += 1;
    if (line.startsWith('-')) deletions += 1;
  }
  return { additions, deletions, lines: lines.length };
}

function compactPathLabel(path: string): string {
  const normalized = path.replace(/\\/g, '/').trim();
  if (normalized.length <= 76) return normalized;
  const parts = normalized.split('/').filter(Boolean);
  if (parts.length <= 2) return `…${normalized.slice(-73)}`;
  const tail = parts.slice(-3).join('/');
  return `…/${tail}`;
}

function normalizeEditPath(path: string): string {
  return path
    .trim()
    .replace(/^["'`]+|["'`]+$/g, '')
    .replace(/^file:\/\//i, '')
    .replace(/\\/g, '/')
    .replace(/^(?:a|b)\//, '');
}

function canonicalEditPathKey(path: string): string {
  return normalizeEditPath(path)
    .replace(/^\.\/+/, '')
    .replace(/\/+/g, '/')
    .toLowerCase();
}

function isGenericEditPathLabel(path: string): boolean {
  const normalized = path.trim().toLowerCase();
  return [
    'edit',
    'edit file',
    'file edit',
    'file change',
    'file changes',
    'changed file',
    'changed files',
    'diff',
    'patch',
    'apply patch',
    'apply_patch',
    'tool call',
  ].includes(normalized);
}

function stripDiffPathMetadata(path: string): string {
  return path
    .split('\t')[0]
    .replace(/\s+\d{4}-\d{2}-\d{2}.*$/, '')
    .trim();
}

function pathFromUnifiedDiff(value: any): string | undefined {
  const text = typeof value === 'string' ? value : formatToolData(value);
  if (!text) return undefined;

  const gitMatch = text.match(/^diff --git\s+"?a\/(.+?)"?\s+"?b\/(.+?)"?\s*$/m);
  if (gitMatch) {
    return normalizeEditPath(stripDiffPathMetadata(gitMatch[2] || gitMatch[1]));
  }

  const plusMatch = text.match(/^\+\+\+\s+(.+)$/m);
  if (plusMatch) {
    const candidate = normalizeEditPath(stripDiffPathMetadata(plusMatch[1]));
    if (candidate && candidate !== '/dev/null') return candidate;
  }

  const minusMatch = text.match(/^---\s+(.+)$/m);
  if (minusMatch) {
    const candidate = normalizeEditPath(stripDiffPathMetadata(minusMatch[1]));
    if (candidate && candidate !== '/dev/null') return candidate;
  }

  return undefined;
}

function looksLikeEditPath(text: string): boolean {
  const normalized = normalizeEditPath(text);
  if (!normalized || normalized.length > 260) return false;
  if (isGenericEditPathLabel(normalized)) return false;
  if (/^https?:\/\//i.test(normalized) || normalized.includes('://')) return false;
  if (/^(?:diff --git|--- |\+\+\+ |\*\*\* Begin Patch)/m.test(normalized)) return false;
  if (/[\r\n{}[\]]/.test(normalized)) return false;

  const basename = activityPathBasename(normalized);
  if (!basename || basename === '.' || basename === '..') return false;
  if (DOTFILE_NAMES.has(basename.toLowerCase()) || EXTENSIONLESS_FILE_NAMES.has(basename.toLowerCase())) return true;
  if (looksLikeFilePath(normalized)) return true;
  if (/[\\/]/.test(normalized)) return true;
  return /^[^/\\\s]+\.[A-Za-z0-9][A-Za-z0-9_-]{0,16}$/.test(basename);
}

function pathCandidateFromText(value: string, requirePathShape = false): string | undefined {
  const diffPath = pathFromUnifiedDiff(value);
  if (diffPath) return diffPath;

  const candidate = normalizeEditPath(value);
  if (!candidate || candidate.length > 260 || /[\r\n]/.test(candidate)) return undefined;
  if (requirePathShape && !looksLikeEditPath(candidate)) return undefined;
  if (!looksLikeEditPath(candidate)) return undefined;
  return candidate;
}

function extractEditPathFromValue(value: any, depth = 0): string | undefined {
  if (value === undefined || value === null || depth > 4) return undefined;

  if (typeof value === 'string') {
    return pathCandidateFromText(value);
  }

  if (Array.isArray(value)) {
    for (const item of value) {
      const nested = extractEditPathFromValue(item, depth + 1);
      if (nested) return nested;
    }
    return undefined;
  }

  if (typeof value !== 'object') return undefined;

  const directPath = firstPresent(
    value.path,
    value.file,
    value.filename,
    value.file_name,
    value.filePath,
    value.file_path,
    value.filepath,
    value.relative_path,
    value.relativePath,
    value.absolute_path,
    value.absolutePath,
  );
  const directText = asString(directPath);
  if (directText) {
    const candidate = pathCandidateFromText(directText, true);
    if (candidate) return candidate;
  }

  const titleText = asString(firstPresent(value.title, value.name, value.label));
  if (titleText) {
    const candidate = pathCandidateFromText(titleText, true);
    if (candidate) return candidate;
  }

  const diffPath = pathFromUnifiedDiff(firstPresent(
    value.diff,
    value.patch,
    value.content,
    value.output,
    value.result,
    value.summary,
  ));
  if (diffPath) return diffPath;

  for (const key of ['input', 'arguments', 'args', 'params', 'changes', 'edits', 'files', 'items', 'data']) {
    const nested = extractEditPathFromValue(value[key], depth + 1);
    if (nested) return nested;
  }

  return undefined;
}

function fileEditPathForTool(tool: ToolDisplay): string | undefined {
  return extractEditPathFromValue(tool.input) || extractEditPathFromValue(tool.output);
}

function fileEditPathLabel(tool: ToolDisplay): string {
  const path = fileEditPathForTool(tool);
  if (path) return compactPathLabel(path);
  return tool.name || 'File change';
}


interface EditSummary {
  id: string;
  path: string;
  status: ToolStatus;
  additions: number;
  deletions: number;
  lines: number;
  created: boolean;
}

interface TurnExecutionState {
  turnEvents: any[];
  commandLine?: string;
  commandArgs: any;
  combinedTerminal: string[];
  toolCalls: ToolDisplay[];
  displayToolCalls: ToolDisplay[];
  editTools: ToolDisplay[];
  editSummaries: EditSummary[];
  actionableTools: ToolDisplay[];
  currentTurn?: Turn;
  turnStatus?: Turn['status'];
  turnIsActive: boolean;
  hasRunningTool: boolean;
  hasLiveTerminal: boolean;
  commandCount: number;
  editCount: number;
  totalAdditions: number;
  totalDeletions: number;
}

function isCreatedFileChange(value: any): boolean {
  const text = formatToolData(value);
  if (!text) return false;
  return (
    /^new file mode\b/m.test(text) ||
    /^---\s+\/dev\/null\s*$/m.test(text) ||
    /\n---\s+\/dev\/null(?:\s|$)/.test(text) ||
    /^create mode\b/im.test(text) ||
    /\bcreated\s+(?:file|path)\b/i.test(text)
  );
}

function mergeEditStatus(current: ToolStatus, next: ToolStatus): ToolStatus {
  if (current === 'failed' || next === 'failed') return 'failed';
  if (current === 'running' || next === 'running') return 'running';
  if (current === 'pending' || next === 'pending') return 'pending';
  if (current === 'cancelled' || next === 'cancelled') return 'cancelled';
  return next || current;
}

function editSummaryVerb(item: EditSummary): string {
  if (item.status === 'running' || item.status === 'pending') return '正在编辑';
  if (item.status === 'failed') return '编辑失败';
  if (item.status === 'cancelled') return '已取消';
  if (item.created) return '已创建';
  return '已编辑';
}

function editSummaryIndicator(item: EditSummary): string {
  if (item.status === 'failed') return '!';
  if (item.status === 'running' || item.status === 'pending') return '…';
  if (item.status === 'cancelled') return '×';
  return '›';
}

function aggregateEditSummaries(editTools: ToolDisplay[]): EditSummary[] {
  const summaries: EditSummary[] = [];
  const byKey = new Map<string, EditSummary>();

  editTools.forEach((tool, index) => {
    const fullPath = fileEditPathForTool(tool);
    const fallbackLabel = fileEditPathLabel(tool);
    const knownPath = fullPath && !isGenericEditPathLabel(fullPath) ? fullPath : undefined;
    const key = knownPath ? canonicalEditPathKey(knownPath) : `unknown:${tool.id}:${index}`;
    const stats = diffStatsFromText(tool.output);
    const nextSummary: EditSummary = {
      id: knownPath ? `edit:${key}` : `${tool.id}:${index}`,
      path: knownPath ? compactPathLabel(knownPath) : fallbackLabel,
      status: tool.status,
      created: isCreatedFileChange(tool.output),
      ...stats,
    };

    const existing = byKey.get(key);
    if (!existing) {
      byKey.set(key, nextSummary);
      summaries.push(nextSummary);
      return;
    }

    existing.status = mergeEditStatus(existing.status, nextSummary.status);
    existing.additions = Math.max(existing.additions, nextSummary.additions);
    existing.deletions = Math.max(existing.deletions, nextSummary.deletions);
    existing.lines = Math.max(existing.lines, nextSummary.lines);
    existing.created = existing.created || nextSummary.created;
    if (isGenericEditPathLabel(existing.path) && !isGenericEditPathLabel(nextSummary.path)) {
      existing.path = nextSummary.path;
    }
  });

  return summaries;
}

function collectTurnExecutionState(
  turnId: string,
  events: any[],
  turns: Turn[],
  realtimeLines?: string[],
  hyardJobs?: Record<string, any>,
  options: RenderTurnEventsOptions = {},
): TurnExecutionState {
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
      const itemType = getItemType(e.payload, item);
      return TERMINAL_OUTPUT_ITEM_TYPES.has(itemType);
    })
    .map((e) => {
      const item = getPayloadItem(e.payload);
      const itemType = getItemType(e.payload, item);
      return terminalOutputText(e.payload, item, itemType);
    })
    .filter((line): line is string => Boolean(line));

  let combinedTerminal = [...dbTerminalLines, ...(realtimeLines || [])];

  if (hyardJobs && hyardJobs[turnId]) {
    const job = hyardJobs[turnId];
    if (job.last_output_preview && combinedTerminal.length === 0) {
      combinedTerminal = String(job.last_output_preview).split('\n');
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
    const protocol = normalizedProtocolKind(payload);
    const itemType = type.toLowerCase();

    if (
      itemType === 'agent_message' ||
      itemType === 'assistant' ||
      TERMINAL_OUTPUT_ITEM_TYPES.has(itemType) ||
      itemType === 'execution_telemetry'
    ) {
      return;
    }

    const effectiveItemType = hasFileEditShape(payload, item, itemType, protocol)
      ? (FILE_EDIT_ITEM_TYPES.has(itemType) ? itemType : 'file_change')
      : itemType;
    const name = toolName(payload, item, effectiveItemType);
    const id = stableToolId(turnId, e, index, payload, item, effectiveItemType, name);

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
    } else if (COMMAND_ITEM_TYPES.has(itemType)) {
      mergeTool(toolCalls, {
        id,
        name,
        input: commandInput(payload, item),
        status: normalizeStatus(payload, item, e.event_type),
        output: toolOutput(payload, item),
      });
    } else if (FILE_EDIT_ITEM_TYPES.has(effectiveItemType) || hasFileEditShape(payload, item, itemType, protocol)) {
      mergeTool(toolCalls, {
        id,
        name,
        input: fileEditInput(payload, item),
        status: normalizeStatus(payload, item, e.event_type),
        output: fileEditOutput(payload, item),
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

  const currentTurn = turns.find((turn) => turn.turn_id === turnId);
  const turnStatus = currentTurn?.status;
  const turnIsActive = isActiveTurnStatus(turnStatus);
  const displayToolCalls = toolCalls.map((tool) => {
    const status = settleToolStatus(tool.status, turnStatus);
    return {
      ...tool,
      status,
      actions: turnIsActive && status === 'pending' ? tool.actions : undefined,
    };
  });
  const hasRunningTool = turnIsActive && displayToolCalls.some((tool) => tool.status === 'running' || tool.status === 'pending');
  const hasLiveTerminal = turnIsActive && (realtimeLines?.length ?? 0) > 0;
  const commandCount = (commandLine ? 1 : 0) + displayToolCalls.filter(isCommandTool).length;
  const editTools = displayToolCalls.filter(isEditTool);
  const editSummaries = aggregateEditSummaries(editTools);
  const editCount = editSummaries.length;
  const totalAdditions = editSummaries.reduce((sum, item) => sum + item.additions, 0);
  const totalDeletions = editSummaries.reduce((sum, item) => sum + item.deletions, 0);
  const actionableTools = displayToolCalls.filter((tool) => Array.isArray(tool.actions) && tool.actions.length > 0);

  return {
    turnEvents,
    commandLine,
    commandArgs,
    combinedTerminal,
    toolCalls,
    displayToolCalls,
    editTools,
    editSummaries,
    actionableTools,
    currentTurn,
    turnStatus,
    turnIsActive,
    hasRunningTool,
    hasLiveTerminal,
    commandCount,
    editCount,
    totalAdditions,
    totalDeletions,
  };
}

function stripAnsi(text: string): string {
  return text.replace(/\x1b\[[0-9;?]*[ -/]*[@-~]/g, '').replace(/\[[0-9]{1,3}(?:;[0-9]{1,3})*m/g, '');
}

function truncateActivityText(text: string, max = 180): string {
  const cleaned = stripAnsi(text).replace(/\s+/g, ' ').trim();
  return cleaned.length > max ? `${cleaned.slice(0, Math.max(0, max - 1))}…` : cleaned;
}

function latestMeaningfulTerminalLine(lines: string[]): string | null {
  for (let i = lines.length - 1; i >= 0; i -= 1) {
    const parts = String(lines[i] ?? '').split(/\r?\n/).reverse();
    for (const part of parts) {
      const cleaned = truncateActivityText(part);
      if (!cleaned) continue;
      if (/^CODEX \((?:CORE|PEER)\)$/i.test(cleaned)) continue;
      if (/^PWD:\s*$/i.test(cleaned) || /^ROOT:\s*$/i.test(cleaned)) continue;
      if (/^\[warning\]$/i.test(cleaned)) continue;
      return cleaned;
    }
  }
  return null;
}

function activityPathBasename(path: string): string {
  const text = path.replace(/\\/g, '/');
  const parts = text.split('/').filter(Boolean);
  return parts[parts.length - 1] || path;
}

function activeToolInputLabel(tool: ToolDisplay): string {
  const input = formatToolData(tool.input);
  if (input) return truncateActivityText(input, 120);
  return tool.name;
}

export function renderTurnActivitySummary(
  turnId: string,
  events: any[],
  turns: Turn[],
  realtimeLines?: string[],
  hyardJobs?: Record<string, any>,
  options: RenderTurnEventsOptions = {},
) {
  const state = collectTurnExecutionState(turnId, events, turns, realtimeLines, hyardJobs, options);
  const {
    commandLine,
    combinedTerminal,
    displayToolCalls,
    editSummaries,
    actionableTools,
    turnIsActive,
    hasRunningTool,
    hasLiveTerminal,
    commandCount,
    totalAdditions,
    totalDeletions,
  } = state;

  const completedEdits = editSummaries.filter((item) => item.status === 'completed');
  const createdCount = completedEdits.filter((item) => item.created).length;
  const editedCompletedCount = completedEdits.filter((item) => !item.created).length;
  const editingCount = editSummaries.filter((item) => item.status === 'running' || item.status === 'pending').length;
  const failedEditCount = editSummaries.filter((item) => item.status === 'failed').length;
  const latestTerminalLine = latestMeaningfulTerminalLine(combinedTerminal);
  const runningEdit = editSummaries.find((item) => item.status === 'running' || item.status === 'pending');
  const latestEdit = runningEdit ?? (turnIsActive ? editSummaries[editSummaries.length - 1] : undefined);
  const runningCommand = displayToolCalls.find((tool) => isCommandTool(tool) && (tool.status === 'running' || tool.status === 'pending'));
  const hasActivity = Boolean(commandLine || displayToolCalls.length > 0 || combinedTerminal.length > 0);
  const summaryParts = [
    createdCount > 0 ? `已创建 ${createdCount} 个文件` : null,
    editedCompletedCount > 0 ? `已编辑 ${editedCompletedCount} 个文件` : null,
    editingCount > 0 ? `正在编辑 ${editingCount} 个文件` : null,
    failedEditCount > 0 ? `${failedEditCount} 个文件编辑失败` : null,
    commandCount > 0 ? `已运行 ${commandCount} 条命令` : null,
    !hasActivity ? '正在连接 provider，等待工具事件…' : null,
  ].filter(Boolean);

  const showSpinner = turnIsActive || hasRunningTool || hasLiveTerminal;
  const detailToolCalls = displayToolCalls.filter((tool) => !actionableTools.includes(tool));

  return (
    <div
      className="message-body live-execution-activity"
      style={{
        display: 'flex',
        flexDirection: 'column',
        gap: 12,
        color: 'var(--text-secondary)',
      }}
    >
      <div
        style={{
          display: 'flex',
          alignItems: 'center',
          gap: 8,
          flexWrap: 'wrap',
          lineHeight: 1.55,
        }}
      >
        <span aria-hidden="true" style={{ color: 'var(--color-secondary)', fontWeight: 800 }}>✎</span>
        <span className="thinking-dots" style={{ color: hasActivity ? 'var(--text-primary)' : 'var(--text-secondary)', fontWeight: 700 }}>
          {summaryParts.join('  ')}
        </span>
        {(totalAdditions > 0 || totalDeletions > 0) && (
          <span style={{ fontFamily: 'monospace', fontSize: 12 }}>
            <span style={{ color: 'var(--color-success)' }}>+{totalAdditions.toLocaleString()}</span>{' '}
            <span style={{ color: 'var(--color-error)' }}>-{totalDeletions.toLocaleString()}</span>
          </span>
        )}
        {showSpinner && <span className="spinner-small" aria-label="运行中" />}
      </div>

      {latestTerminalLine && (
        <div
          style={{
            color: 'var(--text-secondary)',
            fontFamily: 'var(--font-mono, ui-monospace, SFMono-Regular, Menlo, Monaco, Consolas, monospace)',
            fontSize: 12,
            paddingLeft: 23,
            whiteSpace: 'nowrap',
            overflow: 'hidden',
            textOverflow: 'ellipsis',
          }}
          title={latestTerminalLine}
        >
          {latestTerminalLine}
        </div>
      )}

      {latestEdit && (
        <div
          style={{
            display: 'flex',
            alignItems: 'center',
            gap: 8,
            minWidth: 0,
            paddingLeft: 1,
            lineHeight: 1.45,
          }}
        >
          <span aria-hidden="true" style={{ color: 'var(--color-secondary)', fontWeight: 800 }}>✎</span>
          <span style={{ color: 'var(--text-secondary)' }}>{runningEdit ? '正在编辑' : '最近编辑'}</span>
          <span
            style={{
              minWidth: 0,
              overflow: 'hidden',
              textOverflow: 'ellipsis',
              whiteSpace: 'nowrap',
              color: 'var(--color-primary)',
              fontFamily: 'var(--font-mono, ui-monospace, SFMono-Regular, Menlo, Monaco, Consolas, monospace)',
              fontSize: 12,
            }}
            title={latestEdit.path}
          >
            {activityPathBasename(latestEdit.path)}
          </span>
          {(latestEdit.additions > 0 || latestEdit.deletions > 0) && (
            <span style={{ flex: '0 0 auto', fontFamily: 'monospace', fontSize: 12 }}>
              <span style={{ color: 'var(--color-success)' }}>+{latestEdit.additions}</span>{' '}
              <span style={{ color: 'var(--color-error)' }}>-{latestEdit.deletions}</span>
            </span>
          )}
        </div>
      )}

      {!latestEdit && runningCommand && (
        <div
          style={{
            display: 'flex',
            alignItems: 'center',
            gap: 8,
            minWidth: 0,
            paddingLeft: 1,
            lineHeight: 1.45,
          }}
        >
          <span aria-hidden="true" style={{ color: 'var(--color-secondary)', fontWeight: 800 }}>⌁</span>
          <span style={{ color: 'var(--text-secondary)' }}>正在运行</span>
          <span
            style={{
              minWidth: 0,
              overflow: 'hidden',
              textOverflow: 'ellipsis',
              whiteSpace: 'nowrap',
              color: 'var(--color-primary)',
              fontFamily: 'var(--font-mono, ui-monospace, SFMono-Regular, Menlo, Monaco, Consolas, monospace)',
              fontSize: 12,
            }}
            title={formatToolData(runningCommand.input) || runningCommand.name}
          >
            {activeToolInputLabel(runningCommand)}
          </span>
        </div>
      )}

      {actionableTools.length > 0 && (
        <div style={{ display: 'flex', flexDirection: 'column', gap: 8, paddingLeft: 23 }}>
          {actionableTools.map((tool, idx) => (
            <ToolCard key={tool.id || idx} tool={tool} />
          ))}
        </div>
      )}

      {(detailToolCalls.length > 0 || combinedTerminal.length > 0 || commandLine) && (
        <details style={{ marginLeft: 23, color: 'var(--text-muted)' }}>
          <summary style={{ cursor: 'pointer', userSelect: 'none', fontSize: 12, fontWeight: 700 }}>
            查看实时执行详情
          </summary>
          <div style={{ marginTop: 8, display: 'flex', flexDirection: 'column', gap: 8 }}>
            {commandLine && (
              <code style={{ display: 'block', padding: '6px 10px', background: 'rgba(0, 0, 0, 0.22)', borderRadius: 6, wordBreak: 'break-all', fontFamily: 'monospace', color: 'var(--text-secondary)' }}>
                {commandLine}{commandArgsSuffix(state.commandArgs)}
              </code>
            )}
            {detailToolCalls.slice(0, 5).map((tool, idx) => (
              <ToolCard key={tool.id || idx} tool={tool} />
            ))}
            {detailToolCalls.length > 5 && (
              <div style={{ fontSize: 12, color: 'var(--text-muted)' }}>还有 {detailToolCalls.length - 5} 项执行详情会在回合结束后折叠显示。</div>
            )}
            {combinedTerminal.length > 0 && (
              <pre style={{ margin: 0, padding: '8px 10px', background: '#0c0f1d', borderRadius: 6, color: '#38bdf8', fontFamily: 'monospace', fontSize: 11, maxHeight: 140, overflowY: 'auto', border: '1px solid #1e293b', whiteSpace: 'pre-wrap', wordBreak: 'break-word' }}>
                {combinedTerminal.slice(-80).join('\n')}
              </pre>
            )}
          </div>
        </details>
      )}
    </div>
  );
}

export function renderTurnEvents(
  turnId: string,
  events: any[],
  turns: Turn[],
  realtimeLines?: string[],
  hyardJobs?: Record<string, any>,
  options: RenderTurnEventsOptions = {},
) {
  const state = collectTurnExecutionState(turnId, events, turns, realtimeLines, hyardJobs, options);
  const {
    commandLine,
    commandArgs,
    combinedTerminal,
    displayToolCalls,
    editSummaries,
    actionableTools,
    hasRunningTool,
    hasLiveTerminal,
    commandCount,
    editCount,
    totalAdditions,
    totalDeletions,
  } = state;

  if (!commandLine && displayToolCalls.length === 0 && combinedTerminal.length === 0) {
    return null;
  }

  const visibleEditSummaries = editSummaries.slice(0, 4);
  const hiddenEditCount = Math.max(0, editSummaries.length - visibleEditSummaries.length);
  const executionItemCount = Math.max(0, displayToolCalls.length - state.editTools.length + editSummaries.length);
  const summaryParts = [
    editCount > 0 ? `已编辑 ${editCount} 个文件` : null,
    commandCount > 0 ? `已运行 ${commandCount} 条命令` : null,
  ].filter(Boolean);
  const summaryTitle = summaryParts.length > 0 ? summaryParts.join(' · ') : '执行详情与日志';
  const renderEditSummaryRow = (item: EditSummary) => (
    <div
      key={item.id}
      style={{
        display: 'grid',
        gridTemplateColumns: 'auto minmax(0, 1fr) auto auto',
        gap: 8,
        alignItems: 'center',
        color: 'var(--text-secondary)',
        padding: '2px 2px',
        minWidth: 0,
        lineHeight: 1.45,
      }}
    >
      <span
        style={{
          color: item.status === 'failed' ? 'var(--color-error)' : 'var(--text-secondary)',
          fontWeight: 700,
          whiteSpace: 'nowrap',
        }}
      >
        {editSummaryVerb(item)}
      </span>
      <span
        style={{
          minWidth: 0,
          overflow: 'hidden',
          textOverflow: 'ellipsis',
          whiteSpace: 'nowrap',
          color: 'var(--color-primary)',
          fontFamily: 'var(--font-mono, ui-monospace, SFMono-Regular, Menlo, Monaco, Consolas, monospace)',
          fontSize: 11,
        }}
        title={item.path}
      >
        {activityPathBasename(item.path)}
      </span>
      <span style={{ fontFamily: 'monospace', fontSize: 11, whiteSpace: 'nowrap' }}>
        <span style={{ color: 'var(--color-success)' }}>+{item.additions}</span>{' '}
        <span style={{ color: 'var(--color-error)' }}>-{item.deletions}</span>
      </span>
      <span
        aria-hidden="true"
        style={{
          color: item.status === 'failed' ? 'var(--color-error)' : 'var(--text-muted)',
          fontSize: 13,
          lineHeight: 1,
          opacity: 0.8,
        }}
      >
        {editSummaryIndicator(item)}
      </span>
    </div>
  );

  return (
    <div
      className="execution-details-accordion"
      style={{
        marginTop: '10px',
        fontSize: '12px',
        background: 'rgba(0,0,0,0.15)',
        borderRadius: '8px',
        border: '1px solid var(--border-muted)',
        overflow: 'hidden',
      }}
    >
      <div
        style={{
          padding: '9px 12px',
          display: 'flex',
          alignItems: 'center',
          gap: '8px',
          background: 'rgba(255,255,255,0.025)',
          borderBottom: editSummaries.length > 0 || actionableTools.length > 0 ? '1px solid rgba(255,255,255,0.05)' : 'none',
        }}
      >
        <Terminal size={14} style={{ color: 'var(--color-secondary)', flex: '0 0 auto' }} />
        <div style={{ minWidth: 0, flex: 1, display: 'flex', alignItems: 'center', gap: 8, flexWrap: 'wrap' }}>
          <span style={{ color: 'var(--text-primary)', fontWeight: 800 }}>{summaryTitle}</span>
          {(totalAdditions > 0 || totalDeletions > 0) && (
            <span style={{ color: 'var(--text-muted)', fontFamily: 'monospace', fontSize: 11 }}>
              <span style={{ color: 'var(--color-success)' }}>+{totalAdditions.toLocaleString()}</span>{' '}
              <span style={{ color: 'var(--color-error)' }}>-{totalDeletions.toLocaleString()}</span>
            </span>
          )}
        </div>
        <div style={{ display: 'flex', gap: 6, alignItems: 'center', flex: '0 0 auto' }}>
          {hasRunningTool || hasLiveTerminal ? (
            <span style={{ fontSize: '11px', padding: '2px 7px', background: 'rgba(245, 158, 11, 0.1)', color: 'var(--color-warning)', borderRadius: '999px', fontWeight: 700 }}>
              运行中
            </span>
          ) : null}
          {executionItemCount > 0 && (
            <span style={{ fontSize: '11px', padding: '2px 7px', background: 'rgba(6, 182, 212, 0.1)', color: 'var(--color-secondary)', borderRadius: '999px' }}>
              {executionItemCount} 项
            </span>
          )}
          {combinedTerminal.length > 0 && (
            <span style={{ fontSize: '11px', padding: '2px 7px', background: 'rgba(59, 130, 246, 0.1)', color: 'var(--color-primary)', borderRadius: '999px' }}>
              {combinedTerminal.length} 日志
            </span>
          )}
          <span style={{ fontSize: '11px', padding: '4px 8px', border: '1px solid var(--border-muted)', color: 'var(--text-muted)', borderRadius: '6px', opacity: 0.6 }}>
            撤销
          </span>
        </div>
      </div>

      {editSummaries.length > 0 && (
        <div style={{ padding: '7px 12px 9px', display: 'flex', flexDirection: 'column', gap: 4 }}>
          {visibleEditSummaries.map(renderEditSummaryRow)}
          {hiddenEditCount > 0 && (
            <details style={{ marginTop: 2 }}>
              <summary
                style={{
                  color: 'var(--text-muted)',
                  fontSize: 11,
                  padding: '3px 2px',
                  cursor: 'pointer',
                  userSelect: 'none',
                }}
              >
                再显示 {hiddenEditCount} 个文件
              </summary>
              <div style={{ display: 'flex', flexDirection: 'column', gap: 4, marginTop: 4 }}>
                {editSummaries.slice(4).map(renderEditSummaryRow)}
              </div>
            </details>
          )}
        </div>
      )}

      {actionableTools.length > 0 && (
        <div style={{ padding: '8px 12px', borderTop: '1px solid rgba(255,255,255,0.05)' }}>
          {actionableTools.map((tool, idx) => (
            <ToolCard key={tool.id || idx} tool={tool} />
          ))}
        </div>
      )}

      <details style={{ borderTop: '1px solid rgba(255,255,255,0.06)' }}>
        <summary
          style={{
            padding: '8px 12px',
            cursor: 'pointer',
            color: 'var(--text-secondary)',
            fontWeight: 700,
            userSelect: 'none',
            background: 'rgba(255,255,255,0.015)',
          }}
        >
          审核执行详情
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

          {displayToolCalls.length > 0 && (
            <div>
              <div style={{ fontWeight: 'bold', color: 'var(--text-secondary)', marginBottom: '6px' }}>Tool Executions:</div>
              <div style={{ display: 'flex', flexDirection: 'column', gap: '6px' }}>
                {displayToolCalls.map((tc, idx) => (
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
