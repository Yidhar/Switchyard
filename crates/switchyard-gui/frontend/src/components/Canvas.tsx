import React, { useEffect, useMemo } from 'react';
import {
  X,
  Copy,
  Maximize2,
  FileText,
  AlertCircle,
  RefreshCw,
  Save,
  GitCompare,
  Code2,
  Check as CheckIcon,
  Trash2,
} from 'lucide-react';
import CodeMirror from '@uiw/react-codemirror';
import { oneDark } from '@codemirror/theme-one-dark';
import { EditorView } from '@codemirror/view';
import { languageExtensionFor } from './codeMirrorLanguages';

/// Snapshot returned by the `read_file` Tauri command. Mirrors the
/// Rust `FileSnapshot` struct in switchyard-gui/src/main.rs.
export interface FileSnapshot {
  path: string;
  content: string;
  is_binary: boolean;
  size: number;
  language: string;
}

/// Canvas mode per tab.
/// - `edit` — full CodeMirror editor with line numbers + syntax highlighting.
///   The default and only "live" mode. Saving writes back to disk.
/// - `diff` — unified line-by-line diff between `ai_before_content` and
///   the current on-disk content. Toggled in when an AI capture lands.
export type CanvasMode = 'edit' | 'diff';

/// A single open tab in the Canvas. The cached snapshot lets us switch
/// tabs without re-reading from disk; explicit refresh re-fetches.
export interface CanvasTab {
  id: string;
  path: string;
  snapshot: FileSnapshot | null;
  error: string | null;
  reloading: boolean;
  mode: CanvasMode;
  /// Editor buffer. `null` means "fall back to snapshot.content" so we
  /// don't double-store the initial value.
  draft: string | null;
  dirty: boolean;
  saving: boolean;
  /// Snapshot of this file's content immediately BEFORE the AI's most
  /// recent modification. When non-null, Canvas surfaces Diff mode +
  /// Revert/Dismiss controls. Set by the TurnCompleted-driven artifact
  /// intake in App.tsx.
  ai_before_content: string | null;
}

interface CanvasProps {
  tabs: CanvasTab[];
  activeTabId: string | null;
  onSelectTab: (tabId: string) => void;
  onCloseTab: (tabId: string) => void;
  onReloadTab: (tabId: string) => void;
  /// Toggle between edit and diff modes. App.tsx guards diff against
  /// missing `ai_before_content`.
  onToggleMode: (tabId: string, mode?: CanvasMode) => void;
  /// Update the editor buffer for `tabId`.
  onDraftChange: (tabId: string, draft: string) => void;
  /// Persist the current draft via `write_file`.
  onSave: (tabId: string) => void;
  /// Revert this file to the pre-AI snapshot stored in
  /// `ai_before_content`. Writes the before-state back to disk and
  /// clears `ai_before_content` so the diff disappears.
  onRevertAiChange: (tabId: string) => void;
  /// Dismiss the diff without writing — just clears
  /// `ai_before_content` so the next AI change can be tracked fresh.
  onDismissAiChange: (tabId: string) => void;
}

/// Right-side multi-tab file editor. CodeMirror-backed so users get
/// line numbers + syntax highlighting + bracket matching by default;
/// the AI diff view is a separate mode toggleable when an
/// `ai_before_content` capture lands for the tab. The panel collapses
/// to zero width when no tabs are open.
export const Canvas: React.FC<CanvasProps> = ({
  tabs,
  activeTabId,
  onSelectTab,
  onCloseTab,
  onReloadTab,
  onToggleMode,
  onDraftChange,
  onSave,
  onRevertAiChange,
  onDismissAiChange,
}) => {
  if (tabs.length === 0) return null;

  const activeTab = tabs.find((t) => t.id === activeTabId) ?? tabs[0];

  // Ctrl/Cmd+S → save the active tab when it's dirty + not already
  // saving. Bound at the Canvas level so the shortcut works as long as
  // the user's focus is anywhere inside the panel.
  useEffect(() => {
    const handler = (e: KeyboardEvent) => {
      if ((e.ctrlKey || e.metaKey) && e.key === 's' && !e.shiftKey) {
        if (
          activeTab &&
          activeTab.mode === 'edit' &&
          activeTab.dirty &&
          !activeTab.saving
        ) {
          e.preventDefault();
          onSave(activeTab.id);
        }
      }
    };
    window.addEventListener('keydown', handler);
    return () => window.removeEventListener('keydown', handler);
  }, [activeTab, onSave]);

  return (
    <div
      className="canvas-panel"
      style={{
        display: 'flex',
        flexDirection: 'column',
        height: '100%',
        background: 'rgba(0, 0, 0, 0.25)',
        borderLeft: '1px solid var(--border-muted)',
        overflow: 'hidden',
      }}
    >
      {/* Tab bar — fixed 32px height. Together with the file-header
          below (also 32px), the Canvas top region is exactly 64px so
          its bottom border lines up with the chat column's header
          across the full window width. */}
      <div
        className="canvas-tabs"
        style={{
          display: 'flex',
          alignItems: 'stretch',
          background: 'rgba(255, 255, 255, 0.015)',
          borderBottom: '1px solid var(--border-muted)',
          height: 32,
          overflowX: 'auto',
          flexShrink: 0,
        }}
      >
        {tabs.map((tab) => {
          const isActive = tab.id === activeTab.id;
          return (
            <div
              key={tab.id}
              onClick={() => onSelectTab(tab.id)}
              style={{
                display: 'inline-flex',
                alignItems: 'center',
                gap: 6,
                padding: '6px 10px',
                borderRight: '1px solid var(--border-muted)',
                background: isActive ? 'rgba(0, 0, 0, 0.35)' : 'transparent',
                color: isActive ? 'var(--text-primary)' : 'var(--text-secondary)',
                cursor: 'pointer',
                fontSize: 12,
                whiteSpace: 'nowrap',
                position: 'relative',
              }}
              title={tab.path}
            >
              {isActive && (
                <span
                  aria-hidden
                  style={{
                    position: 'absolute',
                    left: 0,
                    right: 0,
                    bottom: 0,
                    height: 2,
                    background: 'var(--color-primary)',
                  }}
                />
              )}
              <FileText size={12} />
              <span>
                {leafName(tab.path)}
                {tab.dirty && (
                  <span
                    aria-label="Unsaved changes"
                    title="Unsaved changes"
                    style={{
                      display: 'inline-block',
                      marginLeft: 6,
                      width: 6,
                      height: 6,
                      borderRadius: '50%',
                      background: 'var(--color-primary)',
                      verticalAlign: 'middle',
                    }}
                  />
                )}
              </span>
              <button
                type="button"
                onClick={(e) => {
                  e.stopPropagation();
                  onCloseTab(tab.id);
                }}
                style={{
                  background: 'none',
                  border: 'none',
                  color: 'var(--text-muted)',
                  cursor: 'pointer',
                  padding: 0,
                  display: 'inline-flex',
                  alignItems: 'center',
                }}
                title="Close tab"
              >
                <X size={12} />
              </button>
            </div>
          );
        })}
      </div>

      {/* File header: full path + actions. Mode toggle only appears
          when an AI change is captured — otherwise the editor is the
          only sensible view and the toggle is just noise. Fixed 32px
          height so [tabs + this] = 64px and aligns with .chat-header. */}
      <div
        className="canvas-file-header"
        style={{
          display: 'flex',
          alignItems: 'center',
          gap: 8,
          padding: '0 14px',
          height: 32,
          borderBottom: '1px solid var(--border-muted)',
          fontSize: 12,
          color: 'var(--text-secondary)',
          flexShrink: 0,
          boxSizing: 'border-box',
        }}
      >
        <FileText size={14} color="var(--text-muted)" />
        <span
          style={{
            flex: 1,
            whiteSpace: 'nowrap',
            overflow: 'hidden',
            textOverflow: 'ellipsis',
            direction: 'rtl',
            textAlign: 'left',
          }}
          title={activeTab.path}
        >
          {activeTab.path}
        </span>

        {/* Save lives next to the path so it's always reachable. The
            keyboard shortcut Ctrl/Cmd+S is the primary path. */}
        <button
          type="button"
          onClick={() => onSave(activeTab.id)}
          disabled={
            !activeTab.dirty || activeTab.saving || !activeTab.snapshot
          }
          title="Save (Ctrl/⌘+S)"
          style={{
            ...iconBtnStyle,
            color: activeTab.dirty
              ? 'var(--color-primary)'
              : 'var(--text-muted)',
            opacity: activeTab.dirty && !activeTab.saving ? 1 : 0.4,
            cursor:
              activeTab.dirty && !activeTab.saving
                ? 'pointer'
                : 'not-allowed',
          }}
        >
          <Save size={13} className={activeTab.saving ? 'spin' : ''} />
        </button>

        {/* Diff toggle — appears only when an AI change is captured.
            Edit is the other half of the toggle; without a capture
            there's nothing to compare against. */}
        {activeTab.ai_before_content !== null && (
          <>
            <ModeButton
              active={activeTab.mode === 'edit'}
              onClick={() => onToggleMode(activeTab.id, 'edit')}
              title="Editor"
            >
              <Code2 size={13} />
            </ModeButton>
            <ModeButton
              active={activeTab.mode === 'diff'}
              onClick={() => onToggleMode(activeTab.id, 'diff')}
              title="Unified diff of the AI's last modification to this file"
            >
              <GitCompare size={13} />
            </ModeButton>
            <button
              type="button"
              onClick={() => onRevertAiChange(activeTab.id)}
              disabled={activeTab.saving}
              title="Revert this file to its content before the AI's last edit"
              style={{
                ...iconBtnStyle,
                color: 'var(--color-error, #ef4444)',
                opacity: activeTab.saving ? 0.5 : 1,
              }}
            >
              <Trash2 size={13} />
            </button>
            <button
              type="button"
              onClick={() => onDismissAiChange(activeTab.id)}
              title="Dismiss the diff (keep AI's change; just hide the comparison)"
              style={{
                ...iconBtnStyle,
                color: 'var(--text-muted)',
              }}
            >
              <CheckIcon size={13} />
            </button>
          </>
        )}

        <button
          type="button"
          onClick={() => onReloadTab(activeTab.id)}
          title="Reload from disk"
          style={iconBtnStyle}
        >
          <RefreshCw size={13} className={activeTab.reloading ? 'spin' : ''} />
        </button>
        <button
          type="button"
          onClick={() => {
            const text =
              activeTab.draft ?? activeTab.snapshot?.content ?? '';
            navigator.clipboard.writeText(text).catch(() => {});
          }}
          title="Copy contents"
          style={iconBtnStyle}
        >
          <Copy size={13} />
        </button>
        <button
          type="button"
          onClick={() => {
            // TODO: open this tab as a fullscreen overlay.
          }}
          title="Maximize (later)"
          style={iconBtnStyle}
        >
          <Maximize2 size={13} />
        </button>
      </div>

      {/* Body — CodeMirror editor or diff viewer */}
      <div
        className="canvas-body"
        style={{
          flex: 1,
          minHeight: 0,
          overflow: 'hidden',
          background: 'rgba(0, 0, 0, 0.35)',
          display: 'flex',
          flexDirection: 'column',
        }}
      >
        <CanvasBody tab={activeTab} onDraftChange={onDraftChange} />
      </div>

      {/* Status bar */}
      <div
        className="canvas-status"
        style={{
          display: 'flex',
          alignItems: 'center',
          gap: 12,
          padding: '4px 14px',
          borderTop: '1px solid var(--border-muted)',
          background: 'rgba(255, 255, 255, 0.015)',
          fontSize: 11,
          color: 'var(--text-muted)',
          flexShrink: 0,
        }}
      >
        <span>{activeTab.snapshot?.language ?? 'plaintext'}</span>
        <span>UTF-8</span>
        {activeTab.snapshot && (
          <span>{formatBytes(activeTab.snapshot.size)}</span>
        )}
        <div style={{ flex: 1 }} />
        <span
          style={{
            color: activeTab.dirty
              ? 'var(--color-primary)'
              : 'var(--text-secondary)',
          }}
        >
          {activeTab.mode === 'diff'
            ? 'AI diff'
            : activeTab.saving
              ? 'Saving…'
              : activeTab.dirty
                ? 'Edit · unsaved'
                : 'Edit'}
        </span>
      </div>
    </div>
  );
};

const iconBtnStyle: React.CSSProperties = {
  background: 'transparent',
  border: '1px solid transparent',
  color: 'var(--text-muted)',
  cursor: 'pointer',
  padding: '3px 4px',
  borderRadius: 3,
  display: 'inline-flex',
  alignItems: 'center',
};

const ModeButton: React.FC<{
  active: boolean;
  onClick: () => void;
  title: string;
  children: React.ReactNode;
}> = ({ active, onClick, title, children }) => (
  <button
    type="button"
    onClick={onClick}
    title={title}
    style={{
      ...iconBtnStyle,
      color: active ? 'var(--color-primary)' : 'var(--text-muted)',
      background: active ? 'rgba(99, 102, 241, 0.12)' : 'transparent',
    }}
  >
    {children}
  </button>
);

const CanvasBody: React.FC<{
  tab: CanvasTab;
  onDraftChange: (tabId: string, draft: string) => void;
}> = ({ tab, onDraftChange }) => {
  if (tab.error) {
    return (
      <div
        style={{
          padding: 24,
          color: 'var(--color-error, #ef4444)',
          display: 'flex',
          alignItems: 'flex-start',
          gap: 10,
        }}
      >
        <AlertCircle size={18} />
        <div>
          <strong style={{ display: 'block', marginBottom: 4 }}>
            Failed to read file
          </strong>
          <span style={{ color: 'var(--text-secondary)', whiteSpace: 'pre-wrap' }}>
            {tab.error}
          </span>
        </div>
      </div>
    );
  }
  if (!tab.snapshot) {
    return (
      <div style={{ padding: 24, color: 'var(--text-muted)' }}>Loading…</div>
    );
  }
  if (tab.snapshot.is_binary) {
    return (
      <div style={{ padding: 24, color: 'var(--text-muted)' }}>
        Binary file ({formatBytes(tab.snapshot.size)}). The Canvas only
        handles UTF-8 text.
      </div>
    );
  }

  // Diff: unified line-by-line comparison of the AI's pre-state
  // (`ai_before_content`) against the current on-disk content.
  if (tab.mode === 'diff' && tab.ai_before_content !== null) {
    return (
      <DiffView before={tab.ai_before_content} after={tab.snapshot.content} />
    );
  }

  // Editor mode (default). CodeMirror with line numbers, syntax
  // highlighting via the workspace's language hint, and the One Dark
  // theme. We key the editor by tab.id so switching tabs gives each
  // file its own undo history instead of bleeding state across tabs.
  const value = tab.draft ?? tab.snapshot.content;
  return (
    <CodeMirror
      key={tab.id}
      value={value}
      onChange={(next) => onDraftChange(tab.id, next)}
      theme={oneDark}
      extensions={[
        ...languageExtensionFor(tab.snapshot.language),
        EditorView.lineWrapping,
      ]}
      basicSetup={{
        lineNumbers: true,
        highlightActiveLine: true,
        highlightActiveLineGutter: true,
        bracketMatching: true,
        closeBrackets: true,
        autocompletion: true,
        foldGutter: true,
        indentOnInput: true,
      }}
      style={{
        flex: 1,
        minHeight: 0,
        height: '100%',
        fontSize: 13,
      }}
      height="100%"
    />
  );
};

/// One row in the line-by-line diff view.
type DiffRow = { kind: 'equal' | 'remove' | 'add'; text: string };

const MAX_INLINE_DIFF_CELLS = 350_000;

/// LCS-based line diff. O(m*n); guarded so opening a large file in the Canvas
/// does not freeze the GUI render thread. The right follow-up is a workerized
/// or Monaco-style virtual diff, but this keeps the current inline view safe.
function computeDiff(before: string, after: string): DiffRow[] | null {
  const a = before.split('\n');
  const b = after.split('\n');
  const m = a.length;
  const n = b.length;
  if (m * n > MAX_INLINE_DIFF_CELLS) {
    return null;
  }
  const lcs: number[][] = Array.from({ length: m + 1 }, () =>
    new Array(n + 1).fill(0),
  );
  for (let i = 1; i <= m; i++) {
    for (let j = 1; j <= n; j++) {
      lcs[i][j] =
        a[i - 1] === b[j - 1]
          ? lcs[i - 1][j - 1] + 1
          : Math.max(lcs[i - 1][j], lcs[i][j - 1]);
    }
  }
  const rows: DiffRow[] = [];
  let i = m;
  let j = n;
  while (i > 0 && j > 0) {
    if (a[i - 1] === b[j - 1]) {
      rows.push({ kind: 'equal', text: a[i - 1] });
      i--;
      j--;
    } else if (lcs[i - 1][j] >= lcs[i][j - 1]) {
      rows.push({ kind: 'remove', text: a[i - 1] });
      i--;
    } else {
      rows.push({ kind: 'add', text: b[j - 1] });
      j--;
    }
  }
  while (i > 0) rows.push({ kind: 'remove', text: a[--i] });
  while (j > 0) rows.push({ kind: 'add', text: b[--j] });
  return rows.reverse();
}

/// Unified line diff renderer. Red lines were on disk, green are the
/// AI's edit, plain lines are unchanged. Line numbers on both sides
/// (git-style) so the user can correlate hunks back to the file.
const DiffView: React.FC<{ before: string; after: string }> = ({ before, after }) => {
  const rows = useMemo(() => computeDiff(before, after), [before, after]);
  if (!rows) {
    const beforeLines = before.split('\n').length;
    const afterLines = after.split('\n').length;
    return (
      <div style={{ padding: 24, color: 'var(--text-muted)', lineHeight: 1.6 }}>
        Diff is too large to render inline without blocking the GUI
        {' '}
        ({beforeLines.toLocaleString()} → {afterLines.toLocaleString()} lines).
        Use the editor view or an external diff tool for this file.
      </div>
    );
  }
  if (rows.every((r) => r.kind === 'equal')) {
    return (
      <div style={{ padding: 24, color: 'var(--text-muted)' }}>
        No changes between baseline and current content.
      </div>
    );
  }
  let beforeLine = 0;
  let afterLine = 0;
  return (
    <div
      style={{
        flex: 1,
        minHeight: 0,
        overflow: 'auto',
        fontFamily:
          'ui-monospace, SFMono-Regular, Menlo, Consolas, "Liberation Mono", monospace',
        fontSize: 13,
        lineHeight: 1.5,
      }}
    >
      <pre
        style={{
          margin: 0,
          padding: '12px 0',
          whiteSpace: 'pre',
        }}
      >
        {rows.map((row, idx) => {
          const isRemove = row.kind === 'remove';
          const isAdd = row.kind === 'add';
          let leftNum: string | number = ' ';
          let rightNum: string | number = ' ';
          if (isRemove) {
            beforeLine += 1;
            leftNum = beforeLine;
          } else if (isAdd) {
            afterLine += 1;
            rightNum = afterLine;
          } else {
            beforeLine += 1;
            afterLine += 1;
            leftNum = beforeLine;
            rightNum = afterLine;
          }
          return (
            <div
              key={idx}
              style={{
                display: 'grid',
                gridTemplateColumns: '3.5em 3.5em 1em 1fr',
                columnGap: 0,
                background: isRemove
                  ? 'rgba(239, 68, 68, 0.10)'
                  : isAdd
                    ? 'rgba(16, 185, 129, 0.10)'
                    : 'transparent',
                color: isRemove
                  ? '#fca5a5'
                  : isAdd
                    ? '#86efac'
                    : 'var(--text-primary)',
                minHeight: '1.5em',
              }}
            >
              <span
                aria-hidden
                style={{
                  textAlign: 'right',
                  paddingRight: 8,
                  color: 'var(--text-muted)',
                  userSelect: 'none',
                  opacity: 0.7,
                }}
              >
                {leftNum}
              </span>
              <span
                aria-hidden
                style={{
                  textAlign: 'right',
                  paddingRight: 8,
                  color: 'var(--text-muted)',
                  userSelect: 'none',
                  opacity: 0.7,
                }}
              >
                {rightNum}
              </span>
              <span
                aria-hidden
                style={{
                  textAlign: 'center',
                  color: 'var(--text-muted)',
                  userSelect: 'none',
                  opacity: 0.7,
                }}
              >
                {isRemove ? '-' : isAdd ? '+' : ' '}
              </span>
              <span style={{ paddingLeft: 8, paddingRight: 14 }}>
                {row.text || ' '}
              </span>
            </div>
          );
        })}
      </pre>
    </div>
  );
};

function leafName(p: string): string {
  if (!p) return '';
  // Handle both / and \ — workspace paths can come back either way on
  // Windows depending on how the resolve / strip_prefix interplay went.
  const norm = p.replace(/\\/g, '/');
  const idx = norm.lastIndexOf('/');
  return idx >= 0 ? norm.slice(idx + 1) : norm;
}

function formatBytes(n: number): string {
  if (n < 1024) return `${n} B`;
  if (n < 1024 * 1024) return `${(n / 1024).toFixed(1)} KB`;
  return `${(n / (1024 * 1024)).toFixed(1)} MB`;
}

export default Canvas;

// Tiny spin animation if the host CSS doesn't already define one.
const styleEl =
  typeof document !== 'undefined'
    ? document.querySelector('style[data-switchyard-canvas-spin]')
    : null;
if (typeof document !== 'undefined' && !styleEl) {
  const s = document.createElement('style');
  s.setAttribute('data-switchyard-canvas-spin', 'true');
  s.textContent = `.spin { animation: switchyard-canvas-spin 0.8s linear infinite; } @keyframes switchyard-canvas-spin { to { transform: rotate(360deg); } }`;
  document.head.appendChild(s);
}
