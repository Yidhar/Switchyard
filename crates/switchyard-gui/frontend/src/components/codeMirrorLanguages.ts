/// Map a Switchyard `language` hint (from the backend `infer_language`
/// in switchyard-gui/src/main.rs) to a CodeMirror language extension.
/// Returns an empty array for "plaintext" / unknown — CodeMirror handles
/// those gracefully with no syntax highlighting.
///
/// Imports are eager (not dynamic) because tree-shaking already drops
/// language extensions we don't reference; dynamic-import would add
/// async complexity to every editor mount without a real bundle win.

import type { Extension } from '@codemirror/state';
import { StreamLanguage } from '@codemirror/language';

// First-party language packs — these come with real parsers + rich
// highlighting groups. The "lang-" packages are CodeMirror 6's
// preferred path.
import { markdown } from '@codemirror/lang-markdown';
import { javascript } from '@codemirror/lang-javascript';
import { rust } from '@codemirror/lang-rust';
import { python } from '@codemirror/lang-python';
import { json } from '@codemirror/lang-json';
import { css } from '@codemirror/lang-css';
import { html } from '@codemirror/lang-html';
import { cpp } from '@codemirror/lang-cpp';
import { go } from '@codemirror/lang-go';
import { sql } from '@codemirror/lang-sql';
import { yaml } from '@codemirror/lang-yaml';
import { java } from '@codemirror/lang-java';
import { php } from '@codemirror/lang-php';
import { xml } from '@codemirror/lang-xml';

// Legacy-modes — older but battle-tested highlighters for languages
// that don't (yet) have a first-party lang-* package. Each is a
// StreamParser we wrap with `StreamLanguage.define`.
import { shell } from '@codemirror/legacy-modes/mode/shell';
import { ruby } from '@codemirror/legacy-modes/mode/ruby';
import { toml } from '@codemirror/legacy-modes/mode/toml';
import { dockerFile } from '@codemirror/legacy-modes/mode/dockerfile';
import { powerShell } from '@codemirror/legacy-modes/mode/powershell';
import { swift } from '@codemirror/legacy-modes/mode/swift';
import { kotlin } from '@codemirror/legacy-modes/mode/clike';
import { csharp } from '@codemirror/legacy-modes/mode/clike';
import { batchParser } from './batchMode';

const TS_OR_JS = javascript({ jsx: true, typescript: true });

const REGISTRY: Record<string, Extension> = {
  // Markup / data
  markdown: markdown(),
  md: markdown(),
  html: html(),
  htm: html(),
  vue: html(),
  xml: xml(),
  json: json(),
  yaml: yaml(),
  yml: yaml(),
  toml: StreamLanguage.define(toml),
  // Styles
  css: css(),
  scss: css(),
  less: css(),
  sass: css(),
  // JS family
  javascript: javascript({ jsx: true }),
  js: javascript({ jsx: true }),
  jsx: javascript({ jsx: true }),
  typescript: TS_OR_JS,
  ts: TS_OR_JS,
  tsx: TS_OR_JS,
  // Systems / compiled
  rust: rust(),
  rs: rust(),
  c: cpp(),
  cpp: cpp(),
  'c++': cpp(),
  go: go(),
  java: java(),
  kotlin: StreamLanguage.define(kotlin),
  swift: StreamLanguage.define(swift),
  csharp: StreamLanguage.define(csharp),
  // Scripting
  python: python(),
  py: python(),
  ruby: StreamLanguage.define(ruby),
  rb: StreamLanguage.define(ruby),
  php: php(),
  shell: StreamLanguage.define(shell),
  bash: StreamLanguage.define(shell),
  zsh: StreamLanguage.define(shell),
  sh: StreamLanguage.define(shell),
  powershell: StreamLanguage.define(powerShell),
  ps1: StreamLanguage.define(powerShell),
  batch: StreamLanguage.define(batchParser),
  bat: StreamLanguage.define(batchParser),
  cmd: StreamLanguage.define(batchParser),
  // Query / config
  sql: sql(),
  dockerfile: StreamLanguage.define(dockerFile),
};

export function languageExtensionFor(language: string | undefined): Extension[] {
  if (!language) return [];
  const ext = REGISTRY[language.toLowerCase()];
  return ext ? [ext] : [];
}
