//! Live smoke test for Claude provider.
//!
//! Skipped by default. Set SWITCHYARD_TEST_CLAUDE=1 to enable.

use switchyard_provider_api::{EventType, LiveInstance, Provider};
use switchyard_provider_claude::{ClaudeLiveInstance, ClaudeProvider};

fn should_run() -> bool {
    std::env::var("SWITCHYARD_TEST_CLAUDE").is_ok_and(|v| v == "1")
}

#[tokio::test]
async fn claude_probe_detects_installation() {
    if !should_run() {
        eprintln!("skipping: set SWITCHYARD_TEST_CLAUDE=1 to enable");
        return;
    }

    let provider = ClaudeProvider::new("claude", vec![], std::collections::HashMap::new(), 30);
    match provider.probe().await {
        Ok(result) => {
            assert!(result.available);
            println!("claude version: {:?}", result.version);
        }
        Err(e) => {
            println!("probe failed (expected if not installed): {e}");
        }
    }
}

#[tokio::test]
async fn claude_minimal_turn() {
    if !should_run() {
        eprintln!("skipping: set SWITCHYARD_TEST_CLAUDE=1 to enable");
        return;
    }

    let provider = ClaudeProvider::new("claude", vec![], std::collections::HashMap::new(), 120);
    let turn_id = uuid::Uuid::now_v7();
    let (tx, mut rx) = tokio::sync::mpsc::channel(64);

    let input = switchyard_provider_api::TurnInput {
        user_message: "Say exactly: hello switchyard".to_string(),
        system_prompt: None,
        attachments: Vec::new(),
    };
    let policy = switchyard_provider_api::ExecutionPolicy {
        timeout_secs: 120,
        write_access: false,
        cwd: std::env::current_dir().unwrap(),
        allowed_paths: vec![],
    };
    let context = switchyard_provider_api::ContextBundle {
        summary: None,
        recent_turns: vec![],
        peer_state: vec![],
        artifacts: vec![],
    };

    match provider
        .start_turn(
            turn_id,
            input,
            policy,
            context,
            tx,
            switchyard_provider_api::CancellationToken::new(),
        )
        .await
    {
        Ok(()) => {
            while let Ok(e) = rx.try_recv() {
                println!("event: {:?} {}", e.event_type, e.provider);
            }
            let (result, _) = provider.finalize_turn(turn_id).await.unwrap();
            println!("response: {}", result.response_text);
            assert!(!result.response_text.is_empty());
        }
        Err(e) => {
            println!("turn failed (may be expected): {e}");
        }
    }
}

/// Verify a single `claude` process serves two consecutive turns via the
/// `LiveInstance` trait. Asserts: PID stable across turns, each turn emits
/// `TurnCompleted`, and the streamed text contains the expected single-word
/// answer.
#[tokio::test]
async fn claude_persistent_instance_handles_two_turns() {
    if !should_run() {
        eprintln!("skipping: set SWITCHYARD_TEST_CLAUDE=1 to enable");
        return;
    }

    let mut instance = ClaudeLiveInstance::spawn(
        "claude",
        &[],
        std::collections::HashMap::new(),
        std::env::current_dir().ok().as_deref(),
    )
    .await
    .expect("spawn ClaudeLiveInstance");

    let pid_before = instance.child.id().expect("claude should have a PID");

    // Turn 1
    let mut rx = instance
        .send_message("Reply with the single word 'one' and nothing else.")
        .await
        .expect("send turn 1");

    let mut turn1_completed = false;
    let mut turn1_text = String::new();
    while let Some(event) = rx.recv().await {
        if let Some(text) = event.payload.get("text").and_then(|v| v.as_str()) {
            turn1_text.push_str(text);
        }
        if event.event_type == EventType::TurnCompleted {
            turn1_completed = true;
        }
    }
    assert!(turn1_completed, "turn 1 should emit TurnCompleted");
    assert!(
        turn1_text.to_lowercase().contains("one"),
        "turn 1 streamed text should contain 'one', got: {turn1_text:?}",
    );
    assert!(
        instance.is_healthy(),
        "claude should still be alive between turns",
    );
    assert_eq!(
        instance.child.id().expect("PID after turn 1"),
        pid_before,
        "PID must not change between turns (same process)",
    );

    // Turn 2 — same process
    let mut rx = instance
        .send_message("Now reply with the single word 'two' and nothing else.")
        .await
        .expect("send turn 2");

    let mut turn2_completed = false;
    let mut turn2_text = String::new();
    while let Some(event) = rx.recv().await {
        if let Some(text) = event.payload.get("text").and_then(|v| v.as_str()) {
            turn2_text.push_str(text);
        }
        if event.event_type == EventType::TurnCompleted {
            turn2_completed = true;
        }
    }
    assert!(turn2_completed, "turn 2 should emit TurnCompleted");
    assert!(
        turn2_text.to_lowercase().contains("two"),
        "turn 2 streamed text should contain 'two', got: {turn2_text:?}",
    );
    assert_eq!(
        instance.child.id().expect("PID after turn 2"),
        pid_before,
        "PID must remain stable across turns",
    );

    instance.terminate().await.expect("terminate cleanly");
}
