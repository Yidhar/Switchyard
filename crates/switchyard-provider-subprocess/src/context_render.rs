//! Renders a ContextBundle into a text block for prompt injection.
//!
//! This is what providers actually see. The structured ContextBundle
//! is flattened into labeled sections that models can parse.

use switchyard_provider_api::ContextBundle;
use switchyard_text::prefix_bytes;

/// Render a ContextBundle into a prompt-injectable text block.
///
/// Format:
/// ```text
/// [Session Summary]
/// ...
///
/// [Recent Turns]
/// #1 You: ...
///    Provider: ...
///
/// [Relevant Artifacts]
/// - artifact title (type)
///
/// [Peer State]
/// - peer conclusion
/// ```
///
/// Empty sections are omitted.
pub fn render_context_bundle(context: &ContextBundle) -> String {
    let mut sections: Vec<String> = Vec::new();

    // Session Summary
    if let Some(ref summary) = context.summary
        && !summary.is_empty()
    {
        sections.push(format!("[Session Summary]\n{summary}"));
    }

    // Recent Turns
    if !context.recent_turns.is_empty() {
        let mut lines = vec!["[Recent Turns]".to_string()];
        for (i, turn) in context.recent_turns.iter().enumerate() {
            let user_msg = turn
                .get("user_message")
                .and_then(|v| v.as_str())
                .unwrap_or("(unknown)");
            let provider = turn.get("provider").and_then(|v| v.as_str()).unwrap_or("?");
            let response = turn.get("provider_response").and_then(|v| v.as_str());

            lines.push(format!("#{} You: {}", i + 1, truncate(user_msg, 200)));
            if let Some(resp) = response {
                lines.push(format!("   {provider}: {}", truncate(resp, 300)));
            }
        }
        sections.push(lines.join("\n"));
    }

    // Relevant Artifacts
    if !context.artifacts.is_empty() {
        let mut lines = vec!["[Relevant Artifacts]".to_string()];
        for artifact in &context.artifacts {
            let title = artifact
                .get("title")
                .and_then(|v| v.as_str())
                .unwrap_or("(untitled)");
            let atype = artifact
                .get("artifact_type")
                .and_then(|v| v.as_str())
                .unwrap_or("unknown");
            let summary = artifact.get("summary").and_then(|v| v.as_str());
            let mut line = format!("- {title} ({atype})");
            if let Some(s) = summary {
                line.push_str(&format!(": {}", truncate(s, 100)));
            }
            lines.push(line);
        }
        sections.push(lines.join("\n"));
    }

    // Peer State
    if !context.peer_state.is_empty() {
        let mut lines = vec!["[Peer State]".to_string()];
        for state in &context.peer_state {
            let provider = state
                .get("provider")
                .and_then(|v| v.as_str())
                .unwrap_or("?");
            let conclusion = state
                .get("conclusion")
                .and_then(|v| v.as_str())
                .unwrap_or_else(|| {
                    state
                        .get("summary")
                        .and_then(|v| v.as_str())
                        .unwrap_or("(no summary)")
                });
            lines.push(format!("- {provider}: {}", truncate(conclusion, 200)));
        }
        sections.push(lines.join("\n"));
    }

    sections.join("\n\n")
}

fn truncate(s: &str, max: usize) -> &str {
    prefix_bytes(s, max)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn empty_context_renders_empty() {
        let ctx = ContextBundle {
            summary: None,
            recent_turns: vec![],
            peer_state: vec![],
            artifacts: vec![],
        };
        assert_eq!(render_context_bundle(&ctx), "");
    }

    #[test]
    fn summary_only() {
        let ctx = ContextBundle {
            summary: Some("Working on auth module".to_string()),
            recent_turns: vec![],
            peer_state: vec![],
            artifacts: vec![],
        };
        assert_eq!(
            render_context_bundle(&ctx),
            "[Session Summary]\nWorking on auth module"
        );
    }

    #[test]
    fn full_context_renders_all_sections() {
        let ctx = ContextBundle {
            summary: Some("Auth work".to_string()),
            recent_turns: vec![serde_json::json!({
                "user_message": "fix the bug",
                "provider": "codex",
                "provider_response": "Done, fixed in main.rs"
            })],
            peer_state: vec![serde_json::json!({
                "provider": "claude",
                "conclusion": "Code looks good"
            })],
            artifacts: vec![serde_json::json!({
                "title": "main.rs",
                "artifact_type": "file_change",
                "summary": "Fixed null check"
            })],
        };
        let rendered = render_context_bundle(&ctx);
        assert!(rendered.contains("[Session Summary]"));
        assert!(rendered.contains("Auth work"));
        assert!(rendered.contains("[Recent Turns]"));
        assert!(rendered.contains("#1 You: fix the bug"));
        assert!(rendered.contains("codex: Done, fixed"));
        assert!(rendered.contains("[Relevant Artifacts]"));
        assert!(rendered.contains("main.rs (file_change)"));
        assert!(rendered.contains("[Peer State]"));
        assert!(rendered.contains("claude: Code looks good"));
    }
}
