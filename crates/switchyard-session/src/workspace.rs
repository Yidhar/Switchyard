//! Workspace — the top-level container that owns sessions, files, and a
//! `primary_root` directory CLI agents are spawned in.
//!
//! Each Switchyard window operates on exactly one current workspace at a
//! time (the "single window single project" invariant). Workspaces are
//! persisted in `~/.switchyard/workspaces.json` (the index) and their own
//! data lives under `~/.switchyard/workspaces/<workspace_id>/` (the
//! store/artifacts/jobs), deliberately decoupled from the project tree so
//! switching workspaces never pollutes the user's source repo.

use std::path::PathBuf;

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use uuid::Uuid;

/// A workspace is a named context binding a set of root directories plus
/// a session history. The Switchyard process opens one workspace at a
/// time and routes all CLI spawn / file ops / hook handlers through its
/// `primary_root`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Workspace {
    pub workspace_id: Uuid,
    /// Display name. Auto-derived from `primary_root`'s leaf segment on
    /// create; user can rename. Surfaced in breadcrumb and session list
    /// header.
    pub name: String,
    /// The directory CLI agents are spawned in (their argv `cwd`). Also
    /// the default file-tree root in Files mode.
    pub primary_root: PathBuf,
    /// Additional directories the user wants visible in Files mode and
    /// readable by file commands. Spawned CLIs still get only
    /// `primary_root` as their cwd; multi-root scope is a UI affordance,
    /// not a CLI argument.
    #[serde(default)]
    pub extra_roots: Vec<PathBuf>,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

impl Workspace {
    /// Mint a fresh workspace bound to `primary_root`. Name defaults to
    /// the path's last segment (e.g. `/home/user/projects/foo` → "foo").
    pub fn new(primary_root: impl Into<PathBuf>) -> Self {
        let primary_root = primary_root.into();
        let name = primary_root
            .file_name()
            .and_then(|n| n.to_str())
            .map(|s| s.to_string())
            .unwrap_or_else(|| "Workspace".to_string());
        let now = Utc::now();
        Self {
            workspace_id: Uuid::now_v7(),
            name,
            primary_root,
            extra_roots: Vec::new(),
            created_at: now,
            updated_at: now,
        }
    }

    /// All directories the workspace cares about — `primary_root` first,
    /// then `extra_roots`. Used by file-read / file-list commands to
    /// enforce that paths stay inside the workspace's declared scope.
    pub fn all_roots(&self) -> Vec<&PathBuf> {
        std::iter::once(&self.primary_root)
            .chain(self.extra_roots.iter())
            .collect()
    }

    /// Whether `path` resolves inside any of the workspace's roots.
    /// Used to gate `read_file` / `write_file` / `list_dir` Tauri commands
    /// so they can't be hijacked to read arbitrary filesystem locations.
    pub fn contains_path(&self, path: &std::path::Path) -> bool {
        self.all_roots().iter().any(|root| path.starts_with(root))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn new_workspace_picks_path_leaf_as_name() {
        let ws = Workspace::new(PathBuf::from("/home/u/projects/foo"));
        assert_eq!(ws.name, "foo");
        assert!(ws.extra_roots.is_empty());
    }

    #[test]
    fn contains_path_matches_primary_and_extra_roots() {
        let mut ws = Workspace::new(PathBuf::from("/project"));
        ws.extra_roots.push(PathBuf::from("/scratch"));
        assert!(ws.contains_path(std::path::Path::new("/project/src/main.rs")));
        assert!(ws.contains_path(std::path::Path::new("/scratch/notes.txt")));
        assert!(!ws.contains_path(std::path::Path::new("/etc/passwd")));
    }
}
