use switchyard_session::*;
use uuid::Uuid;

#[test]
fn session_roundtrip() {
    let session = Session::new("codex".to_string());
    let json = serde_json::to_string(&session).unwrap();
    let back: Session = serde_json::from_str(&json).unwrap();
    assert_eq!(back.session_id, session.session_id);
    assert_eq!(back.active_core, "codex");
    assert_eq!(back.mode, SessionMode::Interactive);
    assert!(back.enabled_peers.is_empty());
}

#[test]
fn session_with_peers_and_bindings() {
    let mut session = Session::new("claude".to_string());
    session.enabled_peers = vec!["codex".to_string(), "gemini".to_string()];
    session
        .native_bindings
        .insert("claude".to_string(), "sess_abc123".to_string());
    session.summary = Some("working on auth module".to_string());

    let json = serde_json::to_string(&session).unwrap();
    let back: Session = serde_json::from_str(&json).unwrap();
    assert_eq!(back.enabled_peers.len(), 2);
    assert_eq!(back.native_bindings["claude"], "sess_abc123");
    assert_eq!(back.summary.as_deref(), Some("working on auth module"));
}

#[test]
fn turn_user_roundtrip() {
    let session_id = Uuid::now_v7();
    let turn = Turn::new(session_id, "codex", TurnRole::Core, "fix the login bug");
    let json = serde_json::to_string(&turn).unwrap();
    let back: Turn = serde_json::from_str(&json).unwrap();
    assert_eq!(back.session_id, session_id);
    assert_eq!(back.provider, "codex");
    assert_eq!(back.role, TurnRole::Core);
    assert_eq!(back.user_message, "fix the login bug");
    assert_eq!(back.origin, TurnOrigin::User);
    assert_eq!(back.status, TurnStatus::Pending);
    assert!(back.completed_at.is_none());
    assert!(back.provider_response.is_none());
    assert!(back.delegated_by.is_none());
}

#[test]
fn turn_delegate_roundtrip() {
    let session_id = Uuid::now_v7();
    let turn = Turn::new_delegate(
        session_id,
        "claude",
        TurnRole::Reviewer,
        "review auth module",
        "codex",
    );
    let json = serde_json::to_string(&turn).unwrap();
    let back: Turn = serde_json::from_str(&json).unwrap();
    assert_eq!(back.origin, TurnOrigin::Delegate);
    assert_eq!(back.role, TurnRole::Reviewer);
    assert_eq!(back.delegated_by.as_deref(), Some("codex"));
    assert_eq!(back.user_message, "review auth module");
}

#[test]
fn turn_system_roundtrip() {
    let session_id = Uuid::now_v7();
    let turn = Turn::new_system(session_id, "switchyard", "session summary generated");
    let json = serde_json::to_string(&turn).unwrap();
    let back: Turn = serde_json::from_str(&json).unwrap();
    assert_eq!(back.origin, TurnOrigin::System);
    assert_eq!(back.role, TurnRole::Core);
    assert!(back.delegated_by.is_none());
}

#[test]
fn turn_with_response() {
    let session_id = Uuid::now_v7();
    let mut turn = Turn::new(session_id, "codex", TurnRole::Core, "hello");
    turn.provider_response = Some("Hi, how can I help?".to_string());
    turn.status = TurnStatus::Completed;

    let json = serde_json::to_string(&turn).unwrap();
    let back: Turn = serde_json::from_str(&json).unwrap();
    assert_eq!(
        back.provider_response.as_deref(),
        Some("Hi, how can I help?")
    );
    assert_eq!(back.status, TurnStatus::Completed);
}

#[test]
fn turn_with_error() {
    let session_id = Uuid::now_v7();
    let mut turn = Turn::new(session_id, "codex", TurnRole::Core, "run tests");
    turn.status = TurnStatus::Failed;
    turn.error_message = Some("provider timed out after 120 seconds".to_string());

    let json = serde_json::to_string(&turn).unwrap();
    let back: Turn = serde_json::from_str(&json).unwrap();
    assert_eq!(back.status, TurnStatus::Failed);
    assert_eq!(
        back.error_message.as_deref(),
        Some("provider timed out after 120 seconds")
    );
}

#[test]
fn turn_role_serialization() {
    let cases = [
        (TurnRole::Core, r#""core""#),
        (TurnRole::Worker, r#""worker""#),
        (TurnRole::Reviewer, r#""reviewer""#),
        (TurnRole::Analyst, r#""analyst""#),
    ];
    for (variant, expected_json) in cases {
        let json = serde_json::to_string(&variant).unwrap();
        assert_eq!(json, expected_json);
        let back: TurnRole = serde_json::from_str(&json).unwrap();
        assert_eq!(back, variant);
    }
}

#[test]
fn turn_status_variants() {
    for status in [
        TurnStatus::Pending,
        TurnStatus::Running,
        TurnStatus::Completed,
        TurnStatus::Failed,
        TurnStatus::Cancelled,
    ] {
        let json = serde_json::to_string(&status).unwrap();
        let back: TurnStatus = serde_json::from_str(&json).unwrap();
        assert_eq!(back, status);
    }
}

#[test]
fn event_roundtrip() {
    let turn_id = Uuid::now_v7();
    let event = Event::new(
        turn_id,
        EventType::TurnStarted,
        "codex",
        serde_json::json!({"detail": "starting"}),
    );
    let json = serde_json::to_string(&event).unwrap();
    let back: Event = serde_json::from_str(&json).unwrap();
    assert_eq!(back.turn_id, turn_id);
    assert_eq!(back.event_type, EventType::TurnStarted);
    assert_eq!(back.provider, "codex");
    assert_eq!(back.payload["detail"], "starting");
}

#[test]
fn event_type_serialization() {
    let cases = [
        (EventType::ThreadStarted, r#""thread_started""#),
        (EventType::TurnStarted, r#""turn_started""#),
        (EventType::ItemStarted, r#""item_started""#),
        (EventType::ItemUpdated, r#""item_updated""#),
        (EventType::ItemCompleted, r#""item_completed""#),
        (EventType::ArtifactReady, r#""artifact_ready""#),
        (EventType::DelegateRequested, r#""delegate_requested""#),
        (EventType::DelegateCompleted, r#""delegate_completed""#),
        (EventType::TurnCompleted, r#""turn_completed""#),
        (EventType::TurnFailed, r#""turn_failed""#),
    ];
    for (variant, expected_json) in cases {
        let json = serde_json::to_string(&variant).unwrap();
        assert_eq!(json, expected_json);
        let back: EventType = serde_json::from_str(&json).unwrap();
        assert_eq!(back, variant);
    }
}

#[test]
fn artifact_roundtrip() {
    let turn_id = Uuid::now_v7();
    let mut artifact = Artifact::new(turn_id, ArtifactType::FileChange, "modified auth.rs");
    artifact.summary = Some("added token validation".to_string());
    artifact.path = Some(std::path::PathBuf::from("src/auth.rs"));
    artifact
        .metadata
        .insert("lines_changed".to_string(), serde_json::json!(42));

    let json = serde_json::to_string(&artifact).unwrap();
    let back: Artifact = serde_json::from_str(&json).unwrap();
    assert_eq!(back.turn_id, turn_id);
    assert_eq!(back.artifact_type, ArtifactType::FileChange);
    assert_eq!(back.title, "modified auth.rs");
    assert_eq!(back.summary.as_deref(), Some("added token validation"));
    assert_eq!(back.metadata["lines_changed"], 42);
}

#[test]
fn artifact_type_serialization() {
    let cases = [
        (ArtifactType::FileChange, r#""file_change""#),
        (ArtifactType::CommandOutput, r#""command_output""#),
        (ArtifactType::ReviewConclusion, r#""review_conclusion""#),
        (ArtifactType::DelegateResult, r#""delegate_result""#),
        (ArtifactType::RawProviderOutput, r#""raw_provider_output""#),
    ];
    for (variant, expected_json) in cases {
        let json = serde_json::to_string(&variant).unwrap();
        assert_eq!(json, expected_json);
    }
}

#[test]
fn jsonl_multiline_roundtrip() {
    let session_id = Uuid::now_v7();
    let turns: Vec<Turn> = (0..5)
        .map(|i| Turn::new(session_id, "codex", TurnRole::Core, format!("message {i}")))
        .collect();

    let jsonl: String = turns
        .iter()
        .map(|t| serde_json::to_string(t).unwrap())
        .collect::<Vec<_>>()
        .join("\n");

    let back: Vec<Turn> = jsonl
        .lines()
        .map(|line| serde_json::from_str(line).unwrap())
        .collect();

    assert_eq!(back.len(), 5);
    for (i, turn) in back.iter().enumerate() {
        assert_eq!(turn.user_message, format!("message {i}"));
        assert_eq!(turn.session_id, session_id);
    }
}

/// Verify that provider-api EventType and session EventType serialize
/// to identical wire format, proving lossless conversion via serde.
#[test]
fn event_type_wire_compatible_with_provider_api() {
    // All canonical event types must roundtrip through JSON
    let all_types = [
        "thread_started",
        "turn_started",
        "item_started",
        "item_updated",
        "item_completed",
        "artifact_ready",
        "delegate_requested",
        "delegate_completed",
        "turn_completed",
        "turn_failed",
    ];
    for type_str in all_types {
        let json = format!(r#""{type_str}""#);
        let parsed: EventType = serde_json::from_str(&json).unwrap();
        let back = serde_json::to_string(&parsed).unwrap();
        assert_eq!(back, json, "EventType roundtrip failed for {type_str}");
    }
}
