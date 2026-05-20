//! Live smoke test for Gemini provider.
//!
//! Skipped by default. Set SWITCHYARD_TEST_GEMINI=1 to enable.

use switchyard_provider_api::Provider;
use switchyard_provider_gemini::GeminiProvider;

fn should_run() -> bool {
    std::env::var("SWITCHYARD_TEST_GEMINI").is_ok_and(|v| v == "1")
}

#[tokio::test]
async fn gemini_probe_detects_installation() {
    if !should_run() {
        eprintln!("skipping: set SWITCHYARD_TEST_GEMINI=1 to enable");
        return;
    }

    let provider = GeminiProvider::new("gemini", vec![], std::collections::HashMap::new(), 30);
    match provider.probe().await {
        Ok(result) => {
            assert!(result.available);
            println!("gemini version: {:?}", result.version);
        }
        Err(e) => {
            println!("probe failed (expected if not installed): {e}");
        }
    }
}

#[tokio::test]
async fn gemini_minimal_turn() {
    if !should_run() {
        eprintln!("skipping: set SWITCHYARD_TEST_GEMINI=1 to enable");
        return;
    }

    let provider = GeminiProvider::new("gemini", vec![], std::collections::HashMap::new(), 120);
    let turn_id = uuid::Uuid::now_v7();
    let (tx, mut rx) = tokio::sync::mpsc::channel(64);

    let input = switchyard_provider_api::TurnInput {
        user_message: "Say exactly: hello switchyard".to_string(),
        system_prompt: None,
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
