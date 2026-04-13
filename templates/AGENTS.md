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
/hyard:await <job-id> <timeout-sec>
/hyard:result <job-id>
```

## Absolute Rules

1. **Never call provider CLIs directly** — no `claude ...`, `codex ...`, `gemini ...`.
2. **Peers cannot delegate** — only the core provider may request delegation.
3. **HYARD is async** — `wait_timeout` means the job is still running.
4. **Reuse job_id** — after `wait_timeout`, continue with `status/result/await`.
5. **Incorporate results** — always integrate peer findings into your final response.
