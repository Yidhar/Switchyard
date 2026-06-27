import React, {
  useCallback,
  useEffect,
  useMemo,
  useRef,
  useState,
} from 'react';
import {
  X,
  Copy,
  ChevronRight,
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
import type { Extension } from '@codemirror/state';
import { EditorView, type ViewUpdate } from '@codemirror/view';
import { loadLanguageExtensionsFor } from './codeMirrorLanguages';

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
  /// 1-based line to scroll to + select on open (e.g. from a search hit).
  /// Transient — bumped each time a "go to match" requests this file.
  gotoLine?: number | null;
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
  const breadcrumbSegments = useMemo(
    () => canvasBreadcrumbSegments(activeTab.path),
    [activeTab.path],
  );

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

      {/* Breadcrumb/action bar — fixed 32px. VS Code keeps the file name
          in the tab and uses this row for path context + editor actions,
          so we avoid showing a second large duplicate file title. */}
      <div className="canvas-breadcrumb-bar">
        <div className="canvas-breadcrumbs" title={activeTab.path}>
          {breadcrumbSegments.length > 1 ? (
            breadcrumbSegments.map((segment, index) => (
              <React.Fragment key={`${segment}-${index}`}>
                {index > 0 && (
                  <ChevronRight
                    size={13}
                    className="canvas-breadcrumb-separator"
                    aria-hidden
                  />
                )}
                <span
                  className={`canvas-breadcrumb-segment ${
                    index === breadcrumbSegments.length - 1 ? 'is-current' : ''
                  }`}
                >
                  {segment}
                </span>
              </React.Fragment>
            ))
          ) : (
            <span className="canvas-breadcrumb-placeholder">./</span>
          )}
        </div>

        <div className="canvas-editor-actions">
          {/* Save lives in the editor action cluster so it's always reachable.
              The keyboard shortcut Ctrl/Cmd+S is still the primary path. */}
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
        </div>
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
  const language = tab.snapshot?.language;
  const [languageExtensions, setLanguageExtensions] = useState<Extension[]>([]);

  useEffect(() => {
    let cancelled = false;
    if (!language) {
      setLanguageExtensions([]);
      return;
    }
    loadLanguageExtensionsFor(language).then((extensions) => {
      if (!cancelled) {
        setLanguageExtensions(extensions);
      }
    });
    return () => {
      cancelled = true;
    };
  }, [language]);

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
    <CanvasEditor
      key={tab.id}
      tabId={tab.id}
      value={value}
      gotoLine={tab.gotoLine}
      languageExtensions={languageExtensions}
      onDraftChange={onDraftChange}
    />
  );
};

const MAX_MINIMAP_LINES = 1600;
const MINIMAP_VERTICAL_PADDING = 6;
const MINIMAP_SHORT_FILE_LINE_HEIGHT = 2;
const MINIMAP_SHORT_FILE_LINE_GAP = 1;
const MINIMAP_MIN_VIEWPORT_HEIGHT = 10;

const vscodeEditorTheme = EditorView.theme({
  '&': {
    height: '100%',
  },
  '.cm-scroller': {
    overflow: 'auto',
    scrollbarColor: 'rgba(148, 163, 184, 0.58) transparent',
    scrollbarWidth: 'thin',
  },
  '.cm-scroller::-webkit-scrollbar': {
    width: '12px',
    height: '12px',
  },
  '.cm-scroller::-webkit-scrollbar-track': {
    background: 'rgba(255, 255, 255, 0.015)',
  },
  '.cm-scroller::-webkit-scrollbar-thumb': {
    background: 'rgba(148, 163, 184, 0.38)',
    borderRadius: '999px',
    border: '3px solid rgba(0, 0, 0, 0.35)',
  },
  '.cm-scroller::-webkit-scrollbar-thumb:hover': {
    background: 'rgba(148, 163, 184, 0.64)',
  },
  '.cm-content': {
    fontFamily: 'var(--font-mono)',
    minWidth: 'max-content',
  },
  '.cm-line': {
    whiteSpace: 'pre',
  },
  '.cm-gutters': {
    userSelect: 'none',
  },
});

interface CanvasEditorProps {
  tabId: string;
  value: string;
  gotoLine?: number | null;
  languageExtensions: Extension[];
  onDraftChange: (tabId: string, draft: string) => void;
}

const CanvasEditor: React.FC<CanvasEditorProps> = ({
  tabId,
  value,
  gotoLine,
  languageExtensions,
  onDraftChange,
}) => {
  const viewRef = useRef<EditorView | null>(null);
  const scrollElementRef = useRef<HTMLElement | null>(null);
  const scrollHandlerRef = useRef<(() => void) | null>(null);
  const scrollRafRef = useRef<number | null>(null);
  const minimapRef = useRef<HTMLDivElement>(null);
  const [navState, setNavState] = useState(() => ({
    scrollTop: 0,
    scrollHeight: 1,
    clientHeight: 1,
    cursorLine: 1,
    cursorColumn: 1,
    lineCount: countLines(value),
  }));

  const readEditorState = useCallback((view: EditorView | null = viewRef.current) => {
    if (!view) return;
    const { scrollDOM } = view;
    const head = view.state.selection.main.head;
    const line = view.state.doc.lineAt(head);
    const next = {
      scrollTop: scrollDOM.scrollTop,
      scrollHeight: Math.max(scrollDOM.scrollHeight, 1),
      clientHeight: Math.max(scrollDOM.clientHeight, 1),
      cursorLine: line.number,
      cursorColumn: Math.max(1, head - line.from + 1),
      lineCount: view.state.doc.lines,
    };
    setNavState((previous) => {
      if (
        Math.abs(previous.scrollTop - next.scrollTop) < 0.5 &&
        previous.scrollHeight === next.scrollHeight &&
        previous.clientHeight === next.clientHeight &&
        previous.cursorLine === next.cursorLine &&
        previous.cursorColumn === next.cursorColumn &&
        previous.lineCount === next.lineCount
      ) {
        return previous;
      }
      return next;
    });
  }, []);

  const scheduleEditorStateRead = useCallback((view: EditorView | null = viewRef.current) => {
    if (scrollRafRef.current !== null) return;
    scrollRafRef.current = window.requestAnimationFrame(() => {
      scrollRafRef.current = null;
      readEditorState(view);
    });
  }, [readEditorState]);

  const attachEditor = useCallback(
    (view: EditorView) => {
      if (scrollElementRef.current && scrollHandlerRef.current) {
        scrollElementRef.current.removeEventListener('scroll', scrollHandlerRef.current);
      }
      viewRef.current = view;
      const handleScroll = () => scheduleEditorStateRead(view);
      scrollElementRef.current = view.scrollDOM;
      scrollHandlerRef.current = handleScroll;
      view.scrollDOM.addEventListener('scroll', handleScroll, { passive: true });
      scheduleEditorStateRead(view);
    },
    [scheduleEditorStateRead],
  );

  useEffect(() => {
    scheduleEditorStateRead();
  }, [scheduleEditorStateRead, value]);

  // Jump to a requested line (search "go to match"): scroll it to center +
  // place the cursor there. Re-runs on `value` so it still fires once the
  // file content finishes loading after the tab opens.
  useEffect(() => {
    if (gotoLine == null || gotoLine < 1) return;
    const view = viewRef.current;
    if (!view) return;
    const ln = Math.min(gotoLine, view.state.doc.lines);
    const info = view.state.doc.line(ln);
    view.dispatch({
      selection: { anchor: info.from },
      effects: EditorView.scrollIntoView(info.from, { y: 'center' }),
    });
    view.focus();
  }, [gotoLine, value]);

  useEffect(() => {
    return () => {
      if (scrollElementRef.current && scrollHandlerRef.current) {
        scrollElementRef.current.removeEventListener('scroll', scrollHandlerRef.current);
      }
      if (scrollRafRef.current !== null) {
        window.cancelAnimationFrame(scrollRafRef.current);
        scrollRafRef.current = null;
      }
    };
  }, []);

  const editorExtensions = useMemo(
    () => [
      ...languageExtensions,
      vscodeEditorTheme,
      EditorView.updateListener.of((update: ViewUpdate) => {
        if (
          update.docChanged ||
          update.selectionSet ||
          update.viewportChanged ||
          update.geometryChanged
        ) {
          scheduleEditorStateRead(update.view);
        }
      }),
    ],
    [languageExtensions, scheduleEditorStateRead],
  );

  const minimapLines = useMemo(() => {
    const lines = value.split(/\r\n|\r|\n/);
    const sampleEvery = Math.max(1, Math.ceil(lines.length / MAX_MINIMAP_LINES));
    return lines
      .filter((_, index) => index % sampleEvery === 0)
      .map((line, index) => {
        const trimmed = line.trimEnd();
        const width = Math.max(6, Math.min(44, trimmed.length * 1.85));
        return {
          key: `${index}-${trimmed.length}`,
          width,
          accent:
            /^\s*(function|fn|class|interface|type|struct|enum|impl|export|pub|def|async)\b/.test(trimmed)
              ? 'strong'
              : trimmed.length === 0
                ? 'blank'
                : 'normal',
        };
      });
  }, [value]);

  const minimapGeometry = useMemo(() => {
    const renderedLineCount = Math.max(1, minimapLines.length);
    const availableHeight = Math.max(1, navState.clientHeight - MINIMAP_VERTICAL_PADDING * 2);
    const naturalHeight =
      renderedLineCount * MINIMAP_SHORT_FILE_LINE_HEIGHT +
      Math.max(0, renderedLineCount - 1) * MINIMAP_SHORT_FILE_LINE_GAP;
    const shouldFitToViewport = naturalHeight > availableHeight;
    const lineGap = shouldFitToViewport ? 0 : MINIMAP_SHORT_FILE_LINE_GAP;
    const lineHeight = shouldFitToViewport
      ? availableHeight / renderedLineCount
      : MINIMAP_SHORT_FILE_LINE_HEIGHT;
    const contentHeight = shouldFitToViewport ? availableHeight : naturalHeight;
    const isScrollable = navState.scrollHeight > navState.clientHeight + 1;
    const viewportHeight = isScrollable
      ? Math.max(
          MINIMAP_MIN_VIEWPORT_HEIGHT,
          Math.min(contentHeight, (navState.clientHeight / Math.max(1, navState.scrollHeight)) * contentHeight),
        )
      : 0;
    const viewportTop = isScrollable
      ? Math.max(
          0,
          Math.min(
            contentHeight - viewportHeight,
            (navState.scrollTop / Math.max(1, navState.scrollHeight - navState.clientHeight)) *
              (contentHeight - viewportHeight),
          ),
        )
      : 0;

    return {
      contentHeight,
      isScrollable,
      lineGap,
      lineHeight,
      viewportHeight,
      viewportTop,
    };
  }, [
    minimapLines.length,
    navState.clientHeight,
    navState.scrollHeight,
    navState.scrollTop,
  ]);

  const scrollToMinimapPointer = useCallback(
    (clientY: number) => {
      const minimap = minimapRef.current;
      const view = viewRef.current;
      if (!minimap || !view || !minimapGeometry.isScrollable) return;
      const rect = minimap.getBoundingClientRect();
      const ratio = Math.max(
        0,
        Math.min(
          1,
          (clientY - rect.top - MINIMAP_VERTICAL_PADDING) /
            Math.max(1, minimapGeometry.contentHeight),
        ),
      );
      const maxScroll = Math.max(0, navState.scrollHeight - navState.clientHeight);
      view.scrollDOM.scrollTop = ratio * maxScroll;
      readEditorState(view);
    },
    [
      minimapGeometry.contentHeight,
      minimapGeometry.isScrollable,
      navState.clientHeight,
      navState.scrollHeight,
      readEditorState,
    ],
  );

  return (
    <div className="canvas-editor-shell">
      <div className="canvas-codemirror-host">
        <CodeMirror
          value={value}
          onChange={(next) => onDraftChange(tabId, next)}
          onCreateEditor={attachEditor}
          theme={oneDark}
          extensions={editorExtensions}
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
      </div>
      <div
        ref={minimapRef}
        className={`canvas-minimap ${minimapGeometry.isScrollable ? 'is-scrollable' : 'is-short-file'}`}
        title="Overview ruler — click or drag to jump through the file"
        onPointerDown={(event) => {
          if (!minimapGeometry.isScrollable) return;
          event.preventDefault();
          event.currentTarget.setPointerCapture(event.pointerId);
          scrollToMinimapPointer(event.clientY);
        }}
        onPointerMove={(event) => {
          if (event.buttons === 1) {
            scrollToMinimapPointer(event.clientY);
          }
        }}
      >
        <div
          className="canvas-minimap-lines"
          style={{
            height: minimapGeometry.contentHeight,
            gap: minimapGeometry.lineGap,
            '--canvas-minimap-line-height': `${minimapGeometry.lineHeight}px`,
          } as React.CSSProperties}
        >
          {minimapLines.map((line) => (
            <span
              key={line.key}
              className={`canvas-minimap-line is-${line.accent}`}
              style={{ width: line.width }}
            />
          ))}
        </div>
        {minimapGeometry.isScrollable && (
          <div
            className="canvas-minimap-viewport"
            style={{
              top: MINIMAP_VERTICAL_PADDING + minimapGeometry.viewportTop,
              height: minimapGeometry.viewportHeight,
            }}
          />
        )}
      </div>
      <div className="canvas-editor-position">
        Ln {navState.cursorLine.toLocaleString()}, Col {navState.cursorColumn.toLocaleString()} ·{' '}
        {navState.lineCount.toLocaleString()} lines
      </div>
    </div>
  );
};

/// One row in the line-by-line diff view.
type DiffKind = 'equal' | 'remove' | 'add';

type DiffRow = {
  kind: DiffKind;
  text: string;
  beforeLine: number | null;
  afterLine: number | null;
};

interface DiffResult {
  rows: DiffRow[];
  beforeLines: number;
  afterLines: number;
  additions: number;
  deletions: number;
  algorithm: string;
  durationMs: number;
  exact: boolean;
}

interface DiffWorkerRequest {
  requestId: number;
  before: string;
  after: string;
}

interface DiffWorkerDone extends DiffResult {
  requestId: number;
  status: 'done';
}

interface DiffWorkerFailure {
  requestId: number;
  status: 'error';
  message: string;
}

type DiffWorkerResponse = DiffWorkerDone | DiffWorkerFailure;

type DiffState =
  | { status: 'loading'; result: null; error: null }
  | { status: 'done'; result: DiffResult; error: null }
  | { status: 'error'; result: null; error: string };

const DIFF_ROW_HEIGHT = 20;
const DIFF_OVERSCAN_ROWS = 36;

function useWorkerDiff(before: string, after: string): DiffState {
  const [state, setState] = useState<DiffState>({
    status: 'loading',
    result: null,
    error: null,
  });
  const requestIdRef = useRef(0);
  const workerRef = useRef<Worker | null>(null);

  useEffect(() => {
    const requestId = requestIdRef.current + 1;
    requestIdRef.current = requestId;
    setState({ status: 'loading', result: null, error: null });

    if (workerRef.current) {
      workerRef.current.terminate();
      workerRef.current = null;
    }

    let worker: Worker | null = null;

    const finish = (nextState: DiffState) => {
      if (requestIdRef.current !== requestId || workerRef.current !== worker) {
        return;
      }
      setState(nextState);
      worker?.terminate();
      workerRef.current = null;
    };

    try {
      worker = new Worker(
        new URL('../workers/diffWorker.ts', import.meta.url),
        { type: 'module' },
      );
      workerRef.current = worker;

      worker.onmessage = (event: MessageEvent<DiffWorkerResponse>) => {
        const data = event.data;
        if (data.requestId !== requestId) return;
        if (data.status === 'done') {
          const {
            rows,
            beforeLines,
            afterLines,
            additions,
            deletions,
            algorithm,
            durationMs,
            exact,
          } = data;
          finish({
            status: 'done',
            result: {
              rows,
              beforeLines,
              afterLines,
              additions,
              deletions,
              algorithm,
              durationMs,
              exact,
            },
            error: null,
          });
        } else {
          finish({ status: 'error', result: null, error: data.message });
        }
      };

      worker.onerror = (event) => {
        finish({
          status: 'error',
          result: null,
          error: event.message || 'Diff worker failed.',
        });
      };

      const request: DiffWorkerRequest = { requestId, before, after };
      worker.postMessage(request);
    } catch (error) {
      worker?.terminate();
      if (workerRef.current === worker) {
        workerRef.current = null;
      }
      const message = error instanceof Error ? error.message : String(error);
      setState({ status: 'error', result: null, error: message });
    }

    return () => {
      if (workerRef.current === worker) {
        workerRef.current?.terminate();
        workerRef.current = null;
      }
    };
  }, [before, after]);

  return state;
}

function countLines(text: string): number {
  let count = 1;
  for (let i = 0; i < text.length; i += 1) {
    if (text.charCodeAt(i) === 10) count += 1;
  }
  return count;
}

function canvasBreadcrumbSegments(p: string): string[] {
  const norm = p.replace(/\\/g, '/').replace(/\/+$/, '');
  const parts = norm.split('/').filter(Boolean);
  if (parts.length <= 5) return parts;
  return [parts[0], '…', ...parts.slice(-3)];
}

/// Unified line diff renderer. Red lines were on disk, green are the
/// AI's edit, plain lines are unchanged. Line numbers on both sides
/// (git-style) so the user can correlate hunks back to the file.
const DiffView: React.FC<{ before: string; after: string }> = ({ before, after }) => {
  const diffState = useWorkerDiff(before, after);
  const lineCounts = useMemo(
    () => ({
      before: countLines(before),
      after: countLines(after),
    }),
    [before, after],
  );
  const scrollRef = useRef<HTMLDivElement>(null);
  const scrollRafRef = useRef<number | null>(null);
  const [viewport, setViewport] = useState({ scrollTop: 0, height: 0 });

  const readViewport = useCallback(() => {
    const el = scrollRef.current;
    if (!el) return;
    const next = { scrollTop: el.scrollTop, height: el.clientHeight };
    setViewport((previous) => {
      if (
        Math.abs(previous.scrollTop - next.scrollTop) < 0.5 &&
        Math.abs(previous.height - next.height) < 0.5
      ) {
        return previous;
      }
      return next;
    });
  }, []);

  const handleScroll = useCallback(() => {
    if (scrollRafRef.current !== null) return;
    scrollRafRef.current = window.requestAnimationFrame(() => {
      scrollRafRef.current = null;
      readViewport();
    });
  }, [readViewport]);

  useEffect(() => {
    return () => {
      if (scrollRafRef.current !== null) {
        window.cancelAnimationFrame(scrollRafRef.current);
        scrollRafRef.current = null;
      }
    };
  }, []);

  useEffect(() => {
    if (diffState.status !== 'done') return;
    const el = scrollRef.current;
    if (!el) return;
    el.scrollTop = 0;
    setViewport({ scrollTop: 0, height: el.clientHeight });
  }, [diffState.status, diffState.result]);

  useEffect(() => {
    if (diffState.status !== 'done') return;
    readViewport();
    const el = scrollRef.current;
    if (!el) return;
    const resizeObserver = new ResizeObserver(readViewport);
    resizeObserver.observe(el);
    return () => resizeObserver.disconnect();
  }, [diffState.status, readViewport]);

  const virtualRows = useMemo(() => {
    if (diffState.status !== 'done') {
      return {
        rows: [] as DiffRow[],
        startIndex: 0,
        endIndex: 0,
        totalHeight: 0,
      };
    }
    const rows = diffState.result.rows;
    const viewportHeight = viewport.height || 480;
    const startIndex = Math.max(
      0,
      Math.floor(viewport.scrollTop / DIFF_ROW_HEIGHT) - DIFF_OVERSCAN_ROWS,
    );
    const endIndex = Math.min(
      rows.length,
      Math.ceil((viewport.scrollTop + viewportHeight) / DIFF_ROW_HEIGHT) +
        DIFF_OVERSCAN_ROWS,
    );
    return {
      rows: rows.slice(startIndex, endIndex),
      startIndex,
      endIndex,
      totalHeight: rows.length * DIFF_ROW_HEIGHT,
    };
  }, [diffState, viewport.height, viewport.scrollTop]);

  if (diffState.status === 'loading') {
    return (
      <div
        style={{
          padding: 24,
          color: 'var(--text-muted)',
          lineHeight: 1.6,
          display: 'flex',
          alignItems: 'flex-start',
          gap: 10,
        }}
      >
        <RefreshCw size={16} className="spin" style={{ marginTop: 3 }} />
        <div>
          <strong style={{ color: 'var(--text-secondary)' }}>
            Computing diff off the UI thread…
          </strong>
          <div>
            {lineCounts.before.toLocaleString()} →{' '}
            {lineCounts.after.toLocaleString()} lines
          </div>
        </div>
      </div>
    );
  }

  if (diffState.status === 'error') {
    return (
      <div
        style={{
          padding: 24,
          color: 'var(--color-error, #ef4444)',
          lineHeight: 1.6,
        }}
      >
        <strong style={{ display: 'block', marginBottom: 4 }}>
          Failed to render diff
        </strong>
        <span style={{ color: 'var(--text-secondary)' }}>
          {diffState.error}
        </span>
      </div>
    );
  }

  const { result } = diffState;
  if (result.additions === 0 && result.deletions === 0) {
    return (
      <div style={{ padding: 24, color: 'var(--text-muted)' }}>
        No changes between baseline and current content.
      </div>
    );
  }

  return (
    <div
      style={{
        flex: 1,
        minHeight: 0,
        display: 'flex',
        flexDirection: 'column',
        fontFamily:
          'ui-monospace, SFMono-Regular, Menlo, Consolas, "Liberation Mono", monospace',
        fontSize: 13,
        color: 'var(--text-primary)',
      }}
    >
      <div
        style={{
          flexShrink: 0,
          display: 'flex',
          alignItems: 'center',
          gap: 10,
          padding: '8px 12px',
          borderBottom: '1px solid var(--border-muted)',
          background: 'rgba(255, 255, 255, 0.025)',
          color: 'var(--text-muted)',
          fontFamily: 'inherit',
          fontSize: 12,
          whiteSpace: 'nowrap',
        }}
      >
        <span>
          {result.beforeLines.toLocaleString()} →{' '}
          {result.afterLines.toLocaleString()} lines
        </span>
        <span style={{ color: '#86efac' }}>
          +{result.additions.toLocaleString()}
        </span>
        <span style={{ color: '#fca5a5' }}>
          -{result.deletions.toLocaleString()}
        </span>
        <span>{result.rows.length.toLocaleString()} rows</span>
        <span>{result.algorithm}</span>
        <span>{result.durationMs.toLocaleString()}ms</span>
        <span style={{ marginLeft: 'auto' }}>
          showing {virtualRows.startIndex + 1}-
          {virtualRows.endIndex.toLocaleString()} of{' '}
          {result.rows.length.toLocaleString()}
        </span>
      </div>
      {!result.exact && (
        <div
          style={{
            flexShrink: 0,
            padding: '6px 12px',
            borderBottom: '1px solid var(--border-muted)',
            background: 'rgba(245, 158, 11, 0.08)',
            color: 'var(--text-secondary)',
            fontFamily:
              'system-ui, -apple-system, BlinkMacSystemFont, "Segoe UI", sans-serif',
            fontSize: 12,
          }}
        >
          Large rewrite shown with bounded alignment; unchanged anchors are
          exact, dense changed blocks are grouped to keep the GUI responsive.
        </div>
      )}
      <div
        ref={scrollRef}
        onScroll={handleScroll}
        style={{
          flex: 1,
          minHeight: 0,
          overflow: 'auto',
          position: 'relative',
          background: 'rgba(0, 0, 0, 0.08)',
        }}
      >
        <div
          style={{
            position: 'relative',
            height: virtualRows.totalHeight,
            minWidth: '100%',
          }}
        >
          {virtualRows.rows.map((row, offset) => {
            const index = virtualRows.startIndex + offset;
            return (
              <DiffLine
                key={index}
                row={row}
                top={index * DIFF_ROW_HEIGHT}
              />
            );
          })}
        </div>
      </div>
    </div>
  );
};

interface DiffLineProps {
  row: DiffRow;
  top: number;
}

const diffLineNumberStyle: React.CSSProperties = {
  textAlign: 'right',
  paddingRight: 8,
  color: 'var(--text-muted)',
  userSelect: 'none',
  opacity: 0.7,
};

const DiffLine = React.memo(function DiffLine({ row, top }: DiffLineProps) {
  const isRemove = row.kind === 'remove';
  const isAdd = row.kind === 'add';
  return (
    <div
      style={{
        position: 'absolute',
        top,
        left: 0,
        height: DIFF_ROW_HEIGHT,
        minWidth: '100%',
        width: 'max-content',
        display: 'grid',
        gridTemplateColumns: '3.5em 3.5em 1em max-content',
        alignItems: 'center',
        columnGap: 0,
        lineHeight: `${DIFF_ROW_HEIGHT}px`,
        whiteSpace: 'pre',
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
      }}
    >
      <span aria-hidden style={diffLineNumberStyle}>
        {row.beforeLine ?? ' '}
      </span>
      <span aria-hidden style={diffLineNumberStyle}>
        {row.afterLine ?? ' '}
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
});

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
