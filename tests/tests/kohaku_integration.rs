//! Deterministic integration test for the KohakuTerrarium provider.
//!
//! Drives the real `KohakuProvider` subprocess path against a fake `kt`
//! binary (no network / no auth), asserting the full chain: argv building →
//! subprocess spawn → headless JSONL parsing → `ProviderEvent` mapping →
//! `TurnResult`. Runs in CI.

use std::collections::HashMap;

use switchyard_provider_api::{
    CancellationToken, ContextBundle, EventType, ExecutionPolicy, Provider, TurnInput,
};
use switchyard_provider_kohaku::KohakuProvider;

fn fake_kt() -> String {
    // Cargo sets this for integration tests of the package that defines the bin.
    env!("CARGO_BIN_EXE_fake_kt").to_string()
}

fn policy() -> ExecutionPolicy {
    ExecutionPolicy {
        timeout_secs: 30,
        write_access: false,
        cwd: std::env::current_dir().unwrap(),
        allowed_paths: vec![],
    }
}

fn context() -> ContextBundle {
    ContextBundle {
        summary: None,
        recent_turns: vec![],
        peer_state: vec![],
        artifacts: vec![],
    }
}

fn input(msg: &str) -> TurnInput {
    TurnInput {
        user_message: msg.to_string(),
        system_prompt: None,
        attachments: vec![],
    }
}

#[tokio::test]
async fn kohaku_headless_turn_maps_jsonl_to_events() {
    let provider = KohakuProvider::new(
        fake_kt(),
        vec!["@fake/creature".to_string()],
        HashMap::new(),
        30,
    );
    let turn_id = uuid::Uuid::now_v7();
    let (tx, mut rx) = tokio::sync::mpsc::channel(256);

    provider
        .start_turn(
            turn_id,
            input("ping"),
            policy(),
            context(),
            tx,
            CancellationToken::new(),
        )
        .await
        .expect("start_turn ok");

    let mut started = false;
    let mut completed = false;
    let mut streamed = String::new();
    while let Some(e) = rx.recv().await {
        match e.event_type {
            EventType::TurnStarted => started = true,
            EventType::TurnCompleted => completed = true,
            _ => {}
        }
        if e.payload.get("item_type").and_then(|v| v.as_str()) == Some("agent_message")
            && let Some(t) = e.payload.get("text").and_then(|v| v.as_str())
        {
            streamed.push_str(t);
        }
    }

    assert!(started, "should emit TurnStarted");
    assert!(completed, "should emit TurnCompleted on exit 0");
    assert!(
        streamed.contains("echo: ping"),
        "streamed assistant text should echo the prompt, got: {streamed:?}"
    );

    let (result, bundle) = provider.finalize_turn(turn_id).await.expect("finalize");
    assert_eq!(result.exit_code, Some(0));
    assert!(
        result.response_text.contains("echo: ping"),
        "response_text: {:?}",
        result.response_text
    );
    assert!(!bundle.artifacts.is_empty(), "should archive raw output");
}

#[tokio::test]
async fn kohaku_keeps_chat_clean_of_protocol_and_sentinel() {
    // The fake emits an activity stream, a fragmented SWITCHYARD sentinel block,
    // and the usual text — the chat path must show only the model's prose: no
    // terminal mirror of the JSONL protocol, and no sentinel leakage.
    let mut env = HashMap::new();
    env.insert("FAKE_KT_SENTINEL".to_string(), "1".to_string());
    let provider = KohakuProvider::new(fake_kt(), vec!["@fake/creature".to_string()], env, 30);
    let turn_id = uuid::Uuid::now_v7();
    let (tx, mut rx) = tokio::sync::mpsc::channel(256);

    provider
        .start_turn(
            turn_id,
            input("ping"),
            policy(),
            context(),
            tx,
            CancellationToken::new(),
        )
        .await
        .expect("start_turn ok");

    let mut display = String::new();
    let mut terminal_outputs = 0;
    let mut execution_telemetry = 0;
    let mut tool_calls: Vec<(String, Option<String>)> = vec![];
    while let Some(e) = rx.recv().await {
        match e.payload.get("item_type").and_then(|v| v.as_str()) {
            Some("agent_message") => {
                if let Some(t) = e.payload.get("text").and_then(|v| v.as_str()) {
                    display.push_str(t);
                }
            }
            Some("terminal_output") => terminal_outputs += 1,
            Some("execution_telemetry") => execution_telemetry += 1,
            Some("tool_call") => {
                if let Some(name) = e.payload.get("name").and_then(|v| v.as_str()) {
                    let id = e
                        .payload
                        .get("id")
                        .and_then(|v| v.as_str())
                        .map(str::to_string);
                    tool_calls.push((name.to_string(), id));
                }
            }
            _ => {}
        }
    }

    assert_eq!(
        terminal_outputs, 0,
        "kt --json protocol lines must not be mirrored as terminal output"
    );
    assert_eq!(
        execution_telemetry, 0,
        "the kt.exe driver must not be surfaced as execution_telemetry (it would \
         headline the live card as '正在运行 kt.exe' and inflate the command count)"
    );
    // The fake runs a `read` tool (start+done) — it must surface as tool_call
    // items named "read" (not a generic "Execute Command"), carrying the stable
    // job_id so the two merge into one card.
    assert!(
        tool_calls.iter().any(|(name, _)| name == "read"),
        "kt tool activity should surface as named tool_call items, got: {tool_calls:?}"
    );
    assert!(
        tool_calls
            .iter()
            .filter(|(name, _)| name == "read")
            .all(|(_, id)| id.as_deref() == Some("read_ab12cd")),
        "tool_call start/done must share the kt job_id so they merge, got: {tool_calls:?}"
    );
    assert!(
        !display.contains("SWITCHYARD_JSON"),
        "sentinel must never leak into the chat display, got: {display:?}"
    );
    // The surrounding prose still streams (block is withheld, not the text).
    assert!(display.contains("echo: ping"), "got: {display:?}");
    assert!(display.contains("Plan:"), "got: {display:?}");
    assert!(display.contains("done"), "got: {display:?}");

    // The router still sees the full body (block included) for delegation.
    let (result, _) = provider.finalize_turn(turn_id).await.expect("finalize");
    assert!(
        result.response_text.contains("SWITCHYARD_JSON"),
        "response_text (routing input) keeps the sentinel: {:?}",
        result.response_text
    );
}

#[tokio::test]
async fn kohaku_headless_turn_failure_propagates() {
    let mut env = HashMap::new();
    env.insert("FAKE_KT_FAIL".to_string(), "1".to_string());
    let provider = KohakuProvider::new(fake_kt(), vec!["@fake/creature".to_string()], env, 30);
    let turn_id = uuid::Uuid::now_v7();
    let (tx, mut rx) = tokio::sync::mpsc::channel(256);

    provider
        .start_turn(
            turn_id,
            input("ping"),
            policy(),
            context(),
            tx,
            CancellationToken::new(),
        )
        .await
        .expect("start_turn returns Ok even when the turn itself fails");

    let mut failed = false;
    while let Some(e) = rx.recv().await {
        if e.event_type == EventType::TurnFailed {
            failed = true;
        }
    }
    assert!(failed, "should emit TurnFailed on non-zero exit");

    let (result, _) = provider.finalize_turn(turn_id).await.expect("finalize");
    assert_eq!(result.exit_code, Some(1), "exit code should propagate");
}

#[tokio::test]
async fn kohaku_missing_creature_fails_clearly() {
    // No creature configured (args empty) — must fail with actionable guidance
    // rather than spawning `kt run --headless` with no agent_path.
    let provider = KohakuProvider::new(fake_kt(), vec![], HashMap::new(), 30);
    let turn_id = uuid::Uuid::now_v7();
    let (tx, mut rx) = tokio::sync::mpsc::channel(256);

    let err = provider
        .start_turn(
            turn_id,
            input("hi"),
            policy(),
            context(),
            tx,
            CancellationToken::new(),
        )
        .await
        .expect_err("should fail when no creature is configured");
    assert!(
        err.to_string().contains("no creature configured"),
        "error should guide the user, got: {err}"
    );

    let mut failed = false;
    while let Some(e) = rx.recv().await {
        if e.event_type == EventType::TurnFailed {
            failed = true;
        }
    }
    assert!(failed, "should emit TurnFailed");
}
