//! Fake `kt` for the KohakuTerrarium provider integration test.
//!
//! Ignores most args, validates that the adapter requested headless JSONL,
//! echoes the `-p` prompt, and emits a canned KohakuTerrarium headless JSONL
//! stream on stdout. Logs go to stderr (mirroring real `kt`) to prove the
//! adapter keeps stdout pure JSONL.
//!
//! Behaviour switches:
//! - `FAKE_KT_FAIL=1` → emit a failed `turn_end` and exit 1.

use std::io::Write;

fn emit(out: &mut impl Write, v: serde_json::Value) {
    writeln!(out, "{v}").expect("write jsonl line");
}

fn main() {
    let args: Vec<String> = std::env::args().collect();
    eprintln!("[fake_kt] argv = {args:?}");

    // The adapter must always request the headless JSONL surface.
    if !args.iter().any(|a| a == "--headless") || !args.iter().any(|a| a == "--json") {
        eprintln!("[fake_kt] missing --headless/--json");
        std::process::exit(3);
    }

    let prompt = args
        .iter()
        .position(|a| a == "-p")
        .and_then(|i| args.get(i + 1))
        .cloned()
        .unwrap_or_default();

    let stdout = std::io::stdout();
    let mut out = stdout.lock();

    emit(
        &mut out,
        serde_json::json!({"type":"turn_start","agent":"fake","model":"fake-model"}),
    );
    emit(
        &mut out,
        serde_json::json!({"type":"activity","activity_type":"processing_start","detail":"","metadata":{}}),
    );

    if std::env::var("FAKE_KT_FAIL").is_ok() {
        emit(
            &mut out,
            serde_json::json!({"type":"turn_end","status":"error","text":"","error":"boom","usage":null,"duration_s":0.0}),
        );
        out.flush().ok();
        std::process::exit(1);
    }

    let echo = format!("echo: {prompt}");
    emit(&mut out, serde_json::json!({"type":"text","content": echo}));
    emit(
        &mut out,
        serde_json::json!({"type":"activity","activity_type":"tool_start","detail":"read","metadata":{"name":"read","args":{"path":"x"}}}),
    );
    emit(
        &mut out,
        serde_json::json!({"type":"activity","activity_type":"tool_done","detail":"read","metadata":{}}),
    );
    emit(
        &mut out,
        serde_json::json!({"type":"activity","activity_type":"token_usage","detail":"","metadata":{"total_tokens":5}}),
    );
    emit(
        &mut out,
        serde_json::json!({"type":"turn_end","status":"ok","text": echo, "error": null, "usage": {"total_tokens":5}, "duration_s": 0.01}),
    );
    out.flush().ok();
}
