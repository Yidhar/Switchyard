/// Minimal Windows-batch (`.bat` / `.cmd`) highlighter for CodeMirror 6.
///
/// There's no first-party `@codemirror/lang-bat` or legacy-modes entry
/// for batch, and falling back to the shell parser misses every batch
/// idiom (`REM` / `::` comments, `%VAR%` expansion, `:label` targets,
/// `@echo off`, etc.). This module wraps a tiny `StreamParser` with
/// the common cues so `.bat` files don't render as plain text.
///
/// Coverage is intentionally narrow — we map tokens to the same set of
/// generic CodeMirror tags the theme already styles ("keyword",
/// "comment", "string", "variableName", "labelName", "operator",
/// "number"). One-Dark + the default highlight style give them the
/// usual color spread without any theme-specific work.

import type { StreamParser } from '@codemirror/language';

/// Keywords lifted from Microsoft's batch reference. Lowercased; the
/// tokenizer compares case-insensitively because real `.bat` files
/// mix `if`/`IF`/`If` freely.
const KEYWORDS = new Set([
  '@echo',
  'echo',
  'set',
  'setlocal',
  'endlocal',
  'if',
  'else',
  'for',
  'in',
  'do',
  'goto',
  'call',
  'exit',
  'pause',
  'rem',
  'cls',
  'cd',
  'pushd',
  'popd',
  'shift',
  'start',
  'title',
  'color',
  'errorlevel',
  'not',
  'defined',
  'exist',
  'equ',
  'neq',
  'lss',
  'leq',
  'gtr',
  'geq',
  'choice',
  'find',
  'findstr',
  'type',
  'copy',
  'xcopy',
  'move',
  'del',
  'rd',
  'rmdir',
  'md',
  'mkdir',
  'attrib',
  'where',
  'tasklist',
  'taskkill',
  'sc',
  'reg',
  'wmic',
]);

interface BatchState {
  /// True when the line started with `:` (a label) so the rest of the
  /// line is highlighted as the label name.
  inLabel: boolean;
}

export const batchParser: StreamParser<BatchState> = {
  name: 'batch',
  startState: (): BatchState => ({ inLabel: false }),
  token(stream, state) {
    // Reset label flag at line start so it only applies to its own line.
    if (stream.sol()) {
      state.inLabel = false;
      // Eat leading whitespace so we can sniff the first significant
      // char and decide whether the line is a comment / label.
      stream.eatSpace();

      // `::` style comments (and `REM ` style further down).
      if (stream.match(/^::.*$/)) return 'comment';

      // `:label` — colon at start of a non-empty line, followed by an
      // identifier. Differentiated from `::` above by the trailing
      // char not being another `:`.
      if (stream.match(/^:[A-Za-z_][\w.-]*\b/)) {
        return 'labelName';
      }
    }

    // Mid-line whitespace.
    if (stream.eatSpace()) return null;

    // `REM …` comment — case-insensitive, must be a word boundary.
    if (stream.match(/^rem\b.*$/i)) return 'comment';

    // Double-quoted string — `.bat` doesn't escape `"`, so anything
    // up to the next quote or EOL is the body.
    if (stream.match(/^"[^"\n]*"?/)) return 'string';

    // `%VAR%`, `%~1`, `%*`, etc. — percent-delimited expansion.
    if (stream.match(/^%[^%\n]*%/)) return 'variableName';
    if (stream.match(/^%[*~]?\d/)) return 'variableName';
    if (stream.match(/^%~[a-zA-Z]+[\d*]?/)) return 'variableName';

    // `!VAR!` delayed expansion (when `setlocal enabledelayedexpansion`).
    if (stream.match(/^![^!\n]+!/)) return 'variableName';

    // Operators and redirects.
    if (stream.match(/^(\|\||&&|>>|<<|[|&><=])/)) return 'operator';

    // Numbers.
    if (stream.match(/^\d+\b/)) return 'number';

    // Identifier — possibly a keyword.
    const ident = stream.match(/^[A-Za-z_][\w.-]*/) as RegExpMatchArray | null;
    if (ident) {
      const word = ident[0].toLowerCase();
      if (KEYWORDS.has(word)) return 'keyword';
      // Bare `@echo off` etc. — the `@` is eaten elsewhere; standalone
      // identifiers get the default style.
      return null;
    }

    // `@` prefix on a line (suppresses echo).
    if (stream.match(/^@/)) return 'keyword';

    // Anything else — advance one char and don't tag it.
    stream.next();
    return null;
  },
  languageData: {
    commentTokens: { line: 'REM' },
  },
};
