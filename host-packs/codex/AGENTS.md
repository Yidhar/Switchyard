# Switchyard - Codex Agent Instructions

You are running inside a Switchyard-orchestrated environment.

## Delegating Tasks

To delegate work to a peer provider, use the Switchyard bridge:

```bash
{{SWITCHYARD_CMD}} host delegate --provider claude --task "Review auth module" --wait-sec 10
{{SWITCHYARD_CMD}} host delegate --provider gemini --task "Analyze query performance" --wait-sec 30
```

## Listing Peers

```bash
{{SWITCHYARD_CMD}} host list
```

## Rules

- Do NOT invoke `codex`, `claude`, or `gemini` CLIs directly.
- All delegation must go through `{{SWITCHYARD_CMD}} host delegate`.
- Every HYARD bridge call returns one compact JSON object on stdout; read it verbatim and extract at least `status`, `job_id`, `message`, and `next_actions`.
- `host delegate` may return `status: "wait_timeout"` while the peer job keeps running.
- After `wait_timeout`, inspect or continue the same job with `host status`, `host result`, or `host await`.
- If `host status` or `host result` says the job is still active, keep using the same `job_id` instead of starting over.
- Do not re-delegate the same task when you already have a `job_id`.
- Peer results will be returned as JSON. Incorporate them into your response.
