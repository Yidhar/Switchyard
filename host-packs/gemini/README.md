# Switchyard Host Pack — Gemini CLI

## Installation

### Instruction / skill mode

1. Ensure `switchyard` CLI is on PATH. If it is missing, the install script can create a short-command shim for you.
2. Copy the skill definition into your Gemini CLI project:
   ```
   cp host-packs/gemini/hyard-skill.md .gemini/skills/hyard.md
   ```

This gives a contextual instruction that teaches Gemini to invoke the bridge
commands via `switchyard host ...`.

### Extension mode

To install the native HYARD extension (commands implemented through the
Gemini extensions API):

```
scripts/install-hyard-gemini.ps1
```

Run the uninstall script to remove the extension and the skill fallback:

```
scripts/uninstall-hyard-gemini.ps1
```

## Commands

The extension exposes the same HYARD semantics:

- `/hyard:list`
- `/hyard:delegate <provider> <task> [wait-sec]`
- `/hyard:status <job-id>`
- `/hyard:await <job-id> --timeout-sec <n>`
- `/hyard:result <job-id>`
- `/hyard:cancel <job-id>`
- `/hyard:help`

All commands internally shell out to the bridge; see
`host-packs/gemini/extension/manifest.yaml` for the exact mapping.
The install script prefers `switchyard` from PATH. If it is missing, the
script can create a short-command `switchyard.cmd` shim in a user PATH
directory; otherwise it falls back to a local `target/debug` or
`target/release` build when available.

## Debugging

Use the bridge directly when debugging the pack:

```bash
switchyard host list
switchyard host delegate --provider claude --task "Review this code" --wait-sec 5
```

Important async behavior:

- `switchyard host delegate` may return `status: "wait_timeout"` while the same job continues running in background.
- Treat HYARD as a **background tool**: complex LLM work may outlast a short wait window.
- Continue other useful work while the peer job runs, then reuse the same `job_id` with `status`, `result`, or `await`.
- Multiple independent HYARD jobs may be kept in flight when their tasks do not overlap.
