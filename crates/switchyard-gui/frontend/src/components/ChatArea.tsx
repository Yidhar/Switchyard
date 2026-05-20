import React, { useRef, useEffect } from 'react';
import { Send, X, RefreshCw, MessageSquare } from 'lucide-react';
import type { Session, Turn } from '../types';

interface ChatAreaProps {
  selectedSession: Session | null;
  isGenerating: boolean;
  turns: Turn[];
  inputText: string;
  setInputText: (text: string) => void;
  handleSend: () => void;
  handleCancel: () => void;
  activeCoreText: string | null;
  activeCoreTurnId: string | null;
  activePeerName: string | null;
  activePeerTurnId: string | null;
  activePeerText: string | null;
  sessionEvents: any[];
  realtimeTerminalLines: Record<string, string[]>;
  renderMessageBody: (text: string | null) => React.ReactNode;
  renderTurnEvents: (turnId: string, events: any[], turns: Turn[], realtimeLines?: string[], hyardJobs?: Record<string, any>) => React.ReactNode;
  queuedMessages: string[];
  onClearQueue: () => void;
}

export const ChatArea: React.FC<ChatAreaProps> = ({
  selectedSession,
  isGenerating,
  turns,
  inputText,
  setInputText,
  handleSend,
  handleCancel,
  activeCoreText,
  activeCoreTurnId,
  activePeerName,
  activePeerTurnId,
  activePeerText,
  sessionEvents,
  realtimeTerminalLines,
  renderMessageBody,
  renderTurnEvents,
  queuedMessages,
  onClearQueue,
}) => {
  const messagesEndRef = useRef<HTMLDivElement>(null);

  // Auto scroll messages container to bottom when turns/state change
  useEffect(() => {
    messagesEndRef.current?.scrollIntoView({ behavior: 'smooth' });
  }, [turns, isGenerating, activeCoreText, activePeerText, queuedMessages.length]);

  return (
    <div className="main-content glass-panel" style={{ display: 'flex', flexDirection: 'column', height: '100%', overflow: 'hidden' }}>
      <div className="chat-header">
        <div className="chat-header-info">
          <h2>
            {selectedSession ? `Session: ${selectedSession.session_id.substring(0, 8)}` : 'No Session Selected'}
          </h2>
          <p>
            {selectedSession 
              ? `Active Core: ${selectedSession.active_core} | Peers: ${selectedSession.enabled_peers.join(', ') || 'None'}` 
              : 'Create a session to begin'}
          </p>
        </div>
        <div className="chat-actions">
          {isGenerating && (
            <div style={{ display: 'flex', alignItems: 'center', gap: '8px', color: 'var(--color-secondary)', fontSize: '13px' }}>
              <RefreshCw className="spin" size={16} style={{ animation: 'spin 2s linear infinite' }} />
              <span>Running Multi-Agent Route...</span>
            </div>
          )}
        </div>
      </div>

      <div className="chat-messages" style={{ flex: 1, overflowY: 'auto', padding: '16px', display: 'flex', flexDirection: 'column', gap: '16px' }}>
        {turns.length === 0 && !activeCoreText ? (
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
                    {(t.provider_response || t.error_message) && (
                      <div className="message-bubble message-assistant">
                        <div className="message-header">{t.provider} ({t.role})</div>
                        {renderMessageBody(t.provider_response || t.error_message)}
                        {renderTurnEvents(t.turn_id, sessionEvents, turns, realtimeTerminalLines[t.turn_id])}
                      </div>
                    )}
                  </React.Fragment>
                );
              }

              return (
                <React.Fragment key={t.turn_id || idx}>
                  <div className="message-bubble message-user">
                    <div className="message-header">You</div>
                    {renderMessageBody(t.user_message)}
                  </div>
                  {(t.provider_response || t.error_message) && (
                    <div className="message-bubble message-assistant">
                      <div className="message-header">{t.provider} ({t.role})</div>
                      {renderMessageBody(t.provider_response || t.error_message)}
                      {renderTurnEvents(t.turn_id, sessionEvents, turns, realtimeTerminalLines[t.turn_id])}
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
                  {(t.provider_response || t.error_message) && (
                    <div className="message-bubble message-assistant">
                      <div className="message-header">{t.provider} ({t.role})</div>
                      {renderMessageBody(t.provider_response || t.error_message)}
                      {renderTurnEvents(t.turn_id, sessionEvents, turns, realtimeTerminalLines[t.turn_id])}
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
                  {renderMessageBody(t.provider_response || t.error_message)}
                  {renderTurnEvents(t.turn_id, sessionEvents, turns, realtimeTerminalLines[t.turn_id])}
                </div>
              );
            }
          })
        )}

        {/* Active Core Streaming response container or Preparing indicator */}
        {isGenerating && !activeCoreText && !activePeerName && (
          <div className="message-bubble message-assistant">
            <div className="message-header">{selectedSession?.active_core} (core)</div>
            <div className="message-body" style={{ display: 'flex', alignItems: 'center', gap: '8px', color: 'var(--text-secondary)' }}>
              <span className="thinking-dots">Orchestrator preparing and executing core provider</span>
              <span className="spinner-small"></span>
            </div>
          </div>
        )}

        {/* Active Core Streaming response container */}
        {activeCoreText && (
          <div className="message-bubble message-assistant">
            <div className="message-header">{selectedSession?.active_core} (core)</div>
            {renderMessageBody(activeCoreText)}
            {activeCoreTurnId && renderTurnEvents(activeCoreTurnId, sessionEvents, turns, realtimeTerminalLines[activeCoreTurnId])}
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
            <div className="message-body" style={{ fontStyle: 'italic', display: 'flex', alignItems: 'center', gap: '8px' }}>
              <span>{activePeerText || 'Waiting for output...'}</span>
              {!activePeerText && <span className="spinner-small"></span>}
            </div>
            {activePeerTurnId && renderTurnEvents(activePeerTurnId, sessionEvents, turns, realtimeTerminalLines[activePeerTurnId])}
          </div>
        )}

        {/* Queued (not-yet-dispatched) user messages */}
        {queuedMessages.length > 0 && (
          <>
            {queuedMessages.map((msg, qIdx) => (
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
                {renderMessageBody(msg)}
              </div>
            ))}
          </>
        )}

        <div ref={messagesEndRef} />
      </div>

      <div className="chat-input-container">
        <div className="input-wrapper">
          <textarea
            className="chat-textarea"
            placeholder={
              !selectedSession
                ? 'Select or create a session to start...'
                : isGenerating
                  ? `Type to queue (${queuedMessages.length} pending) — dispatches after current turn`
                  : 'Ask Switchyard orchestrator to execute routed operations...'
            }
            value={inputText}
            onChange={(e) => setInputText(e.target.value)}
            disabled={!selectedSession}
            onKeyDown={(e) => {
              if (e.key === 'Enter' && !e.shiftKey) {
                e.preventDefault();
                handleSend();
              }
            }}
          />
          <div style={{ display: 'flex', gap: '6px' }}>
            {isGenerating && (
              <button
                className="btn-cancel"
                onClick={handleCancel}
                title="Stop current execution (queued messages are kept)"
                style={{ background: 'rgba(239, 68, 68, 0.2)', color: '#ef4444', border: '1px solid rgba(239, 68, 68, 0.4)' }}
              >
                <X size={16} />
              </button>
            )}
            <button
              className="btn-send"
              onClick={handleSend}
              disabled={!selectedSession || !inputText.trim()}
              title={isGenerating ? 'Queue this message — will dispatch after the current turn' : 'Send'}
            >
              <Send size={16} />
            </button>
          </div>
        </div>
      </div>
    </div>
  );
};
export default ChatArea;
