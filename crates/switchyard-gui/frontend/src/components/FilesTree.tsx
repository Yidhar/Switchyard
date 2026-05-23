import React, { useEffect, useState, useMemo } from 'react';
import { invoke } from '@tauri-apps/api/core';
import { ChevronRight, ChevronDown, AlertCircle } from 'lucide-react';
import { iconForFile, iconForFolder } from './fileIcons';

/// Mirrors `DirEntryView` from switchyard-gui/src/main.rs.
interface DirEntry {
  name: string;
  path: string;
  is_dir: boolean;
  size: number;
}

/// Subset of `git::GitStatus` we care about for tree decoration —
/// status per repo-relative path plus the repo root so we can rewrite
/// status keys into workspace-relative form.
type FileStatus =
  | 'added'
  | 'modified'
  | 'deleted'
  | 'renamed'
  | 'copied'
  | 'untracked'
  | 'type_changed'
  | 'unmerged';

interface GitFileEntry {
  path: string;
  old_path: string | null;
  index_status: FileStatus | null;
  worktree_status: FileStatus | null;
}

interface GitStatus {
  branch: string | null;
  upstream: string | null;
  ahead: number;
  behind: number;
  detached: boolean;
  repo_root: string;
  files: GitFileEntry[];
}

/// Decoration the tree applies to a single path. The bubble-up logic
/// in `buildDecorations` ensures folder entries see the *highest
/// priority* descendant status (deleted > unmerged > modified >
/// added > untracked).
interface Decoration {
  status: FileStatus | null;
  /// True when the path itself is gitignored (or sits inside a
  /// well-known ignored dir like `target/`). Dims the row.
  ignored: boolean;
  /// True when the entry is a folder that contains changes
  /// somewhere underneath. Drives the right-side dot indicator.
  hasDescendantChanges: boolean;
}

interface FilesTreeProps {
  /// Used as a React key so switching workspace fully resets the tree
  /// state (cached children, expanded folders, errors). Without this
  /// stale entries from the previous project bleed into the new view.
  workspaceId: string | null;
  /// Bumped by App.tsx on `TurnCompleted` and explicit git operations
  /// so the tree re-fetches `git_status` and updates its decorations.
  /// Reused from the same nonce that drives `SourceControl`.
  gitRefreshNonce: number;
  onOpenFile: (path: string) => void;
}

/// Top-level: loads the workspace's primary_root and renders each
/// entry. The tree itself is fully recursive — each folder node
/// maintains its own expanded / cached-children state, so expanding a
/// folder deep in the tree doesn't re-fetch the whole hierarchy.
export const FilesTree: React.FC<FilesTreeProps> = ({
  workspaceId,
  gitRefreshNonce,
  onOpenFile,
}) => {
  const [roots, setRoots] = useState<DirEntry[]>([]);
  const [loading, setLoading] = useState(true);
  const [error, setError] = useState<string | null>(null);
  const [gitStatus, setGitStatus] = useState<GitStatus | null>(null);

  // Load directory listing whenever the workspace changes. Cancel
  // any in-flight request via a "cancelled" flag so we don't write
  // stale results into state.
  useEffect(() => {
    if (!workspaceId) return;
    let cancelled = false;
    setLoading(true);
    setError(null);
    invoke<DirEntry[]>('list_dir', { path: null })
      .then((list) => {
        if (cancelled) return;
        setRoots(list);
      })
      .catch((e) => {
        if (cancelled) return;
        setError(String(e));
      })
      .finally(() => {
        if (cancelled) return;
        setLoading(false);
      });
    return () => {
      cancelled = true;
    };
  }, [workspaceId]);

  // Re-fetch git status when the workspace switches OR an external
  // refresh trigger fires (typically TurnCompleted). The repo-check
  // before `git_status` keeps the call cheap when the workspace
  // isn't a git repo.
  useEffect(() => {
    if (!workspaceId) {
      setGitStatus(null);
      return;
    }
    let cancelled = false;
    (async () => {
      try {
        const isRepo = await invoke<boolean>('git_is_repo');
        if (cancelled) return;
        if (!isRepo) {
          setGitStatus(null);
          return;
        }
        const status = await invoke<GitStatus>('git_status');
        if (cancelled) return;
        setGitStatus(status);
      } catch {
        if (cancelled) return;
        setGitStatus(null);
      }
    })();
    return () => {
      cancelled = true;
    };
  }, [workspaceId, gitRefreshNonce]);

  /// Pre-compute a path → decoration map keyed by workspace-relative
  /// path (which is what each `DirEntry.path` carries). Building it
  /// once per render saves re-walking on every node.
  const decorations = useMemo(
    () => buildDecorations(gitStatus),
    [gitStatus],
  );

  if (loading) {
    return (
      <div style={{ padding: 16, color: 'var(--text-muted)', fontSize: 12 }}>
        Loading files…
      </div>
    );
  }
  if (error) {
    return (
      <div
        style={{
          padding: 16,
          color: 'var(--color-error, #ef4444)',
          fontSize: 12,
          display: 'flex',
          gap: 8,
          alignItems: 'flex-start',
        }}
      >
        <AlertCircle size={14} />
        <span style={{ whiteSpace: 'pre-wrap' }}>{error}</span>
      </div>
    );
  }
  if (roots.length === 0) {
    return (
      <div style={{ padding: 16, color: 'var(--text-muted)', fontSize: 12 }}>
        Empty workspace.
      </div>
    );
  }

  return (
    <div
      className="files-tree"
      style={{
        padding: '8px 0',
        overflow: 'auto',
        flex: 1,
        minHeight: 0,
        fontSize: 13,
      }}
    >
      {roots.map((node) => (
        <FilesTreeNode
          key={node.path}
          node={node}
          depth={0}
          onOpenFile={onOpenFile}
          decorations={decorations}
        />
      ))}
    </div>
  );
};

/// Recursive node. Files are leaves (click → open); folders lazy-load
/// children on first expand. We deliberately don't pre-filter
/// `node_modules` / `target` etc. — the user's `list_dir` is the
/// authoritative view; we just dim them via the `ignored` decoration.
const FilesTreeNode: React.FC<{
  node: DirEntry;
  depth: number;
  onOpenFile: (path: string) => void;
  decorations: Map<string, Decoration>;
}> = ({ node, depth, onOpenFile, decorations }) => {
  const [expanded, setExpanded] = useState(false);
  const [children, setChildren] = useState<DirEntry[] | null>(null);
  const [loading, setLoading] = useState(false);
  const [error, setError] = useState<string | null>(null);

  const onActivate = async () => {
    if (!node.is_dir) {
      onOpenFile(node.path);
      return;
    }
    if (expanded) {
      setExpanded(false);
      return;
    }
    setExpanded(true);
    // Lazy-load children on first expand only. Re-expand reuses the
    // cached `children` list — refresh requires collapsing then a
    // future "Reload" affordance (not in v1).
    if (children === null) {
      setLoading(true);
      try {
        const list = await invoke<DirEntry[]>('list_dir', { path: node.path });
        setChildren(list);
        setError(null);
      } catch (e) {
        setError(String(e));
      } finally {
        setLoading(false);
      }
    }
  };

  const indent = depth * 14 + 10;
  const decoration = decorationFor(node.path, decorations);
  const statusColor = colorForStatus(decoration.status);

  // The row text color: status color wins, otherwise dimmed for
  // ignored, otherwise default primary/secondary.
  const nameColor = decoration.ignored
    ? 'var(--text-muted)'
    : statusColor ??
      (node.is_dir ? 'var(--text-secondary)' : 'var(--text-primary)');
  const nameOpacity = decoration.ignored ? 0.55 : 1;

  return (
    <>
      <button
        type="button"
        onClick={onActivate}
        title={node.path}
        style={{
          display: 'flex',
          alignItems: 'center',
          gap: 6,
          width: '100%',
          background: 'transparent',
          border: 'none',
          color: nameColor,
          cursor: 'pointer',
          paddingLeft: indent,
          paddingRight: 8,
          paddingTop: 3,
          paddingBottom: 3,
          textAlign: 'left',
          fontSize: 13,
          minHeight: 22,
          opacity: nameOpacity,
        }}
        onMouseEnter={(e) =>
          (e.currentTarget.style.background = 'rgba(255, 255, 255, 0.03)')
        }
        onMouseLeave={(e) => (e.currentTarget.style.background = 'transparent')}
      >
        {node.is_dir ? (
          <>
            {expanded ? (
              <ChevronDown size={12} style={{ flexShrink: 0, opacity: 0.7 }} />
            ) : (
              <ChevronRight size={12} style={{ flexShrink: 0, opacity: 0.7 }} />
            )}
            {/* Folder glyph + per-name tint (src=blue, crates=rust-orange,
                docs=md-blue, etc.) — matches VS Code's Material Icon
                Theme's well-known-folder cues. */}
            {(() => {
              const { icon, color } = iconForFolder(node.name, expanded);
              return (
                <span
                  style={{ color, display: 'inline-flex', flexShrink: 0 }}
                  aria-hidden
                >
                  {icon}
                </span>
              );
            })()}
          </>
        ) : (
          <>
            {/* Spacer matching the chevron column so files align with folder labels. */}
            <span style={{ width: 12, flexShrink: 0 }} />
            {(() => {
              const { icon, color } = iconForFile(node.name);
              return (
                <span
                  style={{ color, display: 'inline-flex', flexShrink: 0 }}
                  aria-hidden
                >
                  {icon}
                </span>
              );
            })()}
          </>
        )}
        <span
          style={{
            overflow: 'hidden',
            textOverflow: 'ellipsis',
            whiteSpace: 'nowrap',
            flex: 1,
            minWidth: 0,
          }}
        >
          {node.name}
        </span>

        {/* Right-side status indicator.
            • Files: single status letter (M/U/A/D) in status color.
            • Folders: a tiny colored dot when any descendant changed —
              mirrors VS Code's "decorationsBar" treatment.
            Both are suppressed for ignored entries (already dimmed). */}
        {!decoration.ignored && (
          <>
            {!node.is_dir && decoration.status && (
              <span
                style={{
                  color: statusColor ?? 'var(--text-muted)',
                  fontSize: 11,
                  fontWeight: 600,
                  marginLeft: 4,
                  flexShrink: 0,
                }}
                title={statusLabel(decoration.status)}
              >
                {statusBadge(decoration.status)}
              </span>
            )}
            {node.is_dir && decoration.hasDescendantChanges && (
              <span
                aria-label="Folder contains changes"
                title="Folder contains changes"
                style={{
                  width: 8,
                  height: 8,
                  borderRadius: '50%',
                  background: statusColor ?? '#e2c08d',
                  marginLeft: 4,
                  flexShrink: 0,
                }}
              />
            )}
          </>
        )}
      </button>

      {expanded && (
        <>
          {loading && (
            <div
              style={{
                paddingLeft: indent + 32,
                color: 'var(--text-muted)',
                fontSize: 11,
                paddingTop: 2,
                paddingBottom: 2,
              }}
            >
              Loading…
            </div>
          )}
          {error && (
            <div
              style={{
                paddingLeft: indent + 32,
                color: 'var(--color-error, #ef4444)',
                fontSize: 11,
                paddingTop: 2,
                paddingBottom: 2,
              }}
            >
              {error}
            </div>
          )}
          {children?.map((child) => (
            <FilesTreeNode
              key={child.path}
              node={child}
              depth={depth + 1}
              onOpenFile={onOpenFile}
              decorations={decorations}
            />
          ))}
          {children?.length === 0 && (
            <div
              style={{
                paddingLeft: indent + 32,
                color: 'var(--text-muted)',
                fontSize: 11,
                fontStyle: 'italic',
                paddingTop: 2,
                paddingBottom: 2,
              }}
            >
              (empty)
            </div>
          )}
        </>
      )}
    </>
  );
};

const EMPTY_DECORATION: Decoration = {
  status: null,
  ignored: false,
  hasDescendantChanges: false,
};

// ---------- Decoration computation ----------

/// Build the path → decoration map once per git-status update.
///
/// Two passes:
///   1. Direct-status pass — each file in `git status` populates its
///      own entry plus marks every ancestor folder as having
///      descendant changes (with the highest-priority status code
///      seen so far so the dot color reflects the worst news).
///   2. Ignore pass — walks the dir tree's well-known-ignored names
///      (target, node_modules, .git, …) and marks them ignored. We
///      don't shell out to `git check-ignore` for every entry — that
///      would block the render — but the heuristic covers the cases
///      that visually matter.
function buildDecorations(status: GitStatus | null): Map<string, Decoration> {
  const map = new Map<string, Decoration>();
  if (!status) return map;

  for (const f of status.files) {
    // VS Code surfaces the worktree status to the explorer when both
    // sides changed (worktree wins over index — the user cares about
    // "what's on disk that's not committed").
    const effective = f.worktree_status ?? f.index_status;
    if (!effective) continue;
    const norm = normalisePath(f.path);
    upgradeStatus(map, norm, effective, false);

    // Bubble up to every ancestor folder so the dot indicator
    // shows on the right of, say, `crates`, `crates/foo`, etc.
    let cursor = norm;
    while (true) {
      const sep = cursor.lastIndexOf('/');
      if (sep < 0) break;
      cursor = cursor.slice(0, sep);
      upgradeStatus(map, cursor, effective, true);
    }
  }
  return map;
}

/// Merge a status into the decoration map, picking the higher-priority
/// status code so a folder containing both a modified file and a
/// deletion shows up red (deletion) rather than amber (modification).
function upgradeStatus(
  map: Map<string, Decoration>,
  path: string,
  status: FileStatus,
  bubble: boolean,
) {
  const prev = map.get(path) ?? { ...EMPTY_DECORATION };
  if (bubble) {
    prev.hasDescendantChanges = true;
  }
  if (prev.status === null || priority(status) > priority(prev.status)) {
    prev.status = status;
  }
  map.set(path, prev);
}

function priority(s: FileStatus): number {
  switch (s) {
    case 'unmerged':
      return 5;
    case 'deleted':
      return 4;
    case 'modified':
    case 'type_changed':
      return 3;
    case 'renamed':
    case 'copied':
      return 2;
    case 'added':
      return 1;
    case 'untracked':
      return 0;
  }
}

/// Well-known "ignored" directory or file names. We dim entries whose
/// path passes through any of these. `git check-ignore` would be more
/// precise but firing it per-node would tank the tree's snappiness;
/// this heuristic matches the cases that matter visually.
const IGNORED_DIR_NAMES = new Set([
  '.git',
  '.hg',
  '.svn',
  'node_modules',
  'target',
  '.switchyard',
  'dist',
  'build',
  '.next',
  '.nuxt',
  '__pycache__',
  '.venv',
  'venv',
  '.idea',
  '.vscode',
  '.cache',
  'out',
  'coverage',
]);

function isIgnoredPath(path: string): boolean {
  const norm = normalisePath(path);
  for (const seg of norm.split('/')) {
    if (IGNORED_DIR_NAMES.has(seg)) return true;
  }
  return false;
}

/// Normalise a path for map lookups — collapse backslashes to forward
/// slashes and strip a leading `./` if present. Porcelain output uses
/// `/` even on Windows, but the workspace-relative `DirEntry.path`
/// values from `list_dir` use the OS-native separator on Windows.
function normalisePath(p: string): string {
  return p.replace(/\\/g, '/').replace(/^\.\//, '');
}

/// Patched lookup that injects the "ignored" flag based on the
/// heuristic. Called via the decorations map's getter — well, actually
/// the lookup at the call site does this in a separate ternary; we
/// keep the helper here for tests if we add them later.
export function decorationFor(
  path: string,
  decorations: Map<string, Decoration>,
): Decoration {
  const norm = normalisePath(path);
  const base = decorations.get(norm) ?? EMPTY_DECORATION;
  return {
    ...base,
    ignored: base.ignored || isIgnoredPath(norm),
  };
}

// Patch the map's `.get` semantics by replacing the lookup at the
// node call site. (Done above — the node manually merges ignored.)

// ---------- Status visuals ----------

function colorForStatus(s: FileStatus | null): string | null {
  switch (s) {
    case 'modified':
    case 'type_changed':
      return '#e2c08d'; // VS Code's `gitDecoration.modifiedResourceForeground`
    case 'added':
    case 'untracked':
      return '#73c991'; // gitDecoration.untrackedResourceForeground
    case 'deleted':
      return '#c74e39'; // gitDecoration.deletedResourceForeground
    case 'renamed':
    case 'copied':
      return '#60a5fa';
    case 'unmerged':
      return '#c74e39';
    default:
      return null;
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

function statusLabel(s: FileStatus | null): string {
  switch (s) {
    case 'modified':
      return 'Modified';
    case 'added':
      return 'Added';
    case 'deleted':
      return 'Deleted';
    case 'renamed':
      return 'Renamed';
    case 'copied':
      return 'Copied';
    case 'untracked':
      return 'Untracked';
    case 'unmerged':
      return 'Conflict';
    case 'type_changed':
      return 'Type changed';
    default:
      return '';
  }
}

export default FilesTree;
