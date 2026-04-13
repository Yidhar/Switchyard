use switchyard_session::{Artifact, Event, Turn};

/// Assembled context ready for a provider turn.
#[derive(Debug)]
pub struct ComposedContext {
    pub summary: Option<String>,
    pub recent_turns: Vec<Turn>,
    pub recent_events: Vec<Event>,
    pub peer_state: Vec<serde_json::Value>,
    pub relevant_artifacts: Vec<Artifact>,
}

/// Context Composer: assembles provider input from canonical session history.
///
/// Responsible for windowing turns, including summaries, and filtering
/// artifacts. Does NOT do persistence or provider calls.
pub struct ContextComposer {
    pub max_recent_turns: usize,
}

impl Default for ContextComposer {
    fn default() -> Self {
        Self {
            max_recent_turns: 10,
        }
    }
}

impl ContextComposer {
    pub fn new(max_recent_turns: usize) -> Self {
        Self { max_recent_turns }
    }

    /// Compose a context from session data.
    ///
    /// `summary` — stable session summary (may be None for short sessions).
    /// `all_turns` — full turn history for the session, oldest first.
    /// `all_events` — events for the recent window (caller pre-filters by turn_id).
    /// `peer_state` — latest peer conclusions as opaque JSON.
    /// `all_artifacts` — artifacts for the recent window.
    pub fn compose(
        &self,
        summary: Option<String>,
        all_turns: &[Turn],
        all_events: &[Event],
        peer_state: Vec<serde_json::Value>,
        all_artifacts: &[Artifact],
    ) -> ComposedContext {
        let recent_turns = self.window_turns(all_turns);
        let recent_turn_ids: std::collections::HashSet<uuid::Uuid> =
            recent_turns.iter().map(|t| t.turn_id).collect();

        let recent_events: Vec<Event> = all_events
            .iter()
            .filter(|e| recent_turn_ids.contains(&e.turn_id))
            .cloned()
            .collect();

        let relevant_artifacts: Vec<Artifact> = all_artifacts
            .iter()
            .filter(|a| recent_turn_ids.contains(&a.turn_id))
            .cloned()
            .collect();

        ComposedContext {
            summary,
            recent_turns,
            recent_events,
            peer_state,
            relevant_artifacts,
        }
    }

    /// Select the most recent N turns.
    fn window_turns(&self, all_turns: &[Turn]) -> Vec<Turn> {
        let start = all_turns.len().saturating_sub(self.max_recent_turns);
        all_turns[start..].to_vec()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use switchyard_session::*;
    use uuid::Uuid;

    fn make_turns(session_id: Uuid, count: usize) -> Vec<Turn> {
        (0..count)
            .map(|i| Turn::new(session_id, "codex", TurnRole::Core, format!("msg {i}")))
            .collect()
    }

    fn make_events(turns: &[Turn]) -> Vec<Event> {
        turns
            .iter()
            .flat_map(|t| {
                vec![
                    Event::new(
                        t.turn_id,
                        EventType::TurnStarted,
                        "codex",
                        serde_json::json!({}),
                    ),
                    Event::new(
                        t.turn_id,
                        EventType::TurnCompleted,
                        "codex",
                        serde_json::json!({}),
                    ),
                ]
            })
            .collect()
    }

    fn make_artifacts(turns: &[Turn]) -> Vec<Artifact> {
        turns
            .iter()
            .map(|t| {
                Artifact::new(
                    t.turn_id,
                    ArtifactType::FileChange,
                    format!("artifact for {}", t.user_message),
                )
            })
            .collect()
    }

    #[test]
    fn compose_with_few_turns() {
        let composer = ContextComposer::new(10);
        let session_id = Uuid::now_v7();
        let turns = make_turns(session_id, 3);
        let events = make_events(&turns);
        let artifacts = make_artifacts(&turns);

        let ctx = composer.compose(None, &turns, &events, vec![], &artifacts);
        assert_eq!(ctx.recent_turns.len(), 3);
        assert_eq!(ctx.recent_events.len(), 6); // 2 events per turn
        assert_eq!(ctx.relevant_artifacts.len(), 3);
        assert!(ctx.summary.is_none());
    }

    #[test]
    fn compose_windows_to_max_recent() {
        let composer = ContextComposer::new(3);
        let session_id = Uuid::now_v7();
        let turns = make_turns(session_id, 10);
        let events = make_events(&turns);
        let artifacts = make_artifacts(&turns);

        let ctx = composer.compose(
            Some("long session summary".to_string()),
            &turns,
            &events,
            vec![],
            &artifacts,
        );
        assert_eq!(ctx.recent_turns.len(), 3);
        // Only events/artifacts for the last 3 turns
        assert_eq!(ctx.recent_events.len(), 6);
        assert_eq!(ctx.relevant_artifacts.len(), 3);
        assert_eq!(ctx.summary.as_deref(), Some("long session summary"));
        // Verify it's the LAST 3 turns
        assert_eq!(ctx.recent_turns[0].user_message, "msg 7");
        assert_eq!(ctx.recent_turns[2].user_message, "msg 9");
    }

    #[test]
    fn compose_empty_session() {
        let composer = ContextComposer::default();
        let ctx = composer.compose(None, &[], &[], vec![], &[]);
        assert!(ctx.recent_turns.is_empty());
        assert!(ctx.recent_events.is_empty());
        assert!(ctx.relevant_artifacts.is_empty());
    }

    #[test]
    fn compose_includes_peer_state() {
        let composer = ContextComposer::default();
        let peer = vec![
            serde_json::json!({"provider": "claude", "conclusion": "code looks good"}),
            serde_json::json!({"provider": "gemini", "conclusion": "no issues found"}),
        ];
        let ctx = composer.compose(None, &[], &[], peer, &[]);
        assert_eq!(ctx.peer_state.len(), 2);
        assert_eq!(ctx.peer_state[0]["provider"], "claude");
    }

    #[test]
    fn compose_filters_events_by_window() {
        let composer = ContextComposer::new(1);
        let session_id = Uuid::now_v7();
        let turns = make_turns(session_id, 5);
        let events = make_events(&turns);

        let ctx = composer.compose(None, &turns, &events, vec![], &[]);
        // Only 1 turn in window, so only its 2 events
        assert_eq!(ctx.recent_turns.len(), 1);
        assert_eq!(ctx.recent_events.len(), 2);
        assert_eq!(ctx.recent_turns[0].user_message, "msg 4");
    }

    #[test]
    fn default_max_recent_turns_is_10() {
        let composer = ContextComposer::default();
        assert_eq!(composer.max_recent_turns, 10);
    }
}
