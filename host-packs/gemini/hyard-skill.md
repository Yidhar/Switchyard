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

## Rules
- Do NOT invoke `codex`, `claude`, or `gemini` CLIs directly.
- All delegation goes through `{{SWITCHYARD_CMD}} host delegate`.
- Every HYARD bridge call returns one compact JSON object on stdout; read it verbatim and extract at least `status`, `job_id`, `message`, and `next_actions`.
- `wait_timeout` is not failure; it means the job is still running.
- After `wait_timeout`, reuse the same `job_id` with `status/result/await`.
- If `status` or `result` still reports the job as active, keep reusing the same `job_id` until it finishes or you have enough progress to report.
- Do not re-delegate the same task unless you intentionally want a new job.
