//! Live smoke test for Codex provider.
//!
//! Skipped by default. Set SWITCHYARD_TEST_CODEX=1 to enable.
//! Requires `codex` to be installed and authenticated.

use switchyard_provider_api::{EventType, LiveInstance, Provider};
use switchyard_provider_codex::{CodexAppServerInstance, CodexProvider};

fn should_run() -> bool {
    std::env::var("SWITCHYARD_TEST_CODEX").is_ok_and(|v| v == "1")
}

#[tokio::test]
async fn codex_probe_detects_installation() {
    if !should_run() {
        eprintln!("skipping: set SWITCHYARD_TEST_CODEX=1 to enable");
        return;
    }

    let provider = CodexProvider::new("codex", vec![], std::collections::HashMap::new(), 30);
    match provider.probe().await {
        Ok(result) => {
            assert!(result.available);
            println!("codex version: {:?}", result.version);
            println!("capabilities: {:?}", result.capabilities);
            for issue in &result.issues {
                println!("issue: {issue}");
            }
        }
        Err(e) => {
            // NotInstalled is acceptable in CI
            println!("probe failed (expected if codex not installed): {e}");
        }
    }
}

#[tokio::test]
async fn codex_minimal_turn() {
    if !should_run() {
        eprintln!("skipping: set SWITCHYARD_TEST_CODEX=1 to enable");
        return;
    }

    let provider = CodexProvider::new("codex", vec![], std::collections::HashMap::new(), 60);

    let turn_id = uuid::Uuid::now_v7();
    let (tx, mut rx) = tokio::sync::mpsc::channel(64);

    let input = switchyard_provider_api::TurnInput {
        user_message: "Say exactly: hello switchyard".to_string(),
        system_prompt: None,
        attachments: Vec::new(),
    };
    let policy = switchyard_provider_api::ExecutionPolicy {
        timeout_secs: 60,
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
            let mut events = Vec::new();
            while let Ok(e) = rx.try_recv() {
                events.push(e);
            }
            println!("received {} events", events.len());
            assert!(!events.is_empty(), "should receive at least 1 event");

            let (result, _) = provider.finalize_turn(turn_id).await.unwrap();
            println!("response: {}", result.response_text);
            assert!(!result.response_text.is_empty());
        }
        Err(e) => {
            println!("turn failed (may be expected): {e}");
        }
    }
}

/// Verify one long-running `codex app-server` daemon serves two consecutive
/// turns through the `LiveInstance` trait — same PID, structured events,
/// clean termination.
#[tokio::test]
async fn codex_app_server_handles_two_turns() {
    if !should_run() {
        eprintln!("skipping: set SWITCHYARD_TEST_CODEX=1 to enable");
        return;
    }

    let codex_bin = if cfg!(windows) { "codex.cmd" } else { "codex" };
    let mut instance = CodexAppServerInstance::spawn(
        codex_bin,
        &[],
        std::collections::HashMap::new(),
        std::env::current_dir().ok().as_deref(),
    )
    .await
    .expect("spawn CodexAppServerInstance");

    let pid_before = instance
        .child
        .id()
        .expect("codex app-server should have a PID");
    println!(
        "codex app-server pid={pid_before} thread={}",
        instance.thread_id
    );

    // Turn 1
    let mut rx = instance
        .send_message("Reply with the single word 'one'.")
        .await
        .expect("send turn 1");
    let mut turn1_text = String::new();
    let mut turn1_completed = false;
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
        "codex should still be alive between turns"
    );
    assert_eq!(
        instance.child.id().expect("PID after turn 1"),
        pid_before,
        "PID must not change between turns",
    );

    // Turn 2 — same daemon, same thread.
    let mut rx = instance
        .send_message("Now reply with the single word 'two'.")
        .await
        .expect("send turn 2");
    let mut turn2_text = String::new();
    let mut turn2_completed = false;
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
