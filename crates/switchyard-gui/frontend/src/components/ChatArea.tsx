import React, { useRef, useEffect, useState, useCallback, useLayoutEffect, useMemo } from 'react';
import { convertFileSrc } from '@tauri-apps/api/core';
import { getCurrentWebview } from '@tauri-apps/api/webview';
import { open as openDialog } from '@tauri-apps/plugin-dialog';
import { Send, RefreshCw, MessageSquare, Pencil, Check, Square, Plus, ChevronDown, X, FileText, Image as ImageIcon } from 'lucide-react';
import type { Session, Turn, SandboxMode, SendPayload, InputAttachment } from '../types';
import { completeSlash } from './slashCommands';
import { persistAttachmentFile, readImageAttachmentDataUrl, saveClipboardAttachment } from '../services/api';
import type { RuntimeTurnPhase, RenderTurnEventsOptions } from './ui/RenderHelpers';
import {
  attachmentFromPath,
  extractAttachmentsFromAttachmentReferences,
  filenameFromPath,
  mergeInputAttachments,
  stripAttachmentReferences,
} from '../utils/attachments';

interface ChatAreaProps {
  selectedSession: Session | null;
  isGenerating: boolean;
  turns: Turn[];
  turnAttachments?: Record<string, InputAttachment[]>;
  runtimeDispatchStartedAt?: number | null;
  runtimeDispatchPhase?: string | null;
  activeCoreRuntimePhase?: RuntimeTurnPhase;
  activePeerRuntimePhase?: RuntimeTurnPhase;
  handleSend: (payload: SendPayload, restoreText?: (text: string) => void) => void | Promise<void>;
  handleCancel: () => void;
  activeCoreText: string | null;
  activeCoreTurnId: string | null;
  activePeerName: string | null;
  activePeerTurnId: string | null;
  activePeerText: string | null;
  sessionEvents: any[];
  realtimeTerminalLines: Record<string, string[]>;
  hyardJobs?: Record<string, any>;
  renderMessageBody: (
    text: string | null,
    style?: React.CSSProperties,
    onOpenFile?: (path: string) => void,
    onProposeForCanvas?: (path: string, content: string) => void,
  ) => React.ReactNode;
  renderTurnEvents: (
    turnId: string,
    events: any[],
    turns: Turn[],
    realtimeLines?: string[],
    hyardJobs?: Record<string, any>,
    options?: RenderTurnEventsOptions,
  ) => React.ReactNode;
  renderTurnActivitySummary: (
    turnId: string,
    events: any[],
    turns: Turn[],
    realtimeLines?: string[],
    hyardJobs?: Record<string, any>,
    options?: RenderTurnEventsOptions,
  ) => React.ReactNode;
  queuedMessages: SendPayload[];
  onClearQueue: () => void;
  sandboxMode: SandboxMode;
  onSandboxModeChange: (mode: SandboxMode) => void | Promise<void>;
  /// Caller wipes canonical history at `turnId` (and everything later), then
  /// dispatches a fresh turn carrying `newText`. The Core live instance is
  /// terminated server-side as part of the rewind.
  onEditAndResend: (turnId: string, newText: string) => void;
  /// Same as edit, but re-sends the existing user_message unchanged.
  onRetryLastUserTurn: (turnId: string) => void;
  /// Opens a file in the right-side Canvas. Called when the user clicks
  /// an inline file-path code span inside a chat message (the RenderHelpers
  /// turn that on when the text looks like a path).
  onOpenFile: (path: string) => void;
  isHistoryLoading?: boolean;
  historyLoadingPhase?: string | null;
  historyLoadingError?: string | null;
  historyPartial?: boolean;
  historyHasMoreBefore?: boolean;
  onLoadOlderHistory?: () => void | Promise<void>;
  isLoadingOlderHistory?: boolean;
  /// Registers the composer's imperative API (used by the Explorer's
  /// "Add to Chat" to stage context files as chips).
  onComposerReady?: (api: ComposerApi) => void;
}

const SANDBOX_OPTIONS: Array<{
  mode: SandboxMode;
  label: string;
  description: string;
  accent: string;
  background: string;
  border: string;
}> = [
  {
    mode: 'danger-full-access',
    label: '完全访问权限',
    description: '不限制文件读写和命令沙箱，适合需要完整接管项目时使用。',
    accent: '#f59e0b',
    background: 'rgba(245, 158, 11, 0.16)',
    border: 'rgba(245, 158, 11, 0.42)',
  },
  {
    mode: 'workspace-write',
    label: '工作区写入',
    description: '允许读写当前工作区，默认适合编码、测试和重构。',
    accent: '#38bdf8',
    background: 'rgba(56, 189, 248, 0.14)',
    border: 'rgba(56, 189, 248, 0.36)',
  },
  {
    mode: 'read-only',
    label: '只读模式',
    description: '只允许读取和分析，不写入文件。',
    accent: '#a78bfa',
    background: 'rgba(167, 139, 250, 0.14)',
    border: 'rgba(167, 139, 250, 0.36)',
  },
];

const SESSION_ENTRY_AUTO_SCROLL_SETTLE_MS = 2_500;
const VIRTUAL_TURN_ESTIMATED_HEIGHT = 180;
const VIRTUAL_TURN_GAP = 14;
const VIRTUAL_TURN_OVERSCAN = 8;
const VIRTUAL_TURN_SCROLL_SEEK_OVERSCAN = 3;
const VIRTUAL_SCROLL_SEEK_IDLE_MS = 120;
const VIRTUAL_SCROLL_SEEK_MIN_DELTA_PX = 420;
const VIRTUAL_SCROLL_SEEK_VELOCITY_PX_PER_MS = 2.2;
const VIRTUAL_ROW_MEASURE_EPSILON_PX = 2;
const MAX_VIRTUAL_ROW_HEIGHT_SESSIONS = 8;
const MAX_SESSION_DERIVED_CACHE_ENTRIES = 8;
const USER_SCROLL_INTERACTION_IDLE_MS = 260;
const SCROLL_BOTTOM_PIN_THRESHOLD_PX = 128;
const EMPTY_EVENT_LIST: any[] = [];
const EMPTY_TURN_LIST: Turn[] = [];

function scrollClockNow(): number {
  return typeof performance !== 'undefined' ? performance.now() : Date.now();
}

function virtualSessionKey(sessionId: string | null): string {
  return sessionId ?? 'new-session';
}

function virtualRowSessionKey(rowKey: string): string {
  const separatorIndex = rowKey.indexOf(':');
  return separatorIndex >= 0 ? rowKey.slice(0, separatorIndex) : rowKey;
}

function touchSessionCacheEntry<T>(cache: Map<string, T>, sessionKey: string, value: T): void {
  if (cache.has(sessionKey)) {
    cache.delete(sessionKey);
  }
  cache.set(sessionKey, value);
  while (cache.size > MAX_SESSION_DERIVED_CACHE_ENTRIES) {
    const oldestKey = cache.keys().next().value;
    if (oldestKey === undefined) break;
    cache.delete(oldestKey);
  }
}

function sandboxOptionFor(mode: SandboxMode) {
  return SANDBOX_OPTIONS.find((option) => option.mode === mode) ?? SANDBOX_OPTIONS[1];
}

function formatChatRuntimeElapsed(ms?: number): string {
  if (typeof ms !== 'number' || !Number.isFinite(ms) || ms < 0) return '已处理 0s';
  if (ms < 60_000) return `已处理 ${(Math.max(0, ms) / 1000).toFixed(1)}s`;
  const totalSeconds = Math.max(0, Math.floor(ms / 1000));
  const minutes = Math.floor(totalSeconds / 60);
  const seconds = totalSeconds % 60;
  if (minutes < 60) return `已处理 ${minutes}m ${String(seconds).padStart(2, '0')}s`;
  const hours = Math.floor(minutes / 60);
  const remainingMinutes = minutes % 60;
  return `已处理 ${hours}h ${String(remainingMinutes).padStart(2, '0')}m`;
}

function useChatLiveNow(active: boolean, intervalMs = 250): number {
  const [now, setNow] = useState(() => Date.now());

  useEffect(() => {
    if (!active) return;
    const tick = () => setNow(Date.now());
    tick();
    const timer = window.setInterval(tick, intervalMs);
    return () => window.clearInterval(timer);
  }, [active, intervalMs]);

  return now;
}

function ChatLiveElapsedLabel({ startedAt }: { startedAt?: number | null }) {
  const active = typeof startedAt === 'number' && Number.isFinite(startedAt);
  const now = useChatLiveNow(active);
  const elapsedMs = active ? Math.max(0, now - startedAt!) : undefined;
  return <>{formatChatRuntimeElapsed(elapsedMs)}</>;
}

function isRuntimePhaseActive(phase?: RuntimeTurnPhase): boolean {
  return phase === 'running' || phase === 'output_completed' || phase === 'finalizing';
}

function readFileAsDataUrl(file: File): Promise<string> {
  return new Promise((resolve, reject) => {
    const reader = new FileReader();
    reader.onerror = () => reject(reader.error ?? new Error('failed to read file'));
    reader.onload = () => {
      if (typeof reader.result === 'string') {
        resolve(reader.result);
      } else {
        reject(new Error('clipboard item did not produce a data URL'));
      }
    };
    reader.readAsDataURL(file);
  });
}

function nativePathForFile(file: File): string | null {
  const path = (file as File & { path?: string }).path;
  return typeof path === 'string' && path.trim() ? path.trim() : null;
}

function looksLikeEphemeralAttachmentPath(path: string): boolean {
  const normalized = path.replace(/\\/g, '/').toLowerCase();
  return /(^|\/)(\.cache|cache|temp|tmp)(\/|$)/.test(normalized)
    || normalized.includes('/appdata/local/temp/')
    || normalized.includes('/appdata/local/microsoft/windows/inetcache/');
}

function attachmentNameHintForFile(file: File, nativePath?: string | null): string | undefined {
  const name = file.name?.trim();
  if (name) return name;
  if (nativePath?.trim()) return filenameFromPath(nativePath);
  return undefined;
}

function mimeTypeHintForFile(file: File, nativePath?: string | null): string | undefined {
  const mimeType = file.type?.trim();
  if (mimeType) return mimeType;
  if (nativePath?.trim()) return attachmentFromPath(nativePath).mimeType ?? undefined;
  return undefined;
}

interface RenderedMessageBodyProps {
  text: string | null;
  renderMessageBody: ChatAreaProps['renderMessageBody'];
  onOpenFile: (path: string) => void;
}

/// Cache parsed/formatted message bodies. Historical chat turns do not need to
/// re-run markdown-ish splitting, file-reference detection, and React node
/// creation on every streaming chunk or layout change.
const RenderedMessageBody: React.FC<RenderedMessageBodyProps> = React.memo(({
  text,
  renderMessageBody,
  onOpenFile,
}) => {
  return <>{renderMessageBody(text, undefined, onOpenFile)}</>;
}, (prev, next) => (
  prev.text === next.text &&
  prev.onOpenFile === next.onOpenFile
));

const IMAGE_PREVIEW_CACHE_LIMIT = 96;
const imagePreviewSrcCache = new Map<string, string>();
const imagePreviewErrorCache = new Map<string, string>();
const imagePreviewPendingCache = new Map<string, Promise<string>>();

interface ImagePreviewLoadState {
  src: string | null;
  error: string | null;
  loading: boolean;
  retry: () => void;
}

function imagePreviewCacheKey(path: string, mimeType?: string | null): string {
  return `${path}\u0000${mimeType ?? ''}`;
}

function setLimitedImagePreviewCacheValue<T>(cache: Map<string, T>, key: string, value: T): T {
  cache.set(key, value);
  if (cache.size > IMAGE_PREVIEW_CACHE_LIMIT) {
    const oldestKey = cache.keys().next().value;
    if (oldestKey !== undefined) {
      cache.delete(oldestKey);
    }
  }
  return value;
}

function clearImagePreviewCacheValue(key: string) {
  imagePreviewSrcCache.delete(key);
  imagePreviewErrorCache.delete(key);
  imagePreviewPendingCache.delete(key);
}

function normalizeImagePreviewError(error: unknown): string {
  if (error instanceof Error && error.message.trim()) return error.message.trim();
  const text = String(error ?? '').trim();
  return text || '图片加载失败';
}

function shouldFallbackToTauriAssetPreview(error: unknown): boolean {
  const message = normalizeImagePreviewError(error).toLowerCase();
  return message.includes('command read_image_attachment_data_url not found')
    || message.includes('read_image_attachment_data_url')
    || message.includes('not found')
    || message.includes('not allowed')
    || message.includes('forbidden');
}

function tauriAssetPreviewSrc(path: string): string | null {
  try {
    const src = convertFileSrc(path);
    return typeof src === 'string' && src.trim() ? src : null;
  } catch (error) {
    console.warn('Failed to convert image attachment path via Tauri asset protocol', error);
    return null;
  }
}

function truncatePreviewText(text: string, maxLength = 96): string {
  if (text.length <= maxLength) return text;
  return `${text.slice(0, Math.max(0, maxLength - 1))}…`;
}

function loadImageAttachmentDataUrl(path: string, mimeType?: string | null): Promise<string> {
  const key = imagePreviewCacheKey(path, mimeType);
  const cachedSrc = imagePreviewSrcCache.get(key);
  if (cachedSrc) return Promise.resolve(cachedSrc);
  const cachedError = imagePreviewErrorCache.get(key);
  if (cachedError) return Promise.reject(new Error(cachedError));
  const pending = imagePreviewPendingCache.get(key);
  if (pending) return pending;

  const request = readImageAttachmentDataUrl(path, mimeType)
    .then((src) => {
      imagePreviewErrorCache.delete(key);
      return setLimitedImagePreviewCacheValue(imagePreviewSrcCache, key, src);
    })
    .catch((error) => {
      if (shouldFallbackToTauriAssetPreview(error)) {
        const fallbackSrc = tauriAssetPreviewSrc(path);
        if (fallbackSrc) {
          imagePreviewErrorCache.delete(key);
          return setLimitedImagePreviewCacheValue(imagePreviewSrcCache, key, fallbackSrc);
        }
      }
      const message = normalizeImagePreviewError(error);
      setLimitedImagePreviewCacheValue(imagePreviewErrorCache, key, message);
      throw new Error(message);
    })
    .finally(() => {
      imagePreviewPendingCache.delete(key);
    });
  imagePreviewPendingCache.set(key, request);
  return request;
}

function useImageAttachmentDataUrl(attachment: InputAttachment): ImagePreviewLoadState {
  const path = attachment.path;
  const mimeType = attachment.mimeType ?? null;
  const key = imagePreviewCacheKey(path, mimeType);
  const [reloadToken, setReloadToken] = useState(0);
  const [state, setState] = useState<ImagePreviewLoadState>(() => {
    const src = imagePreviewSrcCache.get(key) ?? null;
    const error = imagePreviewErrorCache.get(key) ?? null;
    return { src, error, loading: !src && !error, retry: () => {} };
  });

  const retry = useCallback(() => {
    clearImagePreviewCacheValue(key);
    setReloadToken((value) => value + 1);
  }, [key]);

  useEffect(() => {
    const src = imagePreviewSrcCache.get(key) ?? null;
    const error = imagePreviewErrorCache.get(key) ?? null;
    if (src || error) {
      setState({ src, error, loading: false, retry });
      return;
    }

    let cancelled = false;
    setState({ src: null, error: null, loading: true, retry });
    loadImageAttachmentDataUrl(path, mimeType)
      .then((loadedSrc) => {
        if (!cancelled) {
          setState({ src: loadedSrc, error: null, loading: false, retry });
        }
      })
      .catch((loadError) => {
        if (!cancelled) {
          setState({ src: null, error: normalizeImagePreviewError(loadError), loading: false, retry });
        }
      });

    return () => {
      cancelled = true;
    };
  }, [key, path, mimeType, retry, reloadToken]);

  return state;
}

const AttachmentImageThumb: React.FC<{
  attachment: InputAttachment;
  onOpenImage: (attachment: InputAttachment) => void;
}> = React.memo(({ attachment, onOpenImage }) => {
  const { src, error, loading } = useImageAttachmentDataUrl(attachment);
  const [decodeError, setDecodeError] = useState<string | null>(null);
  const label = attachment.name || filenameFromPath(attachment.path);
  const cacheKey = imagePreviewCacheKey(attachment.path, attachment.mimeType ?? null);

  useEffect(() => {
    setDecodeError(null);
  }, [cacheKey, src]);

  const displayError = error || decodeError;
  const title = `${label}\n${attachment.path}${displayError ? `\n\n图片加载失败：${displayError}` : ''}`;

  return (
    <button
      key={attachment.path}
      type="button"
      onClick={() => onOpenImage(attachment)}
      title={title}
      style={{
        width: 104,
        height: 78,
        border: `1px solid ${displayError ? 'rgba(248, 113, 113, 0.42)' : 'rgba(148, 163, 184, 0.28)'}`,
        borderRadius: 8,
        padding: 0,
        overflow: 'hidden',
        background: displayError ? 'rgba(127, 29, 29, 0.28)' : 'rgba(15, 23, 42, 0.55)',
        cursor: 'zoom-in',
        position: 'relative',
      }}
    >
      {src && !displayError ? (
        <img
          src={src}
          alt={label}
          onError={() => {
            const message = '浏览器无法解码该图片格式';
            setLimitedImagePreviewCacheValue(imagePreviewErrorCache, cacheKey, message);
            setDecodeError(message);
          }}
          style={{
            width: '100%',
            height: '100%',
            objectFit: 'cover',
            display: 'block',
          }}
        />
      ) : (
        <div
          style={{
            width: '100%',
            height: '100%',
            boxSizing: 'border-box',
            padding: '6px 7px',
            display: 'flex',
            flexDirection: 'column',
            alignItems: 'center',
            justifyContent: 'center',
            gap: 3,
            color: displayError ? '#fecaca' : 'var(--text-muted)',
            textAlign: 'center',
          }}
        >
          <ImageIcon size={17} style={{ opacity: 0.9 }} />
          <div style={{ fontSize: 11, fontWeight: 700, lineHeight: 1.15 }}>
            {displayError ? '加载失败' : loading ? '加载中…' : '无预览'}
          </div>
          <div
            style={{
              width: '100%',
              overflow: 'hidden',
              textOverflow: 'ellipsis',
              whiteSpace: 'nowrap',
              fontSize: 10,
              lineHeight: 1.15,
              color: displayError ? 'rgba(254, 202, 202, 0.78)' : 'var(--text-muted)',
            }}
          >
            {label}
          </div>
        </div>
      )}
    </button>
  );
});

const ImageAttachmentModalBody: React.FC<{ attachment: InputAttachment }> = React.memo(({ attachment }) => {
  const { src, error, loading, retry } = useImageAttachmentDataUrl(attachment);
  const [decodeError, setDecodeError] = useState<string | null>(null);
  const label = attachment.name || filenameFromPath(attachment.path);
  const cacheKey = imagePreviewCacheKey(attachment.path, attachment.mimeType ?? null);

  useEffect(() => {
    setDecodeError(null);
  }, [cacheKey, src]);

  const displayError = error || decodeError;
  if (src && !displayError) {
    return (
      <img
        src={src}
        alt={label}
        onError={() => {
          const message = '浏览器无法解码该图片格式';
          setLimitedImagePreviewCacheValue(imagePreviewErrorCache, cacheKey, message);
          setDecodeError(message);
        }}
        style={{
          maxWidth: 'min(86vw, 1100px)',
          maxHeight: '78vh',
          objectFit: 'contain',
          borderRadius: 8,
          background: '#020617',
        }}
      />
    );
  }

  return (
    <div
      style={{
        width: 'min(86vw, 760px)',
        minHeight: 240,
        borderRadius: 8,
        border: `1px solid ${displayError ? 'rgba(248, 113, 113, 0.36)' : 'rgba(148, 163, 184, 0.22)'}`,
        background: displayError ? 'rgba(127, 29, 29, 0.18)' : '#020617',
        color: displayError ? '#fecaca' : 'var(--text-muted)',
        display: 'flex',
        flexDirection: 'column',
        alignItems: 'center',
        justifyContent: 'center',
        gap: 8,
        padding: 24,
        textAlign: 'center',
      }}
    >
      <ImageIcon size={28} />
      <div style={{ color: displayError ? '#fecaca' : 'var(--text-secondary)', fontSize: 13, fontWeight: 800 }}>
        {displayError ? '图片加载失败' : loading ? '图片加载中…' : '无可用预览'}
      </div>
      <div style={{ maxWidth: '100%', color: 'var(--text-muted)', fontSize: 12, overflowWrap: 'anywhere' }}>
        {label}
      </div>
      {displayError && (
        <div style={{ maxWidth: '100%', color: 'rgba(254, 202, 202, 0.82)', fontSize: 11, overflowWrap: 'anywhere' }}>
          {truncatePreviewText(displayError, 220)}
        </div>
      )}
      {displayError && (
        <button
          type="button"
          onClick={() => {
            setDecodeError(null);
            retry();
          }}
          style={{
            marginTop: 4,
            border: '1px solid rgba(147, 197, 253, 0.38)',
            borderRadius: 999,
            padding: '6px 12px',
            background: 'rgba(37, 99, 235, 0.16)',
            color: '#bfdbfe',
            fontSize: 12,
            fontWeight: 800,
            cursor: 'pointer',
          }}
        >
          重新加载图片
        </button>
      )}
    </div>
  );
});

interface UserAttachmentPreviewGridProps {
  attachments: InputAttachment[];
  onOpenImage: (attachment: InputAttachment) => void;
  onOpenFile: (path: string) => void;
}

const UserAttachmentPreviewGrid: React.FC<UserAttachmentPreviewGridProps> = React.memo(({
  attachments,
  onOpenImage,
  onOpenFile,
}) => {
  if (attachments.length === 0) return null;
  return (
    <div
      style={{
        display: 'flex',
        flexWrap: 'wrap',
        gap: 8,
        marginTop: 8,
      }}
    >
      {attachments.map((attachment) => {
        if (attachment.kind === 'image') {
          return <AttachmentImageThumb key={attachment.path} attachment={attachment} onOpenImage={onOpenImage} />;
        }
        return (
          <button
            key={attachment.path}
            type="button"
            onClick={() => onOpenFile(attachment.path)}
            title={`Open ${attachment.path}`}
            style={{
              display: 'inline-flex',
              alignItems: 'center',
              gap: 6,
              maxWidth: 240,
              border: '1px solid rgba(148, 163, 184, 0.24)',
              borderRadius: 999,
              background: 'rgba(15, 23, 42, 0.45)',
              color: 'var(--text-secondary)',
              padding: '5px 9px',
              fontSize: 12,
              cursor: 'pointer',
            }}
          >
            <FileText size={13} style={{ color: 'var(--color-primary)', flex: '0 0 auto' }} />
            <span style={{ overflow: 'hidden', textOverflow: 'ellipsis', whiteSpace: 'nowrap' }}>
              {attachment.name || filenameFromPath(attachment.path)}
            </span>
          </button>
        );
      })}
    </div>
  );
});

const CHAT_RENDER_CACHE_LIMIT = 600;
const strippedAttachmentTextCache = new Map<string, string>();
const attachmentReferenceCache = new Map<string, InputAttachment[]>();
const systemFeedbackResultsCache = new Map<string, any[] | null>();

function setLimitedCacheValue<T>(cache: Map<string, T>, key: string, value: T): T {
  cache.set(key, value);
  if (cache.size > CHAT_RENDER_CACHE_LIMIT) {
    const oldestKey = cache.keys().next().value;
    if (oldestKey !== undefined) {
      cache.delete(oldestKey);
    }
  }
  return value;
}

function cachedStripAttachmentReferences(text: string): string {
  const cached = strippedAttachmentTextCache.get(text);
  if (cached !== undefined) return cached;
  return setLimitedCacheValue(strippedAttachmentTextCache, text, stripAttachmentReferences(text));
}

function cachedExtractAttachmentReferences(text: string): InputAttachment[] {
  const cached = attachmentReferenceCache.get(text);
  if (cached !== undefined) return cached;
  return setLimitedCacheValue(
    attachmentReferenceCache,
    text,
    extractAttachmentsFromAttachmentReferences(text),
  );
}

function cachedParseSystemFeedbackResults(text: string): any[] | null {
  if (!text.includes('<<<SWITCHYARD_JSON_BEGIN>>>')) return null;
  if (systemFeedbackResultsCache.has(text)) {
    return systemFeedbackResultsCache.get(text) ?? null;
  }
  let parsedResults: any[] | null = null;
  try {
    const match = text.match(/<<<SWITCHYARD_JSON_BEGIN>>>([\s\S]*?)<<<SWITCHYARD_JSON_END>>>/);
    if (match?.[1]) {
      const parsed = JSON.parse(match[1]);
      if (Array.isArray(parsed?.results)) {
        parsedResults = parsed.results;
      }
    }
  } catch {
    parsedResults = null;
  }
  return setLimitedCacheValue(systemFeedbackResultsCache, text, parsedResults);
}

const SystemFeedbackResults: React.FC<{ results: any[] | null }> = React.memo(({ results }) => {
  if (!results || results.length === 0) return null;
  return (
    <div
      className="message-bubble message-system"
      style={{
        alignSelf: 'center',
        width: '100%',
        background: 'rgba(255, 255, 255, 0.02)',
        border: '1px dashed var(--border-muted)',
        borderRadius: '4px',
        padding: '12px',
        fontSize: '13px',
        color: 'var(--text-secondary)',
        marginBottom: '16px'
      }}
    >
      <div style={{ fontWeight: 'bold', display: 'flex', alignItems: 'center', gap: '6px', color: 'var(--color-primary)', marginBottom: '8px' }}>
        <span>Aggregated Delegation Results</span>
        <span style={{ fontSize: '11px', padding: '2px 6px', background: 'rgba(59, 130, 246, 0.1)', color: 'var(--color-primary)', borderRadius: '3px' }}>
          System Feedback
        </span>
      </div>
      <div style={{ display: 'flex', flexDirection: 'column', gap: '8px' }}>
        {results.map((res: any, rIdx: number) => (
          <div key={res.id || rIdx} style={{ display: 'flex', justifyContent: 'space-between', padding: '6px 12px', background: 'rgba(0, 0, 0, 0.2)', borderRadius: '4px', borderLeft: `3px solid ${res.status === 'success' ? '#10b981' : '#ef4444'}` }}>
            <div>
              <span style={{ fontWeight: 'bold', color: 'var(--text-primary)' }}>{res.id}</span>
              <span style={{ marginLeft: '8px', color: 'var(--text-muted)' }}>({res.provider})</span>
            </div>
            <div style={{ display: 'flex', gap: '12px', fontSize: '11px' }}>
              <span>Status: <span style={{ color: res.status === 'success' ? '#10b981' : '#ef4444' }}>{res.status}</span></span>
              <span>Duration: {res.duration_ms}ms</span>
            </div>
          </div>
        ))}
      </div>
    </div>
  );
});

interface VirtualTurnEntry {
  key: string;
  turn: Turn;
  originalIndex: number;
}

interface VirtualTurnMetric extends VirtualTurnEntry {
  start: number;
  size: number;
}

interface GroupedSessionEvents {
  eventsByTurnId: Map<string, any[]>;
  eventActivityTurnIds: Set<string>;
}

interface CachedGroupedSessionEvents extends GroupedSessionEvents {
  sourceEvents: any[];
}

interface TurnIndexes {
  lastUserTurnId: string | null;
  delegateTurnsByParent: Map<string, Turn[]>;
  relatedTurnsByTurnId: Map<string, Turn[]>;
}

interface CachedTurnIndexes extends TurnIndexes {
  sourceTurns: Turn[];
}

interface CachedHyardJobIndex {
  sourceJobs?: ChatAreaProps['hyardJobs'];
  jobsByTurnId: ReturnType<typeof indexHyardJobsByTurnId>;
}

function shallowArrayEqual<T>(left: readonly T[] | undefined, right: readonly T[]): boolean {
  if (!left || left.length !== right.length) return false;
  for (let i = 0; i < right.length; i += 1) {
    if (left[i] !== right[i]) return false;
  }
  return true;
}

function setShallowEqual<T>(left: ReadonlySet<T>, right: ReadonlySet<T>): boolean {
  if (left.size !== right.size) return false;
  for (const value of right) {
    if (!left.has(value)) return false;
  }
  return true;
}

function buildTurnIndexes(turns: Turn[]): TurnIndexes {
  const delegateTurnsByParent = new Map<string, Turn[]>();
  for (const turn of turns) {
    const parentId = turn.delegated_by;
    if (!parentId) continue;
    const existing = delegateTurnsByParent.get(parentId);
    if (existing) {
      existing.push(turn);
    } else {
      delegateTurnsByParent.set(parentId, [turn]);
    }
  }

  const relatedTurnsByTurnId = new Map<string, Turn[]>();
  for (const turn of turns) {
    if (!turn.turn_id) continue;
    const delegates = delegateTurnsByParent.get(turn.turn_id);
    relatedTurnsByTurnId.set(turn.turn_id, delegates?.length ? [turn, ...delegates] : [turn]);
  }

  let lastUserTurnId: string | null = null;
  for (let i = turns.length - 1; i >= 0; i -= 1) {
    const turn = turns[i];
    if (turn.origin === 'user' && !turn.user_message.includes('<<<SWITCHYARD_JSON_BEGIN>>>')) {
      lastUserTurnId = turn.turn_id || null;
      break;
    }
  }

  return {
    lastUserTurnId,
    delegateTurnsByParent,
    relatedTurnsByTurnId,
  };
}

function eventTurnId(event: any): string {
  return typeof event?.turn_id === 'string' ? event.turn_id : '';
}

function buildGroupedSessionEvents(
  sessionEvents: any[],
  previous?: CachedGroupedSessionEvents | null,
): GroupedSessionEvents {
  const grouped = new Map<string, any[]>();
  const activityIds = new Set<string>();
  for (const event of sessionEvents) {
    const turnId = eventTurnId(event);
    if (!turnId) continue;
    const existing = grouped.get(turnId);
    if (existing) {
      existing.push(event);
    } else {
      grouped.set(turnId, [event]);
    }
    if (eventHasRenderableArtifact(event)) {
      activityIds.add(turnId);
    }
  }

  if (!previous) {
    return { eventsByTurnId: grouped, eventActivityTurnIds: activityIds };
  }

  const stableGrouped = new Map<string, any[]>();
  for (const [turnId, events] of grouped) {
    const previousEvents = previous.eventsByTurnId.get(turnId);
    const stableEvents = previousEvents && shallowArrayEqual(previousEvents, events)
      ? previousEvents
      : events;
    stableGrouped.set(turnId, stableEvents);
  }

  return {
    eventsByTurnId: stableGrouped,
    eventActivityTurnIds: setShallowEqual(previous.eventActivityTurnIds, activityIds)
      ? previous.eventActivityTurnIds
      : activityIds,
  };
}

function cloneEventActivityIds(
  previous: CachedGroupedSessionEvents,
  turnId: string,
  nextEventsForTurn: any[],
): Set<string> {
  const shouldHaveActivity = nextEventsForTurn.some(eventHasRenderableArtifact);
  const alreadyHasActivity = previous.eventActivityTurnIds.has(turnId);
  if (shouldHaveActivity === alreadyHasActivity) {
    return previous.eventActivityTurnIds;
  }
  const nextActivityIds = new Set(previous.eventActivityTurnIds);
  if (shouldHaveActivity) {
    nextActivityIds.add(turnId);
  } else {
    nextActivityIds.delete(turnId);
  }
  return nextActivityIds;
}

function appendGroupedSessionEvent(
  previous: CachedGroupedSessionEvents,
  nextEvent: any,
): GroupedSessionEvents | null {
  const turnId = eventTurnId(nextEvent);
  if (!turnId) return null;
  const previousEventsForTurn = previous.eventsByTurnId.get(turnId) ?? EMPTY_EVENT_LIST;
  const nextEventsForTurn = [...previousEventsForTurn, nextEvent];
  const nextGrouped = new Map(previous.eventsByTurnId);
  nextGrouped.set(turnId, nextEventsForTurn);
  const nextActivityIds = eventHasRenderableArtifact(nextEvent) && !previous.eventActivityTurnIds.has(turnId)
    ? new Set(previous.eventActivityTurnIds).add(turnId)
    : previous.eventActivityTurnIds;
  return { eventsByTurnId: nextGrouped, eventActivityTurnIds: nextActivityIds };
}

function replaceGroupedSessionEvent(
  previous: CachedGroupedSessionEvents,
  oldEvent: any,
  nextEvent: any,
): GroupedSessionEvents | null {
  const oldTurnId = eventTurnId(oldEvent);
  const nextTurnId = eventTurnId(nextEvent);
  if (!oldTurnId || oldTurnId !== nextTurnId) return null;
  const previousEventsForTurn = previous.eventsByTurnId.get(oldTurnId);
  if (!previousEventsForTurn) return null;
  const eventIndex = previousEventsForTurn.indexOf(oldEvent);
  if (eventIndex < 0) return null;

  const nextEventsForTurn = previousEventsForTurn.slice();
  nextEventsForTurn[eventIndex] = nextEvent;
  const nextGrouped = new Map(previous.eventsByTurnId);
  nextGrouped.set(oldTurnId, nextEventsForTurn);
  return {
    eventsByTurnId: nextGrouped,
    eventActivityTurnIds: cloneEventActivityIds(previous, oldTurnId, nextEventsForTurn),
  };
}

function deriveGroupedSessionEvents(
  sessionEvents: any[],
  previous?: CachedGroupedSessionEvents | null,
): GroupedSessionEvents {
  if (previous?.sourceEvents === sessionEvents) {
    return previous;
  }

  if (previous) {
    const previousEvents = previous.sourceEvents;
    const previousLength = previousEvents.length;
    const nextLength = sessionEvents.length;

    // The live event stream is overwhelmingly append-only. Keep the per-turn
    // arrays stable and update only the touched turn so historical rows do not
    // re-render on every terminal/reasoning tick.
    if (
      nextLength === previousLength + 1 &&
      (previousLength === 0 || sessionEvents[previousLength - 1] === previousEvents[previousLength - 1])
    ) {
      const appended = appendGroupedSessionEvent(previous, sessionEvents[nextLength - 1]);
      if (appended) return appended;
    }

    // Runtime item updates usually replace the active tail event in-place.
    // Handle that path without rebuilding every historical turn group.
    if (
      nextLength === previousLength &&
      nextLength > 0 &&
      sessionEvents[nextLength - 1] !== previousEvents[nextLength - 1] &&
      (nextLength === 1 || sessionEvents[nextLength - 2] === previousEvents[nextLength - 2])
    ) {
      const replaced = replaceGroupedSessionEvent(
        previous,
        previousEvents[nextLength - 1],
        sessionEvents[nextLength - 1],
      );
      if (replaced) return replaced;
    }
  }

  return buildGroupedSessionEvents(sessionEvents, previous);
}

interface VirtualTurnRowProps {
  itemKey: string;
  top: number;
  onMeasure: (key: string, height: number) => void;
  shouldMeasure?: boolean;
  children: React.ReactNode;
}

/// One history turn can contain several actual message cards (user prompt,
/// assistant answer, tool summary). The virtualizer treats that whole block as
/// a single variable-height row and observes its real height after markdown,
/// images, diff expand/collapse, and tool cards settle.
const VirtualTurnRow: React.FC<VirtualTurnRowProps> = React.memo(({
  itemKey,
  top,
  onMeasure,
  shouldMeasure = true,
  children,
}) => {
  const rowRef = useRef<HTMLDivElement>(null);
  const measureRafRef = useRef<number | null>(null);
  const pendingMeasureHeightRef = useRef<number | null>(null);

  useLayoutEffect(() => {
    if (!shouldMeasure) return;
    const node = rowRef.current;
    if (!node) return;
    let disposed = false;
    const report = () => {
      if (disposed) return;
      const pendingHeight = pendingMeasureHeightRef.current;
      pendingMeasureHeightRef.current = null;
      onMeasure(itemKey, pendingHeight ?? node.getBoundingClientRect().height);
    };
    const scheduleReport = (height?: number) => {
      if (typeof height === 'number' && Number.isFinite(height)) {
        pendingMeasureHeightRef.current = height;
      }
      if (measureRafRef.current !== null) return;
      measureRafRef.current = requestAnimationFrame(() => {
        measureRafRef.current = null;
        report();
      });
    };
    report();
    if (typeof ResizeObserver === 'undefined') {
      return () => {
        disposed = true;
        if (measureRafRef.current !== null) {
          cancelAnimationFrame(measureRafRef.current);
          measureRafRef.current = null;
        }
      };
    }
    const observer = new ResizeObserver((entries) => {
      const entry = entries[0];
      const borderBox = entry?.borderBoxSize;
      const borderBoxSize = Array.isArray(borderBox) ? borderBox[0] : borderBox;
      const observedHeight = borderBoxSize?.blockSize ?? entry?.contentRect.height;
      if (typeof observedHeight === 'number' && Number.isFinite(observedHeight)) {
        scheduleReport(observedHeight);
        return;
      }
      scheduleReport();
    });
    observer.observe(node);
    return () => {
      disposed = true;
      observer.disconnect();
      if (measureRafRef.current !== null) {
        cancelAnimationFrame(measureRafRef.current);
        measureRafRef.current = null;
      }
    };
  }, [itemKey, onMeasure, shouldMeasure]);

  return (
    <div
      ref={rowRef}
      data-virtual-turn-row={itemKey}
      style={{
        position: 'absolute',
        left: 0,
        right: 0,
        top: 0,
        overflowAnchor: 'none',
        transform: `translateY(${top}px)`,
        display: 'flex',
        flexDirection: 'column',
        gap: VIRTUAL_TURN_GAP,
      }}
    >
      {children}
    </div>
  );
});

const VirtualTurnPlaceholder: React.FC<{ height: number }> = React.memo(({ height }) => {
  const blockHeight = Math.max(56, Math.ceil(height));
  return (
    <div
      aria-hidden="true"
      style={{
        height: blockHeight,
        boxSizing: 'border-box',
        borderRadius: 10,
        border: '1px solid rgba(148, 163, 184, 0.07)',
        background: 'rgba(148, 163, 184, 0.035)',
        padding: 12,
        overflow: 'hidden',
        opacity: 0.72,
      }}
    >
      <div
        style={{
          width: '22%',
          height: 10,
          borderRadius: 999,
          background: 'rgba(148, 163, 184, 0.12)',
          marginBottom: 12,
        }}
      />
      <div
        style={{
          width: '78%',
          height: 8,
          borderRadius: 999,
          background: 'rgba(148, 163, 184, 0.09)',
          marginBottom: 8,
        }}
      />
      <div
        style={{
          width: '58%',
          height: 8,
          borderRadius: 999,
          background: 'rgba(148, 163, 184, 0.07)',
        }}
      />
    </div>
  );
});

interface HistoricalTurnContentProps {
  turn: Turn;
  originalIndex: number;
  lastUserTurnId: string | null;
  editingTurnId: string | null;
  editingDraft: string;
  isGenerating: boolean;
  storedAttachments?: InputAttachment[];
  hasAssistantActivity: boolean;
  turnEvents: any[];
  relatedTurns: Turn[];
  realtimeLines?: string[];
  hyardJob?: any;
  renderMessageBody: ChatAreaProps['renderMessageBody'];
  renderTurnEvents: ChatAreaProps['renderTurnEvents'];
  scopedRenderOptions: RenderTurnEventsOptions;
  onOpenFile: (path: string) => void;
  onPreviewAttachment: (attachment: InputAttachment) => void;
  onBeginEdit: (turn: Turn) => void;
  onRetryLastUserTurn: (turnId: string) => void;
  onEditingDraftChange: (value: string) => void;
  onCommitEdit: () => void;
  onCancelEdit: () => void;
}

const HistoricalTurnContentBase: React.FC<HistoricalTurnContentProps> = ({
  turn: t,
  originalIndex: idx,
  lastUserTurnId,
  editingTurnId,
  editingDraft,
  isGenerating,
  storedAttachments,
  hasAssistantActivity,
  turnEvents,
  relatedTurns,
  realtimeLines,
  hyardJob,
  renderMessageBody,
  renderTurnEvents,
  scopedRenderOptions,
  onOpenFile,
  onPreviewAttachment,
  onBeginEdit,
  onRetryLastUserTurn,
  onEditingDraftChange,
  onCommitEdit,
  onCancelEdit,
}) => {
  const isSystemFeedback = t.user_message.includes('<<<SWITCHYARD_JSON_BEGIN>>>');
  const assistantContent = t.provider_response || t.error_message;
  const scopedHyardJobs = t.turn_id && hyardJob ? { [t.turn_id]: hyardJob } : undefined;

  if (t.origin === 'user') {
    if (isSystemFeedback) {
      const parsedResults = cachedParseSystemFeedbackResults(t.user_message);
      return (
        <React.Fragment key={t.turn_id || idx}>
          <SystemFeedbackResults results={parsedResults} />
          {(assistantContent || hasAssistantActivity) && (
            <div className="message-assistant-flow">
              <div className="message-header">{t.provider} ({t.role})</div>
              {assistantContent ? (
                <RenderedMessageBody
                  text={assistantContent}
                  renderMessageBody={renderMessageBody}
                  onOpenFile={onOpenFile}
                />
              ) : null}
              {renderTurnEvents(t.turn_id, turnEvents, relatedTurns, realtimeLines, scopedHyardJobs, scopedRenderOptions)}
            </div>
          )}
        </React.Fragment>
      );
    }

    const isLastUser = Boolean(t.turn_id && t.turn_id === lastUserTurnId);
    const isEditing = editingTurnId === t.turn_id;
    const showActions = isLastUser && !isGenerating && !isEditing && Boolean(t.turn_id);
    const stamp = t.started_at
      ? new Date(t.started_at).toLocaleTimeString(undefined, {
          hour: '2-digit',
          minute: '2-digit',
        })
      : '';
    const visibleUserMessage = cachedStripAttachmentReferences(t.user_message);
    const attachmentsForTurn = mergeInputAttachments(
      storedAttachments,
      cachedExtractAttachmentReferences(t.user_message),
    );
    return (
      <React.Fragment key={t.turn_id || idx}>
        <div className="message-bubble message-user">
          <div
            className="message-header"
            style={{ display: 'flex', alignItems: 'center', gap: '6px' }}
          >
            <span>{isEditing ? 'You (editing)' : 'You'}</span>
            {/* Hover-reveal meta strip — timestamp + Edit /
                Retry. Actions only render on the latest user
                turn; the timestamp always renders so hovering
                any past message surfaces when it was sent. */}
            <span className="message-meta" style={{ marginLeft: 'auto' }}>
              {stamp && (
                <span
                  style={{
                    fontSize: '11px',
                    color: 'var(--text-muted)',
                    textTransform: 'none',
                    letterSpacing: 0,
                    fontWeight: 400,
                  }}
                >
                  {stamp}
                </span>
              )}
              {showActions && (
                <>
                  <button
                    onClick={() => onBeginEdit(t)}
                    title="Edit & resend — discards history after this turn, restarts core, then re-sends the edited message"
                    style={{
                      background: 'transparent',
                      border: '1px solid var(--border-muted)',
                      color: 'var(--text-muted)',
                      borderRadius: '3px',
                      padding: '2px 6px',
                      cursor: 'pointer',
                      display: 'inline-flex',
                      alignItems: 'center',
                      gap: '4px',
                      fontSize: '11px',
                    }}
                  >
                    <Pencil size={12} />
                    <span>Edit</span>
                  </button>
                  <button
                    onClick={() => onRetryLastUserTurn(t.turn_id)}
                    title="Retry — discards history after this turn and re-sends the same message"
                    style={{
                      background: 'transparent',
                      border: '1px solid var(--border-muted)',
                      color: 'var(--text-muted)',
                      borderRadius: '3px',
                      padding: '2px 6px',
                      cursor: 'pointer',
                      display: 'inline-flex',
                      alignItems: 'center',
                      gap: '4px',
                      fontSize: '11px',
                    }}
                  >
                    <RefreshCw size={12} />
                    <span>Retry</span>
                  </button>
                </>
              )}
            </span>
          </div>
          {isEditing ? (
            <div style={{ display: 'flex', flexDirection: 'column', gap: '8px' }}>
              <textarea
                value={editingDraft}
                onChange={(e) => onEditingDraftChange(e.target.value)}
                rows={Math.min(8, Math.max(2, editingDraft.split('\n').length))}
                autoFocus
                onKeyDown={(e) => {
                  if (e.key === 'Enter' && (e.ctrlKey || e.metaKey)) {
                    e.preventDefault();
                    onCommitEdit();
                  } else if (e.key === 'Escape') {
                    e.preventDefault();
                    onCancelEdit();
                  }
                }}
                style={{
                  width: '100%',
                  background: 'rgba(0, 0, 0, 0.2)',
                  color: 'var(--text-primary)',
                  border: '1px solid var(--border-muted)',
                  borderRadius: '4px',
                  padding: '6px 8px',
                  fontFamily: 'inherit',
                  fontSize: '14px',
                  resize: 'vertical',
                }}
              />
              <div style={{ display: 'flex', gap: '6px', justifyContent: 'flex-end' }}>
                <button
                  onClick={onCancelEdit}
                  style={{
                    background: 'transparent',
                    border: '1px solid var(--border-muted)',
                    color: 'var(--text-muted)',
                    fontSize: '12px',
                    padding: '4px 10px',
                    borderRadius: '3px',
                    cursor: 'pointer',
                  }}
                >
                  Cancel
                </button>
                <button
                  onClick={onCommitEdit}
                  disabled={!editingDraft.trim()}
                  title="Ctrl/⌘+Enter"
                  style={{
                    background: 'var(--color-primary)',
                    border: '1px solid var(--color-primary)',
                    color: '#fff',
                    fontSize: '12px',
                    padding: '4px 10px',
                    borderRadius: '3px',
                    cursor: editingDraft.trim() ? 'pointer' : 'not-allowed',
                    opacity: editingDraft.trim() ? 1 : 0.5,
                    display: 'inline-flex',
                    alignItems: 'center',
                    gap: '4px',
                  }}
                >
                  <Check size={12} />
                  Save &amp; Resend
                </button>
              </div>
            </div>
          ) : (
            <>
              <RenderedMessageBody
                text={visibleUserMessage}
                renderMessageBody={renderMessageBody}
                onOpenFile={onOpenFile}
              />
              <UserAttachmentPreviewGrid
                attachments={attachmentsForTurn}
                onOpenImage={onPreviewAttachment}
                onOpenFile={onOpenFile}
              />
            </>
          )}
        </div>
        {!isEditing && (assistantContent || hasAssistantActivity) && (
          <div className="message-assistant-flow">
            <div className="message-header">{t.provider} ({t.role})</div>
            {assistantContent ? (
              <RenderedMessageBody
                text={assistantContent}
                renderMessageBody={renderMessageBody}
                onOpenFile={onOpenFile}
              />
            ) : null}
            {renderTurnEvents(t.turn_id, turnEvents, relatedTurns, realtimeLines, scopedHyardJobs, scopedRenderOptions)}
          </div>
        )}
      </React.Fragment>
    );
  }

  if (t.origin === 'system') {
    const parsedResults = cachedParseSystemFeedbackResults(t.user_message);
    return (
      <React.Fragment key={t.turn_id || idx}>
        <SystemFeedbackResults results={parsedResults} />
        {(assistantContent || hasAssistantActivity) && (
          <div className="message-assistant-flow">
            <div className="message-header">{t.provider} ({t.role})</div>
            {assistantContent ? (
              <RenderedMessageBody
                text={assistantContent}
                renderMessageBody={renderMessageBody}
                onOpenFile={onOpenFile}
              />
            ) : null}
            {renderTurnEvents(t.turn_id, turnEvents, relatedTurns, realtimeLines, scopedHyardJobs, scopedRenderOptions)}
          </div>
        )}
      </React.Fragment>
    );
  }

  if (t.origin === 'delegate') {
    return null;
  }

  return (
    <div key={t.turn_id || idx} className="message-bubble message-assistant">
      <div className="message-header">{t.provider} ({t.role})</div>
      {assistantContent ? (
        <RenderedMessageBody
          text={assistantContent}
          renderMessageBody={renderMessageBody}
          onOpenFile={onOpenFile}
        />
      ) : null}
      {renderTurnEvents(t.turn_id, turnEvents, relatedTurns, realtimeLines, scopedHyardJobs, scopedRenderOptions)}
    </div>
  );
};

function historicalTurnContentPropsEqual(
  prev: HistoricalTurnContentProps,
  next: HistoricalTurnContentProps,
): boolean {
  if (prev.turn !== next.turn) return false;
  if (prev.originalIndex !== next.originalIndex) return false;
  if (prev.storedAttachments !== next.storedAttachments) return false;
  if (prev.hasAssistantActivity !== next.hasAssistantActivity) return false;
  if (prev.turnEvents !== next.turnEvents) return false;
  if (prev.relatedTurns !== next.relatedTurns) return false;
  if (prev.realtimeLines !== next.realtimeLines) return false;
  if (prev.hyardJob !== next.hyardJob) return false;

  const turnId = next.turn.turn_id;
  const wasLastUser = Boolean(prev.turn.turn_id && prev.turn.turn_id === prev.lastUserTurnId);
  const isLastUser = Boolean(turnId && turnId === next.lastUserTurnId);
  if (wasLastUser !== isLastUser) return false;
  if ((wasLastUser || isLastUser) && prev.isGenerating !== next.isGenerating) return false;

  const wasEditing = prev.editingTurnId === prev.turn.turn_id;
  const isEditing = next.editingTurnId === turnId;
  if (wasEditing !== isEditing) return false;
  if ((wasEditing || isEditing) && prev.editingDraft !== next.editingDraft) return false;

  return true;
}

const HistoricalTurnContent = React.memo(HistoricalTurnContentBase, historicalTurnContentPropsEqual);

function useRafThrottledValue<T>(value: T): T {
  const [throttled, setThrottled] = useState(value);
  const latestRef = useRef(value);
  const frameRef = useRef<number | null>(null);

  useEffect(() => {
    latestRef.current = value;
    if (frameRef.current !== null) return;
    frameRef.current = requestAnimationFrame(() => {
      frameRef.current = null;
      setThrottled(latestRef.current);
    });
  }, [value]);

  useEffect(() => {
    return () => {
      if (frameRef.current !== null) {
        cancelAnimationFrame(frameRef.current);
        frameRef.current = null;
      }
    };
  }, []);

  return throttled;
}

function hasMeaningfulArtifactValue(value: any): boolean {
  if (value === undefined || value === null) return false;
  if (typeof value === 'string') return value.trim().length > 0;
  if (Array.isArray(value)) return value.some(hasMeaningfulArtifactValue);
  if (typeof value === 'object') {
    return Object.entries(value).some(([key, nested]) => {
      if (['type', 'role', 'status', 'id', 'call_id', 'tool_call_id', 'request_id', 'index', 'encrypted_content'].includes(key)) {
        return false;
      }
      return hasMeaningfulArtifactValue(nested);
    });
  }
  return false;
}

function eventHasRenderableArtifact(event: any): boolean {
  const payload = event?.payload;
  if (!payload) return false;
  const item = (
    payload.item ||
    payload.params?.item ||
    payload.event?.item ||
    payload.msg?.item ||
    payload.message?.item ||
    payload.data?.item ||
    payload.params ||
    payload.event ||
    payload.msg ||
    payload
  );
  const itemTypeCandidate = String(item?.type || '').toLowerCase();
  const itemTypeFromItem = itemTypeCandidate && !itemTypeCandidate.includes('.') && !itemTypeCandidate.includes('/')
    ? itemTypeCandidate
    : '';
  const rawPayloadType = String(payload?.type || payload?.params?.type || '').toLowerCase();
  const payloadTypeAsItem = rawPayloadType && !rawPayloadType.includes('.') && !rawPayloadType.includes('/')
    ? rawPayloadType
    : '';
  const itemType = String(
    payload?.item_type ||
    payload?.params?.item_type ||
    itemTypeFromItem ||
    payloadTypeAsItem,
  ).toLowerCase();
  const protocolType = String(payload?.method || payload?.params?.method || payload?.type || '').toLowerCase().replace(/\//g, '.');
  if (!itemType) {
    return Boolean(
      item?.execution ||
      payload.execution ||
      payload.params?.execution ||
      item?.line ||
      payload.line ||
      payload.params?.line ||
      item?.output ||
      payload.output ||
      payload.params?.output ||
      item?.result ||
      payload.result ||
      payload.params?.result ||
      item?.aggregated_output ||
      payload.aggregated_output ||
      payload.params?.aggregated_output,
    );
  }
  if (['agent_message', 'assistant'].includes(itemType)) return false;
  if (itemType === 'reasoning') {
    return [
      item?.summary,
      payload?.summary,
      payload?.params?.summary,
      item?.text,
      payload?.text,
      payload?.params?.text,
      item?.content,
      payload?.content,
      payload?.params?.content,
      item?.delta?.summary,
      payload?.delta?.summary,
      payload?.params?.delta?.summary,
      item?.delta?.text,
      payload?.delta?.text,
      payload?.params?.delta?.text,
      item?.delta?.content,
      payload?.delta?.content,
      payload?.params?.delta?.content,
    ].some(hasMeaningfulArtifactValue);
  }
  if (protocolType.startsWith('turn.')) return false;
  return true;
}

function normalizedOptionalString(value: unknown): string | null {
  if (value === undefined || value === null) return null;
  const text = String(value).trim();
  return text.length > 0 ? text : null;
}

function indexHyardJobsByTurnId(hyardJobs?: Record<string, any>): Map<string, any> {
  const byTurnId = new Map<string, any>();
  if (!hyardJobs) return byTurnId;
  Object.values(hyardJobs).forEach((job) => {
    const turnId = normalizedOptionalString(job?.turn_id ?? job?.turnId);
    if (!turnId) return;
    byTurnId.set(turnId, job);
  });
  return byTurnId;
}

export const ChatArea: React.FC<ChatAreaProps> = ({
  selectedSession,
  isGenerating,
  turns,
  turnAttachments = {},
  runtimeDispatchStartedAt,
  runtimeDispatchPhase,
  activeCoreRuntimePhase,
  activePeerRuntimePhase,
  handleSend,
  handleCancel,
  activeCoreText,
  activeCoreTurnId,
  activePeerName,
  activePeerTurnId,
  activePeerText,
  sessionEvents,
  realtimeTerminalLines,
  hyardJobs,
  renderMessageBody,
  renderTurnEvents,
  renderTurnActivitySummary,
  queuedMessages,
  onClearQueue,
  sandboxMode,
  onSandboxModeChange,
  onEditAndResend,
  onRetryLastUserTurn,
  onOpenFile,
  isHistoryLoading = false,
  historyLoadingPhase = null,
  historyLoadingError = null,
  historyPartial = false,
  historyHasMoreBefore = false,
  onLoadOlderHistory,
  isLoadingOlderHistory = false,
  onComposerReady,
}) => {
  const messagesContainerRef = useRef<HTMLDivElement>(null);
  const messagesContentRef = useRef<HTMLDivElement>(null);
  const messagesEndRef = useRef<HTMLDivElement>(null);
  const scrollRafRef = useRef<number | null>(null);
  const virtualScrollRafRef = useRef<number | null>(null);
  const virtualMeasureRafRef = useRef<number | null>(null);
  const deferredVirtualMeasureTimerRef = useRef<ReturnType<typeof window.setTimeout> | null>(null);
  const scrollSeekIdleTimerRef = useRef<ReturnType<typeof window.setTimeout> | null>(null);
  const isVirtualScrollSeekingRef = useRef(false);
  const lastVirtualScrollSampleRef = useRef<{ top: number; at: number } | null>(null);
  const suppressScrollSeekUntilRef = useRef(0);
  const userPinnedToBottomRef = useRef(true);
  const userScrollInteractionUntilRef = useRef(0);
  const groupedSessionEventsCacheRef = useRef<Map<string, CachedGroupedSessionEvents>>(new Map());
  const turnIndexesCacheRef = useRef<Map<string, CachedTurnIndexes>>(new Map());
  const hyardJobIndexCacheRef = useRef<Map<string, CachedHyardJobIndex>>(new Map());
  const virtualRowHeightsRef = useRef<Map<string, number>>(new Map());
  const virtualRowHeightSessionTouchedAtRef = useRef<Map<string, number>>(new Map());
  const virtualRowLayoutRef = useRef<Map<string, Pick<VirtualTurnMetric, 'start' | 'size'>>>(new Map());
  const settleScrollTimerRef = useRef<ReturnType<typeof window.setTimeout> | null>(null);
  const settleAutoScrollUntilRef = useRef<number>(0);
  const lastAutoScrollSessionIdRef = useRef<string | null | undefined>(undefined);
  const pendingInitialScrollSessionIdRef = useRef<string | null>(null);
  const handleSendRef = useRef(handleSend);
  const handleCancelRef = useRef(handleCancel);
  handleSendRef.current = handleSend;
  handleCancelRef.current = handleCancel;
  const renderedActiveCoreText = useRafThrottledValue(activeCoreText);
  const renderedActivePeerText = useRafThrottledValue(activePeerText);
  const composerSend = useCallback(
    (payload: SendPayload, restoreText?: (text: string) => void) =>
      handleSendRef.current(payload, restoreText),
    [],
  );
  const composerCancel = useCallback(() => handleCancelRef.current(), []);
  // Inline-edit state for the last user-message bubble. We keep this at the
  // ChatArea level (not inside the bubble) so a session switch or new turn
  // appended above doesn't accidentally trap us in edit mode.
  const [virtualViewport, setVirtualViewport] = useState({ scrollTop: 0, height: 0 });
  const [virtualMeasureVersion, setVirtualMeasureVersion] = useState(0);
  const [isVirtualScrollSeeking, setIsVirtualScrollSeeking] = useState(false);
  const [editingTurnId, setEditingTurnId] = useState<string | null>(null);
  const [editingDraft, setEditingDraft] = useState<string>('');
  const [previewAttachment, setPreviewAttachment] = useState<InputAttachment | null>(null);
  const currentSessionId = selectedSession?.session_id ?? null;
  const sessionCacheKey = virtualSessionKey(currentSessionId);

  const {
    lastUserTurnId,
    delegateTurnsByParent,
    relatedTurnsByTurnId,
  } = useMemo(() => {
    const previous = turnIndexesCacheRef.current.get(sessionCacheKey);
    if (previous?.sourceTurns === turns) {
      touchSessionCacheEntry(turnIndexesCacheRef.current, sessionCacheKey, previous);
      return previous;
    }

    const next = {
      sourceTurns: turns,
      ...buildTurnIndexes(turns),
    };
    touchSessionCacheEntry(turnIndexesCacheRef.current, sessionCacheKey, next);
    return next;
  }, [sessionCacheKey, turns]);

  const { eventsByTurnId, eventActivityTurnIds } = useMemo(() => {
    const grouped = deriveGroupedSessionEvents(
      sessionEvents,
      groupedSessionEventsCacheRef.current.get(sessionCacheKey) ?? null,
    );
    const next = {
      sourceEvents: sessionEvents,
      eventsByTurnId: grouped.eventsByTurnId,
      eventActivityTurnIds: grouped.eventActivityTurnIds,
    };
    touchSessionCacheEntry(groupedSessionEventsCacheRef.current, sessionCacheKey, next);
    return next;
  }, [sessionCacheKey, sessionEvents]);

  const hyardJobsByTurnId = useMemo(() => {
    const previous = hyardJobIndexCacheRef.current.get(sessionCacheKey);
    if (previous && previous.sourceJobs === hyardJobs) {
      touchSessionCacheEntry(hyardJobIndexCacheRef.current, sessionCacheKey, previous);
      return previous.jobsByTurnId;
    }
    const jobsByTurnId = indexHyardJobsByTurnId(hyardJobs);
    touchSessionCacheEntry(hyardJobIndexCacheRef.current, sessionCacheKey, {
      sourceJobs: hyardJobs,
      jobsByTurnId,
    });
    return jobsByTurnId;
  }, [hyardJobs, sessionCacheKey]);

  const scopedRenderOptions = useMemo<RenderTurnEventsOptions>(() => ({
    eventsAlreadyScoped: true,
    turnsAlreadyScoped: true,
  }), []);

  // Keep high-frequency realtime terminal chunks out of the history-row
  // inclusion set. Otherwise every 33ms terminal flush makes the virtual list
  // rescan the full turn history even though only the active/visible rows need
  // to know about live terminal output.
  const historyRenderableActivityTurnIds = useMemo(() => {
    const ids = new Set<string>();
    for (const turnId of eventActivityTurnIds) {
      ids.add(turnId);
    }
    for (const turnId of delegateTurnsByParent.keys()) {
      ids.add(turnId);
    }
    for (const turnId of hyardJobsByTurnId.keys()) {
      ids.add(turnId);
    }
    return ids;
  }, [delegateTurnsByParent, eventActivityTurnIds, hyardJobsByTurnId]);

  const hasRenderableActivityForTurn = useCallback((turnId?: string | null) => {
    if (!turnId) return false;
    if (historyRenderableActivityTurnIds.has(turnId)) return true;
    if ((realtimeTerminalLines[turnId]?.length ?? 0) > 0) return true;
    return hyardJobsByTurnId.has(turnId);
  }, [historyRenderableActivityTurnIds, hyardJobsByTurnId, realtimeTerminalLines]);
  const activeCoreEvents = activeCoreTurnId ? (eventsByTurnId.get(activeCoreTurnId) ?? EMPTY_EVENT_LIST) : EMPTY_EVENT_LIST;
  const activeCoreRelatedTurns = activeCoreTurnId ? (relatedTurnsByTurnId.get(activeCoreTurnId) ?? EMPTY_TURN_LIST) : EMPTY_TURN_LIST;
  const activePeerEvents = activePeerTurnId ? (eventsByTurnId.get(activePeerTurnId) ?? EMPTY_EVENT_LIST) : EMPTY_EVENT_LIST;
  const activePeerRelatedTurns = activePeerTurnId ? (relatedTurnsByTurnId.get(activePeerTurnId) ?? EMPTY_TURN_LIST) : EMPTY_TURN_LIST;
  const activeCoreHasRenderableActivity = hasRenderableActivityForTurn(activeCoreTurnId);
  const activePeerHasRenderableActivity = hasRenderableActivityForTurn(activePeerTurnId);
  const activeCoreRuntimeIsActive = activeCoreTurnId ? isRuntimePhaseActive(activeCoreRuntimePhase) : false;
  const activePeerRuntimeIsActive = activePeerTurnId ? isRuntimePhaseActive(activePeerRuntimePhase) : false;
  const showActiveCorePanel = Boolean(
    activeCoreText ||
    (
      activeCoreTurnId &&
      !activePeerName &&
      (isGenerating || activeCoreRuntimeIsActive || activeCoreHasRenderableActivity)
    ),
  );
  const showActivePeerPanel = Boolean(
    activePeerName &&
    (
      activePeerText ||
      isGenerating ||
      activePeerRuntimeIsActive ||
      activePeerHasRenderableActivity
    ),
  );
  const showRuntimeDispatchPanel =
    typeof runtimeDispatchStartedAt === 'number' &&
    Number.isFinite(runtimeDispatchStartedAt) &&
    !activeCoreText &&
    !activeCoreTurnId &&
    !activePeerName;
  const historyLoadingLabel = historyLoadingPhase === 'activity'
    ? '正在加载运行记录和工具活动…'
    : historyLoadingPhase === 'older'
      ? '正在加载更早的消息…'
      : historyLoadingPhase === 'refresh'
        ? '正在刷新会话历史…'
        : '正在加载最近的会话消息…';
  const showHistoryLoadingEmptyState = Boolean(
    selectedSession &&
    turns.length === 0 &&
    !showRuntimeDispatchPanel &&
    !showActiveCorePanel &&
    !showActivePeerPanel &&
    (isHistoryLoading || historyLoadingError),
  );
  const showHistoryStatusStrip = Boolean(
    selectedSession &&
    turns.length > 0 &&
    (
      historyHasMoreBefore ||
      historyPartial ||
      isHistoryLoading ||
      historyLoadingError
    ),
  );
  const handleLoadOlderHistoryClick = useCallback(() => {
    if (!onLoadOlderHistory || isLoadingOlderHistory) return;
    void onLoadOlderHistory();
  }, [isLoadingOlderHistory, onLoadOlderHistory]);

  const endVirtualScrollSeek = useCallback(() => {
    if (scrollSeekIdleTimerRef.current !== null) {
      window.clearTimeout(scrollSeekIdleTimerRef.current);
      scrollSeekIdleTimerRef.current = null;
    }
    if (isVirtualScrollSeekingRef.current) {
      isVirtualScrollSeekingRef.current = false;
      setIsVirtualScrollSeeking(false);
    }
  }, []);

  const markProgrammaticScroll = useCallback((durationMs = 220) => {
    const now = scrollClockNow();
    suppressScrollSeekUntilRef.current = Math.max(
      suppressScrollSeekUntilRef.current,
      now + durationMs,
    );
    endVirtualScrollSeek();
  }, [endVirtualScrollSeek]);

  const updateScrollPinState = useCallback((container: HTMLDivElement) => {
    const distanceToBottom = container.scrollHeight - container.scrollTop - container.clientHeight;
    userPinnedToBottomRef.current = distanceToBottom <= SCROLL_BOTTOM_PIN_THRESHOLD_PX;
    return distanceToBottom;
  }, []);

  const shouldAutoScrollToLatest = useCallback((force = false) => {
    if (force) return true;
    if (userPinnedToBottomRef.current) return true;
    return Date.now() <= settleAutoScrollUntilRef.current;
  }, []);

  const readVirtualViewport = useCallback(() => {
    const container = messagesContainerRef.current;
    if (!container) return;
    const now = scrollClockNow();
    const next = {
      scrollTop: container.scrollTop,
      height: container.clientHeight,
    };
    updateScrollPinState(container);
    const previousSample = lastVirtualScrollSampleRef.current;
    lastVirtualScrollSampleRef.current = { top: next.scrollTop, at: now };
    const isProgrammaticScroll = now <= suppressScrollSeekUntilRef.current;
    if (!isProgrammaticScroll && previousSample) {
      const deltaPx = Math.abs(next.scrollTop - previousSample.top);
      const deltaMs = Math.max(1, now - previousSample.at);
      const velocity = deltaPx / deltaMs;
      const shouldSeek =
        deltaPx >= VIRTUAL_SCROLL_SEEK_MIN_DELTA_PX ||
        velocity >= VIRTUAL_SCROLL_SEEK_VELOCITY_PX_PER_MS;
      if (shouldSeek || isVirtualScrollSeekingRef.current) {
        if (!isVirtualScrollSeekingRef.current) {
          isVirtualScrollSeekingRef.current = true;
          setIsVirtualScrollSeeking(true);
        }
        if (scrollSeekIdleTimerRef.current !== null) {
          window.clearTimeout(scrollSeekIdleTimerRef.current);
        }
        scrollSeekIdleTimerRef.current = window.setTimeout(() => {
          scrollSeekIdleTimerRef.current = null;
          isVirtualScrollSeekingRef.current = false;
          setIsVirtualScrollSeeking(false);
        }, VIRTUAL_SCROLL_SEEK_IDLE_MS);
      }
    }
    setVirtualViewport((prev) => {
      if (
        Math.abs(prev.scrollTop - next.scrollTop) < 0.5 &&
        Math.abs(prev.height - next.height) < 0.5
      ) {
        return prev;
      }
      return next;
    });
  }, [updateScrollPinState]);

  const scheduleVirtualViewportRead = useCallback(() => {
    if (virtualScrollRafRef.current !== null) return;
    virtualScrollRafRef.current = requestAnimationFrame(() => {
      virtualScrollRafRef.current = null;
      readVirtualViewport();
    });
  }, [readVirtualViewport]);

  const handleMessagesScroll = useCallback(() => {
    const container = messagesContainerRef.current;
    const now = scrollClockNow();
    const isProgrammaticScroll = now <= suppressScrollSeekUntilRef.current;
    if (container) {
      const distanceToBottom = updateScrollPinState(container);
      const isUserOverrideDuringProgrammaticScroll =
        isProgrammaticScroll && distanceToBottom > SCROLL_BOTTOM_PIN_THRESHOLD_PX;
      if (!isProgrammaticScroll && distanceToBottom <= SCROLL_BOTTOM_PIN_THRESHOLD_PX) {
        scheduleVirtualViewportRead();
        return;
      }
      if (!isProgrammaticScroll || isUserOverrideDuringProgrammaticScroll) {
        if (isUserOverrideDuringProgrammaticScroll) {
          suppressScrollSeekUntilRef.current = 0;
        }
        userScrollInteractionUntilRef.current = Math.max(
          userScrollInteractionUntilRef.current,
          now + USER_SCROLL_INTERACTION_IDLE_MS,
        );
        // A stream/update may have queued an auto-scroll while the user was still
        // pinned. If the user then wheels/drags upward before that RAF fires,
        // cancel it immediately so the viewport does not snap back to the live
        // process panel.
        if (distanceToBottom > SCROLL_BOTTOM_PIN_THRESHOLD_PX) {
          if (scrollRafRef.current !== null) {
            cancelAnimationFrame(scrollRafRef.current);
            scrollRafRef.current = null;
          }
          if (settleScrollTimerRef.current !== null) {
            window.clearTimeout(settleScrollTimerRef.current);
            settleScrollTimerRef.current = null;
          }
          settleAutoScrollUntilRef.current = 0;
        }
      }
    }
    scheduleVirtualViewportRead();
  }, [scheduleVirtualViewportRead, updateScrollPinState]);

  const virtualHistoryTurns = useMemo<VirtualTurnEntry[]>(() => {
    const entries: VirtualTurnEntry[] = [];
    turns.forEach((turn, originalIndex) => {
      if (turn.origin === 'delegate') return;
      const assistantContent = turn.provider_response || turn.error_message;
      const containsSystemFeedback = turn.user_message.includes('<<<SWITCHYARD_JSON_BEGIN>>>');
      const hasActivity = Boolean(turn.turn_id && historyRenderableActivityTurnIds.has(turn.turn_id));
      const shouldRender =
        turn.origin === 'user'
          ? true
          : turn.origin === 'system'
            ? Boolean(assistantContent || hasActivity || containsSystemFeedback)
            : true;
      if (!shouldRender) return;
      entries.push({
        key: `${sessionCacheKey}:${turn.turn_id || `${turn.origin}-${originalIndex}`}`,
        turn,
        originalIndex,
      });
    });
    return entries;
  }, [historyRenderableActivityTurnIds, sessionCacheKey, turns]);

  const virtualMetrics = useMemo(() => {
    const metrics: VirtualTurnMetric[] = [];
    let cursor = 0;
    virtualHistoryTurns.forEach((entry, idx) => {
      const size = virtualRowHeightsRef.current.get(entry.key) ?? VIRTUAL_TURN_ESTIMATED_HEIGHT;
      metrics.push({
        ...entry,
        start: cursor,
        size,
      });
      cursor += size;
      if (idx < virtualHistoryTurns.length - 1) {
        cursor += VIRTUAL_TURN_GAP;
      }
    });
    return {
      totalHeight: cursor,
      metrics,
    };
  }, [virtualHistoryTurns, virtualMeasureVersion]);

  const virtualVisibleRows = useMemo(() => {
    const { metrics } = virtualMetrics;
    const viewportHeight = Math.max(virtualViewport.height || 0, 1);
    const rowOverscan = isVirtualScrollSeeking
      ? VIRTUAL_TURN_SCROLL_SEEK_OVERSCAN
      : VIRTUAL_TURN_OVERSCAN;
    const overscanPx = isVirtualScrollSeeking
      ? Math.max(VIRTUAL_TURN_ESTIMATED_HEIGHT, viewportHeight * 0.25)
      : Math.max(
          VIRTUAL_TURN_ESTIMATED_HEIGHT * 2,
          viewportHeight * 0.75,
        );
    const visibleTop = Math.max(0, virtualViewport.scrollTop - overscanPx);
    const visibleBottom = virtualViewport.scrollTop + viewportHeight + overscanPx;

    let low = 0;
    let high = metrics.length;
    while (low < high) {
      const mid = Math.floor((low + high) / 2);
      if (metrics[mid].start + metrics[mid].size < visibleTop) {
        low = mid + 1;
      } else {
        high = mid;
      }
    }
    const startIndex = Math.max(0, low - rowOverscan);

    low = startIndex;
    high = metrics.length;
    while (low < high) {
      const mid = Math.floor((low + high) / 2);
      if (metrics[mid].start <= visibleBottom) {
        low = mid + 1;
      } else {
        high = mid;
      }
    }
    const endIndex = Math.min(metrics.length, low + rowOverscan);

    return metrics.slice(startIndex, endIndex);
  }, [
    isVirtualScrollSeeking,
    virtualMetrics,
    virtualViewport.height,
    virtualViewport.scrollTop,
  ]);

  // No cleanup effect needed: the bubble only enters edit UI when
  // `editingTurnId === t.turn_id`, so a stale id whose turn has been wiped
  // (session switch, rewind) silently goes inert. `beginEdit` always resets
  // `editingDraft` from the fresh turn, so cross-bubble draft bleed is
  // impossible.

  // Auto scroll messages container to bottom when turns/state change. Streaming
  // text/tool logs can update many times per second; repeatedly starting
  // `behavior: "smooth"` animations on every chunk is a real input/paint
  // bottleneck on long transcripts. Throttle to at most one layout scroll per
  // animation frame and use an immediate scroll. Use the scroll container
  // directly instead of only `scrollIntoView()` so selecting/restoring a
  // historical session cannot preserve the previous container offset and leave
  // the user above the latest turn.
  const scrollMessagesToLatest = useCallback(() => {
    const container = messagesContainerRef.current;
    if (container) {
      markProgrammaticScroll();
      container.scrollTop = Math.max(0, container.scrollHeight - container.clientHeight);
      const now = scrollClockNow();
      const nextViewport = {
        scrollTop: container.scrollTop,
        height: container.clientHeight,
      };
      lastVirtualScrollSampleRef.current = { top: nextViewport.scrollTop, at: now };
      userPinnedToBottomRef.current = true;
      setVirtualViewport((prev) => {
        if (
          Math.abs(prev.scrollTop - nextViewport.scrollTop) < 0.5 &&
          Math.abs(prev.height - nextViewport.height) < 0.5
        ) {
          return prev;
        }
        return nextViewport;
      });
      return;
    }
    markProgrammaticScroll();
    messagesEndRef.current?.scrollIntoView({ block: 'end' });
  }, [markProgrammaticScroll]);

  const scheduleVirtualMeasureVersionBump = useCallback((deferWhileUserScrolling = false) => {
    const now = scrollClockNow();
    const userScrollIdleInMs = Math.max(0, userScrollInteractionUntilRef.current - now);
    if (deferWhileUserScrolling && (userScrollIdleInMs > 0 || isVirtualScrollSeekingRef.current)) {
      const delayMs = Math.max(
        VIRTUAL_SCROLL_SEEK_IDLE_MS,
        Math.min(USER_SCROLL_INTERACTION_IDLE_MS, userScrollIdleInMs || VIRTUAL_SCROLL_SEEK_IDLE_MS),
      );
      if (deferredVirtualMeasureTimerRef.current !== null) {
        window.clearTimeout(deferredVirtualMeasureTimerRef.current);
      }
      deferredVirtualMeasureTimerRef.current = window.setTimeout(() => {
        deferredVirtualMeasureTimerRef.current = null;
        if (virtualMeasureRafRef.current !== null) return;
        virtualMeasureRafRef.current = requestAnimationFrame(() => {
          virtualMeasureRafRef.current = null;
          setVirtualMeasureVersion((version) => version + 1);
        });
      }, delayMs);
      return;
    }
    if (deferredVirtualMeasureTimerRef.current !== null) {
      window.clearTimeout(deferredVirtualMeasureTimerRef.current);
      deferredVirtualMeasureTimerRef.current = null;
    }
    if (virtualMeasureRafRef.current !== null) return;
    virtualMeasureRafRef.current = requestAnimationFrame(() => {
      virtualMeasureRafRef.current = null;
      setVirtualMeasureVersion((version) => version + 1);
    });
  }, []);

  const handleVirtualRowMeasure = useCallback((key: string, height: number) => {
    const nextHeight = Math.max(0, Math.ceil(height));
    const previousStoredHeight = virtualRowHeightsRef.current.get(key);
    if (previousStoredHeight !== undefined && Math.abs(previousStoredHeight - nextHeight) < VIRTUAL_ROW_MEASURE_EPSILON_PX) {
      return;
    }

    const container = messagesContainerRef.current;
    const previousHeight = previousStoredHeight ?? VIRTUAL_TURN_ESTIMATED_HEIGHT;
    const delta = nextHeight - previousHeight;
    const rowLayout = virtualRowLayoutRef.current.get(key);
    const isSettlingToBottom = Date.now() <= settleAutoScrollUntilRef.current;
    const wasNearBottom = container
      ? container.scrollHeight - container.scrollTop - container.clientHeight < 96
      : false;

    virtualRowHeightsRef.current.set(key, nextHeight);

    const isUserActivelyScrolling =
      scrollClockNow() <= userScrollInteractionUntilRef.current ||
      isVirtualScrollSeekingRef.current;

    if (container && rowLayout && Math.abs(delta) >= 4 && !wasNearBottom && !isUserActivelyScrolling) {
      const previousRowBottom = rowLayout.start + previousHeight;
      if (previousRowBottom <= container.scrollTop) {
        markProgrammaticScroll();
        container.scrollTop = Math.max(0, container.scrollTop + delta);
        const now = scrollClockNow();
        const nextViewport = {
          scrollTop: container.scrollTop,
          height: container.clientHeight,
        };
        lastVirtualScrollSampleRef.current = { top: nextViewport.scrollTop, at: now };
        setVirtualViewport((prev) => {
          if (
            Math.abs(prev.scrollTop - nextViewport.scrollTop) < 0.5 &&
            Math.abs(prev.height - nextViewport.height) < 0.5
          ) {
            return prev;
          }
          return nextViewport;
        });
      }
    }

    scheduleVirtualMeasureVersionBump(isUserActivelyScrolling);

    if ((wasNearBottom || isSettlingToBottom) && !isUserActivelyScrolling) {
      requestAnimationFrame(scrollMessagesToLatest);
    }
  }, [markProgrammaticScroll, scheduleVirtualMeasureVersionBump, scrollMessagesToLatest]);

  const scheduleSettleScrollPulse = useCallback(() => {
    if (settleScrollTimerRef.current !== null) {
      window.clearTimeout(settleScrollTimerRef.current);
      settleScrollTimerRef.current = null;
    }

    const pulse = () => {
      settleScrollTimerRef.current = null;
      if (!shouldAutoScrollToLatest(false)) {
        return;
      }
      scrollMessagesToLatest();
      const remainingMs = settleAutoScrollUntilRef.current - Date.now();
      if (remainingMs <= 0) {
        return;
      }
      settleScrollTimerRef.current = window.setTimeout(
        pulse,
        Math.min(240, Math.max(50, remainingMs)),
      );
    };

    settleScrollTimerRef.current = window.setTimeout(pulse, 80);
  }, [scrollMessagesToLatest, shouldAutoScrollToLatest]);

  const scheduleScrollMessagesToLatest = useCallback((settleAfterLoad = false, immediate = false, force = false) => {
    if (settleAfterLoad) {
      settleAutoScrollUntilRef.current = Math.max(
        settleAutoScrollUntilRef.current,
        Date.now() + SESSION_ENTRY_AUTO_SCROLL_SETTLE_MS,
      );
    }
    if (!shouldAutoScrollToLatest(force)) {
      return;
    }
    if (immediate) {
      scrollMessagesToLatest();
    }
    if (scrollRafRef.current === null) {
      const forceThisScroll = force;
      scrollRafRef.current = requestAnimationFrame(() => {
        scrollRafRef.current = null;
        if (!shouldAutoScrollToLatest(forceThisScroll)) {
          return;
        }
        scrollMessagesToLatest();
      });
    }
    if (settleAfterLoad) {
      // Session history can expand after the first paint as markdown,
      // syntax blocks, attachment previews, and Chromium content-visibility
      // estimates settle. Re-assert the bottom after that settle window so
      // "enter session" lands on the newest message instead of a stale offset.
      scheduleSettleScrollPulse();
    }
  }, [scheduleSettleScrollPulse, scrollMessagesToLatest, shouldAutoScrollToLatest]);
  const activeCoreTerminalLineCount = activeCoreTurnId ? (realtimeTerminalLines[activeCoreTurnId]?.length ?? 0) : 0;
  const activePeerTerminalLineCount = activePeerTurnId ? (realtimeTerminalLines[activePeerTurnId]?.length ?? 0) : 0;
  useEffect(() => {
    return () => {
      if (scrollRafRef.current !== null) {
        cancelAnimationFrame(scrollRafRef.current);
        scrollRafRef.current = null;
      }
      if (virtualScrollRafRef.current !== null) {
        cancelAnimationFrame(virtualScrollRafRef.current);
        virtualScrollRafRef.current = null;
      }
      if (virtualMeasureRafRef.current !== null) {
        cancelAnimationFrame(virtualMeasureRafRef.current);
        virtualMeasureRafRef.current = null;
      }
      if (deferredVirtualMeasureTimerRef.current !== null) {
        window.clearTimeout(deferredVirtualMeasureTimerRef.current);
        deferredVirtualMeasureTimerRef.current = null;
      }
      if (scrollSeekIdleTimerRef.current !== null) {
        window.clearTimeout(scrollSeekIdleTimerRef.current);
        scrollSeekIdleTimerRef.current = null;
      }
      if (settleScrollTimerRef.current !== null) {
        window.clearTimeout(settleScrollTimerRef.current);
        settleScrollTimerRef.current = null;
      }
    };
  }, []);
  useEffect(() => {
    const sessionKey = virtualSessionKey(currentSessionId);
    virtualRowHeightSessionTouchedAtRef.current.set(sessionKey, Date.now());
    if (virtualRowHeightSessionTouchedAtRef.current.size > MAX_VIRTUAL_ROW_HEIGHT_SESSIONS) {
      const sessionsByAge = Array.from(virtualRowHeightSessionTouchedAtRef.current.entries())
        .sort((a, b) => a[1] - b[1]);
      const staleSessionKeys = new Set(
        sessionsByAge
          .slice(0, Math.max(0, sessionsByAge.length - MAX_VIRTUAL_ROW_HEIGHT_SESSIONS))
          .map(([key]) => key),
      );
      staleSessionKeys.delete(sessionKey);
      if (staleSessionKeys.size > 0) {
        for (const staleSessionKey of staleSessionKeys) {
          virtualRowHeightSessionTouchedAtRef.current.delete(staleSessionKey);
        }
        for (const key of virtualRowHeightsRef.current.keys()) {
          if (staleSessionKeys.has(virtualRowSessionKey(key))) {
            virtualRowHeightsRef.current.delete(key);
          }
        }
      }
    }
    virtualRowLayoutRef.current.clear();
    lastVirtualScrollSampleRef.current = null;
    suppressScrollSeekUntilRef.current = 0;
    userPinnedToBottomRef.current = true;
    userScrollInteractionUntilRef.current = 0;
    isVirtualScrollSeekingRef.current = false;
    setIsVirtualScrollSeeking(false);
    if (virtualMeasureRafRef.current !== null) {
      cancelAnimationFrame(virtualMeasureRafRef.current);
      virtualMeasureRafRef.current = null;
    }
    if (deferredVirtualMeasureTimerRef.current !== null) {
      window.clearTimeout(deferredVirtualMeasureTimerRef.current);
      deferredVirtualMeasureTimerRef.current = null;
    }
    if (scrollSeekIdleTimerRef.current !== null) {
      window.clearTimeout(scrollSeekIdleTimerRef.current);
      scrollSeekIdleTimerRef.current = null;
    }
    setVirtualMeasureVersion((version) => version + 1);
    readVirtualViewport();
  }, [currentSessionId, readVirtualViewport]);
  useEffect(() => {
    const validKeys = new Set(virtualHistoryTurns.map((entry) => entry.key));
    const sessionKey = virtualSessionKey(currentSessionId);
    let removedAny = false;
    for (const key of virtualRowHeightsRef.current.keys()) {
      if (virtualRowSessionKey(key) === sessionKey && !validKeys.has(key)) {
        virtualRowHeightsRef.current.delete(key);
        removedAny = true;
      }
    }
    if (removedAny) {
      setVirtualMeasureVersion((version) => version + 1);
    }
  }, [currentSessionId, virtualHistoryTurns]);
  useLayoutEffect(() => {
    const next = new Map<string, Pick<VirtualTurnMetric, 'start' | 'size'>>();
    for (const metric of virtualMetrics.metrics) {
      next.set(metric.key, {
        start: metric.start,
        size: metric.size,
      });
    }
    virtualRowLayoutRef.current = next;
  }, [virtualMetrics.metrics]);
  useEffect(() => {
    const target = messagesContainerRef.current;
    readVirtualViewport();
    if (!target || typeof ResizeObserver === 'undefined') return;
    const observer = new ResizeObserver(() => {
      scheduleVirtualViewportRead();
    });
    observer.observe(target);
    return () => observer.disconnect();
  }, [readVirtualViewport, scheduleVirtualViewportRead]);
  useEffect(() => {
    const target = messagesContentRef.current;
    if (!target || typeof ResizeObserver === 'undefined') return;
    const observer = new ResizeObserver(() => {
      if (Date.now() <= settleAutoScrollUntilRef.current) {
        scheduleScrollMessagesToLatest(false, false, true);
      }
    });
    observer.observe(target);
    return () => observer.disconnect();
  }, [scheduleScrollMessagesToLatest]);
  useLayoutEffect(() => {
    if (shouldAutoScrollToLatest(false)) {
      scrollMessagesToLatest();
    }
  }, [scrollMessagesToLatest, shouldAutoScrollToLatest, virtualMetrics.totalHeight]);
  useLayoutEffect(() => {
    const sessionId = currentSessionId;
    const sessionChanged = lastAutoScrollSessionIdRef.current !== sessionId;
    if (sessionChanged) {
      lastAutoScrollSessionIdRef.current = sessionId;
      pendingInitialScrollSessionIdRef.current = sessionId;
    }
    const isInitialSessionLoad =
      sessionId !== null && pendingInitialScrollSessionIdRef.current === sessionId;
    const forceAutoScroll = sessionChanged || isInitialSessionLoad;
    scheduleScrollMessagesToLatest(forceAutoScroll, forceAutoScroll, forceAutoScroll);
    if (isInitialSessionLoad && turns.length > 0) {
      pendingInitialScrollSessionIdRef.current = null;
    }
  }, [
    currentSessionId,
    turns,
    isGenerating,
    renderedActiveCoreText,
    activeCoreTurnId,
    renderedActivePeerText,
    activePeerTurnId,
    queuedMessages.length,
    sessionEvents.length,
    activeCoreTerminalLineCount,
    activePeerTerminalLineCount,
    scheduleScrollMessagesToLatest,
  ]);

  const beginEdit = useCallback((turn: Turn) => {
    setEditingTurnId(turn.turn_id);
    setEditingDraft(cachedStripAttachmentReferences(turn.user_message));
  }, []);
  const cancelEdit = useCallback(() => {
    setEditingTurnId(null);
    setEditingDraft('');
  }, []);
  const commitEdit = useCallback(() => {
    const trimmed = editingDraft.trim();
    if (!trimmed || !editingTurnId) return;
    const id = editingTurnId;
    setEditingTurnId(null);
    setEditingDraft('');
    onEditAndResend(id, trimmed);
  }, [editingDraft, editingTurnId, onEditAndResend]);

  return (
    <div className="main-content glass-panel" style={{ display: 'flex', flexDirection: 'column', height: '100%', overflow: 'hidden', position: 'relative' }}>
      {previewAttachment && (
        <div
          role="dialog"
          aria-modal="true"
          aria-label="Image preview"
          onClick={() => setPreviewAttachment(null)}
          style={{
            position: 'absolute',
            inset: 0,
            zIndex: 100,
            background: 'rgba(2, 6, 23, 0.78)',
            backdropFilter: 'blur(8px)',
            display: 'flex',
            alignItems: 'center',
            justifyContent: 'center',
            padding: 24,
          }}
        >
          <div
            onClick={(event) => event.stopPropagation()}
            style={{
              maxWidth: '96%',
              maxHeight: '96%',
              display: 'flex',
              flexDirection: 'column',
              gap: 10,
              background: 'rgba(15, 23, 42, 0.92)',
              border: '1px solid rgba(148, 163, 184, 0.24)',
              borderRadius: 12,
              padding: 12,
              boxShadow: '0 24px 80px rgba(0, 0, 0, 0.55)',
            }}
          >
            <div style={{ display: 'flex', alignItems: 'center', gap: 10, minWidth: 0 }}>
              <ImageIcon size={15} style={{ color: 'var(--color-primary)', flex: '0 0 auto' }} />
              <div style={{ minWidth: 0, flex: 1 }}>
                <div style={{ color: 'var(--text-primary)', fontSize: 12, fontWeight: 700, overflow: 'hidden', textOverflow: 'ellipsis', whiteSpace: 'nowrap' }}>
                  {previewAttachment.name || filenameFromPath(previewAttachment.path)}
                </div>
                <div style={{ color: 'var(--text-muted)', fontSize: 11, overflow: 'hidden', textOverflow: 'ellipsis', whiteSpace: 'nowrap' }}>
                  {previewAttachment.path}
                </div>
              </div>
              <button
                type="button"
                onClick={() => setPreviewAttachment(null)}
                title="Close preview"
                style={{
                  width: 28,
                  height: 28,
                  borderRadius: 999,
                  border: '1px solid var(--border-muted)',
                  background: 'rgba(255,255,255,0.04)',
                  color: 'var(--text-secondary)',
                  display: 'inline-flex',
                  alignItems: 'center',
                  justifyContent: 'center',
                  cursor: 'pointer',
                }}
              >
                <X size={14} />
              </button>
            </div>
            <ImageAttachmentModalBody attachment={previewAttachment} />
          </div>
        </div>
      )}
      {/* Compact chat header — just the session label. Core /
          worker / peer detail moved into the corner bubble + the
          diagnostics drawer to keep the chat surface uncluttered. */}
      <div className="chat-header">
        <div className="chat-header-info">
          <h2 style={{ margin: 0 }}>
            {selectedSession
              ? selectedSession.name ?? `Session ${selectedSession.session_id.substring(0, 8)}`
              : 'New conversation'}
          </h2>
        </div>
        <div className="chat-actions">
          {isGenerating && (
            <div style={{ display: 'flex', alignItems: 'center', gap: '8px', color: 'var(--color-secondary)', fontSize: '12px' }}>
              <RefreshCw className="spin" size={14} style={{ animation: 'spin 2s linear infinite' }} />
              <span>Generating…</span>
            </div>
          )}
        </div>
      </div>

      <div
        ref={messagesContainerRef}
        className="chat-messages"
        onScroll={handleMessagesScroll}
        style={{ flex: 1, overflowY: 'auto', padding: '12px', overflowAnchor: 'none' }}
      >
        <div ref={messagesContentRef} style={{ minHeight: '100%', display: 'flex', flexDirection: 'column', gap: '14px', overflowAnchor: 'none' }}>
          {showHistoryLoadingEmptyState ? (
            <div className="empty-chat history-loading-empty">
              <RefreshCw
                size={32}
                className={isHistoryLoading ? 'spin' : undefined}
                style={{ color: historyLoadingError ? '#f87171' : 'var(--color-primary)' }}
              />
              <div>
                <h3>{historyLoadingError ? 'Conversation history did not load' : 'Loading conversation history…'}</h3>
                <p style={{ fontSize: '13px', marginTop: '6px', maxWidth: 460 }}>
                  {historyLoadingError
                    ? `无法加载这个会话的历史记录：${historyLoadingError}`
                    : historyLoadingLabel}
                </p>
              </div>
            </div>
          ) : turns.length === 0 &&
          !showRuntimeDispatchPanel &&
          !showActiveCorePanel &&
          !showActivePeerPanel ? (
            <div className="empty-chat">
              <MessageSquare size={48} className="empty-chat-logo" />
              <div>
                <h3>Start a Conversation</h3>
                <p style={{ fontSize: '13px', marginTop: '6px' }}>
                  Send a message to run the central orchestrator and delegate tasks to peers.
                </p>
              </div>
            </div>
          ) : (
            <>
              {showHistoryStatusStrip && (
                <div className="history-load-strip">
                  <div className="history-load-strip-copy">
                    {isHistoryLoading && (
                      <span className="history-load-inline-status">
                        <RefreshCw className="spin" size={13} />
                        {historyLoadingLabel}
                      </span>
                    )}
                    {!isHistoryLoading && historyPartial && (
                      <span>已优先载入最近消息，可按需继续加载更早历史。</span>
                    )}
                    {historyLoadingError && (
                      <span className="history-load-error">
                        历史刷新失败：{historyLoadingError}
                      </span>
                    )}
                  </div>
                  {(historyHasMoreBefore || historyPartial) && onLoadOlderHistory && (
                    <button
                      type="button"
                      className="history-load-more-button"
                      onClick={handleLoadOlderHistoryClick}
                      disabled={isLoadingOlderHistory}
                    >
                      {isLoadingOlderHistory ? (
                        <>
                          <RefreshCw className="spin" size={13} />
                          加载中…
                        </>
                      ) : (
                        <>
                          <ChevronDown size={14} />
                          加载更早消息
                        </>
                      )}
                    </button>
                  )}
                </div>
              )}
              {virtualHistoryTurns.length > 0 && (
                <div
                  style={{
                    position: 'relative',
                    height: virtualMetrics.totalHeight,
                    flex: '0 0 auto',
                    overflowAnchor: 'none',
                    pointerEvents: isVirtualScrollSeeking ? 'none' : undefined,
                  }}
                >
                  {virtualVisibleRows.map((virtualRow) => {
                    const hasMeasuredHeight = virtualRowHeightsRef.current.has(virtualRow.key);
                    if (isVirtualScrollSeeking && !hasMeasuredHeight) {
                      return (
                        <VirtualTurnRow
                          key={virtualRow.key}
                          itemKey={virtualRow.key}
                          top={virtualRow.start}
                          onMeasure={handleVirtualRowMeasure}
                          shouldMeasure={false}
                        >
                          <VirtualTurnPlaceholder height={virtualRow.size} />
                        </VirtualTurnRow>
                      );
                    }
                    const t = virtualRow.turn;
                    const renderedByActiveCorePanel = showActiveCorePanel && activeCoreTurnId === t.turn_id && !activePeerName;
                    const renderedByActivePeerPanel = showActivePeerPanel && activePeerTurnId === t.turn_id && Boolean(activePeerName);
                    const hasAssistantActivity =
                      !renderedByActiveCorePanel &&
                      !renderedByActivePeerPanel &&
                      hasRenderableActivityForTurn(t.turn_id);
                    const turnEvents = eventsByTurnId.get(t.turn_id) ?? EMPTY_EVENT_LIST;
                    const relatedTurns = t.turn_id ? (relatedTurnsByTurnId.get(t.turn_id) ?? [t]) : [t];

                    return (
                      <VirtualTurnRow
                        key={virtualRow.key}
                        itemKey={virtualRow.key}
                        top={virtualRow.start}
                        onMeasure={handleVirtualRowMeasure}
                        shouldMeasure={!isVirtualScrollSeeking}
                      >
                        <HistoricalTurnContent
                          turn={t}
                          originalIndex={virtualRow.originalIndex}
                          lastUserTurnId={lastUserTurnId}
                          editingTurnId={editingTurnId}
                          editingDraft={editingDraft}
                          isGenerating={isGenerating}
                          storedAttachments={turnAttachments[t.turn_id]}
                          hasAssistantActivity={hasAssistantActivity}
                          turnEvents={turnEvents}
                          relatedTurns={relatedTurns}
                          realtimeLines={realtimeTerminalLines[t.turn_id]}
                          hyardJob={t.turn_id ? hyardJobsByTurnId.get(t.turn_id) : undefined}
                          renderMessageBody={renderMessageBody}
                          renderTurnEvents={renderTurnEvents}
                          scopedRenderOptions={scopedRenderOptions}
                          onOpenFile={onOpenFile}
                          onPreviewAttachment={setPreviewAttachment}
                          onBeginEdit={beginEdit}
                          onRetryLastUserTurn={onRetryLastUserTurn}
                          onEditingDraftChange={setEditingDraft}
                          onCommitEdit={commitEdit}
                          onCancelEdit={cancelEdit}
                        />
                      </VirtualTurnRow>
                    );
                  })}
                </div>
              )}
            </>
          )}

        {/* Active Core fallback before the backend reports the canonical turn id. */}
        {showRuntimeDispatchPanel && (
          <div className="message-assistant-flow">
            <div className="message-header">{selectedSession?.active_core ?? 'Core'} (core)</div>
            <div className="message-body live-execution-activity">
              <div className="live-execution-card">
                <div className="live-execution-topline">
                  <span className="live-execution-elapsed">
                    <ChatLiveElapsedLabel startedAt={runtimeDispatchStartedAt} />
                  </span>
                </div>
                <div className="live-execution-divider" />
                <div className="live-execution-section-label">正在建立运行时</div>
                <div className="live-execution-stream-row">
                  <span className="live-execution-stream-icon live-execution-output-icon" aria-hidden="true">•</span>
                  <span className="live-execution-stream-text">
                    {runtimeDispatchPhase || '正在准备并启动 core provider，等待首个流式事件…'}
                  </span>
                </div>
                <div className="live-execution-section-label live-execution-thinking-label">
                  正在思考
                  <span className="spinner-small" aria-label="运行中" />
                </div>
                <div className="live-execution-thinking-text">
                  发送已经进入后端调度队列；若 provider 预热、恢复 persistent instance 或权限握手较慢，这里会持续实时计时。
                </div>
              </div>
            </div>
          </div>
        )}

        {/* Active Core Streaming response container.
            Keep this visible even during tool-only/status-only phases, otherwise
            the user sees a silent wait while runtime events are arriving. */}
        {showActiveCorePanel && (
          <div className="message-assistant-flow">
            <div className="message-header">{selectedSession?.active_core ?? 'Core'} (core)</div>
            {renderedActiveCoreText ? (
              <>
                <RenderedMessageBody
                  text={renderedActiveCoreText}
                  renderMessageBody={renderMessageBody}
                  onOpenFile={onOpenFile}
                />
                {activeCoreTurnId && renderTurnActivitySummary(activeCoreTurnId, activeCoreEvents, activeCoreRelatedTurns, realtimeTerminalLines[activeCoreTurnId], hyardJobs, scopedRenderOptions)}
              </>
            ) : activeCoreTurnId ? (
              renderTurnActivitySummary(activeCoreTurnId, activeCoreEvents, activeCoreRelatedTurns, realtimeTerminalLines[activeCoreTurnId], hyardJobs, scopedRenderOptions)
            ) : (
              <div className="message-body" style={{ display: 'flex', alignItems: 'center', gap: '8px', color: 'var(--text-secondary)' }}>
                <span className="thinking-dots">正在连接 provider，等待首个流式事件…</span>
                <span className="spinner-small"></span>
              </div>
            )}
          </div>
        )}

        {/* Active Peer Streaming delegation box */}
        {showActivePeerPanel && (
          <div
            className="message-bubble message-assistant"
            style={{ alignSelf: 'flex-start', borderLeft: '3px solid var(--color-secondary)', background: 'rgba(6, 182, 212, 0.05)' }}
          >
            <div className="message-header" style={{ color: 'var(--color-secondary)' }}>
              Active Delegation: {activePeerName}
            </div>
            {renderedActivePeerText ? (
              <>
                <RenderedMessageBody
                  text={renderedActivePeerText}
                  renderMessageBody={renderMessageBody}
                  onOpenFile={onOpenFile}
                />
                {activePeerTurnId && renderTurnActivitySummary(activePeerTurnId, activePeerEvents, activePeerRelatedTurns, realtimeTerminalLines[activePeerTurnId], hyardJobs, scopedRenderOptions)}
              </>
            ) : activePeerTurnId ? (
              renderTurnActivitySummary(activePeerTurnId, activePeerEvents, activePeerRelatedTurns, realtimeTerminalLines[activePeerTurnId], hyardJobs, scopedRenderOptions)
            ) : (
              <div className="message-body" style={{ fontStyle: 'italic', display: 'flex', alignItems: 'center', gap: '8px' }}>
                <span className="thinking-dots">正在等待 peer 输出…</span>
                <span className="spinner-small"></span>
              </div>
            )}
          </div>
        )}

        {/* Queued (not-yet-dispatched) user messages */}
        {queuedMessages.length > 0 && (
          <>
            {queuedMessages.map((msg, qIdx) => {
              const queuedImageCount = msg.imagePaths.length;
              const queuedFileCount = msg.filePaths?.length ?? 0;
              const queuedAttachmentCount = queuedImageCount + queuedFileCount;
              const queuedAttachmentLabel = [
                queuedImageCount > 0 ? `${queuedImageCount} image${queuedImageCount === 1 ? '' : 's'}` : null,
                queuedFileCount > 0 ? `${queuedFileCount} file${queuedFileCount === 1 ? '' : 's'}` : null,
              ].filter(Boolean).join(', ');
              return (
                <div
                  key={`queued-${qIdx}`}
                  className="message-bubble message-user"
                  style={{ opacity: 0.55, borderStyle: 'dashed', borderWidth: '1px', borderColor: 'var(--border-muted)' }}
                  title="Queued — will dispatch after the current turn finishes"
                >
                  <div className="message-header" style={{ display: 'flex', justifyContent: 'space-between', alignItems: 'center', gap: '8px' }}>
                    <span>You (queued #{qIdx + 1})</span>
                    {qIdx === 0 && (
                      <button
                        onClick={onClearQueue}
                        title="Discard all queued messages"
                        style={{
                          background: 'transparent',
                          border: '1px solid var(--border-muted)',
                          color: 'var(--text-muted)',
                          fontSize: '11px',
                          padding: '2px 8px',
                          borderRadius: '3px',
                          cursor: 'pointer',
                        }}
                      >
                        Clear queue
                      </button>
                    )}
                  </div>
                  <RenderedMessageBody
                    text={msg.text}
                    renderMessageBody={renderMessageBody}
                    onOpenFile={onOpenFile}
                  />
                  {queuedAttachmentCount > 0 && (
                    <div
                      style={{
                        marginTop: 8,
                        display: 'inline-flex',
                        alignItems: 'center',
                        gap: 6,
                        alignSelf: 'flex-start',
                        background: 'rgba(59, 130, 246, 0.10)',
                        border: '1px solid rgba(59, 130, 246, 0.24)',
                        borderRadius: 999,
                        padding: '3px 8px',
                        color: 'var(--text-secondary)',
                        fontSize: 12,
                      }}
                    >
                      {queuedFileCount > 0 ? <FileText size={12} /> : <ImageIcon size={12} />}
                      <span>{queuedAttachmentLabel} attached</span>
                    </div>
                  )}
                </div>
              );
            })}
          </>
        )}

          <div ref={messagesEndRef} />
        </div>
      </div>

      <ChatComposer
        selectedSession={selectedSession}
        isGenerating={isGenerating}
        handleSend={composerSend}
        handleCancel={composerCancel}
        sandboxMode={sandboxMode}
        onSandboxModeChange={onSandboxModeChange}
        onReady={onComposerReady}
      />
    </div>
  );
};

/// Imperative handle the composer registers so other panes (e.g. the file
/// Explorer's "Add to Chat") can attach context files as removable chips
/// instead of pasting raw text — the standard AI-IDE pattern.
export interface ComposerApi {
  addAttachmentsFromPaths: (paths: string[]) => void;
}

interface ChatComposerProps {
  selectedSession: Session | null;
  isGenerating: boolean;
  handleSend: (payload: SendPayload, restoreText?: (text: string) => void) => void | Promise<void>;
  handleCancel: () => void;
  sandboxMode: SandboxMode;
  onSandboxModeChange: (mode: SandboxMode) => void | Promise<void>;
  onReady?: (api: ComposerApi) => void;
}

/// Keep the hot typing state local to the composer. Previously `inputText`
/// lived in App.tsx, so every keystroke re-rendered the entire app shell and
/// rebuilt the full chat transcript (markdown, execution cards, terminal log
/// summaries, canvas side panes, etc.). Localizing the state makes typing cost
/// proportional to the small composer only; sends still hand the committed text
/// to App for slash-command/session orchestration.
const ChatComposer: React.FC<ChatComposerProps> = React.memo(({
  selectedSession,
  isGenerating,
  handleSend,
  handleCancel,
  sandboxMode,
  onSandboxModeChange,
  onReady,
}) => {
  const [inputText, setInputText] = useState('');
  const [attachments, setAttachments] = useState<InputAttachment[]>([]);
  const [attachmentError, setAttachmentError] = useState<string | null>(null);
  const [isDragOver, setIsDragOver] = useState(false);
  const composerRef = useRef<HTMLDivElement>(null);
  const [permissionsOpen, setPermissionsOpen] = useState(false);
  const permissionsMenuRef = useRef<HTMLDivElement>(null);
  const dragDepthRef = useRef(0);
  const currentSandbox = sandboxOptionFor(sandboxMode);
  const canSubmit = inputText.trim().length > 0 || attachments.length > 0;
  const completions = attachments.length === 0 && inputText.startsWith('/') ? completeSlash(inputText) : [];

  useEffect(() => {
    if (!permissionsOpen) return;
    const onPointerDown = (event: PointerEvent) => {
      if (permissionsMenuRef.current?.contains(event.target as Node)) return;
      setPermissionsOpen(false);
    };
    window.addEventListener('pointerdown', onPointerDown);
    return () => window.removeEventListener('pointerdown', onPointerDown);
  }, [permissionsOpen]);

  const addAttachmentsFromPaths = useCallback((paths: string[]) => {
    const cleanPaths = paths
      .map((path) => (typeof path === 'string' ? path.trim() : ''))
      .filter(Boolean);
    if (cleanPaths.length === 0) return;
    setAttachmentError(null);
    setAttachments((prev) => {
      const seen = new Set(prev.map((attachment) => attachment.path));
      const next = [...prev];
      for (const path of cleanPaths) {
        if (seen.has(path)) continue;
        seen.add(path);
        next.push(attachmentFromPath(path));
      }
      return next;
    });
  }, []);

  // Expose the composer API so other panes (file Explorer "Add to Chat")
  // can stage context files as removable chips here.
  useEffect(() => {
    onReady?.({ addAttachmentsFromPaths });
  }, [onReady, addAttachmentsFromPaths]);

  const addOrPersistNativeAttachmentPaths = useCallback(async (paths: string[]) => {
    const savedPaths: string[] = [];
    for (const rawPath of paths) {
      const path = typeof rawPath === 'string' ? rawPath.trim() : '';
      if (!path) continue;

      if (!looksLikeEphemeralAttachmentPath(path)) {
        savedPaths.push(path);
        continue;
      }

      try {
        const savedPath = await persistAttachmentFile(path, attachmentFromPath(path).mimeType);
        savedPaths.push(savedPath);
      } catch (error) {
        console.error('Failed to persist ephemeral attachment path', error);
        setAttachmentError(`无法持久化临时附件 ${filenameFromPath(path)}：${String(error)}`);
      }
    }

    if (savedPaths.length > 0) {
      addAttachmentsFromPaths(savedPaths);
    }
  }, [addAttachmentsFromPaths]);

  const saveDroppedOrPastedFiles = useCallback(async (
    files: File[],
    options?: { preferNativePath?: boolean },
  ) => {
    const preferNativePath = options?.preferNativePath ?? false;
    const savedPaths: string[] = [];
    for (const file of files) {
      const nativePath = nativePathForFile(file);
      if (preferNativePath && nativePath && !looksLikeEphemeralAttachmentPath(nativePath)) {
        savedPaths.push(nativePath);
        continue;
      }

      try {
        const dataUrl = await readFileAsDataUrl(file);
        const savedPath = await saveClipboardAttachment(
          attachmentNameHintForFile(file, nativePath),
          mimeTypeHintForFile(file, nativePath),
          dataUrl,
        );
        savedPaths.push(savedPath);
      } catch (error) {
        if (nativePath) {
          try {
            const savedPath = await persistAttachmentFile(nativePath, mimeTypeHintForFile(file, nativePath));
            savedPaths.push(savedPath);
            continue;
          } catch (persistError) {
            console.error('Failed to persist native attachment fallback', persistError);
          }
        }
        console.error('Failed to save pasted/dropped attachment', error);
        setAttachmentError(`无法读取或保存附件 ${file.name || nativePath || 'clipboard item'}：${String(error)}`);
      }
    }
    if (savedPaths.length > 0) {
      addAttachmentsFromPaths(savedPaths);
    }
  }, [addAttachmentsFromPaths]);

  const addAttachments = async () => {
    try {
      const selected = await openDialog({
        multiple: true,
      });
      if (!selected) return;
      const paths = Array.isArray(selected) ? selected : [selected];
      addAttachmentsFromPaths(paths.filter((path): path is string => typeof path === 'string'));
    } catch (error) {
      console.error('Failed to open attachment picker', error);
      setAttachmentError(`无法打开附件选择器：${String(error)}`);
    }
  };

  const isPointInsideComposer = useCallback((position?: { x: number; y: number }) => {
    const rect = composerRef.current?.getBoundingClientRect();
    if (!rect || !position) return true;
    const scale = window.devicePixelRatio || 1;
    const x = position.x / scale;
    const y = position.y / scale;
    return x >= rect.left && x <= rect.right && y >= rect.top && y <= rect.bottom;
  }, []);

  useEffect(() => {
    let disposed = false;
    let unlisten: (() => void) | undefined;
    getCurrentWebview()
      .onDragDropEvent((event) => {
        const payload = event.payload;
        if (payload.type === 'enter') {
          if (isPointInsideComposer(payload.position)) {
            setIsDragOver(true);
          }
        } else if (payload.type === 'over') {
          setIsDragOver(isPointInsideComposer(payload.position));
        } else if (payload.type === 'leave') {
          setIsDragOver(false);
        } else if (payload.type === 'drop') {
          const inside = isPointInsideComposer(payload.position);
          setIsDragOver(false);
          dragDepthRef.current = 0;
          if (inside) {
            void addOrPersistNativeAttachmentPaths(payload.paths);
          }
        }
      })
      .then((dispose) => {
        if (disposed) {
          dispose();
        } else {
          unlisten = dispose;
        }
      })
      .catch((error) => {
        console.error('Failed to listen for native drag/drop events', error);
      });
    return () => {
      disposed = true;
      unlisten?.();
    };
  }, [addOrPersistNativeAttachmentPaths, isPointInsideComposer]);

  const handlePaste = useCallback((event: React.ClipboardEvent<HTMLTextAreaElement>) => {
    const files = Array.from(event.clipboardData?.files ?? []);
    if (files.length === 0) return;
    event.preventDefault();
    void saveDroppedOrPastedFiles(files, { preferNativePath: false });
  }, [saveDroppedOrPastedFiles]);

  const handleDragEnter = useCallback((event: React.DragEvent<HTMLDivElement>) => {
    if (!Array.from(event.dataTransfer.types).includes('Files')) return;
    event.preventDefault();
    dragDepthRef.current += 1;
    setIsDragOver(true);
  }, []);

  const handleDragOver = useCallback((event: React.DragEvent<HTMLDivElement>) => {
    if (!Array.from(event.dataTransfer.types).includes('Files')) return;
    event.preventDefault();
    event.dataTransfer.dropEffect = 'copy';
    setIsDragOver(true);
  }, []);

  const handleDragLeave = useCallback((event: React.DragEvent<HTMLDivElement>) => {
    if (!Array.from(event.dataTransfer.types).includes('Files')) return;
    event.preventDefault();
    dragDepthRef.current = Math.max(0, dragDepthRef.current - 1);
    if (dragDepthRef.current === 0) {
      setIsDragOver(false);
    }
  }, []);

  const handleDrop = useCallback((event: React.DragEvent<HTMLDivElement>) => {
    if (!Array.from(event.dataTransfer.types).includes('Files')) return;
    event.preventDefault();
    dragDepthRef.current = 0;
    setIsDragOver(false);
    const files = Array.from(event.dataTransfer.files ?? []);
    if (files.length > 0) {
      void saveDroppedOrPastedFiles(files, { preferNativePath: true });
    }
  }, [saveDroppedOrPastedFiles]);

  const removeAttachment = (path: string) => {
    setAttachments((prev) => prev.filter((attachment) => attachment.path !== path));
  };

  const submit = () => {
    if (!canSubmit) return;
    const currentAttachments = attachments;
    const imagePaths = currentAttachments
      .filter((attachment) => attachment.kind === 'image')
      .map((attachment) => attachment.path);
    const filePaths = currentAttachments
      .filter((attachment) => attachment.kind !== 'image')
      .map((attachment) => attachment.path);
    const text = inputText.trim() || (filePaths.length > 0 ? '请分析这些附件。' : '请分析这些图片。');
    const payload: SendPayload = {
      text,
      imagePaths,
      filePaths,
      attachments: currentAttachments,
    };
    setInputText('');
    setAttachments([]);
    setAttachmentError(null);
    void handleSend(payload, (restoredText) => {
      setInputText(restoredText);
      setAttachments(currentAttachments);
    });
  };

  return (
    <div className="chat-input-container" style={{ position: 'relative' }}>
      {/* Slash-command completion popover. Surfaces when the input
          starts with `/`. Filtered by prefix; clicking an entry
          replaces the input with the matched usage stub so the user
          can fill in args. */}
      {completions.length > 0 && (
        <div
          role="listbox"
          style={{
            position: 'absolute',
            bottom: '100%',
            left: 12,
            right: 12,
            marginBottom: 6,
            background: 'rgba(15, 17, 22, 0.98)',
            border: '1px solid var(--border-muted)',
            borderRadius: 6,
            boxShadow: '0 4px 12px rgba(0, 0, 0, 0.5)',
            maxHeight: 240,
            overflow: 'auto',
            zIndex: 10,
          }}
        >
          {completions.map((c) => (
            <div
              key={c.name}
              onClick={() => setInputText(c.usage + ' ')}
              onMouseDown={(e) => e.preventDefault()}
              style={{
                padding: '6px 12px',
                cursor: 'pointer',
                fontSize: 12,
                display: 'flex',
                flexDirection: 'column',
                gap: 2,
                color: 'var(--text-primary)',
              }}
              onMouseEnter={(e) =>
                (e.currentTarget.style.background = 'rgba(99, 102, 241, 0.12)')
              }
              onMouseLeave={(e) => (e.currentTarget.style.background = 'transparent')}
            >
              <code style={{ color: 'var(--color-primary)' }}>{c.usage}</code>
              <span style={{ color: 'var(--text-muted)' }}>{c.description}</span>
            </div>
          ))}
        </div>
      )}
      <div
        ref={composerRef}
        className="input-wrapper"
        onDragEnter={handleDragEnter}
        onDragOver={handleDragOver}
        onDragLeave={handleDragLeave}
        onDrop={handleDrop}
        style={{
          flexDirection: 'column',
          alignItems: 'stretch',
          gap: 8,
          borderRadius: 10,
          padding: 10,
          position: 'relative',
          borderColor: isDragOver ? 'rgba(59, 130, 246, 0.75)' : undefined,
          background: isDragOver ? 'rgba(59, 130, 246, 0.08)' : undefined,
          boxShadow: isDragOver ? '0 0 0 1px rgba(59, 130, 246, 0.22), 0 0 18px rgba(59, 130, 246, 0.14)' : undefined,
        }}
      >
        {isDragOver && (
          <div
            style={{
              position: 'absolute',
              inset: 6,
              border: '1px dashed rgba(96, 165, 250, 0.85)',
              borderRadius: 8,
              display: 'flex',
              alignItems: 'center',
              justifyContent: 'center',
              background: 'rgba(15, 23, 42, 0.72)',
              color: '#bfdbfe',
              fontSize: 12,
              fontWeight: 700,
              letterSpacing: 0.2,
              pointerEvents: 'none',
              zIndex: 4,
            }}
          >
            松开以附加文件或图片
          </div>
        )}
        {attachments.length > 0 && (
          <div style={{ display: 'flex', gap: 6, flexWrap: 'wrap' }}>
            {attachments.map((attachment) => (
              <div
                key={attachment.path}
                title={attachment.path}
                style={{
                  display: 'inline-flex',
                  alignItems: 'center',
                  gap: 6,
                  maxWidth: 220,
                  background: 'rgba(59, 130, 246, 0.10)',
                  border: '1px solid rgba(59, 130, 246, 0.24)',
                  borderRadius: 999,
                  padding: '4px 6px 4px 8px',
                  color: 'var(--text-secondary)',
                  fontSize: 12,
                }}
              >
                {attachment.kind === 'image' ? (
                  <ImageIcon size={13} style={{ color: 'var(--color-primary)', flex: '0 0 auto' }} />
                ) : (
                  <FileText size={13} style={{ color: 'var(--color-primary)', flex: '0 0 auto' }} />
                )}
                <span
                  style={{
                    overflow: 'hidden',
                    textOverflow: 'ellipsis',
                    whiteSpace: 'nowrap',
                  }}
                >
                  {attachment.name}
                </span>
                <button
                  type="button"
                  onClick={() => removeAttachment(attachment.path)}
                  title="Remove attachment"
                  style={{
                    display: 'inline-flex',
                    alignItems: 'center',
                    justifyContent: 'center',
                    width: 18,
                    height: 18,
                    border: 'none',
                    borderRadius: 999,
                    background: 'rgba(255, 255, 255, 0.08)',
                    color: 'var(--text-muted)',
                    cursor: 'pointer',
                    padding: 0,
                  }}
                >
                  <X size={12} />
                </button>
              </div>
            ))}
          </div>
        )}
        {attachmentError && (
          <div
            style={{
              color: '#fca5a5',
              fontSize: 11,
              lineHeight: 1.35,
              background: 'rgba(239, 68, 68, 0.08)',
              border: '1px solid rgba(239, 68, 68, 0.22)',
              borderRadius: 6,
              padding: '5px 8px',
            }}
          >
            {attachmentError}
          </div>
        )}

        <div>
          {/* Textarea is always enabled — sending with no selected
              session triggers an auto-create in App.tsx's handleSend
              (mints a new session against the current core provider
              and dispatches the message as its first turn). The
              placeholder hints at the auto-create behavior so the
              user knows pressing Enter is safe. */}
          <textarea
            className="chat-textarea"
            placeholder={
              !selectedSession
                ? "Ask anything… attach files/images with +, paste, or drop; a new session will be created on send"
                : "Ask anything… attach files/images with +, paste, or drop (type '/' for commands)"
            }
            value={inputText}
            onChange={(e) => setInputText(e.target.value)}
            onPaste={handlePaste}
            onKeyDown={(e) => {
              if (e.key === 'Enter' && !e.shiftKey) {
                e.preventDefault();
                submit();
              }
            }}
            style={{
              width: '100%',
              minHeight: 54,
              lineHeight: 1.5,
            }}
          />
        </div>

        <div style={{ display: 'flex', justifyContent: 'space-between', alignItems: 'center', gap: 8 }}>
          <div style={{ display: 'flex', alignItems: 'center', gap: 8, minWidth: 0 }}>
            <button
              type="button"
              onClick={addAttachments}
              title="Attach files or images"
              style={{
                display: 'inline-flex',
                alignItems: 'center',
                justifyContent: 'center',
                width: 30,
                height: 30,
                borderRadius: 999,
                border: '1px solid var(--border-muted)',
                background: 'rgba(255, 255, 255, 0.04)',
                color: 'var(--text-secondary)',
                cursor: 'pointer',
                flex: '0 0 auto',
              }}
            >
              <Plus size={16} />
            </button>

            <div ref={permissionsMenuRef} style={{ position: 'relative' }}>
              <button
                type="button"
                onClick={() => setPermissionsOpen((open) => !open)}
                title="Quick sandbox permission mode"
                style={{
                  display: 'inline-flex',
                  alignItems: 'center',
                  gap: 6,
                  minHeight: 30,
                  borderRadius: 999,
                  border: `1px solid ${currentSandbox.border}`,
                  background: currentSandbox.background,
                  color: currentSandbox.accent,
                  padding: '5px 10px',
                  fontSize: 12,
                  fontWeight: 600,
                  cursor: 'pointer',
                  whiteSpace: 'nowrap',
                }}
              >
                <span>{currentSandbox.label}</span>
                <ChevronDown size={13} />
              </button>
              {permissionsOpen && (
                <div
                  style={{
                    position: 'absolute',
                    left: 0,
                    bottom: 'calc(100% + 8px)',
                    width: 280,
                    background: 'rgba(15, 17, 22, 0.98)',
                    border: '1px solid var(--border-muted)',
                    borderRadius: 10,
                    boxShadow: '0 12px 32px rgba(0, 0, 0, 0.45)',
                    padding: 6,
                    zIndex: 20,
                  }}
                >
                  {SANDBOX_OPTIONS.map((option) => {
                    const active = option.mode === sandboxMode;
                    return (
                      <button
                        key={option.mode}
                        type="button"
                        onClick={() => {
                          setPermissionsOpen(false);
                          void onSandboxModeChange(option.mode);
                        }}
                        style={{
                          width: '100%',
                          textAlign: 'left',
                          display: 'flex',
                          flexDirection: 'column',
                          gap: 3,
                          border: active ? `1px solid ${option.border}` : '1px solid transparent',
                          borderRadius: 8,
                          background: active ? option.background : 'transparent',
                          color: 'var(--text-primary)',
                          padding: '8px 10px',
                          cursor: 'pointer',
                        }}
                      >
                        <span style={{ color: option.accent, fontWeight: 700, fontSize: 12 }}>{option.label}</span>
                        <span style={{ color: 'var(--text-muted)', fontSize: 11, lineHeight: 1.35 }}>{option.description}</span>
                      </button>
                    );
                  })}
                </div>
              )}
            </div>
          </div>
          <div style={{ display: 'flex', alignItems: 'center', gap: 8, flex: '0 0 auto' }}>
            {attachments.length > 0 && (
              <span style={{ color: 'var(--text-muted)', fontSize: 11, whiteSpace: 'nowrap' }}>
                {attachments.length} attachment{attachments.length === 1 ? '' : 's'} ready
              </span>
            )}
            {/* Single action button toggles between Send (idle) and Stop
                (generating). It lives in the bottom toolbar so the right edge
                no longer shows a tall detached button beside the textarea. */}
            {isGenerating ? (
              <button
                type="button"
                className="btn-send composer-send-button btn-stop"
                onClick={handleCancel}
                title="Stop current execution"
                style={{
                  background: 'rgba(239, 68, 68, 0.15)',
                  color: '#ef4444',
                  border: '1px solid rgba(239, 68, 68, 0.4)',
                }}
              >
                <Square size={14} fill="currentColor" />
              </button>
            ) : (
              <button
                type="button"
                className="btn-send composer-send-button"
                onClick={submit}
                disabled={!canSubmit}
                title="Send (Enter) — creates a new session when none is selected"
              >
                <Send size={16} />
              </button>
            )}
          </div>
        </div>
      </div>
    </div>
  );
});

export default ChatArea;
