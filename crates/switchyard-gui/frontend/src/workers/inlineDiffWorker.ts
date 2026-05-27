/// <reference lib="webworker" />

type InlineDiffKind = 'file' | 'hunk' | 'add' | 'remove' | 'meta' | 'context';

interface InlineDiffLine {
  kind: InlineDiffKind;
  text: string;
}

interface InlineDiffRequest {
  requestId: number;
  text: string;
  maxChars?: number;
}

interface InlineDiffDone {
  requestId: number;
  type: 'done';
  lines: InlineDiffLine[];
  originalCharCount: number;
  renderedCharCount: number;
  truncated: boolean;
}

interface InlineDiffError {
  requestId: number;
  type: 'error';
  error: string;
}

const DEFAULT_MAX_CHARS = 180_000;

const worker = self as DedicatedWorkerGlobalScope;

worker.onmessage = (event: MessageEvent<InlineDiffRequest>) => {
  const requestId = event.data?.requestId ?? 0;
  try {
    const rawText = String(event.data?.text ?? '');
    const maxChars = Math.max(8_000, event.data?.maxChars ?? DEFAULT_MAX_CHARS);
    const normalized = normalizeAndCompactText(rawText, maxChars);
    const lines = normalized.text.split('\n').map((line) => ({
      kind: classifyDiffLine(line),
      text: line || ' ',
    }));

    const result: InlineDiffDone = {
      requestId,
      type: 'done',
      lines,
      originalCharCount: rawText.length,
      renderedCharCount: normalized.text.length,
      truncated: normalized.truncated,
    };
    worker.postMessage(result);
  } catch (error) {
    const result: InlineDiffError = {
      requestId,
      type: 'error',
      error: error instanceof Error ? error.message : String(error),
    };
    worker.postMessage(result);
  }
};

function normalizeAndCompactText(text: string, maxChars: number): { text: string; truncated: boolean } {
  const normalized = text.replace(/\r\n/g, '\n').replace(/\r/g, '\n').trim();
  if (normalized.length <= maxChars) {
    return { text: normalized, truncated: false };
  }

  const headChars = Math.floor(maxChars * 0.7);
  const tailChars = Math.floor(maxChars * 0.22);
  const compacted = [
    normalized.slice(0, headChars),
    '',
    `… diff 内容过长，已省略中间 ${Math.max(0, normalized.length - headChars - tailChars).toLocaleString()} 个字符 …`,
    '',
    normalized.slice(-tailChars),
  ].join('\n');

  return { text: compacted, truncated: true };
}

function classifyDiffLine(line: string): InlineDiffKind {
  if (line.startsWith('diff --git') || line.startsWith('*** ')) return 'file';
  if (line.startsWith('@@')) return 'hunk';
  if (line.startsWith('+') && !line.startsWith('+++')) return 'add';
  if (line.startsWith('-') && !line.startsWith('---')) return 'remove';
  if (line.startsWith('+++') || line.startsWith('---') || line.startsWith('index ')) return 'meta';
  return 'context';
}

export {};
