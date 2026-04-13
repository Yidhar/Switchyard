# Switchyard Usage Guide

You are operating within a **Switchyard** multi-provider orchestration environment.

## What This Means

- Multiple AI coding providers (Claude, Codex, Gemini) are available.
- You may delegate specialized tasks to peer providers.
- One provider acts as the **core** (you); others are **peers**.
- All delegation goes through the Switchyard broker — never call other CLIs directly.

## How to Delegate

### In Switchyard CLI/TUI (sentinel mode)

Emit a delegate block in your response:

```
<<<SWITCHYARD_JSON_BEGIN>>>
{
  "type": "delegate",
  "requests": [{
    "id": "unique-task-id",
    "provider": "claude",
    "role": "reviewer",
    "task": "Review the authentication module for security issues",
    "write_access": false,
    "timeout_sec": 900
  }]
}
<<<SWITCHYARD_JSON_END>>>
```

### In host-native mode (/hyard)

Use the slash command:
```
/hyard:delegate claude "Review the authentication module" --wait-sec 10
```

If HYARD returns `status: "wait_timeout"`, that is **not** a failure.
It means the peer job is still running in background. Continue with:

```text
/hyard:status <job-id>
/hyard:result <job-id>
/hyard:await <job-id> <timeout-sec>
```

## Available Peers

Use `/hyard:list` or check the context provided at the start of your turn.

## Rules

1. **Never invoke provider CLIs directly** (`claude`, `codex`, `gemini`).
2. **wait_timeout is recoverable** — it only ends the current wait window.
3. **Reuse `job_id`** — continue with `status/result/await`, do not blindly re-delegate.
4. **Peers cannot re-delegate** — they execute and return.
5. Incorporate delegate results into your final response to the user.
