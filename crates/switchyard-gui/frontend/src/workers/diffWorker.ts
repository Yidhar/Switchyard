/// <reference lib="webworker" />

export {};

type DiffKind = 'equal' | 'remove' | 'add';

interface BasicDiffRow {
  kind: DiffKind;
  text: string;
}

interface DiffRow extends BasicDiffRow {
  beforeLine: number | null;
  afterLine: number | null;
}

interface DiffWorkerRequest {
  requestId: number;
  before: string;
  after: string;
}

interface DiffWorkerResult {
  requestId: number;
  status: 'done';
  rows: DiffRow[];
  beforeLines: number;
  afterLines: number;
  additions: number;
  deletions: number;
  algorithm: string;
  durationMs: number;
  exact: boolean;
}

interface DiffWorkerError {
  requestId: number;
  status: 'error';
  message: string;
}

interface DiffMetrics {
  algorithm: 'exact-lcs' | 'anchored-lcs' | 'bounded-fallback';
  exact: boolean;
}

interface AnchorPair {
  a: number;
  b: number;
}

// Exact LCS is still useful for small/medium changed regions because it gives
// a stable, readable line diff. It now runs in a worker and is applied only to
// trimmed or anchor-bounded chunks, so the React render thread is never blocked.
//
// 16M cells is ~64MB for the Uint32Array DP matrix. That is intentionally much
// higher than the old main-thread 350k guard, but still bounded so a pathological
// whole-file rewrite cannot exhaust memory in the worker.
const MAX_EXACT_LCS_CELLS = 16_000_000;
const MAX_ANCHOR_RECURSION_DEPTH = 12;

const worker = self as DedicatedWorkerGlobalScope;

worker.onmessage = (event: MessageEvent<DiffWorkerRequest>) => {
  const { requestId, before, after } = event.data;
  const startedAt = performance.now();
  try {
    const beforeLines = splitLines(before);
    const afterLines = splitLines(after);
    const metrics: DiffMetrics = {
      algorithm: 'exact-lcs',
      exact: true,
    };
    const basicRows: BasicDiffRow[] = [];
    appendRangeDiff(
      beforeLines,
      0,
      beforeLines.length,
      afterLines,
      0,
      afterLines.length,
      basicRows,
      metrics,
      0,
    );

    const { rows, additions, deletions } = annotateRows(basicRows);
    const result: DiffWorkerResult = {
      requestId,
      status: 'done',
      rows,
      beforeLines: beforeLines.length,
      afterLines: afterLines.length,
      additions,
      deletions,
      algorithm: metrics.algorithm,
      durationMs: Math.round(performance.now() - startedAt),
      exact: metrics.exact,
    };
    worker.postMessage(result);
  } catch (error) {
    const message = error instanceof Error ? error.message : String(error);
    const result: DiffWorkerError = {
      requestId,
      status: 'error',
      message,
    };
    worker.postMessage(result);
  }
};

function splitLines(text: string): string[] {
  // Match the old Canvas behavior exactly: a trailing newline produces a final
  // empty row, which keeps line numbering consistent with the editor snapshot.
  return text.split('\n');
}

function appendRangeDiff(
  a: string[],
  aStart: number,
  aEnd: number,
  b: string[],
  bStart: number,
  bEnd: number,
  rows: BasicDiffRow[],
  metrics: DiffMetrics,
  depth: number,
): void {
  let leftStart = aStart;
  let rightStart = bStart;
  let leftEnd = aEnd;
  let rightEnd = bEnd;

  while (leftStart < leftEnd && rightStart < rightEnd && a[leftStart] === b[rightStart]) {
    rows.push({ kind: 'equal', text: a[leftStart] });
    leftStart += 1;
    rightStart += 1;
  }

  const suffix: string[] = [];
  while (leftStart < leftEnd && rightStart < rightEnd && a[leftEnd - 1] === b[rightEnd - 1]) {
    suffix.push(a[leftEnd - 1]);
    leftEnd -= 1;
    rightEnd -= 1;
  }

  const leftLength = leftEnd - leftStart;
  const rightLength = rightEnd - rightStart;

  if (leftLength === 0) {
    for (let j = rightStart; j < rightEnd; j += 1) {
      rows.push({ kind: 'add', text: b[j] });
    }
  } else if (rightLength === 0) {
    for (let i = leftStart; i < leftEnd; i += 1) {
      rows.push({ kind: 'remove', text: a[i] });
    }
  } else if (leftLength * rightLength <= MAX_EXACT_LCS_CELLS) {
    appendExactLcsRows(a, leftStart, leftEnd, b, rightStart, rightEnd, rows);
  } else {
    const anchors = depth < MAX_ANCHOR_RECURSION_DEPTH
      ? uniqueLineAnchors(a, leftStart, leftEnd, b, rightStart, rightEnd)
      : [];

    if (anchors.length > 0) {
      if (metrics.algorithm === 'exact-lcs') {
        metrics.algorithm = 'anchored-lcs';
      }
      let currentA = leftStart;
      let currentB = rightStart;
      for (const anchor of anchors) {
        appendRangeDiff(
          a,
          currentA,
          anchor.a,
          b,
          currentB,
          anchor.b,
          rows,
          metrics,
          depth + 1,
        );
        rows.push({ kind: 'equal', text: a[anchor.a] });
        currentA = anchor.a + 1;
        currentB = anchor.b + 1;
      }
      appendRangeDiff(
        a,
        currentA,
        leftEnd,
        b,
        currentB,
        rightEnd,
        rows,
        metrics,
        depth + 1,
      );
    } else {
      // Worst-case fallback: keep the UI usable and still show every changed
      // line, but do not attempt an O(m*n) exact alignment for this range.
      metrics.algorithm = 'bounded-fallback';
      metrics.exact = false;
      for (let i = leftStart; i < leftEnd; i += 1) {
        rows.push({ kind: 'remove', text: a[i] });
      }
      for (let j = rightStart; j < rightEnd; j += 1) {
        rows.push({ kind: 'add', text: b[j] });
      }
    }
  }

  for (let i = suffix.length - 1; i >= 0; i -= 1) {
    rows.push({ kind: 'equal', text: suffix[i] });
  }
}

function appendExactLcsRows(
  a: string[],
  aStart: number,
  aEnd: number,
  b: string[],
  bStart: number,
  bEnd: number,
  rows: BasicDiffRow[],
): void {
  const m = aEnd - aStart;
  const n = bEnd - bStart;
  const width = n + 1;
  const lcs = new Uint32Array((m + 1) * (n + 1));

  for (let i = 1; i <= m; i += 1) {
    const rowOffset = i * width;
    const previousRowOffset = (i - 1) * width;
    const ai = a[aStart + i - 1];
    for (let j = 1; j <= n; j += 1) {
      if (ai === b[bStart + j - 1]) {
        lcs[rowOffset + j] = lcs[previousRowOffset + j - 1] + 1;
      } else {
        const up = lcs[previousRowOffset + j];
        const left = lcs[rowOffset + j - 1];
        lcs[rowOffset + j] = up >= left ? up : left;
      }
    }
  }

  const reversed: BasicDiffRow[] = [];
  let i = m;
  let j = n;
  while (i > 0 && j > 0) {
    if (a[aStart + i - 1] === b[bStart + j - 1]) {
      reversed.push({ kind: 'equal', text: a[aStart + i - 1] });
      i -= 1;
      j -= 1;
    } else if (lcs[(i - 1) * width + j] >= lcs[i * width + j - 1]) {
      reversed.push({ kind: 'remove', text: a[aStart + i - 1] });
      i -= 1;
    } else {
      reversed.push({ kind: 'add', text: b[bStart + j - 1] });
      j -= 1;
    }
  }
  while (i > 0) {
    reversed.push({ kind: 'remove', text: a[aStart + i - 1] });
    i -= 1;
  }
  while (j > 0) {
    reversed.push({ kind: 'add', text: b[bStart + j - 1] });
    j -= 1;
  }

  for (let idx = reversed.length - 1; idx >= 0; idx -= 1) {
    rows.push(reversed[idx]);
  }
}

function uniqueLineAnchors(
  a: string[],
  aStart: number,
  aEnd: number,
  b: string[],
  bStart: number,
  bEnd: number,
): AnchorPair[] {
  const aCounts = new Map<string, number>();
  const bCounts = new Map<string, number>();
  const bIndex = new Map<string, number>();

  for (let i = aStart; i < aEnd; i += 1) {
    const line = a[i];
    aCounts.set(line, Math.min(2, (aCounts.get(line) ?? 0) + 1));
  }
  for (let j = bStart; j < bEnd; j += 1) {
    const line = b[j];
    const nextCount = Math.min(2, (bCounts.get(line) ?? 0) + 1);
    bCounts.set(line, nextCount);
    if (nextCount === 1) {
      bIndex.set(line, j);
    } else {
      bIndex.delete(line);
    }
  }

  const pairs: AnchorPair[] = [];
  for (let i = aStart; i < aEnd; i += 1) {
    const line = a[i];
    if (aCounts.get(line) !== 1 || bCounts.get(line) !== 1) continue;
    const j = bIndex.get(line);
    if (j !== undefined) {
      pairs.push({ a: i, b: j });
    }
  }

  if (pairs.length <= 1) return pairs;
  return longestIncreasingAnchorSubsequence(pairs);
}

function longestIncreasingAnchorSubsequence(pairs: AnchorPair[]): AnchorPair[] {
  const tails: number[] = [];
  const previous = new Int32Array(pairs.length);
  previous.fill(-1);

  for (let i = 0; i < pairs.length; i += 1) {
    const bIndex = pairs[i].b;
    let low = 0;
    let high = tails.length;
    while (low < high) {
      const mid = (low + high) >> 1;
      if (pairs[tails[mid]].b < bIndex) {
        low = mid + 1;
      } else {
        high = mid;
      }
    }
    if (low > 0) {
      previous[i] = tails[low - 1];
    }
    tails[low] = i;
  }

  const result: AnchorPair[] = [];
  let cursor = tails[tails.length - 1] ?? -1;
  while (cursor >= 0) {
    result.push(pairs[cursor]);
    cursor = previous[cursor];
  }
  result.reverse();
  return result;
}

function annotateRows(rows: BasicDiffRow[]): {
  rows: DiffRow[];
  additions: number;
  deletions: number;
} {
  let beforeLine = 0;
  let afterLine = 0;
  let additions = 0;
  let deletions = 0;

  const annotated = rows.map((row): DiffRow => {
    if (row.kind === 'remove') {
      beforeLine += 1;
      deletions += 1;
      return {
        ...row,
        beforeLine,
        afterLine: null,
      };
    }
    if (row.kind === 'add') {
      afterLine += 1;
      additions += 1;
      return {
        ...row,
        beforeLine: null,
        afterLine,
      };
    }
    beforeLine += 1;
    afterLine += 1;
    return {
      ...row,
      beforeLine,
      afterLine,
    };
  });

  return { rows: annotated, additions, deletions };
}
