# HYARD Extension — Gemini CLI

This folder contains the scaffolding for a Gemini CLI extension that maps
native commands to the Switchyard bridge.

## Commands

| Command | Bridge |
|---------|--------|
| `/hyard:list` | `switchyard host list` |
| `/hyard:delegate <provider> <task> [wait-sec]` | `switchyard host delegate --provider <provider> --task "<task>" --wait-sec <n>` |
| `/hyard:status <job-id>` | `switchyard host status --job-id <job-id>` |
| `/hyard:await <job-id> <timeout-sec>` | `switchyard host await --job-id <job-id> --timeout-sec <n>` |
| `/hyard:result <job-id>` | `switchyard host result --job-id <job-id>` |
| `/hyard:cancel <job-id>` | `switchyard host cancel --job-id <job-id>` |
| `/hyard:help` | `switchyard host help` |

Gemini's extension manifest should reference this mapping, and each command must
return the JSON emitted by the bridge.

Async behavior:

- `delegate` may return `status: "wait_timeout"`.
- `wait_timeout` means the job is still running.
- Continue with `status`, `result`, or `await` using the same `job_id`.

## Manual install notes

1. `gemini extensions install <link>` or `gemini extensions link <path>` the
   extension.
2. The extension exposes the `/hyard:*` names listed above.
3. If you cannot install an extension, fallback to `.gemini/skills/hyard.md`.
