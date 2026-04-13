//! Archive raw provider stdout/stderr as files on disk.

use std::fs;
use std::path::{Path, PathBuf};

use thiserror::Error;

#[derive(Debug, Error)]
pub enum ArchiveError {
    #[error("I/O error: {0}")]
    Io(#[from] std::io::Error),
}

/// Archive raw stdout/stderr from a provider turn to disk.
///
/// Returns the paths of the created files.
pub fn archive_raw_output(
    artifact_dir: &Path,
    turn_id: &str,
    stdout: Option<&str>,
    stderr: Option<&str>,
) -> Result<Vec<PathBuf>, ArchiveError> {
    let turn_dir = artifact_dir.join(turn_id);
    fs::create_dir_all(&turn_dir)?;

    let mut paths = Vec::new();

    if let Some(out) = stdout
        && !out.is_empty()
    {
        let path = turn_dir.join("stdout.txt");
        fs::write(&path, out)?;
        paths.push(path);
    }

    if let Some(err) = stderr
        && !err.is_empty()
    {
        let path = turn_dir.join("stderr.txt");
        fs::write(&path, err)?;
        paths.push(path);
    }

    Ok(paths)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn archives_stdout_and_stderr() {
        let dir = tempfile::tempdir().unwrap();
        let paths = archive_raw_output(
            dir.path(),
            "test-turn-123",
            Some("hello world"),
            Some("warning: something"),
        )
        .unwrap();
        assert_eq!(paths.len(), 2);
        assert_eq!(fs::read_to_string(&paths[0]).unwrap(), "hello world");
        assert_eq!(fs::read_to_string(&paths[1]).unwrap(), "warning: something");
    }

    #[test]
    fn skips_empty_output() {
        let dir = tempfile::tempdir().unwrap();
        let paths = archive_raw_output(dir.path(), "turn-1", Some("data"), None).unwrap();
        assert_eq!(paths.len(), 1);

        let paths = archive_raw_output(dir.path(), "turn-2", None, None).unwrap();
        assert!(paths.is_empty());
    }

    #[test]
    fn skips_blank_strings() {
        let dir = tempfile::tempdir().unwrap();
        let paths = archive_raw_output(dir.path(), "turn-3", Some(""), Some("")).unwrap();
        assert!(paths.is_empty());
    }
}
