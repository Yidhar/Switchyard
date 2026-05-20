# Switchyard - Codex Agent Instructions

You are running inside a Switchyard-orchestrated environment.

## Delegating Tasks

To delegate work to a peer provider, use the Switchyard bridge:

```bash
{{SWITCHYARD_CMD}} host delegate --provider claude --task "Review auth module" --session <current-session-id> --wait-sec 1
{{SWITCHYARD_CMD}} host delegate --provider gemini --task "Analyze query performance" --session <current-session-id> --wait-sec 1
```

## Listing Peers

```bash
{{SWITCHYARD_CMD}} host list
```

## Rules

- Do NOT invoke `codex`, `claude`, or `gemini` CLIs directly.
- All delegation must go through `{{SWITCHYARD_CMD}} host delegate`.
- Every HYARD bridge call returns one compact JSON object on stdout; read it verbatim and extract at least `status`, `job_id`, `message`, and `next_actions`.
- Active-job bridge JSON may also include `background_recommended`, `await_immediately_recommended`, and `natural_checkpoint_recommended`; use them as execution hints.
- `host delegate` may return `status: "wait_timeout"` while the peer job keeps running.
- Treat HYARD as a background tool: complex LLM tasks may take a while, so a short wait window is not the same thing as failure.
- Prefer the default short wait (or `--wait-sec 1`) when launching background work.
- When the current Switchyard session id is known, pass `--session <current-session-id>` so callback receipts route back to the live session inbox.
- After `wait_timeout`, inspect or continue the same job with `host status`, `host result`, `host await`, or `host cancel`.
- Do not call `host await` immediately after `host delegate` unless your next step is actually blocked on the peer result.
- Keep doing other useful work while HYARD jobs run, and use the same `job_id` to check back later at a natural checkpoint.
- Treat the session inbox as a callback channel for background jobs. Completed jobs write callback receipts there automatically.
- If the host/runtime can keep a background shell or reminder alive, arm `{{SWITCHYARD_CMD}} host watch --session <session-id> --timeout-sec <n> --mark-read` after a background launch so the running agent can receive a callback receipt later without polling every job id.
- When `host watch` returns `status: "callback_ready"`, treat it as a runtime callback / background completion notice rather than as a new user request.
- You may run multiple independent HYARD jobs in parallel when their tasks do not overlap.
- If `host status` or `host result` says the job is still active, keep using the same `job_id` instead of starting over.
- Do not re-delegate the same task when you already have a `job_id`.
- Peer results will be returned as JSON. Incorporate them into your response.
