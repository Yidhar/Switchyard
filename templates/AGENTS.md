# Switchyard Agent Instructions

This file provides guidance for any AI provider operating within Switchyard.

## Environment

You are running in a **Switchyard** multi-agent orchestration environment.
Multiple AI coding providers collaborate through a central broker.

## Provider Roles

| Role | Description |
|------|-------------|
| Core | Primary provider handling user requests. Can delegate. |
| Worker | Executes implementation tasks. |
| Reviewer | Reviews code, designs, and decisions. |
| Analyst | Analyzes data, performance, and architecture. |

## Delegation Protocol

### Sentinel Mode (Switchyard CLI/TUI)

Emit a JSON block between sentinel markers in your text response:
```
<<<SWITCHYARD_JSON_BEGIN>>>
{"type":"delegate","requests":[...]}
<<<SWITCHYARD_JSON_END>>>
```

### HYARD Mode (Host-Native)

Use slash commands:
```
/hyard:delegate <provider> "<task>" [--wait-sec <n>]
/hyard:list
/hyard:status <job-id>
/hyard:await <job-id> --timeout-sec <n>
/hyard:result <job-id>
/hyard:cancel <job-id>
/hyard:watch --session <session-id> --timeout-sec <n>
/hyard:follow --session <session-id> --timeout-sec <n>
```

Recommended background workflow:
```
1. /hyard:delegate <provider> "<task>" --session <session-id> --wait-sec 1
2. If status=wait_timeout, continue other useful work with the same job_id
3. /hyard:status <job-id>
4. /hyard:await <job-id> --timeout-sec 180
5. /hyard:watch --session <session-id> --timeout-sec 180 --mark-read
6. /hyard:follow --session <session-id> --timeout-sec 180
7. /hyard:result <job-id>
```

## Absolute Rules

1. **Never call provider CLIs directly** — no `claude ...`, `codex ...`, `gemini ...`.
2. **Peers cannot delegate** — only the core provider may request delegation.
3. **HYARD is async** — `wait_timeout` means the job is still running.
4. **Treat HYARD like a background tool** — complex LLM work may take a while, so keep working while jobs run.
5. **Session inbox = callback channel** — completed background jobs write callback receipts there automatically; when you know the live session id, pass it to `/hyard:delegate --session <session-id>` so receipts return to that inbox.
6. **Use watch for live callbacks when available** — if the host can keep a background shell or reminder alive, arm `/hyard:watch --session <session-id> --timeout-sec <n> --mark-read` so the running agent can receive a background completion notice later.
7. **Use follow for automatic wake-and-continue** — prefer `/hyard:follow --session <session-id> --timeout-sec <n>` when you want the single-call watch→resume primitive.
8. **Reuse job_id** — after `wait_timeout`, continue with `status/result/await` (or `cancel` if you intentionally want to stop it).
9. **Parallel is allowed when independent** — you may run multiple independent HYARD jobs in parallel when their tasks do not overlap.
10. **Incorporate results** — always integrate peer findings into your final response.
