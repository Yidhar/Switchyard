# CLAUDE.md

This file provides guidance to Claude Code (claude.ai/code) when working with code in this repository.

## What Switchyard is

A local desktop workbench that drives the AI coding CLIs already installed and
logged in on the user's machine (`codex`, `claude`, `gemini`, `agy`). Switchyard
provides **no model accounts** and does not host code or count tokens — model
capability, auth, billing, and context-window/token budgeting all belong to each
provider CLI. Switchyard owns local concerns: routing turns, the canonical
session store, local diffs, artifacts, permissions, and peer delegation.

## Toolchain (Windows MSVC baseline)

- Dev baseline is `stable-x86_64-pc-windows-msvc` (VS 2019 Build Tools). GNU is
  **not** a supported target — do not assume `x86_64-pc-windows-gnu` works.
- In the **Bash tool**, cargo may not be on `PATH`; prefix sessions with
  `export PATH="$HOME/.cargo/bin:$PATH"`. PowerShell generally has it already.
- The repo-root `cargo` (bash) / `cargo.cmd` wrappers rewrite a
  `--target-dir target-*` arg or `CARGO_TARGET_DIR=target-*` to live under
  `target/` so parallel builds don't clobber the default target dir. Plain
  `cargo` (the real one on PATH) is fine for normal work.

## Common commands

Rust workspace (run from repo root):

```bash
cargo build --workspace
cargo test --workspace --all-targets        # full suite
cargo test -p switchyard-tests --test integration_e2e   # one integration file
cargo test -p switchyard-core router::                  # filter by name
cargo fmt --all -- --check
cargo clippy --workspace --all-targets -- -D warnings   # CI treats warnings as errors
```

CI (`.github/workflows/rust-windows.yml`) gates merges on, in order: `ruff format
--check .`, `ruff check .`, `python tools/validate_ci_config.py`, `cargo fmt`,
`cargo clippy ... -D warnings`, `cargo test --workspace --all-targets`. Match
these locally before pushing. (Ruff only lints `tools/` — the lone Python there.)

Desktop GUI (Tauri 2 + React/Vite frontend):

```powershell
.\start-gui.ps1            # installs frontend deps, starts Vite (:5173), launches Tauri dev
```

Frontend-only (in `crates/switchyard-gui/frontend/`):

```bash
npm run dev      # Vite dev server
npm run build    # tsc -b && vite build
npm run lint     # eslint
npm run test:turn-merge   # the one frontend unit test (turnMerge)
```

CLI / TUI (binary is `switchyard`):

```bash
cargo run -p switchyard-cli -- run --provider claude -m "review auth"
cargo run -p switchyard-cli -- tui --resume-latest
cargo run -p switchyard-cli -- check --json      # probe provider availability + host surfaces
cargo run -p switchyard-cli -- host list         # HYARD bridge (see below)
```

## Live provider tests

Default `cargo test` uses in-repo **fake providers** and never touches real CLIs.
Tests that hit installed providers are gated behind env vars and self-skip when
unset/unauthenticated: `SWITCHYARD_RUN_LIVE_PROVIDER_TESTS=1` plus
`SWITCHYARD_TEST_{CODEX,CLAUDE,GEMINI}=1`. End-to-end smoke entry points are
`scripts/smoke-hyard-host.ps1` (single) and `scripts/smoke-hyard-matrix.ps1`.

## Architecture

A layered Rust workspace; one layer ≈ one or more `switchyard-*` crates. Data
flows: user turn → Router picks the active `core` provider → Context Composer
assembles the send payload → Provider adapter runs a headless turn → native
output is mapped to internal events → UI renders + Store/Artifacts persist.

| Layer | Crate(s) | Responsibility |
|-------|----------|----------------|
| Interface | `switchyard-cli`, `switchyard-tui` | CLI commands, ratatui TUI, diff/event views |
| Application | `switchyard-core` | Router, turn runner, event dispatch, write to canonical session. Router is **not** its own crate — it's a subsystem here |
| Orchestrator | `switchyard-orchestrator` | Validates `delegate` requests, supervises peer jobs (retry/backoff), normalizes results. Hosts `WorkerSupervisor` (moved out of core to break a dependency cycle) |
| Context | `switchyard-context` | Context Composer: summary + recent-window assembly, peer-state injection. **No tokenizer / token counting** by design |
| Provider | `switchyard-provider-api`, `switchyard-provider-{codex,claude,gemini,antigravity,subprocess}` | Adapter trait (probe / start_turn / finalize), maps native output to events; `subprocess` = shared process plumbing |
| Artifact | `switchyard-artifacts` | File-change records, **locally computed** diffs, command/review summaries |
| Store | `switchyard-session`, `switchyard-store` | `session` = domain model only; `store` = persistence/query interfaces + SQLite |
| Runtime | `switchyard-runtime`, `switchyard-host-jobs` | Durable runtime authority: commits lifecycle change + ordered event to SQLite *before* IPC broadcast (event log is exactly-once; IPC is at-least-once). Backs GUI live updates and async HYARD jobs |
| Config | `switchyard-config` | Parses `switchyard.toml`; leaf dep, never depends back on business crates |
| Registry | `switchyard-app-providers` | Central `build_provider_registry(&config)` wiring all adapters |
| Desktop | `switchyard-gui` | Tauri shell; `src/main.rs` holds Tauri commands + state, plus `git.rs` / `file_watcher.rs` / `pty.rs`. React frontend under `frontend/` |

### Two delegation protocols (don't conflate them)

- **Sentinel** — in-process, used by Switchyard's own CLI/TUI. A model emits a
  `<<<SWITCHYARD_JSON_BEGIN>>>…<<<SWITCHYARD_JSON_END>>>` block; the Router parses
  it. See `switchyard-provider-api/src/sentinel.rs`.
- **HYARD** — provider-native host surface (slash commands / skills) for peers.
  Host packs in `host-packs/{claude,codex,gemini}/` translate `/hyard:*` calls to
  `switchyard host <subcommand>` subprocesses, the authoritative async-job
  transport. Each invocation prints exactly one JSON doc to stdout; exit code only
  decides success-doc vs error-doc. Spec: `docs/protocol/HYARD_COMMAND_PROTOCOL_V1.md`
  (filename says V1, content is V2). Delegation is **leaf-only**: peers cannot
  re-delegate, one delegate at a time per session.

## Invariants (enforced; see `docs/development/ENGINEERING_RULES.md`)

- **Canonical session is the single source of truth.** Provider-native session
  state is never authoritative; only Switchyard writes the canonical session.
- **Diffs shown in UI are always locally computed** by the Artifact layer.
  Provider-reported file changes are hints only.
- **Provider differences stay inside adapters** — no provider special-casing in
  higher layers. Keep `store`/`session`/`context` strictly separated (store holds
  no runtime state machine; context reads no config paths / does no workspace IO).
- **Delegation must be replayable** — every delegate is visible in the log/session.
- **Headless by default** — providers run non-interactively; PTY is a gated
  fallback only (`portable-pty`), not the default path.
- Docs/ADRs lead implementation: update `docs/` before landing new modules,
  protocols, or behavior changes.

## Local data

Sessions, artifacts, and the SQLite store live under the workspace's
`.switchyard/` (paths configurable in `switchyard.toml`). It may contain chat
history and attachments — keep it out of public commits (it's gitignored).
Templates seeded into user workspaces live in `templates/` and `host-packs/`.
