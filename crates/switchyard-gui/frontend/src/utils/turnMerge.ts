import type { Turn } from '../types';

export function nonBlankText(value: unknown): string | null {
  if (value === undefined || value === null) return null;
  const text = typeof value === 'string' ? value : String(value);
  return text.trim().length > 0 ? text : null;
}

function normalizeFallbackComparisonText(value: string): string {
  return value.replace(/\r\n/g, '\n').trim();
}

export function fallbackResponseForUserMessage(response: unknown, userMessage: string): string | null {
  const responseText = nonBlankText(response);
  if (responseText === null) return null;

  const normalizedResponse = normalizeFallbackComparisonText(responseText);
  const normalizedUserMessage = normalizeFallbackComparisonText(userMessage);
  if (normalizedUserMessage.length > 0 && normalizedResponse === normalizedUserMessage) {
    return null;
  }

  return responseText;
}

function responseForTurn(response: unknown, turn: Turn): string | null {
  const userMessage = nonBlankText(turn.user_message);
  if (userMessage !== null) {
    return fallbackResponseForUserMessage(response, userMessage);
  }
  return nonBlankText(response);
}

function completedAtFor(status: Turn['status'] | undefined, currentCompletedAt: string | null): string | null {
  if (currentCompletedAt) return currentCompletedAt;
  if (status === 'completed' || status === 'failed' || status === 'cancelled') {
    return new Date().toISOString();
  }
  return currentCompletedAt;
}

export function mergeFinalResponseIntoTurns(
  turns: Turn[],
  turnId: string | null | undefined,
  response: unknown,
  status?: Turn['status'],
): Turn[] {
  if (!turnId) return turns;
  let changed = false;

  const nextTurns = turns.map((turn) => {
    if (turn.turn_id !== turnId) return turn;

    const next: Turn = { ...turn };
    const responseText = responseForTurn(response, turn);
    if (responseText !== null && next.provider_response !== responseText) {
      next.provider_response = responseText;
    }
    if (status !== undefined && next.status !== status) {
      next.status = status;
    }
    const completedAt = completedAtFor(status, next.completed_at);
    if (completedAt !== next.completed_at) {
      next.completed_at = completedAt;
    }

    const didChange =
      next.provider_response !== turn.provider_response ||
      next.status !== turn.status ||
      next.completed_at !== turn.completed_at;
    if (didChange) changed = true;
    return didChange ? next : turn;
  });

  return changed ? nextTurns : turns;
}

export function mergeFallbackResponseIntoTurns(
  turns: Turn[],
  turnId: string | null | undefined,
  response: unknown,
): Turn[] {
  if (!turnId) return turns;
  const responseText = nonBlankText(response);
  if (responseText === null) return turns;

  let changed = false;
  const nextTurns = turns.map((turn) => {
    if (turn.turn_id !== turnId) return turn;
    if (nonBlankText(turn.provider_response) !== null) return turn;
    const responseForThisTurn = responseForTurn(responseText, turn);
    if (responseForThisTurn === null) return turn;

    changed = true;
    return {
      ...turn,
      provider_response: responseForThisTurn,
    };
  });

  return changed ? nextTurns : turns;
}

export function mergeFreshTurnsPreservingKnownResponses(previousTurns: Turn[], freshTurns: Turn[]): Turn[] {
  if (previousTurns.length === 0) return freshTurns;
  const previousById = new Map(previousTurns.map((turn) => [turn.turn_id, turn]));
  let changed = false;

  const merged = freshTurns.map((freshTurn) => {
    const knownTurn = previousById.get(freshTurn.turn_id);
    if (!knownTurn) return freshTurn;

    const knownResponse = nonBlankText(knownTurn.provider_response);
    const freshResponse = nonBlankText(freshTurn.provider_response);
    const knownError = nonBlankText(knownTurn.error_message);
    const freshError = nonBlankText(freshTurn.error_message);
    const shouldPreserveResponse = knownResponse !== null && freshResponse === null;
    const shouldPreserveError = knownError !== null && freshError === null;

    if (!shouldPreserveResponse && !shouldPreserveError) return freshTurn;
    changed = true;
    return {
      ...freshTurn,
      provider_response: shouldPreserveResponse ? knownTurn.provider_response : freshTurn.provider_response,
      error_message: shouldPreserveError ? knownTurn.error_message : freshTurn.error_message,
    };
  });

  return changed ? merged : freshTurns;
}
