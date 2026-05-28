//! Workspace file watcher — Switchyard's lightweight "git-diff"-style
//! capture for AI file edits.
//!
//! Instead of installing CLI hook scripts and parsing per-provider tool
//! invocations, we observe the workspace filesystem directly. The flow:
//!
//! 1. On workspace switch, [`FileWatcherState::watch_workspace`] swaps the
//!    underlying `notify::Watcher`, clears the in-memory baseline, then
//!    kicks a background thread that walks the workspace roots and reads
//!    every text file under a size cap into the baseline map.
//!
//! 2. [`FileWatcherState::start_turn_for`] opens a per-session capture
//!    scope and clears any leftover pending state for that session. Anything
//!    the watcher sees from here on is candidate for an AI-change artifact
//!    for each active session scope.
//!
//! 3. The notify callback runs on the watcher's own thread. For each
//!    relevant event, it records the file's **previous** baseline as the
//!    `before` (if a turn is active and this path hasn't been recorded
//!    yet this turn) and updates the baseline with the new content. If
//!    no turn is active, the baseline is updated silently — that's the
//!    user editing manually, not AI.
//!
//! 4. [`FileWatcherState::end_turn_for`] returns a list of
//!    `(path, before, after)` tuples that `run_turn` then promotes into
//!    `FileChange` artifacts in the canonical store.
//!
//! ## Limitations (by design — keep it simple)
//!
//! - **First-turn-after-workspace-switch race**: if the AI writes a file
//!   before the eager baseline scan finishes, that path's `before` will
//!   be the empty string (it shows up as a "new file" diff). Subsequent
//!   turns are accurate. We don't block workspace switch on the scan.
//!
//! - **Concurrent manual edits during an AI turn** count as AI changes
//!   (false positive). The user can dismiss them from the Canvas tab.
//!   That's a far better failure mode than missing an AI write.
//!
//! - **Binary files and very-large text files** (> [`MAX_TEXT_BYTES`])
//!   are skipped. Switchyard cares about source-code diffs, not blobs.

use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex as StdMutex};
use std::time::SystemTime;

use notify::{EventKind, RecommendedWatcher, RecursiveMode, Watcher};
use switchyard_session::Workspace;

/// Skip text-file reads above this size. AI tools rarely touch huge
/// files and a multi-MB read on every fs event would tank latency.
const MAX_TEXT_BYTES: u64 = 1_048_576; // 1 MiB

/// Cap on how many baseline files we'll cache. A large monorepo can
/// have hundreds of thousands of files; eager-loading all of them eats
/// memory we don't want to pay for. After this many entries, the eager
/// scan stops adding new paths — files touched during a turn still get
/// captured (with empty-before fallback) and join the baseline going
/// forward.
const MAX_BASELINE_ENTRIES: usize = 20_000;

/// One captured AI-driven file change. Returned in batch from
/// [`FileWatcherState::end_turn`] for promotion into `FileChange`
/// artifacts.
#[derive(Debug, Clone)]
pub struct CapturedChange {
    pub path: PathBuf,
    pub before: String,
    pub after: String,
}

/// Tauri-managed handle. The actual mutable state lives in `inner` so
/// the notify callback closure can hold an `Arc` independent of the
/// handle's lifetime.
pub struct FileWatcherState {
    inner: Arc<FileWatcherInner>,
}

struct FileWatcherInner {
    /// Last-known content per watched file. Populated eagerly at
    /// workspace switch, then maintained by the watcher callback.
    baseline: StdMutex<HashMap<PathBuf, String>>,
    /// Per-session active capture scopes. Each concurrently-running session
    /// has an independent first-observed `before` map and start timestamp, so
    /// finishing one turn cannot drain another session's watcher state.
    active_captures: StdMutex<HashMap<uuid::Uuid, CaptureScope>>,
    /// Workspace roots being watched. Cached at `watch_workspace` time
    /// so the fallback scan in `end_turn` knows where to look without
    /// re-plumbing the workspace handle through every call.
    roots: StdMutex<Vec<PathBuf>>,
    /// Holds the active watcher alive. Replaced on workspace switch.
    watcher: StdMutex<Option<RecommendedWatcher>>,
}

#[derive(Debug, Clone)]
struct CaptureScope {
    pending_before: HashMap<PathBuf, String>,
    started_at: SystemTime,
}

impl FileWatcherInner {
    fn new() -> Self {
        Self {
            baseline: StdMutex::new(HashMap::new()),
            active_captures: StdMutex::new(HashMap::new()),
            roots: StdMutex::new(Vec::new()),
            watcher: StdMutex::new(None),
        }
    }
}

impl FileWatcherState {
    pub fn new() -> Self {
        Self {
            inner: Arc::new(FileWatcherInner::new()),
        }
    }

    /// (Re)point the watcher at a new workspace. Clears all in-memory
    /// state, registers a fresh `notify` watcher across the workspace's
    /// primary + extra roots, and kicks an eager baseline-population
    /// thread. Returns immediately — the scan happens in the
    /// background.
    pub fn watch_workspace(&self, workspace: &Workspace) -> Result<(), String> {
        // Drop the previous watcher; this stops its callback thread.
        {
            let mut w = self
                .inner
                .watcher
                .lock()
                .map_err(|_| "watcher mutex poisoned")?;
            *w = None;
        }
        {
            let mut b = self
                .inner
                .baseline
                .lock()
                .map_err(|_| "baseline mutex poisoned")?;
            b.clear();
        }
        if let Ok(mut captures) = self.inner.active_captures.lock() {
            captures.clear();
        }

        let roots: Vec<PathBuf> = std::iter::once(workspace.primary_root.clone())
            .chain(workspace.extra_roots.iter().cloned())
            .filter(|p| p.is_dir())
            .collect();

        if let Ok(mut slot) = self.inner.roots.lock() {
            *slot = roots.clone();
        }

        if roots.is_empty() {
            return Ok(());
        }

        let inner_for_callback = Arc::clone(&self.inner);
        let mut watcher =
            notify::recommended_watcher(move |result: notify::Result<notify::Event>| {
                if let Ok(event) = result {
                    inner_for_callback.handle_event(event);
                }
            })
            .map_err(|e| format!("create watcher: {e}"))?;

        for root in &roots {
            if let Err(e) = watcher.watch(root, RecursiveMode::Recursive) {
                eprintln!("[file_watcher] watch {} failed: {e}", root.display());
            }
        }

        {
            let mut slot = self
                .inner
                .watcher
                .lock()
                .map_err(|_| "watcher mutex poisoned")?;
            *slot = Some(watcher);
        }

        // Eager scan in a background thread. Best-effort: if a turn
        // starts before the scan finishes, some files may show up
        // as "new" (empty before) — see module docs.
        let inner_for_scan = Arc::clone(&self.inner);
        let roots_for_scan = roots.clone();
        std::thread::spawn(move || {
            for root in &roots_for_scan {
                populate_baseline(&inner_for_scan, root);
            }
        });

        Ok(())
    }

    /// Stop watching any workspace and clear all in-memory capture
    /// state. Used when the workbench enters the VS Code-like "no
    /// workspace opened" state.
    pub fn clear_workspace(&self) -> Result<(), String> {
        {
            let mut w = self
                .inner
                .watcher
                .lock()
                .map_err(|_| "watcher mutex poisoned")?;
            *w = None;
        }
        {
            let mut b = self
                .inner
                .baseline
                .lock()
                .map_err(|_| "baseline mutex poisoned")?;
            b.clear();
        }
        if let Ok(mut captures) = self.inner.active_captures.lock() {
            captures.clear();
        }
        if let Ok(mut roots) = self.inner.roots.lock() {
            roots.clear();
        }
        Ok(())
    }

    /// Begin recording AI-driven changes for a specific session. Replaces any
    /// pending state from a prior turn for the same session (defensive —
    /// `end_turn_for` should already have drained it). Stamps the start time
    /// so the fallback scan in `end_turn_for` can filter to "files touched
    /// after this moment".
    pub fn start_turn_for(&self, session_id: uuid::Uuid) {
        let started = capture_started_at();
        if let Ok(mut captures) = self.inner.active_captures.lock() {
            captures.insert(
                session_id,
                CaptureScope {
                    pending_before: HashMap::new(),
                    started_at: started,
                },
            );
        }
    }

    /// Compatibility helper for tests and any legacy single-turn callers.
    #[cfg(test)]
    pub fn start_turn(&self) {
        self.start_turn_for(uuid::Uuid::nil());
    }

    /// Stop recording for a specific session. Returns the set of files
    /// modified during the just-ended turn, paired with their before+after
    /// content.
    ///
    /// **Two-phase capture**: first drain the notify-collected changes
    /// (fast path, populated as events came in), then sweep the
    /// workspace for any file whose mtime is newer than the capture start time
    /// — this catches edits that notify dropped or coalesced (Windows
    /// rename-replace patterns, recommended_watcher debouncer collapsing
    /// rapid sequences, etc.). The fallback is bounded by `mtime` so
    /// untouched files are skipped without a content read.
    pub fn end_turn_for(&self, session_id: uuid::Uuid) -> Vec<CapturedChange> {
        let capture = self
            .inner
            .active_captures
            .lock()
            .ok()
            .and_then(|mut captures| captures.remove(&session_id));
        let Some(capture) = capture else {
            return Vec::new();
        };

        // Phase 1: notify-collected changes.
        let pending = capture.pending_before;
        let mut accumulated: HashMap<PathBuf, (String, String)> =
            HashMap::with_capacity(pending.len());
        {
            let baseline = self.inner.baseline.lock();
            for (path, before) in pending {
                let after = baseline
                    .as_ref()
                    .ok()
                    .and_then(|b| b.get(&path).cloned())
                    .or_else(|| std::fs::read_to_string(&path).ok())
                    .unwrap_or_default();
                accumulated.insert(path, (before, after));
            }
        }

        // Phase 2: mtime-based fallback. Walk roots, look at files
        // modified after `started_at`, compare disk content to baseline.
        // Any drift not already in `accumulated` joins the result set
        // and the baseline is updated so we don't re-emit the same
        // change next turn.
        let roots = self
            .inner
            .roots
            .lock()
            .map(|g| g.clone())
            .unwrap_or_default();
        for root in &roots {
            self.inner
                .sweep_for_drift(root, capture.started_at, &mut accumulated);
        }

        accumulated
            .into_iter()
            .filter_map(|(path, (before, after))| {
                if before == after {
                    // Touch / stat-only event that didn't actually
                    // mutate content. Skip — emitting these as diffs
                    // would surface "no changes" tabs.
                    return None;
                }
                Some(CapturedChange {
                    path,
                    before,
                    after,
                })
            })
            .collect()
    }

    /// Compatibility helper for tests and any legacy single-turn callers.
    #[cfg(test)]
    pub fn end_turn(&self) -> Vec<CapturedChange> {
        self.end_turn_for(uuid::Uuid::nil())
    }
}

impl Default for FileWatcherState {
    fn default() -> Self {
        Self::new()
    }
}

impl FileWatcherInner {
    fn handle_event(&self, event: notify::Event) {
        // Only modification-style events tell us a file's bytes
        // changed. Access and Other events don't carry payload-level
        // info on most platforms; we can't act on them.
        let interested = matches!(
            event.kind,
            EventKind::Create(_) | EventKind::Modify(_) | EventKind::Remove(_)
        );
        if !interested {
            return;
        }

        // Dedupe paths within a single event (notify::Event sometimes
        // bundles a rename's from+to or two paths for a move). Normalize
        // each path before processing so the lookup against the
        // eager-scan-populated baseline doesn't miss on Windows, where
        // notify can emit `\\?\` extended-length prefixes that don't
        // appear in baseline keys derived from `read_dir`.
        let unique_paths: HashSet<PathBuf> = event
            .paths
            .into_iter()
            .map(|p| normalize_path(&p))
            .collect();
        for path in unique_paths {
            if is_ignored_path(&path) {
                continue;
            }
            self.process_path(&path, &event.kind);
        }
    }

    /// Re-read disk and update baseline. If one or more capture scopes are
    /// active and this is the first event for the path in that scope, snapshot
    /// the pre-event baseline into the scope's `pending_before` so
    /// `end_turn_for` can use it as the diff's "before".
    fn process_path(&self, path: &Path, kind: &EventKind) {
        let is_remove = matches!(kind, EventKind::Remove(_));

        // Read after-content (or empty if removed). For directories
        // and binary files we bail early.
        let after = if is_remove {
            String::new()
        } else {
            match read_text_capped(path) {
                Some(text) => text,
                None => return,
            }
        };

        let has_active_captures = self
            .active_captures
            .lock()
            .map(|captures| !captures.is_empty())
            .unwrap_or(false);

        if has_active_captures {
            let before = self
                .baseline
                .lock()
                .ok()
                .and_then(|b| b.get(path).cloned())
                .unwrap_or_default();
            if let Ok(mut captures) = self.active_captures.lock() {
                for scope in captures.values_mut() {
                    scope
                        .pending_before
                        .entry(path.to_path_buf())
                        .or_insert_with(|| before.clone());
                }
            }
        }

        // Update baseline regardless of turn state. Outside a turn,
        // this captures user edits so the next AI write has the right
        // diff baseline.
        if let Ok(mut baseline) = self.baseline.lock() {
            if is_remove {
                baseline.remove(path);
            } else {
                baseline.insert(path.to_path_buf(), after);
            }
        }
    }

    /// Recursive directory walk that compares each file's disk content
    /// to the baseline when its mtime is newer than `since`. Used as
    /// the safety net at `end_turn` for changes notify missed.
    ///
    /// Mutates `accumulated` in-place: a path already there (recorded
    /// by the notify fast path) is left untouched — its `before` is
    /// the truer value because it was snapshotted at the FIRST event.
    /// New paths get `(baseline.cloned().unwrap_or_default(), disk)`.
    /// The baseline is also updated in-place so the next turn starts
    /// from the post-write state.
    fn sweep_for_drift(
        &self,
        root: &Path,
        since: SystemTime,
        accumulated: &mut HashMap<PathBuf, (String, String)>,
    ) {
        let mut stack: Vec<PathBuf> = vec![root.to_path_buf()];
        while let Some(dir) = stack.pop() {
            if is_ignored_path(&dir) {
                continue;
            }
            let entries = match std::fs::read_dir(&dir) {
                Ok(e) => e,
                Err(_) => continue,
            };
            for entry in entries.flatten() {
                let path = entry.path();
                let file_type = match entry.file_type() {
                    Ok(t) => t,
                    Err(_) => continue,
                };
                if file_type.is_dir() {
                    if !is_ignored_path(&path) {
                        stack.push(path);
                    }
                    continue;
                }
                if !file_type.is_file() || is_ignored_path(&path) {
                    continue;
                }
                let meta = match entry.metadata() {
                    Ok(m) => m,
                    Err(_) => continue,
                };
                // mtime filter: a file untouched since the turn started
                // can't have been edited by the AI. Skip the disk read.
                let mtime = meta.modified().unwrap_or(SystemTime::UNIX_EPOCH);
                if mtime < since {
                    continue;
                }
                let key = normalize_path(&path);
                if accumulated.contains_key(&key) {
                    // The notify fast path already recorded this one,
                    // and its `before` is more accurate (snapshotted
                    // at the FIRST event). Don't overwrite.
                    continue;
                }
                let Some(content) = read_text_capped(&path) else {
                    continue;
                };
                let before = self
                    .baseline
                    .lock()
                    .ok()
                    .and_then(|b| b.get(&key).cloned())
                    .unwrap_or_default();
                if before == content {
                    // mtime bumped (e.g. `touch`) but content
                    // unchanged. Don't surface as a diff.
                    continue;
                }
                if let Ok(mut baseline) = self.baseline.lock() {
                    baseline.insert(key.clone(), content.clone());
                }
                accumulated.insert(key, (before, content));
            }
        }
    }
}

fn capture_started_at() -> SystemTime {
    // Snapshot 250ms BEFORE "now" so we're forgiving of clock skew between the
    // workspace filesystem and our wall clock — e.g. network drives or
    // VM-mounted directories.
    SystemTime::now()
        .checked_sub(std::time::Duration::from_millis(250))
        .unwrap_or_else(SystemTime::now)
}

/// Walk a directory tree depth-first and pre-populate the baseline
/// with text-file contents. Skips ignored dirs, binary files, and
/// files larger than [`MAX_TEXT_BYTES`]. Stops adding entries once
/// the baseline reaches [`MAX_BASELINE_ENTRIES`].
fn populate_baseline(inner: &Arc<FileWatcherInner>, root: &Path) {
    let mut stack: Vec<PathBuf> = vec![root.to_path_buf()];
    while let Some(dir) = stack.pop() {
        if is_ignored_path(&dir) {
            continue;
        }
        // Check baseline size; bail when full.
        if let Ok(b) = inner.baseline.lock() {
            if b.len() >= MAX_BASELINE_ENTRIES {
                return;
            }
        }
        let entries = match std::fs::read_dir(&dir) {
            Ok(e) => e,
            Err(_) => continue,
        };
        for entry in entries.flatten() {
            let path = entry.path();
            let file_type = match entry.file_type() {
                Ok(t) => t,
                Err(_) => continue,
            };
            if file_type.is_dir() {
                if !is_ignored_path(&path) {
                    stack.push(path);
                }
                continue;
            }
            if !file_type.is_file() {
                continue;
            }
            if is_ignored_path(&path) {
                continue;
            }
            let Some(content) = read_text_capped(&path) else {
                continue;
            };
            if let Ok(mut baseline) = inner.baseline.lock() {
                if baseline.len() >= MAX_BASELINE_ENTRIES {
                    return;
                }
                baseline.insert(normalize_path(&path), content);
            }
        }
    }
}

/// Strip the Windows `\\?\` extended-length prefix and resolve `.` /
/// `..` lexically so paths from different sources (notify events vs.
/// `read_dir` walks) compare equal as baseline keys.
fn normalize_path(p: &Path) -> PathBuf {
    let stripped = strip_verbatim_prefix(p);
    use std::path::Component;
    let mut out = PathBuf::new();
    for c in stripped.components() {
        match c {
            Component::CurDir => {}
            Component::ParentDir => {
                out.pop();
            }
            other => out.push(other.as_os_str()),
        }
    }
    out
}

/// Windows extended-length / NT path prefixes like `\\?\E:\foo` are
/// safe to strip back to `E:\foo` for the cases Switchyard sees (drive
/// roots, not UNC shares). On other platforms this is a no-op.
fn strip_verbatim_prefix(p: &Path) -> PathBuf {
    let s = p.to_string_lossy();
    if let Some(rest) = s.strip_prefix(r"\\?\") {
        // Only strip the simple drive-letter form; leave \\?\UNC\ alone
        // so we don't accidentally chop a UNC share path's anchor.
        if !rest.starts_with("UNC\\") {
            return PathBuf::from(rest);
        }
    }
    p.to_path_buf()
}

/// Return `Some(content)` if `path` is a text file at most
/// [`MAX_TEXT_BYTES`] in size; `None` for binaries, oversized files,
/// directories, or unreadable paths.
fn read_text_capped(path: &Path) -> Option<String> {
    let meta = std::fs::metadata(path).ok()?;
    if !meta.is_file() {
        return None;
    }
    if meta.len() > MAX_TEXT_BYTES {
        return None;
    }
    let bytes = std::fs::read(path).ok()?;
    // Cheap binary check: presence of a NUL byte in the first 8 KiB.
    // Source code never legitimately contains NUL; binaries usually do.
    let sniff = &bytes[..bytes.len().min(8192)];
    if sniff.contains(&0u8) {
        return None;
    }
    String::from_utf8(bytes).ok()
}

/// Paths under these directories never produce useful AI-change
/// diffs and would spam the watcher with build/output noise.
const IGNORED_DIRS: &[&str] = &[
    ".git",
    ".hg",
    ".svn",
    "node_modules",
    "target",
    ".switchyard",
    "dist",
    "build",
    ".next",
    ".nuxt",
    "__pycache__",
    ".venv",
    "venv",
    ".idea",
    ".vscode",
    ".cache",
    "out",
    "coverage",
];

fn is_ignored_path(path: &Path) -> bool {
    for component in path.components() {
        if let std::path::Component::Normal(name) = component {
            if let Some(s) = name.to_str() {
                if IGNORED_DIRS.contains(&s) {
                    return true;
                }
            }
        }
    }
    false
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    fn ws_at(root: PathBuf) -> Workspace {
        let mut ws = Workspace::new(root);
        ws.name = "test".to_string();
        ws
    }

    #[test]
    fn strip_verbatim_prefix_handles_drive_paths() {
        // The `\\?\` extended-length form is what `Path::canonicalize`
        // and notify both emit on Windows; the watcher's baseline keys
        // live in the unprefixed form.
        assert_eq!(
            strip_verbatim_prefix(Path::new(r"\\?\E:\Switchyard\src\main.rs")),
            PathBuf::from(r"E:\Switchyard\src\main.rs")
        );
        assert_eq!(
            strip_verbatim_prefix(Path::new(r"E:\already\fine")),
            PathBuf::from(r"E:\already\fine")
        );
    }

    #[test]
    fn strip_verbatim_prefix_leaves_unc_paths_alone() {
        // `\\?\UNC\` is a UNC-share prefix; stripping it would drop the
        // path's anchor and break later joins. The function should
        // refuse to touch it.
        let input = Path::new(r"\\?\UNC\server\share\file.rs");
        assert_eq!(strip_verbatim_prefix(input), input.to_path_buf());
    }

    #[test]
    fn normalize_path_handles_dots_and_prefix() {
        assert_eq!(
            normalize_path(Path::new(r"\\?\E:\a\b\.\c\..\d.rs")),
            PathBuf::from(r"E:\a\b\d.rs")
        );
    }

    #[test]
    fn ignored_dirs_are_skipped() {
        assert!(is_ignored_path(Path::new("/proj/node_modules/foo")));
        assert!(is_ignored_path(Path::new("/proj/target/debug/x")));
        assert!(is_ignored_path(Path::new("/proj/.git/HEAD")));
        assert!(!is_ignored_path(Path::new("/proj/src/main.rs")));
    }

    #[test]
    fn read_text_capped_returns_text_content() {
        let dir = TempDir::new().unwrap();
        let path = dir.path().join("hello.rs");
        std::fs::write(&path, "fn main() {}").unwrap();
        assert_eq!(read_text_capped(&path).as_deref(), Some("fn main() {}"));
    }

    #[test]
    fn read_text_capped_rejects_binary() {
        let dir = TempDir::new().unwrap();
        let path = dir.path().join("blob.bin");
        let mut bytes = vec![0xffu8; 100];
        bytes[50] = 0; // NUL → binary
        std::fs::write(&path, &bytes).unwrap();
        assert!(read_text_capped(&path).is_none());
    }

    #[test]
    fn read_text_capped_rejects_oversized() {
        let dir = TempDir::new().unwrap();
        let path = dir.path().join("huge.txt");
        let big = vec![b'a'; (MAX_TEXT_BYTES + 1) as usize];
        std::fs::write(&path, &big).unwrap();
        assert!(read_text_capped(&path).is_none());
    }

    #[test]
    fn populate_baseline_walks_workspace_roots() {
        let dir = TempDir::new().unwrap();
        std::fs::write(dir.path().join("a.rs"), "fn a() {}").unwrap();
        std::fs::create_dir_all(dir.path().join("src")).unwrap();
        std::fs::write(dir.path().join("src/b.rs"), "fn b() {}").unwrap();
        // Should be skipped.
        std::fs::create_dir_all(dir.path().join("target/debug")).unwrap();
        std::fs::write(dir.path().join("target/debug/skip.rs"), "x").unwrap();

        let inner = Arc::new(FileWatcherInner::new());
        populate_baseline(&inner, dir.path());

        let baseline = inner.baseline.lock().unwrap();
        assert_eq!(
            baseline.get(&dir.path().join("a.rs")).map(|s| s.as_str()),
            Some("fn a() {}")
        );
        assert_eq!(
            baseline
                .get(&dir.path().join("src/b.rs"))
                .map(|s| s.as_str()),
            Some("fn b() {}")
        );
        assert!(
            baseline
                .get(&dir.path().join("target/debug/skip.rs"))
                .is_none()
        );
    }

    #[test]
    fn process_path_outside_turn_only_updates_baseline() {
        let dir = TempDir::new().unwrap();
        let path = dir.path().join("file.rs");
        std::fs::write(&path, "v2").unwrap();

        let inner = Arc::new(FileWatcherInner::new());
        inner
            .baseline
            .lock()
            .unwrap()
            .insert(path.clone(), "v1".to_string());

        // Simulate a watcher event WITHOUT an active turn.
        inner.process_path(&path, &EventKind::Modify(notify::event::ModifyKind::Any));

        let baseline = inner.baseline.lock().unwrap();
        assert_eq!(baseline.get(&path).map(|s| s.as_str()), Some("v2"));
        let captures = inner.active_captures.lock().unwrap();
        assert!(captures.is_empty(), "no pending captures outside a turn");
    }

    #[test]
    fn process_path_during_turn_records_before_once() {
        let dir = TempDir::new().unwrap();
        let path = dir.path().join("file.rs");
        std::fs::write(&path, "v2").unwrap();

        let inner = Arc::new(FileWatcherInner::new());
        inner
            .baseline
            .lock()
            .unwrap()
            .insert(path.clone(), "v1".to_string());
        let session_id = uuid::Uuid::nil();
        inner.active_captures.lock().unwrap().insert(
            session_id,
            CaptureScope {
                pending_before: HashMap::new(),
                started_at: capture_started_at(),
            },
        );

        // First write event records the "before" from baseline.
        inner.process_path(&path, &EventKind::Modify(notify::event::ModifyKind::Any));
        assert_eq!(
            inner
                .active_captures
                .lock()
                .unwrap()
                .get(&session_id)
                .and_then(|scope| scope.pending_before.get(&path))
                .map(|s| s.as_str()),
            Some("v1")
        );

        // A second write updates the baseline but doesn't overwrite the before.
        std::fs::write(&path, "v3").unwrap();
        inner.process_path(&path, &EventKind::Modify(notify::event::ModifyKind::Any));
        assert_eq!(
            inner
                .active_captures
                .lock()
                .unwrap()
                .get(&session_id)
                .and_then(|scope| scope.pending_before.get(&path))
                .map(|s| s.as_str()),
            Some("v1"),
            "before is snapshotted on the FIRST event, not the most recent"
        );
        assert_eq!(
            inner
                .baseline
                .lock()
                .unwrap()
                .get(&path)
                .map(|s| s.as_str()),
            Some("v3"),
            "baseline tracks the latest content"
        );
    }

    #[test]
    fn end_turn_returns_before_after_pairs() {
        let dir = TempDir::new().unwrap();
        let path = dir.path().join("file.rs");

        let watcher = FileWatcherState::new();
        watcher
            .inner
            .baseline
            .lock()
            .unwrap()
            .insert(path.clone(), "old".to_string());

        watcher.start_turn();

        std::fs::write(&path, "new").unwrap();
        watcher
            .inner
            .process_path(&path, &EventKind::Modify(notify::event::ModifyKind::Any));

        let changes = watcher.end_turn();
        assert_eq!(changes.len(), 1);
        assert_eq!(changes[0].path, path);
        assert_eq!(changes[0].before, "old");
        assert_eq!(changes[0].after, "new");
    }

    #[test]
    fn end_turn_skips_unchanged_files() {
        let dir = TempDir::new().unwrap();
        let path = dir.path().join("file.rs");
        std::fs::write(&path, "same").unwrap();

        let watcher = FileWatcherState::new();
        watcher
            .inner
            .baseline
            .lock()
            .unwrap()
            .insert(path.clone(), "same".to_string());

        watcher.start_turn();
        // Trigger an event but the disk content matches the baseline
        // — this is a touch/stat-only event.
        watcher
            .inner
            .process_path(&path, &EventKind::Modify(notify::event::ModifyKind::Any));

        let changes = watcher.end_turn();
        assert!(
            changes.is_empty(),
            "touches that don't change content must not produce artifacts"
        );
    }

    #[test]
    fn end_turn_handles_file_removal() {
        let dir = TempDir::new().unwrap();
        let path = dir.path().join("gone.rs");
        // Don't actually write; we simulate a remove event with the
        // baseline carrying the prior content.

        let watcher = FileWatcherState::new();
        watcher
            .inner
            .baseline
            .lock()
            .unwrap()
            .insert(path.clone(), "had stuff".to_string());

        watcher.start_turn();
        watcher
            .inner
            .process_path(&path, &EventKind::Remove(notify::event::RemoveKind::File));

        let changes = watcher.end_turn();
        assert_eq!(changes.len(), 1);
        assert_eq!(changes[0].before, "had stuff");
        assert_eq!(changes[0].after, "");
    }

    #[test]
    fn end_turn_fallback_scan_catches_notify_misses() {
        // Simulate a turn where notify dropped the modify event but
        // mtime still shows the file was touched. The fallback scan
        // should pick it up by comparing disk content to baseline.
        let dir = TempDir::new().unwrap();
        let path = dir.path().join("missed.rs");
        std::fs::write(&path, "old content").unwrap();

        let watcher = FileWatcherState::new();
        watcher
            .inner
            .baseline
            .lock()
            .unwrap()
            .insert(normalize_path(&path), "old content".to_string());
        *watcher.inner.roots.lock().unwrap() = vec![dir.path().to_path_buf()];

        watcher.start_turn();
        // Simulate AI write without firing notify (the bug we're
        // defending against). The fallback scan should still catch it.
        std::thread::sleep(std::time::Duration::from_millis(50));
        std::fs::write(&path, "new content from AI").unwrap();

        let changes = watcher.end_turn();
        assert_eq!(changes.len(), 1, "fallback scan must pick up missed write");
        assert_eq!(changes[0].before, "old content");
        assert_eq!(changes[0].after, "new content from AI");
    }

    #[test]
    fn end_turn_fallback_skips_unchanged_files() {
        let dir = TempDir::new().unwrap();
        let path = dir.path().join("untouched.rs");
        std::fs::write(&path, "same content").unwrap();

        let watcher = FileWatcherState::new();
        watcher
            .inner
            .baseline
            .lock()
            .unwrap()
            .insert(normalize_path(&path), "same content".to_string());
        *watcher.inner.roots.lock().unwrap() = vec![dir.path().to_path_buf()];

        watcher.start_turn();
        // Bump mtime without changing content (e.g. `touch`). The
        // fallback must compare content, not just mtime.
        std::thread::sleep(std::time::Duration::from_millis(50));
        let now = std::time::SystemTime::now();
        filetime::set_file_mtime(&path, filetime::FileTime::from_system_time(now)).unwrap();

        let changes = watcher.end_turn();
        assert!(
            changes.is_empty(),
            "mtime bump without content change must not produce a diff"
        );
    }

    #[test]
    fn end_turn_fallback_captures_brand_new_files() {
        // AI creates a file that the eager scan never saw — baseline
        // doesn't have it. The fallback should emit `before = ""` so
        // the diff renders as all-add.
        let dir = TempDir::new().unwrap();
        let path = dir.path().join("brand_new.rs");

        let watcher = FileWatcherState::new();
        *watcher.inner.roots.lock().unwrap() = vec![dir.path().to_path_buf()];

        watcher.start_turn();
        std::thread::sleep(std::time::Duration::from_millis(50));
        std::fs::write(&path, "fresh from AI").unwrap();

        let changes = watcher.end_turn();
        assert_eq!(changes.len(), 1);
        assert_eq!(changes[0].before, "");
        assert_eq!(changes[0].after, "fresh from AI");
    }

    #[test]
    fn watch_workspace_swaps_underlying_watcher() {
        let dir1 = TempDir::new().unwrap();
        let dir2 = TempDir::new().unwrap();

        let watcher = FileWatcherState::new();
        watcher
            .watch_workspace(&ws_at(dir1.path().to_path_buf()))
            .unwrap();
        // Switching workspaces clears prior baseline.
        watcher
            .inner
            .baseline
            .lock()
            .unwrap()
            .insert(dir1.path().join("ghost.rs"), "stale".to_string());
        watcher
            .watch_workspace(&ws_at(dir2.path().to_path_buf()))
            .unwrap();
        // Give the eager-scan thread time to start (it walks an empty
        // dir so this should be quick), then verify the stale baseline
        // is gone.
        std::thread::sleep(std::time::Duration::from_millis(50));
        let baseline = watcher.inner.baseline.lock().unwrap();
        assert!(baseline.get(&dir1.path().join("ghost.rs")).is_none());
    }
}
