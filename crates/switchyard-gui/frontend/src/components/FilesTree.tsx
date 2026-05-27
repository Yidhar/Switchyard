import React, { useEffect, useMemo, useState } from 'react';
import { invoke } from '@tauri-apps/api/core';
import {
  AlertCircle,
  ChevronDown,
  ChevronRight,
  FolderPlus,
  MoreHorizontal,
  RefreshCw,
} from 'lucide-react';
import type { Workspace } from '../types';
import { iconForFile, iconForFolder } from './fileIcons';
import { ContextMenu, type ContextMenuItem } from './ui/ContextMenu';

/// Mirrors `DirEntryView` from switchyard-gui/src/main.rs.
interface DirEntry {
  name: string;
  path: string;
  is_dir: boolean;
  size: number;
}

interface TreeNode extends DirEntry {
  isRoot?: boolean;
  isExtraRoot?: boolean;
  rootPath?: string;
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
  /// Active workspace. The Explorer renders root nodes explicitly so
  /// multi-root workspaces look like VS Code: primary root first, then
  /// extra roots below it.
  workspace: Workspace | null;
  /// Bumped by App.tsx on `TurnCompleted` and explicit git operations
  /// so the tree re-fetches `git_status` and updates its decorations.
  /// Reused from the same nonce that drives `SourceControl`.
  gitRefreshNonce: number;
  onOpenFile: (path: string) => void;
  onAddFolder: () => void;
  onRemoveExtraRoot: (root: string) => void;
}

/// Top-level Explorer. Unlike the first version, it renders synthetic
/// root nodes instead of only showing the primary root's children, which
/// makes workspace scope explicit and allows additional project folders.
export const FilesTree: React.FC<FilesTreeProps> = ({
  workspace,
  gitRefreshNonce,
  onOpenFile,
  onAddFolder,
  onRemoveExtraRoot,
}) => {
  const [gitStatus, setGitStatus] = useState<GitStatus | null>(null);
  const [treeRefreshNonce, setTreeRefreshNonce] = useState(0);
  const [contextMenu, setContextMenu] = useState<{
    x: number;
    y: number;
    items: ContextMenuItem[];
  } | null>(null);

  const roots = useMemo<TreeNode[]>(() => {
    if (!workspace) return [];
    return [
      {
        name: leafName(workspace.primary_root) || workspace.name || workspace.primary_root,
        path: '',
        is_dir: true,
        size: 0,
        isRoot: true,
        rootPath: workspace.primary_root,
      },
      ...workspace.extra_roots.map((root) => ({
        name: leafName(root) || root,
        path: root,
        is_dir: true,
        size: 0,
        isRoot: true,
        isExtraRoot: true,
        rootPath: root,
      })),
    ];
  }, [workspace]);

  // Re-fetch git status when the workspace switches OR an external
  // refresh trigger fires (typically TurnCompleted). The repo-check
  // before `git_status` keeps the call cheap when the workspace
  // isn't a git repo.
  useEffect(() => {
    if (!workspace) {
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
  }, [workspace?.workspace_id, gitRefreshNonce]);

  /// Pre-compute a path → decoration map keyed by workspace-relative
  /// path (which is what primary-root `DirEntry.path` carries). Extra
  /// roots may come back as absolute paths; those still get ignored-dir
  /// dimming even if git porcelain decorations are primary-root only.
  const decorations = useMemo(
    () => buildDecorations(gitStatus),
    [gitStatus],
  );

  const copyText = async (text: string) => {
    try {
      await navigator.clipboard.writeText(text);
    } catch {
      prompt('Copy this value', text);
    }
  };

  const refreshTree = () => {
    setTreeRefreshNonce((value) => value + 1);
  };

  const showContextMenu = (
    event: React.MouseEvent,
    items: ContextMenuItem[],
  ) => {
    event.preventDefault();
    event.stopPropagation();
    setContextMenu({ x: event.clientX, y: event.clientY, items });
  };

  const handleBackgroundContextMenu = (event: React.MouseEvent) => {
    showContextMenu(event, [
      {
        id: 'add-folder',
        label: workspace ? 'Add Folder to Workspace…' : 'Open Folder…',
        onSelect: onAddFolder,
      },
      { id: 'refresh', label: 'Refresh Explorer', onSelect: refreshTree },
    ]);
  };

  if (!workspace) {
    return (
      <div className="files-tree files-tree-empty">
        <div className="files-tree-header">
          <span>EXPLORER</span>
        </div>
        <div className="files-tree-empty-body">
          <AlertCircle size={18} />
          <div>No workspace opened.</div>
          <button type="button" onClick={onAddFolder}>
            <FolderPlus size={14} />
            Open Folder…
          </button>
        </div>
      </div>
    );
  }

  return (
    <div className="files-tree" onContextMenu={handleBackgroundContextMenu}>
      <div className="files-tree-header">
        <div className="files-tree-title-block">
          <span>EXPLORER</span>
          <small>
            {roots.length === 1 ? '1 folder' : `${roots.length} folders`}
          </small>
        </div>
        <div className="files-tree-header-actions">
          <button
            type="button"
            title="Add Folder to Workspace"
            onClick={onAddFolder}
          >
            <FolderPlus size={13} />
          </button>
          <button type="button" title="Refresh Explorer" onClick={refreshTree}>
            <RefreshCw size={13} />
          </button>
          <button
            type="button"
            title="Explorer Actions"
            onClick={(event) =>
              showContextMenu(event, [
                {
                  id: 'add-folder',
                  label: 'Add Folder to Workspace…',
                  onSelect: onAddFolder,
                },
                { id: 'refresh', label: 'Refresh Explorer', onSelect: refreshTree },
              ])
            }
          >
            <MoreHorizontal size={13} />
          </button>
        </div>
      </div>

      <div className="files-tree-body">
        {roots.map((node) => (
          <FilesTreeNode
            key={`${node.rootPath ?? node.path}-${treeRefreshNonce}`}
            node={node}
            depth={0}
            workspace={workspace}
            onOpenFile={onOpenFile}
            onAddFolder={onAddFolder}
            onRemoveExtraRoot={onRemoveExtraRoot}
            decorations={decorations}
            copyText={copyText}
            showContextMenu={showContextMenu}
          />
        ))}
      </div>

      {contextMenu && (
        <ContextMenu
          x={contextMenu.x}
          y={contextMenu.y}
          items={contextMenu.items}
          onClose={() => setContextMenu(null)}
        />
      )}
    </div>
  );
};

/// Recursive node. Files are leaves (click → open); folders lazy-load
/// children on first expand. Root nodes are folders too, but their label is
/// the actual workspace folder and they start expanded like VS Code.
const FilesTreeNode: React.FC<{
  node: TreeNode;
  depth: number;
  workspace: Workspace;
  onOpenFile: (path: string) => void;
  onAddFolder: () => void;
  onRemoveExtraRoot: (root: string) => void;
  decorations: Map<string, Decoration>;
  copyText: (text: string) => Promise<void>;
  showContextMenu: (event: React.MouseEvent, items: ContextMenuItem[]) => void;
}> = ({
  node,
  depth,
  workspace,
  onOpenFile,
  onAddFolder,
  onRemoveExtraRoot,
  decorations,
  copyText,
  showContextMenu,
}) => {
  const [expanded, setExpanded] = useState(Boolean(node.isRoot));
  const [children, setChildren] = useState<TreeNode[] | null>(null);
  const [loading, setLoading] = useState(false);
  const [error, setError] = useState<string | null>(null);

  const loadChildren = async (force = false) => {
    if (!node.is_dir) return;
    if (!force && children !== null) return;
    setLoading(true);
    try {
      const list = await invoke<DirEntry[]>('list_dir', { path: node.path });
      setChildren(
        list.map((entry) => ({
          ...entry,
          rootPath: node.rootPath,
          isExtraRoot: node.isExtraRoot,
        })),
      );
      setError(null);
    } catch (e) {
      setError(String(e));
    } finally {
      setLoading(false);
    }
  };

  useEffect(() => {
    if (!node.isRoot) return;
    let cancelled = false;
    setLoading(true);
    invoke<DirEntry[]>('list_dir', { path: node.path })
      .then((list) => {
        if (cancelled) return;
        setChildren(
          list.map((entry) => ({
            ...entry,
            rootPath: node.rootPath,
            isExtraRoot: node.isExtraRoot,
          })),
        );
        setError(null);
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
  }, [node.path, node.rootPath, node.isRoot, node.isExtraRoot]);

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
    await loadChildren(false);
  };

  const expandNode = async () => {
    if (!node.is_dir) return;
    setExpanded(true);
    await loadChildren(false);
  };

  const collapseNode = () => {
    if (node.is_dir) setExpanded(false);
  };

  const refreshNode = async () => {
    if (!node.is_dir) return;
    setExpanded(true);
    await loadChildren(true);
  };

  const absolutePath = absoluteNodePath(node, workspace);
  const relativePath = relativeNodePath(node, workspace);
  const decoration = decorationFor(node.path || relativePath, decorations);
  const statusColor = colorForStatus(decoration.status);
  const indent = node.isRoot ? 10 : depth * 14 + 10;

  // The row text color: status color wins, otherwise dimmed for
  // ignored, otherwise default primary/secondary.
  const nameColor = decoration.ignored
    ? 'var(--text-muted)'
    : statusColor ??
      (node.is_dir ? 'var(--text-secondary)' : 'var(--text-primary)');
  const nameOpacity = decoration.ignored ? 0.55 : 1;

  const contextItems: ContextMenuItem[] = node.is_dir
    ? [
        {
          id: 'expand-collapse',
          label: expanded ? 'Collapse' : 'Expand',
          onSelect: expanded ? collapseNode : expandNode,
        },
        { id: 'refresh', label: 'Refresh', onSelect: refreshNode },
        { id: 'sep-open', separator: true },
        {
          id: 'copy-path',
          label: 'Copy Path',
          onSelect: () => copyText(absolutePath),
        },
        {
          id: 'copy-relative-path',
          label: 'Copy Relative Path',
          onSelect: () => copyText(relativePath),
        },
        { id: 'sep-workspace', separator: true },
        {
          id: 'add-folder',
          label: 'Add Folder to Workspace…',
          onSelect: onAddFolder,
        },
        ...(node.isExtraRoot && node.rootPath
          ? [
              {
                id: 'remove-root',
                label: 'Remove Folder from Workspace',
                danger: true,
                onSelect: () => onRemoveExtraRoot(node.rootPath ?? ''),
              } satisfies ContextMenuItem,
            ]
          : []),
      ]
    : [
        { id: 'open', label: 'Open', onSelect: () => onOpenFile(node.path) },
        { id: 'sep-open', separator: true },
        {
          id: 'copy-path',
          label: 'Copy Path',
          onSelect: () => copyText(absolutePath),
        },
        {
          id: 'copy-relative-path',
          label: 'Copy Relative Path',
          onSelect: () => copyText(relativePath),
        },
      ];

  return (
    <>
      <button
        type="button"
        className={`files-tree-row ${node.isRoot ? 'is-root' : ''}`}
        onClick={onActivate}
        onContextMenu={(event) => showContextMenu(event, contextItems)}
        title={node.isRoot ? `${node.name}\n${node.rootPath}` : absolutePath}
        style={{
          paddingLeft: indent,
          color: nameColor,
          opacity: nameOpacity,
        }}
      >
        {node.is_dir ? (
          <>
            {expanded ? (
              <ChevronDown size={12} className="files-tree-chevron" />
            ) : (
              <ChevronRight size={12} className="files-tree-chevron" />
            )}
            {(() => {
              const { icon, color } = iconForFolder(node.name, expanded);
              return (
                <span
                  className="files-tree-icon"
                  style={{ color }}
                  aria-hidden
                >
                  {icon}
                </span>
              );
            })()}
          </>
        ) : (
          <>
            <span className="files-tree-chevron" />
            {(() => {
              const { icon, color } = iconForFile(node.name);
              return (
                <span
                  className="files-tree-icon"
                  style={{ color }}
                  aria-hidden
                >
                  {icon}
                </span>
              );
            })()}
          </>
        )}
        <span className="files-tree-name">
          {node.name}
          {node.isRoot && node.isExtraRoot && (
            <em className="files-tree-root-tag">extra</em>
          )}
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
                className="files-tree-status-badge"
                style={{ color: statusColor ?? 'var(--text-muted)' }}
                title={statusLabel(decoration.status)}
              >
                {statusBadge(decoration.status)}
              </span>
            )}
            {node.is_dir && decoration.hasDescendantChanges && (
              <span
                aria-label="Folder contains changes"
                title="Folder contains changes"
                className="files-tree-status-dot"
                style={{ background: statusColor ?? '#e2c08d' }}
              />
            )}
          </>
        )}
      </button>

      {expanded && (
        <>
          {loading && (
            <div
              className="files-tree-inline-state"
              style={{ paddingLeft: indent + 32 }}
            >
              Loading…
            </div>
          )}
          {error && (
            <div
              className="files-tree-inline-state is-error"
              style={{ paddingLeft: indent + 32 }}
            >
              {error}
            </div>
          )}
          {children?.map((child) => (
            <FilesTreeNode
              key={`${child.rootPath ?? ''}:${child.path}`}
              node={child}
              depth={depth + 1}
              workspace={workspace}
              onOpenFile={onOpenFile}
              onAddFolder={onAddFolder}
              onRemoveExtraRoot={onRemoveExtraRoot}
              decorations={decorations}
              copyText={copyText}
              showContextMenu={showContextMenu}
            />
          ))}
          {children?.length === 0 && (
            <div
              className="files-tree-inline-state is-empty"
              style={{ paddingLeft: indent + 32 }}
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

// ---------- Path helpers ----------

function leafName(path: string): string {
  if (!path) return '';
  const normalised = path.replace(/\\/g, '/').replace(/\/+$/, '');
  const idx = normalised.lastIndexOf('/');
  return idx >= 0 ? normalised.slice(idx + 1) : normalised;
}

function stripTrailingSeparator(path: string): string {
  return path.replace(/[\\/]+$/, '');
}

function isAbsoluteLike(path: string): boolean {
  return /^[A-Za-z]:[\\/]/.test(path) || path.startsWith('/') || path.startsWith('\\\\');
}

function joinFsPath(root: string, child: string): string {
  if (!child) return root;
  const separator = root.includes('\\') ? '\\' : '/';
  return `${stripTrailingSeparator(root)}${separator}${child.replace(/^[\\/]+/, '')}`;
}

function rootCandidates(workspace: Workspace): string[] {
  return [workspace.primary_root, ...workspace.extra_roots];
}

function absoluteNodePath(node: TreeNode, workspace: Workspace): string {
  if (node.isRoot) return node.rootPath ?? node.path;
  if (isAbsoluteLike(node.path)) return node.path;
  const root = node.rootPath || workspace.primary_root;
  return joinFsPath(root, node.path);
}

function relativeNodePath(node: TreeNode, workspace: Workspace): string {
  if (node.isRoot) return '.';
  if (!isAbsoluteLike(node.path)) return normalisePath(node.path) || '.';
  const nodePath = normalisePath(stripTrailingSeparator(node.path)).toLowerCase();
  for (const root of rootCandidates(workspace)) {
    const rootPath = normalisePath(stripTrailingSeparator(root)).toLowerCase();
    if (nodePath === rootPath) return '.';
    if (nodePath.startsWith(`${rootPath}/`)) {
      return normalisePath(node.path).slice(rootPath.length + 1) || '.';
    }
  }
  return normalisePath(node.path);
}

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
function normalisePath(path: string): string {
  return path.replace(/\\/g, '/').replace(/^\.\//, '');
}

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