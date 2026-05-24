import React, { useRef, useEffect, useState, useCallback } from 'react';
import { convertFileSrc } from '@tauri-apps/api/core';
import { getCurrentWebview } from '@tauri-apps/api/webview';
import { open as openDialog } from '@tauri-apps/plugin-dialog';
import { Send, RefreshCw, MessageSquare, Pencil, Check, Square, Plus, ChevronDown, X, FileText, Image as ImageIcon } from 'lucide-react';
import type { Session, Turn, SandboxMode, SendPayload, InputAttachment } from '../types';
import { completeSlash } from './slashCommands';
import { saveClipboardAttachment } from '../services/api';
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
  renderTurnEvents: (turnId: string, events: any[], turns: Turn[], realtimeLines?: string[], hyardJobs?: Record<string, any>) => React.ReactNode;
  renderTurnActivitySummary: (turnId: string, events: any[], turns: Turn[], realtimeLines?: string[], hyardJobs?: Record<string, any>) => React.ReactNode;
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

function sandboxOptionFor(mode: SandboxMode) {
  return SANDBOX_OPTIONS.find((option) => option.mode === mode) ?? SANDBOX_OPTIONS[1];
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
          const src = convertFileSrc(attachment.path);
          return (
            <button
              key={attachment.path}
              type="button"
              onClick={() => onOpenImage(attachment)}
              title={`${attachment.name || filenameFromPath(attachment.path)}\n${attachment.path}`}
              style={{
                width: 104,
                height: 78,
                border: '1px solid rgba(148, 163, 184, 0.28)',
                borderRadius: 8,
                padding: 0,
                overflow: 'hidden',
                background: 'rgba(15, 23, 42, 0.55)',
                cursor: 'zoom-in',
                position: 'relative',
              }}
            >
              <img
                src={src}
                alt={attachment.name || filenameFromPath(attachment.path)}
                style={{
                  width: '100%',
                  height: '100%',
                  objectFit: 'cover',
                  display: 'block',
                }}
              />
            </button>
          );
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

export const ChatArea: React.FC<ChatAreaProps> = ({
  selectedSession,
  isGenerating,
  turns,
  turnAttachments = {},
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
}) => {
  const messagesEndRef = useRef<HTMLDivElement>(null);
  const scrollRafRef = useRef<number | null>(null);
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
  const [editingTurnId, setEditingTurnId] = useState<string | null>(null);
  const [editingDraft, setEditingDraft] = useState<string>('');
  const [previewAttachment, setPreviewAttachment] = useState<InputAttachment | null>(null);

  // Index of the most recent "real" user-origin turn (excludes the structured
  // system-feedback turns the orchestrator stamps with origin=user but a JSON
  // sentinel payload). Used to gate the edit/retry affordance — only the
  // tail user turn gets the buttons because deleting older ones would also
  // discard everything that came after.
  const lastUserTurnIdx = (() => {
    for (let i = turns.length - 1; i >= 0; i--) {
      const t = turns[i];
      if (t.origin === 'user' && !t.user_message.includes('<<<SWITCHYARD_JSON_BEGIN>>>')) {
        return i;
      }
    }
    return -1;
  })();

  const eventHasRenderableArtifact = (event: any) => {
    const hasMeaningfulArtifactValue = (value: any): boolean => {
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
    };
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
  };

  const hasRenderableActivityForTurn = (turnId?: string | null) => {
    if (!turnId) return false;
    if ((realtimeTerminalLines[turnId]?.length ?? 0) > 0) return true;
    if (hyardJobs?.[turnId]) return true;
    if (turns.some((candidate) => candidate.delegated_by === turnId)) return true;
    return sessionEvents.some((event) => event.turn_id === turnId && eventHasRenderableArtifact(event));
  };

  // No cleanup effect needed: the bubble only enters edit UI when
  // `editingTurnId === t.turn_id`, so a stale id whose turn has been wiped
  // (session switch, rewind) silently goes inert. `beginEdit` always resets
  // `editingDraft` from the fresh turn, so cross-bubble draft bleed is
  // impossible.

  // Auto scroll messages container to bottom when turns/state change. Streaming
  // text/tool logs can update many times per second; repeatedly starting
  // `behavior: "smooth"` animations on every chunk is a real input/paint
  // bottleneck on long transcripts. Throttle to at most one layout scroll per
  // animation frame and use an immediate scroll.
  const activeCoreTerminalLineCount = activeCoreTurnId ? (realtimeTerminalLines[activeCoreTurnId]?.length ?? 0) : 0;
  const activePeerTerminalLineCount = activePeerTurnId ? (realtimeTerminalLines[activePeerTurnId]?.length ?? 0) : 0;
  useEffect(() => {
    return () => {
      if (scrollRafRef.current !== null) {
        cancelAnimationFrame(scrollRafRef.current);
        scrollRafRef.current = null;
      }
    };
  }, []);
  useEffect(() => {
    if (scrollRafRef.current !== null) return;
    scrollRafRef.current = requestAnimationFrame(() => {
      scrollRafRef.current = null;
      messagesEndRef.current?.scrollIntoView({ block: 'end' });
    });
  }, [
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
  ]);

  const beginEdit = (turn: Turn) => {
    setEditingTurnId(turn.turn_id);
    setEditingDraft(stripAttachmentReferences(turn.user_message));
  };
  const cancelEdit = () => {
    setEditingTurnId(null);
    setEditingDraft('');
  };
  const commitEdit = () => {
    const trimmed = editingDraft.trim();
    if (!trimmed || !editingTurnId) return;
    const id = editingTurnId;
    setEditingTurnId(null);
    setEditingDraft('');
    onEditAndResend(id, trimmed);
  };

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
            <img
              src={convertFileSrc(previewAttachment.path)}
              alt={previewAttachment.name || filenameFromPath(previewAttachment.path)}
              style={{
                maxWidth: 'min(86vw, 1100px)',
                maxHeight: '78vh',
                objectFit: 'contain',
                borderRadius: 8,
                background: '#020617',
              }}
            />
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

      <div className="chat-messages" style={{ flex: 1, overflowY: 'auto', padding: '12px', display: 'flex', flexDirection: 'column', gap: '14px' }}>
        {turns.length === 0 && !activeCoreText && !isGenerating && !activeCoreTurnId ? (
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
          turns.map((t, idx) => {
            const isSystemFeedback = t.user_message.includes('<<<SWITCHYARD_JSON_BEGIN>>>');
            const assistantContent = t.provider_response || t.error_message;
            const renderedByActiveCorePanel = isGenerating && activeCoreTurnId === t.turn_id && !activePeerName;
            const renderedByActivePeerPanel = isGenerating && activePeerTurnId === t.turn_id && Boolean(activePeerName);
            const hasAssistantActivity =
              !renderedByActiveCorePanel &&
              !renderedByActivePeerPanel &&
              hasRenderableActivityForTurn(t.turn_id);

            if (t.origin === 'user') {
              if (isSystemFeedback) {
                let parsedResults = null;
                try {
                  const match = t.user_message.match(/<<<SWITCHYARD_JSON_BEGIN>>>([\s\S]*?)<<<SWITCHYARD_JSON_END>>>/);
                  if (match && match[1]) {
                    parsedResults = JSON.parse(match[1]);
                  }
                } catch (e) {
                  // Ignore parsing error
                }

                return (
                  <React.Fragment key={t.turn_id || idx}>
                    {parsedResults && parsedResults.results && (
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
                          {parsedResults.results.map((res: any, rIdx: number) => (
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
                    )}
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
                        {renderTurnEvents(t.turn_id, sessionEvents, turns, realtimeTerminalLines[t.turn_id], hyardJobs)}
                      </div>
                    )}
                  </React.Fragment>
                );
              }

              const isLastUser = idx === lastUserTurnIdx;
              const isEditing = editingTurnId === t.turn_id;
              const showActions = isLastUser && !isGenerating && !isEditing && Boolean(t.turn_id);
              const stamp = t.started_at
                ? new Date(t.started_at).toLocaleTimeString(undefined, {
                    hour: '2-digit',
                    minute: '2-digit',
                  })
                : '';
              const visibleUserMessage = stripAttachmentReferences(t.user_message);
              const attachmentsForTurn = mergeInputAttachments(
                turnAttachments[t.turn_id],
                extractAttachmentsFromAttachmentReferences(t.user_message),
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
                            onClick={() => beginEdit(t)}
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
                          onChange={(e) => setEditingDraft(e.target.value)}
                          rows={Math.min(8, Math.max(2, editingDraft.split('\n').length))}
                          autoFocus
                          onKeyDown={(e) => {
                            if (e.key === 'Enter' && (e.ctrlKey || e.metaKey)) {
                              e.preventDefault();
                              commitEdit();
                            } else if (e.key === 'Escape') {
                              e.preventDefault();
                              cancelEdit();
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
                            onClick={cancelEdit}
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
                            onClick={commitEdit}
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
                          onOpenImage={setPreviewAttachment}
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
                      {renderTurnEvents(t.turn_id, sessionEvents, turns, realtimeTerminalLines[t.turn_id], hyardJobs)}
                    </div>
                  )}
                </React.Fragment>
              );
            } else if (t.origin === 'system') {
              let parsedResults = null;
              try {
                const match = t.user_message.match(/<<<SWITCHYARD_JSON_BEGIN>>>([\s\S]*?)<<<SWITCHYARD_JSON_END>>>/);
                if (match && match[1]) {
                  parsedResults = JSON.parse(match[1]);
                }
              } catch (e) {
                // Ignore parsing error
              }

              return (
                <React.Fragment key={t.turn_id || idx}>
                  {parsedResults && parsedResults.results && (
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
                        {parsedResults.results.map((res: any, rIdx: number) => (
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
                  )}
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
                      {renderTurnEvents(t.turn_id, sessionEvents, turns, realtimeTerminalLines[t.turn_id], hyardJobs)}
                    </div>
                  )}
                </React.Fragment>
              );
            } else if (t.origin === 'delegate') {
              return null;
            } else {
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
                  {renderTurnEvents(t.turn_id, sessionEvents, turns, realtimeTerminalLines[t.turn_id], hyardJobs)}
                </div>
              );
            }
          })
        )}

        {/* Active Core fallback before the backend reports the canonical turn id. */}
        {isGenerating && !activeCoreText && !activeCoreTurnId && !activePeerName && (
          <div className="message-assistant-flow">
            <div className="message-header">{selectedSession?.active_core ?? 'Core'} (core)</div>
            <div className="message-body" style={{ display: 'flex', alignItems: 'center', gap: '8px', color: 'var(--text-secondary)' }}>
              <span className="thinking-dots">正在准备并启动 core provider…</span>
              <span className="spinner-small"></span>
            </div>
          </div>
        )}

        {/* Active Core Streaming response container.
            Keep this visible even during tool-only/status-only phases, otherwise
            the user sees a silent wait while runtime events are arriving. */}
        {(activeCoreText || (isGenerating && activeCoreTurnId && !activePeerName)) && (
          <div className="message-assistant-flow">
            <div className="message-header">{selectedSession?.active_core ?? 'Core'} (core)</div>
            {renderedActiveCoreText ? (
              <>
                <RenderedMessageBody
                  text={renderedActiveCoreText}
                  renderMessageBody={renderMessageBody}
                  onOpenFile={onOpenFile}
                />
                {activeCoreTurnId && renderTurnActivitySummary(activeCoreTurnId, sessionEvents, turns, realtimeTerminalLines[activeCoreTurnId], hyardJobs)}
              </>
            ) : activeCoreTurnId ? (
              renderTurnActivitySummary(activeCoreTurnId, sessionEvents, turns, realtimeTerminalLines[activeCoreTurnId], hyardJobs)
            ) : (
              <div className="message-body" style={{ display: 'flex', alignItems: 'center', gap: '8px', color: 'var(--text-secondary)' }}>
                <span className="thinking-dots">正在连接 provider，等待首个流式事件…</span>
                <span className="spinner-small"></span>
              </div>
            )}
          </div>
        )}

        {/* Active Peer Streaming delegation box */}
        {activePeerName && (
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
                {activePeerTurnId && renderTurnActivitySummary(activePeerTurnId, sessionEvents, turns, realtimeTerminalLines[activePeerTurnId], hyardJobs)}
              </>
            ) : activePeerTurnId ? (
              renderTurnActivitySummary(activePeerTurnId, sessionEvents, turns, realtimeTerminalLines[activePeerTurnId], hyardJobs)
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

      <ChatComposer
        selectedSession={selectedSession}
        isGenerating={isGenerating}
        handleSend={composerSend}
        handleCancel={composerCancel}
        sandboxMode={sandboxMode}
        onSandboxModeChange={onSandboxModeChange}
      />
    </div>
  );
};

interface ChatComposerProps {
  selectedSession: Session | null;
  isGenerating: boolean;
  handleSend: (payload: SendPayload, restoreText?: (text: string) => void) => void | Promise<void>;
  handleCancel: () => void;
  sandboxMode: SandboxMode;
  onSandboxModeChange: (mode: SandboxMode) => void | Promise<void>;
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

  const saveDroppedOrPastedFiles = useCallback(async (files: File[]) => {
    const savedPaths: string[] = [];
    for (const file of files) {
      const nativePath = (file as File & { path?: string }).path;
      if (typeof nativePath === 'string' && nativePath.trim()) {
        savedPaths.push(nativePath);
        continue;
      }

      try {
        const dataUrl = await readFileAsDataUrl(file);
        const savedPath = await saveClipboardAttachment(
          file.name || undefined,
          file.type || undefined,
          dataUrl,
        );
        savedPaths.push(savedPath);
      } catch (error) {
        console.error('Failed to save pasted/dropped attachment', error);
        setAttachmentError(`无法读取或保存附件 ${file.name || 'clipboard item'}：${String(error)}`);
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
            addAttachmentsFromPaths(payload.paths);
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
  }, [addAttachmentsFromPaths, isPointInsideComposer]);

  const handlePaste = useCallback((event: React.ClipboardEvent<HTMLTextAreaElement>) => {
    const files = Array.from(event.clipboardData?.files ?? []);
    if (files.length === 0) return;
    event.preventDefault();
    void saveDroppedOrPastedFiles(files);
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
      void saveDroppedOrPastedFiles(files);
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
