//! Workspace index — the global registry of all known workspaces for a
//! Switchyard install. Stored as a single JSON file at
//! `~/.switchyard/workspaces.json` regardless of any individual workspace's
//! `primary_root`. The "current workspace" pointer (which one this window
//! is operating on) also lives here so the next launch lands on the same
//! project.

use std::fs;
use std::io;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};
use switchyard_session::Workspace;
use uuid::Uuid;

/// On-disk shape for `workspaces.json`. The `current` field records
/// which workspace the user was last operating on so `WorkspaceIndex::load`
/// can resume the window state.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct WorkspaceIndex {
    /// All workspaces ever created (not counting deleted ones). Ordered
    /// most-recent-first by `updated_at` on load.
    pub workspaces: Vec<Workspace>,
    /// Pointer to the workspace the last open Switchyard window was
    /// looking at. `None` on first run (the UI presents a "create
    /// workspace" prompt).
    pub current: Option<Uuid>,
}

#[derive(Debug, thiserror::Error)]
pub enum WorkspaceIndexError {
    #[error("io: {0}")]
    Io(#[from] io::Error),
    #[error("json: {0}")]
    Json(#[from] serde_json::Error),
    #[error("workspace {0} not found")]
    NotFound(Uuid),
}

impl WorkspaceIndex {
    /// Load the index from disk, or return an empty default if the file
    /// doesn't exist yet.
    pub fn load(index_path: &Path) -> Result<Self, WorkspaceIndexError> {
        match fs::read_to_string(index_path) {
            Ok(s) if !s.trim().is_empty() => {
                let mut idx: Self = serde_json::from_str(&s)?;
                idx.workspaces
                    .sort_by_key(|workspace| std::cmp::Reverse(workspace.updated_at));
                Ok(idx)
            }
            Ok(_) => Ok(Self::default()),
            Err(e) if e.kind() == io::ErrorKind::NotFound => Ok(Self::default()),
            Err(e) => Err(WorkspaceIndexError::Io(e)),
        }
    }

    /// Persist to disk, creating parent dirs as needed. The write is
    /// atomic-ish: serialize first, then a single write. We don't bother
    /// with tmp+rename because the worst case is a partial write on
    /// power loss; next launch the user just re-creates a workspace.
    pub fn save(&self, index_path: &Path) -> Result<(), WorkspaceIndexError> {
        if let Some(parent) = index_path.parent() {
            fs::create_dir_all(parent)?;
        }
        let serialized = serde_json::to_string_pretty(self)?;
        fs::write(index_path, serialized)?;
        Ok(())
    }

    /// Find a workspace by id.
    pub fn get(&self, workspace_id: Uuid) -> Option<&Workspace> {
        self.workspaces
            .iter()
            .find(|w| w.workspace_id == workspace_id)
    }

    /// Mutable find — for rename / extra_roots / updated_at bumps.
    pub fn get_mut(&mut self, workspace_id: Uuid) -> Option<&mut Workspace> {
        self.workspaces
            .iter_mut()
            .find(|w| w.workspace_id == workspace_id)
    }

    /// Add a workspace and mark it current. Returns the workspace_id so
    /// callers can route their next session into it.
    pub fn insert(&mut self, workspace: Workspace) -> Uuid {
        let id = workspace.workspace_id;
        self.workspaces.insert(0, workspace);
        self.current = Some(id);
        id
    }

    /// Remove a workspace by id. Idempotent. If the deleted workspace was
    /// current, picks the next-most-recent one (or `None` if empty).
    pub fn remove(&mut self, workspace_id: Uuid) -> Option<Workspace> {
        let pos = self
            .workspaces
            .iter()
            .position(|w| w.workspace_id == workspace_id)?;
        let removed = self.workspaces.remove(pos);
        if self.current == Some(workspace_id) {
            self.current = self.workspaces.first().map(|w| w.workspace_id);
        }
        Some(removed)
    }

    /// Update the current-workspace pointer. Returns an error if the id
    /// isn't registered so the UI can't accidentally point at nothing.
    pub fn set_current(&mut self, workspace_id: Uuid) -> Result<(), WorkspaceIndexError> {
        if self.get(workspace_id).is_none() {
            return Err(WorkspaceIndexError::NotFound(workspace_id));
        }
        self.current = Some(workspace_id);
        Ok(())
    }

    /// Resolve the current workspace ref. `None` when no workspaces exist
    /// or the pointer is stale (caller should prompt the user).
    pub fn current_workspace(&self) -> Option<&Workspace> {
        self.current.and_then(|id| self.get(id))
    }
}

/// Default location of the index file. We deliberately don't put it
/// inside the user's project tree — workspaces are global to the user.
pub fn default_index_path() -> PathBuf {
    home_dir()
        .unwrap_or_else(|| PathBuf::from("."))
        .join(".switchyard")
        .join("workspaces.json")
}

/// Directory that holds a workspace's store / artifacts / jobs. This is
/// what the per-workspace [`SwitchyardConfig::store_path`] should
/// resolve to when a workspace is active.
pub fn workspace_data_dir(workspace_id: Uuid) -> PathBuf {
    home_dir()
        .unwrap_or_else(|| PathBuf::from("."))
        .join(".switchyard")
        .join("workspaces")
        .join(workspace_id.to_string())
}

fn home_dir() -> Option<PathBuf> {
    std::env::var_os("USERPROFILE")
        .or_else(|| std::env::var_os("HOME"))
        .map(PathBuf::from)
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    fn temp_index() -> (PathBuf, TempDir) {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("workspaces.json");
        (path, dir)
    }

    #[test]
    fn load_missing_returns_empty() {
        let (path, _dir) = temp_index();
        let idx = WorkspaceIndex::load(&path).unwrap();
        assert!(idx.workspaces.is_empty());
        assert!(idx.current.is_none());
    }

    #[test]
    fn save_then_load_roundtrips() {
        let (path, _dir) = temp_index();
        let mut idx = WorkspaceIndex::default();
        let w = Workspace::new(PathBuf::from("/project/foo"));
        let id = idx.insert(w);
        idx.save(&path).unwrap();

        let loaded = WorkspaceIndex::load(&path).unwrap();
        assert_eq!(loaded.workspaces.len(), 1);
        assert_eq!(loaded.current, Some(id));
        assert_eq!(loaded.workspaces[0].name, "foo");
    }

    #[test]
    fn insert_marks_new_workspace_current() {
        let mut idx = WorkspaceIndex::default();
        let first = Workspace::new(PathBuf::from("/a"));
        let first_id = idx.insert(first);
        let second = Workspace::new(PathBuf::from("/b"));
        let second_id = idx.insert(second);
        assert_eq!(idx.current, Some(second_id));
        assert_ne!(first_id, second_id);
    }

    #[test]
    fn remove_current_falls_back_to_next() {
        let mut idx = WorkspaceIndex::default();
        let a_id = idx.insert(Workspace::new(PathBuf::from("/a")));
        let b_id = idx.insert(Workspace::new(PathBuf::from("/b")));
        // b is current after both inserts; removing it falls back to a.
        idx.remove(b_id);
        assert_eq!(idx.current, Some(a_id));
        // Removing the last leaves None.
        idx.remove(a_id);
        assert!(idx.current.is_none());
    }

    #[test]
    fn set_current_rejects_unknown_id() {
        let mut idx = WorkspaceIndex::default();
        let result = idx.set_current(Uuid::now_v7());
        assert!(matches!(result, Err(WorkspaceIndexError::NotFound(_))));
    }
}
