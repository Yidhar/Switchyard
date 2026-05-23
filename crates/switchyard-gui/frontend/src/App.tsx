import { useState, useEffect, useRef, useCallback } from 'react';
import type { CSSProperties, PointerEvent as ReactPointerEvent } from 'react';
import { invoke } from '@tauri-apps/api/core';
import { listen } from '@tauri-apps/api/event';
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
} from './types';
import { Sidebar } from './components/Sidebar';
import { IconRail, type RailMode } from './components/IconRail';
import { WorkspaceHeader } from './components/WorkspaceHeader';
import { ChatArea } from './components/ChatArea';
import { ControlCenter } from './components/ControlCenter';
import { SettingsModal } from './components/SettingsModal';
import { Canvas, type CanvasMode, type CanvasTab } from './components/Canvas';
import { fetchSnapshot, saveFile } from './components/canvasApi';
import { FilesTree } from './components/FilesTree';
import { SourceControl } from './components/SourceControl';
import { TopologyOverlay } from './components/TopologyOverlay';
import { TerminalPanel } from './components/TerminalPanel';
import { StatusBar } from './components/StatusBar';
import { parseSlash, type SlashContext } from './components/slashCommands';
// ArtifactDrawer is no longer rendered — its bottom-bar UX didn't fit
// the new layout. The import + state are dropped along with the bar.
import { renderMessageBody, isSystemStatusText, renderTurnEvents } from './components/ui/RenderHelpers';
import { resolveToolApproval } from './services/api';

const RUNTIME_ITEM_EVENT_FALLBACK = 'item_updated';
const DEBUG_RUNTIME_EVENTS = false;
const MAX_REALTIME_TERMINAL_LINES = 1000;
const DEFAULT_LEFT_COLUMN_WIDTH = 280;
const MIN_LEFT_COLUMN_WIDTH = 220;
const MAX_LEFT_COLUMN_WIDTH = 460;
const MIN_CHAT_COLUMN_WIDTH = 380;
const MIN_CANVAS_COLUMN_WIDTH = 360;
const DEFAULT_CANVAS_COLUMN_WIDTH = 680;
const DEFAULT_SANDBOX_MODE: SandboxMode = 'workspace-write';
const ATTACHMENT_MARKER = '[Switchyard Attachments]';
const IMAGE_EXTENSIONS = new Set(['png', 'jpg', 'jpeg', 'webp', 'gif', 'bmp', 'tif', 'tiff']);

function isImageAttachmentPath(path: string): boolean {
  const cleanPath = path.trim().replace(/^["']|["']$/g, '');
  const filename = cleanPath.split(/[\\/]/).filter(Boolean).pop() ?? cleanPath;
  const dotIndex = filename.lastIndexOf('.');
  if (dotIndex === -1) return false;
  return IMAGE_EXTENSIONS.has(filename.slice(dotIndex + 1).toLowerCase());
}

function normalizeSendPayload(input: string | SendPayload): SendPayload {
  if (typeof input === 'string') {
    return { text: input, imagePaths: [], filePaths: [] };
  }
  return {
    text: input.text,
    imagePaths: Array.isArray(input.imagePaths) ? input.imagePaths : [],
    filePaths: Array.isArray(input.filePaths) ? input.filePaths : [],
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

function extractAttachmentPathsFromAttachmentReferences(text: string): string[] {
  const markerIndex = text.indexOf(ATTACHMENT_MARKER);
  if (markerIndex === -1) return [];
  return text
    .slice(markerIndex + ATTACHMENT_MARKER.length)
    .split(/\r?\n/)
    .map((line) => line.trim())
    .filter((line) => line.startsWith('- '))
    .map((line) => line.slice(2).replace(/\s+\([^)]*\)\s*$/, '').trim())
    .filter(Boolean);
}

function extractImagePathsFromAttachmentReferences(text: string): string[] {
  return extractAttachmentPathsFromAttachmentReferences(text).filter(isImageAttachmentPath);
}

function extractFilePathsFromAttachmentReferences(text: string): string[] {
  return extractAttachmentPathsFromAttachmentReferences(text).filter((path) => !isImageAttachmentPath(path));
}

function displayMessageWithAttachmentReferences(payload: SendPayload): string {
  const attachmentPaths = [...payload.imagePaths, ...(payload.filePaths ?? [])];
  if (attachmentPaths.length === 0 || payload.text.includes(ATTACHMENT_MARKER)) {
    return payload.text;
  }
  return `${payload.text}\n\n${ATTACHMENT_MARKER}\n${attachmentPaths.map((path) => `- ${path}`).join('\n')}`;
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
  'delegate_request',
  'delegate_result',
  'approval_request',
  'approval_decision',
  'server_request',
  'terminal_output',
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

function runtimeDeltaText(value: any): string | null {
  if (value === undefined || value === null) return null;
  if (typeof value === 'string') return value.length > 0 ? value : null;
  if (typeof value !== 'object') return null;
  if (typeof value.text === 'string' && value.text.length > 0) return value.text;
  const content = runtimeContentText(value.content);
  if (content) return content;
  const nested = runtimeDeltaText(value.delta);
  if (nested) return nested;
  return runtimeContentText(value.message?.content);
}

function runtimePayloadText(payload: any): string | null {
  if (!payload) return null;
  const item = runtimePayloadItem(payload);
  const params = payload.params || {};

  if (typeof payload.text === 'string' && payload.text.length > 0) return payload.text;
  if (typeof params.text === 'string' && params.text.length > 0) return params.text;

  const deltaText =
    runtimeDeltaText(payload.delta) ||
    runtimeDeltaText(params.delta) ||
    runtimeDeltaText(item?.delta);
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
  return String(payload?.method || payload?.params?.method || payload?.type || payload?.params?.type || '')
    .toLowerCase()
    .replace(/\//g, '.');
}

function runtimeDeltaKind(payload: any): string {
  return String(payload?.delta?.type || payload?.params?.delta?.type || runtimePayloadItem(payload)?.delta?.type || '')
    .toLowerCase()
    .replace(/[\/_-]+/g, '.');
}

function isRuntimeTextishDelta(payload: any): boolean {
  if (!runtimePayloadText(payload)) return false;
  const protocol = runtimeProtocolKind(payload);
  const deltaKind = runtimeDeltaKind(payload);
  const deltaKindIsTextish =
    deltaKind.includes('agent.message') ||
    deltaKind.includes('assistant') ||
    deltaKind.includes('message.delta') ||
    deltaKind.includes('content.block.delta') ||
    deltaKind.includes('text.delta') ||
    deltaKind === 'text' ||
    deltaKind === 'output.text';
  if (deltaKind && !deltaKindIsTextish) return false;
  return (
    protocol.includes('agentmessage') ||
    protocol.includes('agent.message') ||
    protocol.includes('assistant') ||
    protocol.includes('message.delta') ||
    protocol.includes('content.delta') ||
    protocol.includes('text.delta') ||
    (protocol.includes('item.delta') && (!deltaKind || deltaKindIsTextish)) ||
    deltaKindIsTextish
  );
}

function appendRealtimeLines(current: string[] | undefined, incoming: string[]): string[] {
  const next = [...(current || []), ...incoming];
  if (next.length <= MAX_REALTIME_TERMINAL_LINES) return next;
  return next.slice(next.length - MAX_REALTIME_TERMINAL_LINES);
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
    !protocol.startsWith('item.')
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
  return Boolean(runtimePayloadText(payload));
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

function runtimeEventKey(event: any): string {
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

function mergeSessionEventLists(existing: any[], incoming: any[]): any[] {
  const visibleExisting = existing.filter((event) => !isEmptyReasoningRuntimeEvent(event));
  const merged = [...visibleExisting];
  const indexByKey = new Map<string, number>();
  merged.forEach((event, index) => indexByKey.set(runtimeEventKey(event), index));

  incoming.filter((event) => !isEmptyReasoningRuntimeEvent(event)).forEach((event) => {
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

function upsertRuntimeItemEvent(existing: any[], data: any): any[] {
  const payload = data?.payload;
  if (!payload) return existing;
  if (isRuntimeAssistantTextPayload(payload)) return existing;
  if (isEmptyReasoningRuntimeEvent(data)) return existing;

  const eventType = normalizeRuntimeEventType(data.event_type || RUNTIME_ITEM_EVENT_FALLBACK) || RUNTIME_ITEM_EVENT_FALLBACK;
  const itemId = runtimeItemIdentity(payload);
  const timestamp = new Date().toISOString();
  const nextEvent = {
    event_id: data.event_id || `live:${data.turn_id}:${eventType}:${itemId || timestamp}`,
    turn_id: data.turn_id,
    event_type: eventType,
    provider: data.provider,
    timestamp,
    payload,
  };
  const key = runtimeEventKey(nextEvent);
  const existingIdx = existing.findIndex((event) => runtimeEventKey(event) === key);
  if (existingIdx === -1) {
    return [...existing, nextEvent];
  }
  const next = [...existing];
  const preferred = preferRuntimeEvent(next[existingIdx], nextEvent);
  if (isRuntimeEventNoop(next[existingIdx], preferred)) return existing;
  next[existingIdx] = preferred;
  return next;
}

function applyProviderTextUpdate(prev: string, text: string, payload: any): string {
  const incomingText = typeof text === 'string' ? text : '';
  if (!payload) return incomingText ? prev + incomingText : prev;
  if (!isAssistantTextRuntimePayload(payload)) return prev;

  const item = runtimePayloadItem(payload);
  const params = payload.params || {};

  const directText = typeof payload.text === 'string' ? payload.text : null;
  if (directText !== null) {
    return isSystemStatusText(directText) ? prev : prev + directText;
  }
  const paramsText = typeof params.text === 'string' ? params.text : null;
  if (paramsText !== null) {
    return isSystemStatusText(paramsText) ? prev : prev + paramsText;
  }

  const deltaText =
    runtimeDeltaText(payload.delta) ||
    runtimeDeltaText(params.delta) ||
    runtimeDeltaText(item?.delta);
  if (deltaText !== null) return prev + deltaText;

  const contentText =
    runtimeContentText(payload.content) ||
    runtimeContentText(params.content) ||
    runtimeContentText(item?.content);
  if (contentText !== null) {
    const protocol = runtimeProtocolKind(payload);
    const isDelta = payload.delta === true || params.delta === true || item?.delta === true || protocol.includes('delta');
    return isDelta ? prev + contentText : contentText;
  }

  if (typeof item?.text === 'string') return item.text;
  if (typeof payload.item?.text === 'string') return payload.item.text;
  if (typeof payload.params?.item?.text === 'string') return payload.params.item.text;
  if (typeof item?.result === 'string') return item.result;
  if (typeof payload.result === 'string') return payload.result;
  if (typeof params.result === 'string') return params.result;

  const messageText =
    runtimeContentText(payload.message?.content) ||
    runtimeContentText(params.message?.content) ||
    runtimeContentText(item?.message?.content);
  if (messageText) return messageText;

  return incomingText ? prev + incomingText : prev;
}

function App() {
  const [sessions, setSessions] = useState<Session[]>([]);
  const [selectedSession, setSelectedSession] = useState<Session | null>(null);
  const [turns, setTurns] = useState<Turn[]>([]);
  const [isGenerating, setIsGenerating] = useState(false);
  const [messageQueue, setMessageQueue] = useState<SendPayload[]>([]);
  // React state is not synchronous enough to decide whether a rapid follow-up
  // send should dispatch or queue. Keep refs as the authoritative in-flight and
  // FIFO queue state so double-Enter / button-click bursts cannot start two
  // `run_turn` invocations concurrently.
  const isDispatchingRef = useRef(false);
  const isPreparingDispatchRef = useRef(false);
  // Mirror of messageQueue for reading inside dispatchMessage's finally without
  // making the queue a dep of the in-flight invocation. The ref always reflects
  // the latest queue state.
  const messageQueueRef = useRef<SendPayload[]>([]);
  useEffect(() => {
    messageQueueRef.current = messageQueue;
  }, [messageQueue]);
  
  // Streaming state during active run
  const [activeCoreText, setActiveCoreText] = useState('');
  const [activePeerText, setActivePeerText] = useState('');
  const [activePeerName, setActivePeerName] = useState<string | null>(null);
  const [activeNodes, setActiveNodes] = useState<string[]>([]);
  const [activeTurnIds, setActiveTurnIds] = useState<string[]>([]);
  const [telemetryLogs, setTelemetryLogs] = useState<TelemetryLog[]>([]);
  const [sessionEvents, setSessionEvents] = useState<any[]>([]);
  const [realtimeTerminalLines, setRealtimeTerminalLines] = useState<Record<string, string[]>>({});
  const [activeCoreTurnId, setActiveCoreTurnId] = useState<string | null>(null);
  const [activePeerTurnId, setActivePeerTurnId] = useState<string | null>(null);
  const [selectedAgentTurnId, setSelectedAgentTurnId] = useState<string | null>(null);
  const [hyardJobs, setHyardJobs] = useState<Record<string, any>>({});
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

  // Workspace state — drives the left-rail column and scopes the session
  // list. The backend bootstraps a "Default" workspace on first launch so
  // `currentWorkspace` is reliably non-null after `loadWorkspaces` resolves.
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

  const loadSessionWorkers = async (sessionId: string | null) => {
    if (!sessionId) {
      setSessionWorkers([]);
      return;
    }
    try {
      const list = await invoke<InstanceMetadata[]>('list_session_workers', { sessionId });
      setSessionWorkers(list);
    } catch (e) {
      console.error('Failed to load session workers:', e);
    }
  };

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
  // correct workspace scope. loadAppConfig runs in parallel (it doesn't
  // need the workspace; the backend resolves config from the current ws).
  useEffect(() => {
    (async () => {
      await loadWorkspaces();
      await loadSessions();
    })();
    loadAppConfig();
  }, []);

  const loadWorkspaces = async () => {
    try {
      const list = await invoke<Workspace[]>('list_workspaces');
      setWorkspaces(list);
      const current = await invoke<Workspace | null>('get_current_workspace');
      setCurrentWorkspace(current);
    } catch (e) {
      console.error('Failed to load workspaces:', e);
    }
  };

  const handleSwitchWorkspace = async (workspaceId: string) => {
    try {
      const next = await invoke<Workspace>('set_current_workspace', { workspaceId });
      setCurrentWorkspace(next);
      // Wipe per-workspace UI state — sessions, turns, events all belong
      // to the previous workspace and would be confusing if left visible.
      setSelectedSession(null);
      setTurns([]);
      setSessionEvents([]);
      setSessionWorkers([]);
      setActiveCoreText('');
      setActivePeerText('');
      await loadSessions();
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
      setSelectedSession(null);
      setTurns([]);
      setSessionEvents([]);
      setSessionWorkers([]);
      await loadSessions();
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

  // One-shot initial sync when the session changes. Live updates after that
  // come through Worker* runtime events handled below — polling retired.
  useEffect(() => {
    if (!selectedSession) {
      setSessionWorkers([]);
      return;
    }
    loadSessionWorkers(selectedSession.session_id);
  }, [selectedSession?.session_id]);

  // Update selectedAgentTurnId from core-agent or temp-user-id to activeCoreTurnId when activeCoreTurnId is resolved
  useEffect(() => {
    if (activeCoreTurnId && (selectedAgentTurnId === 'core-agent' || selectedAgentTurnId === 'temp-user-id')) {
      setSelectedAgentTurnId(activeCoreTurnId);
    }
  }, [activeCoreTurnId, selectedAgentTurnId]);

  // Ref to always capture the latest selectedSession inside the singleton listener
  const selectedSessionRef = useRef<Session | null>(null);
  useEffect(() => {
    selectedSessionRef.current = selectedSession;
  }, [selectedSession]);

  // Listen for Tauri events
  useEffect(() => {
    let active = true;
    let unlistenFn: (() => void) | null = null;

    const setupListener = async () => {
      if (DEBUG_RUNTIME_EVENTS) console.log('Setting up Tauri event listener for runtime_event...');
      const refreshTurns = async () => {
        if (!active) return;
        const session = selectedSessionRef.current;
        const sessionId = session?.session_id;
        if (DEBUG_RUNTIME_EVENTS) console.log('refreshTurns called, current session:', sessionId);
        if (!session || !sessionId) return;
        try {
          const turnList = await invoke<Turn[]>('get_session_turns', { sessionId });
          if (!active || selectedSessionRef.current?.session_id !== sessionId) return;
          if (DEBUG_RUNTIME_EVENTS) console.log(`Loaded ${turnList.length} turns for session ${sessionId}`);
          setTurns(turnList);
          const eventList = await invoke<any[]>('get_session_events', { sessionId });
          if (!active || selectedSessionRef.current?.session_id !== sessionId) return;
          // DB writes can lag a live runtime event by a few milliseconds. Merge
          // instead of wholesale replacement so a refresh triggered by a status
          // event cannot erase just-rendered streaming/tool cards.
          setSessionEvents((prev) => mergeSessionEventLists(prev, eventList));
        } catch (e) {
          console.error('Error fetching session turns/events:', e);
        }
      };

      try {
        const u = await listen<any>('runtime_event', (event) => {
          if (!active) return;
          if (DEBUG_RUNTIME_EVENTS) console.log('Received runtime_event event:', event);
          const payload = event.payload;
          const type = payload.event;
          const data = payload.data;
          const now = new Date().toLocaleTimeString();
          if (DEBUG_RUNTIME_EVENTS) console.log(`Event type: ${type}, data:`, data);
  
        switch (type) {
          case 'TurnPreparing':
            if (
              data.session_id &&
              selectedSessionRef.current?.session_id &&
              selectedSessionRef.current.session_id !== data.session_id
            ) {
              break;
            }
            setActiveCoreText('');
            setActivePeerText('');
            setActivePeerName(null);
            setActiveNodes(['host', data.provider]);
            setActiveTurnIds([]);
            addLog(now, 'core', `Preparing ${data.provider}: ${data.phase ?? 'starting turn'}`);
            break;

          case 'CoreTurnStarted':
            setActiveCoreText('');
            setActivePeerText('');
            setActiveNodes(['host', data.provider]);
            setActiveTurnIds([]);
            setActiveCoreTurnId(data.turn_id);
            setRealtimeTerminalLines((prev) => ({ ...prev, [data.turn_id]: [] }));
            setHyardJobs({});
            addLog(now, 'core', `Core turn started on [${data.provider}] (ID: ${data.turn_id})`);
            refreshTurns();
            break;
          
          case 'CoreItemUpdated':
            if (data.turn_id) {
              setActiveCoreTurnId((prev) => prev ?? data.turn_id);
              setRealtimeTerminalLines((prev) => (
                prev[data.turn_id] ? prev : { ...prev, [data.turn_id]: [] }
              ));
            }
            if (isRuntimeReasoningEvent(data)) {
              if (!isEmptyReasoningRuntimeEvent(data) && data.payload) {
                setSessionEvents((prev) => upsertRuntimeItemEvent(prev, data));
              }
              break;
            }
            {
              const itemText = typeof data.text === 'string' ? data.text : '';
              if (isSystemStatusText(itemText)) {
                addLog(now, 'core', itemText);
              } else if (hasProviderTextUpdate(itemText, data.payload)) {
                setActiveCoreText((prev) => applyProviderTextUpdate(prev, itemText, data.payload));
              }
            }
            if (data.payload) {
              setSessionEvents((prev) => upsertRuntimeItemEvent(prev, data));
            }
            break;

          case 'PeerTurnStarted':
            setActiveNodes((prev) => prev.includes(data.provider) ? prev : [...prev, data.provider]);
            setActiveTurnIds((prev) => prev.includes(data.turn_id) ? prev : [...prev, data.turn_id]);
            setActivePeerName(data.provider);
            setActivePeerText('');
            setActivePeerTurnId(data.turn_id);
            setRealtimeTerminalLines((prev) => ({ ...prev, [data.turn_id]: [] }));
            addLog(now, 'peer', `Delegating subtask to Peer [${data.provider}] (ID: ${data.turn_id})`);
            refreshTurns();
            break;

          case 'PeerItemUpdated':
            if (data.turn_id) {
              setActivePeerTurnId((prev) => prev ?? data.turn_id);
              setActivePeerName((prev) => prev ?? data.provider);
              setActiveNodes((prev) => prev.includes(data.provider) ? prev : [...prev, data.provider]);
              setActiveTurnIds((prev) => prev.includes(data.turn_id) ? prev : [...prev, data.turn_id]);
              setRealtimeTerminalLines((prev) => (
                prev[data.turn_id] ? prev : { ...prev, [data.turn_id]: [] }
              ));
            }
            if (isRuntimeReasoningEvent(data)) {
              if (!isEmptyReasoningRuntimeEvent(data) && data.payload) {
                setSessionEvents((prev) => upsertRuntimeItemEvent(prev, data));
              }
              break;
            }
            {
              const itemText = typeof data.text === 'string' ? data.text : '';
              if (isSystemStatusText(itemText)) {
                addLog(now, 'peer', itemText);
              } else if (hasProviderTextUpdate(itemText, data.payload)) {
                setActivePeerText((prev) => applyProviderTextUpdate(prev, itemText, data.payload));
              }
            }
            if (data.payload) {
              setSessionEvents((prev) => upsertRuntimeItemEvent(prev, data));
            }
            break;

          case 'DelegateRequested':
            addLog(now, 'sys', `Core requested delegation to [${data.peer}] as [${data.role}]: "${data.task_summary}"`);
            break;

          case 'DelegateCompleted':
            setActiveNodes((prev) => prev.filter((n) => n !== data.peer));
            setActivePeerName(null);
            addLog(now, 'sys', `Delegation to [${data.peer}] completed with status: ${data.status}`);
            refreshTurns();
            break;

          case 'CoreOutputCompleted':
            if (data.turn_id) {
              setActiveCoreTurnId((prev) => prev ?? data.turn_id);
              setRealtimeTerminalLines((prev) => (
                prev[data.turn_id] ? prev : { ...prev, [data.turn_id]: [] }
              ));
            }
            addLog(now, 'core', `Core output completed for [${data.provider}]`);
            refreshTurns();
            break;

          case 'PeerOutputCompleted':
            setActiveTurnIds((prev) => prev.filter((id) => id !== data.turn_id));
            refreshTurns();
            break;

          case 'CoreExecutionTelemetry':
            if (data.turn_id) {
              setActiveCoreTurnId((prev) => prev ?? data.turn_id);
              setRealtimeTerminalLines((prev) => (
                prev[data.turn_id] ? prev : { ...prev, [data.turn_id]: [] }
              ));
            }
            if (data.execution) {
              setSessionEvents((prev) => upsertRuntimeItemEvent(prev, {
                ...data,
                event_type: RUNTIME_ITEM_EVENT_FALLBACK,
                payload: { item_type: 'execution_telemetry', execution: data.execution },
              }));
              const transport = data.execution.io_transport ? ` [${String(data.execution.io_transport).toUpperCase()}]` : '';
              addLog(now, 'info', `Core command${transport}: ${executionDisplay(data.execution)}`);
            }
            break;

          case 'PeerExecutionTelemetry':
            if (data.turn_id) {
              setActivePeerTurnId((prev) => prev ?? data.turn_id);
              setActivePeerName((prev) => prev ?? data.provider);
              setActiveNodes((prev) => prev.includes(data.provider) ? prev : [...prev, data.provider]);
              setActiveTurnIds((prev) => prev.includes(data.turn_id) ? prev : [...prev, data.turn_id]);
              setRealtimeTerminalLines((prev) => (
                prev[data.turn_id] ? prev : { ...prev, [data.turn_id]: [] }
              ));
            }
            if (data.execution) {
              setSessionEvents((prev) => upsertRuntimeItemEvent(prev, {
                ...data,
                event_type: RUNTIME_ITEM_EVENT_FALLBACK,
                payload: { item_type: 'execution_telemetry', execution: data.execution },
              }));
              const transport = data.execution.io_transport ? ` [${String(data.execution.io_transport).toUpperCase()}]` : '';
              addLog(now, 'info', `Peer command${transport}: ${executionDisplay(data.execution)}`);
            }
            break;

          case 'CoreTerminalOutput':
            if (data.turn_id) {
              setActiveCoreTurnId((prev) => prev ?? data.turn_id);
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
                addLogs(newLogs);
              }
              if (data.turn_id) {
                setRealtimeTerminalLines((prev) => {
                  return { ...prev, [data.turn_id]: appendRealtimeLines(prev[data.turn_id], lines) };
                });
              }
            }
            break;

          case 'PeerTerminalOutput':
            if (data.turn_id) {
              setActivePeerTurnId((prev) => prev ?? data.turn_id);
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
                addLogs(newLogs);
              }
              if (data.turn_id) {
                setRealtimeTerminalLines((prev) => {
                  return { ...prev, [data.turn_id]: appendRealtimeLines(prev[data.turn_id], lines) };
                });
              }
            }
            break;

          case 'CallbackReceiptsInjected':
            addLog(now, 'sys', `Injected ${data.count} unread callback receipts for provider [${data.provider}]`);
            refreshTurns();
            break;

          case 'HyardJobObserved':
            setHyardJobs((prev) => {
              const job = {
                ...data.job,
                observed_at: data.observed_at,
              };
              return {
                ...prev,
                [data.job.job_id]: job,
                ...(data.turn_id ? { [data.turn_id]: job } : {}),
              };
            });
            addLog(now, 'sys', `[HYARD] Observed background job ${data.job.job_id} (${data.job.provider}) status: ${data.job.status}`);
            refreshTurns();
            break;

          case 'FinalizationStarted':
            setActiveCoreText('');
            setActivePeerText('');
            setActivePeerName(null);
            setActiveNodes(['host', data.provider]);
            setActiveTurnIds([]);
            setActiveCoreTurnId(data.turn_id);
            setRealtimeTerminalLines((prev) => (
              prev[data.turn_id] ? prev : { ...prev, [data.turn_id]: [] }
            ));
            addLog(now, 'core', `Finalization phase started on [${data.provider}] (ID: ${data.turn_id})`);
            refreshTurns();
            break;

          case 'TurnCompleted':
            setActiveNodes([]);
            setActiveTurnIds([]);
            setActiveCoreText('');
            setActivePeerText('');
            addLog(now, 'sys', `Routed turn completed successfully.`);
            refreshTurns();
            // Bump the git refresh counter so the Source Control panel
            // re-fetches `git status` and surfaces whatever the AI just
            // wrote. This is the primary AI-change discovery path now
            // (git is the source of truth — see SourceControl.tsx).
            setGitRefreshNonce((n) => n + 1);
            break;

          case 'TurnFailed':
            setActiveNodes([]);
            setActiveTurnIds([]);
            setActiveCoreText('');
            setActivePeerText('');
            addLog(now, 'sys', `Turn failed: ${data.error}`);
            refreshTurns();
            break;

          case 'WorkerSpawned': {
            const session = selectedSessionRef.current;
            if (session && session.session_id === data.session_id) {
              setSessionWorkers((prev) => {
                if (prev.some((w) => w.instance_id === data.instance_id)) return prev;
                return [
                  ...prev,
                  {
                    instance_id: data.instance_id,
                    provider: data.provider,
                    session_id: data.session_id,
                    label: data.label ?? null,
                    kind: data.kind,
                    spawned_at: data.spawned_at,
                    state: 'idle',
                    in_flight_turn_id: null,
                  } as InstanceMetadata,
                ];
              });
              addLog(now, 'sys', `Worker spawned: ${data.provider}${data.label ? ` (${data.label})` : ''}`);
            }
            break;
          }

          case 'WorkerStateChanged': {
            const session = selectedSessionRef.current;
            if (session && session.session_id === data.session_id) {
              setSessionWorkers((prev) =>
                prev.map((w) =>
                  w.instance_id === data.instance_id
                    ? { ...w, state: data.state, in_flight_turn_id: data.in_flight_turn_id ?? null }
                    : w,
                ),
              );
            }
            break;
          }

          case 'WorkerRetrying': {
            const session = selectedSessionRef.current;
            if (session && session.session_id === data.session_id) {
              addLog(
                now,
                'sys',
                `Worker retrying (attempt ${data.attempt}) ${data.provider}${data.label ? ` [${data.label}]` : ''}: ${data.last_error}`,
              );
              // The new attempt will emit its own WorkerSpawned + StateChanged
              // events, so no roster mutation here beyond surfacing the cause.
            }
            break;
          }

          case 'WorkerTerminated': {
            const session = selectedSessionRef.current;
            if (session && session.session_id === data.session_id) {
              setSessionWorkers((prev) => prev.filter((w) => w.instance_id !== data.instance_id));
              addLog(
                now,
                'sys',
                `Worker terminated (${data.reason}): ${data.provider}${data.label ? ` [${data.label}]` : ''}`,
              );
            }
            break;
          }
        }
      });
      if (!active) {
        u();
      } else {
        unlistenFn = u;
      }
      } catch (err) {
        console.error('Error setting up Tauri event listener:', err);
      }
    };

    setupListener();

    return () => {
      active = false;
      if (unlistenFn) {
        unlistenFn();
      }
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

  const renderTurnEventsWithActions = (
    turnId: string,
    eventList: any[],
    turnList: Turn[],
    realtimeLines?: string[],
    jobs?: Record<string, any>,
  ) => renderTurnEvents(turnId, eventList, turnList, realtimeLines, jobs, {
    onResolveApproval: handleResolveToolApproval,
  });

  const enqueueQueuedMessage = (payload: SendPayload) => {
    const next = [...messageQueueRef.current, payload];
    messageQueueRef.current = next;
    setMessageQueue(next);
    addLog(
      new Date().toLocaleTimeString(),
      'sys',
      `Queued message (${next.length} pending): ${describeSendPayload(payload)}`,
    );
    return next.length;
  };

  const clearQueuedMessages = (emitLog: boolean) => {
    if (messageQueueRef.current.length === 0) return;
    messageQueueRef.current = [];
    setMessageQueue([]);
    if (emitLog) {
      addLog(new Date().toLocaleTimeString(), 'sys', 'Cleared queued messages');
    }
  };

  const activateSessionShell = (session: Session) => {
    const previous = selectedSessionRef.current;
    // Keep the imperative ref in sync immediately. Runtime events and
    // refreshTurns() can fire before React commits setSelectedSession(); if the
    // ref is stale those live updates either no-op or hydrate the wrong chat.
    if (previous && previous.session_id !== session.session_id) {
      clearQueuedMessages(false);
    }
    selectedSessionRef.current = session;
    setSelectedSession(session);
    setSelectedAgentTurnId(null);
    setActiveCoreText('');
    setActivePeerText('');
    setActivePeerName(null);
    setActiveNodes([]);
    setActiveTurnIds([]);
    setActiveCoreTurnId(null);
    setActivePeerTurnId(null);
    setRealtimeTerminalLines({});
    setHyardJobs({});
  };

  const takeNextQueuedMessage = (): SendPayload | null => {
    const pending = messageQueueRef.current;
    if (pending.length === 0) return null;
    const [nextMessage, ...rest] = pending;
    messageQueueRef.current = rest;
    setMessageQueue(rest);
    return nextMessage;
  };

  const loadSessions = async () => {
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

  const selectSession = async (session: Session) => {
    activateSessionShell(session);
    setTurns([]);
    setSessionEvents([]);
    try {
      const turnList = await invoke<Turn[]>('get_session_turns', { sessionId: session.session_id });
      if (selectedSessionRef.current?.session_id !== session.session_id) return;
      setTurns(turnList);
      const eventList = await invoke<any[]>('get_session_events', { sessionId: session.session_id });
      if (selectedSessionRef.current?.session_id !== session.session_id) return;
      setSessionEvents(mergeSessionEventLists([], eventList));
    } catch (e) {
      console.error(e);
    }
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

  const loadAppConfig = async () => {
    try {
      const cfg = await invoke<SwitchyardConfig>('load_config');
      setConfig(cfg);
      if (cfg && cfg.core && cfg.core.default_provider) {
        setNewSessionProvider(cfg.core.default_provider);
      }
      refreshProviderStatuses();
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

  const isSendPipelineBusy = () => isPreparingDispatchRef.current || isDispatchingRef.current;

  const runSingleMessage = async (sessionForSend: Session, payload: SendPayload) => {
    const message = payload.text;
    const imagePaths = payload.imagePaths;
    const filePaths = payload.filePaths ?? [];
    const sandboxMode = sandboxModeRef.current;
    setActiveCoreText('');
    setActivePeerText('');
    setActivePeerName(null);
    setActiveNodes(['host']);
    setActiveTurnIds([]);
    setTelemetryLogs([]);

    // Add visual temp turn/message instantly for reactive feel
    const tempUserTurn: Turn = {
      turn_id: `temp-user-${Date.now()}`,
      session_id: sessionForSend.session_id,
      origin: 'user',
      provider: 'user',
      role: 'core',
      user_message: displayMessageWithAttachmentReferences(payload),
      provider_response: null,
      error_message: null,
      status: 'completed',
      started_at: new Date().toISOString(),
      completed_at: null,
      delegated_by: null
    };
    setTurns((prev) => [...prev, tempUserTurn]);

    try {
      await invoke('run_turn', {
        sessionId: sessionForSend.session_id,
        message,
        provider: sessionForSend.active_core,
        sandboxMode,
        imagePaths,
        filePaths,
      });
    } catch (e) {
      addLog(new Date().toLocaleTimeString(), 'sys', `Execution failed: ${e}`);
    } finally {
      setActiveNodes([]);
      setActivePeerName(null);
      setActiveTurnIds([]);
      setActiveCoreTurnId(null);
      setActivePeerTurnId(null);
      setActiveCoreText('');
      setActivePeerText('');
      // Reload turns database state
      try {
        const updatedTurns = await invoke<Turn[]>('get_session_turns', { sessionId: sessionForSend.session_id });
        if (selectedSessionRef.current?.session_id === sessionForSend.session_id) {
          setTurns(updatedTurns);
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
        const eventList = await invoke<any[]>('get_session_events', { sessionId: sessionForSend.session_id });
        if (selectedSessionRef.current?.session_id === sessionForSend.session_id) {
          setSessionEvents((prev) => mergeSessionEventLists(prev, eventList));
        }
      } catch (e) {
        console.error(e);
      }
    }
  };

  const dispatchMessage = async (sessionForSend: Session, initialPayload: SendPayload) => {
    if (isSendPipelineBusy()) {
      enqueueQueuedMessage(initialPayload);
      return;
    }
    isDispatchingRef.current = true;
    setIsGenerating(true);
    let payload: SendPayload | null = initialPayload;
    try {
      while (payload !== null) {
        await runSingleMessage(sessionForSend, payload);

        // Drain queued messages FIFO without dropping the authoritative
        // in-flight flag between turns. This avoids the old recursive gap where
        // a rapid send at provider-completion time could slip through as a
        // second overlapping `run_turn` instead of joining the queue.
        payload = takeNextQueuedMessage();
        if (payload) {
          addLog(
            new Date().toLocaleTimeString(),
            'sys',
            `Dispatching queued message (${messageQueueRef.current.length} remaining): ${describeSendPayload(payload)}`,
          );
        }
      }
    } finally {
      isDispatchingRef.current = false;
      setIsGenerating(false);
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
    event.preventDefault();
    event.currentTarget.setPointerCapture(event.pointerId);
    document.body.style.cursor = 'col-resize';
    document.body.style.userSelect = 'none';
    document.body.classList.add('is-layout-resizing');

    const leftOrigin = leftColumn.getBoundingClientRect().left;
    let nextWidth = leftColumnWidthRef.current;
    let resizeFrame: number | null = null;
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

    const handlePointerUp = () => {
      document.removeEventListener('pointermove', handlePointerMove);
      document.removeEventListener('pointerup', handlePointerUp);
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
    };

    document.addEventListener('pointermove', handlePointerMove);
    document.addEventListener('pointerup', handlePointerUp, { once: true });
  };

  const startCanvasResize = (event: ReactPointerEvent<HTMLDivElement>) => {
    if (event.button !== 0) return;
    const app = appContainerRef.current;
    const row = mainRowRef.current;
    if (!app || !row) return;
    event.preventDefault();
    event.currentTarget.setPointerCapture(event.pointerId);
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

    const handlePointerUp = () => {
      document.removeEventListener('pointermove', handlePointerMove);
      document.removeEventListener('pointerup', handlePointerUp);
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
    };

    document.addEventListener('pointermove', handlePointerMove);
    document.addEventListener('pointerup', handlePointerUp, { once: true });
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
    const text = rawPayload.text.trim();
    const imagePaths = rawPayload.imagePaths;
    const filePaths = rawPayload.filePaths ?? [];
    if (!text && imagePaths.length === 0 && filePaths.length === 0) return;
    const payload: SendPayload = {
      text: text || (filePaths.length > 0 ? '请分析这些附件。' : '请分析这些图片。'),
      imagePaths,
      filePaths,
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

    // If a turn is already running, this send is a follow-up for that same
    // active session. Queue it synchronously instead of trusting `isGenerating`
    // (React state can lag behind rapid Enter presses and would otherwise allow
    // overlapping run_turn calls).
    if (isSendPipelineBusy()) {
      enqueueQueuedMessage(payload);
      return;
    }

    // No session yet? Mint one on the fly using the current core
    // provider — same path the Sidebar's "+ New" button takes — then
    // dispatch the message into it. This is what the user expects
    // when typing into a fresh workspace: just send, don't make me
    // click "New Session" first.
    let target = selectedSession;
    if (!target) {
      isPreparingDispatchRef.current = true;
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
        setTurns([]);
        setSessionEvents([]);
        target = created;
      } catch (e) {
        appendSystemNote(`Failed to create session: ${e}`);
        // Restore the user's text so they can retry without retyping.
        restoreText?.(rawPayload.text);
        isPreparingDispatchRef.current = false;
        setIsGenerating(false);
        return;
      }
      isPreparingDispatchRef.current = false;
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
          setTurns(refreshed);
        }
      } catch (e) {
        console.error('refresh after rewind failed', e);
      }
      // Queue + dispatch the new message just like a normal send.
      const payload: SendPayload = {
        text: newText,
        imagePaths: extractImagePathsFromAttachmentReferences(newText),
        filePaths: extractFilePathsFromAttachmentReferences(newText),
      };
      if (isSendPipelineBusy()) {
        enqueueQueuedMessage(payload);
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
          setTurns(refreshed);
        }
      } catch (e) {
        console.error('refresh after rewind failed', e);
      }
      const payload: SendPayload = {
        text: message,
        imagePaths: extractImagePathsFromAttachmentReferences(message),
        filePaths: extractFilePathsFromAttachmentReferences(message),
      };
      if (isSendPipelineBusy()) {
        enqueueQueuedMessage(payload);
      } else {
        void dispatchMessage(sessionForSend, payload);
      }
    } catch (e) {
      addLog(new Date().toLocaleTimeString(), 'sys', `Retry failed: ${e}`);
    }
  };

  const handleCancel = async () => {
    try {
      await invoke('cancel_turn');
      addLog(new Date().toLocaleTimeString(), 'sys', '取消指令已发送至智能体内核...');
    } catch (e) {
      addLog(new Date().toLocaleTimeString(), 'sys', `取消失败: ${e}`);
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
        ? 'codex-cli'
        : trimmedBackend === 'claude'
          ? 'claude-cli'
          : trimmedBackend === 'gemini'
            ? 'gemini-cli'
            : 'agy'; // antigravity ships as `agy`, not `antigravity-cli`
    // Antigravity is one-shot and does not accept a `run` subcommand.
    const defaultArgs = trimmedBackend === 'antigravity' ? [] : ['run'];

    setConfig((prev) => {
      if (!prev) return null;
      const providersCopy = { ...prev.providers };
      providersCopy[trimmed] = {
        command: defaultCommand,
        args: defaultArgs,
        env: {},
        timeout_secs: 900,
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
      setSessions((prev) => {
        const next = prev.filter((s) => s.session_id !== sessionId);
        if (selectedSession?.session_id === sessionId) {
          if (next.length > 0) {
            selectSession(next[0]);
          } else {
            setSelectedSession(null);
            setTurns([]);
            setSessionEvents([]);
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
      {/* 0. Icon Rail — mode switcher + bottom settings control. */}
      <IconRail
        mode={railMode}
        onModeChange={setRailMode}
        onOpenDiagnostics={() => setDrawerOpen((v) => !v)}
        onOpenSettings={() => setShowSettings(true)}
      />

      {/* 1. Second column — Workspace header + session list (Chat mode).
          Files/Terminal modes land in later phases; today they show a
          placeholder so the rail still feels responsive. */}
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
        <WorkspaceHeader
          current={currentWorkspace}
          workspaces={workspaces}
          onSwitch={handleSwitchWorkspace}
          onCreate={handleCreateWorkspace}
          onRename={handleRenameWorkspace}
          onUpdateExtraRoots={handleUpdateExtraRoots}
        />
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
          <FilesTree
            // workspace_id as key so swapping workspaces fully resets
            // the tree's cached children + expanded state.
            key={currentWorkspace?.workspace_id ?? 'no-workspace'}
            workspaceId={currentWorkspace?.workspace_id ?? null}
            // Shared with SourceControl — bumping `gitRefreshNonce`
            // re-fetches `git status` so the tree's status colors
            // and folder dots stay in sync with whatever the AI just
            // wrote (or the user staged / discarded).
            gitRefreshNonce={gitRefreshNonce}
            onOpenFile={openFileInCanvas}
          />
        )}
        {railMode === 'source_control' && (
          <SourceControl
            // workspaceId as key forces a full remount on workspace
            // switch so committed-message + section-open state don't
            // bleed across projects.
            key={currentWorkspace?.workspace_id ?? 'no-workspace'}
            workspaceId={currentWorkspace?.workspace_id ?? null}
            refreshNonce={gitRefreshNonce}
            onOpenDiff={openGitDiffInCanvas}
          />
        )}
      </div>

      <div
        role="separator"
        aria-orientation="vertical"
        className="layout-sash layout-sash-vertical"
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
          style={{
            display: 'flex',
            flex: 1,
            minHeight: 0,
            minWidth: 0,
            overflow: 'hidden',
          }}
        >
          <div style={{ flex: 1, minWidth: 0, display: 'flex', flexDirection: 'column' }}>
            <ChatArea
              selectedSession={selectedSession}
              isGenerating={isGenerating}
              turns={turns}
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
              queuedMessages={messageQueue}
              onClearQueue={handleClearQueue}
              sandboxMode={config?.sandbox?.mode ?? DEFAULT_SANDBOX_MODE}
              onSandboxModeChange={handleSandboxModeChange}
              onEditAndResend={handleEditAndResend}
              onRetryLastUserTurn={handleRetryLastUserTurn}
              onOpenFile={openFileInCanvas}
            />
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
                style={{
                  width: 'var(--switchyard-canvas-column-width, 680px)',
                  minWidth: MIN_CANVAS_COLUMN_WIDTH,
                  maxWidth: `calc(100% - ${MIN_CHAT_COLUMN_WIDTH}px)`,
                  display: 'flex',
                  flexDirection: 'column',
                  flexShrink: 0,
                }}
              >
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
              </div>
            </>
          )}
        </div>

        {/* Bottom terminal panel — VS Code-style tab strip + toolbar.
            Keep it mounted after the first open so hiding the panel does
            not force a PTY restart the next time it is shown. */}
        {terminalEverOpened && (
          <TerminalPanel
            visible={terminalOpen}
            cwd={currentWorkspace?.primary_root ?? null}
            onClose={() => setTerminalOpen(false)}
          />
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
            onUpdateSessionSummary={handleUpdateSessionSummary}
            onUpdateSessionChecklist={handleUpdateSessionChecklist}
          />
        </div>
      </div>

      {/* Settings Modal */}
      {showSettings && config && (
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
      )}

      {/* The legacy ArtifactDrawer bottom bar was removed — artifacts
          are accessible via the diagnostics drawer and (Phase 3) via
          the Files mode. Keeping a persistent bar at the bottom eats
          vertical space without earning it. */}

      {/* Topology overlay — fullscreen modal launched from the
          diagnostics drawer header. Rendered above the StatusBar so
          its fixed-position backdrop layers above everything else. */}
      <TopologyOverlay
        open={topologyOverlayOpen}
        onClose={() => setTopologyOverlayOpen(false)}
        activeCore={selectedSession?.active_core || 'None'}
        enabledPeers={selectedSession?.enabled_peers || []}
        activeNode={activeNodes[activeNodes.length - 1] || null}
        isGenerating={isGenerating}
      />

      {/* Status bar — bottom row after the icon rail. Hosts the Terminal
          toggle (VS Code idiom), worker count, workspace name + running
          indicator. */}
      <StatusBar
        workspace={currentWorkspace}
        coreProvider={selectedSession?.active_core ?? null}
        workerCount={sessionWorkers.filter((w) => w.kind === 'worker').length}
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
