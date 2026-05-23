//! Spike: verify `codex app-server` JSON-RPC 2.0 over stdio.
//!
//! Drives the daemon through `initialize` → `thread/start` → `turn/start`,
//! drains notifications until `turn/completed`, then closes stdin.
//!
//! Run with:
//!   cargo run -p switchyard-provider-codex --example app_server_spike
//!
//! Costs one cheap Codex API call. Auth comes from the locally-configured
//! `codex` CLI.

use std::process::Stdio;
use std::sync::Arc;
use std::time::Duration;

use serde_json::{Value, json};
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::process::{ChildStdin, Command};
use tokio::sync::Mutex;

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    // On Windows the npm-installed codex is a .cmd wrapper, no native .exe.
    // tokio::process::Command::new doesn't honour PATHEXT by default, so we
    // pick the platform-specific binary name explicitly. The production
    // adapter will use `resolve_command` from switchyard-provider-subprocess.
    let codex_bin = if cfg!(windows) { "codex.cmd" } else { "codex" };
    let mut child = Command::new(codex_bin)
        .args(["app-server"])
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .kill_on_drop(true)
        .spawn()?;

    let pid = child.id().unwrap_or(0);
    println!("[spike] spawned codex app-server pid={pid}");

    let stdin = Arc::new(Mutex::new(child.stdin.take().expect("stdin")));
    let stdout = child.stdout.take().expect("stdout");
    let stderr = child.stderr.take().expect("stderr");

    tokio::spawn(async move {
        let mut reader = BufReader::new(stderr).lines();
        while let Ok(Some(line)) = reader.next_line().await {
            eprintln!("[stderr] {line}");
        }
    });

    // Reader task: each line is a JSON-RPC frame. Route by id (request reply)
    // or by lack-of-id (notification).
    let (frame_tx, mut frame_rx) = tokio::sync::mpsc::channel::<Value>(256);
    tokio::spawn(async move {
        let mut reader = BufReader::new(stdout).lines();
        let mut count = 0usize;
        while let Ok(Some(line)) = reader.next_line().await {
            count += 1;
            match serde_json::from_str::<Value>(&line) {
                Ok(v) => {
                    if frame_tx.send(v).await.is_err() {
                        break;
                    }
                }
                Err(e) => eprintln!("[stdout-parse-err #{count}] {e}: {line}"),
            }
        }
        eprintln!("[reader] stdout closed after {count} frames");
    });

    let mut next_id: i64 = 0;

    // 1. initialize
    next_id += 1;
    let init_id = next_id;
    send_request(
        stdin.clone(),
        init_id,
        "initialize",
        json!({
            "clientInfo": { "name": "switchyard-spike", "version": "0.0.0" }
        }),
    )
    .await?;
    let init_reply = await_response(&mut frame_rx, init_id).await?;
    println!("[spike] initialize ←\n{}", pretty(&init_reply));

    // The protocol may require an `initialized` notification after the reply.
    send_notification(stdin.clone(), "initialized", json!({})).await?;
    println!("[spike] sent `initialized` notification");

    // 2. thread/start
    next_id += 1;
    let thread_start_id = next_id;
    send_request(stdin.clone(), thread_start_id, "thread/start", json!({})).await?;
    let thread_reply = await_response(&mut frame_rx, thread_start_id).await?;
    println!("[spike] thread/start ←\n{}", pretty(&thread_reply));

    // The reply nests the thread metadata under `result.thread`. The id field
    // is `result.thread.id` (also surfaced as `result.thread.sessionId`).
    let thread_id = thread_reply
        .get("result")
        .and_then(|r| r.get("thread"))
        .and_then(|t| t.get("id"))
        .and_then(|v| v.as_str())
        .ok_or("missing thread.id in thread/start result")?
        .to_string();
    println!("[spike] thread_id = {thread_id}");

    // 3. turn/start — single text input.
    next_id += 1;
    let turn_start_id = next_id;
    send_request(
        stdin.clone(),
        turn_start_id,
        "turn/start",
        json!({
            "threadId": thread_id,
            "input": [{ "type": "text", "text": "Reply with the single word 'one'." }]
        }),
    )
    .await?;
    println!("[spike] turn/start sent, draining events until turn boundary…");

    let mut saw_turn_completed = false;
    let mut started_at = std::time::Instant::now();
    loop {
        let timeout = Duration::from_secs(60).saturating_sub(started_at.elapsed());
        let frame = match tokio::time::timeout(timeout, frame_rx.recv()).await {
            Ok(Some(f)) => f,
            Ok(None) => {
                eprintln!("[spike] stdout closed before turn boundary");
                break;
            }
            Err(_) => {
                eprintln!("[spike] timed out after 60s");
                break;
            }
        };

        // Server-initiated approval requests need a response to unblock the
        // agent. We auto-approve for spike purposes.
        if frame.get("id").is_some() && frame.get("method").is_some() {
            let method = frame.get("method").and_then(|m| m.as_str()).unwrap_or("");
            let req_id = frame.get("id").cloned().unwrap_or(Value::Null);
            println!("[spike] ← server request {method} (id={req_id}) — auto-approving");
            send_response(stdin.clone(), req_id, json!({ "decision": "approve" })).await?;
            continue;
        }

        // Notification or response.
        if let Some(method) = frame.get("method").and_then(|m| m.as_str()) {
            println!("[notification {method}] {}", preview(&frame, 200));
            if method.starts_with("turn/") {
                if method == "turn/completed" || method == "turn/failed" {
                    saw_turn_completed = true;
                    started_at = std::time::Instant::now();
                    println!("[spike] turn boundary: {method}");
                    break;
                }
            }
            continue;
        }

        if let Some(id) = frame.get("id") {
            if id == &Value::from(turn_start_id) {
                println!("[spike] turn/start ←\n{}", pretty(&frame));
                // Some servers reply with an empty success here, then send
                // turn/* notifications. Keep draining.
                continue;
            }
            println!(
                "[spike] response for unknown id={id}: {}",
                preview(&frame, 200)
            );
        }
    }

    println!("[spike] saw_turn_completed = {saw_turn_completed}");

    // Close stdin → app-server should exit cleanly.
    drop(stdin);
    let _ = tokio::time::timeout(Duration::from_secs(5), child.wait()).await;
    println!("[spike] done");
    Ok(())
}

async fn send_request(
    stdin: Arc<Mutex<ChildStdin>>,
    id: i64,
    method: &str,
    params: Value,
) -> std::io::Result<()> {
    let frame = json!({ "jsonrpc": "2.0", "id": id, "method": method, "params": params });
    write_line(stdin, frame).await
}

async fn send_response(
    stdin: Arc<Mutex<ChildStdin>>,
    id: Value,
    result: Value,
) -> std::io::Result<()> {
    let frame = json!({ "jsonrpc": "2.0", "id": id, "result": result });
    write_line(stdin, frame).await
}

async fn send_notification(
    stdin: Arc<Mutex<ChildStdin>>,
    method: &str,
    params: Value,
) -> std::io::Result<()> {
    let frame = json!({ "jsonrpc": "2.0", "method": method, "params": params });
    write_line(stdin, frame).await
}

async fn write_line(stdin: Arc<Mutex<ChildStdin>>, frame: Value) -> std::io::Result<()> {
    let mut s = frame.to_string();
    s.push('\n');
    let mut guard = stdin.lock().await;
    guard.write_all(s.as_bytes()).await?;
    guard.flush().await
}

async fn await_response(
    rx: &mut tokio::sync::mpsc::Receiver<Value>,
    id: i64,
) -> Result<Value, Box<dyn std::error::Error>> {
    let timeout = Duration::from_secs(30);
    let deadline = tokio::time::Instant::now() + timeout;
    loop {
        let remaining = deadline.saturating_duration_since(tokio::time::Instant::now());
        let frame = tokio::time::timeout(remaining, rx.recv())
            .await
            .map_err(|_| "timeout waiting for response")?
            .ok_or("stdout closed waiting for response")?;
        if frame.get("id") == Some(&Value::from(id)) {
            return Ok(frame);
        }
        // Notification or unrelated response; print and keep waiting.
        let method = frame.get("method").and_then(|m| m.as_str()).unwrap_or("");
        if !method.is_empty() {
            println!("[startup-notification {method}] {}", preview(&frame, 200));
        } else {
            println!("[startup-frame] {}", preview(&frame, 200));
        }
    }
}

fn pretty(value: &Value) -> String {
    serde_json::to_string_pretty(value).unwrap_or_else(|_| value.to_string())
}

fn preview(value: &Value, n: usize) -> String {
    let s = value.to_string();
    if s.len() > n {
        format!("{}…(+{}B)", &s[..n], s.len() - n)
    } else {
        s
    }
}
