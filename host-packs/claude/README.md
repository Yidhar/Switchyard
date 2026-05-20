# Switchyard Host Pack — Claude Code

## Installation

### Instruction / skill mode (current)

1. Ensure `switchyard` CLI is on PATH. If it is missing, the install script can create a short-command shim:
   ```
   cargo install --path crates/switchyard-cli
   ```

2. Copy `hyard-skill.md` into your Claude Code project:
   ```
   cp host-packs/claude/hyard-skill.md .claude/skills/hyard.md
   ```

3. Restart Claude Code or reload your skill set.

This gives you the documented `/hyard:*` semantics, but the actual host still
invokes the `switchyard host ...` CLI bridge.

### Native pack (beta scaffolding)

When you want Claude's slash surface to mirror the canonical HYARD commands,
run the install script:

```
scripts/install-hyard-claude.ps1
```

This copies the Claude manifest and skill definitions into your `.claude`
workspace and keeps a lightweight manifest you can wire into Claude's native
slash command registry. The install script prefers `switchyard` from PATH. If
it is missing, the script can create a short-command `switchyard.cmd` shim in
a user PATH directory; otherwise it falls back to `target/debug` or
`target/release` so the installed files can still reference a concrete local
build.

Use the uninstall script to clean up:

```
scripts/uninstall-hyard-claude.ps1
```

## Commands and mappings

The HYARD command surface remains:

- `/hyard:list`
- `/hyard:delegate <provider> <task> [wait-sec]`
- `/hyard:status <job-id>`
- `/hyard:await <job-id> --timeout-sec <n>`
- `/hyard:result <job-id>`
- `/hyard:cancel <job-id>`
- `/hyard:help`

Each command is backed by `switchyard host ...` subprocess calls described in
`host-packs/claude/native/manifest.yaml`.

## Native manifest

Inspect `host-packs/claude/native/manifest.yaml` for the full command-to-CLI
mapping and an install checklist. The manifest is what the script copies into
your Claude install to inform the slash command owner about the command name
and the bridge invocation.

## How It Works

Claude Code is instructed to invoke:

```bash
switchyard host delegate --provider <name> --task "..." --wait-sec 10
```

The Switchyard broker executes the peer turn and returns structured JSON.
For long-running tasks it may return `status: "wait_timeout"` first; in that
case Claude should continue with `switchyard host status/result/await` using
the same `job_id`.

Treat HYARD as a **background tool**:

- complex LLM work may outlast a short wait window;
- Claude should continue other useful work while the peer runs; and
- you may run multiple independent HYARD jobs in parallel when their tasks do not overlap.

## Debugging

```bash
# Test the bridge directly
switchyard host list
switchyard host delegate --provider gemini --task "Review this code" --wait-sec 5
```
