//! Shared helpers for subprocess-based provider adapters.
//!
//! Eliminates duplication across codex/claude/gemini turn.rs and probe.rs.

use std::collections::{HashMap, HashSet};

use tokio::sync::mpsc;
use uuid::Uuid;

use switchyard_provider_api::*;

use crate::runner::{SubprocessError, SubprocessOutput};

/// Convert a SubprocessError into a ProviderError, emitting a turn_failed event.
pub async fn handle_subprocess_error(
    err: SubprocessError,
    turn_id: Uuid,
    provider_name: &str,
    event_tx: &mpsc::Sender<ProviderEvent>,
) -> ProviderError {
    let (event_msg, provider_err) = match err {
        SubprocessError::NotFound(ref cmd) => (
            format!("command not found: {cmd}"),
            ProviderError::NotInstalled(format!("'{cmd}' not found")),
        ),
        SubprocessError::Timeout(secs) => (
            format!("timed out after {secs}s"),
            ProviderError::Timeout(secs),
        ),
        SubprocessError::Cancelled => (
            "cancelled by user".to_string(),
            ProviderError::ExecutionFailed("cancelled".to_string()),
        ),
        ref e => (e.to_string(), ProviderError::ExecutionFailed(e.to_string())),
    };

    event_tx
        .send(ProviderEvent::turn_failed(
            turn_id,
            provider_name,
            &event_msg,
        ))
        .await
        .ok();

    provider_err
}

/// Emit turn_completed or turn_failed based on subprocess exit code.
pub async fn emit_completion_event(
    output: &SubprocessOutput,
    turn_id: Uuid,
    provider_name: &str,
    event_tx: &mpsc::Sender<ProviderEvent>,
) -> bool {
    let success = output.exit_code == Some(0);
    if success {
        event_tx
            .send(ProviderEvent::turn_completed(turn_id, provider_name))
            .await
            .ok();
    } else {
        let error_msg = output
            .stderr
            .as_deref()
            .filter(|value| !value.trim().is_empty())
            .map(ToString::to_string)
            .or_else(|| {
                output
                    .stdout
                    .lines()
                    .rev()
                    .find(|line| !line.trim().is_empty())
                    .map(|line| switchyard_text::preview_chars(line.trim(), 160, "…"))
            })
            .unwrap_or_else(|| "non-zero exit".to_string());
        event_tx
            .send(ProviderEvent::turn_failed(
                turn_id,
                provider_name,
                &error_msg,
            ))
            .await
            .ok();
    }
    success
}

/// Default capabilities for a subprocess-based CLI provider.
pub fn default_cli_capabilities() -> HashSet<ProviderCapability> {
    let mut caps = HashSet::new();
    caps.insert(ProviderCapability::HeadlessTurn);
    caps.insert(ProviderCapability::StreamingOutput);
    caps.insert(ProviderCapability::StructuredOutput);
    caps
}

/// Check if combined stdout+stderr output suggests an authentication issue.
pub fn check_auth_error(stdout: &str, stderr: &str, provider_name: &str) -> Option<ProviderError> {
    let combined = format!("{stdout}\n{stderr}");
    let auth_keywords = ["auth", "login", "API key", "token"];
    if auth_keywords.iter().any(|kw| combined.contains(kw)) {
        Some(ProviderError::NotAuthenticated(format!(
            "{provider_name} requires authentication"
        )))
    } else {
        None
    }
}

/// Compose the prompt envelope for a provider CLI.
///
/// When system_prompt is present (session summary, delegate results, peer catalog),
/// wraps it with section markers so the model sees structured context.
/// Without system_prompt, passes user_message unchanged.
///
/// Attachments are intentionally not appended as text here. Native providers
/// pass them through structured CLI/API arguments (for example Codex `--image`);
/// non-native fallbacks should make an explicit, provider-specific decision
/// before exposing local paths to the model prompt.
pub fn compose_prompt(input: &switchyard_provider_api::TurnInput) -> String {
    let user_message = input.user_message_text();
    match &input.system_prompt {
        Some(sp) if !sp.is_empty() => {
            format!("[Context]\n{sp}\n\n[Task]\n{user_message}")
        }
        _ => user_message,
    }
}

/// Resolve the effective timeout for one provider turn.
///
/// `policy_timeout_secs == 0` means "use the provider's configured default".
/// A returned value of `0` means "no hard timeout".
pub fn effective_timeout_secs(provider_default_timeout_secs: u64, policy_timeout_secs: u64) -> u64 {
    if policy_timeout_secs == 0 {
        provider_default_timeout_secs
    } else {
        policy_timeout_secs
    }
}

/// Metadata key for raw stdout (pre-extraction). Archived separately so
/// corrupted extraction doesn't lose the original output for debugging.
pub const META_RAW_STDOUT: &str = "raw_stdout";

/// Build a TurnResult + ArtifactBundle from subprocess output.
pub fn build_turn_result(
    response_text: String,
    output: &SubprocessOutput,
    provider_name: &str,
) -> (TurnResult, ArtifactBundle) {
    let mut metadata = HashMap::new();
    // Preserve raw stdout for archiving (distinct from normalized response_text)
    let raw = output.stdout.trim();
    if !raw.is_empty() && raw != response_text {
        metadata.insert(
            META_RAW_STDOUT.to_string(),
            serde_json::Value::String(raw.to_string()),
        );
    }
    let result = TurnResult {
        response_text,
        exit_code: output.exit_code,
        stderr: output.stderr.clone(),
        metadata,
    };
    let bundle = ArtifactBundle {
        artifacts: vec![ArtifactEntry {
            artifact_type: ARTIFACT_TYPE_RAW_OUTPUT.to_string(),
            title: format!("{provider_name} stdout"),
            summary: None,
            path: None,
            metadata: HashMap::new(),
        }],
    };
    (result, bundle)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    #[test]
    fn effective_timeout_uses_provider_default_when_policy_is_zero() {
        assert_eq!(effective_timeout_secs(0, 0), 0);
        assert_eq!(effective_timeout_secs(300, 0), 300);
        assert_eq!(effective_timeout_secs(900, 0), 900);
    }

    #[test]
    fn effective_timeout_uses_policy_when_present() {
        assert_eq!(effective_timeout_secs(300, 120), 120);
        assert_eq!(effective_timeout_secs(300, 900), 900);
    }

    #[test]
    fn compose_prompt_does_not_append_attachment_references() {
        let input = TurnInput::text("图片输入测试").with_attachments(vec![InputAttachment {
            path: PathBuf::from(r"C:\Users\demo\.switchyard\clipboard_attachments\image.png"),
            mime_type: Some("image/png".to_string()),
        }]);

        let prompt = compose_prompt(&input);

        assert_eq!(prompt, "图片输入测试");
        assert!(!prompt.contains("[Switchyard Attachments]"));
        assert!(!prompt.contains("clipboard_attachments"));
        assert!(!prompt.contains("image.png"));
    }
}
