//! Spike: verify that `claude --print --input-format stream-json` keeps a
//! single `claude.exe` process alive across multiple turns, with structured
//! events on both ends.
//!
//! Run with:
//!   cargo run -p switchyard-provider-claude --example persistent_spike
//!
//! Costs two cheap API calls (single-word responses) against whatever Claude
//! auth is configured for the local CLI.

use std::process::Stdio;
use std::sync::Arc;

use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::process::{ChildStdin, Command};
use tokio::sync::Notify;
use uuid::Uuid;

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let session_id = Uuid::now_v7();
    println!("[spike] session_id = {session_id}");

    let mut child = Command::new("claude")
        .args([
            "--print",
            "--input-format",
            "stream-json",
            "--output-format",
            "stream-json",
            "--session-id",
            &session_id.to_string(),
            "--include-partial-messages",
            "--verbose",
        ])
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .kill_on_drop(true)
        .spawn()?;

    let pid = child.id().unwrap_or(0);
    println!("[spike] spawned claude pid={pid}");

    let mut stdin = child.stdin.take().expect("stdin");
    let stdout = child.stdout.take().expect("stdout");
    let stderr = child.stderr.take().expect("stderr");

    // Drain stderr so claude doesn't block on a full pipe.
    tokio::spawn(async move {
        let mut reader = BufReader::new(stderr).lines();
        while let Ok(Some(line)) = reader.next_line().await {
            eprintln!("[stderr] {line}");
        }
    });

    // Notifier the stdout reader pings every time a `type=result` event arrives,
    // so the main task knows the current turn has finished.
    let result_notify = Arc::new(Notify::new());
    let result_notify_reader = result_notify.clone();

    let reader_task = tokio::spawn(async move {
        let mut reader = BufReader::new(stdout).lines();
        let mut count = 0usize;
        while let Ok(Some(line)) = reader.next_line().await {
            count += 1;
            let json: serde_json::Value =
                serde_json::from_str(&line).unwrap_or(serde_json::Value::Null);
            let evt_type = json.get("type").and_then(|v| v.as_str()).unwrap_or("?");
            let evt_subtype = json.get("subtype").and_then(|v| v.as_str()).unwrap_or("");
            let preview = if line.len() > 300 {
                format!("{}...(+{} bytes)", &line[..300], line.len() - 300)
            } else {
                line.clone()
            };
            println!("[event #{count:>3} type={evt_type} subtype={evt_subtype}] {preview}");
            if evt_type == "result" {
                result_notify_reader.notify_one();
            }
        }
        println!("[reader] stdout closed after {count} events");
    });

    send_user_turn(
        &mut stdin,
        "Reply with the single word 'one' and nothing else.",
    )
    .await?;
    wait_for_result(&result_notify, "turn 1").await;

    send_user_turn(
        &mut stdin,
        "Now reply with the single word 'two' and nothing else.",
    )
    .await?;
    wait_for_result(&result_notify, "turn 2").await;

    // Close stdin so claude sees EOF and exits cleanly.
    drop(stdin);
    let _ = tokio::time::timeout(tokio::time::Duration::from_secs(10), reader_task).await;

    let status = child.wait().await?;
    println!("[spike] claude exited with {status:?}");
    println!("[spike] session jsonl: ~/.claude/projects/<cwd-hash>/{session_id}.jsonl");

    Ok(())
}

async fn send_user_turn(stdin: &mut ChildStdin, text: &str) -> std::io::Result<()> {
    let msg = serde_json::json!({
        "type": "user",
        "message": {
            "role": "user",
            "content": [{ "type": "text", "text": text }]
        }
    });
    let line = format!("{msg}\n");
    println!("[spike] >>> sending: {text:?}");
    stdin.write_all(line.as_bytes()).await?;
    stdin.flush().await?;
    Ok(())
}

async fn wait_for_result(notify: &Notify, label: &str) {
    tokio::select! {
        _ = notify.notified() => println!("[spike] <<< {label} result received"),
        _ = tokio::time::sleep(tokio::time::Duration::from_secs(90)) => {
            println!("[spike] WARN: {label} did not produce a result event within 90s");
        }
    }
}
