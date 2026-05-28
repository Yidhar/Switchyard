use std::path::{Path, PathBuf};

use switchyard_config::{SandboxMode, SwitchyardConfig};
use switchyard_provider_api::ExecutionPolicy;

/// Build the effective provider execution policy for a user-facing turn.
///
/// Defaults come from `[sandbox]` in `switchyard.toml`. `workspace-write` is
/// the safe default: providers may write inside `cwd` and explicitly allowed
/// additional paths, but Switchyard never emits the legacy permissive
/// `write_access=true + allowed_paths=[]` sentinel unless the caller chooses
/// `danger-full-access`.
pub fn execution_policy_from_config(config: &SwitchyardConfig, cwd: &Path) -> ExecutionPolicy {
    execution_policy_from_config_with_overrides(config, cwd, None, &[])
}

/// Build an execution policy with optional CLI/session overrides.
///
/// `extra_allowed_paths` are resolved relative to `cwd` before being merged
/// with config-level `[sandbox].allowed_paths`.
pub fn execution_policy_from_config_with_overrides(
    config: &SwitchyardConfig,
    cwd: &Path,
    mode_override: Option<SandboxMode>,
    extra_allowed_paths: &[PathBuf],
) -> ExecutionPolicy {
    let mode = mode_override.unwrap_or(config.sandbox.mode);
    let cwd = cwd.to_path_buf();
    match mode {
        SandboxMode::ReadOnly => ExecutionPolicy::read_only(cwd),
        SandboxMode::WorkspaceWrite => ExecutionPolicy::workspace_write(cwd.clone())
            .add_allowed_paths(config.sandbox_allowed_paths(&cwd))
            .add_allowed_paths(
                extra_allowed_paths
                    .iter()
                    .map(|path| resolve_path(&cwd, path))
                    .collect::<Vec<_>>(),
            ),
        SandboxMode::DangerFullAccess => ExecutionPolicy::danger_full_access(cwd),
    }
}

fn resolve_path(base: &Path, path: &Path) -> PathBuf {
    if path.is_absolute() {
        path.to_path_buf()
    } else {
        base.join(path)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use switchyard_config::{SandboxConfig, SandboxMode, SwitchyardConfig};

    #[test]
    fn default_policy_is_workspace_write_for_cwd() {
        let policy = execution_policy_from_config(&SwitchyardConfig::default(), Path::new("/repo"));
        assert!(policy.write_access);
        assert_eq!(policy.cwd, PathBuf::from("/repo"));
        assert_eq!(policy.allowed_paths, vec![PathBuf::from("/repo")]);
    }

    #[test]
    fn read_only_override_ignores_allowed_paths() {
        let mut config = SwitchyardConfig::default();
        config.sandbox.allowed_paths = vec![PathBuf::from("../shared")];

        let policy = execution_policy_from_config_with_overrides(
            &config,
            Path::new("/repo"),
            Some(SandboxMode::ReadOnly),
            &[PathBuf::from("../tmp")],
        );

        assert!(!policy.write_access);
        assert!(policy.allowed_paths.is_empty());
    }

    #[test]
    fn workspace_write_merges_and_dedups_allowed_paths() {
        let config = SwitchyardConfig {
            sandbox: SandboxConfig {
                mode: SandboxMode::WorkspaceWrite,
                allowed_paths: vec![PathBuf::from("../shared"), PathBuf::from(".")],
            },
            ..Default::default()
        };

        let policy = execution_policy_from_config_with_overrides(
            &config,
            Path::new("/repo/project"),
            None,
            &[PathBuf::from("../shared"), PathBuf::from("/tmp/scratch")],
        );

        assert!(policy.write_access);
        assert_eq!(policy.cwd, PathBuf::from("/repo/project"));
        assert!(
            policy
                .allowed_paths
                .contains(&PathBuf::from("/repo/project"))
        );
        assert!(
            policy
                .allowed_paths
                .contains(&PathBuf::from("/repo/project/../shared"))
        );
        assert!(
            policy
                .allowed_paths
                .contains(&PathBuf::from("/tmp/scratch"))
        );
    }

    #[test]
    fn danger_full_access_uses_empty_allowed_paths_sentinel() {
        let policy = execution_policy_from_config_with_overrides(
            &SwitchyardConfig::default(),
            Path::new("/repo"),
            Some(SandboxMode::DangerFullAccess),
            &[PathBuf::from("/tmp/scratch")],
        );

        assert!(policy.write_access);
        assert!(policy.allowed_paths.is_empty());
    }
}
