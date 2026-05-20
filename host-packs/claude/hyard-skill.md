---
name: hyard
description: Delegate tasks to other AI coding providers via the Switchyard orchestration broker.
---

# /hyard - Switchyard Delegation

You are running inside a Switchyard-orchestrated environment.
To delegate tasks to peer providers, use the `{{SWITCHYARD_CMD}} host` CLI bridge.

## Available Commands

### Delegate a task
```bash
{{SWITCHYARD_CMD}} host delegate --provider <name> --task "<description>" --wait-sec <n>
```

### List available peers
```bash
{{SWITCHYARD_CMD}} host list
```

### Check job status
```bash
{{SWITCHYARD_CMD}} host status --job-id <uuid>
```

### Continue waiting on the same job
```bash
{{SWITCHYARD_CMD}} host await --job-id <uuid> --timeout-sec <n>
```

### Get full result
```bash
{{SWITCHYARD_CMD}} host result --job-id <uuid>
```

### Wait for callback receipts
```bash
{{SWITCHYARD_CMD}} host watch [--session <id-or-prefix> | --resume-latest] --timeout-sec <n> [--mark-read | --consume]
```

### Wait and auto-resume on callbacks
```bash
{{SWITCHYARD_CMD}} host follow [--session <id-or-prefix> | --resume-latest] --timeout-sec <n>
```

## Rules

- Do NOT invoke `codex`, `claude`, or `gemini` CLIs directly.
- All delegation must go through `{{SWITCHYARD_CMD}} host delegate`.
- Every HYARD bridge call returns one compact JSON object on stdout; read it verbatim and extract at least `status`, `job_id`, `message`, and `next_actions`.
- Active-job bridge JSON may also include `background_recommended`, `await_immediately_recommended`, and `natural_checkpoint_recommended`; use them as hints instead of polling blindly.
- `host delegate` may return `status: "completed"` or `status: "wait_timeout"`.
- `wait_timeout` is not a failure — it means the peer job is still running.
- Treat HYARD as a background tool: complex LLM tasks may take longer than a short wait window.
- After `wait_timeout`, continue with `status`, `result`, or `await` using the same `job_id`.
- While the job is running, continue other useful work instead of idling on the wait, then check back at a natural checkpoint.
- Treat the session inbox as a callback channel for background jobs. Completed jobs write callback receipts automatically.
- When the active session id is known, pass `--session <session-id>` on `host delegate` so callback receipts route back to the live session inbox.
- If the host/runtime can keep a background shell or reminder alive, arm `{{SWITCHYARD_CMD}} host watch --session <session_id> --timeout-sec <n> --mark-read` after launching a background job so the running agent can receive a callback receipt later without tight polling.
- When `host watch` returns `status: "callback_ready"`, treat it as a runtime callback / background completion notice rather than a new user instruction.
- `host follow` is the single-call watch→resume primitive for wake-up style background continuation.
- You may run multiple independent HYARD jobs in parallel when their tasks do not overlap.
- If `status` or `result` still says the job is active, keep using that same `job_id` instead of starting over.
- Do not re-delegate the same task from scratch when you already have a `job_id`.
- Peer providers execute in isolation and return structured results.
- You will receive bridge JSON and should incorporate the peer result into your response once available.
