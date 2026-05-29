import { Suspense, lazy, useState, useEffect, useRef, useCallback, useMemo } from 'react';
import type { CSSProperties, PointerEvent as ReactPointerEvent } from 'react';
import { unstable_batchedUpdates } from 'react-dom';
import { invoke } from '@tauri-apps/api/core';
import { listen } from '@tauri-apps/api/event';
import { open as openDialog } from '@tauri-apps/plugin-dialog';
import type {
  SwitchyardConfig,
  Session,
  Turn,
  TelemetryLog,
  ProviderStatus,
  ProviderConfig,
  InstanceMetadata,
  Workspace,
  SandboxMode,
  SendPayload,
  InputAttachment,
} from './types';
import { Sidebar } from './components/Sidebar';
import { IconRail, type RailMode } from './components/IconRail';
import { AppTopBar } from './components/AppTopBar';
import { WelcomeWorkspace } from './components/WelcomeWorkspace';
import { ChatArea } from './components/ChatArea';
import type { CanvasMode, CanvasTab } from './components/Canvas';
import { fetchSnapshot, saveFile } from './components/canvasApi';
import { StatusBar } from './components/StatusBar';
import { parseSlash, type SlashContext } from './components/slashCommands';
// ArtifactDrawer is no longer rendered — its bottom-bar UX didn't fit
// the new layout. The import + state are dropped along with the bar.
import { renderMessageBody, isSystemStatusText, renderTurnEvents, renderTurnActivitySummary } from './components/ui/RenderHelpers';
import type { RuntimeTurnPhase, RenderTurnEventsOptions } from './components/ui/RenderHelpers';
import { resolveToolApproval } from './services/api';
import {
  attachmentFromPath,
  extractAttachmentsFromAttachmentReferences,
  extractFilePathsFromAttachmentReferences,
  extractImagePathsFromAttachmentReferences,
  mergeInputAttachments,
  stripAttachmentReferences,
} from './utils/attachments';
import {
  fallbackResponseForUserMessage,
  mergeFallbackResponseIntoTurns,
  mergeFinalResponseIntoTurns,
  mergeFreshTurnsPreservingKnownResponses,
  nonBlankText,
} from './utils/turnMerge';

const Canvas = lazy(() => import('./components/Canvas'));
const ControlCenter = lazy(() => import('./components/ControlCenter'));
const FilesTree = lazy(() => import('./components/FilesTree'));
const SettingsModal = lazy(() => import('./components/SettingsModal'));
const SourceControl = lazy(() => import('./components/SourceControl'));
const TerminalPanel = lazy(() => import('./components/TerminalPanel'));
const TopologyOverlay = lazy(() => import('./components/TopologyOverlay'));

const RUNTIME_ITEM_EVENT_FALLBACK = 'item_updated';
const MAX_REALTIME_TERMINAL_LINES = 1000;
const MAX_REALTIME_TERMINAL_CHARS = 200_000;
const DEFAULT_LEFT_COLUMN_WIDTH = 280;
const MIN_LEFT_COLUMN_WIDTH = 220;
const MAX_LEFT_COLUMN_WIDTH = 460;
const MIN_CHAT_COLUMN_WIDTH = 380;
const MIN_CANVAS_COLUMN_WIDTH = 360;
const DEFAULT_CANVAS_COLUMN_WIDTH = 680;
const DEFAULT_SANDBOX_MODE: SandboxMode = 'workspace-write';
const TURN_ATTACHMENTS_STORAGE_KEY = 'switchyard.turnAttachments.v1';
const TEMP_USER_TURN_ID_PREFIX = 'temp-user-';
const NEW_SESSION_PIPELINE_ID = '__switchyard_new_session__';
const CACHED_SESSION_TURN_REFRESH_IDLE_TIMEOUT_MS = 700;

interface RuntimeSnapshot {
  max_event_id?: number;
  host_jobs?: any[];
  events?: any[];
}

interface RuntimeEventRecord {
  event_id?: number;
  workspace_id?: string | null;
  session_id?: string | null;
  aggregate_type?: string;
  aggregate_id?: string;
  aggregate_version?: number;
  event_type?: string;
  payload?: any;
  occurred_at?: string;
  source?: string;
}

const RUNTIME_TURN_SESSION_CACHE_MAX_ENTRIES = 2_048;
const RUNTIME_TURN_SESSION_CACHE_TTL_MS = 6 * 60 * 60 * 1000;

interface RuntimeTurnSessionCacheEntry {
  sessionId: string;
  lastSeenAt: number;
}

interface PendingTurnAttachmentBinding {
  bindingId: string;
  sessionId: string;
  text: string;
  attachments: InputAttachment[];
  beforeTurnIds: string[];
  tempTurnId?: string;
  createdAt: number;
  resolvedTurnId?: string;
}

interface SessionUiSnapshot {
  turns: Turn[];
  sessionEvents: any[];
  sessionWorkers: InstanceMetadata[];
  activeCoreText: string;
  activePeerText: string;
  activePeerName: string | null;
  activeNodes: string[];
  activeTurnIds: string[];
  activeCoreTurnId: string | null;
  activePeerTurnId: string | null;
  selectedAgentTurnId: string | null;
  hyardJobs: Record<string, any>;
  realtimeTerminalLines: Record<string, string[]>;
  realtimeTerminalBuffers: Record<string, string>;
  runtimeTurnPhases: Record<string, RuntimeTurnPhase>;
  runtimeTurnStartedAt: Record<string, number>;
  runtimeTurnPhaseChangedAt: Record<string, number>;
  runtimeDispatchStartedAt: number | null;
  runtimePreparingPhase: string | null;
  isGenerating: boolean;
  messageQueue: SendPayload[];
  loaded: boolean;
  loadedAt: number;
}

function createEmptySessionUiSnapshot(): SessionUiSnapshot {
  return {
    turns: [],
    sessionEvents: [],
    sessionWorkers: [],
    activeCoreText: '',
    activePeerText: '',
    activePeerName: null,
    activeNodes: [],
    activeTurnIds: [],
    activeCoreTurnId: null,
    activePeerTurnId: null,
    selectedAgentTurnId: null,
    hyardJobs: {},
    realtimeTerminalLines: {},
    realtimeTerminalBuffers: {},
    runtimeTurnPhases: {},
    runtimeTurnStartedAt: {},
    runtimeTurnPhaseChangedAt: {},
    runtimeDispatchStartedAt: null,
    runtimePreparingPhase: null,
    isGenerating: false,
    messageQueue: [],
    loaded: false,
    loadedAt: 0,
  };
}

function sessionUiSnapshotWithRuntimePhase(
  snapshot: SessionUiSnapshot,
  turnId: unknown,
  phase: RuntimeTurnPhase,
  options?: { ensureOnly?: boolean; now?: number },
): SessionUiSnapshot {
  if (typeof turnId !== 'string' || !turnId) return snapshot;
  const previousPhase = snapshot.runtimeTurnPhases[turnId];
  if (options?.ensureOnly && previousPhase) return snapshot;
  const now = options?.now ?? Date.now();
  const nextStartedAt =
    phase === 'running' || phase === 'output_completed' || phase === 'finalizing'
      ? (snapshot.runtimeTurnStartedAt[turnId] ?? now)
      : snapshot.runtimeTurnStartedAt[turnId];
  return {
    ...snapshot,
    runtimeTurnPhases: {
      ...snapshot.runtimeTurnPhases,
      [turnId]: previousPhase ?? phase,
      ...(options?.ensureOnly ? {} : { [turnId]: phase }),
    },
    runtimeTurnPhaseChangedAt: {
      ...snapshot.runtimeTurnPhaseChangedAt,
      [turnId]: options?.ensureOnly
        ? (snapshot.runtimeTurnPhaseChangedAt[turnId] ?? now)
        : now,
    },
    runtimeTurnStartedAt: nextStartedAt
      ? { ...snapshot.runtimeTurnStartedAt, [turnId]: nextStartedAt }
      : snapshot.runtimeTurnStartedAt,
  };
}

function sessionUiSnapshotSeedStartedAt(
  snapshot: SessionUiSnapshot,
  turnId: unknown,
  startedAt: number | null,
): SessionUiSnapshot {
  if (typeof turnId !== 'string' || !turnId || !startedAt || snapshot.runtimeTurnStartedAt[turnId]) {
    return snapshot;
  }
  return {
    ...snapshot,
    runtimeTurnStartedAt: {
      ...snapshot.runtimeTurnStartedAt,
      [turnId]: startedAt,
    },
  };
}

function sessionUiSnapshotEnsureTerminal(
  snapshot: SessionUiSnapshot,
  turnId: unknown,
): SessionUiSnapshot {
  if (typeof turnId !== 'string' || !turnId) return snapshot;
  if (
    Object.prototype.hasOwnProperty.call(snapshot.realtimeTerminalBuffers, turnId) &&
    Object.prototype.hasOwnProperty.call(snapshot.realtimeTerminalLines, turnId)
  ) {
    return snapshot;
  }
  return {
    ...snapshot,
    realtimeTerminalBuffers: Object.prototype.hasOwnProperty.call(snapshot.realtimeTerminalBuffers, turnId)
      ? snapshot.realtimeTerminalBuffers
      : { ...snapshot.realtimeTerminalBuffers, [turnId]: '' },
    realtimeTerminalLines: Object.prototype.hasOwnProperty.call(snapshot.realtimeTerminalLines, turnId)
      ? snapshot.realtimeTerminalLines
      : { ...snapshot.realtimeTerminalLines, [turnId]: [] },
  };
}

function sessionUiSnapshotResetTerminal(
  snapshot: SessionUiSnapshot,
  turnId: unknown,
): SessionUiSnapshot {
  if (typeof turnId !== 'string' || !turnId) return snapshot;
  return {
    ...snapshot,
    realtimeTerminalBuffers: {
      ...snapshot.realtimeTerminalBuffers,
      [turnId]: '',
    },
    realtimeTerminalLines: {
      ...snapshot.realtimeTerminalLines,
      [turnId]: [],
    },
  };
}

function sessionUiSnapshotAppendTerminalText(
  snapshot: SessionUiSnapshot,
  turnId: unknown,
  text: unknown,
): SessionUiSnapshot {
  if (typeof turnId !== 'string' || !turnId || typeof text !== 'string' || text.length === 0) {
    return snapshot;
  }
  const nextText = trimRealtimeTextBuffer(
    `${snapshot.realtimeTerminalBuffers[turnId] || ''}${normalizeRealtimeText(text)}`,
  );
  return {
    ...snapshot,
    realtimeTerminalBuffers: {
      ...snapshot.realtimeTerminalBuffers,
      [turnId]: nextText,
    },
    realtimeTerminalLines: {
      ...snapshot.realtimeTerminalLines,
      [turnId]: realtimeTextToLines(nextText),
    },
  };
}

function LazyPanelFallback({ label, minHeight = 96 }: { label: string; minHeight?: number }) {
  return (
    <div
      style={{
        minHeight,
        height: '100%',
        display: 'flex',
        alignItems: 'center',
        justifyContent: 'center',
        padding: 16,
        color: 'var(--text-muted)',
        fontSize: 12,
      }}
    >
      {label}
    </div>
  );
}

function truthyDebugFlag(value: unknown): boolean {
  return ['1', 'true', 'yes', 'on'].includes(String(value ?? '').trim().toLowerCase());
}

function debugRuntimeEventsEnabled(): boolean {
  if (typeof window === 'undefined') return false;
  try {
    return truthyDebugFlag(window.localStorage.getItem('switchyard.debugRuntimeEvents'));
  } catch {
    return false;
  }
}

function runtimeEventDebugSummary(type: string, data: any) {
  const payload = data?.payload;
  return {
    type,
    turn_id: data?.turn_id,
    provider: data?.provider,
    event_type: data?.event_type,
    text_len: typeof data?.text === 'string' ? data.text.length : 0,
    payload_item_type: payload ? runtimeItemType(payload) : '',
    payload_method: firstRuntimeIdentity(payload?.method, payload?.params?.method) || '',
  };
}

function leafName(path: string): string {
  if (!path) return '';
  const normalised = path.replace(/\\/g, '/').replace(/\/+$/, '');
  const idx = normalised.lastIndexOf('/');
  return idx >= 0 ? normalised.slice(idx + 1) : normalised;
}

function normalizePathKey(path: string): string {
  const normalised = path.replace(/\\/g, '/').replace(/\/+$/, '');
  // Windows drive-letter and UNC paths should be treated case-insensitively
  // when de-duping workspace roots; POSIX paths keep their case.
  if (/^[A-Za-z]:\//.test(normalised) || normalised.startsWith('//')) {
    return normalised.toLowerCase();
  }
  return normalised;
}

function normalizeSendPayload(input: string | SendPayload): SendPayload {
  if (typeof input === 'string') {
    return { text: input, imagePaths: [], filePaths: [] };
  }
  const attachments = Array.isArray(input.attachments)
    ? mergeInputAttachments(input.attachments)
    : [];
  const imagePaths = Array.isArray(input.imagePaths) && input.imagePaths.length > 0
    ? input.imagePaths
    : attachments.filter((attachment) => attachment.kind === 'image').map((attachment) => attachment.path);
  const filePaths = Array.isArray(input.filePaths) && input.filePaths.length > 0
    ? input.filePaths
    : attachments.filter((attachment) => attachment.kind !== 'image').map((attachment) => attachment.path);
  return {
    text: input.text,
    imagePaths,
    filePaths,
    attachments: attachments.length > 0 ? attachments : input.attachments,
  };
}

function describeSendPayload(payload: SendPayload): string {
  const imageCount = payload.imagePaths.length;
  const fileCount = payload.filePaths?.length ?? 0;
  if (imageCount === 0 && fileCount === 0) return payload.text;
  const parts = [
    imageCount > 0 ? `${imageCount} image${imageCount === 1 ? '' : 's'}` : null,
    fileCount > 0 ? `${fileCount} file${fileCount === 1 ? '' : 's'}` : null,
  ].filter(Boolean);
  const fallback = fileCount > 0 ? 'Attachment message' : 'Image message';
  return `${payload.text || fallback} (${parts.join(', ')})`;
}

function attachmentsFromPayload(payload: SendPayload): InputAttachment[] {
  const explicit = Array.isArray(payload.attachments)
    ? mergeInputAttachments(payload.attachments)
    : [];
  if (explicit.length > 0) return explicit;
  return mergeInputAttachments(
    payload.imagePaths.map((path) => attachmentFromPath(path)),
    (payload.filePaths ?? []).map((path) => attachmentFromPath(path)),
    extractAttachmentsFromAttachmentReferences(payload.text),
  );
}

function normalizedAttachmentMatchText(text: string): string {
  return stripAttachmentReferences(text).trim();
}

function createTempUserTurnId(sessionId: string): string {
  const random = Math.random().toString(36).slice(2, 10);
  return `${TEMP_USER_TURN_ID_PREFIX}${sessionId}-${Date.now()}-${random}`;
}

function turnIdsBeforeSend(turns: Turn[], sessionId: string): string[] {
  return turns
    .filter((turn) => (
      turn.session_id === sessionId &&
      !turn.turn_id.startsWith(TEMP_USER_TURN_ID_PREFIX)
    ))
    .map((turn) => turn.turn_id);
}

function findMatchingAttachmentTurn(
  turnList: Turn[],
  binding: PendingTurnAttachmentBinding,
): Turn | null {
  if (binding.resolvedTurnId) {
    const resolved = turnList.find((turn) => (
      turn.turn_id === binding.resolvedTurnId &&
      turn.session_id === binding.sessionId
    ));
    if (resolved) return resolved;
  }

  const targetText = normalizedAttachmentMatchText(binding.text);
  const beforeIds = new Set(binding.beforeTurnIds);
  const candidates = turnList
    .filter((turn) => (
      turn.session_id === binding.sessionId &&
      turn.origin === 'user' &&
      turn.turn_id !== binding.tempTurnId
    ))
    .reverse();

  return (
    candidates.find((turn) => (
      !beforeIds.has(turn.turn_id) &&
      normalizedAttachmentMatchText(turn.user_message) === targetText
    )) ??
    candidates.find((turn) => !beforeIds.has(turn.turn_id)) ??
    candidates.find((turn) => normalizedAttachmentMatchText(turn.user_message) === targetText) ??
    null
  );
}

function findFreshlySentTurn(
  turnList: Turn[],
  sessionId: string,
  message: string,
  beforeTurnIds: string[],
  tempTurnId?: string,
): Turn | null {
  const targetText = normalizedAttachmentMatchText(message);
  const beforeIds = new Set(beforeTurnIds);
  const candidates = turnList
    .filter((turn) => (
      turn.session_id === sessionId &&
      turn.origin === 'user' &&
      turn.turn_id !== tempTurnId &&
      !turn.turn_id.startsWith(TEMP_USER_TURN_ID_PREFIX)
    ))
    .reverse();

  return (
    candidates.find((turn) => (
      !beforeIds.has(turn.turn_id) &&
      normalizedAttachmentMatchText(turn.user_message) === targetText
    )) ??
    candidates.find((turn) => !beforeIds.has(turn.turn_id)) ??
    candidates.find((turn) => normalizedAttachmentMatchText(turn.user_message) === targetText) ??
    null
  );
}

function readStoredTurnAttachments(): Record<string, InputAttachment[]> {
  if (typeof window === 'undefined') return {};
  try {
    const parsed = JSON.parse(window.localStorage.getItem(TURN_ATTACHMENTS_STORAGE_KEY) || '{}');
    if (!parsed || typeof parsed !== 'object') return {};
    const normalized: Record<string, InputAttachment[]> = {};
    for (const [turnId, value] of Object.entries(parsed)) {
      if (!Array.isArray(value)) continue;
      const attachments = mergeInputAttachments(value as InputAttachment[]);
      if (attachments.length > 0) normalized[turnId] = attachments;
    }
    return normalized;
  } catch {
    return {};
  }
}

function runtimePayloadItem(payload: any): any {
  return (
    payload?.item ||
    payload?.params?.item ||
    payload?.event?.item ||
    payload?.msg?.item ||
    payload?.message?.item ||
    payload?.data?.item ||
    payload?.params ||
    payload?.event ||
    payload?.msg ||
    payload ||
    {}
  );
}

function firstRuntimeIdentity(...values: any[]): string | undefined {
  const found = values.find((value) => value !== undefined && value !== null && String(value).length > 0);
  return found === undefined ? undefined : String(found);
}

function runtimeItemIdentity(payload: any): string | undefined {
  const item = runtimePayloadItem(payload);
  return firstRuntimeIdentity(
    item?.id,
    payload?.id,
    item?.call_id,
    payload?.call_id,
    item?.tool_call_id,
    payload?.tool_call_id,
    item?.request_id,
    payload?.request_id,
    item?.command,
    payload?.command,
    item?.execution?.actual_display,
    item?.execution?.actual_command,
    item?.execution?.resolved_command,
    item?.execution?.original_command,
    payload?.execution?.actual_display,
    payload?.execution?.actual_command,
    payload?.execution?.resolved_command,
    payload?.execution?.original_command,
  );
}

function executionDisplay(execution: any): string {
  return firstRuntimeIdentity(
    execution?.actual_display,
    execution?.actual_command,
    execution?.resolved_command,
    execution?.original_command,
    execution?.command,
  ) || 'unknown command';
}

function normalizeRuntimeEventType(value: any): string {
  const text = value === undefined || value === null ? '' : String(value).trim();
  if (!text) return '';
  return text
    .replace(/([a-z0-9])([A-Z])/g, '$1_$2')
    .replace(/[./\s-]+/g, '_')
    .toLowerCase();
}

const KNOWN_WORKER_STATES = new Set<InstanceMetadata['state']>([
  'spawning',
  'idle',
  'busy',
  'retrying',
  'dying',
  'dead',
]);

const ACTIVE_HYARD_JOB_STATUSES = new Set(['queued', 'worker_booting', 'running', 'cancel_requested', 'wait_timeout']);

function normalizedString(value: any): string | null {
  if (value === undefined || value === null) return null;
  const text = String(value).trim();
  return text.length > 0 ? text : null;
}

function normalizeWorkerKind(value: any): InstanceMetadata['kind'] {
  return normalizeRuntimeEventType(value) === 'core' ? 'core' : 'worker';
}

function normalizeWorkerState(value: any): InstanceMetadata['state'] {
  const state = normalizeRuntimeEventType(value);
  if (KNOWN_WORKER_STATES.has(state as InstanceMetadata['state'])) {
    return state as InstanceMetadata['state'];
  }
  return 'idle';
}

function normalizeInstanceMetadata(raw: any): InstanceMetadata | null {
  if (!raw || typeof raw !== 'object') return null;
  const instanceId = normalizedString(raw.instance_id ?? raw.instanceId ?? raw.id);
  const provider = normalizedString(raw.provider);
  const sessionId = normalizedString(raw.session_id ?? raw.sessionId);
  if (!instanceId || !provider || !sessionId) return null;

  return {
    instance_id: instanceId,
    provider,
    session_id: sessionId,
    label: normalizedString(raw.label),
    kind: normalizeWorkerKind(raw.kind),
    spawned_at: normalizedString(raw.spawned_at ?? raw.spawnedAt) ?? new Date().toISOString(),
    state: normalizeWorkerState(raw.state),
    in_flight_turn_id: normalizedString(raw.in_flight_turn_id ?? raw.inFlightTurnId),
  };
}

function normalizeInstanceMetadataList(rawList: any): InstanceMetadata[] {
  if (!Array.isArray(rawList)) return [];
  return rawList
    .map((item) => normalizeInstanceMetadata(item))
    .filter((item): item is InstanceMetadata => item !== null);
}

function upsertInstanceMetadata(list: InstanceMetadata[], next: InstanceMetadata): InstanceMetadata[] {
  const index = list.findIndex((item) => item.instance_id === next.instance_id);
  if (index === -1) return [...list, next];
  const clone = list.slice();
  clone[index] = { ...clone[index], ...next };
  return clone;
}

function activeHyardJobStatus(job: any): string {
  const liveStatus = normalizeRuntimeEventType(job?.job_status ?? job?.live_status);
  if (liveStatus) return liveStatus;
  return normalizeRuntimeEventType(job?.status);
}

function isActiveHyardJob(job: any): boolean {
  return ACTIVE_HYARD_JOB_STATUSES.has(activeHyardJobStatus(job));
}

function countActiveHyardJobs(jobs: Record<string, any>): number {
  const seen = new Set<string>();
  let count = 0;
  Object.values(jobs).forEach((job, index) => {
    if (!isActiveHyardJob(job)) return;
    const key = normalizedString(job?.job_id ?? job?.id) ?? `anonymous-${index}`;
    if (seen.has(key)) return;
    seen.add(key);
    count += 1;
  });
  return count;
}

function hyardJobRecordFromRuntimeEvent(data: any): any | null {
  const source = data?.job;
  const jobId = normalizedString(source?.job_id ?? source?.id);
  if (!source || !jobId) return null;

  const record: Record<string, any> = {
    ...source,
    job_id: jobId,
  };
  const observedAt = normalizedString(data?.observed_at ?? source?.observed_at);
  const turnId = normalizedString(data?.turn_id ?? source?.turn_id);
  const sessionId = normalizedString(data?.session_id ?? source?.session_id);
  const sourceProvider = normalizedString(data?.source_provider ?? source?.source_provider);
  if (observedAt) record.observed_at = observedAt;
  if (turnId) record.turn_id = turnId;
  if (sessionId) record.session_id = sessionId;
  if (sourceProvider) record.source_provider = sourceProvider;
  return record;
}

function hyardJobRecordFromRuntimeHostJob(source: any): any | null {
  const jobId = normalizedString(source?.job_id ?? source?.id);
  if (!source || !jobId) return null;

  const sessionId = normalizedString(
    source?.session_id ??
    source?.owner_session_id ??
    source?.callback_session_id ??
    source?.worker_session_id,
  );
  const observedAt = normalizedString(
    source?.observed_at ??
    source?.updated_at ??
    source?.started_at ??
    source?.created_at,
  );
  const turnId = normalizedString(source?.turn_id ?? source?.turnId);
  const record: Record<string, any> = {
    ...source,
    job_id: jobId,
    // The older live HYARD UI shape used `session_id` and `observed_at`.
    // Keep those aliases so cards restored from the SQLite runtime snapshot
    // render through the same reducer/components as broadcast events.
    ...(sessionId ? { session_id: sessionId } : {}),
    ...(observedAt ? { observed_at: observedAt } : {}),
    ...(turnId ? { turn_id: turnId } : {}),
  };
  return record;
}

function hyardJobRecordFromRuntimeDbEvent(event: RuntimeEventRecord): any | null {
  if (!event || event.aggregate_type !== 'host_job') return null;

  const payload = event.payload ?? {};
  const jobId = normalizedString(payload.job_id ?? event.aggregate_id);
  if (!jobId) return null;

  const sessionId = normalizedString(
    event.session_id ??
    payload.owner_session_id ??
    payload.callback_session_id ??
    payload.worker_session_id,
  );
  const runtimeStatus = normalizedString(payload.runtime_status);
  const bridgeStatus = normalizedString(payload.status);
  const observedAt = normalizedString(event.occurred_at);
  const sourceProvider = normalizedString(event.source) ?? payload.source_provider;

  return hyardJobRecordFromRuntimeHostJob({
    ...payload,
    id: jobId,
    job_id: jobId,
    ...(sessionId ? { session_id: sessionId } : {}),
    ...(runtimeStatus ? { status: runtimeStatus } : bridgeStatus ? { status: bridgeStatus } : {}),
    ...(bridgeStatus ? { bridge_status: bridgeStatus } : {}),
    ...(observedAt ? { observed_at: observedAt } : {}),
    ...(sourceProvider ? { source_provider: sourceProvider } : {}),
    event_id: event.event_id,
    event_type: event.event_type,
    aggregate_version: event.aggregate_version,
  });
}

function hyardJobsFromRuntimeSnapshot(snapshot: RuntimeSnapshot | null | undefined): Record<string, any> {
  const jobs: Record<string, any> = {};
  const hostJobs = Array.isArray(snapshot?.host_jobs) ? snapshot?.host_jobs ?? [] : [];
  hostJobs.forEach((item) => {
    const record = hyardJobRecordFromRuntimeHostJob(item);
    if (!record?.job_id) return;
    jobs[record.job_id] = record;
  });
  return jobs;
}

function pruneRuntimeTurnSessionCache(
  entries: Record<string, RuntimeTurnSessionCacheEntry>,
  nowMs = Date.now(),
): Record<string, RuntimeTurnSessionCacheEntry> {
  const freshEntries = Object.entries(entries).filter(([, entry]) => (
    entry &&
    entry.sessionId &&
    Number.isFinite(entry.lastSeenAt) &&
    nowMs - entry.lastSeenAt <= RUNTIME_TURN_SESSION_CACHE_TTL_MS
  ));
  if (freshEntries.length <= RUNTIME_TURN_SESSION_CACHE_MAX_ENTRIES) {
    return Object.fromEntries(freshEntries);
  }
  freshEntries.sort((a, b) => b[1].lastSeenAt - a[1].lastSeenAt);
  return Object.fromEntries(freshEntries.slice(0, RUNTIME_TURN_SESSION_CACHE_MAX_ENTRIES));
}

function upsertHyardJobRecord(jobs: Record<string, any>, record: any | null): Record<string, any> {
  if (!record?.job_id) return jobs;
  return {
    ...jobs,
    [record.job_id]: {
      ...(jobs[record.job_id] ?? {}),
      ...record,
    },
  };
}

function deferUntilNextPaint(): Promise<void> {
  return new Promise((resolve) => {
    if (typeof window === 'undefined' || typeof window.requestAnimationFrame !== 'function') {
      resolve();
      return;
    }
    window.requestAnimationFrame(() => window.setTimeout(resolve, 0));
  });
}

function deferUntilBrowserIdle(
  timeoutMs = CACHED_SESSION_TURN_REFRESH_IDLE_TIMEOUT_MS,
): Promise<void> {
  return new Promise((resolve) => {
    if (typeof window === 'undefined') {
      resolve();
      return;
    }
    const idleWindow = window as Window & {
      requestIdleCallback?: (callback: () => void, options?: { timeout?: number }) => number;
    };
    if (typeof idleWindow.requestIdleCallback === 'function') {
      idleWindow.requestIdleCallback(() => resolve(), { timeout: timeoutMs });
      return;
    }
    window.setTimeout(resolve, Math.min(120, timeoutMs));
  });
}

function turnListsEquivalentForUi(left: Turn[], right: Turn[]): boolean {
  if (left === right) return true;
  if (left.length !== right.length) return false;
  for (let index = 0; index < left.length; index += 1) {
    const a = left[index];
    const b = right[index];
    if (
      a.turn_id !== b.turn_id ||
      a.session_id !== b.session_id ||
      a.origin !== b.origin ||
      a.provider !== b.provider ||
      a.role !== b.role ||
      a.user_message !== b.user_message ||
      a.provider_response !== b.provider_response ||
      a.error_message !== b.error_message ||
      a.status !== b.status ||
      a.started_at !== b.started_at ||
      a.completed_at !== b.completed_at ||
      a.delegated_by !== b.delegated_by
    ) {
      return false;
    }
  }
  return true;
}

function shallowObjectEquivalent(
  left: Record<string, unknown> | null | undefined,
  right: Record<string, unknown> | null | undefined,
): boolean {
  if (left === right) return true;
  if (!left || !right) return false;
  const leftKeys = Object.keys(left);
  const rightKeys = Object.keys(right);
  if (leftKeys.length !== rightKeys.length) return false;
  return leftKeys.every((key) => Object.prototype.hasOwnProperty.call(right, key) && left[key] === right[key]);
}

function mergeHyardJobRecords(
  existing: Record<string, Record<string, unknown>>,
  incoming: Record<string, Record<string, unknown>>,
): Record<string, Record<string, unknown>> {
  const incomingEntries = Object.entries(incoming);
  if (incomingEntries.length === 0) return existing;

  let changed = false;
  const next: Record<string, Record<string, unknown>> = { ...existing };
  for (const [jobId, record] of incomingEntries) {
    const current = existing[jobId];
    const merged = current ? { ...current, ...record } : record;
    if (!shallowObjectEquivalent(current, merged)) {
      next[jobId] = merged;
      changed = true;
    }
  }
  return changed ? next : existing;
}

function runtimeItemType(payload: any): string {
  const item = runtimePayloadItem(payload);
  const raw = firstRuntimeIdentity(
    payload?.item_type,
    payload?.params?.item_type,
    payload?.item?.type,
    payload?.params?.item?.type,
    item?.type,
  );
  if (!raw) return '';
  const text = String(raw).trim();
  // Values such as `item.started`, `item/updated`, or `turn.completed` are
  // lifecycle protocol markers, not renderable item kinds.
  if (!text || text.includes('.') || text.includes('/')) return '';
  return normalizeRuntimeEventType(text);
}

const NON_ASSISTANT_RUNTIME_ITEM_TYPES = new Set([
  'tool_use',
  'tool_call',
  'function_call',
  'custom_tool_call',
  'mcp_tool_call',
  'dynamic_tool_call',
  'collab_agent_tool_call',
  'web_search',
  'image_view',
  'image_generation',
  'local_shell_call',
  'tool_result',
  'tool_response',
  'function_call_output',
  'custom_tool_call_output',
  'mcp_tool_call_output',
  'local_shell_call_output',
  'command_execution',
  'file_change',
  'diff_ready',
  'todo_list',
  'plan',
  'hook',
  'runtime_status',
  'auto_approval_review',
  'terminal_interaction',
  'mcp_tool_call_progress',
  'raw_response_item',
  'delegate_request',
  'delegate_result',
  'approval_request',
  'approval_decision',
  'server_request',
  'terminal_output',
  'terminal_output_delta',
  'tool_output_delta',
  'command_output_delta',
  'shell_output_delta',
  'stdout_delta',
  'stderr_delta',
  'process_output_delta',
  'file_change_delta',
  'diff_delta',
  'patch_delta',
  'execution_telemetry',
  'reasoning',
  'error',
]);

function isNonAssistantRuntimeItemType(itemType: string): boolean {
  return NON_ASSISTANT_RUNTIME_ITEM_TYPES.has(itemType);
}

function hasMeaningfulRuntimeValue(value: any): boolean {
  if (value === undefined || value === null) return false;
  if (typeof value === 'string') return value.trim().length > 0;
  if (Array.isArray(value)) return value.some(hasMeaningfulRuntimeValue);
  if (typeof value === 'object') {
    return Object.entries(value).some(([key, nested]) => {
      if ([
        'type',
        'role',
        'status',
        'id',
        'call_id',
        'tool_call_id',
        'request_id',
        'index',
        'encrypted_content',
      ].includes(key)) {
        return false;
      }
      return hasMeaningfulRuntimeValue(nested);
    });
  }
  return false;
}

function runtimeContentText(value: any): string | null {
  if (value === undefined || value === null) return null;
  if (typeof value === 'string') return value.length > 0 ? value : null;
  if (Array.isArray(value)) {
    const joined = value
      .map((block) => runtimeContentText(block))
      .filter((text): text is string => Boolean(text))
      .join('');
    return joined.length > 0 ? joined : null;
  }
  if (typeof value === 'object') {
    if (typeof value.text === 'string' && value.text.length > 0) return value.text;
    if (value.content !== undefined) return runtimeContentText(value.content);
  }
  return null;
}

function normalizeRuntimeKind(value: any): string {
  return normalizeRuntimeEventType(value).replace(/_/g, '.');
}

function runtimeDeltaText(value: any, inheritedTextHint = false): string | null {
  if (value === undefined || value === null) return null;
  if (typeof value === 'string') return inheritedTextHint && value.length > 0 ? value : null;
  if (typeof value !== 'object') return null;
  const ownKind = normalizeRuntimeKind(value.type);
  const ownKindIsTextish = ownKind ? isTextishRuntimeDeltaKind(ownKind) : false;
  if (ownKind && !ownKindIsTextish) return null;
  if (!inheritedTextHint && !ownKindIsTextish) return null;
  const nestedTextHint = inheritedTextHint || ownKindIsTextish;
  if (typeof value.text === 'string' && value.text.length > 0) return value.text;
  const content = runtimeContentText(value.content);
  if (content) return content;
  const nested = runtimeDeltaText(value.delta, nestedTextHint);
  if (nested) return nested;
  return runtimeContentText(value.message?.content);
}

function runtimePayloadText(payload: any): string | null {
  if (!payload) return null;
  const item = runtimePayloadItem(payload);
  const params = payload.params || {};
  const textProtocolHint = runtimeProtocolHasTextHint(payload);
  const paramsTextProtocolHint = runtimeProtocolHasTextHint(params);

  if (typeof payload.text === 'string' && payload.text.length > 0) return payload.text;
  if (typeof params.text === 'string' && params.text.length > 0) return params.text;

  const deltaText =
    runtimeDeltaText(payload.delta, textProtocolHint) ||
    runtimeDeltaText(params.delta, textProtocolHint || paramsTextProtocolHint) ||
    runtimeDeltaText(item?.delta, textProtocolHint);
  if (deltaText) return deltaText;

  const contentText =
    runtimeContentText(payload.content) ||
    runtimeContentText(params.content) ||
    runtimeContentText(item?.content);
  if (contentText) return contentText;

  if (typeof item?.text === 'string' && item.text.length > 0) return item.text;
  if (typeof payload.item?.text === 'string' && payload.item.text.length > 0) return payload.item.text;
  if (typeof payload.params?.item?.text === 'string' && payload.params.item.text.length > 0) return payload.params.item.text;
  if (typeof payload.result === 'string' && payload.result.length > 0) return payload.result;
  if (typeof params.result === 'string' && params.result.length > 0) return params.result;
  if (typeof item?.result === 'string' && item.result.length > 0) return item.result;

  return (
    runtimeContentText(payload.message?.content) ||
    runtimeContentText(params.message?.content) ||
    runtimeContentText(item?.message?.content)
  );
}

function runtimeProtocolKind(payload: any): string {
  return normalizeRuntimeEventType(payload?.method || payload?.params?.method || payload?.type || payload?.params?.type || '')
    .replace(/_/g, '.');
}

function runtimeDeltaKind(payload: any): string {
  return normalizeRuntimeKind(payload?.delta?.type || payload?.params?.delta?.type || runtimePayloadItem(payload)?.delta?.type || '');
}

function isTextishRuntimeDeltaKind(kind: string): boolean {
  const normalized = normalizeRuntimeKind(kind);
  return (
    normalized.includes('agent.message') ||
    normalized.includes('assistant') ||
    normalized.includes('message.delta') ||
    normalized.includes('content.block.delta') ||
    normalized.includes('text.delta') ||
    normalized === 'text' ||
    normalized === 'output.text' ||
    normalized.includes('output.text.delta') ||
    normalized === 'agent.message.delta'
  );
}

function runtimeProtocolHasTextHint(payload: any): boolean {
  const protocol = runtimeProtocolKind(payload);
  return (
    protocol.includes('agentmessage') ||
    protocol.includes('agent.message') ||
    protocol.includes('assistant') ||
    protocol.includes('message.delta') ||
    protocol.includes('content.delta') ||
    protocol.includes('text.delta') ||
    protocol.includes('output.text')
  );
}

function isRuntimeTextishDelta(payload: any): boolean {
  const protocolTextHint = runtimeProtocolHasTextHint(payload);
  const paramsProtocolTextHint = payload?.params ? runtimeProtocolHasTextHint(payload.params) : false;
  const item = runtimePayloadItem(payload);
  const deltaText =
    runtimeDeltaText(payload?.delta, protocolTextHint) ||
    runtimeDeltaText(payload?.params?.delta, protocolTextHint || paramsProtocolTextHint) ||
    runtimeDeltaText(item?.delta, protocolTextHint);
  if (!deltaText) return false;
  const deltaKind = runtimeDeltaKind(payload);
  const deltaKindIsTextish = deltaKind ? isTextishRuntimeDeltaKind(deltaKind) : false;
  if (deltaKind && !deltaKindIsTextish) return false;
  return protocolTextHint || paramsProtocolTextHint || deltaKindIsTextish;
}

function payloadLooksLikeToolOrActivity(payload: any): boolean {
  if (!payload || typeof payload !== 'object') return false;
  const item = runtimePayloadItem(payload);
  const params = payload.params || {};
  const itemType = runtimeItemType(payload);
  if (itemType && isNonAssistantRuntimeItemType(itemType)) return true;

  const protocol = runtimeProtocolKind(payload);
  if (
    protocol.includes('tool') ||
    protocol.includes('command') ||
    protocol.includes('shell') ||
    protocol.includes('approval') ||
    protocol.includes('server.request') ||
    protocol.includes('terminal') ||
    protocol.includes('execution') ||
    protocol.includes('plan') ||
    protocol.includes('hook') ||
    protocol.includes('reasoning') ||
    protocol.includes('diff') ||
    protocol.includes('file.change')
  ) {
    return true;
  }

  const activityKeys = [
    'execution',
    'command',
    'cmd',
    'stdout',
    'stderr',
    'aggregated_output',
    'aggregatedOutput',
    'exit_code',
    'exitCode',
    'diff',
    'patch',
    'file',
    'filePath',
    'relativePath',
    'sourcePath',
    'path',
    'request',
    'tool',
    'tool_name',
    'function',
    'arguments',
    'changes',
    'edits',
  ];

  return [payload, params, item].some((candidate) => {
    if (!candidate || typeof candidate !== 'object' || Array.isArray(candidate)) return false;
    return activityKeys.some((key) => Object.prototype.hasOwnProperty.call(candidate, key));
  });
}

function compactRealtimeLines(incoming: string[]): string[] {
  const next: string[] = [];
  for (const rawLine of incoming) {
    const line = String(rawLine ?? '').replace(/\r/g, '');
    // Runtime status text and terminal output can occasionally mirror the same
    // provider line. De-dupe consecutive duplicates so the live activity block
    // stays readable while still preserving the full stream order.
    if (next[next.length - 1] === line) continue;
    next.push(line);
  }
  if (next.length <= MAX_REALTIME_TERMINAL_LINES) return next;
  return next.slice(next.length - MAX_REALTIME_TERMINAL_LINES);
}

function normalizeRealtimeText(text: string): string {
  return text.replace(/\r\n/g, '\n').replace(/\r/g, '\n');
}

function trimRealtimeTextBuffer(text: string): string {
  if (text.length <= MAX_REALTIME_TERMINAL_CHARS) return text;
  const tail = text.slice(text.length - MAX_REALTIME_TERMINAL_CHARS);
  const firstNewline = tail.indexOf('\n');
  return firstNewline >= 0 ? tail.slice(firstNewline + 1) : tail;
}

function realtimeTextToLines(text: string): string[] {
  return compactRealtimeLines(normalizeRealtimeText(text).split('\n'));
}

function hasMeaningfulReasoningContent(payload: any): boolean {
  const item = runtimePayloadItem(payload);
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
  ].some(hasMeaningfulRuntimeValue);
}

function isAssistantTextRuntimePayload(payload: any): boolean {
  if (!payload) return true;
  const item = runtimePayloadItem(payload);
  const itemType = runtimeItemType(payload);
  const role = firstRuntimeIdentity(
    item?.role,
    payload?.role,
    payload?.params?.role,
    payload?.message?.role,
    payload?.params?.message?.role,
  )?.toLowerCase();
  const protocol = runtimeProtocolKind(payload);

  if (isNonAssistantRuntimeItemType(itemType)) return false;
  if (itemType === 'agent_message' || itemType === 'assistant') return true;
  if (itemType === 'message') return role === 'assistant';
  if (protocol.includes('agentmessage') || protocol.includes('agent_message')) return true;
  if (isRuntimeTextishDelta(payload)) return true;
  if (role === 'assistant') return true;

  // If the provider sent a completely generic textual payload (no recognized
  // renderable item kind), treat it as assistant text. Once an item kind is
  // present, keep tool/result/reasoning payloads out of the chat body and let
  // renderTurnEvents display them in Execution Details instead.
  if (
    !itemType &&
    !protocol.startsWith('turn.') &&
    !protocol.startsWith('thread.') &&
    !protocol.startsWith('item.') &&
    !payloadLooksLikeToolOrActivity(payload)
  ) {
    return Boolean(runtimePayloadText(payload));
  }

  return false;
}

function isRuntimeAssistantTextPayload(payload: any): boolean {
  if (!payload || !isAssistantTextRuntimePayload(payload)) return false;
  const itemType = runtimeItemType(payload);
  const protocol = runtimeProtocolKind(payload);
  return (
    !itemType ||
    itemType === 'agent_message' ||
    itemType === 'assistant' ||
    itemType === 'message' ||
    protocol.includes('agentmessage') ||
    protocol.includes('agent_message') ||
    isRuntimeTextishDelta(payload)
  );
}

function hasProviderTextUpdate(text: any, payload: any): boolean {
  const incomingText = typeof text === 'string' ? text : '';
  if (incomingText.trim() && !isSystemStatusText(incomingText)) {
    return isAssistantTextRuntimePayload(payload);
  }
  if (!payload || !isAssistantTextRuntimePayload(payload)) return false;
  const payloadText = runtimePayloadText(payload);
  return Boolean(payloadText && !isSystemStatusText(payloadText));
}

function isRuntimeReasoningEvent(data: any): boolean {
  return runtimeItemType(data?.payload) === 'reasoning';
}

function isEmptyReasoningRuntimeEvent(data: any): boolean {
  return isRuntimeReasoningEvent(data) && !hasMeaningfulReasoningContent(data?.payload);
}

function reasoningRuntimeSignature(payload: any): string {
  const item = runtimePayloadItem(payload);
  const pieces = [
    item?.summary,
    payload?.summary,
    payload?.params?.summary,
    item?.text,
    payload?.text,
    payload?.params?.text,
    item?.content,
    payload?.content,
    payload?.params?.content,
  ];
  try {
    const signature = JSON.stringify(pieces) ?? '';
    return signature.length > 4096 ? `${signature.slice(0, 4096)}#${signature.length}` : signature;
  } catch {
    return pieces.map((piece) => String(piece ?? '')).join('|');
  }
}

const runtimeEventKeyObjectCache = new WeakMap<object, string>();
const sessionEventTimestampObjectCache = new WeakMap<object, number | null>();

function computeRuntimeEventKey(event: any): string {
  const itemType = runtimeItemType(event?.payload);
  // Reasoning streams can emit a fresh protocol item id for every heartbeat.
  // Coalesce them per turn/provider before considering item ids; otherwise one
  // empty/near-empty reasoning heartbeat can become one React row/tool card.
  if (event?.turn_id && itemType === 'reasoning') {
    return `item:${event.turn_id}:${event.provider || ''}:reasoning`;
  }
  const itemId = runtimeItemIdentity(event?.payload);
  if (event?.turn_id && itemId) {
    return `item:${event.turn_id}:${itemId}`;
  }
  if (event?.event_id) {
    return `event:${event.event_id}`;
  }
  return [
    'anon',
    event?.turn_id || '',
    event?.event_type || '',
    event?.provider || '',
    event?.timestamp || '',
  ].join(':');
}

function runtimeEventKey(event: any): string {
  if (event && typeof event === 'object') {
    const cached = runtimeEventKeyObjectCache.get(event);
    if (cached !== undefined) return cached;
    const key = computeRuntimeEventKey(event);
    runtimeEventKeyObjectCache.set(event, key);
    return key;
  }
  return computeRuntimeEventKey(event);
}

function runtimeLifecycleRank(event: any): number {
  const payload = event?.payload || {};
  const item = runtimePayloadItem(payload);
  const eventType = normalizeRuntimeEventType(event?.event_type);
  const protocol = String(payload?.method || payload?.params?.method || payload?.type || payload?.params?.type || '').toLowerCase().replace(/\//g, '.');
  const status = String(item?.status || payload?.status || '').toLowerCase();

  if (status.includes('fail') || status.includes('error') || protocol.includes('failed')) return 4;
  if (
    eventType === 'item_completed' ||
    eventType === 'artifact_ready' ||
    protocol.includes('completed') ||
    protocol.includes('complete') ||
    ['completed', 'complete', 'success', 'succeeded', 'done', 'finished'].includes(status)
  ) {
    return 3;
  }
  if (
    eventType === 'item_updated' ||
    protocol.includes('updated') ||
    ['in_progress', 'in-progress', 'running', 'streaming'].includes(status)
  ) {
    return 2;
  }
  if (eventType === 'item_started' || protocol.includes('started') || ['pending', 'queued', 'started'].includes(status)) {
    return 1;
  }
  return 0;
}

function mergeRuntimePayloadValue(preferred: any, fallback: any): any {
  if (preferred === undefined || preferred === null) return fallback;
  if (fallback === undefined || fallback === null) return preferred;
  if (typeof preferred === 'string') return preferred.trim().length > 0 ? preferred : fallback;
  if (Array.isArray(preferred)) return hasMeaningfulRuntimeValue(preferred) ? preferred : fallback;
  if (typeof preferred === 'object' && typeof fallback === 'object' && !Array.isArray(fallback)) {
    const merged: Record<string, any> = { ...fallback, ...preferred };
    for (const key of Object.keys(fallback)) {
      if (key in preferred) {
        merged[key] = mergeRuntimePayloadValue(preferred[key], fallback[key]);
      }
    }
    return merged;
  }
  return preferred;
}

function preferRuntimeEvent(current: any, next: any): any {
  const preferNextEnvelope = runtimeLifecycleRank(next) >= runtimeLifecycleRank(current);
  const preferred = preferNextEnvelope ? { ...current, ...next } : { ...next, ...current };
  preferred.payload = preferNextEnvelope
    ? mergeRuntimePayloadValue(next.payload, current.payload)
    : mergeRuntimePayloadValue(current.payload, next.payload);
  return preferred;
}

function isRuntimeEventNoop(current: any, next: any): boolean {
  const itemType = runtimeItemType(next?.payload);
  if (itemType !== 'reasoning') return false;
  return (
    normalizeRuntimeEventType(current?.event_type) === normalizeRuntimeEventType(next?.event_type) &&
    runtimeLifecycleRank(current) === runtimeLifecycleRank(next) &&
    reasoningRuntimeSignature(current?.payload) === reasoningRuntimeSignature(next?.payload)
  );
}

function compactVisibleSessionEvents(events: any[]): any[] {
  let next: any[] | null = null;
  for (let index = 0; index < events.length; index += 1) {
    const event = events[index];
    if (isEmptyReasoningRuntimeEvent(event)) {
      if (next === null) next = events.slice(0, index);
      continue;
    }
    if (next !== null) next.push(event);
  }
  return next ?? events;
}

function mergeSessionEventLists(existing: any[], incoming: any[]): any[] {
  const visibleIncoming = compactVisibleSessionEvents(incoming);

  if (visibleIncoming.length === 0) {
    return existing;
  }

  // Hot path for live streaming: the backend now sends incremental session
  // events, so most merges are either "replace the active tail item" or
  // "append a few fresh items". Avoid rebuilding a Map over the entire long
  // history on every runtime tick.
  if (visibleIncoming.length <= 64) {
    let next = existing;
    let changed = false;
    let canUseFastPath = true;
    const recentSearchWindow = 512;

    for (const event of visibleIncoming) {
      const key = runtimeEventKey(event);
      let existingIndex = -1;
      const searchStart = next.length - 1;
      const searchEnd = Math.max(0, next.length - recentSearchWindow);
      for (let index = searchStart; index >= searchEnd; index -= 1) {
        if (runtimeEventKey(next[index]) === key) {
          existingIndex = index;
          break;
        }
      }

      if (existingIndex >= 0) {
        const preferred = preferRuntimeEvent(event, next[existingIndex]);
        const replacement = isRuntimeEventNoop(next[existingIndex], preferred)
          ? next[existingIndex]
          : preferred;
        if (replacement !== next[existingIndex]) {
          if (!changed) next = next.slice();
          next[existingIndex] = replacement;
          changed = true;
        }
        continue;
      }

      const eventMs = sessionEventTimestampMs(event);
      const lastMs = next.length > 0 ? sessionEventTimestampMs(next[next.length - 1]) : null;
      if (eventMs !== null && lastMs !== null && eventMs < lastMs) {
        canUseFastPath = false;
        break;
      }

      if (!changed) next = next.slice();
      next.push(event);
      changed = true;
    }

    if (canUseFastPath) {
      return changed ? next : existing;
    }
  }

  const visibleExisting = compactVisibleSessionEvents(existing);
  const merged = [...visibleExisting];
  const indexByKey = new Map<string, number>();
  merged.forEach((event, index) => indexByKey.set(runtimeEventKey(event), index));

  visibleIncoming.forEach((event) => {
    const key = runtimeEventKey(event);
    const existingIndex = indexByKey.get(key);
    if (existingIndex === undefined) {
      indexByKey.set(key, merged.length);
      merged.push(event);
      return;
    }
    const preferred = preferRuntimeEvent(event, merged[existingIndex]);
    merged[existingIndex] = isRuntimeEventNoop(merged[existingIndex], preferred)
      ? merged[existingIndex]
      : preferred;
  });

  if (
    visibleExisting.length === existing.length &&
    merged.length === existing.length &&
    merged.every((event, index) => event === existing[index])
  ) {
    return existing;
  }

  return merged;
}

function sessionEventTimestampMs(event: any): number | null {
  if (event && typeof event === 'object' && sessionEventTimestampObjectCache.has(event)) {
    return sessionEventTimestampObjectCache.get(event) ?? null;
  }
  const raw = event?.timestamp ?? event?.created_at ?? event?.updated_at;
  if (typeof raw !== 'string' || !raw.trim()) {
    if (event && typeof event === 'object') sessionEventTimestampObjectCache.set(event, null);
    return null;
  }
  const ms = Date.parse(raw);
  const parsed = Number.isFinite(ms) ? ms : null;
  if (event && typeof event === 'object') sessionEventTimestampObjectCache.set(event, parsed);
  return parsed;
}

function maxSessionEventTimestamp(events: any[], fallback?: string): string | undefined {
  let bestMs = fallback ? Date.parse(fallback) : Number.NEGATIVE_INFINITY;
  let bestTimestamp = fallback;
  for (const event of events) {
    const raw = event?.timestamp;
    if (typeof raw !== 'string' || !raw.trim()) continue;
    const ms = sessionEventTimestampMs(event);
    if (ms === null) continue;
    if (ms >= bestMs) {
      bestMs = ms;
      bestTimestamp = raw;
    }
  }
  return bestTimestamp;
}

function upsertRuntimeItemEvent(existing: any[], data: any): any[] {
  const timestamp = new Date().toISOString();
  const nextEvent = buildRuntimeItemEvent(data, timestamp);
  if (!nextEvent) return existing;
  return upsertRuntimeEventObject(existing, nextEvent);
}

function upsertRuntimeEventObject(existing: any[], nextEvent: any): any[] {
  const key = runtimeEventKey(nextEvent);
  // Streaming updates usually mutate the most recently appended runtime item.
  // Search from the tail to keep the hot path near O(1) on long histories.
  let existingIdx = -1;
  for (let index = existing.length - 1; index >= 0; index -= 1) {
    if (runtimeEventKey(existing[index]) === key) {
      existingIdx = index;
      break;
    }
  }
  if (existingIdx === -1) {
    return [...existing, nextEvent];
  }
  const next = [...existing];
  const preferred = preferRuntimeEvent(next[existingIdx], nextEvent);
  if (isRuntimeEventNoop(next[existingIdx], preferred)) return existing;
  next[existingIdx] = preferred;
  return next;
}

function buildRuntimeItemEvent(data: any, timestamp: string): any | null {
  const payload = data?.payload;
  if (!payload) return null;
  if (isRuntimeAssistantTextPayload(payload)) return null;
  if (isEmptyReasoningRuntimeEvent(data)) return null;

  const eventType = normalizeRuntimeEventType(data.event_type || RUNTIME_ITEM_EVENT_FALLBACK) || RUNTIME_ITEM_EVENT_FALLBACK;
  const itemId = runtimeItemIdentity(payload);
  return {
    event_id: data.event_id || `live:${data.turn_id}:${eventType}:${itemId || timestamp}`,
    turn_id: data.turn_id,
    event_type: eventType,
    provider: data.provider,
    timestamp,
    payload,
  };
}

function upsertRuntimeItemEvents(existing: any[], dataList: any[]): any[] {
  if (dataList.length === 0) return existing;
  if (dataList.length === 1) return upsertRuntimeItemEvent(existing, dataList[0]);

  const timestamp = new Date().toISOString();
  const incoming: any[] = [];
  const incomingIndexByKey = new Map<string, number>();
  for (const data of dataList) {
    const event = buildRuntimeItemEvent(data, timestamp);
    if (!event) continue;
    const key = runtimeEventKey(event);
    const existingIndex = incomingIndexByKey.get(key);
    if (existingIndex === undefined) {
      incomingIndexByKey.set(key, incoming.length);
      incoming.push(event);
    } else {
      incoming[existingIndex] = preferRuntimeEvent(incoming[existingIndex], event);
    }
  }
  if (incoming.length === 0) return existing;

  // A runtime batch can contain dozens of item/tool/reasoning updates for the
  // same active turn. Coalesce duplicate updates first and commit them through
  // one React state updater; each surviving event still uses the exact same
  // tail-first upsert path as the single-event stream to avoid creating
  // duplicate tool cards for older-but-still-active items.
  let next = existing;
  for (const event of incoming) {
    next = upsertRuntimeEventObject(next, event);
  }
  return next;
}

function shouldDropRuntimeAssistantText(text: string | null): boolean {
  return Boolean(text && text.trim() && isSystemStatusText(text));
}

function appendRuntimeAssistantText(prev: string, text: string): string {
  return shouldDropRuntimeAssistantText(text) ? prev : prev + text;
}

function replaceRuntimeAssistantText(prev: string, text: string): string {
  return shouldDropRuntimeAssistantText(text) ? prev : text;
}

function applyProviderTextUpdate(prev: string, text: string, payload: any): string {
  const incomingText = typeof text === 'string' ? text : '';
  if (!payload) return incomingText && !isSystemStatusText(incomingText) ? prev + incomingText : prev;
  if (!isAssistantTextRuntimePayload(payload)) return prev;

  const item = runtimePayloadItem(payload);
  const params = payload.params || {};
  const textProtocolHint = runtimeProtocolHasTextHint(payload);
  const paramsTextProtocolHint = runtimeProtocolHasTextHint(params);

  const directText = typeof payload.text === 'string' ? payload.text : null;
  if (directText !== null) {
    return appendRuntimeAssistantText(prev, directText);
  }
  const paramsText = typeof params.text === 'string' ? params.text : null;
  if (paramsText !== null) {
    return appendRuntimeAssistantText(prev, paramsText);
  }

  const deltaText =
    runtimeDeltaText(payload.delta, textProtocolHint) ||
    runtimeDeltaText(params.delta, textProtocolHint || paramsTextProtocolHint) ||
    runtimeDeltaText(item?.delta, textProtocolHint);
  if (deltaText !== null) return appendRuntimeAssistantText(prev, deltaText);

  const contentText =
    runtimeContentText(payload.content) ||
    runtimeContentText(params.content) ||
    runtimeContentText(item?.content);
  if (contentText !== null) {
    const protocol = runtimeProtocolKind(payload);
    const isDelta = payload.delta === true || params.delta === true || item?.delta === true || protocol.includes('delta');
    return isDelta
      ? appendRuntimeAssistantText(prev, contentText)
      : replaceRuntimeAssistantText(prev, contentText);
  }

  if (typeof item?.text === 'string') return replaceRuntimeAssistantText(prev, item.text);
  if (typeof payload.item?.text === 'string') return replaceRuntimeAssistantText(prev, payload.item.text);
  if (typeof payload.params?.item?.text === 'string') return replaceRuntimeAssistantText(prev, payload.params.item.text);
  if (typeof item?.result === 'string') return replaceRuntimeAssistantText(prev, item.result);
  if (typeof payload.result === 'string') return replaceRuntimeAssistantText(prev, payload.result);
  if (typeof params.result === 'string') return replaceRuntimeAssistantText(prev, params.result);

  const messageText =
    runtimeContentText(payload.message?.content) ||
    runtimeContentText(params.message?.content) ||
    runtimeContentText(item?.message?.content);
  if (messageText) return replaceRuntimeAssistantText(prev, messageText);

  return incomingText ? appendRuntimeAssistantText(prev, incomingText) : prev;
}

function App() {
  const [sessions, setSessions] = useState<Session[]>([]);
  const [selectedSession, setSelectedSession] = useState<Session | null>(null);
  // Ref to always capture the latest selectedSession inside async loaders and
  // the singleton runtime listener. Keep this hook before worker sync effects
  // so those effects never compare against a stale previous session.
  const selectedSessionRef = useRef<Session | null>(null);
  useEffect(() => {
    selectedSessionRef.current = selectedSession;
  }, [selectedSession]);
  const [turns, setTurns] = useState<Turn[]>([]);
  const turnsRef = useRef<Turn[]>([]);
  const finalTurnResponsesRef = useRef<Record<string, string>>({});
  useEffect(() => {
    turnsRef.current = turns;
  }, [turns]);
  const [turnAttachments, setTurnAttachments] = useState<Record<string, InputAttachment[]>>(readStoredTurnAttachments);
  const pendingAttachmentBindingsRef = useRef<PendingTurnAttachmentBinding[]>([]);
  const activeAttachmentBindingIdRef = useRef<string | null>(null);
  const [isGenerating, setIsGenerating] = useState(false);
  const [messageQueue, setMessageQueue] = useState<SendPayload[]>([]);
  // React state is not synchronous enough to decide whether a rapid follow-up
  // send should dispatch or queue. Keep refs as the authoritative in-flight and
  // FIFO queue state so double-Enter / button-click bursts cannot start two
  // `run_turn` invocations concurrently.
  const dispatchingSessionIdsRef = useRef<Set<string>>(new Set());
  const preparingSessionIdsRef = useRef<Set<string>>(new Set());
  // Mirror of messageQueue for reading inside dispatchMessage's finally without
  // making the queue a dep of the in-flight invocation. The ref always reflects
  // the latest queue state.
  const messageQueueRef = useRef<SendPayload[]>([]);
  useEffect(() => {
    messageQueueRef.current = messageQueue;
  }, [messageQueue]);

  useEffect(() => {
    try {
      window.localStorage.setItem(TURN_ATTACHMENTS_STORAGE_KEY, JSON.stringify(turnAttachments));
    } catch (error) {
      console.warn('Failed to persist turn attachment previews', error);
    }
  }, [turnAttachments]);

  const commitTurns = (next: Turn[] | ((previous: Turn[]) => Turn[])) => {
    const resolved = typeof next === 'function'
      ? (next as (previousTurns: Turn[]) => Turn[])(turnsRef.current)
      : next;
    turnsRef.current = resolved;
    setTurns(resolved);
  };

  const rememberFinalTurnResponse = (turnId: unknown, response: unknown) => {
    if (typeof turnId !== 'string' || !turnId) return null;
    const knownTurn = turnsRef.current.find((turn) => turn.turn_id === turnId);
    const finalResponse = knownTurn
      ? fallbackResponseForUserMessage(response, knownTurn.user_message)
      : nonBlankText(response);
    if (finalResponse === null) return null;
    finalTurnResponsesRef.current = {
      ...finalTurnResponsesRef.current,
      [turnId]: finalResponse,
    };
    return finalResponse;
  };

  const applyRememberedFinalTurnResponses = (turnList: Turn[]) => {
    const remembered = finalTurnResponsesRef.current;
    const entries = Object.entries(remembered);
    if (entries.length === 0) return turnList;

    let nextTurns = turnList;
    const remaining = { ...remembered };
    for (const [turnId, response] of entries) {
      const existingTurn = nextTurns.find((turn) => turn.turn_id === turnId);
      if (!existingTurn) continue;
      if (nonBlankText(existingTurn.provider_response) !== null) {
        delete remaining[turnId];
        continue;
      }
      const finalResponse = fallbackResponseForUserMessage(response, existingTurn.user_message);
      if (finalResponse === null) {
        delete remaining[turnId];
        continue;
      }
      nextTurns = mergeFinalResponseIntoTurns(nextTurns, turnId, finalResponse);
    }
    finalTurnResponsesRef.current = remaining;
    return nextTurns;
  };

  const applyAttachmentBindingToTurn = (
    binding: PendingTurnAttachmentBinding,
    turnId: string,
    options?: { keepTempAttachment?: boolean },
  ) => {
    setTurnAttachments((prev) => {
      const next = { ...prev };
      const tempAttachments = binding.tempTurnId ? next[binding.tempTurnId] : undefined;
      const merged = mergeInputAttachments(next[turnId], tempAttachments, binding.attachments);
      if (merged.length > 0) {
        next[turnId] = merged;
      }
      if (!options?.keepTempAttachment && binding.tempTurnId && binding.tempTurnId !== turnId) {
        delete next[binding.tempTurnId];
      }
      return next;
    });
  };

  const bindActiveAttachmentTurnFromRuntime = (turnId: string, sessionId?: string | null) => {
    const bindingId = activeAttachmentBindingIdRef.current;
    const pending = pendingAttachmentBindingsRef.current;
    let binding = bindingId
      ? pending.find((item) => (
          item.bindingId === bindingId &&
          (!sessionId || item.sessionId === sessionId)
        ))
      : undefined;
    if (!binding && sessionId) {
      binding = pending.find((item) => item.sessionId === sessionId && !item.resolvedTurnId);
    }
    if (!binding) {
      if (bindingId && !pending.some((item) => item.bindingId === bindingId)) {
        activeAttachmentBindingIdRef.current = null;
      }
      return false;
    }
    binding.resolvedTurnId = turnId;
    applyAttachmentBindingToTurn(binding, turnId, { keepTempAttachment: true });
    if (activeAttachmentBindingIdRef.current === binding.bindingId) {
      activeAttachmentBindingIdRef.current = null;
    }
    return true;
  };

  const reconcileTurnAttachmentBindings = (sessionId: string, turnList: Turn[]) => {
    const pending = pendingAttachmentBindingsRef.current;
    if (pending.length === 0) return;
    const resolved: Array<{ binding: PendingTurnAttachmentBinding; turnId: string }> = [];
    for (const binding of pending) {
      if (binding.sessionId !== sessionId || binding.attachments.length === 0) continue;
      const matchedTurn = findMatchingAttachmentTurn(turnList, binding);
      if (matchedTurn) {
        resolved.push({ binding, turnId: matchedTurn.turn_id });
      }
    }
    if (resolved.length === 0) return;

    const resolvedBindingIds = new Set(resolved.map(({ binding }) => binding.bindingId));
    pendingAttachmentBindingsRef.current = pending.filter((binding) => !resolvedBindingIds.has(binding.bindingId));
    if (activeAttachmentBindingIdRef.current && resolvedBindingIds.has(activeAttachmentBindingIdRef.current)) {
      activeAttachmentBindingIdRef.current = null;
    }
    setTurnAttachments((prev) => {
      const next = { ...prev };
      for (const { binding, turnId } of resolved) {
        const tempAttachments = binding.tempTurnId ? next[binding.tempTurnId] : undefined;
        const merged = mergeInputAttachments(next[turnId], tempAttachments, binding.attachments);
        if (merged.length > 0) {
          next[turnId] = merged;
        }
        if (binding.tempTurnId && binding.tempTurnId !== turnId) {
          delete next[binding.tempTurnId];
        }
      }
      return next;
    });
  };
  
  // Streaming state during active run
  const [activeCoreText, setActiveCoreText] = useState('');
  const [activePeerText, setActivePeerText] = useState('');
  const [activePeerName, setActivePeerName] = useState<string | null>(null);
  const [activeNodes, setActiveNodes] = useState<string[]>([]);
  const [activeTurnIds, setActiveTurnIds] = useState<string[]>([]);
  const [telemetryLogs, setTelemetryLogs] = useState<TelemetryLog[]>([]);
  const [sessionEvents, setSessionEvents] = useState<any[]>([]);
  const sessionEventsCursorRef = useRef<Record<string, string>>({});
  const [realtimeTerminalLines, setRealtimeTerminalLines] = useState<Record<string, string[]>>({});
  // Keep the unbounded-ish text accumulator out of React state. Terminal
  // chunks can arrive dozens of times per second; storing the buffer in state
  // made every chunk recompute line splits and re-render the whole app. The
  // visible `realtimeTerminalLines` state is flushed on a short timer instead.
  const realtimeTerminalBuffersRef = useRef<Record<string, string>>({});
  const realtimeTerminalPendingRef = useRef<Map<string, string>>(new Map());
  const realtimeTerminalFlushTimerRef = useRef<number | null>(null);
  const [activeCoreTurnId, setActiveCoreTurnId] = useState<string | null>(null);
  const [activePeerTurnId, setActivePeerTurnId] = useState<string | null>(null);
  const [selectedAgentTurnId, setSelectedAgentTurnId] = useState<string | null>(null);
  const [hyardJobs, setHyardJobs] = useState<Record<string, any>>({});
  const runtimeSnapshotCursorRef = useRef<Record<string, number>>({});
  const [runtimeTurnPhases, setRuntimeTurnPhases] = useState<Record<string, RuntimeTurnPhase>>({});
  const [runtimeTurnStartedAt, setRuntimeTurnStartedAt] = useState<Record<string, number>>({});
  const [runtimeTurnPhaseChangedAt, setRuntimeTurnPhaseChangedAt] = useState<Record<string, number>>({});
  const [runtimeDispatchStartedAt, setRuntimeDispatchStartedAt] = useState<number | null>(null);
  const [runtimePreparingPhase, setRuntimePreparingPhase] = useState<string | null>(null);
  const runtimeDispatchStartedAtRef = useRef<number | null>(null);
  const sessionUiSnapshotsRef = useRef<Record<string, SessionUiSnapshot>>({});
  const sessionDataRefreshGenerationRef = useRef<Record<string, number>>({});
  const runtimeTurnSessionIdRef = useRef<Record<string, RuntimeTurnSessionCacheEntry>>({});
  const [providerStatuses, setProviderStatuses] = useState<ProviderStatus[]>([]);
  const [providerStatusLoading, setProviderStatusLoading] = useState(false);
  const [providerStatusError, setProviderStatusError] = useState<string | null>(null);

  // Settings State
  const [config, setConfig] = useState<SwitchyardConfig | null>(null);
  const sandboxModeRef = useRef<SandboxMode>(DEFAULT_SANDBOX_MODE);
  useEffect(() => {
    sandboxModeRef.current = config?.sandbox?.mode ?? DEFAULT_SANDBOX_MODE;
  }, [config?.sandbox?.mode]);
  const [showSettings, setShowSettings] = useState(false);
  const [settingsTab, setSettingsTab] = useState<string>('general');

  // New Session Creator State
  const [newSessionProvider, setNewSessionProvider] = useState('codex');

  // Persistence & Artifact Drawer State
  const [sessionWorkers, setSessionWorkers] = useState<InstanceMetadata[]>([]);

  // Workspace state — drives the workbench shell and scopes the session list.
  // Like VS Code, Switchyard can start with no folder/workspace opened; the
  // chat/workspace-scoped panels stay on the welcome screen until the user
  // explicitly opens a folder.
  const [workspaces, setWorkspaces] = useState<Workspace[]>([]);
  const [currentWorkspace, setCurrentWorkspace] = useState<Workspace | null>(null);

  // Left-rail mode. `chat` shows the session list (current behavior);
  // `files` / `terminal` are placeholders wired to land in later phases.
  const [railMode, setRailMode] = useState<RailMode>('chat');
  const [leftColumnWidth, setLeftColumnWidth] = useState(() =>
    readStoredLayoutNumber(
      'switchyard.leftColumnWidth',
      DEFAULT_LEFT_COLUMN_WIDTH,
      MIN_LEFT_COLUMN_WIDTH,
      MAX_LEFT_COLUMN_WIDTH,
    ),
  );
  const [canvasColumnWidth, setCanvasColumnWidth] = useState(() =>
    readStoredLayoutNumber(
      'switchyard.canvasColumnWidth',
      DEFAULT_CANVAS_COLUMN_WIDTH,
      MIN_CANVAS_COLUMN_WIDTH,
      Math.round(window.innerWidth * 0.75),
    ),
  );
  const appContainerRef = useRef<HTMLDivElement | null>(null);
  const mainRowRef = useRef<HTMLDivElement | null>(null);
  const leftColumnWidthRef = useRef(leftColumnWidth);
  const canvasColumnWidthRef = useRef(canvasColumnWidth);

  useEffect(() => {
    leftColumnWidthRef.current = leftColumnWidth;
    appContainerRef.current?.style.setProperty(
      '--switchyard-left-column-width',
      `${Math.round(leftColumnWidth)}px`,
    );
  }, [leftColumnWidth]);

  useEffect(() => {
    canvasColumnWidthRef.current = canvasColumnWidth;
    appContainerRef.current?.style.setProperty(
      '--switchyard-canvas-column-width',
      `${Math.round(canvasColumnWidth)}px`,
    );
  }, [canvasColumnWidth]);

  // Diagnostics drawer (formerly the always-on ControlCenter column).
  // Closed by default — users opt in via the rail's Activity button or
  // the Workers item in the bottom status bar.
  const [drawerOpen, setDrawerOpen] = useState(false);
  // Load the diagnostics bundle on first use, then keep it mounted after
  // closing so tab/search state is preserved without putting the heavy
  // graph + telemetry panels on the initial chat render path.
  const [drawerEverOpened, setDrawerEverOpened] = useState(false);

  useEffect(() => {
    if (drawerOpen) {
      setDrawerEverOpened(true);
    }
  }, [drawerOpen]);

  // Bottom-anchored terminal panel. Toggled independently of railMode
  // so users keep the left column (workspace / files) visible while a
  // terminal is open — same UX as VS Code's integrated terminal.
  const [terminalOpen, setTerminalOpen] = useState(false);
  // Once opened, keep the terminal component mounted while hidden. This
  // avoids repeatedly tearing down and recreating the backend PTY on every
  // show/hide cycle, which makes terminal startup stalls much more visible.
  const [terminalEverOpened, setTerminalEverOpened] = useState(false);

  // Topology overlay — fullscreen modal showing the agent-graph view.
  // Lives outside the diagnostics drawer so it can claim the whole
  // window when the user wants the big picture, then close back to the
  // normal UI without disturbing other panels.
  const [topologyOverlayOpen, setTopologyOverlayOpen] = useState(false);

  // Counter bumped whenever the source-control panel should re-fetch
  // git status — TurnCompleted bumps it, the user's manual refresh
  // button calls SourceControl's own refresh hook directly.
  const [gitRefreshNonce, setGitRefreshNonce] = useState(0);

  // Canvas tabs — opened by clicking file references in chat or files
  // tree. Empty list collapses the canvas to zero width so the chat
  // gets the full main area.
  const [canvasTabs, setCanvasTabs] = useState<CanvasTab[]>([]);
  const [activeCanvasTabId, setActiveCanvasTabId] = useState<string | null>(null);

  const openFileInCanvas = useCallback(async (path: string) => {
    const id = path;
    // If the tab already exists, focus it without re-reading. User can
    // hit the refresh button if they want a fresh read.
    setCanvasTabs((prev) => {
      if (prev.some((t) => t.id === id)) return prev;
      return [
        ...prev,
        {
          id,
          path,
          snapshot: null,
          error: null,
          reloading: false,
          mode: 'edit',
          draft: null,
          dirty: false,
          saving: false,
          ai_before_content: null,
        },
      ];
    });
    setActiveCanvasTabId(id);
    try {
      const snapshot = await fetchSnapshot(path);
      setCanvasTabs((prev) =>
        prev.map((t) =>
          t.id === id
            ? { ...t, snapshot, error: null, reloading: false }
            : t,
        ),
      );
    } catch (e) {
      setCanvasTabs((prev) =>
        prev.map((t) =>
          t.id === id
            ? { ...t, snapshot: null, error: String(e), reloading: false }
            : t,
        ),
      );
    }
  }, []);

  const closeCanvasTab = (id: string) => {
    // Confirm before closing a dirty tab — losing unsaved edits silently
    // would be a foot-gun. Keep the dialog terse so it doesn't
    // interrupt the user when they meant to discard.
    const tab = canvasTabs.find((t) => t.id === id);
    if (tab?.dirty) {
      const ok = confirm(`Discard unsaved changes to ${tab.path}?`);
      if (!ok) return;
    }
    setCanvasTabs((prev) => {
      const next = prev.filter((t) => t.id !== id);
      if (activeCanvasTabId === id) {
        const fallback = next.length > 0 ? next[next.length - 1].id : null;
        setActiveCanvasTabId(fallback);
      }
      return next;
    });
  };

  const reloadCanvasTab = async (id: string) => {
    const tab = canvasTabs.find((t) => t.id === id);
    if (!tab) return;
    // Reloading silently throws away the dirty buffer — ask first.
    if (tab.dirty) {
      const ok = confirm(`Reloading discards unsaved edits to ${tab.path}. Continue?`);
      if (!ok) return;
    }
    setCanvasTabs((prev) =>
      prev.map((t) => (t.id === id ? { ...t, reloading: true } : t)),
    );
    try {
      const snapshot = await fetchSnapshot(tab.path);
      setCanvasTabs((prev) =>
        prev.map((t) =>
          t.id === id
            ? {
                ...t,
                snapshot,
                error: null,
                reloading: false,
                draft: null,
                dirty: false,
              }
            : t,
        ),
      );
    } catch (e) {
      setCanvasTabs((prev) =>
        prev.map((t) =>
          t.id === id ? { ...t, error: String(e), reloading: false } : t,
        ),
      );
    }
  };

  const toggleCanvasTabMode = (id: string, target?: CanvasMode) => {
    setCanvasTabs((prev) =>
      prev.map((t) => {
        if (t.id !== id) return t;
        // Don't enter edit / diff on binary or errored snapshots —
        // the textarea would render an empty string and a save could
        // clobber binary contents.
        if (!t.snapshot || t.snapshot.is_binary) return t;
        // Refuse diff when no AI capture has populated the baseline.
        if (target === 'diff' && t.ai_before_content === null) {
          return t;
        }
        // Without an explicit target (old toggle call sites), flip
        // between edit and diff when a diff baseline exists; otherwise
        // stay in edit mode.
        const next: CanvasMode =
          target ??
          (t.mode === 'edit' && t.ai_before_content !== null
            ? 'diff'
            : 'edit');
        return { ...t, mode: next };
      }),
    );
  };

  /// Open a git diff in the Canvas. Called from the SourceControl panel
  /// when the user clicks a changed file. The "before" comes from
  /// `git show HEAD:gitPath` (or `:gitPath` for staged comparisons);
  /// the "after" is the on-disk content fetched by `openFileInCanvas`.
  ///
  /// Two paths because the workspace's `primary_root` may be a
  /// subdirectory of the git repo. `gitPath` is repo-relative (what
  /// the git RPC wants); `absPath` is the absolute on-disk path
  /// (which `read_file` knows how to handle via `resolve_workspace_path`).
  const openGitDiffInCanvas = async (
    absPath: string,
    gitPath: string,
    staged: boolean,
  ) => {
    try {
      const diff = await invoke<{
        path: string;
        head: string;
        working: string;
        staged: boolean;
      }>('git_file_diff', { path: gitPath, staged });
      await openFileInCanvas(absPath);
      setCanvasTabs((prev) =>
        prev.map((t) =>
          t.id === absPath
            ? {
                ...t,
                ai_before_content: diff.head,
                mode: 'diff',
              }
            : t,
        ),
      );
      setActiveCanvasTabId(absPath);
    } catch (e) {
      alert(`Failed to load git diff: ${e}`);
    }
  };

  /// Revert the AI's most recent change to a tab: write the captured
  /// `ai_before_content` back to disk and clear the buffer.
  const revertCanvasAiChange = async (id: string) => {
    const tab = canvasTabs.find((t) => t.id === id);
    if (!tab || tab.ai_before_content === null) return;
    setCanvasTabs((prev) =>
      prev.map((t) => (t.id === id ? { ...t, saving: true } : t)),
    );
    try {
      const snapshot = await saveFile(tab.path, tab.ai_before_content);
      setCanvasTabs((prev) =>
        prev.map((t) =>
          t.id === id
            ? {
                ...t,
                snapshot,
                ai_before_content: null,
                draft: null,
                dirty: false,
                saving: false,
                error: null,
                mode: 'edit',
              }
            : t,
        ),
      );
    } catch (e) {
      setCanvasTabs((prev) =>
        prev.map((t) => (t.id === id ? { ...t, saving: false } : t)),
      );
      alert(`Revert failed: ${e}`);
    }
  };

  /// Dismiss the captured AI change without writing anything. The
  /// AI's edit stays on disk; Canvas just stops showing the diff.
  const dismissCanvasAiChange = (id: string) => {
    setCanvasTabs((prev) =>
      prev.map((t) =>
        t.id === id
          ? {
              ...t,
              ai_before_content: null,
              mode: t.mode === 'diff' ? 'edit' : t.mode,
            }
          : t,
      ),
    );
  };

  const updateCanvasDraft = (id: string, draft: string) => {
    setCanvasTabs((prev) =>
      prev.map((t) => {
        if (t.id !== id) return t;
        const baseline = t.snapshot?.content ?? '';
        return { ...t, draft, dirty: draft !== baseline };
      }),
    );
  };

  const saveCanvasTab = async (id: string) => {
    const tab = canvasTabs.find((t) => t.id === id);
    if (!tab || !tab.snapshot || tab.draft === null) return;
    setCanvasTabs((prev) =>
      prev.map((t) => (t.id === id ? { ...t, saving: true } : t)),
    );
    try {
      const snapshot = await saveFile(tab.path, tab.draft);
      setCanvasTabs((prev) =>
        prev.map((t) =>
          t.id === id
            ? {
                ...t,
                snapshot,
                // After a successful save the on-disk version IS the
                // draft, so reset dirty and discard the cached draft
                // (next edit starts fresh).
                draft: null,
                dirty: false,
                saving: false,
                error: null,
              }
            : t,
        ),
      );
    } catch (e) {
      setCanvasTabs((prev) =>
        prev.map((t) => (t.id === id ? { ...t, saving: false } : t)),
      );
      alert(`Save failed: ${e}`);
    }
  };

  const loadSessionWorkers = useCallback(async (sessionId: string | null) => {
    if (!sessionId) {
      setSessionWorkers([]);
      return;
    }
    try {
      const list = await invoke<any[]>('list_session_workers', { sessionId });
      if (selectedSessionRef.current?.session_id && selectedSessionRef.current.session_id !== sessionId) {
        return;
      }
      setSessionWorkers(normalizeInstanceMetadataList(list));
    } catch (e) {
      console.error('Failed to load session workers:', e);
    }
  }, []);

  const handleResetCore = async () => {
    if (!selectedSession) return;
    try {
      await invoke('reset_core', { sessionId: selectedSession.session_id });
      addLog(new Date().toLocaleTimeString(), 'sys', 'Core instance reset — next message will respawn');
      await loadSessionWorkers(selectedSession.session_id);
    } catch (e) {
      addLog(new Date().toLocaleTimeString(), 'sys', `Reset Core failed: ${e}`);
    }
  };

  // Initial loading — workspaces first so list_sessions lands inside the
  // correct workspace scope. With no workspace selected, do not call
  // workspace-scoped session commands; show the welcome/no-workspace shell.
  useEffect(() => {
    (async () => {
      const current = await loadWorkspaces();
      if (current) {
        await loadSessions(current);
      } else {
        resetWorkspaceScopedUi();
      }
    })();
    loadAppConfig();
  }, []);

  // Replace the browser/native right-click menu across workbench chrome with
  // Switchyard/VSC-style menus. Keep native menus inside text inputs and any
  // explicit escape hatch so copying/editing text still feels normal.
  useEffect(() => {
    const handleContextMenu = (event: MouseEvent) => {
      const target = event.target as HTMLElement | null;
      if (
        target?.closest(
          'input, textarea, select, [contenteditable="true"], [data-allow-native-context-menu="true"]',
        )
      ) {
        return;
      }
      event.preventDefault();
    };
    window.addEventListener('contextmenu', handleContextMenu);
    return () => window.removeEventListener('contextmenu', handleContextMenu);
  }, []);

  const loadWorkspaces = async (): Promise<Workspace | null> => {
    try {
      const list = await invoke<Workspace[]>('list_workspaces');
      setWorkspaces(list);
      const current = await invoke<Workspace | null>('get_current_workspace');
      setCurrentWorkspace(current);
      return current;
    } catch (e) {
      console.error('Failed to load workspaces:', e);
      setWorkspaces([]);
      setCurrentWorkspace(null);
      return null;
    }
  };

  const handleSwitchWorkspace = async (workspaceId: string) => {
    try {
      const next = await invoke<Workspace>('set_current_workspace', { workspaceId });
      setCurrentWorkspace(next);
      // Wipe per-workspace UI state — sessions, turns, events all belong
      // to the previous workspace and would be confusing if left visible.
      resetWorkspaceScopedUi();
      void loadAppConfig();
      await loadSessions(next);
      addLog(new Date().toLocaleTimeString(), 'sys', `Switched workspace to: ${next.name}`);
    } catch (e) {
      console.error('Failed to switch workspace:', e);
      alert('Failed to switch workspace: ' + e);
    }
  };

  const handleCreateWorkspace = async (primaryRoot: string, name: string | null) => {
    try {
      const created = await invoke<Workspace>('create_workspace', { primaryRoot, name });
      setWorkspaces((prev) => [created, ...prev]);
      setCurrentWorkspace(created);
      resetWorkspaceScopedUi();
      void loadAppConfig();
      await loadSessions(created);
      addLog(new Date().toLocaleTimeString(), 'sys', `Created workspace: ${created.name}`);
    } catch (e) {
      console.error('Failed to create workspace:', e);
      alert('Failed to create workspace: ' + e);
    }
  };

  const handleRenameWorkspace = async (workspaceId: string, name: string) => {
    try {
      const updated = await invoke<Workspace>('update_workspace', {
        workspaceId,
        name,
        extraRoots: null,
      });
      setWorkspaces((prev) =>
        prev.map((w) => (w.workspace_id === workspaceId ? updated : w)),
      );
      if (currentWorkspace?.workspace_id === workspaceId) {
        setCurrentWorkspace(updated);
      }
    } catch (e) {
      console.error('Failed to rename workspace:', e);
      alert('Failed to rename workspace: ' + e);
    }
  };

  const handleUpdateExtraRoots = async (
    workspaceId: string,
    extraRoots: string[],
  ) => {
    try {
      const updated = await invoke<Workspace>('update_workspace', {
        workspaceId,
        name: null,
        extraRoots,
      });
      setWorkspaces((prev) =>
        prev.map((w) => (w.workspace_id === workspaceId ? updated : w)),
      );
      if (currentWorkspace?.workspace_id === workspaceId) {
        setCurrentWorkspace(updated);
      }
    } catch (e) {
      console.error('Failed to update extra roots:', e);
      alert('Failed to update extra roots: ' + e);
    }
  };

  const pickFolder = async (title: string): Promise<string | null> => {
    try {
      const picked = await openDialog({
        directory: true,
        multiple: false,
        title,
      });
      return typeof picked === 'string' ? picked : null;
    } catch (e) {
      console.error('Failed to open folder picker:', e);
      alert('Folder picker failed: ' + e);
      return null;
    }
  };

  const handleOpenFolderAsWorkspace = async () => {
    const path = await pickFolder('Open Folder as Workspace');
    if (!path) return;
    await handleCreateWorkspace(path, leafName(path) || null);
  };

  const handleCloseWorkspace = async () => {
    if (!currentWorkspace) return;
    try {
      await invoke('clear_current_workspace');
      setCurrentWorkspace(null);
      resetWorkspaceScopedUi();
      setGitRefreshNonce((nonce) => nonce + 1);
      void loadAppConfig();
      addLog(new Date().toLocaleTimeString(), 'sys', 'Closed workspace');
    } catch (e) {
      console.error('Failed to close workspace:', e);
      alert('Failed to close workspace: ' + e);
    }
  };

  const handleAddFolderToCurrentWorkspace = async () => {
    if (!currentWorkspace) {
      await handleOpenFolderAsWorkspace();
      return;
    }
    const path = await pickFolder('Add Folder to Workspace');
    if (!path) return;
    const pickedKey = normalizePathKey(path);
    const primaryKey = normalizePathKey(currentWorkspace.primary_root);
    const extraKeys = new Set(currentWorkspace.extra_roots.map(normalizePathKey));
    if (pickedKey === primaryKey || extraKeys.has(pickedKey)) return;
    await handleUpdateExtraRoots(currentWorkspace.workspace_id, [
      ...currentWorkspace.extra_roots,
      path,
    ]);
  };

  const handleRemoveExtraRoot = async (root: string) => {
    if (!currentWorkspace) return;
    const rootKey = normalizePathKey(root);
    await handleUpdateExtraRoots(
      currentWorkspace.workspace_id,
      currentWorkspace.extra_roots.filter((item) => normalizePathKey(item) !== rootKey),
    );
  };

  const persistentTeamWorkerCount = useMemo(
    () => sessionWorkers.filter((w) => w.kind === 'worker').length,
    [sessionWorkers],
  );
  const activeRuntimeWorkerCount = useMemo(() => {
    const activeWorkerTurnIds = new Set<string>();
    turns.forEach((turn) => {
      if (turn.role !== 'core' && (turn.status === 'pending' || turn.status === 'running')) {
        activeWorkerTurnIds.add(turn.turn_id);
      }
    });
    activeTurnIds.forEach((turnId) => activeWorkerTurnIds.add(turnId));
    if (activePeerTurnId) activeWorkerTurnIds.add(activePeerTurnId);
    return activeWorkerTurnIds.size;
  }, [turns, activeTurnIds, activePeerTurnId]);
  const activeHyardWorkerCount = useMemo(
    () => countActiveHyardJobs(hyardJobs),
    [hyardJobs],
  );
  const displayedWorkerCount = useMemo(
    () => Math.max(persistentTeamWorkerCount, activeRuntimeWorkerCount) + activeHyardWorkerCount,
    [persistentTeamWorkerCount, activeRuntimeWorkerCount, activeHyardWorkerCount],
  );
  const workerRosterSyncActive = useMemo(() => (
    isGenerating ||
    Boolean(activeCoreTurnId) ||
    Boolean(activePeerTurnId) ||
    activeTurnIds.length > 0 ||
    activeHyardWorkerCount > 0
  ), [isGenerating, activeCoreTurnId, activePeerTurnId, activeTurnIds.length, activeHyardWorkerCount]);

  // Worker roster is event-first, but keep a light resync while a turn or
  // background delegate is active. That prevents a missed/mis-shaped Worker*
  // event from permanently freezing the status-bar count at zero.
  useEffect(() => {
    if (!selectedSession) {
      setSessionWorkers([]);
      return;
    }
    const sessionId = selectedSession.session_id;
    void loadSessionWorkers(sessionId);
    if (!workerRosterSyncActive) return;
    const timer = window.setInterval(() => {
      void loadSessionWorkers(sessionId);
    }, 2000);
    return () => window.clearInterval(timer);
  }, [selectedSession?.session_id, workerRosterSyncActive, loadSessionWorkers]);

  // Update selectedAgentTurnId from core-agent or temp-user-id to activeCoreTurnId when activeCoreTurnId is resolved
  useEffect(() => {
    if (activeCoreTurnId && (selectedAgentTurnId === 'core-agent' || selectedAgentTurnId === 'temp-user-id')) {
      setSelectedAgentTurnId(activeCoreTurnId);
    }
  }, [activeCoreTurnId, selectedAgentTurnId]);

  const beginRuntimeDispatch = (phase?: string, startedAt = Date.now()) => {
    const existing = runtimeDispatchStartedAtRef.current;
    const nextStartedAt = existing ?? startedAt;
    runtimeDispatchStartedAtRef.current = nextStartedAt;
    setRuntimeDispatchStartedAt((prev) => {
      const next = prev ?? nextStartedAt;
      runtimeDispatchStartedAtRef.current = next;
      return next;
    });
    if (phase !== undefined) {
      setRuntimePreparingPhase(phase);
    }
  };

  const clearRuntimeDispatch = () => {
    runtimeDispatchStartedAtRef.current = null;
    setRuntimeDispatchStartedAt(null);
    setRuntimePreparingPhase(null);
  };

  const seedRuntimeTurnStartedAtFromDispatch = (turnId: unknown) => {
    if (typeof turnId !== 'string' || !turnId) return;
    const startedAt = runtimeDispatchStartedAtRef.current;
    if (!startedAt) return;
    setRuntimeTurnStartedAt((prev) => (prev[turnId] ? prev : { ...prev, [turnId]: startedAt }));
  };

  const markRuntimeTurnPhase = (turnId: unknown, phase: RuntimeTurnPhase) => {
    if (typeof turnId !== 'string' || !turnId) return;
    const now = Date.now();
    setRuntimeTurnPhases((prev) => (prev[turnId] === phase ? prev : { ...prev, [turnId]: phase }));
    setRuntimeTurnPhaseChangedAt((prev) => ({ ...prev, [turnId]: now }));
    if (phase === 'running' || phase === 'output_completed' || phase === 'finalizing') {
      setRuntimeTurnStartedAt((prev) => (prev[turnId] ? prev : { ...prev, [turnId]: now }));
    }
  };

  const ensureRuntimeTurnPhase = (turnId: unknown, phase: RuntimeTurnPhase) => {
    if (typeof turnId !== 'string' || !turnId) return;
    const now = Date.now();
    setRuntimeTurnPhases((prev) => (prev[turnId] ? prev : { ...prev, [turnId]: phase }));
    setRuntimeTurnPhaseChangedAt((prev) => (prev[turnId] ? prev : { ...prev, [turnId]: now }));
    if (phase === 'running' || phase === 'output_completed' || phase === 'finalizing') {
      setRuntimeTurnStartedAt((prev) => (prev[turnId] ? prev : { ...prev, [turnId]: now }));
    }
  };

  const resetRealtimeTerminalForTurn = (turnId: unknown) => {
    if (typeof turnId !== 'string' || !turnId) return;
    realtimeTerminalPendingRef.current.delete(turnId);
    realtimeTerminalBuffersRef.current = { ...realtimeTerminalBuffersRef.current, [turnId]: '' };
    setRealtimeTerminalLines((prev) => ({ ...prev, [turnId]: [] }));
  };

  const ensureRealtimeTerminalForTurn = (turnId: unknown) => {
    if (typeof turnId !== 'string' || !turnId) return;
    if (!Object.prototype.hasOwnProperty.call(realtimeTerminalBuffersRef.current, turnId)) {
      realtimeTerminalBuffersRef.current = { ...realtimeTerminalBuffersRef.current, [turnId]: '' };
    }
    setRealtimeTerminalLines((prev) => (prev[turnId] ? prev : { ...prev, [turnId]: [] }));
  };

  const flushRealtimeTerminalText = useCallback(() => {
    realtimeTerminalFlushTimerRef.current = null;
    const pending = realtimeTerminalPendingRef.current;
    if (pending.size === 0) return;

    const updates: Record<string, string[]> = {};
    pending.forEach((chunk, turnId) => {
      const nextText = trimRealtimeTextBuffer((realtimeTerminalBuffersRef.current[turnId] || '') + chunk);
      realtimeTerminalBuffersRef.current[turnId] = nextText;
      updates[turnId] = realtimeTextToLines(nextText);
    });
    pending.clear();

    setRealtimeTerminalLines((prev) => {
      const entries = Object.entries(updates);
      if (entries.length === 0) return prev;
      const next = { ...prev };
      entries.forEach(([turnId, lines]) => {
        next[turnId] = lines;
      });
      return next;
    });
  }, []);

  const scheduleRealtimeTerminalFlush = useCallback(() => {
    if (realtimeTerminalFlushTimerRef.current !== null) return;
    realtimeTerminalFlushTimerRef.current = window.setTimeout(flushRealtimeTerminalText, 33);
  }, [flushRealtimeTerminalText]);

  const appendRealtimeTerminalText = (turnId: unknown, text: unknown) => {
    if (typeof turnId !== 'string' || !turnId || typeof text !== 'string' || text.length === 0) return;
    const normalizedText = normalizeRealtimeText(text);
    const pending = realtimeTerminalPendingRef.current;
    pending.set(turnId, (pending.get(turnId) || '') + normalizedText);
    scheduleRealtimeTerminalFlush();
  };

  const clearRealtimeTerminalText = useCallback(() => {
    if (realtimeTerminalFlushTimerRef.current !== null) {
      window.clearTimeout(realtimeTerminalFlushTimerRef.current);
      realtimeTerminalFlushTimerRef.current = null;
    }
    realtimeTerminalPendingRef.current.clear();
    realtimeTerminalBuffersRef.current = {};
    setRealtimeTerminalLines({});
  }, []);

  useEffect(() => () => {
    if (realtimeTerminalFlushTimerRef.current !== null) {
      window.clearTimeout(realtimeTerminalFlushTimerRef.current);
      realtimeTerminalFlushTimerRef.current = null;
    }
  }, []);

  const materializeRealtimeTerminalSnapshot = () => {
    const buffers = { ...realtimeTerminalBuffersRef.current };
    realtimeTerminalPendingRef.current.forEach((chunk, turnId) => {
      buffers[turnId] = trimRealtimeTextBuffer((buffers[turnId] || '') + chunk);
    });
    const lines: Record<string, string[]> = { ...realtimeTerminalLines };
    Object.entries(buffers).forEach(([turnId, text]) => {
      lines[turnId] = realtimeTextToLines(text);
    });
    return { buffers, lines };
  };

  const patchSessionUiSnapshot = (
    sessionId: string | null | undefined,
    patch:
      | Partial<SessionUiSnapshot>
      | ((previous: SessionUiSnapshot) => SessionUiSnapshot),
  ) => {
    if (!sessionId) return createEmptySessionUiSnapshot();
    const previous = sessionUiSnapshotsRef.current[sessionId] ?? createEmptySessionUiSnapshot();
    const next = typeof patch === 'function'
      ? patch(previous)
      : {
          ...previous,
          ...patch,
          loadedAt: patch.loadedAt ?? Date.now(),
        };
    sessionUiSnapshotsRef.current = {
      ...sessionUiSnapshotsRef.current,
      [sessionId]: next,
    };
    return next;
  };

  const buildVisibleSessionUiSnapshot = (
    sessionId: string,
    options?: { loaded?: boolean },
  ): SessionUiSnapshot => {
    const terminal = materializeRealtimeTerminalSnapshot();
    const previous = sessionUiSnapshotsRef.current[sessionId];
    return {
      ...(previous ?? createEmptySessionUiSnapshot()),
      turns: turnsRef.current,
      sessionEvents,
      sessionWorkers,
      activeCoreText,
      activePeerText,
      activePeerName,
      activeNodes,
      activeTurnIds,
      activeCoreTurnId,
      activePeerTurnId,
      selectedAgentTurnId,
      hyardJobs,
      realtimeTerminalLines: terminal.lines,
      realtimeTerminalBuffers: terminal.buffers,
      runtimeTurnPhases,
      runtimeTurnStartedAt,
      runtimeTurnPhaseChangedAt,
      runtimeDispatchStartedAt: runtimeDispatchStartedAtRef.current,
      runtimePreparingPhase,
      isGenerating,
      messageQueue: messageQueueRef.current,
      loaded: options?.loaded ?? previous?.loaded ?? true,
      loadedAt: Date.now(),
    };
  };

  const captureCurrentSessionUiSnapshot = (
    sessionId = selectedSessionRef.current?.session_id,
    options?: { loaded?: boolean },
  ) => {
    if (!sessionId) return;
    const snapshot = buildVisibleSessionUiSnapshot(sessionId, options);
    sessionUiSnapshotsRef.current = {
      ...sessionUiSnapshotsRef.current,
      [sessionId]: snapshot,
    };
  };

  const replaceRealtimeTerminalSnapshot = (snapshot: SessionUiSnapshot) => {
    if (realtimeTerminalFlushTimerRef.current !== null) {
      window.clearTimeout(realtimeTerminalFlushTimerRef.current);
      realtimeTerminalFlushTimerRef.current = null;
    }
    realtimeTerminalPendingRef.current.clear();
    realtimeTerminalBuffersRef.current = { ...snapshot.realtimeTerminalBuffers };
    setRealtimeTerminalLines(snapshot.realtimeTerminalLines);
  };

  const applySessionUiSnapshot = (snapshot?: SessionUiSnapshot) => {
    const next = snapshot ?? createEmptySessionUiSnapshot();
    unstable_batchedUpdates(() => {
      commitTurns(next.turns);
      setSessionEvents(next.sessionEvents);
      setSessionWorkers(next.sessionWorkers);
      setActiveCoreText(next.activeCoreText);
      setActivePeerText(next.activePeerText);
      setActivePeerName(next.activePeerName);
      setActiveNodes(next.activeNodes);
      setActiveTurnIds(next.activeTurnIds);
      setActiveCoreTurnId(next.activeCoreTurnId);
      setActivePeerTurnId(next.activePeerTurnId);
      setSelectedAgentTurnId(next.selectedAgentTurnId);
      setHyardJobs(next.hyardJobs);
      setRuntimeTurnPhases(next.runtimeTurnPhases);
      setRuntimeTurnStartedAt(next.runtimeTurnStartedAt);
      setRuntimeTurnPhaseChangedAt(next.runtimeTurnPhaseChangedAt);
      runtimeDispatchStartedAtRef.current = next.runtimeDispatchStartedAt;
      setRuntimeDispatchStartedAt(next.runtimeDispatchStartedAt);
      setRuntimePreparingPhase(next.runtimePreparingPhase);
      messageQueueRef.current = next.messageQueue;
      setMessageQueue(next.messageQueue);
      setIsGenerating(next.isGenerating);
      replaceRealtimeTerminalSnapshot(next);
    });
  };

  useEffect(() => {
    const sessionId = selectedSessionRef.current?.session_id;
    if (!sessionId) return;
    const terminal = materializeRealtimeTerminalSnapshot();
    patchSessionUiSnapshot(sessionId, {
      turns,
      sessionEvents,
      sessionWorkers,
      activeCoreText,
      activePeerText,
      activePeerName,
      activeNodes,
      activeTurnIds,
      activeCoreTurnId,
      activePeerTurnId,
      selectedAgentTurnId,
      hyardJobs,
      realtimeTerminalLines: terminal.lines,
      realtimeTerminalBuffers: terminal.buffers,
      runtimeTurnPhases,
      runtimeTurnStartedAt,
      runtimeTurnPhaseChangedAt,
      runtimeDispatchStartedAt: runtimeDispatchStartedAtRef.current,
      runtimePreparingPhase,
      isGenerating,
      messageQueue,
      loaded: true,
    });
  }, [
    turns,
    sessionEvents,
    sessionWorkers,
    activeCoreText,
    activePeerText,
    activePeerName,
    activeNodes,
    activeTurnIds,
    activeCoreTurnId,
    activePeerTurnId,
    selectedAgentTurnId,
    hyardJobs,
    realtimeTerminalLines,
    runtimeTurnPhases,
    runtimeTurnStartedAt,
    runtimeTurnPhaseChangedAt,
    runtimeDispatchStartedAt,
    runtimePreparingPhase,
    isGenerating,
    messageQueue,
  ]);

  const updateSessionEventsCursor = (sessionId: string, events: any[]) => {
    const nextCursor = maxSessionEventTimestamp(events, sessionEventsCursorRef.current[sessionId]);
    if (nextCursor) {
      sessionEventsCursorRef.current = {
        ...sessionEventsCursorRef.current,
        [sessionId]: nextCursor,
      };
    }
  };

  const seedSessionEventCursorFromSnapshot = (
    sessionId: string,
    snapshot?: SessionUiSnapshot,
  ) => {
    if (!snapshot?.sessionEvents?.length || sessionEventsCursorRef.current[sessionId]) return;
    const persistedEvents = snapshot.sessionEvents.filter((event) => {
      const eventId = event?.event_id;
      return typeof eventId !== 'string' || !eventId.startsWith('live:');
    });
    const nextCursor = maxSessionEventTimestamp(persistedEvents);
    if (!nextCursor) return;
    sessionEventsCursorRef.current = {
      ...sessionEventsCursorRef.current,
      [sessionId]: nextCursor,
    };
  };

  const fetchSessionEventsForMerge = async (sessionId: string, mode: 'full' | 'incremental' = 'incremental') => {
    const afterTimestamp = mode === 'incremental' ? sessionEventsCursorRef.current[sessionId] : undefined;
    const eventList = await invoke<any[]>('get_session_events', {
      sessionId,
      ...(afterTimestamp ? { afterTimestamp } : {}),
    });
    updateSessionEventsCursor(sessionId, eventList);
    return eventList;
  };

  const fetchRuntimeSnapshot = async (sessionId: string, mode: 'full' | 'incremental' = 'incremental') => {
    const afterEventId = mode === 'incremental' ? (runtimeSnapshotCursorRef.current[sessionId] ?? 0) : 0;
    const snapshot = await invoke<RuntimeSnapshot>('get_runtime_snapshot', {
      sessionId,
      afterEventId,
      eventLimit: mode === 'full' ? 512 : 256,
      jobLimit: 512,
    });
    const maxEventId = Number(snapshot?.max_event_id ?? 0);
    if (Number.isFinite(maxEventId)) {
      runtimeSnapshotCursorRef.current = {
        ...runtimeSnapshotCursorRef.current,
        [sessionId]: Math.max(runtimeSnapshotCursorRef.current[sessionId] ?? 0, maxEventId),
      };
    }
    return snapshot;
  };

  // Listen for Tauri events
  useEffect(() => {
    let active = true;
    const unlistenFns: Array<() => void> = [];
    let refreshTurnsTimer: number | null = null;
    let refreshTurnsInFlight = false;
    let refreshTurnsQueued = false;
    let workerRefreshTimer: number | null = null;
    const pendingWorkerRefreshSessionIds = new Set<string>();

    const setupListener = async () => {
      const runtimeDebug = debugRuntimeEventsEnabled();
      if (runtimeDebug) console.debug('[runtime_event] listener attached');
      const refreshTurns = async () => {
        if (!active) return;
        const session = selectedSessionRef.current;
        const sessionId = session?.session_id;
        if (runtimeDebug) console.debug('[runtime_event] refreshTurns', { session_id: sessionId || null });
        if (!session || !sessionId) return;
        try {
          const turnList = await invoke<Turn[]>('get_session_turns', { sessionId });
          if (!active || selectedSessionRef.current?.session_id !== sessionId) return;
          if (runtimeDebug) console.debug('[runtime_event] turns loaded', { session_id: sessionId, count: turnList.length });
          const turnListWithFinalResponses = applyRememberedFinalTurnResponses(turnList);
          reconcileTurnAttachmentBindings(sessionId, turnListWithFinalResponses);
          // A refresh can race with the final runtime event / run_turn return.
          // Treat DB data as authoritative for ordering/status, but never let a
          // transient empty provider_response erase a final answer already seen
          // by the UI.
          commitTurns((prev) => mergeFreshTurnsPreservingKnownResponses(prev, turnListWithFinalResponses));
          const eventList = await fetchSessionEventsForMerge(sessionId, 'incremental');
          if (!active || selectedSessionRef.current?.session_id !== sessionId) return;
          // DB writes can lag a live runtime event by a few milliseconds. Merge
          // instead of wholesale replacement so a refresh triggered by a status
          // event cannot erase just-rendered streaming/tool cards.
          setSessionEvents((prev) => mergeSessionEventLists(prev, eventList));
          const runtimeSnapshot = await fetchRuntimeSnapshot(sessionId, 'incremental');
          if (!active || selectedSessionRef.current?.session_id !== sessionId) return;
          const runtimeHyardJobs = hyardJobsFromRuntimeSnapshot(runtimeSnapshot);
          if (Object.keys(runtimeHyardJobs).length > 0) {
            setHyardJobs((prev) => ({
              ...prev,
              ...runtimeHyardJobs,
            }));
          }
        } catch (e) {
          console.error('Error fetching session turns/events:', e);
        }
      };
      const runScheduledRefreshTurns = async () => {
        if (!active) return;
        if (refreshTurnsInFlight) {
          refreshTurnsQueued = true;
          return;
        }
        refreshTurnsInFlight = true;
        try {
          await refreshTurns();
        } finally {
          refreshTurnsInFlight = false;
          if (refreshTurnsQueued && active) {
            refreshTurnsQueued = false;
            scheduleRefreshTurns(50);
          }
        }
      };
      const scheduleRefreshTurns = (delayMs = 80) => {
        if (!active) return;
        if (refreshTurnsInFlight) {
          refreshTurnsQueued = true;
          return;
        }
        if (refreshTurnsTimer !== null) return;
        refreshTurnsTimer = window.setTimeout(() => {
          refreshTurnsTimer = null;
          void runScheduledRefreshTurns();
        }, delayMs);
      };
      const refreshSessionWorkers = async (sessionIdValue: any) => {
        if (!active) return;
        const sessionId = normalizedString(sessionIdValue);
        if (!sessionId || selectedSessionRef.current?.session_id !== sessionId) return;
        try {
          const list = await invoke<any[]>('list_session_workers', { sessionId });
          if (!active || selectedSessionRef.current?.session_id !== sessionId) return;
          setSessionWorkers(normalizeInstanceMetadataList(list));
        } catch (e) {
          console.error('Failed to refresh session workers:', e);
        }
      };
      const flushWorkerRefreshes = () => {
        workerRefreshTimer = null;
        const sessionIds = Array.from(pendingWorkerRefreshSessionIds);
        pendingWorkerRefreshSessionIds.clear();
        sessionIds.forEach((sessionId) => {
          void refreshSessionWorkers(sessionId);
        });
      };
      const scheduleRefreshSessionWorkers = (sessionIdValue: any, delayMs = 150) => {
        if (!active) return;
        const sessionId = normalizedString(sessionIdValue);
        if (!sessionId || selectedSessionRef.current?.session_id !== sessionId) return;
        pendingWorkerRefreshSessionIds.add(sessionId);
        if (workerRefreshTimer !== null) return;
        workerRefreshTimer = window.setTimeout(flushWorkerRefreshes, delayMs);
      };

      type RuntimeEventBatchContext = {
        sessionEventUpserts: any[];
        logs: TelemetryLog[];
      };
      let runtimeEventBatchContext: RuntimeEventBatchContext | null = null;
      const enqueueRuntimeItemEvent = (data: any) => {
        if (runtimeEventBatchContext) {
          runtimeEventBatchContext.sessionEventUpserts.push(data);
          return;
        }
        setSessionEvents((prev) => upsertRuntimeItemEvent(prev, data));
      };
      const enqueueLogs = (logs: TelemetryLog[]) => {
        if (logs.length === 0) return;
        if (runtimeEventBatchContext) {
          runtimeEventBatchContext.logs.push(...logs);
          return;
        }
        addLogs(logs);
      };
      const enqueueLog = (time: string, tag: 'core' | 'peer' | 'sys' | 'info', message: string) => {
        enqueueLogs([{ timestamp: time, tag, message }]);
      };
      const flushRuntimeEventBatchContext = (context: RuntimeEventBatchContext) => {
        if (context.sessionEventUpserts.length > 0) {
          setSessionEvents((prev) => upsertRuntimeItemEvents(prev, context.sessionEventUpserts));
        }
        if (context.logs.length > 0) {
          addLogs(context.logs);
        }
      };

      const rememberRuntimeTurnSession = (turnIdValue: unknown, sessionIdValue: unknown) => {
        const turnId = normalizedString(turnIdValue);
        const sessionId = normalizedString(sessionIdValue);
        if (!turnId || !sessionId) return;
        const nowMs = Date.now();
        runtimeTurnSessionIdRef.current = {
          ...pruneRuntimeTurnSessionCache(runtimeTurnSessionIdRef.current, nowMs),
          [turnId]: { sessionId, lastSeenAt: nowMs },
        };
      };

      const forgetRuntimeTurnSession = (turnIdValue: unknown) => {
        const turnId = normalizedString(turnIdValue);
        if (!turnId || !runtimeTurnSessionIdRef.current[turnId]) return;
        const next = { ...runtimeTurnSessionIdRef.current };
        delete next[turnId];
        runtimeTurnSessionIdRef.current = next;
      };

      const runtimeEventSessionId = (_type: string, data: any): string | null => {
        const explicit = normalizedString(data?.session_id);
        if (explicit) return explicit;
        const turnId = normalizedString(data?.turn_id ?? data?.core_turn_id ?? data?.in_flight_turn_id);
        const remembered = turnId ? runtimeTurnSessionIdRef.current[turnId] : null;
        if (remembered) {
          remembered.lastSeenAt = Date.now();
          return remembered.sessionId;
        }
        return null;
      };

      const runtimeEventMarksRunning = (type: string) => (
        type === 'TurnPreparing' ||
        type === 'CoreTurnStarted' ||
        type === 'CoreExecutionTelemetry' ||
        type === 'CoreItemUpdated' ||
        type === 'CoreTerminalOutput' ||
        type === 'DelegateRequested' ||
        type === 'DelegateCompleted' ||
        type === 'HyardJobObserved' ||
        type === 'CoreOutputCompleted' ||
        type === 'PeerTurnStarted' ||
        type === 'PeerExecutionTelemetry' ||
        type === 'PeerItemUpdated' ||
        type === 'PeerTerminalOutput' ||
        type === 'PeerOutputCompleted' ||
        type === 'FinalizationStarted' ||
        type === 'CallbackReceiptsInjected'
      );

      const patchBackgroundRuntimeSnapshot = (sessionId: string, type: string, data: any) => {
        patchSessionUiSnapshot(sessionId, (previous) => {
          let next: SessionUiSnapshot = {
            ...previous,
            isGenerating: type === 'TurnCompleted' || type === 'TurnFailed'
              ? false
              : runtimeEventMarksRunning(type) || previous.isGenerating,
            loadedAt: Date.now(),
          };
          const provider = normalizedString(data?.provider);
          const turnId = normalizedString(data?.turn_id);

          switch (type) {
            case 'TurnPreparing': {
              const startedAt = next.runtimeDispatchStartedAt ?? Date.now();
              return {
                ...next,
                activeCoreText: '',
                activePeerText: '',
                activePeerName: null,
                activeNodes: ['host', provider ?? 'provider'],
                activeTurnIds: [],
                runtimeDispatchStartedAt: startedAt,
                runtimePreparingPhase: data.phase ?? '正在准备并启动 core provider…',
                isGenerating: true,
              };
            }
            case 'CoreTurnStarted':
              rememberRuntimeTurnSession(turnId, sessionId);
              next = {
                ...next,
                activeCoreText: '',
                activePeerText: '',
                activeNodes: ['host', provider ?? 'core'],
                activeTurnIds: [],
                activeCoreTurnId: turnId,
                hyardJobs: {},
                runtimeDispatchStartedAt: null,
                runtimePreparingPhase: null,
                isGenerating: true,
              };
              next = sessionUiSnapshotResetTerminal(next, turnId);
              next = sessionUiSnapshotSeedStartedAt(next, turnId, previous.runtimeDispatchStartedAt);
              return sessionUiSnapshotWithRuntimePhase(next, turnId, 'running');
            case 'FinalizationStarted':
              rememberRuntimeTurnSession(turnId, sessionId);
              next = {
                ...next,
                activeCoreText: '',
                activePeerText: '',
                activePeerName: null,
                activeNodes: ['host', provider ?? 'core'],
                activeTurnIds: [],
                activeCoreTurnId: turnId,
                runtimeDispatchStartedAt: null,
                runtimePreparingPhase: null,
                isGenerating: true,
              };
              next = sessionUiSnapshotEnsureTerminal(next, turnId);
              next = sessionUiSnapshotSeedStartedAt(next, turnId, previous.runtimeDispatchStartedAt);
              return sessionUiSnapshotWithRuntimePhase(next, turnId, 'finalizing');
            case 'CoreItemUpdated': {
              if (turnId) {
                rememberRuntimeTurnSession(turnId, sessionId);
                next = {
                  ...next,
                  activeCoreTurnId: next.activeCoreTurnId ?? turnId,
                  isGenerating: true,
                };
                next = sessionUiSnapshotWithRuntimePhase(next, turnId, 'running', { ensureOnly: true });
                next = sessionUiSnapshotEnsureTerminal(next, turnId);
              }
              if (isRuntimeReasoningEvent(data)) {
                if (!isEmptyReasoningRuntimeEvent(data) && data.payload) {
                  next = {
                    ...next,
                    sessionEvents: upsertRuntimeItemEvent(next.sessionEvents, data),
                  };
                }
                return next;
              }
              {
                const itemText = typeof data.text === 'string' ? data.text : '';
                if (isSystemStatusText(itemText)) {
                  if (turnId && itemText.trim()) {
                    next = sessionUiSnapshotAppendTerminalText(
                      next,
                      turnId,
                      itemText.endsWith('\n') ? itemText : `${itemText}\n`,
                    );
                  }
                } else if (hasProviderTextUpdate(itemText, data.payload)) {
                  next = {
                    ...next,
                    activeCoreText: applyProviderTextUpdate(next.activeCoreText, itemText, data.payload),
                  };
                }
              }
              if (data.payload) {
                next = {
                  ...next,
                  sessionEvents: upsertRuntimeItemEvent(next.sessionEvents, data),
                };
              }
              return next;
            }
            case 'PeerTurnStarted':
              rememberRuntimeTurnSession(turnId, sessionId);
              next = {
                ...next,
                activeNodes: provider && !next.activeNodes.includes(provider)
                  ? [...next.activeNodes, provider]
                  : next.activeNodes,
                activeTurnIds: turnId && !next.activeTurnIds.includes(turnId)
                  ? [...next.activeTurnIds, turnId]
                  : next.activeTurnIds,
                activePeerName: provider,
                activePeerText: '',
                activePeerTurnId: turnId,
                isGenerating: true,
              };
              next = sessionUiSnapshotResetTerminal(next, turnId);
              return sessionUiSnapshotWithRuntimePhase(next, turnId, 'running');
            case 'PeerItemUpdated': {
              if (turnId) {
                rememberRuntimeTurnSession(turnId, sessionId);
                next = {
                  ...next,
                  activePeerTurnId: next.activePeerTurnId ?? turnId,
                  activePeerName: next.activePeerName ?? provider,
                  activeNodes: provider && !next.activeNodes.includes(provider)
                    ? [...next.activeNodes, provider]
                    : next.activeNodes,
                  activeTurnIds: !next.activeTurnIds.includes(turnId)
                    ? [...next.activeTurnIds, turnId]
                    : next.activeTurnIds,
                  isGenerating: true,
                };
                next = sessionUiSnapshotWithRuntimePhase(next, turnId, 'running', { ensureOnly: true });
                next = sessionUiSnapshotEnsureTerminal(next, turnId);
              }
              if (isRuntimeReasoningEvent(data)) {
                if (!isEmptyReasoningRuntimeEvent(data) && data.payload) {
                  next = {
                    ...next,
                    sessionEvents: upsertRuntimeItemEvent(next.sessionEvents, data),
                  };
                }
                return next;
              }
              {
                const itemText = typeof data.text === 'string' ? data.text : '';
                if (isSystemStatusText(itemText)) {
                  if (turnId && itemText.trim()) {
                    next = sessionUiSnapshotAppendTerminalText(
                      next,
                      turnId,
                      itemText.endsWith('\n') ? itemText : `${itemText}\n`,
                    );
                  }
                } else if (hasProviderTextUpdate(itemText, data.payload)) {
                  next = {
                    ...next,
                    activePeerText: applyProviderTextUpdate(next.activePeerText, itemText, data.payload),
                  };
                }
              }
              if (data.payload) {
                next = {
                  ...next,
                  sessionEvents: upsertRuntimeItemEvent(next.sessionEvents, data),
                };
              }
              return next;
            }
            case 'CoreExecutionTelemetry':
            case 'PeerExecutionTelemetry':
              if (turnId) {
                rememberRuntimeTurnSession(turnId, sessionId);
                next = type === 'CoreExecutionTelemetry'
                  ? { ...next, activeCoreTurnId: next.activeCoreTurnId ?? turnId, isGenerating: true }
                  : {
                      ...next,
                      activePeerTurnId: next.activePeerTurnId ?? turnId,
                      activePeerName: next.activePeerName ?? provider,
                      activeNodes: provider && !next.activeNodes.includes(provider)
                        ? [...next.activeNodes, provider]
                        : next.activeNodes,
                      activeTurnIds: !next.activeTurnIds.includes(turnId)
                        ? [...next.activeTurnIds, turnId]
                        : next.activeTurnIds,
                      isGenerating: true,
                    };
                next = sessionUiSnapshotWithRuntimePhase(next, turnId, 'running', { ensureOnly: true });
                next = sessionUiSnapshotEnsureTerminal(next, turnId);
              }
              if (data.execution) {
                return {
                  ...next,
                  sessionEvents: upsertRuntimeItemEvent(next.sessionEvents, {
                    ...data,
                    event_type: RUNTIME_ITEM_EVENT_FALLBACK,
                    payload: { item_type: 'execution_telemetry', execution: data.execution },
                  }),
                };
              }
              return next;
            case 'CoreTerminalOutput':
            case 'PeerTerminalOutput':
              if (turnId) {
                rememberRuntimeTurnSession(turnId, sessionId);
                next = type === 'CoreTerminalOutput'
                  ? { ...next, activeCoreTurnId: next.activeCoreTurnId ?? turnId, isGenerating: true }
                  : {
                      ...next,
                      activePeerTurnId: next.activePeerTurnId ?? turnId,
                      activePeerName: next.activePeerName ?? provider,
                      activeNodes: provider && !next.activeNodes.includes(provider)
                        ? [...next.activeNodes, provider]
                        : next.activeNodes,
                      activeTurnIds: !next.activeTurnIds.includes(turnId)
                        ? [...next.activeTurnIds, turnId]
                        : next.activeTurnIds,
                      isGenerating: true,
                    };
                next = sessionUiSnapshotWithRuntimePhase(next, turnId, 'running', { ensureOnly: true });
                next = sessionUiSnapshotAppendTerminalText(next, turnId, data.text);
              }
              return next;
            case 'HyardJobObserved':
              if (turnId) rememberRuntimeTurnSession(turnId, sessionId);
              {
                const job = hyardJobRecordFromRuntimeEvent(data);
                if (!job) return next;
                return {
                  ...next,
                  hyardJobs: upsertHyardJobRecord(next.hyardJobs, job),
                };
              }
            case 'CoreOutputCompleted':
              return sessionUiSnapshotWithRuntimePhase(
                sessionUiSnapshotEnsureTerminal({
                  ...next,
                  activeCoreTurnId: next.activeCoreTurnId ?? turnId,
                }, turnId),
                turnId,
                'output_completed',
              );
            case 'PeerOutputCompleted':
              return sessionUiSnapshotWithRuntimePhase({
                ...next,
                activeTurnIds: turnId ? next.activeTurnIds.filter((id) => id !== turnId) : next.activeTurnIds,
              }, turnId, 'output_completed');
            case 'DelegateCompleted':
              return {
                ...next,
                activeNodes: data.peer ? next.activeNodes.filter((node) => node !== data.peer) : next.activeNodes,
                activePeerName: null,
              };
            case 'TurnCompleted':
            case 'TurnFailed':
              forgetRuntimeTurnSession(turnId);
              return {
                ...next,
                activeNodes: [],
                activeTurnIds: [],
                activeCoreTurnId: null,
                activePeerTurnId: null,
                activePeerName: null,
                activeCoreText: '',
                activePeerText: '',
                runtimeTurnPhases: {},
                runtimeTurnStartedAt: {},
                runtimeTurnPhaseChangedAt: {},
                runtimeDispatchStartedAt: null,
                runtimePreparingPhase: null,
                isGenerating: false,
              };
            case 'WorkerSpawned':
            case 'worker_spawned': {
              const worker = normalizeInstanceMetadata({ ...data, state: data.state ?? 'idle' });
              if (!worker) return next;
              return {
                ...next,
                sessionWorkers: upsertInstanceMetadata(next.sessionWorkers, worker),
              };
            }
            case 'WorkerStateChanged':
            case 'worker_state_changed': {
              const instanceId = normalizedString(data.instance_id);
              if (!instanceId) return next;
              return {
                ...next,
                sessionWorkers: next.sessionWorkers.map((worker) => (
                  worker.instance_id === instanceId
                    ? {
                        ...worker,
                        state: normalizeWorkerState(data.state),
                        in_flight_turn_id: normalizedString(data.in_flight_turn_id),
                      }
                    : worker
                )),
              };
            }
            case 'WorkerTerminated':
            case 'worker_terminated': {
              const instanceId = normalizedString(data.instance_id);
              if (!instanceId) return next;
              return {
                ...next,
                sessionWorkers: next.sessionWorkers.filter((worker) => worker.instance_id !== instanceId),
              };
            }
            default:
              return next;
          }
        });
      };

      try {
        const handleRuntimeEventPayload = (payload: any) => {
          if (!active) return;
          const envelope = payload || {};
          const rawType = envelope.event ?? envelope.type ?? envelope.event_type;
          const type = normalizedString(rawType) ?? '';
          const data = envelope.data ?? envelope.payload ?? {};
          const now = new Date().toLocaleTimeString();
          if (runtimeDebug) console.debug('[runtime_event]', runtimeEventDebugSummary(type, data));
          const eventSessionId = runtimeEventSessionId(type, data);
          if (eventSessionId && selectedSessionRef.current?.session_id !== eventSessionId) {
            if (
              type === 'CoreTurnStarted' ||
              type === 'PeerTurnStarted' ||
              type === 'FinalizationStarted'
            ) {
              rememberRuntimeTurnSession(data.turn_id, eventSessionId);
            }
            patchBackgroundRuntimeSnapshot(eventSessionId, type, data);
            return;
          }
  
        switch (type) {
          case 'TurnPreparing':
            if (
              data.session_id &&
              selectedSessionRef.current?.session_id &&
              selectedSessionRef.current.session_id !== data.session_id
            ) {
              break;
            }
            beginRuntimeDispatch(data.phase ?? '正在准备并启动 core provider…');
            setActiveCoreText('');
            setActivePeerText('');
            setActivePeerName(null);
            setActiveNodes(['host', data.provider]);
            setActiveTurnIds([]);
            enqueueLog(now, 'core', `Preparing ${data.provider}: ${data.phase ?? 'starting turn'}`);
            break;

          case 'CoreTurnStarted':
            rememberRuntimeTurnSession(data.turn_id, eventSessionId ?? selectedSessionRef.current?.session_id);
            setActiveCoreText('');
            setActivePeerText('');
            setActiveNodes(['host', data.provider]);
            setActiveTurnIds([]);
            setActiveCoreTurnId(data.turn_id);
            if (data.turn_id) {
              bindActiveAttachmentTurnFromRuntime(String(data.turn_id), eventSessionId ?? selectedSessionRef.current?.session_id);
            }
            resetRealtimeTerminalForTurn(data.turn_id);
            setHyardJobs({});
            seedRuntimeTurnStartedAtFromDispatch(data.turn_id);
            markRuntimeTurnPhase(data.turn_id, 'running');
            clearRuntimeDispatch();
            enqueueLog(now, 'core', `Core turn started on [${data.provider}] (ID: ${data.turn_id})`);
            scheduleRefreshTurns();
            break;
          
          case 'CoreItemUpdated':
            if (data.turn_id) {
              rememberRuntimeTurnSession(data.turn_id, eventSessionId ?? selectedSessionRef.current?.session_id);
              setActiveCoreTurnId((prev) => prev ?? data.turn_id);
              ensureRuntimeTurnPhase(data.turn_id, 'running');
              ensureRealtimeTerminalForTurn(data.turn_id);
            }
            if (isRuntimeReasoningEvent(data)) {
              if (!isEmptyReasoningRuntimeEvent(data) && data.payload) {
                enqueueRuntimeItemEvent(data);
              }
              break;
            }
            {
              const itemText = typeof data.text === 'string' ? data.text : '';
              if (isSystemStatusText(itemText)) {
                enqueueLog(now, 'core', itemText);
                if (data.turn_id && itemText.trim()) {
                  appendRealtimeTerminalText(data.turn_id, itemText.endsWith('\n') ? itemText : `${itemText}\n`);
                }
              } else if (hasProviderTextUpdate(itemText, data.payload)) {
                setActiveCoreText((prev) => applyProviderTextUpdate(prev, itemText, data.payload));
              }
            }
            if (data.payload) {
              enqueueRuntimeItemEvent(data);
            }
            break;

          case 'PeerTurnStarted':
            rememberRuntimeTurnSession(data.turn_id, eventSessionId ?? selectedSessionRef.current?.session_id);
            setActiveNodes((prev) => prev.includes(data.provider) ? prev : [...prev, data.provider]);
            setActiveTurnIds((prev) => prev.includes(data.turn_id) ? prev : [...prev, data.turn_id]);
            setActivePeerName(data.provider);
            setActivePeerText('');
            setActivePeerTurnId(data.turn_id);
            resetRealtimeTerminalForTurn(data.turn_id);
            markRuntimeTurnPhase(data.turn_id, 'running');
            enqueueLog(now, 'peer', `Delegating subtask to Peer [${data.provider}] (ID: ${data.turn_id})`);
            scheduleRefreshTurns();
            break;

          case 'PeerItemUpdated':
            if (data.turn_id) {
              rememberRuntimeTurnSession(data.turn_id, eventSessionId ?? selectedSessionRef.current?.session_id);
              setActivePeerTurnId((prev) => prev ?? data.turn_id);
              ensureRuntimeTurnPhase(data.turn_id, 'running');
              setActivePeerName((prev) => prev ?? data.provider);
              setActiveNodes((prev) => prev.includes(data.provider) ? prev : [...prev, data.provider]);
              setActiveTurnIds((prev) => prev.includes(data.turn_id) ? prev : [...prev, data.turn_id]);
              ensureRealtimeTerminalForTurn(data.turn_id);
            }
            if (isRuntimeReasoningEvent(data)) {
              if (!isEmptyReasoningRuntimeEvent(data) && data.payload) {
                enqueueRuntimeItemEvent(data);
              }
              break;
            }
            {
              const itemText = typeof data.text === 'string' ? data.text : '';
              if (isSystemStatusText(itemText)) {
                enqueueLog(now, 'peer', itemText);
                if (data.turn_id && itemText.trim()) {
                  appendRealtimeTerminalText(data.turn_id, itemText.endsWith('\n') ? itemText : `${itemText}\n`);
                }
              } else if (hasProviderTextUpdate(itemText, data.payload)) {
                setActivePeerText((prev) => applyProviderTextUpdate(prev, itemText, data.payload));
              }
            }
            if (data.payload) {
              enqueueRuntimeItemEvent(data);
            }
            break;

          case 'DelegateRequested':
            enqueueLog(now, 'sys', `Core requested delegation to [${data.peer}] as [${data.role}]: "${data.task_summary}"`);
            break;

          case 'DelegateCompleted':
            setActiveNodes((prev) => prev.filter((n) => n !== data.peer));
            setActivePeerName(null);
            enqueueLog(now, 'sys', `Delegation to [${data.peer}] completed with status: ${data.status}`);
            scheduleRefreshTurns();
            break;

          case 'CoreOutputCompleted':
            if (data.turn_id) {
              setActiveCoreTurnId((prev) => prev ?? data.turn_id);
              ensureRealtimeTerminalForTurn(data.turn_id);
              markRuntimeTurnPhase(data.turn_id, 'output_completed');
            }
            enqueueLog(now, 'core', `Core output completed for [${data.provider}]`);
            scheduleRefreshTurns();
            break;

          case 'PeerOutputCompleted':
            setActiveTurnIds((prev) => prev.filter((id) => id !== data.turn_id));
            markRuntimeTurnPhase(data.turn_id, 'output_completed');
            scheduleRefreshTurns();
            break;

          case 'CoreExecutionTelemetry':
            if (data.turn_id) {
              setActiveCoreTurnId((prev) => prev ?? data.turn_id);
              ensureRuntimeTurnPhase(data.turn_id, 'running');
              ensureRealtimeTerminalForTurn(data.turn_id);
            }
            if (data.execution) {
              enqueueRuntimeItemEvent({
                ...data,
                event_type: RUNTIME_ITEM_EVENT_FALLBACK,
                payload: { item_type: 'execution_telemetry', execution: data.execution },
              });
              const transport = data.execution.io_transport ? ` [${String(data.execution.io_transport).toUpperCase()}]` : '';
              enqueueLog(now, 'info', `Core command${transport}: ${executionDisplay(data.execution)}`);
            }
            break;

          case 'PeerExecutionTelemetry':
            if (data.turn_id) {
              setActivePeerTurnId((prev) => prev ?? data.turn_id);
              ensureRuntimeTurnPhase(data.turn_id, 'running');
              setActivePeerName((prev) => prev ?? data.provider);
              setActiveNodes((prev) => prev.includes(data.provider) ? prev : [...prev, data.provider]);
              setActiveTurnIds((prev) => prev.includes(data.turn_id) ? prev : [...prev, data.turn_id]);
              ensureRealtimeTerminalForTurn(data.turn_id);
            }
            if (data.execution) {
              enqueueRuntimeItemEvent({
                ...data,
                event_type: RUNTIME_ITEM_EVENT_FALLBACK,
                payload: { item_type: 'execution_telemetry', execution: data.execution },
              });
              const transport = data.execution.io_transport ? ` [${String(data.execution.io_transport).toUpperCase()}]` : '';
              enqueueLog(now, 'info', `Peer command${transport}: ${executionDisplay(data.execution)}`);
            }
            break;

          case 'CoreTerminalOutput':
            if (data.turn_id) {
              setActiveCoreTurnId((prev) => prev ?? data.turn_id);
              ensureRuntimeTurnPhase(data.turn_id, 'running');
            }
            if (data.text) {
              const lines = data.text.split('\n');
              const newLogs: TelemetryLog[] = [];
              for (const line of lines) {
                const trimmed = line.trim();
                if (trimmed) {
                  newLogs.push({ timestamp: now, tag: 'core', message: `[Subprocess Out]: ${trimmed}` });
                }
              }
              if (newLogs.length > 0) {
                enqueueLogs(newLogs);
              }
              if (data.turn_id) {
                appendRealtimeTerminalText(data.turn_id, data.text);
              }
            }
            break;

          case 'PeerTerminalOutput':
            if (data.turn_id) {
              setActivePeerTurnId((prev) => prev ?? data.turn_id);
              ensureRuntimeTurnPhase(data.turn_id, 'running');
              setActivePeerName((prev) => prev ?? data.provider);
              setActiveNodes((prev) => prev.includes(data.provider) ? prev : [...prev, data.provider]);
              setActiveTurnIds((prev) => prev.includes(data.turn_id) ? prev : [...prev, data.turn_id]);
            }
            if (data.text) {
              const lines = data.text.split('\n');
              const newLogs: TelemetryLog[] = [];
              for (const line of lines) {
                const trimmed = line.trim();
                if (trimmed) {
                  newLogs.push({ timestamp: now, tag: 'peer', message: `[Peer Subprocess - ${data.provider}]: ${trimmed}` });
                }
              }
              if (newLogs.length > 0) {
                enqueueLogs(newLogs);
              }
              if (data.turn_id) {
                appendRealtimeTerminalText(data.turn_id, data.text);
              }
            }
            break;

          case 'CallbackReceiptsInjected':
            enqueueLog(now, 'sys', `Injected ${data.count} unread callback receipts for provider [${data.provider}]`);
            scheduleRefreshTurns();
            break;

          case 'HyardJobObserved':
            {
              const job = hyardJobRecordFromRuntimeEvent(data);
              setHyardJobs((prev) => upsertHyardJobRecord(prev, job));
              if (job?.job_id) {
                enqueueLog(now, 'sys', `[HYARD] Observed background job ${job.job_id} (${job.provider}) status: ${job.status}`);
              }
            }
            break;

          case 'FinalizationStarted':
            rememberRuntimeTurnSession(data.turn_id, eventSessionId ?? selectedSessionRef.current?.session_id);
            setActiveCoreText('');
            setActivePeerText('');
            setActivePeerName(null);
            setActiveNodes(['host', data.provider]);
            setActiveTurnIds([]);
            setActiveCoreTurnId(data.turn_id);
            ensureRealtimeTerminalForTurn(data.turn_id);
            seedRuntimeTurnStartedAtFromDispatch(data.turn_id);
            markRuntimeTurnPhase(data.turn_id, 'finalizing');
            clearRuntimeDispatch();
            enqueueLog(now, 'core', `Finalization phase started on [${data.provider}] (ID: ${data.turn_id})`);
            scheduleRefreshTurns();
            break;

          case 'TurnCompleted':
            {
              const finalResponse = rememberFinalTurnResponse(data.turn_id, data.response);
              if (finalResponse && typeof data.turn_id === 'string') {
                commitTurns((prev) => mergeFinalResponseIntoTurns(prev, data.turn_id, finalResponse, 'completed'));
              }
            }
            forgetRuntimeTurnSession(data.turn_id);
            setActiveNodes([]);
            setActiveTurnIds([]);
            setActiveCoreTurnId(null);
            setActivePeerTurnId(null);
            setActivePeerName(null);
            setActiveCoreText('');
            setActivePeerText('');
            setRuntimeTurnPhases({});
            setRuntimeTurnStartedAt({});
            setRuntimeTurnPhaseChangedAt({});
            clearRuntimeDispatch();
            enqueueLog(now, 'sys', `Routed turn completed successfully.`);
            scheduleRefreshTurns();
            // Bump the git refresh counter so the Source Control panel
            // re-fetches `git status` and surfaces whatever the AI just
            // wrote. This is the primary AI-change discovery path now
            // (git is the source of truth — see SourceControl.tsx).
            setGitRefreshNonce((n) => n + 1);
            break;

          case 'TurnFailed':
            forgetRuntimeTurnSession(data.turn_id);
            setActiveNodes([]);
            setActiveTurnIds([]);
            setActiveCoreTurnId(null);
            setActivePeerTurnId(null);
            setActivePeerName(null);
            setActiveCoreText('');
            setActivePeerText('');
            setRuntimeTurnPhases({});
            setRuntimeTurnStartedAt({});
            setRuntimeTurnPhaseChangedAt({});
            clearRuntimeDispatch();
            enqueueLog(now, 'sys', `Turn failed: ${data.error}`);
            scheduleRefreshTurns();
            break;

          case 'WorkerSpawned':
          case 'worker_spawned': {
            const session = selectedSessionRef.current;
            const worker = normalizeInstanceMetadata({ ...data, state: data.state ?? 'idle' });
            if (session && worker && session.session_id === worker.session_id) {
              setSessionWorkers((prev) => upsertInstanceMetadata(prev, worker));
              enqueueLog(now, 'sys', `Worker spawned: ${data.provider}${data.label ? ` (${data.label})` : ''}`);
              scheduleRefreshSessionWorkers(worker.session_id);
            }
            break;
          }

          case 'WorkerStateChanged':
          case 'worker_state_changed': {
            const session = selectedSessionRef.current;
            const sessionId = normalizedString(data.session_id);
            const instanceId = normalizedString(data.instance_id);
            if (session && sessionId && instanceId && session.session_id === sessionId) {
              setSessionWorkers((prev) =>
                prev.map((w) =>
                  w.instance_id === instanceId
                    ? {
                        ...w,
                        state: normalizeWorkerState(data.state),
                        in_flight_turn_id: normalizedString(data.in_flight_turn_id),
                      }
                    : w,
                ),
              );
              scheduleRefreshSessionWorkers(sessionId);
            }
            break;
          }

          case 'WorkerRetrying':
          case 'worker_retrying': {
            const session = selectedSessionRef.current;
            const sessionId = normalizedString(data.session_id);
            if (session && sessionId && session.session_id === sessionId) {
              enqueueLog(
                now,
                'sys',
                `Worker retrying (attempt ${data.attempt}) ${data.provider}${data.label ? ` [${data.label}]` : ''}: ${data.last_error}`,
              );
              // The new attempt will emit its own WorkerSpawned + StateChanged
              // events, so no roster mutation here beyond surfacing the cause.
              scheduleRefreshSessionWorkers(sessionId);
            }
            break;
          }

          case 'WorkerTerminated':
          case 'worker_terminated': {
            const session = selectedSessionRef.current;
            const sessionId = normalizedString(data.session_id);
            const instanceId = normalizedString(data.instance_id);
            if (session && sessionId && instanceId && session.session_id === sessionId) {
              setSessionWorkers((prev) => prev.filter((w) => w.instance_id !== instanceId));
              enqueueLog(
                now,
                'sys',
                `Worker terminated (${data.reason}): ${data.provider}${data.label ? ` [${data.label}]` : ''}`,
              );
              scheduleRefreshSessionWorkers(sessionId);
            }
            break;
          }
        }
      };

      const handleRuntimeEventBatchPayloads = (batch: any[]) => {
        if (batch.length <= 1) {
          if (batch.length === 1) handleRuntimeEventPayload(batch[0]);
          return;
        }
        const context: RuntimeEventBatchContext = { sessionEventUpserts: [], logs: [] };
        runtimeEventBatchContext = context;
        try {
          for (const payload of batch) {
            handleRuntimeEventPayload(payload);
          }
        } finally {
          runtimeEventBatchContext = null;
        }
        flushRuntimeEventBatchContext(context);
      };

      const handleRuntimeDbEventBatch = (batch: RuntimeEventRecord[]) => {
        if (!active || batch.length === 0) return;

        const recordsBySession: Record<string, Record<string, any>> = {};
        const maxEventIdBySession: Record<string, number> = {};

        batch.forEach((dbEvent) => {
          const eventId = Number(dbEvent?.event_id ?? 0);
          const job = hyardJobRecordFromRuntimeDbEvent(dbEvent);
          const sessionId = normalizedString(job?.session_id ?? dbEvent?.session_id);

          if (sessionId && Number.isFinite(eventId) && eventId > 0) {
            maxEventIdBySession[sessionId] = Math.max(maxEventIdBySession[sessionId] ?? 0, eventId);
          }

          if (!job?.job_id || !sessionId) return;

          if (!recordsBySession[sessionId]) recordsBySession[sessionId] = {};
          recordsBySession[sessionId][job.job_id] = job;
        });

        const cursorEntries = Object.entries(maxEventIdBySession);
        if (cursorEntries.length > 0) {
          const nextCursor = { ...runtimeSnapshotCursorRef.current };
          cursorEntries.forEach(([sessionId, maxId]) => {
            nextCursor[sessionId] = Math.max(nextCursor[sessionId] ?? 0, maxId);
          });
          runtimeSnapshotCursorRef.current = nextCursor;
        }

        const currentSessionId = selectedSessionRef.current?.session_id;
        Object.entries(recordsBySession).forEach(([sessionId, jobs]) => {
          if (sessionId === currentSessionId) {
            setHyardJobs((prev) => ({ ...prev, ...jobs }));
          } else {
            patchSessionUiSnapshot(sessionId, (previous) => ({
              ...previous,
              hyardJobs: {
                ...(previous.hyardJobs ?? {}),
                ...jobs,
              },
              loadedAt: Date.now(),
            }));
          }
        });

        if (runtimeDebug) {
          console.debug('[runtime_db_event_batch]', {
            events: batch.length,
            sessions: Object.keys(recordsBySession),
            jobs: Object.values(recordsBySession).reduce(
              (count, jobs) => count + Object.keys(jobs).length,
              0,
            ),
          });
        }
      };

      const uEvent = await listen<any>('runtime_event', (event) => {
        unstable_batchedUpdates(() => {
          handleRuntimeEventPayload(event.payload || {});
        });
      });
      const uBatch = await listen<any[]>('runtime_event_batch', (event) => {
        const batch = Array.isArray(event.payload) ? event.payload : [];
        unstable_batchedUpdates(() => {
          handleRuntimeEventBatchPayloads(batch);
        });
      });
      const uRuntimeDbBatch = await listen<RuntimeEventRecord[]>('runtime_db_event_batch', (event) => {
        const batch = Array.isArray(event.payload) ? event.payload : [];
        unstable_batchedUpdates(() => {
          handleRuntimeDbEventBatch(batch);
        });
      });
      if (!active) {
        uEvent();
        uBatch();
        uRuntimeDbBatch();
      } else {
        unlistenFns.push(uEvent, uBatch, uRuntimeDbBatch);
      }
      } catch (err) {
        console.error('Error setting up Tauri event listener:', err);
      }
    };

    setupListener();

    return () => {
      active = false;
      if (refreshTurnsTimer !== null) {
        window.clearTimeout(refreshTurnsTimer);
        refreshTurnsTimer = null;
      }
      if (workerRefreshTimer !== null) {
        window.clearTimeout(workerRefreshTimer);
        workerRefreshTimer = null;
      }
      pendingWorkerRefreshSessionIds.clear();
      unlistenFns.splice(0).forEach((unlisten) => unlisten());
    };
  }, []);

  const addLogs = (logs: TelemetryLog[]) => {
    setTelemetryLogs((prev) => {
      const next = [...prev, ...logs];
      if (next.length > 1000) {
        return next.slice(next.length - 1000);
      }
      return next;
    });
  };

  const addLog = (time: string, tag: 'core' | 'peer' | 'sys' | 'info', message: string) => {
    addLogs([{ timestamp: time, tag, message }]);
  };

  const persistedTurnStartedAtMs = useMemo(() => {
    const next: Record<string, number> = {};
    turns.forEach((turn) => {
      if (!turn.started_at) return;
      const parsed = Date.parse(turn.started_at);
      if (Number.isFinite(parsed)) {
        next[turn.turn_id] = parsed;
      }
    });
    return next;
  }, [turns]);

  const handleResolveToolApproval = async (
    requestId: string,
    decision: 'approve' | 'deny',
    reason?: string,
  ) => {
    try {
      await resolveToolApproval(requestId, decision, reason);
      addLog(
        new Date().toLocaleTimeString(),
        'sys',
        `Tool approval ${decision}: ${requestId}`,
      );
    } catch (error) {
      addLog(
        new Date().toLocaleTimeString(),
        'sys',
        `Failed to resolve tool approval ${requestId}: ${error}`,
      );
      throw error;
    }
  };

  const runtimeTurnStartedAtMs = (turnId: string): number | undefined => {
    const liveStartedAt = runtimeTurnStartedAt[turnId];
    if (liveStartedAt) return liveStartedAt;
    return persistedTurnStartedAtMs[turnId];
  };

  const runtimePhaseStartedAtMs = (turnId: string): number | undefined => {
    return runtimeTurnPhaseChangedAt[turnId] || runtimeTurnStartedAtMs(turnId);
  };

  const handleCancel = async () => {
    try {
      await invoke('cancel_turn', {
        sessionId: selectedSessionRef.current?.session_id ?? null,
      });
      addLog(new Date().toLocaleTimeString(), 'sys', '取消指令已发送至智能体内核...');
    } catch (e) {
      addLog(new Date().toLocaleTimeString(), 'sys', `取消失败: ${e}`);
    }
  };

  const renderTurnEventsWithActions = (
    turnId: string,
    eventList: any[],
    turnList: Turn[],
    realtimeLines?: string[],
    jobs?: Record<string, any>,
    options: RenderTurnEventsOptions = {},
  ) => renderTurnEvents(turnId, eventList, turnList, realtimeLines, jobs, {
    ...options,
    onResolveApproval: handleResolveToolApproval,
    onCancelTurn: handleCancel,
    runtimePhase: runtimeTurnPhases[turnId],
    runtimeStartedAtMs: runtimeTurnStartedAtMs(turnId),
    runtimePhaseStartedAtMs: runtimePhaseStartedAtMs(turnId),
  });

  const renderTurnActivitySummaryWithActions = (
    turnId: string,
    eventList: any[],
    turnList: Turn[],
    realtimeLines?: string[],
    jobs?: Record<string, any>,
    options: RenderTurnEventsOptions = {},
  ) => renderTurnActivitySummary(turnId, eventList, turnList, realtimeLines, jobs, {
    ...options,
    onResolveApproval: handleResolveToolApproval,
    onCancelTurn: handleCancel,
    runtimePhase: runtimeTurnPhases[turnId],
    runtimeStartedAtMs: runtimeTurnStartedAtMs(turnId),
    runtimePhaseStartedAtMs: runtimePhaseStartedAtMs(turnId),
  });

  const getQueuedMessagesForSession = (sessionId: string | null | undefined) => {
    if (!sessionId) return [];
    if (selectedSessionRef.current?.session_id === sessionId) {
      return messageQueueRef.current;
    }
    return sessionUiSnapshotsRef.current[sessionId]?.messageQueue ?? [];
  };

  const setQueuedMessagesForSession = (
    sessionId: string | null | undefined,
    next: SendPayload[],
  ) => {
    if (!sessionId) return;
    patchSessionUiSnapshot(sessionId, {
      messageQueue: next,
    });
    if (selectedSessionRef.current?.session_id === sessionId) {
      messageQueueRef.current = next;
      setMessageQueue(next);
    }
  };

  const enqueueQueuedMessage = (
    payload: SendPayload,
    sessionId = selectedSessionRef.current?.session_id,
  ) => {
    const next = [...getQueuedMessagesForSession(sessionId), payload];
    setQueuedMessagesForSession(sessionId, next);
    addLog(
      new Date().toLocaleTimeString(),
      'sys',
      `Queued message (${next.length} pending): ${describeSendPayload(payload)}`,
    );
    return next.length;
  };

  const clearQueuedMessages = (
    emitLog: boolean,
    sessionId = selectedSessionRef.current?.session_id,
  ) => {
    if (getQueuedMessagesForSession(sessionId).length === 0) return;
    setQueuedMessagesForSession(sessionId, []);
    if (emitLog) {
      addLog(new Date().toLocaleTimeString(), 'sys', 'Cleared queued messages');
    }
  };

  const resetWorkspaceScopedUi = () => {
    selectedSessionRef.current = null;
    sessionEventsCursorRef.current = {};
    runtimeSnapshotCursorRef.current = {};
    sessionUiSnapshotsRef.current = {};
    sessionDataRefreshGenerationRef.current = {};
    runtimeTurnSessionIdRef.current = {};
    pendingAttachmentBindingsRef.current = [];
    activeAttachmentBindingIdRef.current = null;
    setSessions([]);
    setSelectedSession(null);
    commitTurns([]);
    setSessionEvents([]);
    setSessionWorkers([]);
    setActiveCoreText('');
    setActivePeerText('');
    setActivePeerName(null);
    setActiveNodes([]);
    setActiveTurnIds([]);
    setActiveCoreTurnId(null);
    setActivePeerTurnId(null);
    setSelectedAgentTurnId(null);
    clearRealtimeTerminalText();
    setHyardJobs({});
    setRuntimeTurnPhases({});
    setRuntimeTurnStartedAt({});
    setRuntimeTurnPhaseChangedAt({});
    clearRuntimeDispatch();
    setCanvasTabs([]);
    setActiveCanvasTabId(null);
    preparingSessionIdsRef.current.clear();
    dispatchingSessionIdsRef.current.clear();
    setIsGenerating(false);
    messageQueueRef.current = [];
    setMessageQueue([]);
  };

  const activateSessionShell = (session: Session) => {
    const previous = selectedSessionRef.current;
    // Keep the imperative ref in sync immediately. Runtime events and
    // refreshTurns() can fire before React commits setSelectedSession(); if the
    // ref is stale those live updates either no-op or hydrate the wrong chat.
    if (previous?.session_id) {
      captureCurrentSessionUiSnapshot(previous.session_id);
    }
    selectedSessionRef.current = session;
    setSelectedSession(session);
    applySessionUiSnapshot(sessionUiSnapshotsRef.current[session.session_id]);
  };

  const beginSessionDataRefresh = (sessionId: string): number => {
    const refreshGeneration = (sessionDataRefreshGenerationRef.current[sessionId] ?? 0) + 1;
    sessionDataRefreshGenerationRef.current = {
      ...sessionDataRefreshGenerationRef.current,
      [sessionId]: refreshGeneration,
    };
    return refreshGeneration;
  };

  const isLatestSessionDataRefresh = (sessionId: string, refreshGeneration: number): boolean => (
    sessionDataRefreshGenerationRef.current[sessionId] === refreshGeneration
  );

  const refreshSessionDataForSelection = async (
    session: Session,
    options: {
      mode: 'full' | 'incremental';
      refreshGeneration: number;
      deferTurnRefresh: boolean;
    },
  ) => {
    const sessionId = session.session_id;
    const isLatestRefresh = () => isLatestSessionDataRefresh(sessionId, options.refreshGeneration);

    try {
      if (options.mode === 'full') {
        const [turnList, runtimeSnapshot, eventList] = await Promise.all([
          invoke<Turn[]>('get_session_turns', { sessionId }),
          fetchRuntimeSnapshot(sessionId, 'full'),
          fetchSessionEventsForMerge(sessionId, 'full'),
        ]);
        if (!isLatestRefresh()) return;

        const runtimeHyardJobs = hyardJobsFromRuntimeSnapshot(runtimeSnapshot);
        const turnListWithFinalResponses = applyRememberedFinalTurnResponses(turnList);
        reconcileTurnAttachmentBindings(sessionId, turnListWithFinalResponses);
        const mergedEvents = mergeSessionEventLists([], eventList);
        const snapshotBeforeMerge = sessionUiSnapshotsRef.current[sessionId] ?? createEmptySessionUiSnapshot();
        const mergedTurns = mergeFreshTurnsPreservingKnownResponses(
          snapshotBeforeMerge.turns,
          turnListWithFinalResponses,
        );

        patchSessionUiSnapshot(sessionId, {
          turns: turnListsEquivalentForUi(snapshotBeforeMerge.turns, mergedTurns)
            ? snapshotBeforeMerge.turns
            : mergedTurns,
          hyardJobs: runtimeHyardJobs,
          sessionEvents: mergedEvents,
          loaded: true,
        });

        if (selectedSessionRef.current?.session_id !== sessionId) return;
        unstable_batchedUpdates(() => {
          commitTurns((prev) => {
            const nextTurns = mergeFreshTurnsPreservingKnownResponses(prev, turnListWithFinalResponses);
            return turnListsEquivalentForUi(prev, nextTurns) ? prev : nextTurns;
          });
          setHyardJobs(runtimeHyardJobs);
          setSessionEvents(mergedEvents);
        });
        return;
      }

      // Cached sessions already have enough state to paint instantly. Refresh
      // light, cursor-based data after the shell has painted, then postpone the
      // expensive full turns read/deserialization until the browser is idle so
      // toggling between two long transcripts does not block input.
      await deferUntilNextPaint();
      if (!isLatestRefresh()) return;

      const [runtimeSnapshot, eventList] = await Promise.all([
        fetchRuntimeSnapshot(sessionId, 'incremental'),
        fetchSessionEventsForMerge(sessionId, 'incremental'),
      ]);
      if (!isLatestRefresh()) return;

      const runtimeHyardJobs = hyardJobsFromRuntimeSnapshot(runtimeSnapshot);
      const snapshotBeforeMerge = sessionUiSnapshotsRef.current[sessionId] ?? createEmptySessionUiSnapshot();
      const mergedEvents = mergeSessionEventLists(snapshotBeforeMerge.sessionEvents, eventList);
      const mergedHyardJobs = mergeHyardJobRecords(snapshotBeforeMerge.hyardJobs, runtimeHyardJobs);

      patchSessionUiSnapshot(sessionId, {
        sessionEvents: mergedEvents,
        hyardJobs: mergedHyardJobs,
        loaded: true,
      });

      if (selectedSessionRef.current?.session_id === sessionId) {
        unstable_batchedUpdates(() => {
          if (eventList.length > 0) {
            setSessionEvents((prev) => mergeSessionEventLists(prev, eventList));
          }
          if (Object.keys(runtimeHyardJobs).length > 0) {
            setHyardJobs((prev) => mergeHyardJobRecords(prev, runtimeHyardJobs));
          }
        });
      }

      if (options.deferTurnRefresh) {
        await deferUntilBrowserIdle();
      }
      if (!isLatestRefresh()) return;

      const turnList = await invoke<Turn[]>('get_session_turns', { sessionId });
      if (!isLatestRefresh()) return;

      const turnListWithFinalResponses = applyRememberedFinalTurnResponses(turnList);
      reconcileTurnAttachmentBindings(sessionId, turnListWithFinalResponses);
      patchSessionUiSnapshot(sessionId, (previous) => {
        const mergedTurns = mergeFreshTurnsPreservingKnownResponses(previous.turns, turnListWithFinalResponses);
        return {
          ...previous,
          turns: turnListsEquivalentForUi(previous.turns, mergedTurns) ? previous.turns : mergedTurns,
          loaded: true,
          loadedAt: Date.now(),
        };
      });

      if (selectedSessionRef.current?.session_id !== sessionId) return;
      commitTurns((prev) => {
        const mergedTurns = mergeFreshTurnsPreservingKnownResponses(prev, turnListWithFinalResponses);
        return turnListsEquivalentForUi(prev, mergedTurns) ? prev : mergedTurns;
      });
    } catch (e) {
      console.error('Failed to refresh selected session data:', e);
    }
  };

  const takeNextQueuedMessage = (sessionId = selectedSessionRef.current?.session_id): SendPayload | null => {
    const pending = getQueuedMessagesForSession(sessionId);
    if (pending.length === 0) return null;
    const [nextMessage, ...rest] = pending;
    setQueuedMessagesForSession(sessionId, rest);
    return nextMessage;
  };

  const loadSessions = async (workspaceOverride?: Workspace | null) => {
    const workspace = workspaceOverride === undefined ? currentWorkspace : workspaceOverride;
    if (!workspace) {
      resetWorkspaceScopedUi();
      return;
    }
    try {
      const res = await invoke<Session[]>('list_sessions');
      setSessions(res);
      if (res.length > 0 && !selectedSessionRef.current) {
        selectSession(res[0]);
      }
    } catch (e) {
      console.error(e);
    }
  };

  const selectSession = (session: Session) => {
    const sessionId = session.session_id;
    if (selectedSessionRef.current?.session_id === sessionId) {
      selectedSessionRef.current = session;
      setSelectedSession(session);
      return;
    }

    const cachedSnapshot = sessionUiSnapshotsRef.current[sessionId];
    const hasLoadedSnapshot = Boolean(cachedSnapshot?.loaded);

    activateSessionShell(session);

    if (hasLoadedSnapshot) {
      seedSessionEventCursorFromSnapshot(sessionId, cachedSnapshot);
    } else {
      // Cold visits need an authoritative full hydration. Cached sessions keep
      // their cursors so switching back does not accidentally turn the next
      // refresh into another full event/runtime replay.
      sessionEventsCursorRef.current = {
        ...sessionEventsCursorRef.current,
        [sessionId]: '',
      };
      runtimeSnapshotCursorRef.current = {
        ...runtimeSnapshotCursorRef.current,
        [sessionId]: 0,
      };
    }

    const refreshGeneration = beginSessionDataRefresh(sessionId);
    void refreshSessionDataForSelection(session, {
      mode: hasLoadedSnapshot ? 'incremental' : 'full',
      refreshGeneration,
      deferTurnRefresh: hasLoadedSnapshot,
    });
  };

  const createNewSession = async () => {
    try {
      const session = await invoke<Session>('create_session', { provider: newSessionProvider });
      setSessions((prev) => [session, ...prev]);
      selectSession(session);
    } catch (e) {
      console.error(e);
    }
  };

  const refreshProviderStatuses = async () => {
    setProviderStatusLoading(true);
    setProviderStatusError(null);
    try {
      const statuses = await invoke<ProviderStatus[]>('list_provider_status');
      setProviderStatuses(statuses);
    } catch (e) {
      const message = String(e);
      setProviderStatusError(message);
      console.error('Failed to refresh provider statuses:', e);
    } finally {
      setProviderStatusLoading(false);
    }
  };

  const loadProviderStatusSnapshot = async () => {
    setProviderStatusError(null);
    try {
      const statuses = await invoke<ProviderStatus[]>('list_provider_status_quick');
      setProviderStatuses(statuses);
    } catch (e) {
      const message = String(e);
      setProviderStatusError(message);
      console.error('Failed to load provider status snapshot:', e);
    }
  };

  const loadAppConfig = async () => {
    try {
      const cfg = await invoke<SwitchyardConfig>('load_config');
      setConfig(cfg);
      if (cfg && cfg.core && cfg.core.default_provider) {
        setNewSessionProvider(cfg.core.default_provider);
      }
      void loadProviderStatusSnapshot();
    } catch (e) {
      console.error(e);
    }
  };

  const handleSaveConfig = async () => {
    if (!config) return;
    try {
      await invoke('save_config', { config });
      setShowSettings(false);
      addLog(new Date().toLocaleTimeString(), 'sys', 'Configuration successfully saved to switchyard.toml');
      refreshProviderStatuses();
    } catch (e) {
      alert('Failed to save config: ' + e);
    }
  };

  const handleSandboxModeChange = async (mode: SandboxMode) => {
    if (!config) return;
    const nextConfig: SwitchyardConfig = {
      ...config,
      sandbox: {
        ...(config.sandbox ?? { allowed_paths: [] }),
        mode,
        allowed_paths: config.sandbox?.allowed_paths ?? [],
      },
    };
    sandboxModeRef.current = mode;
    setConfig(nextConfig);
    try {
      await invoke('save_config', { config: nextConfig });
      addLog(new Date().toLocaleTimeString(), 'sys', `Sandbox mode set to ${mode}`);
    } catch (e) {
      addLog(new Date().toLocaleTimeString(), 'sys', `Failed to save sandbox mode: ${e}`);
    }
  };

  const isSessionSendPipelineBusy = (sessionId: string | null | undefined) => {
    if (!sessionId) return false;
    return (
      preparingSessionIdsRef.current.has(sessionId) ||
      dispatchingSessionIdsRef.current.has(sessionId)
    );
  };

  const runSingleMessage = async (sessionForSend: Session, payload: SendPayload) => {
    const targetSessionId = sessionForSend.session_id;
    const isTargetVisible = () => selectedSessionRef.current?.session_id === targetSessionId;
    const targetTurnsForSend = () => (
      isTargetVisible()
        ? turnsRef.current
        : sessionUiSnapshotsRef.current[targetSessionId]?.turns ?? []
    );
    const patchTargetSnapshot = (
      patch:
        | Partial<SessionUiSnapshot>
        | ((previous: SessionUiSnapshot) => SessionUiSnapshot),
    ) => patchSessionUiSnapshot(targetSessionId, patch);
    const clearTargetRuntimeState = () => {
      if (isTargetVisible()) {
        setActiveNodes([]);
        setActivePeerName(null);
        setActiveTurnIds([]);
        setActiveCoreTurnId(null);
        setActivePeerTurnId(null);
        setActiveCoreText('');
        setActivePeerText('');
        setRuntimeTurnPhases({});
        setRuntimeTurnStartedAt({});
        setRuntimeTurnPhaseChangedAt({});
        clearRuntimeDispatch();
        return;
      }
      patchTargetSnapshot((previous) => ({
        ...previous,
        activeNodes: [],
        activeTurnIds: [],
        activeCoreTurnId: null,
        activePeerTurnId: null,
        activePeerName: null,
        activeCoreText: '',
        activePeerText: '',
        runtimeTurnPhases: {},
        runtimeTurnStartedAt: {},
        runtimeTurnPhaseChangedAt: {},
        runtimeDispatchStartedAt: null,
        runtimePreparingPhase: null,
        isGenerating: false,
        loadedAt: Date.now(),
      }));
    };

    const message = stripAttachmentReferences(payload.text);
    const imagePaths = payload.imagePaths;
    const filePaths = payload.filePaths ?? [];
    const payloadAttachments = attachmentsFromPayload(payload);
    const sandboxMode = sandboxModeRef.current;
    const dispatchStartedAt = Date.now();
    if (isTargetVisible()) {
      setActiveCoreText('');
      setActivePeerText('');
      setActivePeerName(null);
      setActiveNodes(['host']);
      setActiveTurnIds([]);
      setTelemetryLogs([]);
      setRuntimeTurnPhases({});
      setRuntimeTurnStartedAt({});
      setRuntimeTurnPhaseChangedAt({});
      beginRuntimeDispatch('正在进入后端调度队列…', dispatchStartedAt);
    } else {
      patchTargetSnapshot((previous) => ({
        ...previous,
        activeCoreText: '',
        activePeerText: '',
        activePeerName: null,
        activeNodes: ['host'],
        activeTurnIds: [],
        activeCoreTurnId: null,
        activePeerTurnId: null,
        runtimeTurnPhases: {},
        runtimeTurnStartedAt: {},
        runtimeTurnPhaseChangedAt: {},
        runtimeDispatchStartedAt: dispatchStartedAt,
        runtimePreparingPhase: '正在进入后端调度队列…',
        isGenerating: true,
        loadedAt: Date.now(),
      }));
    }

    // Add visual temp turn/message instantly for reactive feel. For a hidden
    // session this writes only to its snapshot cache, so dispatching queued work
    // in Session B cannot repaint/pollute the currently visible Session A.
    const tempTurnId = createTempUserTurnId(targetSessionId);
    const beforeTurnIds = turnIdsBeforeSend(targetTurnsForSend(), targetSessionId);
    const attachmentBinding: PendingTurnAttachmentBinding | null = payloadAttachments.length > 0
      ? {
          bindingId: `${tempTurnId}:attachments`,
          sessionId: targetSessionId,
          text: message,
          attachments: payloadAttachments,
          beforeTurnIds,
          tempTurnId,
          createdAt: Date.now(),
        }
      : null;
    const tempUserTurn: Turn = {
      turn_id: tempTurnId,
      session_id: targetSessionId,
      origin: 'user',
      provider: 'user',
      role: 'core',
      user_message: message,
      provider_response: null,
      error_message: null,
      status: 'completed',
      started_at: new Date().toISOString(),
      completed_at: null,
      delegated_by: null
    };
    if (isTargetVisible()) {
      commitTurns((prev) => [...prev, tempUserTurn]);
    } else {
      patchTargetSnapshot((previous) => ({
        ...previous,
        turns: [...previous.turns, tempUserTurn],
        loaded: true,
        loadedAt: Date.now(),
      }));
    }
    if (attachmentBinding) {
      pendingAttachmentBindingsRef.current = [
        ...pendingAttachmentBindingsRef.current,
        attachmentBinding,
      ].slice(-100);
      activeAttachmentBindingIdRef.current = attachmentBinding.bindingId;
      setTurnAttachments((prev) => ({
        ...prev,
        [tempTurnId]: payloadAttachments,
      }));
    } else {
      activeAttachmentBindingIdRef.current = null;
    }

    let runTurnResponse: string | null = null;
    try {
      runTurnResponse = await invoke<string>('run_turn', {
        sessionId: targetSessionId,
        message,
        provider: sessionForSend.active_core,
        sandboxMode,
        imagePaths,
        filePaths,
      });
    } catch (e) {
      addLog(new Date().toLocaleTimeString(), 'sys', `Execution failed: ${e}`);
      // If the backend rejects before a canonical runtime event exists, there
      // will be no TurnFailed event to clear the "preparing" card. Success and
      // normal provider failures are cleared by TurnCompleted/TurnFailed so we
      // don't tear down live stream state prematurely.
      clearTargetRuntimeState();
    } finally {
      // Reload turns database state
      try {
        const updatedTurns = await invoke<Turn[]>('get_session_turns', { sessionId: sessionForSend.session_id });
        let nextTurns = applyRememberedFinalTurnResponses(updatedTurns);
        const returnedResponse = fallbackResponseForUserMessage(runTurnResponse, message);
        if (returnedResponse !== null) {
          const matchedTurn = findFreshlySentTurn(
            nextTurns,
            sessionForSend.session_id,
            message,
            beforeTurnIds,
            tempTurnId,
          );
          if (matchedTurn && nonBlankText(matchedTurn.provider_response) === null) {
            rememberFinalTurnResponse(matchedTurn.turn_id, returnedResponse);
            nextTurns = mergeFallbackResponseIntoTurns(nextTurns, matchedTurn.turn_id, returnedResponse);
          }
        }
        reconcileTurnAttachmentBindings(sessionForSend.session_id, nextTurns);
        if (selectedSessionRef.current?.session_id === sessionForSend.session_id) {
          commitTurns((prev) => mergeFreshTurnsPreservingKnownResponses(prev, nextTurns));
        } else {
          patchSessionUiSnapshot(sessionForSend.session_id, (previous) => ({
            ...previous,
            turns: mergeFreshTurnsPreservingKnownResponses(previous.turns, nextTurns),
            loaded: true,
            loadedAt: Date.now(),
          }));
        }
      } catch (e) {
        console.error('Error reloading turns after dispatch:', e);
        addLog(new Date().toLocaleTimeString(), 'sys', `Failed to refresh turns after run: ${e}`);
      }
      // Reload sessions list to refresh updated times
      void loadSessions();
      // Reload session events. Merge instead of replacing so a DB read that
      // races slightly behind live runtime events cannot erase just-rendered
      // streaming/tool-call cards.
      try {
        const eventList = await fetchSessionEventsForMerge(sessionForSend.session_id, 'incremental');
        if (selectedSessionRef.current?.session_id === sessionForSend.session_id) {
          setSessionEvents((prev) => mergeSessionEventLists(prev, eventList));
        } else {
          patchSessionUiSnapshot(sessionForSend.session_id, (previous) => ({
            ...previous,
            sessionEvents: mergeSessionEventLists(previous.sessionEvents, eventList),
            loaded: true,
            loadedAt: Date.now(),
          }));
        }
      } catch (e) {
        console.error(e);
      }
    }
  };

  const dispatchMessage = async (sessionForSend: Session, initialPayload: SendPayload) => {
    const targetSessionId = sessionForSend.session_id;
    if (isSessionSendPipelineBusy(targetSessionId)) {
      enqueueQueuedMessage(initialPayload, sessionForSend.session_id);
      return;
    }
    dispatchingSessionIdsRef.current.add(targetSessionId);
    if (selectedSessionRef.current?.session_id === targetSessionId) {
      setIsGenerating(true);
    } else {
      patchSessionUiSnapshot(targetSessionId, { isGenerating: true });
    }
    let payload: SendPayload | null = initialPayload;
    try {
      while (payload !== null) {
        await runSingleMessage(sessionForSend, payload);

        // Drain queued messages FIFO without dropping the authoritative
        // in-flight flag between turns. This avoids the old recursive gap where
        // a rapid send at provider-completion time could slip through as a
        // second overlapping `run_turn` instead of joining the queue.
        payload = takeNextQueuedMessage(targetSessionId);
        if (payload) {
          addLog(
            new Date().toLocaleTimeString(),
            'sys',
            `Dispatching queued message (${getQueuedMessagesForSession(targetSessionId).length} remaining): ${describeSendPayload(payload)}`,
          );
        }
      }
    } finally {
      dispatchingSessionIdsRef.current.delete(targetSessionId);
      if (selectedSessionRef.current?.session_id === targetSessionId) {
        setIsGenerating(false);
      } else {
        patchSessionUiSnapshot(targetSessionId, {
          isGenerating: false,
          activeCoreTurnId: null,
          activePeerTurnId: null,
          activePeerName: null,
          activeNodes: [],
          activeTurnIds: [],
          runtimeDispatchStartedAt: null,
          runtimePreparingPhase: null,
        });
      }
    }
  };

  /// Append a synthetic system message to the chat. Used by slash
  /// commands (`/help`, `/workers`, etc.) to surface inline feedback
  /// without going through the orchestrator. The message goes into the
  /// telemetry log so it persists across renders without faking a
  /// canonical turn.
  const appendSystemNote = (note: string) => {
    addLog(new Date().toLocaleTimeString(), 'sys', note);
  };

  const toggleTerminalPanel = () => {
    setTerminalEverOpened(true);
    setTerminalOpen((v) => !v);
  };

  const startLeftColumnResize = (event: ReactPointerEvent<HTMLDivElement>) => {
    if (event.button !== 0) return;
    const app = appContainerRef.current;
    const leftColumn = event.currentTarget.previousElementSibling as HTMLElement | null;
    if (!app || !leftColumn) return;
    const sash = event.currentTarget;
    const pointerId = event.pointerId;
    event.preventDefault();
    sash.setPointerCapture(pointerId);
    document.body.style.cursor = 'col-resize';
    document.body.style.userSelect = 'none';
    document.body.classList.add('is-layout-resizing');

    const leftOrigin = leftColumn.getBoundingClientRect().left;
    let nextWidth = leftColumnWidthRef.current;
    let resizeFrame: number | null = null;
    let finished = false;
    const applyPendingWidth = () => {
      resizeFrame = null;
      app.style.setProperty(
        '--switchyard-left-column-width',
        `${Math.round(nextWidth)}px`,
      );
    };

    const handlePointerMove = (moveEvent: PointerEvent) => {
      nextWidth = clampLayoutNumber(
        moveEvent.clientX - leftOrigin,
        MIN_LEFT_COLUMN_WIDTH,
        MAX_LEFT_COLUMN_WIDTH,
      );
      leftColumnWidthRef.current = nextWidth;
      if (resizeFrame === null) {
        resizeFrame = requestAnimationFrame(applyPendingWidth);
      }
    };

    const finishResize = () => {
      if (finished) return;
      finished = true;
      document.removeEventListener('pointermove', handlePointerMove);
      document.removeEventListener('pointerup', finishResize);
      document.removeEventListener('pointercancel', finishResize);
      window.removeEventListener('blur', finishResize);
      if (resizeFrame !== null) {
        cancelAnimationFrame(resizeFrame);
        resizeFrame = null;
      }
      applyPendingWidth();
      const committed = Math.round(nextWidth);
      leftColumnWidthRef.current = committed;
      setLeftColumnWidth(committed);
      window.localStorage.setItem('switchyard.leftColumnWidth', String(committed));
      document.body.style.cursor = '';
      document.body.style.userSelect = '';
      document.body.classList.remove('is-layout-resizing');
      try {
        if (sash.hasPointerCapture(pointerId)) {
          sash.releasePointerCapture(pointerId);
        }
      } catch {
        // The pointer may already be released after window blur/cancel.
      }
    };

    document.addEventListener('pointermove', handlePointerMove, { passive: true });
    document.addEventListener('pointerup', finishResize, { once: true });
    document.addEventListener('pointercancel', finishResize, { once: true });
    window.addEventListener('blur', finishResize, { once: true });
  };

  const startCanvasResize = (event: ReactPointerEvent<HTMLDivElement>) => {
    if (event.button !== 0) return;
    const app = appContainerRef.current;
    const row = mainRowRef.current;
    if (!app || !row) return;
    const sash = event.currentTarget;
    const pointerId = event.pointerId;
    event.preventDefault();
    sash.setPointerCapture(pointerId);
    document.body.style.cursor = 'col-resize';
    document.body.style.userSelect = 'none';
    document.body.classList.add('is-layout-resizing');

    const rowRect = row.getBoundingClientRect();
    const maxCanvasWidth = Math.max(
      MIN_CANVAS_COLUMN_WIDTH,
      rowRect.width - MIN_CHAT_COLUMN_WIDTH,
    );
    let nextWidth = canvasColumnWidthRef.current;
    let resizeFrame: number | null = null;
    let finished = false;
    const applyPendingWidth = () => {
      resizeFrame = null;
      app.style.setProperty(
        '--switchyard-canvas-column-width',
        `${Math.round(nextWidth)}px`,
      );
    };

    const handlePointerMove = (moveEvent: PointerEvent) => {
      nextWidth = clampLayoutNumber(
        rowRect.right - moveEvent.clientX,
        MIN_CANVAS_COLUMN_WIDTH,
        maxCanvasWidth,
      );
      canvasColumnWidthRef.current = nextWidth;
      if (resizeFrame === null) {
        resizeFrame = requestAnimationFrame(applyPendingWidth);
      }
    };

    const finishResize = () => {
      if (finished) return;
      finished = true;
      document.removeEventListener('pointermove', handlePointerMove);
      document.removeEventListener('pointerup', finishResize);
      document.removeEventListener('pointercancel', finishResize);
      window.removeEventListener('blur', finishResize);
      if (resizeFrame !== null) {
        cancelAnimationFrame(resizeFrame);
        resizeFrame = null;
      }
      applyPendingWidth();
      const committed = Math.round(nextWidth);
      canvasColumnWidthRef.current = committed;
      setCanvasColumnWidth(committed);
      window.localStorage.setItem('switchyard.canvasColumnWidth', String(committed));
      document.body.style.cursor = '';
      document.body.style.userSelect = '';
      document.body.classList.remove('is-layout-resizing');
      try {
        if (sash.hasPointerCapture(pointerId)) {
          sash.releasePointerCapture(pointerId);
        }
      } catch {
        // The pointer may already be released after window blur/cancel.
      }
    };

    document.addEventListener('pointermove', handlePointerMove, { passive: true });
    document.addEventListener('pointerup', finishResize, { once: true });
    document.addEventListener('pointercancel', finishResize, { once: true });
    window.addEventListener('blur', finishResize, { once: true });
  };

  /// Context object passed to slash command handlers. Built fresh on
  /// each send so handlers see the live state closures.
  const slashContext = (): SlashContext => ({
    openSettings: () => setShowSettings(true),
    openDiagnostics: () => setDrawerOpen(true),
    toggleTerminal: toggleTerminalPanel,
    resetCore: async () => {
      if (selectedSession) {
        await handleResetCore();
      }
    },
    openCanvasFile: openFileInCanvas,
    appendSystemNote,
  });

  const handleSend = async (
    rawInput: string | SendPayload,
    restoreText?: (text: string) => void,
  ) => {
    const rawPayload = normalizeSendPayload(rawInput);
    const text = stripAttachmentReferences(rawPayload.text).trim();
    const attachments = attachmentsFromPayload(rawPayload);
    const imagePaths = rawPayload.imagePaths.length > 0
      ? rawPayload.imagePaths
      : attachments.filter((attachment) => attachment.kind === 'image').map((attachment) => attachment.path);
    const filePaths = (rawPayload.filePaths?.length ?? 0) > 0
      ? rawPayload.filePaths ?? []
      : attachments.filter((attachment) => attachment.kind !== 'image').map((attachment) => attachment.path);
    if (!text && imagePaths.length === 0 && filePaths.length === 0) return;
    const payload: SendPayload = {
      text: text || (filePaths.length > 0 ? '请分析这些附件。' : '请分析这些图片。'),
      imagePaths,
      filePaths,
      attachments: attachments.length > 0 ? attachments : undefined,
    };

    // Slash commands short-circuit the orchestrator dispatch. They
    // work without a selected session (e.g. `/help` from a blank
    // workspace), so we resolve them before the session check.
    const slash = parseSlash(payload.text);
    if (slash && payload.imagePaths.length === 0 && (payload.filePaths?.length ?? 0) === 0) {
      const ctx = slashContext();
      slash.cmd.run(slash.args, ctx).then((result) => {
        if (result.error) {
          appendSystemNote(`/${slash.cmd.name}: ${result.error}`);
        } else if (result.systemMessage) {
          appendSystemNote(result.systemMessage);
        }
      });
      return;
    }

    // No session yet? Mint one on the fly using the current core
    // provider — same path the Sidebar's "+ New" button takes — then
    // dispatch the message into it. This is what the user expects
    // when typing into a fresh workspace: just send, don't make me
    // click "New Session" first.
    let target = selectedSessionRef.current ?? selectedSession;
    if (target && isSessionSendPipelineBusy(target.session_id)) {
      enqueueQueuedMessage(payload, target.session_id);
      return;
    }

    if (!target) {
      if (isSessionSendPipelineBusy(NEW_SESSION_PIPELINE_ID)) {
        restoreText?.(rawPayload.text);
        appendSystemNote('正在创建新会话，请稍后再发送。');
        return;
      }
      preparingSessionIdsRef.current.add(NEW_SESSION_PIPELINE_ID);
      setIsGenerating(true);
      try {
        const created = await invoke<Session>('create_session', {
          provider: newSessionProvider,
        });
        setSessions((prev) => [created, ...prev]);
        // Wire it as the active session synchronously. Do not call
        // selectSession(created) here: its async empty-history fetch can return
        // after runSingleMessage has appended the optimistic user bubble and
        // erase that first visible feedback, making a fresh-chat send look like
        // a silent hang.
        activateSessionShell(created);
        sessionEventsCursorRef.current = {
          ...sessionEventsCursorRef.current,
          [created.session_id]: '',
        };
        runtimeSnapshotCursorRef.current = {
          ...runtimeSnapshotCursorRef.current,
          [created.session_id]: 0,
        };
        commitTurns([]);
        setSessionEvents([]);
        target = created;
      } catch (e) {
        appendSystemNote(`Failed to create session: ${e}`);
        // Restore the user's text so they can retry without retyping.
        restoreText?.(rawPayload.text);
        setIsGenerating(false);
        return;
      } finally {
        preparingSessionIdsRef.current.delete(NEW_SESSION_PIPELINE_ID);
      }
    }

    if (isSessionSendPipelineBusy(target.session_id)) {
      enqueueQueuedMessage(payload, target.session_id);
      return;
    }
    void dispatchMessage(target, payload);
  };

  const handleClearQueue = () => {
    if (messageQueueRef.current.length === 0) return;
    clearQueuedMessages(true);
  };

  // Edit & resend: backend wipes the canonical tail at `turnId` (incl. any
  // streamed delegate/assistant responses that came after) and terminates the
  // session's Core live instance. The frontend then redispatches the edited
  // text through the regular `run_turn` path, which lazily respawns the Core
  // on a clean slate. The on-disk CLI session is lost (no warm fork yet); the
  // canonical store retains everything before the rewind point.
  const handleEditAndResend = async (turnId: string, newText: string) => {
    if (!selectedSession) return;
    const sessionForSend = selectedSession;
    try {
      await invoke('edit_and_resend_last_turn', {
        sessionId: sessionForSend.session_id,
        turnId,
      });
      addLog(
        new Date().toLocaleTimeString(),
        'sys',
        `Rewound history at turn ${turnId.substring(0, 8)} — resending edited message`,
      );
      // Refresh turns so the wiped tail disappears before the new turn starts.
      try {
        const refreshed = await invoke<Turn[]>('get_session_turns', { sessionId: sessionForSend.session_id });
        if (selectedSessionRef.current?.session_id === sessionForSend.session_id) {
          reconcileTurnAttachmentBindings(sessionForSend.session_id, refreshed);
          commitTurns(refreshed);
        }
      } catch (e) {
        console.error('refresh after rewind failed', e);
      }
      // Queue + dispatch the new message just like a normal send.
      const originalTurn = turns.find((turn) => turn.turn_id === turnId);
      const attachments = mergeInputAttachments(
        turnAttachments[turnId],
        originalTurn ? extractAttachmentsFromAttachmentReferences(originalTurn.user_message) : [],
        extractAttachmentsFromAttachmentReferences(newText),
      );
      const cleanText = stripAttachmentReferences(newText);
      const payload: SendPayload = {
        text: cleanText,
        imagePaths: attachments.length > 0
          ? attachments.filter((attachment) => attachment.kind === 'image').map((attachment) => attachment.path)
          : extractImagePathsFromAttachmentReferences(newText),
        filePaths: attachments.length > 0
          ? attachments.filter((attachment) => attachment.kind !== 'image').map((attachment) => attachment.path)
          : extractFilePathsFromAttachmentReferences(newText),
        attachments: attachments.length > 0 ? attachments : undefined,
      };
      if (isSessionSendPipelineBusy(sessionForSend.session_id)) {
        enqueueQueuedMessage(payload, sessionForSend.session_id);
      } else {
        void dispatchMessage(sessionForSend, payload);
      }
    } catch (e) {
      addLog(new Date().toLocaleTimeString(), 'sys', `Edit & resend failed: ${e}`);
    }
  };

  // Retry: same rewind, but resends the existing user_message verbatim.
  const handleRetryLastUserTurn = async (turnId: string) => {
    if (!selectedSession) return;
    const sessionForSend = selectedSession;
    const turn = turns.find((t) => t.turn_id === turnId);
    if (!turn) {
      addLog(new Date().toLocaleTimeString(), 'sys', `Retry: turn ${turnId} not found in local cache`);
      return;
    }
    const message = turn.user_message;
    try {
      await invoke('edit_and_resend_last_turn', {
        sessionId: sessionForSend.session_id,
        turnId,
      });
      addLog(
        new Date().toLocaleTimeString(),
        'sys',
        `Rewound history at turn ${turnId.substring(0, 8)} — retrying same message`,
      );
      try {
        const refreshed = await invoke<Turn[]>('get_session_turns', { sessionId: sessionForSend.session_id });
        if (selectedSessionRef.current?.session_id === sessionForSend.session_id) {
          reconcileTurnAttachmentBindings(sessionForSend.session_id, refreshed);
          commitTurns(refreshed);
        }
      } catch (e) {
        console.error('refresh after rewind failed', e);
      }
      const attachments = mergeInputAttachments(
        turnAttachments[turnId],
        extractAttachmentsFromAttachmentReferences(message),
      );
      const cleanMessage = stripAttachmentReferences(message);
      const payload: SendPayload = {
        text: cleanMessage,
        imagePaths: attachments.length > 0
          ? attachments.filter((attachment) => attachment.kind === 'image').map((attachment) => attachment.path)
          : extractImagePathsFromAttachmentReferences(message),
        filePaths: attachments.length > 0
          ? attachments.filter((attachment) => attachment.kind !== 'image').map((attachment) => attachment.path)
          : extractFilePathsFromAttachmentReferences(message),
        attachments: attachments.length > 0 ? attachments : undefined,
      };
      if (isSessionSendPipelineBusy(sessionForSend.session_id)) {
        enqueueQueuedMessage(payload, sessionForSend.session_id);
      } else {
        void dispatchMessage(sessionForSend, payload);
      }
    } catch (e) {
      addLog(new Date().toLocaleTimeString(), 'sys', `Retry failed: ${e}`);
    }
  };

  const handleSettingsFieldChange = (section: keyof SwitchyardConfig, field: string, value: any) => {
    if (!config) return;
    setConfig((prev) => {
      if (!prev) return null;
      return {
        ...prev,
        [section]: {
          ...prev[section],
          [field]: value
        }
      };
    });
  };

  const handleProviderFieldChange = (provider: string, field: keyof ProviderConfig, value: any) => {
    if (!config) return;
    setConfig((prev) => {
      if (!prev) return null;
      const providersCopy = { ...prev.providers };
      providersCopy[provider] = {
        ...providersCopy[provider],
        [field]: value
      };
      return {
        ...prev,
        providers: providersCopy
      };
    });
  };

  const addEnvVar = (provider: string) => {
    if (!config) return;
    const key = prompt('Enter Env Key:');
    if (!key) return;
    const value = prompt('Enter Env Value:');
    if (value === null) return;
    
    setConfig((prev) => {
      if (!prev) return null;
      const providersCopy = { ...prev.providers };
      const envCopy = { ...providersCopy[provider].env };
      envCopy[key] = value;
      providersCopy[provider] = {
        ...providersCopy[provider],
        env: envCopy
      };
      return { ...prev, providers: providersCopy };
    });
  };

  const removeEnvVar = (provider: string, key: string) => {
    if (!config) return;
    setConfig((prev) => {
      if (!prev) return null;
      const providersCopy = { ...prev.providers };
      const envCopy = { ...providersCopy[provider].env };
      delete envCopy[key];
      providersCopy[provider] = {
        ...providersCopy[provider],
        env: envCopy
      };
      return { ...prev, providers: providersCopy };
    });
  };

  const addCustomProvider = () => {
    if (!config) return;
    const name = prompt('Enter new provider name (e.g. codex2, claude-worker):');
    if (!name) return;
    const trimmed = name.trim();
    if (!trimmed) return;
    if (config.providers[trimmed]) {
      alert('Provider already exists!');
      return;
    }
    
    // Choose standard backend template
    const backend = prompt(
      'Choose provider backend type (codex, claude, gemini, antigravity):',
      'codex',
    );
    if (backend === null) return;
    const trimmedBackend = backend.trim().toLowerCase();
    if (!['codex', 'claude', 'gemini', 'antigravity'].includes(trimmedBackend)) {
      alert('Backend must be one of: codex, claude, gemini, antigravity');
      return;
    }

    const defaultCommand =
      trimmedBackend === 'codex'
        ? 'codex'
        : trimmedBackend === 'claude'
          ? 'claude'
          : trimmedBackend === 'gemini'
            ? 'gemini'
            : 'agy'; // antigravity ships as `agy`, not `antigravity-cli`
    const defaultArgs: string[] = [];

    setConfig((prev) => {
      if (!prev) return null;
      const providersCopy = { ...prev.providers };
      providersCopy[trimmed] = {
        command: defaultCommand,
        args: defaultArgs,
        env: {},
        model: null,
        thinking_level: null,
        timeout_secs: 0,
        backend: trimmedBackend,
      };
      return {
        ...prev,
        providers: providersCopy
      };
    });
    setSettingsTab(trimmed);
  };

  const handleDeleteProvider = (providerName: string) => {
    if (!config) return;
    setConfig((prev) => {
      if (!prev) return null;
      const providersCopy = { ...prev.providers };
      delete providersCopy[providerName];
      
      let defaultProvider = prev.core.default_provider;
      if (defaultProvider === providerName) {
        defaultProvider = Object.keys(providersCopy)[0] || '';
      }
      const defaultPeers = prev.core.default_peers.filter(p => p !== providerName);

      return {
        ...prev,
        core: {
          ...prev.core,
          default_provider: defaultProvider,
          default_peers: defaultPeers,
        },
        providers: providersCopy
      };
    });
    setSettingsTab('general');
  };

  // `handleTogglePeer` removed alongside the SESSION PEERS UI. Peer
  // toggling now lives in session-level settings (Phase 3 surface).
  // The `update_session_peers` Tauri command is still wired and can
  // be invoked from there when the new UI lands.

  const handleDeleteSession = async (sessionId: string) => {
    if (!confirm('Are you sure you want to delete this session? This action cannot be undone.')) {
      return;
    }
    try {
      await invoke('delete_session', { sessionId });
      const remainingCursors = { ...sessionEventsCursorRef.current };
      delete remainingCursors[sessionId];
      sessionEventsCursorRef.current = remainingCursors;
      const remainingRuntimeCursors = { ...runtimeSnapshotCursorRef.current };
      delete remainingRuntimeCursors[sessionId];
      runtimeSnapshotCursorRef.current = remainingRuntimeCursors;
      const remainingRefreshGenerations = { ...sessionDataRefreshGenerationRef.current };
      delete remainingRefreshGenerations[sessionId];
      sessionDataRefreshGenerationRef.current = remainingRefreshGenerations;
      const remainingSnapshots = { ...sessionUiSnapshotsRef.current };
      delete remainingSnapshots[sessionId];
      sessionUiSnapshotsRef.current = remainingSnapshots;
      runtimeTurnSessionIdRef.current = Object.fromEntries(
        Object.entries(runtimeTurnSessionIdRef.current).filter(([, value]) => value.sessionId !== sessionId),
      );
      preparingSessionIdsRef.current.delete(sessionId);
      dispatchingSessionIdsRef.current.delete(sessionId);
      setSessions((prev) => {
        const next = prev.filter((s) => s.session_id !== sessionId);
        if (selectedSession?.session_id === sessionId) {
          if (next.length > 0) {
            selectSession(next[0]);
          } else {
            selectedSessionRef.current = null;
            setSelectedSession(null);
            commitTurns([]);
            setSessionEvents([]);
            setSessionWorkers([]);
            messageQueueRef.current = [];
            setMessageQueue([]);
          }
        }
        return next;
      });
      addLog(new Date().toLocaleTimeString(), 'sys', `Deleted session ${sessionId}`);
    } catch (e) {
      console.error('Failed to delete session:', e);
      alert('Failed to delete session: ' + e);
    }
  };

  const handleRenameSession = async (sessionId: string, newName: string) => {
    try {
      await invoke('rename_session', { sessionId, name: newName });
      setSessions((prev) =>
        prev.map((s) => (s.session_id === sessionId ? { ...s, name: newName } : s))
      );
      if (selectedSession?.session_id === sessionId) {
        setSelectedSession((prev) => prev ? { ...prev, name: newName } : null);
      }
      addLog(new Date().toLocaleTimeString(), 'sys', `Renamed session to: ${newName}`);
    } catch (e) {
      console.error('Failed to rename session:', e);
      alert('Failed to rename session: ' + e);
    }
  };

  const handleUpdateSessionSummary = async (sessionId: string, summary: string | null) => {
    try {
      await invoke('update_session_summary', { sessionId, summary });
      setSessions((prev) =>
        prev.map((s) => (s.session_id === sessionId ? { ...s, summary } : s))
      );
      if (selectedSession?.session_id === sessionId) {
        setSelectedSession((prev) => prev ? { ...prev, summary } : null);
      }
      addLog(new Date().toLocaleTimeString(), 'sys', 'Session summary updated successfully');
    } catch (e) {
      console.error('Failed to update session summary:', e);
      alert('Failed to update session summary: ' + e);
    }
  };

  const handleUpdateSessionRuntime = async (
    sessionId: string,
    activeCore: string,
    model: string | null,
    thinkingLevel: string | null,
  ) => {
    try {
      const updated = await invoke<Session>('update_session_runtime', {
        sessionId,
        activeCore,
        model,
        thinkingLevel,
      });
      setSessions((prev) =>
        prev.map((s) => (s.session_id === sessionId ? updated : s)),
      );
      if (selectedSessionRef.current?.session_id === sessionId) {
        selectedSessionRef.current = updated;
        setSelectedSession(updated);
        await loadSessionWorkers(sessionId);
      }
      addLog(
        new Date().toLocaleTimeString(),
        'sys',
        `Session runtime updated: ${updated.active_core}${updated.model ? ` · ${updated.model}` : ''}`,
      );
    } catch (e) {
      console.error('Failed to update session runtime:', e);
      alert('Failed to update session runtime: ' + e);
    }
  };

  const handleUpdateSessionChecklist = async (sessionId: string, checklistJson: string) => {
    try {
      await invoke('update_session_checklist', { sessionId, checklistJson });
      setSessions((prev) =>
        prev.map((s) => {
          if (s.session_id === sessionId) {
            const native_bindings = { ...s.native_bindings, checklist: checklistJson };
            return { ...s, native_bindings };
          }
          return s;
        })
      );
      if (selectedSession?.session_id === sessionId) {
        setSelectedSession((prev) => {
          if (!prev) return null;
          const native_bindings = { ...prev.native_bindings, checklist: checklistJson };
          return { ...prev, native_bindings };
        });
      }
      addLog(new Date().toLocaleTimeString(), 'sys', 'Todo checklist updated successfully');
    } catch (e) {
      console.error('Failed to update session checklist:', e);
      alert('Failed to update session checklist: ' + e);
    }
  };

  return (
    <div
      ref={appContainerRef}
      className="app-container"
      style={{
        '--switchyard-left-column-width': `${Math.round(leftColumnWidthRef.current)}px`,
        '--switchyard-canvas-column-width': `${Math.round(canvasColumnWidthRef.current)}px`,
      } as CSSProperties}
    >
      <AppTopBar
        current={currentWorkspace}
        workspaces={workspaces}
        railMode={railMode}
        terminalOpen={terminalOpen}
        onRailModeChange={setRailMode}
        onSwitchWorkspace={handleSwitchWorkspace}
        onRenameWorkspace={handleRenameWorkspace}
        onOpenFolder={handleOpenFolderAsWorkspace}
        onCloseWorkspace={handleCloseWorkspace}
        onAddFolder={handleAddFolderToCurrentWorkspace}
        onRemoveExtraRoot={handleRemoveExtraRoot}
        onOpenSettings={() => setShowSettings(true)}
        onToggleTerminal={toggleTerminalPanel}
        onOpenDiagnostics={() => setDrawerOpen((v) => !v)}
      />

      {/* 0. Icon Rail — mode switcher + bottom settings control. */}
      <IconRail
        mode={railMode}
        onModeChange={setRailMode}
        onOpenDiagnostics={() => setDrawerOpen((v) => !v)}
        onOpenSettings={() => setShowSettings(true)}
      />

      {/* 1. Second column — session list / explorer / source control.
          Workspace operations moved into the custom top bar so this column
          can behave like VS Code's activity side pane without a pinned
          floating header. */}
      <div
        className="left-column"
        style={{
          width: '100%',
          display: 'flex',
          flexDirection: 'column',
          flexShrink: 0,
          borderRight: '1px solid var(--border-muted)',
        }}
      >
        {railMode === 'chat' && (
          <Sidebar
            sessions={sessions}
            selectedSession={selectedSession}
            config={config}
            newSessionProvider={newSessionProvider}
            setNewSessionProvider={setNewSessionProvider}
            onCreateSession={createNewSession}
            onSelectSession={selectSession}
            onDeleteSession={handleDeleteSession}
            onRenameSession={handleRenameSession}
          />
        )}
        {railMode === 'files' && (
          <Suspense fallback={<LazyPanelFallback label="Loading explorer…" />}>
            <FilesTree
              // workspace_id as key so swapping workspaces fully resets
              // the tree's cached children + expanded state.
              key={currentWorkspace?.workspace_id ?? 'no-workspace'}
              workspace={currentWorkspace}
              // Shared with SourceControl — bumping `gitRefreshNonce`
              // re-fetches `git status` so the tree's status colors
              // and folder dots stay in sync with whatever the AI just
              // wrote (or the user staged / discarded).
              gitRefreshNonce={gitRefreshNonce}
              onOpenFile={openFileInCanvas}
              onAddFolder={handleAddFolderToCurrentWorkspace}
              onRemoveExtraRoot={handleRemoveExtraRoot}
            />
          </Suspense>
        )}
        {railMode === 'source_control' && (
          <Suspense fallback={<LazyPanelFallback label="Loading source control…" />}>
            <SourceControl
              // workspaceId as key forces a full remount on workspace
              // switch so committed-message + section-open state don't
              // bleed across projects.
              key={currentWorkspace?.workspace_id ?? 'no-workspace'}
              workspaceId={currentWorkspace?.workspace_id ?? null}
              refreshNonce={gitRefreshNonce}
              onOpenDiff={openGitDiffInCanvas}
            />
          </Suspense>
        )}
      </div>

      <div
        role="separator"
        aria-orientation="vertical"
        className="layout-sash layout-sash-vertical layout-sash-sidebar"
        title="Drag to resize side bar · Double-click to reset"
        onPointerDown={startLeftColumnResize}
        onDoubleClick={() => {
          appContainerRef.current?.style.setProperty(
            '--switchyard-left-column-width',
            `${DEFAULT_LEFT_COLUMN_WIDTH}px`,
          );
          leftColumnWidthRef.current = DEFAULT_LEFT_COLUMN_WIDTH;
          setLeftColumnWidth(DEFAULT_LEFT_COLUMN_WIDTH);
          window.localStorage.setItem('switchyard.leftColumnWidth', String(DEFAULT_LEFT_COLUMN_WIDTH));
        }}
      />

      {/* 2. Main area — vertical stack:
            top:    [ChatArea | Canvas]   (horizontal flex)
            bottom: Terminal panel         (full width, when open)
          Wrapping the row in a column lets the terminal slide up
          underneath both chat and canvas at once, matching VS Code's
          integrated-terminal layout. */}
      <div
        className="main-pane-stack"
        style={{
          display: 'flex',
          flexDirection: 'column',
          minWidth: 0,
          minHeight: 0,
          height: '100%',
          overflow: 'hidden',
        }}
      >
        <div
          ref={mainRowRef}
          className="chat-canvas-row"
          style={{
            display: 'flex',
            flex: 1,
            minHeight: 0,
            minWidth: 0,
            overflow: 'hidden',
          }}
        >
          <div className="chat-pane" style={{ flex: 1, minWidth: 0, display: 'flex', flexDirection: 'column' }}>
            {currentWorkspace ? (
              <ChatArea
                selectedSession={selectedSession}
                isGenerating={isGenerating}
                turns={turns}
                turnAttachments={turnAttachments}
                runtimeDispatchStartedAt={runtimeDispatchStartedAt}
                runtimeDispatchPhase={runtimePreparingPhase}
                activeCoreRuntimePhase={activeCoreTurnId ? runtimeTurnPhases[activeCoreTurnId] : undefined}
                activePeerRuntimePhase={activePeerTurnId ? runtimeTurnPhases[activePeerTurnId] : undefined}
                handleSend={handleSend}
                handleCancel={handleCancel}
                activeCoreText={activeCoreText}
                activeCoreTurnId={activeCoreTurnId}
                activePeerName={activePeerName}
                activePeerTurnId={activePeerTurnId}
                activePeerText={activePeerText}
                sessionEvents={sessionEvents}
                realtimeTerminalLines={realtimeTerminalLines}
                hyardJobs={hyardJobs}
                renderMessageBody={renderMessageBody}
                renderTurnEvents={renderTurnEventsWithActions}
                renderTurnActivitySummary={renderTurnActivitySummaryWithActions}
                queuedMessages={messageQueue}
                onClearQueue={handleClearQueue}
                sandboxMode={config?.sandbox?.mode ?? DEFAULT_SANDBOX_MODE}
                onSandboxModeChange={handleSandboxModeChange}
                onEditAndResend={handleEditAndResend}
                onRetryLastUserTurn={handleRetryLastUserTurn}
                onOpenFile={openFileInCanvas}
              />
            ) : (
              <WelcomeWorkspace
                workspaces={workspaces}
                onOpenFolder={handleOpenFolderAsWorkspace}
                onSwitchWorkspace={handleSwitchWorkspace}
              />
            )}
          </div>
          {canvasTabs.length > 0 && (
            <>
              <div
                role="separator"
                aria-orientation="vertical"
                className="layout-sash layout-sash-vertical layout-sash-main"
                title="Drag to resize editor · Double-click to reset"
                onPointerDown={startCanvasResize}
                onDoubleClick={() => {
                  appContainerRef.current?.style.setProperty(
                    '--switchyard-canvas-column-width',
                    `${DEFAULT_CANVAS_COLUMN_WIDTH}px`,
                  );
                  canvasColumnWidthRef.current = DEFAULT_CANVAS_COLUMN_WIDTH;
                  setCanvasColumnWidth(DEFAULT_CANVAS_COLUMN_WIDTH);
                  window.localStorage.setItem('switchyard.canvasColumnWidth', String(DEFAULT_CANVAS_COLUMN_WIDTH));
                }}
              />
              <div
                className="canvas-column"
                style={{
                  width: 'var(--switchyard-canvas-column-width, 680px)',
                  minWidth: MIN_CANVAS_COLUMN_WIDTH,
                  maxWidth: `calc(100% - ${MIN_CHAT_COLUMN_WIDTH}px)`,
                  display: 'flex',
                  flexDirection: 'column',
                  flexShrink: 0,
                }}
              >
                <Suspense fallback={<LazyPanelFallback label="Loading editor…" minHeight={240} />}>
                  <Canvas
                    tabs={canvasTabs}
                    activeTabId={activeCanvasTabId}
                    onSelectTab={setActiveCanvasTabId}
                    onCloseTab={closeCanvasTab}
                    onReloadTab={reloadCanvasTab}
                    onToggleMode={toggleCanvasTabMode}
                    onDraftChange={updateCanvasDraft}
                    onSave={saveCanvasTab}
                    onRevertAiChange={revertCanvasAiChange}
                    onDismissAiChange={dismissCanvasAiChange}
                  />
                </Suspense>
              </div>
            </>
          )}
        </div>

        {/* Bottom terminal panel — VS Code-style tab strip + toolbar.
            Keep it mounted after the first open so hiding the panel does
            not force a PTY restart the next time it is shown. */}
        {terminalEverOpened && (
          <Suspense fallback={terminalOpen ? <LazyPanelFallback label="Loading terminal…" minHeight={160} /> : null}>
            <TerminalPanel
              visible={terminalOpen}
              cwd={currentWorkspace?.primary_root ?? null}
              onClose={() => setTerminalOpen(false)}
            />
          </Suspense>
        )}
      </div>

      {/* 3. Diagnostics drawer — formerly the always-on ControlCenter
          column. Now lives in a slide-in overlay anchored to the right
          edge. Stays mounted regardless of open/closed so React doesn't
          re-render the (expensive) topology graph + telemetry list each
          time the user toggles. */}
      <div className={`diagnostics-drawer ${drawerOpen ? 'is-open' : ''}`}>
        <div className="diagnostics-drawer-header">
          <span
            style={{
              fontSize: 10,
              fontWeight: 700,
              color: 'var(--text-muted)',
              letterSpacing: '0.5px',
              textTransform: 'uppercase',
            }}
          >
            Diagnostics
          </span>
          <div style={{ flex: 1 }} />
          <button
            type="button"
            onClick={() => setTopologyOverlayOpen(true)}
            title="Open topology view (fullscreen)"
            style={{
              background: 'transparent',
              border: '1px solid var(--border-muted)',
              borderRadius: 4,
              color: 'var(--text-secondary)',
              cursor: 'pointer',
              padding: '2px 8px',
              fontSize: 12,
              marginRight: 6,
            }}
          >
            Topology
          </button>
          <button
            type="button"
            onClick={() => setDrawerOpen(false)}
            title="Close (press the rail's Activity icon to re-open)"
            style={{
              background: 'transparent',
              border: '1px solid var(--border-muted)',
              borderRadius: 4,
              color: 'var(--text-secondary)',
              cursor: 'pointer',
              padding: '2px 8px',
              fontSize: 12,
            }}
          >
            Close
          </button>
        </div>
        <div className="diagnostics-drawer-body">
          {drawerEverOpened && (
            <Suspense fallback={<LazyPanelFallback label="Loading diagnostics…" />}>
              <ControlCenter
                activeCore={selectedSession?.active_core || 'None'}
                enabledPeers={selectedSession?.enabled_peers || []}
                activeNode={activeNodes[activeNodes.length - 1] || null}
                isGenerating={isGenerating}
                turns={turns}
                sessionEvents={sessionEvents}
                realtimeTerminalLines={realtimeTerminalLines}
                hyardJobs={hyardJobs}
                selectedAgentTurnId={selectedAgentTurnId}
                setSelectedAgentTurnId={setSelectedAgentTurnId}
                telemetryLogs={telemetryLogs}
                activeTurnIds={activeTurnIds}
                activeNodes={activeNodes}
                activePeerName={activePeerName}
                activePeerTurnId={activePeerTurnId}
                activePeerText={activePeerText}
                activeCoreText={activeCoreText}
                renderTurnEvents={renderTurnEventsWithActions}
                providerStatuses={providerStatuses}
                providerStatusLoading={providerStatusLoading}
                providerStatusError={providerStatusError}
                refreshProviderStatuses={refreshProviderStatuses}
                sessionWorkers={sessionWorkers}
                onResetCore={handleResetCore}
                selectedSession={selectedSession}
                onUpdateSessionRuntime={handleUpdateSessionRuntime}
                onUpdateSessionSummary={handleUpdateSessionSummary}
                onUpdateSessionChecklist={handleUpdateSessionChecklist}
              />
            </Suspense>
          )}
        </div>
      </div>

      {/* Settings Modal */}
      {showSettings && config && (
        <Suspense fallback={null}>
          <SettingsModal
            config={config}
            settingsTab={settingsTab}
            setSettingsTab={setSettingsTab}
            onClose={() => setShowSettings(false)}
            onSave={handleSaveConfig}
            onFieldChange={handleSettingsFieldChange}
            onProviderFieldChange={handleProviderFieldChange}
            onAddEnvVar={addEnvVar}
            onRemoveEnvVar={removeEnvVar}
            onAddCustomProvider={addCustomProvider}
            onDeleteProvider={handleDeleteProvider}
          />
        </Suspense>
      )}

      {/* The legacy ArtifactDrawer bottom bar was removed — artifacts
          are accessible via the diagnostics drawer and (Phase 3) via
          the Files mode. Keeping a persistent bar at the bottom eats
          vertical space without earning it. */}

      {/* Topology overlay — fullscreen modal launched from the
          diagnostics drawer header. Rendered above the StatusBar so
          its fixed-position backdrop layers above everything else. */}
      {topologyOverlayOpen && (
        <Suspense fallback={null}>
          <TopologyOverlay
            open={topologyOverlayOpen}
            onClose={() => setTopologyOverlayOpen(false)}
            activeCore={selectedSession?.active_core || 'None'}
            enabledPeers={selectedSession?.enabled_peers || []}
            activeNode={activeNodes[activeNodes.length - 1] || null}
            isGenerating={isGenerating}
          />
        </Suspense>
      )}

      {/* Status bar — bottom row after the icon rail. Hosts the Terminal
          toggle (VS Code idiom), worker count, workspace name + running
          indicator. */}
      <StatusBar
        workspace={currentWorkspace}
        coreProvider={selectedSession?.active_core ?? null}
        workerCount={displayedWorkerCount}
        isGenerating={isGenerating}
        terminalOpen={terminalOpen}
        onToggleTerminal={toggleTerminalPanel}
        onOpenDiagnostics={() => setDrawerOpen((v) => !v)}
      />
    </div>
  );
}

function clampLayoutNumber(value: number, min: number, max: number): number {
  return Math.min(max, Math.max(min, value));
}

function readStoredLayoutNumber(
  key: string,
  fallback: number,
  min: number,
  max: number,
): number {
  try {
    const raw = window.localStorage.getItem(key);
    if (raw === null) return fallback;
    const parsed = Number(raw);
    if (!Number.isFinite(parsed)) return fallback;
    return clampLayoutNumber(parsed, min, max);
  } catch {
    return fallback;
  }
}

export default App;
