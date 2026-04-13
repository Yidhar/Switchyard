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

## Rules

- Do NOT invoke `codex`, `claude`, or `gemini` CLIs directly.
- All delegation must go through `{{SWITCHYARD_CMD}} host delegate`.
- Every HYARD bridge call returns one compact JSON object on stdout; read it verbatim and extract at least `status`, `job_id`, `message`, and `next_actions`.
- `host delegate` may return `status: "completed"` or `status: "wait_timeout"`.
- `wait_timeout` is not a failure — it means the peer job is still running.
- After `wait_timeout`, continue with `status`, `result`, or `await` using the same `job_id`.
- If `status` or `result` still says the job is active, keep using that same `job_id` instead of starting over.
- Do not re-delegate the same task from scratch when you already have a `job_id`.
- Peer providers execute in isolation and return structured results.
- You will receive bridge JSON and should incorporate the peer result into your response once available.
