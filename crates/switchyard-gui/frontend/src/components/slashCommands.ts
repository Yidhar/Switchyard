/// Slash command framework for the chat input. When the user's input
/// starts with `/`, parsing routes the text through a registered
/// handler instead of dispatching to the orchestrator. Handlers can
/// mutate UI state (open settings / drawer), invoke Tauri commands,
/// or short-circuit with a one-shot system message.

import { invoke } from '@tauri-apps/api/core';

/// Outcome of running a slash command. `replaceWithMessage` lets a
/// handler short-circuit the input with a synthetic system message
/// instead of opening UI; useful for `/help` and `/workers`.
export interface SlashResult {
  /// Drop the input (success path) — most commands clear after run.
  ok: boolean;
  /// Optional message to append to the chat as a synthetic system
  /// note (e.g. `/help` output).
  systemMessage?: string;
  /// Optional error rendered to the user.
  error?: string;
}

/// Context passed to every command handler — actions on the app state.
export interface SlashContext {
  openSettings: () => void;
  openDiagnostics: () => void;
  toggleTerminal: () => void;
  resetCore: () => Promise<void>;
  openCanvasFile: (path: string) => Promise<void>;
  /// Append a synthetic system note to the active session — used by
  /// /help and /workers to give the user inline feedback.
  appendSystemNote: (note: string) => void;
}

export interface SlashCommand {
  name: string;
  /// Short usage hint (e.g. "/canvas &lt;path&gt;").
  usage: string;
  /// One-line description shown in /help and the completion list.
  description: string;
  /// Handler — args is the whitespace-tokenised tail after the command name.
  run: (args: string[], ctx: SlashContext) => Promise<SlashResult>;
}

export const COMMANDS: SlashCommand[] = [
  {
    name: 'help',
    usage: '/help',
    description: 'List available slash commands.',
    run: async (_args, ctx) => {
      const lines = COMMANDS.map((c) => `${c.usage.padEnd(24)}  ${c.description}`);
      ctx.appendSystemNote(`Slash commands:\n${lines.join('\n')}`);
      return { ok: true };
    },
  },
  {
    name: 'clear',
    usage: '/clear',
    description: 'Clear the chat input. (History stays — use /reset to drop the live core.)',
    run: async () => {
      // Returning ok=true with no systemMessage just drops the input.
      return { ok: true };
    },
  },
  {
    name: 'reset',
    usage: '/reset',
    description: 'Terminate the session\'s Core live instance — next message respawns it fresh.',
    run: async (_args, ctx) => {
      await ctx.resetCore();
      return { ok: true, systemMessage: 'Core reset — next message will respawn the live instance.' };
    },
  },
  {
    name: 'workers',
    usage: '/workers',
    description: 'Open the diagnostics drawer (workers, telemetry, provider status).',
    run: async (_args, ctx) => {
      ctx.openDiagnostics();
      return { ok: true };
    },
  },
  {
    name: 'terminal',
    usage: '/terminal',
    description: 'Toggle the bottom terminal panel.',
    run: async (_args, ctx) => {
      ctx.toggleTerminal();
      return { ok: true };
    },
  },
  {
    name: 'settings',
    usage: '/settings',
    description: 'Open the Settings modal.',
    run: async (_args, ctx) => {
      ctx.openSettings();
      return { ok: true };
    },
  },
  {
    name: 'canvas',
    usage: '/canvas <path>',
    description: 'Open a file in the Canvas (workspace-relative or absolute).',
    run: async (args, ctx) => {
      const path = args.join(' ').trim();
      if (!path) {
        return { ok: false, error: 'Usage: /canvas <path>' };
      }
      try {
        await ctx.openCanvasFile(path);
        return { ok: true };
      } catch (e) {
        return { ok: false, error: `Could not open ${path}: ${e}` };
      }
    },
  },
  {
    name: 'hook',
    usage: '/hook <install|uninstall|status> [codex|claude|all]',
    description: 'Manage Switchyard hooks in Codex / Claude config files.',
    run: async (args, ctx) => {
      const action = (args[0] ?? '').toLowerCase();
      const provider = (args[1] ?? 'all').toLowerCase();
      if (!['install', 'uninstall', 'status'].includes(action)) {
        return {
          ok: false,
          error: 'Usage: /hook <install|uninstall|status> [codex|claude|all]',
        };
      }
      try {
        // Status returns a JSON struct from the backend; the others
        // return only ok/err. We surface a one-line summary either way.
        if (action === 'status') {
          const status = await invoke<{
            codex_config_path: string;
            codex_installed_events: string[];
            claude_config_path: string;
            claude_installed_events: string[];
          }>('hook_status');
          const codex = status.codex_installed_events.length;
          const claude = status.claude_installed_events.length;
          ctx.appendSystemNote(
            `Hook status — Codex: ${codex} event(s) at ${status.codex_config_path}, ` +
              `Claude: ${claude} event(s) at ${status.claude_config_path}`,
          );
          return { ok: true };
        }
        const cmd = action === 'install' ? 'hook_install' : 'hook_uninstall';
        await invoke(cmd, { provider });
        return {
          ok: true,
          systemMessage: `Hook ${action} (${provider}) succeeded.`,
        };
      } catch (e) {
        return { ok: false, error: `Hook ${action} failed: ${e}` };
      }
    },
  },
];

/// Parse a chat-input string. Returns the matched command + remaining
/// args, or `null` for non-slash inputs (which should fall through to
/// the regular chat dispatch).
export function parseSlash(input: string): { cmd: SlashCommand; args: string[] } | null {
  const trimmed = input.trim();
  if (!trimmed.startsWith('/')) return null;
  const [head, ...rest] = trimmed.slice(1).split(/\s+/);
  if (!head) return null;
  const cmd = COMMANDS.find((c) => c.name === head.toLowerCase());
  if (!cmd) return null;
  return { cmd, args: rest };
}

/// Completion list filtered by current prefix. Used by the chat input
/// to show inline suggestions while the user types `/foo`.
export function completeSlash(input: string): SlashCommand[] {
  const trimmed = input.trim();
  if (!trimmed.startsWith('/')) return [];
  const prefix = trimmed.slice(1).split(/\s+/)[0].toLowerCase();
  return COMMANDS.filter((c) => c.name.startsWith(prefix));
}
