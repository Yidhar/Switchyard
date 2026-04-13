# Switchyard Host Pack — Codex CLI

## Installation

1. Ensure `switchyard` CLI is on PATH.
2. Add the agent instructions to your Codex config:
   ```
   cp host-packs/codex/AGENTS.md .codex/AGENTS.md
   ```
3. (Optional) Install the skill-based fallback:
   ```
   cp host-packs/codex/skills/hyard/SKILL.md .codex/skills/hyard.md
   ```

### Capability-gated note

This pack is intentionally **probe-first**: it documents the HYARD semantics
and teaches Codex how to use `switchyard host ...`. Unlike Claude or Gemini,
Codex is currently expected to use a fallback path until plugin/hook
capabilities are explicitly detected on the host.

### Scripts

```
scripts/install-hyard-codex.ps1
scripts/uninstall-hyard-codex.ps1
```

The install script copies the instructions and skill files into `.codex`. It
also resolves the Switchyard binary from PATH first, then falls back to a
local `target/debug` or `target/release` build when available. The skill is
installed under `.codex/skills/hyard/SKILL.md`, with a legacy flat-file path
still accepted by probe logic for compatibility.

## Commands

Same as other host packs — see `docs/protocol/HYARD_COMMAND_PROTOCOL_V1.md`
(the file now documents the HYARD v2 async job bridge semantics).

Important behavior:

- `switchyard host delegate` may return `wait_timeout`.
- `wait_timeout` is not failure; continue with `status/result/await`.
- Keep using the same `job_id` until the job settles.

## Debugging

```bash
switchyard host list
switchyard host delegate --provider claude --task "Review this code" --wait-sec 5
```
