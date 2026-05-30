use std::collections::HashSet;

use switchyard_provider_api::*;
use uuid::Uuid;

#[test]
fn capability_roundtrip() {
    let cap = ProviderCapability::StructuredOutput;
    let json = serde_json::to_string(&cap).unwrap();
    assert_eq!(json, r#""structured_output""#);
    let back: ProviderCapability = serde_json::from_str(&json).unwrap();
    assert_eq!(back, cap);
}

#[test]
fn provider_identity_roundtrip() {
    let mut caps = HashSet::new();
    caps.insert(ProviderCapability::HeadlessTurn);
    caps.insert(ProviderCapability::StreamingOutput);

    let id = ProviderIdentity {
        provider_id: "codex".to_string(),
        backend_id: "codex-cli".to_string(),
        display_name: "Codex CLI".to_string(),
        capabilities: caps,
    };
    let json = serde_json::to_string(&id).unwrap();
    let back: ProviderIdentity = serde_json::from_str(&json).unwrap();
    assert_eq!(back.provider_id, "codex");
    assert!(
        back.capabilities
            .contains(&ProviderCapability::HeadlessTurn)
    );
}

#[test]
fn probe_result_roundtrip() {
    let probe = ProbeResult {
        version: Some("1.0.0".to_string()),
        available: true,
        capabilities: HashSet::new(),
        issues: vec!["minor warning".to_string()],
        host_surface: HostSurfaceProbe::ready(HostSurfaceKind::Skill),
    };
    let json = serde_json::to_string(&probe).unwrap();
    let back: ProbeResult = serde_json::from_str(&json).unwrap();
    assert!(back.available);
    assert_eq!(back.version.as_deref(), Some("1.0.0"));
}

#[test]
fn provider_event_constructors() {
    let turn_id = Uuid::now_v7();
    let started = ProviderEvent::turn_started(turn_id, "codex");
    assert_eq!(started.event_type, EventType::TurnStarted);
    assert_eq!(started.turn_id, turn_id);
    assert_eq!(started.provider, "codex");

    let msg = ProviderEvent::text_message(turn_id, "codex", "hello world");
    assert_eq!(msg.event_type, EventType::ItemUpdated);
    assert_eq!(msg.payload["text"], "hello world");

    let failed = ProviderEvent::turn_failed(turn_id, "codex", "timeout");
    assert_eq!(failed.event_type, EventType::TurnFailed);
    assert_eq!(failed.payload["error"], "timeout");
}

#[test]
fn provider_event_serialization() {
    let turn_id = Uuid::now_v7();
    let event = ProviderEvent::turn_started(turn_id, "claude");
    let json = serde_json::to_string(&event).unwrap();
    let back: ProviderEvent = serde_json::from_str(&json).unwrap();
    assert_eq!(back.turn_id, turn_id);
    assert_eq!(back.event_type, EventType::TurnStarted);
}

#[test]
fn delegate_request_roundtrip() {
    let task = DelegateTask {
        id: "t1".to_string(),
        provider: "claude".to_string(),
        role: ProviderRole::Reviewer,
        task: "review this code".to_string(),
        write_access: false,
        cwd: None,
        allowed_paths: vec![],
        timeout_sec: 60,
    };
    let req = DelegateRequest::new(vec![task]);
    let json = serde_json::to_string(&req).unwrap();
    assert!(json.contains(r#""type":"delegate""#));
    let back: DelegateRequest = serde_json::from_str(&json).unwrap();
    assert_eq!(back.requests.len(), 1);
    assert_eq!(back.requests[0].role, ProviderRole::Reviewer);
}

#[test]
fn delegate_request_missing_timeout_defaults_to_no_hard_deadline() {
    let json = r#"{
        "type": "delegate",
        "requests": [{
            "id": "t1",
            "provider": "claude",
            "role": "reviewer",
            "task": "review this code"
        }]
    }"#;

    let req: DelegateRequest = serde_json::from_str(json).unwrap();

    assert_eq!(req.requests[0].timeout_sec, 0);
}

#[test]
fn delegate_response_roundtrip() {
    let result = DelegateTaskResult {
        id: "t1".to_string(),
        provider: "claude".to_string(),
        status: DelegateStatus::Success,
        summary: Some("LGTM".to_string()),
        changed_files: vec![],
        artifacts: vec![],
        error: None,
        exit_code: Some(0),
        duration_ms: Some(1500),
    };
    let resp = DelegateResponse::new(vec![result]);
    let json = serde_json::to_string(&resp).unwrap();
    assert!(json.contains(r#""type":"delegate_result""#));
    let back: DelegateResponse = serde_json::from_str(&json).unwrap();
    assert_eq!(back.results[0].status, DelegateStatus::Success);
}

#[test]
fn provider_role_display() {
    assert_eq!(ProviderRole::Core.to_string(), "core");
    assert_eq!(ProviderRole::Worker.to_string(), "worker");
    assert_eq!(ProviderRole::Reviewer.to_string(), "reviewer");
    assert_eq!(ProviderRole::Analyst.to_string(), "analyst");
}

#[test]
fn error_display() {
    let err = ProviderError::NotInstalled("codex".to_string());
    assert_eq!(err.to_string(), "provider not installed: codex");

    let err = ProviderError::Timeout(30);
    assert_eq!(err.to_string(), "provider timed out after 30 seconds");
}

#[test]
fn turn_input_roundtrip() {
    let input = TurnInput {
        user_message: "fix the bug".to_string(),
        system_prompt: Some("you are a coder".to_string()),
        attachments: Vec::new(),
    };
    let json = serde_json::to_string(&input).unwrap();
    let back: TurnInput = serde_json::from_str(&json).unwrap();
    assert_eq!(back.user_message, "fix the bug");
    assert_eq!(back.system_prompt.as_deref(), Some("you are a coder"));
}

#[test]
fn execution_policy_roundtrip() {
    let policy = ExecutionPolicy {
        timeout_secs: 120,
        write_access: true,
        cwd: std::path::PathBuf::from("/tmp/work"),
        allowed_paths: vec![std::path::PathBuf::from("/tmp/work/src")],
    };
    let json = serde_json::to_string(&policy).unwrap();
    let back: ExecutionPolicy = serde_json::from_str(&json).unwrap();
    assert_eq!(back.timeout_secs, 120);
    assert!(back.write_access);
}
