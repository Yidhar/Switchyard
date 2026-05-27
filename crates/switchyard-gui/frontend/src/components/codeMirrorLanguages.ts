/// Map a Switchyard `language` hint (from the backend `infer_language`
/// in switchyard-gui/src/main.rs) to a CodeMirror language extension.
/// Returns an empty array for "plaintext" / unknown — CodeMirror handles
/// those gracefully with no syntax highlighting.
///
/// Language packs are intentionally loaded on demand. The Canvas itself is
/// lazy-loaded from App.tsx, but importing every `@codemirror/lang-*` module
/// here still made the editor chunk enormous and blocked the first file-open
/// path. This async registry keeps the initial editor payload focused on the
/// editor shell; syntax support arrives one language at a time and is cached
/// for subsequent tabs.

import type { Extension } from '@codemirror/state';
import { StreamLanguage } from '@codemirror/language';
import { batchParser } from './batchMode';

type LanguageLoader = () => Promise<Extension>;

const cachedLanguages = new Map<string, Promise<Extension[]>>();

const sharedJavascript = (() => {
  let cached: Promise<Extension> | null = null;
  return () => {
    if (!cached) {
      cached = import('@codemirror/lang-javascript').then(({ javascript }) =>
        javascript({ jsx: true, typescript: true }),
      );
    }
    return cached;
  };
})();

const REGISTRY: Record<string, LanguageLoader> = {
  // Markup / data
  markdown: () => import('@codemirror/lang-markdown').then(({ markdown }) => markdown()),
  md: () => import('@codemirror/lang-markdown').then(({ markdown }) => markdown()),
  html: () => import('@codemirror/lang-html').then(({ html }) => html()),
  htm: () => import('@codemirror/lang-html').then(({ html }) => html()),
  vue: () => import('@codemirror/lang-html').then(({ html }) => html()),
  xml: () => import('@codemirror/lang-xml').then(({ xml }) => xml()),
  json: () => import('@codemirror/lang-json').then(({ json }) => json()),
  yaml: () => import('@codemirror/lang-yaml').then(({ yaml }) => yaml()),
  yml: () => import('@codemirror/lang-yaml').then(({ yaml }) => yaml()),
  toml: () => import('@codemirror/legacy-modes/mode/toml').then(({ toml }) => StreamLanguage.define(toml)),
  // Styles
  css: () => import('@codemirror/lang-css').then(({ css }) => css()),
  scss: () => import('@codemirror/lang-css').then(({ css }) => css()),
  less: () => import('@codemirror/lang-css').then(({ css }) => css()),
  sass: () => import('@codemirror/lang-css').then(({ css }) => css()),
  // JS family
  javascript: () => import('@codemirror/lang-javascript').then(({ javascript }) => javascript({ jsx: true })),
  js: () => import('@codemirror/lang-javascript').then(({ javascript }) => javascript({ jsx: true })),
  jsx: () => import('@codemirror/lang-javascript').then(({ javascript }) => javascript({ jsx: true })),
  typescript: sharedJavascript,
  ts: sharedJavascript,
  tsx: sharedJavascript,
  // Systems / compiled
  rust: () => import('@codemirror/lang-rust').then(({ rust }) => rust()),
  rs: () => import('@codemirror/lang-rust').then(({ rust }) => rust()),
  c: () => import('@codemirror/lang-cpp').then(({ cpp }) => cpp()),
  cpp: () => import('@codemirror/lang-cpp').then(({ cpp }) => cpp()),
  'c++': () => import('@codemirror/lang-cpp').then(({ cpp }) => cpp()),
  go: () => import('@codemirror/lang-go').then(({ go }) => go()),
  java: () => import('@codemirror/lang-java').then(({ java }) => java()),
  kotlin: () => import('@codemirror/legacy-modes/mode/clike').then(({ kotlin }) => StreamLanguage.define(kotlin)),
  swift: () => import('@codemirror/legacy-modes/mode/swift').then(({ swift }) => StreamLanguage.define(swift)),
  csharp: () => import('@codemirror/legacy-modes/mode/clike').then(({ csharp }) => StreamLanguage.define(csharp)),
  // Scripting
  python: () => import('@codemirror/lang-python').then(({ python }) => python()),
  py: () => import('@codemirror/lang-python').then(({ python }) => python()),
  ruby: () => import('@codemirror/legacy-modes/mode/ruby').then(({ ruby }) => StreamLanguage.define(ruby)),
  rb: () => import('@codemirror/legacy-modes/mode/ruby').then(({ ruby }) => StreamLanguage.define(ruby)),
  php: () => import('@codemirror/lang-php').then(({ php }) => php()),
  shell: () => import('@codemirror/legacy-modes/mode/shell').then(({ shell }) => StreamLanguage.define(shell)),
  bash: () => import('@codemirror/legacy-modes/mode/shell').then(({ shell }) => StreamLanguage.define(shell)),
  zsh: () => import('@codemirror/legacy-modes/mode/shell').then(({ shell }) => StreamLanguage.define(shell)),
  sh: () => import('@codemirror/legacy-modes/mode/shell').then(({ shell }) => StreamLanguage.define(shell)),
  powershell: () => import('@codemirror/legacy-modes/mode/powershell').then(({ powerShell }) => StreamLanguage.define(powerShell)),
  ps1: () => import('@codemirror/legacy-modes/mode/powershell').then(({ powerShell }) => StreamLanguage.define(powerShell)),
  batch: () => Promise.resolve(StreamLanguage.define(batchParser)),
  bat: () => Promise.resolve(StreamLanguage.define(batchParser)),
  cmd: () => Promise.resolve(StreamLanguage.define(batchParser)),
  // Query / config
  sql: () => import('@codemirror/lang-sql').then(({ sql }) => sql()),
  dockerfile: () => import('@codemirror/legacy-modes/mode/dockerfile').then(({ dockerFile }) => StreamLanguage.define(dockerFile)),
};

export function loadLanguageExtensionsFor(language: string | undefined): Promise<Extension[]> {
  if (!language) return Promise.resolve([]);
  const key = language.toLowerCase();
  const loader = REGISTRY[key];
  if (!loader) return Promise.resolve([]);
  const cached = cachedLanguages.get(key);
  if (cached) return cached;
  const pending = loader()
    .then((extension) => [extension])
    .catch((error) => {
      cachedLanguages.delete(key);
      console.warn(`[switchyard] Failed to load CodeMirror language "${key}"`, error);
      return [];
    });
  cachedLanguages.set(key, pending);
  return pending;
}
