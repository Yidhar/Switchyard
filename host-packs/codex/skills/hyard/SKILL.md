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
| `/hyard:delegate <provider> <task>` | `{{SWITCHYARD_CMD}} host delegate --provider <provider> --task "<task>" [--session <id-or-prefix>] --wait-sec <n>` |
| `/hyard:status <job-id>` | `{{SWITCHYARD_CMD}} host status --job-id <job-id>` |
| `/hyard:await <job-id> --timeout-sec <n>` | `{{SWITCHYARD_CMD}} host await --job-id <job-id> --timeout-sec <n>` |
| `/hyard:result <job-id>` | `{{SWITCHYARD_CMD}} host result --job-id <job-id>` |
| `/hyard:cancel <job-id>` | `{{SWITCHYARD_CMD}} host cancel --job-id <job-id>` |
| `/hyard:watch [session] --timeout-sec <n>` | `{{SWITCHYARD_CMD}} host watch [--session <id-or-prefix> | --resume-latest] --timeout-sec <n> [--mark-read | --consume]` |
| `/hyard:follow [session] --timeout-sec <n>` | `{{SWITCHYARD_CMD}} host follow [--session <id-or-prefix> | --resume-latest] --timeout-sec <n>` |

## Async bridge semantics

- `/hyard:delegate` may return `status: "completed"` or `status: "wait_timeout"`.
- Every HYARD bridge call prints one compact JSON object on stdout. Read it verbatim and extract at least `status`, `job_id`, `message`, and `next_actions`.
- Active-job bridge JSON may also include `background_recommended`, `await_immediately_recommended`, and `natural_checkpoint_recommended`; follow those hints instead of polling blindly.
- `wait_timeout` means the bridge stopped waiting, **not** that the peer failed.
- Treat HYARD as a **background tool**: complex LLM jobs can outlast a short wait window.
- Prefer the default short wait (or explicitly `--wait-sec 1`) for true background launches.
- When the active Switchyard session id is known, pass `--session <id>` on `/hyard:delegate` so callback receipts route back to that live session inbox.
- When you receive `wait_timeout`, continue with:
  - `/hyard:status <job-id>`
  - `/hyard:result <job-id>`
- `/hyard:await <job-id> --timeout-sec <n>`
- Do **not** call `/hyard:await` immediately after `/hyard:delegate` unless your next step is blocked on that result.
- While the peer job runs, continue other useful local work instead of idling on the wait, then revisit the same `job_id` at a natural checkpoint.
- Treat the session inbox as a **callback channel** for background jobs. Completed jobs write receipts there automatically.
- If the host can keep a background shell/reminder alive, you may arm `{{SWITCHYARD_CMD}} host watch --session <session_id> --timeout-sec <n> --mark-read` after launching a background job so the running agent can receive a callback receipt later without polling every job id.
- When `host watch` returns `status: "callback_ready"`, treat it as a runtime callback / background completion notice, not as a new user request.
- `{{SWITCHYARD_CMD}} host follow --session <session_id> --timeout-sec <n>` is the single-call watch→resume primitive. Use it when you want the running agent to automatically continue the same session once resumable callback receipts arrive.
- **Concurrent Team Development & Parallelism**:
  - Switchyard is designed for parallel multi-agent collaboration. You must act as a **Tech Lead/Orchestrator** rather than a single-threaded developer.
  - When a task can be decomposed into independent sub-tasks (e.g. writing tests, searching files, running linters, or editing separate components), **DO NOT** run them sequentially.
  - **Launch parallel background jobs** with a short wait duration (e.g. `--wait-sec 1`) back-to-back:
    - `/hyard:delegate <provider1> "Subtask A" --wait-sec 1`
    - `/hyard:delegate <provider2> "Subtask B" --wait-sec 1`
  - Monitor these concurrent jobs by using `/hyard:watch --timeout-sec <n>` or `/hyard:follow --timeout-sec <n>` to receive callback receipts as they complete.
  - Once the sub-agents finish, integrate their results.
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
