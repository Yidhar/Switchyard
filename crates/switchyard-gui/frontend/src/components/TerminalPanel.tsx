import React, { useState, useRef, useEffect, useCallback } from 'react';
import {
  Plus,
  X,
  Trash2,
  SplitSquareHorizontal,
  Terminal as TerminalIcon,
  ChevronDown,
  ChevronUp,
  ExternalLink,
} from 'lucide-react';
import { invoke, Channel } from '@tauri-apps/api/core';
import { Terminal } from '@xterm/xterm';
import { FitAddon } from '@xterm/addon-fit';
import { WebLinksAddon } from '@xterm/addon-web-links';
import '@xterm/xterm/css/xterm.css';

const TERMINAL_DEBUG_BOOT = false;
const TERMINAL_MIN_HEIGHT = 160;
const TERMINAL_DEFAULT_HEIGHT = 280;
const TERMINAL_OUTPUT_MAX_BYTES_PER_WRITE = 256 * 1024;
const TERMINAL_RESIZE_DEBOUNCE_MS = 50;

interface TerminalOutputBuffer {
  push: (bytes: Uint8Array) => void;
  pushText: (text: string) => void;
  dispose: () => void;
}

interface TerminalPanelProps {
  /// Whether the bottom panel is currently shown. The component may
  /// remain mounted while hidden so existing PTYs do not need to
  /// restart every time the user toggles the panel.
  visible: boolean;
  /// Current workspace's primary_root. Each new PTY spawns with this
  /// as its cwd so shells land in the project.
  cwd: string | null;
  /// Hide the panel (rail toggle off).
  onClose: () => void;
}

interface TerminalSession {
  /// Frontend tab identifier (used as React key).
  uiId: string;
  /// Display label shown in the tab strip.
  label: string;
  /// PTY id returned from the backend's `pty_create` command — only
  /// set once the async spawn resolves. While `null` the tab shows a
  /// "connecting…" hint.
  ptyId: string | null;
}

/// Bottom-anchored terminal panel. Each tab is backed by a real
/// `portable-pty` PTY on the Rust side (see `crates/switchyard-gui/
/// src/pty.rs`). The xterm.js instances render the bytes the PTY
/// streams via Tauri events; user keystrokes go the other way via
/// the `pty_write` command.
export const TerminalPanel: React.FC<TerminalPanelProps> = ({ visible, cwd, onClose }) => {
  const [sessions, setSessions] = useState<TerminalSession[]>([]);
  const [activeId, setActiveId] = useState<string | null>(null);
  const [maximized, setMaximized] = useState(false);
  const [panelHeight, setPanelHeight] = useState(() =>
    readStoredNumber(
      'switchyard.terminalPanelHeight',
      TERMINAL_DEFAULT_HEIGHT,
      TERMINAL_MIN_HEIGHT,
      Math.round(window.innerHeight * 0.85),
    ),
  );
  const panelRef = useRef<HTMLDivElement | null>(null);
  const panelHeightRef = useRef(panelHeight);

  useEffect(() => {
    panelHeightRef.current = panelHeight;
    panelRef.current?.style.setProperty(
      '--switchyard-terminal-panel-height',
      `${Math.round(panelHeight)}px`,
    );
  }, [panelHeight]);

  const addSession = useCallback(() => {
    const id = `t-${Date.now()}-${Math.random().toString(36).slice(2, 6)}`;
    setSessions((prev) => [
      ...prev,
      { uiId: id, label: `Terminal ${prev.length + 1}`, ptyId: null },
    ]);
    setActiveId(id);
  }, []);

  // First show → spawn the initial terminal. When the panel is only
  // hidden we keep `sessions` intact, so toggling it back open does
  // not restart the shell. If the user killed the last terminal, the
  // next show creates a fresh one.
  useEffect(() => {
    if (!visible) return;
    if (!cwd) return;
    if (sessions.length > 0) return;
    addSession();
  }, [addSession, cwd, sessions.length, visible]);

  const killSession = useCallback(
    (uiId: string) => {
      setSessions((prev) => {
        const dying = prev.find((s) => s.uiId === uiId);
        if (dying?.ptyId) {
          void invoke('pty_close', { ptyId: dying.ptyId }).catch(() => {});
        }
        const next = prev.filter((s) => s.uiId !== uiId);
        if (activeId === uiId && next.length > 0) {
          setActiveId(next[next.length - 1].uiId);
        }
        if (next.length === 0) {
          // Hide the panel when the last shell dies — matches VS
          // Code's behavior of folding the panel away.
          onClose();
        }
        return next;
      });
    },
    [activeId, onClose],
  );

  /// Callback invoked from a TerminalInstance once its backend PTY
  /// has been provisioned. Stores the returned PTY id on the session
  /// so `killSession` knows what to kill.
  const handlePtyId = useCallback((uiId: string, ptyId: string) => {
    setSessions((prev) =>
      prev.map((s) => (s.uiId === uiId ? { ...s, ptyId } : s)),
    );
  }, []);

  const clearActive = () => {
    // Sending Ctrl+L to the active PTY clears the screen the same way
    // a real shell would. We avoid touching xterm.clear() directly
    // because that bypasses the shell and leaves it confused.
    const active = sessions.find((s) => s.uiId === activeId);
    if (!active || !active.ptyId) return;
    const ctrlL = encodeBytesBase64(new Uint8Array([12]));
    void invoke('pty_write', { ptyId: active.ptyId, data: ctrlL }).catch(
      (e) => console.warn('[switchyard] pty_write(Ctrl+L) failed', e),
    );
  };

  const openExternalTerminal = useCallback(() => {
    if (!cwd) return;
    void invoke('open_external_terminal', { cwd }).catch((e) => {
      console.warn('[switchyard] open_external_terminal failed', e);
      window.alert(`Failed to open external terminal:\n${String(e)}`);
    });
  }, [cwd]);

  const startPanelResize = (event: React.PointerEvent<HTMLDivElement>) => {
    if (event.button !== 0) return;
    const panel = panelRef.current;
    const parent = panel?.parentElement;
    if (!panel || !parent) return;
    event.preventDefault();
    event.currentTarget.setPointerCapture(event.pointerId);

    const parentRect = parent.getBoundingClientRect();
    const maxHeight = Math.max(
      TERMINAL_MIN_HEIGHT,
      Math.floor(parentRect.height * 0.85),
    );
    const bottom = parentRect.bottom;

    document.body.style.cursor = 'row-resize';
    document.body.style.userSelect = 'none';
    document.body.classList.add('is-layout-resizing');
    setMaximized(false);

    let nextHeight = panelHeightRef.current;
    let resizeFrame: number | null = null;
    const applyPendingHeight = () => {
      resizeFrame = null;
      panel.style.setProperty(
        '--switchyard-terminal-panel-height',
        `${Math.round(nextHeight)}px`,
      );
    };

    const handlePointerMove = (moveEvent: PointerEvent) => {
      nextHeight = clampNumber(bottom - moveEvent.clientY, TERMINAL_MIN_HEIGHT, maxHeight);
      panelHeightRef.current = nextHeight;
      if (resizeFrame === null) {
        resizeFrame = requestAnimationFrame(applyPendingHeight);
      }
    };

    const handlePointerUp = () => {
      document.removeEventListener('pointermove', handlePointerMove);
      document.removeEventListener('pointerup', handlePointerUp);
      if (resizeFrame !== null) {
        cancelAnimationFrame(resizeFrame);
        resizeFrame = null;
      }
      applyPendingHeight();
      const committed = Math.round(nextHeight);
      panelHeightRef.current = committed;
      setMaximized(false);
      setPanelHeight(committed);
      window.localStorage.setItem('switchyard.terminalPanelHeight', String(committed));
      document.body.style.cursor = '';
      document.body.style.userSelect = '';
      document.body.classList.remove('is-layout-resizing');
    };

    document.addEventListener('pointermove', handlePointerMove);
    document.addEventListener('pointerup', handlePointerUp, { once: true });
  };

  return (
    <div
      ref={panelRef}
      className="terminal-panel"
      style={{
        '--switchyard-terminal-panel-height': `${Math.round(panelHeightRef.current)}px`,
        height: maximized ? '70vh' : 'var(--switchyard-terminal-panel-height, 280px)',
        flexShrink: 0,
        background: '#1e1e1e',
        borderTop: '1px solid var(--border-muted)',
        display: visible ? 'flex' : 'none',
        flexDirection: 'column',
        overflow: 'hidden',
        position: 'relative',
      } as React.CSSProperties}
    >
      <div
        role="separator"
        aria-orientation="horizontal"
        className="layout-sash layout-sash-horizontal terminal-panel-sash"
        title="Drag to resize terminal panel · Double-click to reset"
        onPointerDown={startPanelResize}
        onDoubleClick={() => {
          panelRef.current?.style.setProperty(
            '--switchyard-terminal-panel-height',
            `${TERMINAL_DEFAULT_HEIGHT}px`,
          );
          panelHeightRef.current = TERMINAL_DEFAULT_HEIGHT;
          setMaximized(false);
          setPanelHeight(TERMINAL_DEFAULT_HEIGHT);
          window.localStorage.setItem(
            'switchyard.terminalPanelHeight',
            String(TERMINAL_DEFAULT_HEIGHT),
          );
        }}
      />
      {/* Tab strip + toolbar */}
      <div
        style={{
          display: 'flex',
          alignItems: 'center',
          background: 'rgba(0, 0, 0, 0.3)',
          borderBottom: '1px solid var(--border-muted)',
          minHeight: 30,
          flexShrink: 0,
        }}
      >
        <div
          style={{
            display: 'flex',
            alignItems: 'stretch',
            flex: 1,
            minWidth: 0,
            overflowX: 'auto',
          }}
        >
          {sessions.map((s) => {
            const isActive = s.uiId === activeId;
            return (
              <div
                key={s.uiId}
                onClick={() => setActiveId(s.uiId)}
                style={{
                  display: 'inline-flex',
                  alignItems: 'center',
                  gap: 6,
                  padding: '4px 10px',
                  borderRight: '1px solid var(--border-muted)',
                  background: isActive ? '#1e1e1e' : 'transparent',
                  color: isActive ? 'var(--text-primary)' : 'var(--text-muted)',
                  cursor: 'pointer',
                  fontSize: 12,
                  whiteSpace: 'nowrap',
                  position: 'relative',
                  height: 30,
                  lineHeight: '22px',
                }}
              >
                {isActive && (
                  <span
                    aria-hidden
                    style={{
                      position: 'absolute',
                      left: 0,
                      right: 0,
                      top: 0,
                      height: 2,
                      background: 'var(--color-primary)',
                    }}
                  />
                )}
                <TerminalIcon size={11} />
                <span>{s.label}</span>
                <button
                  type="button"
                  onClick={(e) => {
                    e.stopPropagation();
                    killSession(s.uiId);
                  }}
                  style={{
                    background: 'none',
                    border: 'none',
                    color: 'var(--text-muted)',
                    cursor: 'pointer',
                    padding: 0,
                    marginLeft: 4,
                    display: 'inline-flex',
                    alignItems: 'center',
                  }}
                  title="Kill terminal"
                >
                  <X size={11} />
                </button>
              </div>
            );
          })}
        </div>

        {/* Right-side toolbar — VS Code mirrors this set: add, split, kill, expand, close. */}
        <div
          style={{
            display: 'flex',
            alignItems: 'center',
            gap: 2,
            paddingRight: 6,
            paddingLeft: 6,
            borderLeft: '1px solid var(--border-muted)',
          }}
        >
          <ToolbarBtn onClick={addSession} title="New terminal">
            <Plus size={13} />
          </ToolbarBtn>
          <ToolbarBtn
            onClick={() => {
              // No real split-pane yet — open another tab as a
              // pragmatic stand-in so the user has the affordance.
              addSession();
            }}
            title="Split terminal"
          >
            <SplitSquareHorizontal size={13} />
          </ToolbarBtn>
          <ToolbarBtn onClick={openExternalTerminal} title="Open external terminal">
            <ExternalLink size={13} />
          </ToolbarBtn>
          <ToolbarBtn onClick={clearActive} title="Clear (Ctrl+L)">
            <Trash2 size={13} />
          </ToolbarBtn>
          <ToolbarBtn
            onClick={() => setMaximized((v) => !v)}
            title={maximized ? 'Restore size' : 'Maximize panel'}
          >
            {maximized ? <ChevronDown size={13} /> : <ChevronUp size={13} />}
          </ToolbarBtn>
          <ToolbarBtn onClick={onClose} title="Hide terminal">
            <X size={13} />
          </ToolbarBtn>
        </div>
      </div>

      {/* Terminal bodies — render ALL sessions as siblings, hide the
          inactive ones with `visibility: hidden + position absolute`
          so their xterm instances stay mounted (keeping scrollback +
          PTY connection alive) but only the active one is shown. */}
      <div
        style={{
          flex: 1,
          minHeight: 0,
          position: 'relative',
          background: '#1e1e1e',
        }}
      >
        {sessions.map((s) => (
          <TerminalInstance
            key={s.uiId}
            uiId={s.uiId}
            visible={visible && s.uiId === activeId}
            cwd={cwd ?? '.'}
            onPtyIdReady={handlePtyId}
            onExit={() => killSession(s.uiId)}
          />
        ))}
      </div>
    </div>
  );
};

const ToolbarBtn: React.FC<{
  onClick: () => void;
  title: string;
  children: React.ReactNode;
}> = ({ onClick, title, children }) => (
  <button
    type="button"
    onClick={onClick}
    title={title}
    style={{
      background: 'transparent',
      border: 'none',
      borderRadius: 3,
      color: 'var(--text-muted)',
      cursor: 'pointer',
      padding: '4px 6px',
      display: 'inline-flex',
      alignItems: 'center',
    }}
    onMouseEnter={(e) => {
      e.currentTarget.style.background = 'rgba(255, 255, 255, 0.05)';
      e.currentTarget.style.color = 'var(--text-primary)';
    }}
    onMouseLeave={(e) => {
      e.currentTarget.style.background = 'transparent';
      e.currentTarget.style.color = 'var(--text-muted)';
    }}
  >
    {children}
  </button>
);

// ---------------------------------------------------------------------------
// Single terminal instance — xterm.js bound to a backend PTY
// ---------------------------------------------------------------------------

interface TerminalInstanceProps {
  uiId: string;
  visible: boolean;
  cwd: string;
  onPtyIdReady: (uiId: string, ptyId: string) => void;
  onExit: () => void;
}

const TerminalInstance: React.FC<TerminalInstanceProps> = ({
  uiId,
  visible,
  cwd,
  onPtyIdReady,
  onExit,
}) => {
  const containerRef = useRef<HTMLDivElement | null>(null);
  const visibleRef = useRef(visible);
  /// `state` holds the actively-mounted xterm + addons + PTY id.
  /// We use a ref (not state) because none of these are React-rendered
  /// and we don't want re-renders to recreate them.
  const stateRef = useRef<{
    term: Terminal;
    fit: FitAddon;
    ptyId: string | null;
    disposables: Array<() => void>;
    scheduleFit: (focus?: boolean) => void;
  } | null>(null);

  useEffect(() => {
    visibleRef.current = visible;
  }, [visible]);

  // One-shot init. Spawns the PTY, attaches the xterm instance,
  // wires data flow both ways, and cleans up on unmount.
  useEffect(() => {
    const container = containerRef.current;
    if (!container) return;
    let cancelled = false;
    console.debug('[switchyard] terminal mount', uiId);

    const term = new Terminal({
      fontFamily:
        'ui-monospace, SFMono-Regular, Menlo, Consolas, "Liberation Mono", monospace',
      fontSize: 13,
      lineHeight: 1.2,
      cursorBlink: true,
      // VS Code-ish palette so the terminal blends with the rest of
      // the GUI's One Dark editor theme.
      theme: {
        background: '#1e1e1e',
        foreground: '#d4d4d4',
        cursor: '#d4d4d4',
        selectionBackground: 'rgba(99, 102, 241, 0.3)',
        black: '#000000',
        red: '#cd3131',
        green: '#0dbc79',
        yellow: '#e5e510',
        blue: '#2472c8',
        magenta: '#bc3fbc',
        cyan: '#11a8cd',
        white: '#e5e5e5',
        brightBlack: '#666666',
        brightRed: '#f14c4c',
        brightGreen: '#23d18b',
        brightYellow: '#f5f543',
        brightBlue: '#3b8eea',
        brightMagenta: '#d670d6',
        brightCyan: '#29b8db',
        brightWhite: '#e5e5e5',
      },
      scrollback: 10_000,
      allowProposedApi: false,
    });
    const fit = new FitAddon();
    term.loadAddon(fit);
    term.loadAddon(new WebLinksAddon());
    term.open(container);

    const outputBuffer = createTerminalOutputBuffer(term, () => cancelled);
    let fitRaf: number | null = null;
    let resizeTimer: ReturnType<typeof setTimeout> | null = null;
    let ptyIdForResize: string | null = null;
    let lastSentCols = 0;
    let lastSentRows = 0;
    let pendingResize: { cols: number; rows: number } | null = null;

    const flushPtyResize = () => {
      resizeTimer = null;
      if (cancelled || !ptyIdForResize || !pendingResize) return;
      const { cols, rows } = pendingResize;
      pendingResize = null;
      if (cols === lastSentCols && rows === lastSentRows) return;
      lastSentCols = cols;
      lastSentRows = rows;
      void invoke('pty_resize', { ptyId: ptyIdForResize, cols, rows }).catch((e) => {
        if (!cancelled) console.warn('[switchyard] pty_resize failed', e);
      });
    };

    const schedulePtyResize = (cols: number, rows: number, immediate = false) => {
      if (!ptyIdForResize || cols < 1 || rows < 1) return;
      pendingResize = { cols, rows };
      if (resizeTimer !== null) {
        clearTimeout(resizeTimer);
        resizeTimer = null;
      }
      if (immediate) {
        flushPtyResize();
      } else {
        resizeTimer = setTimeout(flushPtyResize, TERMINAL_RESIZE_DEBOUNCE_MS);
      }
    };

    const fitNow = (focus = false) => {
      if (cancelled || !visibleRef.current) return;
      const rect = container.getBoundingClientRect();
      if (rect.width < 8 || rect.height < 8) return;
      try {
        fit.fit();
        if (ptyIdForResize) {
          schedulePtyResize(Math.max(1, term.cols), Math.max(1, term.rows));
        }
        if (focus) term.focus();
      } catch {
        // ignored — fit can throw if the container has zero size
        // during transient layouts (e.g. tab switch).
      }
    };

    const scheduleFit = (focus = false) => {
      if (fitRaf !== null) {
        cancelAnimationFrame(fitRaf);
      }
      fitRaf = requestAnimationFrame(() => {
        fitRaf = null;
        fitNow(focus);
      });
    };

    // Fit needs a tick after open() because the container's size is
    // measured from layout, not from React's render commit.
    scheduleFit();

    // Visible boot breadcrumbs so any failure has obvious provenance:
    //   xterm ok  → "xterm initialized"
    //   IPC ok    → "[switchyard] PTY ready, …" (from Rust)
    //   reader ok → "[switchyard] reader thread entered, …" (from Rust)
    //   shell ok  → actual shell banner / prompt afterwards
    // The frontend lines are gray, the Rust lines are cyan, the
    // shell output is whatever the shell paints.
    if (TERMINAL_DEBUG_BOOT) {
      term.write('\x1b[90m[switchyard] xterm initialized\x1b[0m\r\n');
    }

    const init = async () => {
      // Wire up the output + exit channels FIRST, then pass them into
      // the backend's `pty_create`. Tauri's `Channel<T>` is bound at
      // command-call time, so any bytes the shell emits during spawn
      // arrive through `onmessage` — no risk of dropping cmd.exe's
      // banner the way the old `listen()` approach did.
      const onOutput = new Channel<string>();
      onOutput.onmessage = (b64) => {
        if (cancelled) return;
        try {
          const decoded = decodeBase64ToUint8(b64);
          outputBuffer.push(decoded);
        } catch (e) {
          outputBuffer.pushText(`\r\n\x1b[31m[decode error: ${String(e)}]\x1b[0m\r\n`);
        }
      };

      const onExitChannel = new Channel<number>();
      onExitChannel.onmessage = () => {
        if (cancelled) return;
        outputBuffer.pushText('\r\n\x1b[90m[process exited]\x1b[0m\r\n');
        // Slight delay so the user sees the exit banner before
        // the tab disappears.
        setTimeout(onExit, 600);
      };

      try {
        // Use the post-fit dimensions so the shell gets its initial
        // size right from the first prompt.
        await new Promise<void>((resolve) => {
          requestAnimationFrame(() => {
            fitNow(false);
            resolve();
          });
        });
        const cols = Math.max(1, Math.floor(term.cols));
        const rows = Math.max(1, Math.floor(term.rows));
        if (TERMINAL_DEBUG_BOOT) {
          term.write(
            `\x1b[90m[switchyard] invoking pty_create cwd="${cwd}" size=${cols}x${rows}\x1b[0m\r\n`,
          );
        }
        const { pty_id } = await invoke<{ pty_id: string }>('pty_create', {
          cwd,
          cols,
          rows,
          onOutput,
          onExit: onExitChannel,
        });
        if (cancelled) {
          void invoke('pty_close', { ptyId: pty_id }).catch(() => {});
          return;
        }
        ptyIdForResize = pty_id;
        lastSentCols = Math.max(1, Math.floor(term.cols));
        lastSentRows = Math.max(1, Math.floor(term.rows));

        // Wire input: xterm → PTY. Keep failures visible and batch same-tick
        // keystrokes/paste bursts so the hot path is not one Tauri IPC call
        // per JavaScript callback. The write promise chain preserves ordering
        // if a user pastes while an earlier write is still crossing IPC.
        let pendingInputChunks: Uint8Array[] = [];
        let pendingInputBytes = 0;
        let inputFlushScheduled = false;
        let writeChain: Promise<void> = Promise.resolve();
        let inputPathLogged = false;
        let writeErrorShown = false;

        const reportPtyWriteError = (e: unknown) => {
          if (cancelled) return;
          console.warn('[switchyard] pty_write failed', e);
          if (!writeErrorShown) {
            writeErrorShown = true;
            outputBuffer.pushText(
              `\r\n\x1b[31m[switchyard] pty_write failed: ${String(e)}\x1b[0m\r\n`,
            );
          }
        };

        const flushInput = () => {
          inputFlushScheduled = false;
          if (cancelled || pendingInputChunks.length === 0) return;

          const bytes = concatByteChunks(pendingInputChunks, pendingInputBytes);
          pendingInputChunks = [];
          pendingInputBytes = 0;
          const encoded = encodeBytesBase64(bytes);

          writeChain = writeChain
            .catch(() => {
              // The previous write already surfaced its error; keep future
              // input usable instead of leaving the chain permanently rejected.
            })
            .then(async () => {
              if (cancelled) return;
              await invoke<void>('pty_write', { ptyId: pty_id, data: encoded });
            })
            .catch(reportPtyWriteError);
        };

        const queuePtyBytes = (bytes: Uint8Array, source: 'data' | 'binary') => {
          if (cancelled || bytes.length === 0) return;
          if (!inputPathLogged) {
            inputPathLogged = true;
            console.debug(`[switchyard] terminal input path active (${source})`);
          }
          pendingInputChunks.push(bytes);
          pendingInputBytes += bytes.length;
          if (!inputFlushScheduled) {
            inputFlushScheduled = true;
            setTimeout(flushInput, 0);
          }
        };

        // VS Code-like key routing: when focus is inside xterm, terminal
        // shortcuts should not leak to the surrounding React app / WebView.
        // Ctrl+C copies a terminal selection, otherwise it is left alone so
        // xterm sends SIGINT to the PTY.
        term.attachCustomKeyEventHandler((event) => {
          if (event.type !== 'keydown') return true;
          const key = event.key.toLowerCase();
          const ctrlOrMeta = event.ctrlKey || event.metaKey;
          if (ctrlOrMeta && key === 'c' && term.hasSelection()) {
            event.preventDefault();
            event.stopPropagation();
            const selection = term.getSelection();
            if (selection) {
              void navigator.clipboard.writeText(selection).catch((e) => {
                console.warn('[switchyard] terminal copy failed', e);
              });
            }
            return false;
          }

          if (
            ctrlOrMeta ||
            event.altKey ||
            event.key === 'Tab' ||
            event.key === 'Escape' ||
            event.key.startsWith('Arrow') ||
            event.key.startsWith('F')
          ) {
            event.stopPropagation();
          }
          return true;
        });

        const pasteHandler = (event: ClipboardEvent) => {
          if (!visibleRef.current) return;
          const text = event.clipboardData?.getData('text/plain');
          if (!text) return;
          event.preventDefault();
          event.stopPropagation();
          term.paste(text);
        };
        container.addEventListener('paste', pasteHandler, true);

        // `onData` is text-oriented; encode with TextEncoder so
        // multibyte input (CJK, emoji, composed chars) reaches the shell as
        // real UTF-8 bytes. Avoid the legacy unescape/encodeURIComponent path:
        // it is deprecated and makes failures hard to diagnose.
        const dataDisposable = term.onData((data) => {
          queuePtyBytes(encodeUtf8(data), 'data');
        });
        // Same for `onBinary` — xterm fires this for raw 8-bit byte
        // sequences (e.g. when an addon writes binary back).
        const binaryDisposable = term.onBinary((data) => {
          queuePtyBytes(binaryStringToBytes(data), 'binary');
        });

        // Resize: xterm geometry → PTY.
        const resizeDisposable = term.onResize(({ cols, rows }) => {
          schedulePtyResize(cols, rows);
        });

        stateRef.current = {
          term,
          fit,
          ptyId: pty_id,
          disposables: [
            () => dataDisposable.dispose(),
            () => binaryDisposable.dispose(),
            () => resizeDisposable.dispose(),
            () => container.removeEventListener('paste', pasteHandler, true),
          ],
          scheduleFit,
        };
        onPtyIdReady(uiId, pty_id);

        // Steal focus into xterm once everything's wired. Without
        // this, the very first tab boots with stateRef still null
        // (init() is async; the visibility effect already fired on
        // mount with `if (!s) return;`), so keypresses fall on the
        // floor until the user clicks the panel. Doing it here makes
        // typing work the moment the prompt appears.
        try {
          scheduleFit(true);
          term.focus();
          requestAnimationFrame(() => {
            if (!cancelled) {
              try {
                scheduleFit(true);
              } catch {
                // ignore; not critical
              }
            }
          });
          setTimeout(() => {
            if (!cancelled) {
              try {
                term.focus();
              } catch {
                // ignore; not critical
              }
            }
          }, 0);
        } catch {
          // ignore; not critical
        }
      } catch (e) {
        outputBuffer.pushText(`\r\n\x1b[31mFailed to start shell:\x1b[0m ${String(e)}\r\n`);
      }
    };
    void init();

    // ResizeObserver — keep the PTY size synced with the panel size.
    // Independently of xterm's own onResize because the FitAddon
    // recomputation only fires when the container's CSS size changes.
    const ro = new ResizeObserver(() => {
      scheduleFit(false);
    });
    ro.observe(container);

    return () => {
      cancelled = true;
      console.debug('[switchyard] terminal unmount', uiId);
      ro.disconnect();
      outputBuffer.dispose();
      if (fitRaf !== null) {
        cancelAnimationFrame(fitRaf);
      }
      if (resizeTimer !== null) {
        clearTimeout(resizeTimer);
      }
      const s = stateRef.current;
      if (s) {
        s.disposables.forEach((d) => d());
        if (s.ptyId) {
          void invoke('pty_close', { ptyId: s.ptyId }).catch(() => {});
        }
        s.term.dispose();
        stateRef.current = null;
      } else {
        term.dispose();
      }
    };
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, []);

  // When the tab becomes visible again, FitAddon needs to remeasure
  // (its previous fit() ran on a hidden 0×0 container if we just
  // switched here).
  useEffect(() => {
    if (!visible) return;
    const s = stateRef.current;
    if (!s) return;
    // RAF instead of immediate so the parent's visibility-change CSS
    // has flushed to layout first.
    const id = requestAnimationFrame(() => {
      try {
        s.scheduleFit(true);
      } catch {
        // ignored
      }
    });
    return () => cancelAnimationFrame(id);
  }, [visible]);

  return (
    <div
      ref={containerRef}
      // Click-anywhere-to-focus: xterm's internal textarea catches
      // keystrokes only when it has DOM focus. Clicking the canvas
      // directly works on its own, but clicks on the padding (or any
      // future toolbar overlay) wouldn't — explicit handler covers
      // both cases.
      onMouseDownCapture={() => stateRef.current?.term.focus()}
      onClick={() => stateRef.current?.term.focus()}
      style={{
        position: 'absolute',
        inset: 0,
        visibility: visible ? 'visible' : 'hidden',
        padding: '4px 0 4px 8px',
      }}
    />
  );
};

/// PTY output can arrive as many small Channel messages. Calling
/// `term.write()` for every message makes the WebView main thread stutter,
/// especially for TUI redraws and commands that stream progress with `\r`.
/// This queue batches bytes to animation frames and waits for xterm's write
/// callback before feeding the next batch, which gives us VS Code-like
/// backpressure without changing the Rust PTY contract.
function createTerminalOutputBuffer(
  term: Terminal,
  isCancelled: () => boolean,
): TerminalOutputBuffer {
  let chunks: Uint8Array[] = [];
  let totalBytes = 0;
  let rafId: number | null = null;
  let writing = false;
  let disposed = false;

  const takeBatch = (): Uint8Array => {
    const targetBytes = Math.min(totalBytes, TERMINAL_OUTPUT_MAX_BYTES_PER_WRITE);
    if (chunks.length === 1 && chunks[0].length <= targetBytes) {
      const only = chunks[0];
      chunks = [];
      totalBytes = 0;
      return only;
    }

    const out = new Uint8Array(targetBytes);
    let offset = 0;
    while (offset < targetBytes && chunks.length > 0) {
      const first = chunks.shift();
      if (!first) break;
      const remaining = targetBytes - offset;
      if (first.length <= remaining) {
        out.set(first, offset);
        offset += first.length;
      } else {
        out.set(first.subarray(0, remaining), offset);
        chunks.unshift(first.subarray(remaining));
        offset += remaining;
      }
    }
    totalBytes -= targetBytes;
    return out;
  };

  const schedule = () => {
    if (disposed || writing || rafId !== null || chunks.length === 0) return;
    rafId = requestAnimationFrame(flush);
  };

  const flush = () => {
    rafId = null;
    if (disposed || isCancelled() || writing || chunks.length === 0) return;
    const batch = takeBatch();
    writing = true;
    try {
      term.write(batch, () => {
        writing = false;
        schedule();
      });
    } catch (e) {
      writing = false;
      console.warn('[switchyard] terminal write failed', e);
      schedule();
    }
  };

  return {
    push(bytes: Uint8Array) {
      if (disposed || isCancelled() || bytes.length === 0) return;
      chunks.push(bytes);
      totalBytes += bytes.length;
      schedule();
    },
    pushText(text: string) {
      this.push(encodeUtf8(text));
    },
    dispose() {
      disposed = true;
      chunks = [];
      totalBytes = 0;
      if (rafId !== null) {
        cancelAnimationFrame(rafId);
        rafId = null;
      }
    },
  };
}

/// Decode a base64 string into a Uint8Array. xterm's `write` method
/// accepts either string or Uint8Array; we use the byte form so
/// non-UTF-8 ANSI escape sequences (which contain bytes outside the
/// ASCII range) pass through cleanly without the browser's
/// string-decoder mangling them.
function decodeBase64ToUint8(b64: string): Uint8Array {
  const bin = atob(b64);
  const out = new Uint8Array(bin.length);
  for (let i = 0; i < bin.length; i++) {
    out[i] = bin.charCodeAt(i);
  }
  return out;
}

const UTF8_ENCODER = new TextEncoder();

function encodeUtf8(data: string): Uint8Array {
  return UTF8_ENCODER.encode(data);
}

/// xterm's `onBinary` payload is a JavaScript "binary string": one byte per
/// code unit. Convert it explicitly before base64-encoding so bytes above 0x7f
/// are not reinterpreted as Unicode text.
function binaryStringToBytes(data: string): Uint8Array {
  const out = new Uint8Array(data.length);
  for (let i = 0; i < data.length; i++) {
    out[i] = data.charCodeAt(i) & 0xff;
  }
  return out;
}

function concatByteChunks(chunks: Uint8Array[], totalBytes: number): Uint8Array {
  if (chunks.length === 1) return chunks[0];
  const out = new Uint8Array(totalBytes);
  let offset = 0;
  for (const chunk of chunks) {
    out.set(chunk, offset);
    offset += chunk.length;
  }
  return out;
}

function encodeBytesBase64(bytes: Uint8Array): string {
  let binary = '';
  // Keep chunks modest; large pastes should not blow the call stack or create
  // an enormous argument list for String.fromCharCode.
  const chunkSize = 0x8000;
  for (let i = 0; i < bytes.length; i += chunkSize) {
    const chunk = bytes.subarray(i, i + chunkSize);
    for (let j = 0; j < chunk.length; j++) {
      binary += String.fromCharCode(chunk[j]);
    }
  }
  return btoa(binary);
}

function clampNumber(value: number, min: number, max: number): number {
  return Math.min(max, Math.max(min, value));
}

function readStoredNumber(key: string, fallback: number, min: number, max: number): number {
  try {
    const raw = window.localStorage.getItem(key);
    if (raw === null) return fallback;
    const parsed = Number(raw);
    if (!Number.isFinite(parsed)) return fallback;
    return clampNumber(parsed, min, max);
  } catch {
    return fallback;
  }
}

export default TerminalPanel;
