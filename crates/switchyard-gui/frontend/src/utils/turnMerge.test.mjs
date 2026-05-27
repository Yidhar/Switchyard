import assert from 'node:assert/strict';
import fs from 'node:fs';
import vm from 'node:vm';
import { fileURLToPath } from 'node:url';
import ts from 'typescript';

const sourcePath = fileURLToPath(new URL('./turnMerge.ts', import.meta.url));
const source = fs.readFileSync(sourcePath, 'utf8');
const { outputText } = ts.transpileModule(source, {
  compilerOptions: {
    module: ts.ModuleKind.CommonJS,
    target: ts.ScriptTarget.ES2022,
  },
});

const module = { exports: {} };
vm.runInNewContext(outputText, { module, exports: module.exports }, { filename: 'turnMerge.js' });

const {
  fallbackResponseForUserMessage,
  mergeFallbackResponseIntoTurns,
  mergeFinalResponseIntoTurns,
  mergeFreshTurnsPreservingKnownResponses,
  nonBlankText,
} = module.exports;

function makeTurn(overrides = {}) {
  return {
    turn_id: 'turn-1',
    session_id: 'session-1',
    origin: 'user',
    provider: 'codex',
    role: 'core',
    user_message: 'hello',
    provider_response: null,
    error_message: null,
    status: 'pending',
    started_at: '2026-05-28T00:00:00.000Z',
    completed_at: null,
    delegated_by: null,
    ...overrides,
  };
}

assert.equal(nonBlankText(null), null);
assert.equal(nonBlankText('   '), null);
assert.equal(nonBlankText('final answer'), 'final answer');

assert.equal(fallbackResponseForUserMessage('hello', 'hello'), null);
assert.equal(fallbackResponseForUserMessage('  hello  ', 'hello'), null);
assert.equal(fallbackResponseForUserMessage('hello\r\nthere', 'hello\nthere'), null);
assert.equal(fallbackResponseForUserMessage('actual assistant answer', 'hello'), 'actual assistant answer');

{
  const [turn] = mergeFinalResponseIntoTurns([makeTurn()], 'turn-1', 'final answer', 'completed');
  assert.equal(turn.provider_response, 'final answer');
  assert.equal(turn.status, 'completed');
  assert.ok(turn.completed_at, 'completed turns should receive a completed_at fallback');
}

{
  const [turn] = mergeFinalResponseIntoTurns([makeTurn({ user_message: 'hello' })], 'turn-1', 'hello', 'completed');
  assert.equal(turn.provider_response, null);
  assert.equal(turn.status, 'completed');
}

{
  const original = makeTurn({ provider_response: 'kept answer' });
  const merged = mergeFinalResponseIntoTurns([original], 'turn-1', '   ');
  assert.equal(merged[0].provider_response, 'kept answer');
}

{
  const original = makeTurn({ provider_response: null });
  const merged = mergeFallbackResponseIntoTurns([original], 'turn-1', 'fallback answer');
  assert.equal(merged[0].provider_response, 'fallback answer');
}

{
  const original = makeTurn({ user_message: 'hello', provider_response: null });
  const merged = mergeFallbackResponseIntoTurns([original], 'turn-1', 'hello');
  assert.equal(merged[0].provider_response, null);
}

{
  const original = makeTurn({ provider_response: 'db answer' });
  const merged = mergeFallbackResponseIntoTurns([original], 'turn-1', 'fallback answer');
  assert.equal(merged[0].provider_response, 'db answer');
}

{
  const previous = [makeTurn({ provider_response: 'known final answer', status: 'completed' })];
  const staleRefresh = [makeTurn({ provider_response: null, status: 'completed' })];
  const merged = mergeFreshTurnsPreservingKnownResponses(previous, staleRefresh);
  assert.equal(merged[0].provider_response, 'known final answer');
}

{
  const previous = [makeTurn({ provider_response: 'old in-memory answer' })];
  const authoritativeRefresh = [makeTurn({ provider_response: 'db final answer' })];
  const merged = mergeFreshTurnsPreservingKnownResponses(previous, authoritativeRefresh);
  assert.equal(merged[0].provider_response, 'db final answer');
}

{
  const previous = [makeTurn({ error_message: 'known error' })];
  const staleRefresh = [makeTurn({ error_message: null })];
  const merged = mergeFreshTurnsPreservingKnownResponses(previous, staleRefresh);
  assert.equal(merged[0].error_message, 'known error');
}

console.log('turnMerge regression tests passed');
