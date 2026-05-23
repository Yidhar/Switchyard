//! Command resolution for subprocess-based providers.
//!
//! On Windows, provider CLIs can be installed through several mechanisms:
//! - `PATH` entries with `PATHEXT` wrappers (`.cmd`, `.exe`, `.bat`)
//! - WinGet links under `%LOCALAPPDATA%\\Microsoft\\WinGet\\Links`
//! - App execution aliases under `%LOCALAPPDATA%\\Microsoft\\WindowsApps`
//! - npm global wrappers under `%APPDATA%\\npm`
//! - cargo binaries under `%USERPROFILE%\\.cargo\\bin`
//! - Node.js under `%ProgramFiles%\\nodejs` / `%ProgramFiles(x86)%\\nodejs`
//!
//! This module resolves command names to concrete executable paths so probe and
//! runtime execution use the same discovery behavior.

use std::collections::HashSet;
use std::ffi::OsString;
use std::path::{Path, PathBuf};

/// Resolve a bare command name to its full executable path.
///
/// If the command already contains a path separator, returns as-is.
/// On Windows, searches PATH plus common install locations with PATHEXT
/// extensions (`.cmd`, `.exe`, `.bat`, etc.). On Unix, returns the input
/// unchanged (the OS handles PATH lookup).
///
/// **Alias fallback**: provider CLIs occasionally drop or pick up a
/// `-cli` suffix in newer releases (OpenAI's `codex-cli` → `codex`,
/// Anthropic's `claude-cli` → `claude`, etc.). When the configured
/// name isn't on PATH we try the alias forms before giving up, so an
/// outdated `switchyard.toml` doesn't break detection on a machine
/// that has the freshly-renamed binary installed. Logged via the
/// `SWITCHYARD_DEBUG_RESOLVE` env var.
pub fn resolve_command(cmd: &str) -> String {
    if contains_path_separator(cmd) {
        return cmd.to_string();
    }

    if cfg!(windows) {
        if let Some(resolved) = find_on_path(cmd) {
            return resolved;
        }
        for alias in alias_candidates(cmd) {
            if let Some(resolved) = find_on_path(&alias) {
                if std::env::var("SWITCHYARD_DEBUG_RESOLVE").as_deref() == Ok("1") {
                    eprintln!(
                        "[switchyard] '{cmd}' not found on PATH; using alias '{alias}' → {resolved}"
                    );
                }
                return resolved;
            }
        }
    }

    cmd.to_string()
}

/// Likely alternative names for a provider CLI, in priority order.
/// Each upstream tool that renamed its binary gets one entry pair
/// (old ↔ new). Generic `-cli` suffix toggling acts as a catch-all
/// for any other tool that follows the same pattern.
fn alias_candidates(cmd: &str) -> Vec<String> {
    let mut out: Vec<String> = Vec::new();
    let lower = cmd.to_ascii_lowercase();

    // Explicit upstream-rename pairs first — narrow matches so we
    // don't fabricate a fake `git-cli` from `git`.
    match lower.as_str() {
        "codex" => out.push("codex-cli".to_string()),
        "codex-cli" => out.push("codex".to_string()),
        "claude" => {
            out.push("claude-cli".to_string());
            out.push("claude-code".to_string());
        }
        "claude-cli" | "claude-code" => out.push("claude".to_string()),
        "gemini" => out.push("gemini-cli".to_string()),
        "gemini-cli" => out.push("gemini".to_string()),
        "agy" => out.push("antigravity".to_string()),
        "antigravity" => out.push("agy".to_string()),
        _ => {
            // Generic fallback for any other tool the user might
            // configure: try toggling the `-cli` suffix.
            if let Some(stripped) = cmd.strip_suffix("-cli") {
                out.push(stripped.to_string());
            } else {
                out.push(format!("{cmd}-cli"));
            }
        }
    }

    // De-dup while preserving order; drop entries matching the input.
    let mut seen = std::collections::HashSet::new();
    out.retain(|s| s != cmd && seen.insert(s.clone()));
    out
}

/// Search PATH for a command, checking PATHEXT extensions on Windows.
///
/// On Windows this also searches a few well-known install locations so nested
/// environments with stripped PATH still discover official CLIs.
pub fn find_on_path(cmd: &str) -> Option<String> {
    let has_extension = Path::new(cmd).extension().is_some();
    let search_dirs = command_search_dirs_from_env(
        std::env::var_os("PATH"),
        std::env::var_os("LOCALAPPDATA"),
        std::env::var_os("APPDATA"),
        std::env::var_os("USERPROFILE").or_else(|| std::env::var_os("HOME")),
        std::env::var_os("ProgramFiles"),
        std::env::var_os("ProgramFiles(x86)"),
    );
    let extensions = command_extensions_from_env(std::env::var_os("PATHEXT"));

    for dir in search_dirs {
        if let Some(candidate) = find_command_in_dir(&dir, cmd, has_extension, &extensions) {
            return Some(finalize_resolved_path(candidate));
        }
    }

    None
}

fn contains_path_separator(cmd: &str) -> bool {
    cmd.contains('/') || cmd.contains('\\')
}

fn command_extensions_from_env(path_ext: Option<OsString>) -> Vec<String> {
    if cfg!(windows) {
        path_ext
            .and_then(|value| value.into_string().ok())
            .unwrap_or_else(|| ".CMD;.EXE;.BAT;.COM".to_string())
            .split(';')
            .filter_map(|s| {
                let trimmed = s.trim();
                (!trimmed.is_empty()).then(|| trimmed.to_ascii_lowercase())
            })
            .collect()
    } else {
        vec![String::new()]
    }
}

fn command_search_dirs_from_env(
    path_var: Option<OsString>,
    local_app_data: Option<OsString>,
    app_data: Option<OsString>,
    home_dir: Option<OsString>,
    program_files: Option<OsString>,
    program_files_x86: Option<OsString>,
) -> Vec<PathBuf> {
    let mut dirs = Vec::new();
    let mut deferred_windows_apps = Vec::new();
    let mut seen = HashSet::new();

    if let Some(path_var) = path_var {
        for dir in std::env::split_paths(&path_var) {
            push_unique_dir(&mut dirs, &mut deferred_windows_apps, &mut seen, dir);
        }
    }

    if cfg!(windows) {
        if let Some(local) = local_app_data.as_ref().map(PathBuf::from) {
            push_unique_dir(
                &mut dirs,
                &mut deferred_windows_apps,
                &mut seen,
                local.join("Microsoft").join("WinGet").join("Links"),
            );
            push_unique_dir(
                &mut dirs,
                &mut deferred_windows_apps,
                &mut seen,
                local.join("Microsoft").join("WindowsApps"),
            );
        }

        if let Some(app_data) = app_data.as_ref().map(PathBuf::from) {
            push_unique_dir(
                &mut dirs,
                &mut deferred_windows_apps,
                &mut seen,
                app_data.join("npm"),
            );
        }

        if let Some(home) = home_dir.as_ref().map(PathBuf::from) {
            push_unique_dir(
                &mut dirs,
                &mut deferred_windows_apps,
                &mut seen,
                home.join(".cargo").join("bin"),
            );
        }

        if let Some(program_files) = program_files.as_ref().map(PathBuf::from) {
            push_unique_dir(
                &mut dirs,
                &mut deferred_windows_apps,
                &mut seen,
                program_files.join("nodejs"),
            );
        }

        if let Some(program_files_x86) = program_files_x86.as_ref().map(PathBuf::from) {
            push_unique_dir(
                &mut dirs,
                &mut deferred_windows_apps,
                &mut seen,
                program_files_x86.join("nodejs"),
            );
        }

        if let Some(local) = local_app_data.as_ref().map(PathBuf::from) {
            push_unique_dir(
                &mut dirs,
                &mut deferred_windows_apps,
                &mut seen,
                local.join("Programs").join("nodejs"),
            );
        }
    }

    dirs.extend(deferred_windows_apps);
    dirs
}

fn push_unique_dir(
    dirs: &mut Vec<PathBuf>,
    deferred_windows_apps: &mut Vec<PathBuf>,
    seen: &mut HashSet<String>,
    dir: PathBuf,
) {
    if dir.as_os_str().is_empty() {
        return;
    }

    let key = if cfg!(windows) {
        dir.to_string_lossy().to_ascii_lowercase()
    } else {
        dir.to_string_lossy().to_string()
    };

    if seen.insert(key) {
        if cfg!(windows) && is_windows_apps_dir(&dir) {
            deferred_windows_apps.push(dir);
        } else {
            dirs.push(dir);
        }
    }
}

fn find_command_in_dir(
    dir: &Path,
    cmd: &str,
    has_extension: bool,
    extensions: &[String],
) -> Option<PathBuf> {
    if cfg!(windows) && !has_extension {
        for ext in extensions {
            let candidate = dir.join(format!("{cmd}{ext}"));
            if candidate.is_file() {
                return Some(candidate);
            }
        }
    }

    let exact = dir.join(cmd);
    if exact.is_file() {
        return Some(exact);
    }

    None
}

fn finalize_resolved_path(candidate: PathBuf) -> String {
    #[cfg(windows)]
    {
        if let Ok(metadata) = std::fs::symlink_metadata(&candidate)
            && metadata.file_type().is_symlink()
            && let Ok(canonical) = std::fs::canonicalize(&candidate)
        {
            return displayable_path(&canonical);
        }
    }

    displayable_path(&candidate)
}

fn displayable_path(path: &Path) -> String {
    let rendered = path.to_string_lossy().to_string();

    #[cfg(windows)]
    {
        if let Some(stripped) = rendered.strip_prefix(r"\\?\UNC\") {
            return format!(r"\\{stripped}");
        }
        if let Some(stripped) = rendered.strip_prefix(r"\\?\") {
            return stripped.to_string();
        }
    }

    rendered
}

fn is_windows_apps_dir(dir: &Path) -> bool {
    let lowered = dir.to_string_lossy().to_ascii_lowercase();
    lowered.ends_with("\\microsoft\\windowsapps") || lowered.ends_with("/microsoft/windowsapps")
}

/// For npm-installed CLIs on Windows: read the extensionless shell script wrapper
/// to find the node.js entry point path.
///
/// Returns the absolute path to the `.js` entry file, or None if not an npm script.
pub fn resolve_npm_entry(resolved_cmd: &str) -> Option<PathBuf> {
    let script_path = std::path::Path::new(resolved_cmd);
    let shell_script = if script_path.extension().is_some() {
        script_path.with_extension("")
    } else {
        script_path.to_path_buf()
    };

    let content = std::fs::read_to_string(&shell_script).ok()?;
    if !content.starts_with("#!/bin/sh") {
        return None;
    }

    let basedir = shell_script.parent()?.to_string_lossy();
    for line in content.lines() {
        if let Some(rel) = extract_node_module_path(line.trim()) {
            let abs = PathBuf::from(format!("{}/{}", basedir, rel));
            if abs.is_file() {
                return Some(abs);
            }
        }
    }

    None
}

/// Extract relative node_modules path from an npm shell script line.
fn extract_node_module_path(line: &str) -> Option<&str> {
    let marker = "\"$basedir/node_modules/";
    if let Some(start) = line.find(marker) {
        let rest = &line[start + "\"$basedir/".len()..];
        if let Some(end) = rest.find('"') {
            return Some(&rest[..end]);
        }
    }
    None
}

/// Check if a resolved command path is a Windows batch wrapper (.cmd / .bat).
///
/// Multiline prompts passed as argv to batch wrappers can trigger parsing
/// errors. Providers should use stdin for prompt transport when this returns true.
pub fn is_windows_batch_wrapper(cmd: &str) -> bool {
    let lower = cmd.to_lowercase();
    lower.ends_with(".cmd") || lower.ends_with(".bat")
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::ffi::OsString;
    use std::time::{SystemTime, UNIX_EPOCH};

    #[test]
    fn resolve_preserves_paths_with_separators() {
        assert_eq!(resolve_command("/usr/bin/codex"), "/usr/bin/codex");
        assert_eq!(resolve_command("C:\\bin\\codex.exe"), "C:\\bin\\codex.exe");
    }

    #[test]
    fn alias_candidates_handle_known_renames() {
        // codex-cli was renamed to codex upstream — both directions
        // should fall back so users with either configured can still
        // find a binary the other way around.
        assert_eq!(alias_candidates("codex-cli"), vec!["codex".to_string()]);
        assert_eq!(alias_candidates("codex"), vec!["codex-cli".to_string()]);

        assert!(alias_candidates("claude").contains(&"claude-cli".to_string()));
        assert!(alias_candidates("claude").contains(&"claude-code".to_string()));
        assert_eq!(alias_candidates("claude-code"), vec!["claude".to_string()]);

        assert_eq!(alias_candidates("gemini-cli"), vec!["gemini".to_string()]);
        assert_eq!(alias_candidates("gemini"), vec!["gemini-cli".to_string()]);

        assert_eq!(alias_candidates("antigravity"), vec!["agy".to_string()]);
        assert_eq!(alias_candidates("agy"), vec!["antigravity".to_string()]);
    }

    #[test]
    fn alias_candidates_generic_cli_suffix() {
        // Unknown tools fall through to the generic `-cli` toggle.
        assert_eq!(
            alias_candidates("custom-tool"),
            vec!["custom-tool-cli".to_string()]
        );
        assert_eq!(
            alias_candidates("custom-tool-cli"),
            vec!["custom-tool".to_string()]
        );
    }

    #[test]
    fn alias_candidates_never_returns_input() {
        // Sanity check — alias_candidates shouldn't suggest the input
        // back to itself; that would loop in the resolver.
        for name in ["codex", "codex-cli", "claude", "gemini", "agy", "weird"] {
            assert!(
                !alias_candidates(name).iter().any(|s| s == name),
                "alias_candidates({name:?}) suggested itself"
            );
        }
    }

    #[test]
    fn extract_npm_path_works() {
        let line = r#"  exec node  "$basedir/node_modules/@google/gemini-cli/dist/index.js" "$@""#;
        assert_eq!(
            extract_node_module_path(line),
            Some("node_modules/@google/gemini-cli/dist/index.js"),
        );
    }

    #[test]
    fn extract_npm_path_no_match() {
        assert_eq!(extract_node_module_path("echo hello"), None);
    }

    #[test]
    fn batch_wrapper_detection() {
        assert!(is_windows_batch_wrapper("C:\\npm\\gemini.cmd"));
        assert!(is_windows_batch_wrapper("codex.CMD"));
        assert!(is_windows_batch_wrapper("run.bat"));
        assert!(!is_windows_batch_wrapper("codex.exe"));
        assert!(!is_windows_batch_wrapper("codex"));
        assert!(!is_windows_batch_wrapper("/usr/bin/gemini"));
    }

    #[test]
    fn search_dirs_include_windows_fallback_locations_without_duplicates() {
        let dirs = command_search_dirs_from_env(
            Some(OsString::from(
                "C:\\Tools;C:\\Users\\me\\AppData\\Roaming\\npm",
            )),
            Some(OsString::from("C:\\Users\\me\\AppData\\Local")),
            Some(OsString::from("C:\\Users\\me\\AppData\\Roaming")),
            Some(OsString::from("C:\\Users\\me")),
            Some(OsString::from("C:\\Program Files")),
            Some(OsString::from("C:\\Program Files (x86)")),
        );

        let rendered: Vec<String> = dirs
            .iter()
            .map(|dir| dir.to_string_lossy().to_string())
            .collect();

        assert!(rendered.iter().any(|d| d.ends_with("C:\\Tools")));
        assert!(
            rendered
                .iter()
                .any(|d| d.ends_with("C:\\Users\\me\\AppData\\Local\\Microsoft\\WinGet\\Links"))
        );
        assert!(
            rendered
                .iter()
                .any(|d| d.ends_with("C:\\Users\\me\\AppData\\Local\\Microsoft\\WindowsApps"))
        );
        assert!(
            rendered
                .iter()
                .any(|d| d.ends_with("C:\\Users\\me\\AppData\\Roaming\\npm"))
        );
        assert!(
            rendered
                .iter()
                .any(|d| d.ends_with("C:\\Users\\me\\.cargo\\bin"))
        );
        assert!(
            rendered
                .iter()
                .any(|d| d.ends_with("C:\\Program Files\\nodejs"))
        );
        assert!(
            rendered
                .iter()
                .any(|d| d.ends_with("C:\\Program Files (x86)\\nodejs"))
        );
        assert!(
            rendered
                .iter()
                .any(|d| d.ends_with("C:\\Users\\me\\AppData\\Local\\Programs\\nodejs"))
        );

        let npm_count = rendered
            .iter()
            .filter(|d| d.ends_with("C:\\Users\\me\\AppData\\Roaming\\npm"))
            .count();
        assert_eq!(npm_count, 1);
    }

    #[test]
    fn find_command_in_dir_checks_exact_name_before_pathext_variants() {
        let dir = unique_test_dir("resolve_exact_name");
        std::fs::create_dir_all(&dir).unwrap();
        let exact = dir.join("claude.exe");
        std::fs::write(&exact, "stub").unwrap();

        let found = find_command_in_dir(
            &dir,
            "claude.exe",
            true,
            &[".exe".to_string(), ".cmd".to_string()],
        )
        .unwrap();

        assert_eq!(found, exact);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn find_command_in_dir_appends_pathext_for_extensionless_names() {
        let dir = unique_test_dir("resolve_pathext_name");
        std::fs::create_dir_all(&dir).unwrap();
        let cmd = dir.join("gemini.cmd");
        std::fs::write(&cmd, "stub").unwrap();

        let found = find_command_in_dir(
            &dir,
            "gemini",
            false,
            &[".cmd".to_string(), ".exe".to_string()],
        )
        .unwrap();

        assert_eq!(found, cmd);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn find_command_in_dir_prefers_windows_wrapper_over_extensionless_shim() {
        let dir = unique_test_dir("resolve_windows_wrapper");
        std::fs::create_dir_all(&dir).unwrap();
        let shim = dir.join("codex");
        let wrapper = dir.join("codex.cmd");
        std::fs::write(&shim, "#!/bin/sh\n").unwrap();
        std::fs::write(&wrapper, "@echo off\r\n").unwrap();

        let found = find_command_in_dir(
            &dir,
            "codex",
            false,
            &[".cmd".to_string(), ".exe".to_string()],
        )
        .unwrap();

        assert_eq!(found, wrapper);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn search_dirs_defer_windows_apps_until_after_other_candidates() {
        let dirs = command_search_dirs_from_env(
            Some(OsString::from(
                "C:\\Users\\me\\AppData\\Local\\Microsoft\\WindowsApps;C:\\Tools",
            )),
            Some(OsString::from("C:\\Users\\me\\AppData\\Local")),
            Some(OsString::from("C:\\Users\\me\\AppData\\Roaming")),
            Some(OsString::from("C:\\Users\\me")),
            Some(OsString::from("C:\\Program Files")),
            Some(OsString::from("C:\\Program Files (x86)")),
        );

        let rendered: Vec<String> = dirs
            .iter()
            .map(|dir| dir.to_string_lossy().to_string())
            .collect();

        let tools_index = rendered
            .iter()
            .position(|value| value.ends_with("C:\\Tools"))
            .unwrap();
        let winget_index = rendered
            .iter()
            .position(|value| {
                value.ends_with("C:\\Users\\me\\AppData\\Local\\Microsoft\\WinGet\\Links")
            })
            .unwrap();
        let windows_apps_index = rendered
            .iter()
            .position(|value| {
                value.ends_with("C:\\Users\\me\\AppData\\Local\\Microsoft\\WindowsApps")
            })
            .unwrap();

        assert!(tools_index < windows_apps_index);
        assert!(winget_index < windows_apps_index);
    }

    #[cfg(windows)]
    #[test]
    fn displayable_path_strips_windows_verbatim_prefix() {
        assert_eq!(
            displayable_path(Path::new(
                r"\\?\C:\Users\me\AppData\Local\Microsoft\WinGet\Links\claude.exe"
            )),
            r"C:\Users\me\AppData\Local\Microsoft\WinGet\Links\claude.exe"
        );
    }

    fn unique_test_dir(prefix: &str) -> PathBuf {
        let nanos = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        std::env::temp_dir().join(format!("switchyard_{prefix}_{nanos}"))
    }
}
