import React, { useEffect, useState, useCallback } from 'react';
import { invoke } from '@tauri-apps/api/core';
import {
  GitBranch,
  RefreshCw,
  Plus,
  Minus,
  Trash2,
  FileText,
  FilePlus,
  FileX,
  FileEdit,
  ChevronDown,
  ChevronRight,
  AlertCircle,
  Check,
} from 'lucide-react';

/// Mirrors `git::FileStatus` from switchyard-gui/src/git.rs.
type FileStatus =
  | 'added'
  | 'modified'
  | 'deleted'
  | 'renamed'
  | 'copied'
  | 'untracked'
  | 'type_changed'
  | 'unmerged';

/// Mirrors `git::GitFileEntry`.
interface GitFileEntry {
  path: string;
  old_path: string | null;
  index_status: FileStatus | null;
  worktree_status: FileStatus | null;
}

/// Mirrors `git::GitStatus`.
interface GitStatus {
  branch: string | null;
  upstream: string | null;
  ahead: number;
  behind: number;
  detached: boolean;
  /// Absolute path of the repository's worktree root. We pair this
  /// with each entry's `path` (repo-relative) to compute the absolute
  /// path on disk — needed because `path` alone doesn't resolve
  /// correctly when the workspace's primary_root is a subdirectory
  /// of the repo.
  repo_root: string;
  files: GitFileEntry[];
}

interface SourceControlProps {
  /// Used as a React key so switching workspaces fully resets the
  /// panel (committed message, expanded sections).
  workspaceId: string | null;
  /// Refresh trigger — bump from the parent whenever a turn completes
  /// or the user explicitly asks to refresh.
  refreshNonce: number;
  /// Open a file's git diff in the Canvas. `gitPath` is repo-relative
  /// (used for the `git_file_diff` RPC); `absPath` is the resolved
  /// disk path (used for `read_file` / Canvas tab id). `staged`
  /// selects the index-vs-HEAD comparison rather than worktree-vs-HEAD.
  onOpenDiff: (absPath: string, gitPath: string, staged: boolean) => void;
}

/// VS-Code-style source-control panel: branch indicator, commit input,
/// staged + unstaged sections with per-file stage / unstage / discard
/// affordances. Talks exclusively to the `git_*` Tauri commands —
/// the panel itself holds no state besides the in-flight UI.
export const SourceControl: React.FC<SourceControlProps> = ({
  workspaceId,
  refreshNonce,
  onOpenDiff,
}) => {
  const [status, setStatus] = useState<GitStatus | null>(null);
  const [isRepo, setIsRepo] = useState<boolean | null>(null);
  const [loading, setLoading] = useState(false);
  const [error, setError] = useState<string | null>(null);
  const [commitMessage, setCommitMessage] = useState('');
  const [committing, setCommitting] = useState(false);
  const [stagedOpen, setStagedOpen] = useState(true);
  const [changesOpen, setChangesOpen] = useState(true);

  const refresh = useCallback(async () => {
    setLoading(true);
    setError(null);
    try {
      const repo = await invoke<boolean>('git_is_repo');
      setIsRepo(repo);
      if (repo) {
        const s = await invoke<GitStatus>('git_status');
        setStatus(s);
      } else {
        setStatus(null);
      }
    } catch (e) {
      setError(String(e));
    } finally {
      setLoading(false);
    }
  }, []);

  useEffect(() => {
    if (!workspaceId) return;
    void refresh();
  }, [workspaceId, refreshNonce, refresh]);

  const handleStage = async (path: string) => {
    try {
      await invoke('git_stage', { path });
      await refresh();
    } catch (e) {
      setError(String(e));
    }
  };

  const handleUnstage = async (path: string) => {
    try {
      await invoke('git_unstage', { path });
      await refresh();
    } catch (e) {
      setError(String(e));
    }
  };

  const handleDiscard = async (path: string) => {
    // Hard guard — discard is destructive and mirrors VS Code's confirm.
    if (!confirm(`Discard changes to ${path}? This cannot be undone.`)) return;
    try {
      await invoke('git_discard', { path });
      await refresh();
    } catch (e) {
      setError(String(e));
    }
  };

  const handleStageAll = async () => {
    const targets = (status?.files ?? []).filter((f) => f.worktree_status !== null);
    for (const f of targets) {
      try {
        await invoke('git_stage', { path: f.path });
      } catch (e) {
        setError(String(e));
        return;
      }
    }
    await refresh();
  };

  const handleCommit = async () => {
    if (!commitMessage.trim()) {
      setError('Commit message cannot be empty.');
      return;
    }
    setCommitting(true);
    setError(null);
    try {
      const hash = await invoke<string>('git_commit', { message: commitMessage });
      setCommitMessage('');
      // Confirmation lives in the status bar via refresh; surfacing
      // the short SHA in the error slot would be misleading.
      console.log('[source-control] committed', hash);
      await refresh();
    } catch (e) {
      setError(String(e));
    } finally {
      setCommitting(false);
    }
  };

  const handleInit = async () => {
    setError(null);
    try {
      await invoke('git_init');
      await refresh();
    } catch (e) {
      setError(String(e));
    }
  };

  // --- Empty / error states -------------------------------------------------

  if (loading && isRepo === null) {
    return (
      <div style={panelStyle}>
        <div style={messageStyle}>Loading git status…</div>
      </div>
    );
  }

  if (isRepo === false) {
    return (
      <div style={panelStyle}>
        <div style={{ ...messageStyle, textAlign: 'center', padding: 24 }}>
          <div style={{ marginBottom: 12, opacity: 0.7 }}>
            The workspace folder is not a git repository.
          </div>
          <button
            type="button"
            onClick={() => void handleInit()}
            style={primaryButtonStyle}
          >
            Initialize Repository
          </button>
          {error && <ErrorBanner text={error} />}
        </div>
      </div>
    );
  }

  if (!status) {
    return (
      <div style={panelStyle}>
        {error && <ErrorBanner text={error} />}
      </div>
    );
  }

  const staged = status.files.filter((f) => f.index_status !== null);
  const changes = status.files.filter((f) => f.worktree_status !== null);

  // --- Main panel -----------------------------------------------------------

  return (
    <div style={panelStyle}>
      {/* Header: branch + actions */}
      <div style={headerStyle}>
        <GitBranch size={14} />
        <span
          style={{
            flex: 1,
            whiteSpace: 'nowrap',
            overflow: 'hidden',
            textOverflow: 'ellipsis',
            fontWeight: 500,
          }}
          title={
            status.upstream
              ? `${status.branch} ⇄ ${status.upstream}`
              : (status.branch ?? 'detached HEAD')
          }
        >
          {status.detached ? 'detached HEAD' : (status.branch ?? '(unknown)')}
        </span>
        {(status.ahead > 0 || status.behind > 0) && (
          <span style={{ fontSize: 11, color: 'var(--text-muted)' }}>
            {status.ahead > 0 && `↑${status.ahead} `}
            {status.behind > 0 && `↓${status.behind}`}
          </span>
        )}
        <button
          type="button"
          onClick={() => void refresh()}
          title="Refresh"
          style={iconBtnStyle}
        >
          <RefreshCw size={13} className={loading ? 'spin' : ''} />
        </button>
      </div>

      {/* Commit composer */}
      <div style={composerStyle}>
        <textarea
          value={commitMessage}
          onChange={(e) => setCommitMessage(e.target.value)}
          placeholder="Message (Ctrl+Enter to commit)"
          rows={2}
          style={textareaStyle}
          onKeyDown={(e) => {
            if ((e.ctrlKey || e.metaKey) && e.key === 'Enter') {
              e.preventDefault();
              void handleCommit();
            }
          }}
        />
        <div style={{ display: 'flex', gap: 6 }}>
          <button
            type="button"
            onClick={() => void handleCommit()}
            disabled={committing || staged.length === 0 || !commitMessage.trim()}
            style={{
              ...primaryButtonStyle,
              flex: 1,
              opacity:
                committing || staged.length === 0 || !commitMessage.trim()
                  ? 0.5
                  : 1,
              cursor:
                committing || staged.length === 0 || !commitMessage.trim()
                  ? 'not-allowed'
                  : 'pointer',
            }}
          >
            <Check size={13} />
            <span>{committing ? 'Committing…' : 'Commit'}</span>
          </button>
        </div>
      </div>

      {error && <ErrorBanner text={error} />}

      {/* Sections */}
      <div style={{ overflow: 'auto', flex: 1, minHeight: 0 }}>
        <Section
          title="Staged Changes"
          count={staged.length}
          open={stagedOpen}
          onToggle={() => setStagedOpen((v) => !v)}
          headerActions={null}
        >
          {staged.map((f) => (
            <FileRow
              key={`staged-${f.path}`}
              entry={f}
              staged
              onOpenDiff={() =>
                onOpenDiff(joinPath(status.repo_root, f.path), f.path, true)
              }
              onPrimary={() => void handleUnstage(f.path)}
              primaryIcon={<Minus size={13} />}
              primaryTitle="Unstage"
            />
          ))}
        </Section>
        <Section
          title="Changes"
          count={changes.length}
          open={changesOpen}
          onToggle={() => setChangesOpen((v) => !v)}
          headerActions={
            changes.length > 0 ? (
              <button
                type="button"
                title="Stage all"
                onClick={(e) => {
                  e.stopPropagation();
                  void handleStageAll();
                }}
                style={iconBtnStyle}
              >
                <Plus size={13} />
              </button>
            ) : null
          }
        >
          {changes.map((f) => (
            <FileRow
              key={`work-${f.path}`}
              entry={f}
              staged={false}
              onOpenDiff={() =>
                onOpenDiff(joinPath(status.repo_root, f.path), f.path, false)
              }
              onPrimary={() => void handleStage(f.path)}
              primaryIcon={<Plus size={13} />}
              primaryTitle="Stage"
              onDiscard={() => void handleDiscard(f.path)}
            />
          ))}
        </Section>
        {staged.length === 0 && changes.length === 0 && (
          <div style={{ ...messageStyle, textAlign: 'center', padding: 24 }}>
            No changes.
          </div>
        )}
      </div>
    </div>
  );
};

const Section: React.FC<{
  title: string;
  count: number;
  open: boolean;
  onToggle: () => void;
  headerActions: React.ReactNode;
  children: React.ReactNode;
}> = ({ title, count, open, onToggle, headerActions, children }) => {
  return (
    <div>
      <div
        onClick={onToggle}
        style={{
          display: 'flex',
          alignItems: 'center',
          gap: 6,
          padding: '6px 10px',
          fontSize: 11,
          textTransform: 'uppercase',
          letterSpacing: '0.05em',
          color: 'var(--text-secondary)',
          cursor: 'pointer',
          userSelect: 'none',
          background: 'rgba(255, 255, 255, 0.015)',
        }}
      >
        {open ? <ChevronDown size={12} /> : <ChevronRight size={12} />}
        <span style={{ flex: 1 }}>{title}</span>
        <span
          style={{
            background: 'rgba(255, 255, 255, 0.06)',
            color: 'var(--text-secondary)',
            padding: '0 6px',
            borderRadius: 8,
            fontSize: 10,
            fontWeight: 500,
          }}
        >
          {count}
        </span>
        {headerActions}
      </div>
      {open && <div>{children}</div>}
    </div>
  );
};

const FileRow: React.FC<{
  entry: GitFileEntry;
  staged: boolean;
  onOpenDiff: () => void;
  onPrimary: () => void;
  primaryIcon: React.ReactNode;
  primaryTitle: string;
  onDiscard?: () => void;
}> = ({ entry, staged, onOpenDiff, onPrimary, primaryIcon, primaryTitle, onDiscard }) => {
  const status = staged ? entry.index_status : entry.worktree_status;
  const { icon, color, badge } = statusVisuals(status);
  const name = leafName(entry.path);
  const dir = parentName(entry.path);

  return (
    <div
      onClick={onOpenDiff}
      className="source-control-file-row"
      style={{
        display: 'flex',
        alignItems: 'center',
        gap: 6,
        padding: '4px 10px 4px 28px',
        cursor: 'pointer',
        fontSize: 13,
        position: 'relative',
      }}
      title={entry.path}
    >
      <span style={{ color, display: 'inline-flex', alignItems: 'center' }}>
        {icon}
      </span>
      <span
        style={{
          whiteSpace: 'nowrap',
          overflow: 'hidden',
          textOverflow: 'ellipsis',
          color: 'var(--text-primary)',
        }}
      >
        {name}
      </span>
      {dir && (
        <span
          style={{
            color: 'var(--text-muted)',
            fontSize: 11,
            whiteSpace: 'nowrap',
            overflow: 'hidden',
            textOverflow: 'ellipsis',
            flex: 1,
            minWidth: 0,
          }}
        >
          {dir}
        </span>
      )}
      {!dir && <div style={{ flex: 1 }} />}
      <div
        className="source-control-row-actions"
        style={{
          display: 'flex',
          gap: 2,
          opacity: 0,
          transition: 'opacity 120ms',
        }}
      >
        {onDiscard && (
          <button
            type="button"
            title="Discard changes"
            onClick={(e) => {
              e.stopPropagation();
              onDiscard();
            }}
            style={{ ...iconBtnStyle, color: 'var(--color-error, #ef4444)' }}
          >
            <Trash2 size={13} />
          </button>
        )}
        <button
          type="button"
          title={primaryTitle}
          onClick={(e) => {
            e.stopPropagation();
            onPrimary();
          }}
          style={iconBtnStyle}
        >
          {primaryIcon}
        </button>
      </div>
      <span
        style={{
          color,
          fontSize: 11,
          fontWeight: 600,
          minWidth: 14,
          textAlign: 'center',
        }}
        title={badge}
      >
        {statusBadge(status)}
      </span>
    </div>
  );
};

const ErrorBanner: React.FC<{ text: string }> = ({ text }) => (
  <div
    style={{
      margin: 8,
      padding: 8,
      border: '1px solid rgba(239, 68, 68, 0.4)',
      background: 'rgba(239, 68, 68, 0.08)',
      color: '#fca5a5',
      borderRadius: 4,
      fontSize: 12,
      display: 'flex',
      gap: 8,
      alignItems: 'flex-start',
    }}
  >
    <AlertCircle size={14} />
    <span style={{ whiteSpace: 'pre-wrap' }}>{text}</span>
  </div>
);

// ---------- Status mapping ----------

function statusVisuals(s: FileStatus | null): {
  icon: React.ReactNode;
  color: string;
  badge: string;
} {
  switch (s) {
    case 'modified':
      return {
        icon: <FileEdit size={13} />,
        color: '#f59e0b',
        badge: 'Modified',
      };
    case 'added':
      return {
        icon: <FilePlus size={13} />,
        color: '#10b981',
        badge: 'Added',
      };
    case 'deleted':
      return {
        icon: <FileX size={13} />,
        color: '#ef4444',
        badge: 'Deleted',
      };
    case 'renamed':
    case 'copied':
      return {
        icon: <FileEdit size={13} />,
        color: '#60a5fa',
        badge: s === 'renamed' ? 'Renamed' : 'Copied',
      };
    case 'untracked':
      return {
        icon: <FilePlus size={13} />,
        color: '#10b981',
        badge: 'Untracked',
      };
    case 'unmerged':
      return {
        icon: <AlertCircle size={13} />,
        color: '#ef4444',
        badge: 'Conflict',
      };
    case 'type_changed':
      return {
        icon: <FileEdit size={13} />,
        color: '#f59e0b',
        badge: 'Type changed',
      };
    default:
      return { icon: <FileText size={13} />, color: 'var(--text-muted)', badge: '' };
  }
}

function statusBadge(s: FileStatus | null): string {
  switch (s) {
    case 'modified':
      return 'M';
    case 'added':
      return 'A';
    case 'deleted':
      return 'D';
    case 'renamed':
      return 'R';
    case 'copied':
      return 'C';
    case 'untracked':
      return 'U';
    case 'unmerged':
      return '!';
    case 'type_changed':
      return 'T';
    default:
      return '';
  }
}

// ---------- Path helpers (handle / and \) ----------

function leafName(p: string): string {
  if (!p) return '';
  const n = p.replace(/\\/g, '/');
  const idx = n.lastIndexOf('/');
  return idx >= 0 ? n.slice(idx + 1) : n;
}

function parentName(p: string): string {
  if (!p) return '';
  const n = p.replace(/\\/g, '/');
  const idx = n.lastIndexOf('/');
  return idx >= 0 ? n.slice(0, idx) : '';
}

/// Join a repo-root absolute path with a repo-relative path, picking
/// the right separator from `root`'s shape. Porcelain output uses `/`
/// even on Windows, but Tauri commands want native separators —
/// preserving the root's flavor keeps the joined path consistent with
/// how the backend stores workspace.primary_root.
function joinPath(root: string, rel: string): string {
  if (!rel) return root;
  const useBackslash = root.includes('\\') && !root.includes('/');
  const sep = useBackslash ? '\\' : '/';
  const trimmedRoot = root.replace(/[/\\]+$/, '');
  const normalisedRel = useBackslash
    ? rel.replace(/\//g, '\\')
    : rel.replace(/\\/g, '/');
  return trimmedRoot + sep + normalisedRel;
}

// ---------- Styles ----------

const panelStyle: React.CSSProperties = {
  display: 'flex',
  flexDirection: 'column',
  height: '100%',
  background: 'transparent',
  color: 'var(--text-primary)',
  overflow: 'hidden',
};

const headerStyle: React.CSSProperties = {
  display: 'flex',
  alignItems: 'center',
  gap: 6,
  padding: '8px 10px',
  borderBottom: '1px solid var(--border-muted)',
  fontSize: 12,
  color: 'var(--text-primary)',
};

const composerStyle: React.CSSProperties = {
  padding: 8,
  display: 'flex',
  flexDirection: 'column',
  gap: 6,
  borderBottom: '1px solid var(--border-muted)',
};

const textareaStyle: React.CSSProperties = {
  width: '100%',
  resize: 'vertical',
  background: 'rgba(0, 0, 0, 0.25)',
  color: 'var(--text-primary)',
  border: '1px solid var(--border-muted)',
  borderRadius: 3,
  padding: '6px 8px',
  fontSize: 12,
  fontFamily: 'inherit',
  outline: 'none',
  boxSizing: 'border-box',
};

const primaryButtonStyle: React.CSSProperties = {
  display: 'inline-flex',
  alignItems: 'center',
  justifyContent: 'center',
  gap: 6,
  padding: '6px 12px',
  background: 'var(--color-primary)',
  color: '#fff',
  border: '1px solid var(--color-primary)',
  borderRadius: 3,
  fontSize: 12,
  cursor: 'pointer',
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

const messageStyle: React.CSSProperties = {
  padding: 16,
  color: 'var(--text-muted)',
  fontSize: 12,
};

export default SourceControl;

// Hover-reveal stage/unstage/discard buttons (matches VS Code).
if (typeof document !== 'undefined') {
  const id = 'switchyard-source-control-styles';
  if (!document.querySelector(`style[data-${id}]`)) {
    const s = document.createElement('style');
    s.setAttribute(`data-${id}`, 'true');
    s.textContent = `
      .source-control-file-row:hover {
        background: rgba(255, 255, 255, 0.04);
      }
      .source-control-file-row:hover .source-control-row-actions {
        opacity: 1 !important;
      }
    `;
    document.head.appendChild(s);
  }
}
