---
name: hyard
description: Hyard fallback instructions for Codex.
---

# HYARD fallback skill

You are running inside a Switchyard-managed workspace. Native `/hyard:*`
extensions are currently unverified in this environment, so use the fallback
bridge documented below.

Warmup:

- `{{SWITCHYARD_CMD}} host help`
- `{{SWITCHYARD_CMD}} host list`

## Canonical commands

| Command | Shell command |
|---------|-----------------|
| `/hyard:list` | `{{SWITCHYARD_CMD}} host list` |
| `/hyard:delegate <provider> <task>` | `{{SWITCHYARD_CMD}} host delegate --provider <provider> --task "<task>" --wait-sec <n>` |
| `/hyard:status <job-id>` | `{{SWITCHYARD_CMD}} host status --job-id <job-id>` |
| `/hyard:await <job-id> <timeout-sec>` | `{{SWITCHYARD_CMD}} host await --job-id <job-id> --timeout-sec <n>` |
| `/hyard:result <job-id>` | `{{SWITCHYARD_CMD}} host result --job-id <job-id>` |
| `/hyard:cancel <job-id>` | `{{SWITCHYARD_CMD}} host cancel --job-id <job-id>` |

## Async bridge semantics

- `/hyard:delegate` may return `status: "completed"` or `status: "wait_timeout"`.
- Every HYARD bridge call prints one compact JSON object on stdout. Read it verbatim and extract at least `status`, `job_id`, `message`, and `next_actions`.
- `wait_timeout` means the bridge stopped waiting, **not** that the peer failed.
- When you receive `wait_timeout`, continue with:
  - `/hyard:status <job-id>`
  - `/hyard:result <job-id>`
  - `/hyard:await <job-id> <timeout-sec>`
- If `status` or `result` still says the job is running, keep reusing the same `job_id` until you reach a terminal state or have enough progress to report.
- Do not re-delegate the same task when you already have a `job_id`.

## Capability gating

1. Run `codex features list`.
2. If `plugins` and `codex_hooks` are `true`, you may consider wiring
   `/hyard:*` via Codex plugin hooks (not covered by this fallback).
3. Otherwise, stick to the shell bridge above.

## Goals

- Keep the bridge command as the only delegator.
- Return the JSON response from the bridge so the orchestrator can parse
  turn/event/artifact structures.
- Do not invoke Canonical peer CLIs directly; always go through `{{SWITCHYARD_CMD}} host`.
- Prefer `status/result/await` over starting a duplicate job after `wait_timeout`.
