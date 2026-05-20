# HYARD native helper — Claude Code

This folder describes the **native command manifest** that the Claude CLI
installer should consume to register `/hyard:*` commands. The manifest is a
lightweight reference for teams that can wire native slash commands; the
authoritative bridge remains `switchyard host ...` in `crates/switchyard-cli`.

## Slots

| Command | Bridge |
|---------|--------|
| `/hyard:list` | `switchyard host list` |
| `/hyard:delegate <provider> <task> [wait-sec]` | `switchyard host delegate --provider <provider> --task "<task>" --wait-sec <n>` |
| `/hyard:status <job-id>` | `switchyard host status --job-id <job-id>` |
| `/hyard:await <job-id> --timeout-sec <n>` | `switchyard host await --job-id <job-id> --timeout-sec <n>` |
| `/hyard:result <job-id>` | `switchyard host result --job-id <job-id>` |
| `/hyard:cancel <job-id>` | `switchyard host cancel --job-id <job-id>` |
| `/hyard:help` | `switchyard host help` |

Any native wiring should execute the bridge command listed above, capture
stdout/stderr, and return the JSON payload back to the Claude session. The
bridge itself ensures canonical session events, artifacts, and logging.

Important async behavior:

- `/hyard:delegate` may return `status: "completed"` or `status: "wait_timeout"`.
- `wait_timeout` is **not** a failure; it means the peer job is still running.
- Treat HYARD as a **background tool**: complex LLM jobs may take longer than a short wait window.
- While the peer job runs, continue other useful local work instead of idling on the wait.
- You may run multiple independent HYARD jobs in parallel when their tasks do not overlap.
- After `wait_timeout`, use `/hyard:status`, `/hyard:result`, or `/hyard:await`
  with the same `job_id`.
- Do not re-delegate the same task when you already have a `job_id`.

## Installation checklist

1. Run `scripts/install-hyard-claude.ps1` from the repo root.
2. Confirm `.claude/skills/hyard.md` and `.claude/hyard-native-manifest.yaml`
   exist.
3. If needed, copy `host-packs/claude/native/manifest.yaml` into Claude's
   slash-command registry and wire each binding to the corresponding `/hyard:*`
   name.

The uninstall script removes both the skill and the manifest copy.
