import React from 'react';

type InlineDiffKind = 'file' | 'hunk' | 'add' | 'remove' | 'meta' | 'context';

interface InlineDiffLine {
  kind: InlineDiffKind;
  text: string;
}

type InlineDiffState =
  | { status: 'pending'; lines: InlineDiffLine[]; error: null; truncated: false }
  | {
      status: 'done';
      lines: InlineDiffLine[];
      error: null;
      truncated: boolean;
      originalCharCount: number;
      renderedCharCount: number;
    }
  | { status: 'error'; lines: InlineDiffLine[]; error: string; truncated: false };

interface InlineDiffWorkerDone {
  requestId: number;
  type: 'done';
  lines: InlineDiffLine[];
  originalCharCount: number;
  renderedCharCount: number;
  truncated: boolean;
}

interface InlineDiffWorkerError {
  requestId: number;
  type: 'error';
  error: string;
}

type InlineDiffWorkerResponse = InlineDiffWorkerDone | InlineDiffWorkerError;

interface VirtualizedDiffBlockProps {
  text: string;
  maxHeight?: number;
  rowHeight?: number;
  maxChars?: number;
  lineNumberWidth?: number;
  style?: React.CSSProperties;
}

const DEFAULT_MAX_HEIGHT = 360;
const DEFAULT_ROW_HEIGHT = 20;
const DEFAULT_MAX_CHARS = 180_000;
const WORKER_THRESHOLD_CHARS = 24_000;
const OVERSCAN_ROWS = 14;

function normalizeInlineDiffText(text: string, maxChars = DEFAULT_MAX_CHARS): {
  text: string;
  truncated: boolean;
  originalCharCount: number;
} {
  const normalized = String(text ?? '').replace(/\r\n/g, '\n').replace(/\r/g, '\n').trim();
  if (normalized.length <= maxChars) {
    return { text: normalized, truncated: false, originalCharCount: normalized.length };
  }

  const headChars = Math.floor(maxChars * 0.7);
  const tailChars = Math.floor(maxChars * 0.22);
  return {
    text: [
      normalized.slice(0, headChars),
      '',
      `… diff 内容过长，已省略中间 ${Math.max(0, normalized.length - headChars - tailChars).toLocaleString()} 个字符 …`,
      '',
      normalized.slice(-tailChars),
    ].join('\n'),
    truncated: true,
    originalCharCount: normalized.length,
  };
}

function parseInlineDiffLinesSync(text: string, maxChars = DEFAULT_MAX_CHARS): Omit<Extract<InlineDiffState, { status: 'done' }>, 'status' | 'error'> {
  const normalized = normalizeInlineDiffText(text, maxChars);
  const lines = normalized.text.split('\n').map((line) => ({
    kind: classifyDiffLine(line),
    text: line || ' ',
  }));
  return {
    lines,
    truncated: normalized.truncated,
    originalCharCount: normalized.originalCharCount,
    renderedCharCount: normalized.text.length,
  };
}

function useInlineDiffLines(text: string, maxChars = DEFAULT_MAX_CHARS): InlineDiffState {
  const requestIdRef = React.useRef(0);
  const workerRef = React.useRef<Worker | null>(null);
  const [state, setState] = React.useState<InlineDiffState>(() => {
    // Do not synchronously split/classify a large diff during the first render:
    // that would defeat the worker path exactly when the component is mounted
    // from an expanded "edit file" row and would block the chat scroll thread.
    if (String(text ?? '').length >= WORKER_THRESHOLD_CHARS) {
      return { status: 'pending', lines: [], error: null, truncated: false };
    }
    const parsed = parseInlineDiffLinesSync(text, maxChars);
    return { status: 'done', error: null, ...parsed };
  });

  React.useEffect(() => {
    const requestId = requestIdRef.current + 1;
    requestIdRef.current = requestId;

    if (workerRef.current) {
      workerRef.current.terminate();
      workerRef.current = null;
    }

    if (text.length < WORKER_THRESHOLD_CHARS) {
      const parsed = parseInlineDiffLinesSync(text, maxChars);
      setState({ status: 'done', error: null, ...parsed });
      return;
    }

    setState({ status: 'pending', lines: [], error: null, truncated: false });

    let worker: Worker | null = null;
    try {
      worker = new Worker(
        new URL('../../workers/inlineDiffWorker.ts', import.meta.url),
        { type: 'module' },
      );
      workerRef.current = worker;

      worker.onmessage = (event: MessageEvent<InlineDiffWorkerResponse>) => {
        if (requestIdRef.current !== requestId || workerRef.current !== worker) return;
        const payload = event.data;
        worker?.terminate();
        workerRef.current = null;

        if (payload.type === 'done') {
          setState({
            status: 'done',
            lines: payload.lines,
            error: null,
            truncated: payload.truncated,
            originalCharCount: payload.originalCharCount,
            renderedCharCount: payload.renderedCharCount,
          });
        } else {
          setState({ status: 'error', lines: [], error: payload.error, truncated: false });
        }
      };

      worker.onerror = (event) => {
        if (requestIdRef.current !== requestId || workerRef.current !== worker) return;
        worker?.terminate();
        workerRef.current = null;
        setState({
          status: 'error',
          lines: [],
          error: event.message || 'Diff worker failed',
          truncated: false,
        });
      };

      worker.postMessage({ requestId, text, maxChars });
    } catch {
      worker?.terminate();
      if (workerRef.current === worker) {
        workerRef.current = null;
      }
      // Fallback keeps the UI functional in test or older WebView contexts
      // where module workers may be unavailable.
      const parsed = parseInlineDiffLinesSync(text, maxChars);
      setState({ status: 'done', error: null, ...parsed });
    }

    return () => {
      if (workerRef.current === worker) {
        workerRef.current?.terminate();
        workerRef.current = null;
      }
    };
  }, [maxChars, text]);

  return state;
}

function classifyDiffLine(line: string): InlineDiffKind {
  if (line.startsWith('diff --git') || line.startsWith('*** ')) return 'file';
  if (line.startsWith('@@')) return 'hunk';
  if (line.startsWith('+') && !line.startsWith('+++')) return 'add';
  if (line.startsWith('-') && !line.startsWith('---')) return 'remove';
  if (line.startsWith('+++') || line.startsWith('---') || line.startsWith('index ')) return 'meta';
  return 'context';
}

function diffLineStyle(kind: InlineDiffKind): React.CSSProperties {
  switch (kind) {
    case 'file':
      return {
        color: 'var(--text-primary)',
        background: 'rgba(255,255,255,0.035)',
        fontWeight: 700,
      };
    case 'hunk':
      return {
        color: 'var(--color-secondary)',
        background: 'rgba(6, 182, 212, 0.07)',
      };
    case 'add':
      return {
        color: '#34d399',
        background: 'rgba(16, 185, 129, 0.08)',
      };
    case 'remove':
      return {
        color: '#fb7185',
        background: 'rgba(239, 68, 68, 0.08)',
      };
    case 'meta':
      return {
        color: '#93c5fd',
        background: 'rgba(59, 130, 246, 0.055)',
      };
    default:
      return {
        color: 'var(--text-secondary)',
        background: 'transparent',
      };
  }
}

export const VirtualizedDiffBlock: React.FC<VirtualizedDiffBlockProps> = React.memo(({
  text,
  maxHeight = DEFAULT_MAX_HEIGHT,
  rowHeight = DEFAULT_ROW_HEIGHT,
  maxChars = DEFAULT_MAX_CHARS,
  lineNumberWidth = 44,
  style,
}) => {
  const state = useInlineDiffLines(text, maxChars);
  const [scrollTop, setScrollTop] = React.useState(0);
  const containerRef = React.useRef<HTMLDivElement>(null);
  const scrollRafRef = React.useRef<number | null>(null);
  const pendingScrollTopRef = React.useRef(0);

  React.useEffect(() => {
    if (containerRef.current) {
      containerRef.current.scrollTop = 0;
    }
    pendingScrollTopRef.current = 0;
    setScrollTop(0);
  }, [text]);

  React.useEffect(() => () => {
    if (scrollRafRef.current !== null) {
      window.cancelAnimationFrame(scrollRafRef.current);
      scrollRafRef.current = null;
    }
  }, []);

  const handleScroll = React.useCallback((event: React.UIEvent<HTMLDivElement>) => {
    pendingScrollTopRef.current = event.currentTarget.scrollTop;
    if (scrollRafRef.current !== null) return;
    scrollRafRef.current = window.requestAnimationFrame(() => {
      scrollRafRef.current = null;
      setScrollTop(pendingScrollTopRef.current);
    });
  }, []);

  const lines = state.lines;
  const viewportHeight = maxHeight;
  const totalHeight = Math.max(rowHeight, lines.length * rowHeight);
  const startIndex = Math.max(0, Math.floor(scrollTop / rowHeight) - OVERSCAN_ROWS);
  const endIndex = Math.min(
    lines.length,
    Math.ceil((scrollTop + viewportHeight) / rowHeight) + OVERSCAN_ROWS,
  );

  return (
    <div
      ref={containerRef}
      onScroll={handleScroll}
      style={{
        margin: '4px 0 7px 0',
        padding: '8px 10px',
        border: '1px solid rgba(255,255,255,0.08)',
        borderRadius: 7,
        background: '#08090d',
        maxHeight,
        overflow: 'auto',
        fontFamily: 'var(--font-mono, ui-monospace, SFMono-Regular, Menlo, Monaco, Consolas, monospace)',
        fontSize: 11,
        lineHeight: `${rowHeight}px`,
        ...style,
      }}
    >
      {state.status === 'pending' && (
        <div style={{ color: 'var(--text-muted)', padding: '4px 6px' }}>
          正在后台准备 diff 预览…
        </div>
      )}
      {state.status === 'error' && (
        <div style={{ color: 'var(--color-error)', padding: '4px 6px' }}>
          Diff 渲染失败：{state.error}
        </div>
      )}
      {state.status === 'done' && state.truncated && (
        <div style={{ color: 'var(--text-muted)', padding: '0 6px 6px 6px' }}>
          Diff 过长，已在后台压缩渲染（{state.originalCharCount.toLocaleString()} → {state.renderedCharCount.toLocaleString()} 字符）。
        </div>
      )}
      {state.status === 'done' && (
        <div
          style={{
            height: totalHeight,
            minWidth: 'max-content',
            position: 'relative',
          }}
        >
          {Array.from({ length: Math.max(0, endIndex - startIndex) }, (_, offset) => {
            const index = startIndex + offset;
            const line = lines[index];
            const kindStyle = diffLineStyle(line.kind);
            return (
              <div
                key={index}
                style={{
                  position: 'absolute',
                  top: index * rowHeight,
                  left: 0,
                  right: 0,
                  height: rowHeight,
                  display: 'grid',
                  gridTemplateColumns: `${lineNumberWidth}px minmax(0, 1fr)`,
                  gap: 8,
                  minWidth: 'max-content',
                  borderRadius: 3,
                  padding: '0 6px',
                  boxSizing: 'border-box',
                  ...kindStyle,
                }}
              >
                <span style={{ color: 'var(--text-muted)', opacity: 0.58, userSelect: 'none', textAlign: 'right' }}>
                  {index + 1}
                </span>
                <span style={{ whiteSpace: 'pre', tabSize: 2 }}>{line.text}</span>
              </div>
            );
          })}
        </div>
      )}
    </div>
  );
});

VirtualizedDiffBlock.displayName = 'VirtualizedDiffBlock';
