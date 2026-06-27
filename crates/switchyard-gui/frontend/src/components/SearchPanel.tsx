import React, { useEffect, useMemo, useRef, useState } from 'react';
import { invoke } from '@tauri-apps/api/core';
import { ChevronDown, ChevronRight } from 'lucide-react';

/// One match from the backend `search_workspace`. `path` is workspace-relative;
/// `spans` are char-offset [start,end) ranges within `text` to highlight.
interface SearchMatch {
  path: string;
  line: number;
  text: string;
  spans: [number, number][];
}

interface SearchOptions {
  caseSensitive: boolean;
  wholeWord: boolean;
  regex: boolean;
  filename: boolean;
  maxResults?: number;
}

interface SearchPanelProps {
  /// Open a (workspace-relative) file path in the Canvas, optionally jumping to
  /// a 1-based line.
  onOpenFile: (path: string, line?: number) => void;
}

function renderHighlighted(text: string, spans: [number, number][]): React.ReactNode {
  if (!spans || spans.length === 0) return text;
  const sorted = [...spans].sort((a, b) => a[0] - b[0]);
  const out: React.ReactNode[] = [];
  let cursor = 0;
  sorted.forEach(([s, e], i) => {
    const start = Math.max(s, cursor);
    if (start > cursor) out.push(text.slice(cursor, start));
    if (e > start) {
      out.push(
        <mark
          key={i}
          style={{
            background: 'var(--color-primary, #6366f1)',
            color: '#fff',
            borderRadius: 2,
            padding: '0 1px',
          }}
        >
          {text.slice(start, e)}
        </mark>,
      );
    }
    cursor = Math.max(cursor, e);
  });
  if (cursor < text.length) out.push(text.slice(cursor));
  return out;
}

const ToggleButton: React.FC<{
  active: boolean;
  title: string;
  onClick: () => void;
  children: React.ReactNode;
}> = ({ active, title, onClick, children }) => (
  <button
    type="button"
    title={title}
    onClick={onClick}
    style={{
      width: 22,
      height: 22,
      fontSize: 11,
      fontFamily: 'var(--font-mono, monospace)',
      display: 'inline-flex',
      alignItems: 'center',
      justifyContent: 'center',
      borderRadius: 4,
      border: '1px solid transparent',
      background: active ? 'rgba(99,102,241,0.25)' : 'transparent',
      color: active ? 'var(--color-primary, #6366f1)' : 'var(--text-muted)',
      cursor: 'pointer',
    }}
  >
    {children}
  </button>
);

/// VS Code-style search + replace panel: content / filename modes, case /
/// whole-word / regex toggles, hit highlighting, collapsible per-file groups,
/// click-to-open at the matched line, and batch replace.
export const SearchPanel: React.FC<SearchPanelProps> = ({ onOpenFile }) => {
  const [query, setQuery] = useState('');
  const [replaceText, setReplaceText] = useState('');
  const [showReplace, setShowReplace] = useState(false);
  const [caseSensitive, setCaseSensitive] = useState(false);
  const [wholeWord, setWholeWord] = useState(false);
  const [regex, setRegex] = useState(false);
  const [filenameMode, setFilenameMode] = useState(false);

  const [results, setResults] = useState<SearchMatch[]>([]);
  const [loading, setLoading] = useState(false);
  const [error, setError] = useState<string | null>(null);
  const [collapsed, setCollapsed] = useState<Set<string>>(new Set());
  const [status, setStatus] = useState<string | null>(null);
  const reqRef = useRef(0);

  const options: SearchOptions = useMemo(
    () => ({ caseSensitive, wholeWord, regex, filename: filenameMode, maxResults: 1000 }),
    [caseSensitive, wholeWord, regex, filenameMode],
  );

  useEffect(() => {
    const q = query.trim();
    if (!q) {
      setResults([]);
      setError(null);
      setLoading(false);
      return;
    }
    const id = ++reqRef.current;
    setLoading(true);
    const timer = setTimeout(async () => {
      try {
        const r = await invoke<SearchMatch[]>('search_workspace', { query: q, options });
        if (reqRef.current === id) {
          setResults(r);
          setError(null);
        }
      } catch (e) {
        if (reqRef.current === id) {
          setError(String(e));
          setResults([]);
        }
      } finally {
        if (reqRef.current === id) setLoading(false);
      }
    }, 250);
    return () => clearTimeout(timer);
  }, [query, options]);

  const groups = useMemo(() => {
    const map = new Map<string, SearchMatch[]>();
    for (const r of results) {
      const arr = map.get(r.path);
      if (arr) arr.push(r);
      else map.set(r.path, [r]);
    }
    return Array.from(map.entries());
  }, [results]);

  const toggleCollapse = (path: string) => {
    setCollapsed((prev) => {
      const next = new Set(prev);
      if (next.has(path)) next.delete(path);
      else next.add(path);
      return next;
    });
  };

  const handleReplaceAll = async () => {
    const q = query.trim();
    if (!q) return;
    const total = results.length;
    if (
      !window.confirm(
        `Replace all ${total} match${total === 1 ? '' : 'es'} with "${replaceText}"? This writes to disk and cannot be undone here.`,
      )
    ) {
      return;
    }
    setStatus('Replacing…');
    try {
      const summary = await invoke<{ filesChanged: number; replacements: number }>(
        'replace_in_workspace',
        { query: q, replacement: replaceText, options },
      );
      setStatus(
        `Replaced ${summary.replacements} occurrence${summary.replacements === 1 ? '' : 's'} in ${summary.filesChanged} file${summary.filesChanged === 1 ? '' : 's'}.`,
      );
      // Re-run the search to reflect the new state.
      const id = ++reqRef.current;
      const r = await invoke<SearchMatch[]>('search_workspace', { query: q, options });
      if (reqRef.current === id) setResults(r);
    } catch (e) {
      setStatus(`Replace failed: ${String(e)}`);
    }
  };

  return (
    <div className="files-tree" style={{ display: 'flex', flexDirection: 'column', height: '100%' }}>
      <div className="files-tree-header">
        <span>SEARCH</span>
      </div>

      <div style={{ padding: '8px', display: 'flex', flexDirection: 'column', gap: 6 }}>
        <div style={{ display: 'flex', alignItems: 'center', gap: 4 }}>
          {!filenameMode && (
            <button
              type="button"
              title={showReplace ? 'Hide replace' : 'Toggle Replace'}
              onClick={() => setShowReplace((v) => !v)}
              style={{
                background: 'transparent',
                border: 'none',
                color: 'var(--text-muted)',
                cursor: 'pointer',
                padding: 0,
                width: 16,
              }}
            >
              {showReplace ? <ChevronDown size={14} /> : <ChevronRight size={14} />}
            </button>
          )}
          <input
            autoFocus
            className="settings-input settings-input-mono"
            placeholder={filenameMode ? 'Search files by name…' : 'Search'}
            value={query}
            onChange={(e) => setQuery(e.target.value)}
            style={{ flex: 1, minWidth: 0 }}
          />
          <ToggleButton active={caseSensitive} title="Match Case" onClick={() => setCaseSensitive((v) => !v)}>
            Aa
          </ToggleButton>
          <ToggleButton active={wholeWord} title="Match Whole Word" onClick={() => setWholeWord((v) => !v)}>
            ab
          </ToggleButton>
          <ToggleButton active={regex} title="Use Regular Expression" onClick={() => setRegex((v) => !v)}>
            .*
          </ToggleButton>
        </div>

        {showReplace && !filenameMode && (
          <div style={{ display: 'flex', alignItems: 'center', gap: 4, paddingLeft: 20 }}>
            <input
              className="settings-input settings-input-mono"
              placeholder="Replace"
              value={replaceText}
              onChange={(e) => setReplaceText(e.target.value)}
              style={{ flex: 1, minWidth: 0 }}
            />
            <button
              type="button"
              title="Replace All (writes to disk)"
              onClick={handleReplaceAll}
              disabled={!query.trim() || results.length === 0}
              className="btn-add-row"
              style={{ padding: '2px 8px', whiteSpace: 'nowrap' }}
            >
              Replace All
            </button>
          </div>
        )}

        <div style={{ display: 'flex', gap: 10, fontSize: 11, color: 'var(--text-muted)' }}>
          <label style={{ display: 'inline-flex', alignItems: 'center', gap: 4, cursor: 'pointer' }}>
            <input
              type="checkbox"
              checked={filenameMode}
              onChange={(e) => setFilenameMode(e.target.checked)}
            />
            Search file names
          </label>
        </div>
      </div>

      <div style={{ flex: 1, overflowY: 'auto', fontSize: 12 }}>
        {loading && <div style={{ padding: 8, color: 'var(--text-muted)' }}>Searching…</div>}
        {error && (
          <div style={{ padding: 8, color: 'var(--color-error, #f87171)' }}>{error}</div>
        )}
        {status && !error && (
          <div style={{ padding: 8, color: 'var(--text-secondary)' }}>{status}</div>
        )}
        {!loading && !error && query.trim() !== '' && results.length === 0 && (
          <div style={{ padding: 8, color: 'var(--text-muted)' }}>No results</div>
        )}
        {!loading && results.length > 0 && (
          <div style={{ padding: '0 8px 4px', color: 'var(--text-muted)' }}>
            {results.length} result{results.length === 1 ? '' : 's'} in {groups.length} file
            {groups.length === 1 ? '' : 's'}
          </div>
        )}
        {groups.map(([path, hits]) => {
          const isCollapsed = collapsed.has(path);
          return (
            <div key={path}>
              <button
                type="button"
                onClick={() =>
                  filenameMode ? onOpenFile(path) : toggleCollapse(path)
                }
                title={path}
                style={{
                  display: 'flex',
                  alignItems: 'center',
                  gap: 4,
                  width: '100%',
                  textAlign: 'left',
                  padding: '3px 8px',
                  background: 'transparent',
                  border: 'none',
                  color: 'var(--text-secondary)',
                  fontWeight: 600,
                  cursor: 'pointer',
                  whiteSpace: 'nowrap',
                  overflow: 'hidden',
                }}
              >
                {!filenameMode &&
                  (isCollapsed ? <ChevronRight size={13} /> : <ChevronDown size={13} />)}
                <span style={{ overflow: 'hidden', textOverflow: 'ellipsis' }}>{path}</span>
                {!filenameMode && (
                  <span style={{ color: 'var(--text-muted)', fontWeight: 400 }}>
                    ({hits.length})
                  </span>
                )}
              </button>
              {!filenameMode &&
                !isCollapsed &&
                hits.map((h, idx) => (
                  <button
                    key={`${path}:${h.line}:${idx}`}
                    type="button"
                    onClick={() => onOpenFile(path, h.line)}
                    title={`${path}:${h.line}`}
                    style={{
                      display: 'flex',
                      gap: 6,
                      width: '100%',
                      textAlign: 'left',
                      padding: '2px 8px 2px 24px',
                      background: 'transparent',
                      border: 'none',
                      color: 'var(--text-primary)',
                      cursor: 'pointer',
                      whiteSpace: 'pre',
                      overflow: 'hidden',
                      textOverflow: 'ellipsis',
                      fontFamily: 'var(--font-mono, monospace)',
                      fontSize: 12,
                    }}
                  >
                    <span style={{ color: 'var(--text-muted)', minWidth: 34, flexShrink: 0 }}>
                      {h.line}
                    </span>
                    <span style={{ overflow: 'hidden', textOverflow: 'ellipsis' }}>
                      {renderHighlighted(h.text, h.spans)}
                    </span>
                  </button>
                ))}
            </div>
          );
        })}
      </div>
    </div>
  );
};

export default SearchPanel;
