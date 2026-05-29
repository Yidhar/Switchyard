//! Git source-control bridge — Switchyard's "Source Control" panel
//! backend. Shells out to the user's `git` CLI rather than linking
//! libgit2: it's one fewer C dep, supports the same range of repos
//! the user already trusts, and works with custom credential helpers /
//! `core.sshCommand` / etc. without re-implementing any of that.
//!
//! ## Why porcelain v1?
//!
//! Both v1 and v2 are stable formats. We pick v1 because:
//! - Single line per file, two-char status code — straightforward to parse.
//! - `--branch` adds one extra `## branch...upstream [ahead/behind]` line.
//! - `-z` swaps `\n` separators for `\0`, making non-ASCII filenames safe.
//!
//! v2 carries more metadata (file modes, OIDs) but Switchyard's UI
//! doesn't surface any of it, so the simpler parser wins.
//!
//! ## Error model
//!
//! Every function returns `Result<_, String>`. We deliberately bubble
//! up git's stderr verbatim — when a commit fails because of a hook,
//! or staging fails because of a path outside the repo, the user
//! benefits from seeing exactly what git said.

use std::path::{Path, PathBuf};
use std::process::Command;

use serde::Serialize;

#[cfg(windows)]
const CREATE_NO_WINDOW: u32 = 0x08000000;

fn suppress_windows_console(command: &mut Command) {
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        command.creation_flags(CREATE_NO_WINDOW);
    }
    #[cfg(not(windows))]
    {
        let _ = command;
    }
}

/// A two-character porcelain v1 status code split into staged + worktree.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum FileStatus {
    /// In index but not yet committed.
    Added,
    /// Tracked and modified.
    Modified,
    /// Tracked and deleted.
    Deleted,
    /// Renamed (the new path lives on the entry; old path in `old_path`).
    Renamed,
    /// Copied (rare; treated the same way as Renamed for display).
    Copied,
    /// File present on disk but not yet under git's control.
    Untracked,
    /// Type changed (e.g. file → symlink). Rare.
    TypeChanged,
    /// In a merge conflict — needs manual resolution.
    Unmerged,
}

/// One row in either the "Staged Changes" or "Changes" section of the
/// source-control panel. `index_status` describes the staged side and
/// `worktree_status` describes the working-tree side; either can be
/// `None` when the file is only changed on one side (most common case).
#[derive(Debug, Clone, Serialize)]
pub struct GitFileEntry {
    /// Path relative to the repo root.
    pub path: String,
    /// Set when this entry is a rename / copy.
    pub old_path: Option<String>,
    /// Staged change status, if any. Drives the "Staged Changes"
    /// section. `None` means the file isn't staged.
    pub index_status: Option<FileStatus>,
    /// Working-tree change status, if any. Drives the "Changes" section.
    /// `None` means the working tree matches the index.
    pub worktree_status: Option<FileStatus>,
}

/// Branch + upstream + divergence info, plus the changed files.
#[derive(Debug, Clone, Serialize, Default)]
pub struct GitStatus {
    pub branch: Option<String>,
    pub upstream: Option<String>,
    pub ahead: u32,
    pub behind: u32,
    /// True for a detached-HEAD repo. The UI grays out push/pull
    /// affordances when set.
    pub detached: bool,
    /// Absolute path of the repository's worktree root. File entries'
    /// `path` fields are relative to THIS — not the workspace's
    /// `primary_root`, which may be a subdirectory of the repo. The
    /// frontend joins `repo_root + entry.path` to get a path it can
    /// pass to `read_file` / `write_file`.
    pub repo_root: String,
    /// Every file the porcelain output listed. The frontend buckets
    /// them into Staged vs Changes based on which sides are populated.
    pub files: Vec<GitFileEntry>,
}

/// HEAD content + working-tree content for a single file, ready to
/// feed into the Canvas's unified-diff renderer. `head` is empty for
/// untracked / newly-added files.
#[derive(Debug, Clone, Serialize)]
pub struct GitFileDiff {
    pub path: String,
    pub head: String,
    pub working: String,
    /// `staged` true means `head` is the index (`git show :path`)
    /// rather than `HEAD:path` — used to render the per-file staged
    /// diff in the "Staged Changes" section.
    pub staged: bool,
}

/// Quickly check whether `cwd` is inside a git worktree. Used to
/// decide whether to mount the SourceControl panel or show the
/// "Initialize repository" empty state. Runs from `cwd` directly
/// (the only operation that does — every other call uses the resolved
/// repo root as cwd so path arguments line up with porcelain output).
pub fn is_repo(cwd: &Path) -> bool {
    run(cwd, &["rev-parse", "--is-inside-work-tree"])
        .map(|s| s.trim() == "true")
        .unwrap_or(false)
}

/// Return the repo root (top-level worktree directory) for `cwd`.
/// Every other operation in this module runs from the repo root so
/// porcelain-relative paths and command arguments stay consistent.
pub fn repo_root(cwd: &Path) -> Result<PathBuf, String> {
    run(cwd, &["rev-parse", "--show-toplevel"]).map(|s| PathBuf::from(s.trim()))
}

/// Resolve the cwd to use for path-bearing git operations. Always the
/// repo root, so `git add foo/bar.rs` looks at `<repo-root>/foo/bar.rs`
/// — the same path porcelain output reports. When the workspace's
/// `primary_root` is a subdirectory of the repo (very common — e.g.
/// `~/code/myproject/crates/foo`), using `primary_root` as cwd would
/// re-join the porcelain path onto the subdir, causing the
/// `crates/foo/crates/foo/...` duplication bug the user hit.
fn git_cwd(primary_root: &Path) -> Result<PathBuf, String> {
    repo_root(primary_root)
}

/// Full status summary. The frontend calls this on mount, on
/// TurnCompleted, and on explicit refresh.
pub fn status(primary_root: &Path) -> Result<GitStatus, String> {
    let cwd = git_cwd(primary_root)?;
    let raw = run(
        &cwd,
        &[
            "status",
            "--porcelain=v1",
            "--branch",
            "--untracked-files=all",
            "-z",
        ],
    )?;
    let mut status = parse_porcelain_v1(&raw)?;
    status.repo_root = cwd.to_string_lossy().into_owned();
    Ok(status)
}

/// Pull the file's content at HEAD (or at the index when `staged` is
/// true) plus the current on-disk content. Frontend pipes both into
/// the existing diff renderer.
pub fn file_diff(primary_root: &Path, path: &str, staged: bool) -> Result<GitFileDiff, String> {
    let cwd = git_cwd(primary_root)?;
    let head_ref = if staged {
        format!(":{path}")
    } else {
        format!("HEAD:{path}")
    };
    // `git show` failing usually means the file doesn't exist at HEAD
    // (e.g. brand-new file) — that's not an error from the UI's
    // perspective, it's "the before-content is empty".
    let head = run(&cwd, &["show", &head_ref]).unwrap_or_default();

    let abs = cwd.join(path);
    let working = std::fs::read_to_string(&abs).unwrap_or_default();

    Ok(GitFileDiff {
        path: path.to_string(),
        head,
        working,
        staged,
    })
}

/// Stage a single file (`git add <path>`). Works for new + modified +
/// deleted (`git add` understands all three).
pub fn stage(primary_root: &Path, path: &str) -> Result<(), String> {
    let cwd = git_cwd(primary_root)?;
    run(&cwd, &["add", "--", path]).map(|_| ())
}

/// Unstage a single file (`git restore --staged <path>`). Leaves the
/// working tree untouched.
pub fn unstage(primary_root: &Path, path: &str) -> Result<(), String> {
    let cwd = git_cwd(primary_root)?;
    run(&cwd, &["restore", "--staged", "--", path]).map(|_| ())
}

/// Discard working-tree changes for a file (`git restore <path>` for
/// tracked; for untracked we just delete the file). Equivalent to
/// VS Code's "Discard Changes" prompt.
pub fn discard(primary_root: &Path, path: &str) -> Result<(), String> {
    let cwd = git_cwd(primary_root)?;
    // Tracked files: restore from HEAD. Untracked: delete (git restore
    // doesn't touch untracked files, so we'd silently no-op and leave
    // the file on disk).
    let is_tracked = run(&cwd, &["ls-files", "--error-unmatch", "--", path]).is_ok();
    if is_tracked {
        run(&cwd, &["restore", "--", path]).map(|_| ())
    } else {
        let abs = cwd.join(path);
        std::fs::remove_file(&abs).map_err(|e| format!("delete {}: {e}", abs.display()))
    }
}

/// Commit staged changes with `message`. Returns the new commit hash.
/// Failure surfaces git's stderr verbatim (hook rejection, signing
/// failure, "nothing to commit" — all useful to the user).
pub fn commit(primary_root: &Path, message: &str) -> Result<String, String> {
    let cwd = git_cwd(primary_root)?;
    run(&cwd, &["commit", "-m", message])?;
    run(&cwd, &["rev-parse", "HEAD"]).map(|s| s.trim().to_string())
}

/// Initialise a new git repository in `cwd`. Used by the "Initialize
/// Repository" affordance on the SourceControl empty state. Runs from
/// `primary_root` directly (there's no repo to resolve yet).
pub fn init(primary_root: &Path) -> Result<(), String> {
    run(primary_root, &["init"]).map(|_| ())
}

// ---------------------------------------------------------------------------
// Internals
// ---------------------------------------------------------------------------

/// Run `git <args>` in `cwd`. Returns stdout on success, formatted
/// stderr on failure. We pipe stdout through `String::from_utf8_lossy`
/// because filenames on Windows are technically WTF-8; lossy is fine
/// for UI display.
fn run(cwd: &Path, args: &[&str]) -> Result<String, String> {
    let mut cmd = Command::new("git");
    suppress_windows_console(&mut cmd);
    cmd.arg("-C").arg(cwd);
    for a in args {
        cmd.arg(a);
    }
    // Suppress git's interactive credential helpers — the GUI has no
    // way to forward a TTY prompt, and hanging is worse than failing.
    cmd.env("GIT_TERMINAL_PROMPT", "0");
    let output = cmd
        .output()
        .map_err(|e| format!("failed to spawn git: {e}"))?;
    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        let stdout = String::from_utf8_lossy(&output.stdout);
        let detail = if stderr.is_empty() { stdout } else { stderr };
        return Err(format!("git {}: {}", args.join(" "), detail.trim()));
    }
    Ok(String::from_utf8_lossy(&output.stdout).into_owned())
}

/// Parse `git status --porcelain=v1 --branch -z` output. Format:
///
/// - `## branch...upstream [ahead N, behind M]` (optional bracket part)
/// - For each file: `XY path\0` (X = index status, Y = worktree status)
///   - Renames: `R  new\0old\0` — two NUL-separated paths in one record
///
/// The `\n` separator after `##` is preserved when `-z` is set; only
/// the file records use `\0`. We slice the branch line off first, then
/// split the rest on `\0`.
fn parse_porcelain_v1(raw: &str) -> Result<GitStatus, String> {
    let mut status = GitStatus::default();
    let mut rest = raw;

    // Branch info — single record present when --branch was passed.
    // With `-z`, git terminates this header with NUL just like the
    // file records (older docs said `\n`; observed behavior on
    // 2.x is NUL). Accept whichever the host git emits.
    if rest.starts_with("## ") {
        let nl = rest.find(['\n', '\0']).unwrap_or(rest.len());
        let header = rest[3..nl].trim_end_matches('\r');
        parse_branch_header(header, &mut status);
        rest = &rest[(nl + 1).min(rest.len())..];
    }

    // File records — '\0'-separated. We walk a sliding iterator
    // because rename records consume two records (new + old).
    let mut parts = rest.split('\0').filter(|p| !p.is_empty()).peekable();
    while let Some(record) = parts.next() {
        let bytes = record.as_bytes();
        if bytes.len() < 3 {
            // Too short to be valid — skip.
            continue;
        }
        let index_char = bytes[0] as char;
        let worktree_char = bytes[1] as char;
        let path = &record[3..]; // skip "XY "
        let (index_status, worktree_status) = (map_status(index_char), map_status(worktree_char));

        // Renames / copies (R or C in either side) carry an extra
        // record with the old path. Pull it off.
        let needs_old =
            index_char == 'R' || index_char == 'C' || worktree_char == 'R' || worktree_char == 'C';
        let old_path = if needs_old {
            parts.next().map(|s| s.to_string())
        } else {
            None
        };

        status.files.push(GitFileEntry {
            path: path.to_string(),
            old_path,
            index_status,
            worktree_status,
        });
    }

    Ok(status)
}

/// Parse the line after `## ` from a porcelain --branch header.
/// Examples:
///   `main`                                       — no upstream
///   `main...origin/main`                         — clean
///   `main...origin/main [ahead 1]`               — ahead only
///   `main...origin/main [ahead 1, behind 2]`     — both
///   `HEAD (no branch)`                           — detached HEAD
///   `No commits yet on main`                     — empty repo
fn parse_branch_header(header: &str, out: &mut GitStatus) {
    if header.starts_with("HEAD (no branch)") {
        out.detached = true;
        return;
    }
    if let Some(rest) = header.strip_prefix("No commits yet on ") {
        out.branch = Some(rest.trim().to_string());
        return;
    }

    let (refs, brackets) = match header.find(" [") {
        Some(idx) => (&header[..idx], Some(&header[idx + 2..])),
        None => (header, None),
    };
    if let Some(sep) = refs.find("...") {
        out.branch = Some(refs[..sep].to_string());
        out.upstream = Some(refs[sep + 3..].to_string());
    } else {
        out.branch = Some(refs.to_string());
    }
    if let Some(brackets) = brackets {
        // brackets ends with ']' — strip it for parsing.
        let trimmed = brackets.strip_suffix(']').unwrap_or(brackets);
        for part in trimmed.split(", ") {
            if let Some(n) = part.strip_prefix("ahead ") {
                out.ahead = n.trim().parse().unwrap_or(0);
            } else if let Some(n) = part.strip_prefix("behind ") {
                out.behind = n.trim().parse().unwrap_or(0);
            }
        }
    }
}

fn map_status(c: char) -> Option<FileStatus> {
    match c {
        ' ' => None,
        'M' => Some(FileStatus::Modified),
        'A' => Some(FileStatus::Added),
        'D' => Some(FileStatus::Deleted),
        'R' => Some(FileStatus::Renamed),
        'C' => Some(FileStatus::Copied),
        'T' => Some(FileStatus::TypeChanged),
        'U' => Some(FileStatus::Unmerged),
        '?' => Some(FileStatus::Untracked),
        '!' => None, // ignored — we don't surface these
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_clean_repo_with_no_changes() {
        let raw = "## main...origin/main\n";
        let s = parse_porcelain_v1(raw).unwrap();
        assert_eq!(s.branch.as_deref(), Some("main"));
        assert_eq!(s.upstream.as_deref(), Some("origin/main"));
        assert_eq!(s.ahead, 0);
        assert_eq!(s.behind, 0);
        assert!(s.files.is_empty());
    }

    #[test]
    fn parse_branch_with_ahead_and_behind() {
        let raw = "## feature...origin/feature [ahead 3, behind 1]\n";
        let s = parse_porcelain_v1(raw).unwrap();
        assert_eq!(s.branch.as_deref(), Some("feature"));
        assert_eq!(s.ahead, 3);
        assert_eq!(s.behind, 1);
    }

    #[test]
    fn parse_branch_ahead_only() {
        let raw = "## main...origin/main [ahead 2]\n";
        let s = parse_porcelain_v1(raw).unwrap();
        assert_eq!(s.ahead, 2);
        assert_eq!(s.behind, 0);
    }

    #[test]
    fn parse_detached_head() {
        let raw = "## HEAD (no branch)\n";
        let s = parse_porcelain_v1(raw).unwrap();
        assert!(s.detached);
        assert!(s.branch.is_none());
    }

    #[test]
    fn parse_empty_repo_no_commits() {
        let raw = "## No commits yet on main\n";
        let s = parse_porcelain_v1(raw).unwrap();
        assert_eq!(s.branch.as_deref(), Some("main"));
    }

    #[test]
    fn parse_worktree_modified_file() {
        let raw = "## main\n M src/foo.rs\0";
        let s = parse_porcelain_v1(raw).unwrap();
        assert_eq!(s.files.len(), 1);
        let f = &s.files[0];
        assert_eq!(f.path, "src/foo.rs");
        assert_eq!(f.index_status, None);
        assert_eq!(f.worktree_status, Some(FileStatus::Modified));
    }

    #[test]
    fn parse_staged_added_file() {
        let raw = "## main\nA  new.rs\0";
        let s = parse_porcelain_v1(raw).unwrap();
        let f = &s.files[0];
        assert_eq!(f.index_status, Some(FileStatus::Added));
        assert_eq!(f.worktree_status, None);
    }

    #[test]
    fn parse_modified_in_both_index_and_worktree() {
        let raw = "## main\nMM src/foo.rs\0";
        let s = parse_porcelain_v1(raw).unwrap();
        let f = &s.files[0];
        assert_eq!(f.index_status, Some(FileStatus::Modified));
        assert_eq!(f.worktree_status, Some(FileStatus::Modified));
    }

    #[test]
    fn parse_untracked_file() {
        let raw = "## main\n?? brand_new.md\0";
        let s = parse_porcelain_v1(raw).unwrap();
        let f = &s.files[0];
        assert_eq!(f.worktree_status, Some(FileStatus::Untracked));
        assert_eq!(f.path, "brand_new.md");
    }

    #[test]
    fn parse_rename_consumes_two_records() {
        // R<space> new\0old\0
        let raw = "## main\nR  src/new.rs\0src/old.rs\0 M unrelated.rs\0";
        let s = parse_porcelain_v1(raw).unwrap();
        assert_eq!(s.files.len(), 2);
        let r = &s.files[0];
        assert_eq!(r.path, "src/new.rs");
        assert_eq!(r.old_path.as_deref(), Some("src/old.rs"));
        assert_eq!(r.index_status, Some(FileStatus::Renamed));
        let m = &s.files[1];
        assert_eq!(m.path, "unrelated.rs");
        assert_eq!(m.worktree_status, Some(FileStatus::Modified));
    }

    #[test]
    fn parse_deleted_in_worktree() {
        let raw = "## main\n D src/gone.rs\0";
        let s = parse_porcelain_v1(raw).unwrap();
        let f = &s.files[0];
        assert_eq!(f.worktree_status, Some(FileStatus::Deleted));
    }

    #[test]
    fn parse_multiple_files() {
        let raw = "## main\n M a.rs\0?? b.rs\0A  c.rs\0";
        let s = parse_porcelain_v1(raw).unwrap();
        assert_eq!(s.files.len(), 3);
        assert_eq!(s.files[0].path, "a.rs");
        assert_eq!(s.files[1].path, "b.rs");
        assert_eq!(s.files[2].path, "c.rs");
    }

    /// Integration-ish tests using a real git binary + temp repo.
    /// Skipped when git isn't on PATH so CI environments without git
    /// (rare for our team) don't fail.
    fn skip_if_no_git() -> bool {
        Command::new("git")
            .arg("--version")
            .output()
            .ok()
            .map(|o| !o.status.success())
            .unwrap_or(true)
    }

    fn init_temp_repo() -> tempfile::TempDir {
        let dir = tempfile::tempdir().unwrap();
        let cwd = dir.path();
        // Use --initial-branch=main so the tests don't care which
        // default branch the host git is configured for.
        Command::new("git")
            .args(["init", "--initial-branch=main"])
            .current_dir(cwd)
            .output()
            .unwrap();
        Command::new("git")
            .args(["config", "user.email", "test@example.com"])
            .current_dir(cwd)
            .output()
            .unwrap();
        Command::new("git")
            .args(["config", "user.name", "Test"])
            .current_dir(cwd)
            .output()
            .unwrap();
        Command::new("git")
            .args(["config", "commit.gpgsign", "false"])
            .current_dir(cwd)
            .output()
            .unwrap();
        dir
    }

    #[test]
    fn status_reports_untracked_then_staged_then_committed() {
        if skip_if_no_git() {
            return;
        }
        let dir = init_temp_repo();
        let cwd = dir.path();

        std::fs::write(cwd.join("hello.txt"), "hi").unwrap();

        let s = status(cwd).unwrap();
        assert_eq!(
            s.files
                .iter()
                .find(|f| f.path == "hello.txt")
                .and_then(|f| f.worktree_status),
            Some(FileStatus::Untracked),
        );

        stage(cwd, "hello.txt").unwrap();
        let s = status(cwd).unwrap();
        assert_eq!(
            s.files
                .iter()
                .find(|f| f.path == "hello.txt")
                .and_then(|f| f.index_status),
            Some(FileStatus::Added),
        );

        commit(cwd, "first commit").unwrap();
        let s = status(cwd).unwrap();
        assert!(s.files.is_empty(), "after commit, no pending changes");
    }

    #[test]
    fn file_diff_returns_head_and_working() {
        if skip_if_no_git() {
            return;
        }
        let dir = init_temp_repo();
        let cwd = dir.path();
        std::fs::write(cwd.join("a.txt"), "v1").unwrap();
        stage(cwd, "a.txt").unwrap();
        commit(cwd, "initial").unwrap();
        std::fs::write(cwd.join("a.txt"), "v2").unwrap();

        let d = file_diff(cwd, "a.txt", false).unwrap();
        assert_eq!(d.head, "v1");
        assert_eq!(d.working, "v2");
    }

    #[test]
    fn discard_restores_tracked_file() {
        if skip_if_no_git() {
            return;
        }
        let dir = init_temp_repo();
        let cwd = dir.path();
        std::fs::write(cwd.join("a.txt"), "committed").unwrap();
        stage(cwd, "a.txt").unwrap();
        commit(cwd, "initial").unwrap();

        std::fs::write(cwd.join("a.txt"), "scratched").unwrap();
        discard(cwd, "a.txt").unwrap();
        assert_eq!(
            std::fs::read_to_string(cwd.join("a.txt")).unwrap(),
            "committed"
        );
    }

    #[test]
    fn discard_deletes_untracked_file() {
        if skip_if_no_git() {
            return;
        }
        let dir = init_temp_repo();
        let cwd = dir.path();
        let path = cwd.join("trash.txt");
        std::fs::write(&path, "noise").unwrap();
        discard(cwd, "trash.txt").unwrap();
        assert!(!path.exists(), "discard should delete untracked file");
    }

    #[test]
    fn status_works_when_primary_root_is_subdir_of_repo() {
        // Regression for the "...\crates\switchyard-gui\crates\switchyard-gui\..."
        // duplication bug. Repo root at `dir/`, primary_root at
        // `dir/sub/`. A file inside `sub/` must come back with path
        // "sub/file.txt" (repo-relative), and the status payload must
        // carry `repo_root = dir/` so the frontend can construct the
        // absolute path back out.
        if skip_if_no_git() {
            return;
        }
        let dir = init_temp_repo();
        let repo = dir.path();
        let sub = repo.join("sub");
        std::fs::create_dir(&sub).unwrap();
        std::fs::write(sub.join("file.txt"), "hello").unwrap();

        let s = status(&sub).unwrap();
        let entry = s.files.iter().find(|f| f.path.ends_with("file.txt"));
        assert!(entry.is_some(), "untracked file in subdir must be listed");
        let entry = entry.unwrap();
        // Porcelain output uses POSIX separators even on Windows, so
        // the assertion is invariant across platforms.
        let normalised = entry.path.replace('\\', "/");
        assert_eq!(normalised, "sub/file.txt");
        // `repo_root` lets the frontend resolve back to an absolute
        // path. We canonicalize both sides so Windows extended-length
        // paths and platform-specific trailing slashes don't trip up
        // the comparison.
        let observed = std::path::PathBuf::from(&s.repo_root)
            .canonicalize()
            .unwrap();
        let expected = repo.canonicalize().unwrap();
        assert_eq!(observed, expected);
    }

    #[test]
    fn stage_works_from_subdir() {
        // Calling `stage(primary_root=sub, "sub/file.txt")` from a
        // workspace rooted at the subdir used to fail because git was
        // run with cwd=sub and would interpret the path argument
        // relative to that — looking for `sub/sub/file.txt`. With the
        // git_cwd fix, every operation runs from repo_root so the
        // porcelain path passes through unchanged.
        if skip_if_no_git() {
            return;
        }
        let dir = init_temp_repo();
        let repo = dir.path();
        let sub = repo.join("sub");
        std::fs::create_dir(&sub).unwrap();
        std::fs::write(sub.join("file.txt"), "hello").unwrap();

        stage(&sub, "sub/file.txt").unwrap();
        let s = status(&sub).unwrap();
        let entry = s
            .files
            .iter()
            .find(|f| f.path.replace('\\', "/") == "sub/file.txt")
            .expect("staged file present");
        assert_eq!(entry.index_status, Some(FileStatus::Added));
    }

    #[test]
    fn is_repo_detects_initialised_dir() {
        if skip_if_no_git() {
            return;
        }
        let dir = init_temp_repo();
        assert!(is_repo(dir.path()));

        let plain = tempfile::tempdir().unwrap();
        assert!(!is_repo(plain.path()));
    }
}
