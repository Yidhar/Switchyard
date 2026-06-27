//! Live smoke test for the KohakuTerrarium provider.
//!
//! Skipped by default. Enable with `SWITCHYARD_TEST_KOHAKU=1`.
//!
//! Optional env (the `kt` console script usually is NOT on the global PATH —
//! it lives in KohakuTerrarium's venv, so point at it explicitly):
//!   SWITCHYARD_KOHAKU_CMD       path to `kt` (default: "kt")
//!   SWITCHYARD_KOHAKU_CREATURE  creature ref/path (required for the turn test)
//!   SWITCHYARD_KOHAKU_LLM       --llm selector (e.g. "enzi/gpt-5.5-custom")

use std::collections::HashMap;

use switchyard_provider_api::{
    CancellationToken, ContextBundle, EventType, ExecutionPolicy, Provider, TurnInput,
};
use switchyard_provider_kohaku::KohakuProvider;

fn should_run() -> bool {
    std::env::var("SWITCHYARD_TEST_KOHAKU").is_ok_and(|v| v == "1")
}

fn kt_cmd() -> String {
    std::env::var("SWITCHYARD_KOHAKU_CMD").unwrap_or_else(|_| "kt".to_string())
}

#[tokio::test]
async fn kohaku_probe_detects_installation() {
    if !should_run() {
        eprintln!("skipping: set SWITCHYARD_TEST_KOHAKU=1 to enable");
        return;
    }

    let provider = KohakuProvider::new(kt_cmd(), vec![], HashMap::new(), 30);
    match provider.probe().await {
        Ok(result) => {
            assert!(result.available);
            println!("kt version: {:?}", result.version);
            // The switchyard-headless fork advertises headless support; on a
            // stock `kt` this surfaces as an issue rather than a hard failure.
            if !result.issues.is_empty() {
                println!("probe issues: {:?}", result.issues);
            }
        }
        Err(e) => {
            println!("probe failed (expected if kt not installed/on PATH): {e}");
        }
    }
}

#[tokio::test]
async fn kohaku_minimal_turn() {
    if !should_run() {
        eprintln!("skipping: set SWITCHYARD_TEST_KOHAKU=1 to enable");
        return;
    }
    let Ok(creature) = std::env::var("SWITCHYARD_KOHAKU_CREATURE") else {
        eprintln!("skipping turn: set SWITCHYARD_KOHAKU_CREATURE to a leaf creature ref");
        return;
    };
    let model = std::env::var("SWITCHYARD_KOHAKU_LLM").ok();

    let provider = KohakuProvider::new_with_options(
        kt_cmd(),
        vec![creature],
        HashMap::new(),
        120,
        model,
        None,
    );
    let turn_id = uuid::Uuid::now_v7();
    let (tx, mut rx) = tokio::sync::mpsc::channel(256);

    let input = TurnInput {
        user_message: "Say exactly: hello switchyard".to_string(),
        system_prompt: None,
        attachments: vec![],
    };
    let policy = ExecutionPolicy {
        timeout_secs: 120,
        write_access: false,
        cwd: std::env::current_dir().unwrap(),
        allowed_paths: vec![],
    };
    let context = ContextBundle {
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
            CancellationToken::new(),
        )
        .await
    {
        Ok(()) => {
            let mut completed = false;
            let mut streamed = String::new();
            while let Some(e) = rx.recv().await {
                if e.event_type == EventType::TurnCompleted {
                    completed = true;
                }
                if e.payload.get("item_type").and_then(|v| v.as_str()) == Some("agent_message")
                    && let Some(t) = e.payload.get("text").and_then(|v| v.as_str())
                {
                    streamed.push_str(t);
                }
            }
            let (result, _) = provider.finalize_turn(turn_id).await.unwrap();
            println!(
                "response: {} (completed={completed}, streamed={streamed:?})",
                result.response_text
            );
            assert!(!result.response_text.trim().is_empty());
        }
        Err(e) => {
            println!("turn failed (may be expected without a valid backend): {e}");
        }
    }
}
