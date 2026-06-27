import type { ProviderConfig } from './types';

export type ProviderBackend =
  | 'codex'
  | 'claude'
  | 'gemini'
  | 'antigravity'
  | 'kohaku'
  | string;

/// Built-in provider backends, always offered in the UI even when the active
/// workspace's switchyard.toml does not list them (mirrors the Rust
/// `BUILT_IN_PROVIDER_ALIASES`).
export const BUILTIN_PROVIDER_NAMES = [
  'codex',
  'claude',
  'gemini',
  'antigravity',
  'kohaku',
];

/// Default subprocess command for a known backend (mirrors the Rust
/// `default_provider_command`).
export function defaultProviderCommand(backend: ProviderBackend): string {
  switch (backend) {
    case 'codex':
      return 'codex';
    case 'claude':
      return 'claude';
    case 'gemini':
      return 'gemini';
    case 'antigravity':
      return 'agy';
    case 'kohaku':
      return 'kt';
    default:
      return '';
  }
}

/// A complete default ProviderConfig for a provider name — used to seed an
/// editable Settings entry for a built-in that isn't in the config yet, so
/// partial/broken entries are never created on first edit.
export function defaultProviderConfigFor(name: string): ProviderConfig {
  const backend = inferProviderBackend(name);
  // KohakuTerrarium needs a creature as the first arg; default to the
  // official kt-biome `general` creature so it works out of the box.
  const args = backend === 'kohaku' ? ['@kt-biome/creatures/general'] : [];
  return {
    command: defaultProviderCommand(backend),
    args,
    env: {},
    model: null,
    thinking_level: null,
    timeout_secs: 0,
    backend: backend || null,
  };
}


export interface ThinkingLevelOption {
  value: string;
  label: string;
}

export interface ProviderCliMapping {
  backend: ProviderBackend;
  modelMapped: boolean;
  thinkingMapped: boolean;
  summary: string;
  modelHint: string;
  thinkingHint: string;
}

export const THINKING_LEVEL_OPTIONS: ThinkingLevelOption[] = [
  { value: '', label: 'Auto / CLI default' },
  { value: 'minimal', label: 'Minimal' },
  { value: 'low', label: 'Low' },
  { value: 'medium', label: 'Medium' },
  { value: 'high', label: 'High / Deep' },
  { value: 'xhigh', label: 'Extra High (Claude)' },
  { value: 'max', label: 'Max (Claude)' },
];

export function inferProviderBackend(
  providerName: string,
  backend?: string | null,
): ProviderBackend {
  const explicit = (backend ?? '').trim().toLowerCase();
  if (explicit) return explicit;

  const name = providerName.toLowerCase();
  if (name.includes('codex')) return 'codex';
  if (name.includes('claude')) return 'claude';
  if (name.includes('antigravity') || name.includes('agy')) return 'antigravity';
  if (name.includes('gemini')) return 'gemini';
  if (name.includes('kohaku')) return 'kohaku';
  return '';
}

export function providerCliMapping(
  providerName: string,
  backend?: string | null,
): ProviderCliMapping {
  const inferred = inferProviderBackend(providerName, backend);
  switch (inferred) {
    case 'codex':
      return {
        backend: inferred,
        modelMapped: true,
        thinkingMapped: true,
        summary:
          'Codex: Switchyard passes -c model="<model>" and -c model_reasoning_effort=<level>, which works for both exec and app-server.',
        modelHint: '-c model="<model>"',
        thinkingHint: '-c model_reasoning_effort=<minimal|low|medium|high>',
      };
    case 'claude':
      return {
        backend: inferred,
        modelMapped: true,
        thinkingMapped: true,
        summary:
          'Claude: Switchyard passes --model <model> and --effort <level>.',
        modelHint: '--model <model>',
        thinkingHint: '--effort <low|medium|high|xhigh|max>',
      };
    case 'gemini':
      return {
        backend: inferred,
        modelMapped: true,
        thinkingMapped: false,
        summary:
          'Gemini: Switchyard passes --model <model>. Thinking level is kept as metadata because Gemini CLI reasoning flags are not stable across releases; add raw extra args if needed.',
        modelHint: '--model <model>',
        thinkingHint: 'Not mapped automatically; use extra args for release-specific flags.',
      };
    case 'antigravity':
      return {
        backend: inferred,
        modelMapped: false,
        thinkingMapped: false,
        summary:
          'Antigravity / agy: current CLI has no stable --model or thinking flag, so Switchyard keeps the CLI default and does not synthesize runtime flags.',
        modelHint: 'Not mapped; agy keeps its configured/default model.',
        thinkingHint: 'Not mapped; agy keeps its configured/default thinking behavior.',
      };
    case 'kohaku':
      return {
        backend: inferred,
        modelMapped: true,
        thinkingMapped: false,
        summary:
          'KohakuTerrarium (kt): Switchyard runs `kt run <creature> --headless --json -p <prompt>`. ' +
          'Set "Subprocess CLI Command" to `kt` if it is on PATH, otherwise the full path to kt.exe ' +
          '(e.g. <venv>\\Scripts\\kt.exe). The FIRST CLI Execution Argument must be the creature ref ' +
          '(a config-folder path or @pkg/creatures/<name>). Default Model maps to `--llm <selector>`; ' +
          'the sandbox mode maps to `--sandbox READ_ONLY|WORKSPACE|off`. Requires a kt with headless ' +
          'support (the switchyard-headless fork).',
        modelHint: '--llm <selector> (e.g. enzi/gpt-5.5-custom)',
        thinkingHint:
          'Not a separate flag; encode reasoning in the --llm selector, e.g. <selector>@reasoning=low.',
      };
    default:
      return {
        backend: inferred,
        modelMapped: false,
        thinkingMapped: false,
        summary:
          'Custom backend: Switchyard stores model/thinking values, but only built-in adapters map them to CLI flags.',
        modelHint: 'Not mapped automatically for custom backends.',
        thinkingHint: 'Not mapped automatically for custom backends.',
      };
  }
}
