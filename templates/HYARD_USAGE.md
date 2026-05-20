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
/hyard:delegate claude "Review the authentication module" --wait-sec 1
```

If HYARD returns `status: "wait_timeout"`, that is **not** a failure.
It means the peer job is still running in background. Continue with:

```text
/hyard:status <job-id>
/hyard:result <job-id>
/hyard:await <job-id> --timeout-sec <n>
/hyard:watch --session <session-id> --timeout-sec <n>
/hyard:follow --session <session-id> --timeout-sec <n>
```

Treat HYARD as a **background tool**:

- Complex LLM work can easily outlast a short wait window.
- Keep the returned `job_id` and continue other useful local work while the peer runs.
- You may run multiple independent HYARD jobs in parallel when they do not overlap.
- Completed background jobs also write callback receipts into the session inbox.
- When the active session id is known, pass `--session <session-id>` on `/hyard:delegate` so callback receipts route back to the live session inbox.
- If your host/runtime supports background shells or reminders, arm `/hyard:watch --session <session-id> --timeout-sec <n> --mark-read` so the running agent can receive a callback receipt later without polling every job id.
- When `/hyard:watch` returns `status: "callback_ready"`, treat that payload as a runtime callback / background completion notice rather than a new user request.
- `/hyard:follow` is the single-call watch→resume primitive: wait for unread non-quiet callback receipts and automatically continue the same session when they arrive.

### Quick-start background workflow

```text
1. /hyard:delegate claude "Review the authentication module" --wait-sec 1
2. If status=wait_timeout, continue other useful work and keep the same job_id
3. /hyard:status <job-id>
4. /hyard:await <job-id> --timeout-sec 180
5. /hyard:watch --session <session-id> --timeout-sec 180 --mark-read
6. /hyard:follow --session <session-id> --timeout-sec 180
7. /hyard:result <job-id>
```

## Available Peers

Use `/hyard:list` to refresh peer availability, or refer to the peer catalog injected at the start of your turn.

## Rules

1. **Never invoke provider CLIs directly** (`claude`, `codex`, `gemini`).
2. **wait_timeout is non-terminal** — it only ends the current wait window while the same background job keeps running.
3. **Reuse `job_id`** — continue with `status/result/await`, do not blindly re-delegate.
4. **Use HYARD like a background tool** — do other useful work while long-running peer jobs continue.
5. **Peers cannot re-delegate** — they execute and return.
6. Incorporate delegate results into your final response to the user.
