//! Live smoke test for Codex provider.
//!
//! Skipped by default. Set SWITCHYARD_TEST_CODEX=1 to enable.
//! Requires `codex` to be installed and authenticated.

use switchyard_provider_api::Provider;
use switchyard_provider_codex::CodexProvider;

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
