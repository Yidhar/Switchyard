# Switchyard Host Pack — Codex CLI

## Installation

1. Ensure `switchyard` CLI is on PATH. If it is missing, the install script can create a short-command shim for you.
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
prefers `switchyard` from PATH. If it is missing, the script can install a
short-command `switchyard.cmd` shim into a user PATH directory; otherwise it
falls back to a local `target/debug` or `target/release` build when available.
The skill is installed under `.codex/skills/hyard/SKILL.md`, with a legacy
flat-file path still accepted by probe logic for compatibility.

## Commands

Use the installed `switchyard` binary as the HYARD bridge:

```bash
switchyard host list
switchyard host delegate --provider claude --task "Review this code" --wait-sec 1
switchyard host status <job_id>
switchyard host result <job_id>
switchyard host await <job_id>
switchyard host cancel <job_id>
```

Important behavior:

- `switchyard host delegate` may return `wait_timeout`.
- `wait_timeout` is not failure; continue with `status/result/await`.
- Treat HYARD as a **background tool**: complex LLM jobs may take longer than a short wait window.
- Prefer the default short wait (or `--wait-sec 1`) when launching background work.
- While the peer job runs, continue other useful local work instead of idling on the wait.
- Do not call `await` immediately after `delegate` unless your next step is truly blocked on the peer result.
- You may run multiple independent HYARD jobs in parallel when their tasks do not overlap.
- Keep using the same `job_id` until the job settles.
- Do not re-delegate the same task when you already have a `job_id`.

## Debugging

```bash
switchyard host list
switchyard host delegate --provider claude --task "Review this code" --wait-sec 1
```
