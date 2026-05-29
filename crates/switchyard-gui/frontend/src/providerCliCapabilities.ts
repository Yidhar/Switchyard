export type ProviderBackend = 'codex' | 'claude' | 'gemini' | 'antigravity' | string;

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
