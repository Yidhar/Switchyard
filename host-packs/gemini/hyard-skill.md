---
name: hyard
description: Delegate tasks to peer AI providers via the Switchyard broker.
---

# /hyard - Switchyard Delegation

You are running inside a Switchyard-orchestrated environment.

## Delegate a task
```bash
{{SWITCHYARD_CMD}} host delegate --provider <name> --task "<description>" --wait-sec <n>
```

## List peers
```bash
{{SWITCHYARD_CMD}} host list
```

## Continue / inspect an existing job
```bash
{{SWITCHYARD_CMD}} host status --job-id <uuid>
{{SWITCHYARD_CMD}} host await --job-id <uuid> --timeout-sec <n>
{{SWITCHYARD_CMD}} host result --job-id <uuid>
```

## Wait for callback receipts
```bash
{{SWITCHYARD_CMD}} host watch [--session <id-or-prefix> | --resume-latest] --timeout-sec <n> [--mark-read | --consume]
```

## Wait and auto-resume on callbacks
```bash
{{SWITCHYARD_CMD}} host follow [--session <id-or-prefix> | --resume-latest] --timeout-sec <n>
```

## Rules
- Do NOT invoke `codex`, `claude`, or `gemini` CLIs directly.
- All delegation goes through `{{SWITCHYARD_CMD}} host delegate`.
- Every HYARD bridge call returns one compact JSON object on stdout; read it verbatim and extract at least `status`, `job_id`, `message`, and `next_actions`.
- Active-job bridge JSON may also include `background_recommended`, `await_immediately_recommended`, and `natural_checkpoint_recommended`; prefer those hints over tight polling loops.
- `wait_timeout` is not failure; it means the job is still running.
- Treat HYARD as a background tool: complex LLM tasks may take longer than a short wait window.
- After `wait_timeout`, reuse the same `job_id` with `status/result/await`.
- While the job is running, continue other useful work instead of idling on the wait, then revisit the same `job_id` at a natural checkpoint.
- Treat the session inbox as a callback channel for background jobs. Completed jobs write callback receipts there automatically.
- When the active session id is known, pass `--session <session-id>` on `host delegate` so callback receipts route back to the live session inbox.
- If the host can keep a background watcher/reminder alive, arm `{{SWITCHYARD_CMD}} host watch --session <session_id> --timeout-sec <n> --mark-read` after a background launch so the running agent can receive a callback receipt later without tight polling.
- When `host watch` returns `status: "callback_ready"`, treat it as a runtime callback / background completion notice, not as a fresh user request.
- `host follow` is the single-call watch→resume primitive for wake-up style background continuation.
- You may run multiple independent HYARD jobs in parallel when their tasks do not overlap.
- If `status` or `result` still reports the job as active, keep reusing the same `job_id` until it finishes or you have enough progress to report.
- Do not re-delegate the same task when you already have a `job_id`.
