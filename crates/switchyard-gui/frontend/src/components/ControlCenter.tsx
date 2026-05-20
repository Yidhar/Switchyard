import React, { useState, useRef, useEffect } from 'react';
import { Terminal, Users, Network, Search, ChevronLeft, RefreshCw, ClipboardList, FileText, Trash2, Edit } from 'lucide-react';
import type { Turn, TelemetryLog, ProviderStatus, Session } from '../types';
import { TopologyGraph } from './ui/TopologyGraph';

interface ChecklistItem {
  id: string;
  text: string;
  completed: boolean;
}

interface ControlCenterProps {
  activeCore: string;
  enabledPeers: string[];
  activeNode: string | null;
  isGenerating: boolean;
  turns: Turn[];
  sessionEvents: any[];
  realtimeTerminalLines: Record<string, string[]>;
  hyardJobs: Record<string, any>;
  selectedAgentTurnId: string | null;
  setSelectedAgentTurnId: (id: string | null) => void;
  telemetryLogs: TelemetryLog[];
  activeTurnIds: string[];
  activeNodes: string[];
  activePeerName: string | null;
  activePeerTurnId: string | null;
  activePeerText: string | null;
  activeCoreText: string | null;
  renderTurnEvents: (turnId: string, events: any[], turns: Turn[], realtimeLines?: string[], hyardJobs?: Record<string, any>) => React.ReactNode;
  providerStatuses: ProviderStatus[];
  providerStatusLoading: boolean;
  providerStatusError: string | null;
  refreshProviderStatuses: () => void;
  activePersistentInstances: string[];
  onStartPersistentInstance: (provider: string) => Promise<void>;
  onStopPersistentInstance: (provider: string) => Promise<void>;
  selectedSession: Session | null;
  onUpdateSessionSummary: (sessionId: string, summary: string | null) => Promise<void>;
  onUpdateSessionChecklist: (sessionId: string, checklistJson: string) => Promise<void>;
}

type TabType = 'topology' | 'agents' | 'checklist' | 'summary' | 'telemetry';

export const ControlCenter: React.FC<ControlCenterProps> = ({
  activeCore,
  enabledPeers,
  activeNode,
  isGenerating,
  turns,
  sessionEvents,
  realtimeTerminalLines,
  hyardJobs,
  selectedAgentTurnId,
  setSelectedAgentTurnId,
  telemetryLogs,
  activeTurnIds,
  activeNodes,
  activePeerName,
  activePeerTurnId,
  activePeerText,
  activeCoreText,
  renderTurnEvents,
  providerStatuses,
  providerStatusLoading,
  providerStatusError,
  refreshProviderStatuses,
  activePersistentInstances,
  onStartPersistentInstance,
  onStopPersistentInstance,
  selectedSession,
  onUpdateSessionSummary,
  onUpdateSessionChecklist,
}) => {
  const [activeTab, setActiveTab] = useState<TabType>('topology');
  const [logSearch, setLogSearch] = useState('');
  const [selectedLogTag, setSelectedLogTag] = useState<'all' | 'core' | 'peer' | 'sys' | 'info'>('all');
  const [actionLoading, setActionLoading] = useState<Record<string, boolean>>({});
  
  const [editingSummary, setEditingSummary] = useState(false);
  const [summaryText, setSummaryText] = useState('');

  // Sync summaryText when selectedSession changes
  useEffect(() => {
    setSummaryText(selectedSession?.summary || '');
  }, [selectedSession?.summary]);

  const checklistItems: ChecklistItem[] = React.useMemo(() => {
    if (!selectedSession?.native_bindings?.checklist) return [];
    try {
      return JSON.parse(selectedSession.native_bindings.checklist);
    } catch (e) {
      console.error('Failed to parse checklist JSON:', e);
      return [];
    }
  }, [selectedSession?.native_bindings?.checklist]);

  const handleToggleChecklistItem = (itemId: string) => {
    if (!selectedSession) return;
    const nextItems = checklistItems.map((item) =>
      item.id === itemId ? { ...item, completed: !item.completed } : item
    );
    onUpdateSessionChecklist(selectedSession.session_id, JSON.stringify(nextItems));
  };

  const handleAddChecklistItem = (text: string) => {
    if (!selectedSession || !text.trim()) return;
    const newItem: ChecklistItem = {
      id: Math.random().toString(36).substring(2, 9),
      text: text.trim(),
      completed: false,
    };
    const nextItems = [...checklistItems, newItem];
    onUpdateSessionChecklist(selectedSession.session_id, JSON.stringify(nextItems));
  };

  const handleDeleteChecklistItem = (itemId: string) => {
    if (!selectedSession) return;
    const nextItems = checklistItems.filter((item) => item.id !== itemId);
    onUpdateSessionChecklist(selectedSession.session_id, JSON.stringify(nextItems));
  };
  
  const handleNodeSelect = (nodeName: string) => {
    if (nodeName === 'host') {
      setActiveTab('telemetry');
      setSelectedLogTag('sys');
      setLogSearch('');
    } else if (nodeName.toLowerCase() === activeCore.toLowerCase()) {
      setActiveTab('telemetry');
      setSelectedLogTag('core');
      setLogSearch('');
    } else {
      setActiveTab('telemetry');
      setSelectedLogTag('peer');
      setLogSearch(nodeName);
    }
  };

  const handleTogglePersistent = async (providerId: string, active: boolean) => {
    setActionLoading(prev => ({ ...prev, [providerId]: true }));
    try {
      if (active) {
        await onStopPersistentInstance(providerId);
      } else {
        await onStartPersistentInstance(providerId);
      }
    } catch (e) {
      alert(e);
    } finally {
      setActionLoading(prev => ({ ...prev, [providerId]: false }));
    }
  };
  
  const telemetryEndRef = useRef<HTMLDivElement>(null);

  // Auto scroll telemetry logs to bottom
  useEffect(() => {
    if (activeTab === 'telemetry') {
      telemetryEndRef.current?.scrollIntoView({ behavior: 'smooth' });
    }
  }, [telemetryLogs, activeTab]);

  // Build the list of agents/jobs
  const getAgentsList = () => {
    const list: any[] = [];
    
    // 1. Core Turn
    const activeCoreTurn = turns.find((t) => t.status === 'running' && t.role === 'core');
    if (activeCoreTurn) {
      list.push({
        id: activeCoreTurn.turn_id,
        name: activeCoreTurn.provider,
        role: 'core',
        status: 'running',
        task: activeCoreTurn.user_message,
        response: activeCoreText || 'Running central orchestrator...',
        error: null,
        started_at: activeCoreTurn.started_at,
        completed_at: null,
      });
    }

    // 2. Active turns in db
    turns.forEach((t) => {
      if (t.role !== 'core' && !list.some((a) => a.id === t.turn_id)) {
        list.push({
          id: t.turn_id,
          name: t.provider,
          role: t.role,
          status: t.status,
          task: t.user_message,
          response: t.provider_response || (t.status === 'running' ? 'Running task...' : ''),
          error: t.error_message,
          started_at: t.started_at,
          completed_at: t.completed_at,
        });
      }
    });

    // 3. Dynamic stream active peer
    if (activePeerName && activePeerTurnId) {
      const existing = list.find((a) => a.id === activePeerTurnId);
      if (existing) {
        existing.status = 'running';
        if (activePeerText) existing.response = activePeerText;
      } else {
        list.push({
          id: activePeerTurnId,
          name: activePeerName,
          role: 'worker',
          status: 'running',
          task: 'Delegated task execution',
          response: activePeerText || 'Executing...',
          error: null,
          started_at: new Date().toISOString(),
          completed_at: null,
        });
      }
    }

    // 4. Merge observed HYARD jobs
    Object.values(hyardJobs).forEach((job: any) => {
      if (!list.some((a) => a.id === job.job_id)) {
        list.push({
          id: job.job_id,
          name: job.provider,
          role: 'worker',
          status: job.status,
          task: job.last_event || 'Background delegated task',
          response: job.last_output_preview || (job.status === 'queued' ? 'Queued...' : 'Executing...'),
          error: job.error,
          started_at: job.observed_at,
          completed_at: (job.status === 'completed' || job.status === 'failed') ? job.observed_at : null,
        });
      }
    });

    return list;
  };

  const agents = getAgentsList();

  // Filter telemetry logs
  const filteredLogs = telemetryLogs.filter((log) => {
    const matchesTag = selectedLogTag === 'all' || log.tag === selectedLogTag;
    const matchesSearch = log.message.toLowerCase().includes(logSearch.toLowerCase()) || 
                          log.tag.toLowerCase().includes(logSearch.toLowerCase());
    return matchesTag && matchesSearch;
  });

  return (
    <div className="control-panel glass-panel" style={{ display: 'flex', flexDirection: 'column', height: '100%', overflow: 'hidden' }}>
      {/* Tabs Header */}
      <div 
        style={{ 
          display: 'flex', 
          borderBottom: '1px solid var(--border-muted)', 
          background: 'rgba(0,0,0,0.1)', 
          padding: '4px' 
        }}
      >
        <button 
          onClick={() => setActiveTab('topology')}
          style={{
            flex: 1,
            display: 'flex',
            alignItems: 'center',
            justifyContent: 'center',
            gap: '6px',
            padding: '8px 4px',
            fontSize: '11px',
            fontWeight: '600',
            borderRadius: '4px',
            background: activeTab === 'topology' ? 'rgba(59, 130, 246, 0.1)' : 'transparent',
            color: activeTab === 'topology' ? 'var(--color-primary)' : 'var(--text-secondary)',
            border: 'none',
            cursor: 'pointer',
            transition: 'all 0.2s',
          }}
        >
          <Network size={13} />
          Topology
        </button>
        <button 
          onClick={() => setActiveTab('agents')}
          style={{
            flex: 1,
            display: 'flex',
            alignItems: 'center',
            justifyContent: 'center',
            gap: '6px',
            padding: '8px 4px',
            fontSize: '11px',
            fontWeight: '600',
            borderRadius: '4px',
            background: activeTab === 'agents' ? 'rgba(59, 130, 246, 0.1)' : 'transparent',
            color: activeTab === 'agents' ? 'var(--color-primary)' : 'var(--text-secondary)',
            border: 'none',
            cursor: 'pointer',
            transition: 'all 0.2s',
          }}
        >
          <Users size={13} />
          Agents ({agents.length})
        </button>
        <button 
          onClick={() => setActiveTab('checklist')}
          style={{
            flex: 1,
            display: 'flex',
            alignItems: 'center',
            justifyContent: 'center',
            gap: '6px',
            padding: '8px 4px',
            fontSize: '11px',
            fontWeight: '600',
            borderRadius: '4px',
            background: activeTab === 'checklist' ? 'rgba(59, 130, 246, 0.1)' : 'transparent',
            color: activeTab === 'checklist' ? 'var(--color-primary)' : 'var(--text-secondary)',
            border: 'none',
            cursor: 'pointer',
            transition: 'all 0.2s',
          }}
        >
          <ClipboardList size={13} />
          Checklist ({checklistItems.filter(i => !i.completed).length})
        </button>
        <button 
          onClick={() => setActiveTab('summary')}
          style={{
            flex: 1,
            display: 'flex',
            alignItems: 'center',
            justifyContent: 'center',
            gap: '6px',
            padding: '8px 4px',
            fontSize: '11px',
            fontWeight: '600',
            borderRadius: '4px',
            background: activeTab === 'summary' ? 'rgba(59, 130, 246, 0.1)' : 'transparent',
            color: activeTab === 'summary' ? 'var(--color-primary)' : 'var(--text-secondary)',
            border: 'none',
            cursor: 'pointer',
            transition: 'all 0.2s',
          }}
        >
          <FileText size={13} />
          Summary
        </button>
        <button 
          onClick={() => setActiveTab('telemetry')}
          style={{
            flex: 1,
            display: 'flex',
            alignItems: 'center',
            justifyContent: 'center',
            gap: '6px',
            padding: '8px 4px',
            fontSize: '11px',
            fontWeight: '600',
            borderRadius: '4px',
            background: activeTab === 'telemetry' ? 'rgba(59, 130, 246, 0.1)' : 'transparent',
            color: activeTab === 'telemetry' ? 'var(--color-primary)' : 'var(--text-secondary)',
            border: 'none',
            cursor: 'pointer',
            transition: 'all 0.2s',
          }}
        >
          <Terminal size={13} />
          Telemetry
        </button>
      </div>

      {/* Tab Contents */}
      <div style={{ flex: 1, overflowY: 'auto', display: 'flex', flexDirection: 'column' }}>
        
        {/* 1. Topology View */}
        {activeTab === 'topology' && (
          <div style={{ padding: '16px', display: 'flex', flexDirection: 'column', gap: '16px' }}>
            <TopologyGraph 
              activeCore={activeCore}
              enabledPeers={enabledPeers}
              activeNode={activeNode}
              isGenerating={isGenerating}
              onNodeSelect={handleNodeSelect}
            />
            
            {/* Quick Stats Panel */}
            <div 
              className="glass-panel" 
              style={{ 
                padding: '12px', 
                fontSize: '11px', 
                display: 'flex', 
                flexDirection: 'column', 
                gap: '8px',
                background: 'rgba(255, 255, 255, 0.01)'
              }}
            >
              <div style={{ fontWeight: 'bold', borderBottom: '1px solid rgba(255,255,255,0.05)', paddingBottom: '4px' }}>Orchestration Rules</div>
              <div style={{ display: 'flex', justifyContent: 'space-between' }}>
                <span style={{ color: 'var(--text-muted)' }}>Core Orchestrator</span>
                <span style={{ color: 'var(--color-primary)', textTransform: 'capitalize' }}>{activeCore}</span>
              </div>
              <div style={{ display: 'flex', justifyContent: 'space-between' }}>
                <span style={{ color: 'var(--text-muted)' }}>Active Delegate Workers</span>
                <span>{enabledPeers.length > 0 ? enabledPeers.join(', ') : 'None configured'}</span>
              </div>
              <div style={{ display: 'flex', justifyContent: 'space-between' }}>
                <span style={{ color: 'var(--text-muted)' }}>Event Pipeline</span>
                <span style={{ color: 'var(--color-success)' }}>Websocket Bridge OK</span>
              </div>
            </div>
          </div>
        )}

        {/* 2. Agents List / Details */}
        {activeTab === 'agents' && (
          <div style={{ padding: '12px', display: 'flex', flexDirection: 'column', flex: 1 }}>
            {selectedAgentTurnId ? (
              // Agent Details View
              (() => {
                const agent = agents.find((a) => a.id === selectedAgentTurnId);
                if (!agent) {
                  return (
                    <div style={{ display: 'flex', flexDirection: 'column', gap: '10px' }}>
                      <div style={{ color: 'var(--text-muted)', fontStyle: 'italic' }}>Agent execution not found.</div>
                      <button className="btn-back-link" onClick={() => setSelectedAgentTurnId(null)}>
                        &larr; Back to List
                      </button>
                    </div>
                  );
                }

                return (
                  <div className="agent-detail-container" style={{ display: 'flex', flexDirection: 'column', gap: '12px', flex: 1 }}>
                    <button 
                      className="btn-back-link" 
                      onClick={() => setSelectedAgentTurnId(null)}
                      style={{
                        display: 'flex',
                        alignItems: 'center',
                        gap: '4px',
                        background: 'transparent',
                        border: 'none',
                        color: 'var(--color-primary)',
                        cursor: 'pointer',
                        fontSize: '11px',
                        fontWeight: '600',
                        alignSelf: 'flex-start',
                      }}
                    >
                      <ChevronLeft size={12} /> Back to List
                    </button>
                    
                    <div className="agent-detail-header" style={{ display: 'flex', justifyContent: 'space-between', alignItems: 'center' }}>
                      <span className="agent-detail-title" style={{ fontWeight: 'bold', fontSize: '14px', textTransform: 'capitalize' }}>
                        {agent.name}
                      </span>
                      <span className={`status-badge status-${agent.status}`}>
                        {agent.status}
                      </span>
                    </div>

                    <div className="agent-meta-grid" style={{ display: 'grid', gridTemplateColumns: '1fr 1fr', gap: '8px', fontSize: '11px', borderBottom: '1px solid var(--border-muted)', paddingBottom: '8px' }}>
                      <div><strong>Role:</strong> {agent.role.toUpperCase()}</div>
                      <div><strong>Started:</strong> {new Date(agent.started_at).toLocaleTimeString()}</div>
                      {agent.completed_at && (
                        <div style={{ gridColumn: 'span 2' }}><strong>Ended:</strong> {new Date(agent.completed_at).toLocaleTimeString()}</div>
                      )}
                    </div>

                    <div className="agent-detail-section">
                      <span className="agent-section-title" style={{ fontSize: '11px', fontWeight: 'bold', color: 'var(--text-muted)', display: 'block', marginBottom: '4px' }}>
                        Task Description
                      </span>
                      <div className="agent-section-content" style={{ fontSize: '12px', background: 'rgba(0,0,0,0.1)', padding: '8px', borderRadius: '4px' }}>
                        {agent.task || 'Orchestration prompt'}
                      </div>
                    </div>

                    <div className="agent-detail-section" style={{ display: 'flex', flexDirection: 'column', flex: 1, minHeight: '150px' }}>
                      <span className="agent-section-title" style={{ fontSize: '11px', fontWeight: 'bold', color: 'var(--text-muted)', display: 'block', marginBottom: '4px' }}>
                        Output Logs & Terminal Preview
                      </span>
                      <div style={{ overflowY: 'auto', flex: 1, background: 'rgba(0,0,0,0.15)', padding: '6px', borderRadius: '4px' }}>
                        {renderTurnEvents(agent.id, sessionEvents, turns, realtimeTerminalLines[agent.id], hyardJobs) || (
                          <span style={{ color: 'var(--text-muted)', fontStyle: 'italic', fontSize: '11px', padding: '6px', display: 'block' }}>
                            No terminal output for this execution.
                          </span>
                        )}
                      </div>
                    </div>
                  </div>
                );
              })()
            ) : (
              // Agent Cards List View
              <div style={{ display: 'flex', flexDirection: 'column', gap: '12px' }}>
                <div style={{ display: 'flex', flexDirection: 'column', gap: '8px' }}>
                  {agents.map((agent) => (
                    <div 
                      key={agent.id} 
                      className={`agent-card glass-panel-interactive ${activeTurnIds.includes(agent.id) || agent.status === 'running' || activeNodes.includes(agent.name) ? 'active' : ''}`}
                      onClick={() => setSelectedAgentTurnId(agent.id)}
                      style={{
                        padding: '10px 12px',
                        borderRadius: '4px',
                        cursor: 'pointer',
                        borderLeft: `3px solid ${agent.status === 'running' ? 'var(--color-primary)' : agent.status === 'completed' ? 'var(--color-success)' : 'var(--border-muted)'}`,
                      }}
                    >
                      <div className="agent-card-header" style={{ display: 'flex', justifyContent: 'space-between', alignItems: 'center', marginBottom: '6px' }}>
                        <div className="agent-name-role">
                          <span className="agent-card-name" style={{ fontWeight: 'bold', fontSize: '12px', textTransform: 'capitalize' }}>{agent.name}</span>
                          <span className="agent-card-role" style={{ fontSize: '10px', color: 'var(--text-muted)', marginLeft: '6px', textTransform: 'uppercase' }}>({agent.role})</span>
                        </div>
                        <span className={`status-badge status-${agent.status}`}>
                          {agent.status}
                        </span>
                      </div>
                      <div className="agent-card-preview" style={{ fontSize: '11px', color: 'var(--text-secondary)', textOverflow: 'ellipsis', overflow: 'hidden', whiteSpace: 'nowrap' }}>
                        {agent.task || 'Click to view execution logs...'}
                      </div>
                    </div>
                  ))}
                  {agents.length === 0 && (
                    <div style={{ color: 'var(--text-muted)', fontStyle: 'italic', textAlign: 'center', marginTop: '10px', marginBottom: '10px', fontSize: '12px' }}>
                      No active agent execution in this session.
                    </div>
                  )}
                </div>

                {/* Provider Health Panel */}
                <div className="provider-health-panel" style={{ marginTop: '12px', borderTop: '1px solid var(--border-muted)', paddingTop: '16px' }}>
                  <div className="provider-health-header" style={{ display: 'flex', justifyContent: 'space-between', alignItems: 'center', marginBottom: '12px' }}>
                    <div>
                      <div className="provider-health-title" style={{ fontWeight: 'bold', fontSize: '12px' }}>Provider Health</div>
                      <div className="provider-health-subtitle" style={{ fontSize: '10px', color: 'var(--text-muted)' }}>
                        {providerStatuses[0]?.checked_at
                          ? `Last probe ${new Date(providerStatuses[0].checked_at).toLocaleTimeString()}`
                          : 'CLI capability and HYARD surface probe'}
                      </div>
                    </div>
                    <button
                      className="provider-refresh-btn"
                      onClick={refreshProviderStatuses}
                      disabled={providerStatusLoading}
                      title="Refresh provider CLI probes"
                      style={{
                        background: 'transparent',
                        border: 'none',
                        color: 'var(--text-muted)',
                        cursor: 'pointer',
                        display: 'flex',
                        alignItems: 'center',
                        justifyContent: 'center',
                      }}
                    >
                      {providerStatusLoading ? <span className="spinner-small" /> : <RefreshCw size={13} />}
                    </button>
                  </div>

                  {providerStatusError && (
                    <div className="provider-health-error" style={{ color: 'var(--color-error)', fontSize: '11px', marginBottom: '8px' }}>
                      {providerStatusError}
                    </div>
                  )}

                  {providerStatuses.length === 0 && !providerStatusError ? (
                    <div className="provider-health-empty" style={{ fontSize: '11px', color: 'var(--text-muted)', fontStyle: 'italic' }}>
                      {providerStatusLoading ? 'Probing providers...' : 'No provider probe data yet.'}
                    </div>
                  ) : (
                    <div className="provider-health-list" style={{ display: 'flex', flexDirection: 'column', gap: '8px' }}>
                      {providerStatuses.map((status) => {
                        const surfaceReady = Boolean(
                          status.host_surface?.installed &&
                          status.host_surface?.configured &&
                          status.host_surface?.discoverable
                        );
                        const commandLine = status.command
                          ? [status.command, ...(status.args || [])].join(' ')
                          : 'built-in provider fallback';
                        const visibleCapabilities = status.capabilities.slice(0, 2);
                        const hiddenCapabilityCount = Math.max(0, status.capabilities.length - visibleCapabilities.length);

                        return (
                          <div 
                            key={status.provider_id} 
                            className={`provider-card ${status.available ? 'provider-online' : 'provider-offline'}`}
                            style={{
                              padding: '8px 10px',
                              borderRadius: '4px',
                              background: 'rgba(255, 255, 255, 0.01)',
                              border: '1px solid var(--border-muted)',
                            }}
                          >
                            <div className="provider-card-top" style={{ display: 'flex', justifyContent: 'space-between', alignItems: 'center', marginBottom: '4px' }}>
                              <div className="provider-name-row" style={{ display: 'flex', gap: '4px', alignItems: 'center' }}>
                                <span className="provider-name" style={{ fontWeight: 'bold', fontSize: '11px' }}>{status.provider_id}</span>
                                {status.is_default_core && <span className="provider-chip chip-core" style={{ fontSize: '9px', padding: '1px 4px', borderRadius: '3px', background: 'rgba(59, 130, 246, 0.1)', color: 'var(--color-primary)' }}>core</span>}
                                {status.is_default_peer && <span className="provider-chip chip-peer" style={{ fontSize: '9px', padding: '1px 4px', borderRadius: '3px', background: 'rgba(245, 158, 11, 0.1)', color: 'var(--color-warning)' }}>peer</span>}
                              </div>
                              <span className={`status-badge ${status.available ? 'status-completed' : 'status-failed'}`} style={{ fontSize: '9px' }}>
                                {status.available ? 'ready' : 'offline'}
                              </span>
                            </div>

                            <div className="provider-command-line" style={{ fontSize: '10px', fontFamily: 'monospace', color: 'var(--text-muted)', overflow: 'hidden', textOverflow: 'ellipsis', whiteSpace: 'nowrap', marginBottom: '4px' }} title={commandLine}>
                              {commandLine}
                            </div>

                            <div className="provider-meta-row" style={{ display: 'flex', gap: '8px', fontSize: '10px', color: 'var(--text-muted)', marginBottom: '4px' }}>
                              <span>{status.backend || 'auto'}</span>
                              <span>{status.version || 'v?'}</span>
                            </div>

                            <div className="provider-chip-row" style={{ display: 'flex', gap: '4px', flexWrap: 'wrap' }}>
                              {status.roles.map((role) => (
                                <span key={role} className="provider-chip chip-role" style={{ fontSize: '9px', padding: '1px 4px', borderRadius: '3px', background: 'rgba(255, 255, 255, 0.05)', color: 'var(--text-secondary)' }}>{role}</span>
                              ))}
                              {visibleCapabilities.map((cap) => (
                                <span key={cap} className="provider-chip chip-capability" style={{ fontSize: '9px', padding: '1px 4px', borderRadius: '3px', background: 'rgba(255, 255, 255, 0.03)', color: 'var(--text-muted)' }}>{cap}</span>
                              ))}
                              {status.host_surface && (
                                <span className={`provider-chip ${surfaceReady ? 'chip-capability' : 'chip-muted'}`} style={{ fontSize: '9px', padding: '1px 4px', borderRadius: '3px' }}>
                                  {surfaceReady ? 'hyard: ok' : 'hyard: fail'}
                                </span>
                              )}
                              {hiddenCapabilityCount > 0 && (
                                <span className="provider-chip chip-muted" style={{ fontSize: '9px', color: 'var(--text-muted)' }}>+{hiddenCapabilityCount}</span>
                              )}
                            </div>
                            
                            {status.backend === 'codex' && (
                              <div style={{
                                marginTop: '8px',
                                borderTop: '1px dashed rgba(255, 255, 255, 0.05)',
                                paddingTop: '8px',
                                display: 'flex',
                                justifyContent: 'space-between',
                                alignItems: 'center'
                              }}>
                                <div style={{ display: 'flex', alignItems: 'center', gap: '6px' }}>
                                  <span style={{
                                    width: '6px',
                                    height: '6px',
                                    borderRadius: '50%',
                                    backgroundColor: activePersistentInstances.includes(status.provider_id) ? 'var(--color-success)' : 'var(--text-muted)',
                                    boxShadow: activePersistentInstances.includes(status.provider_id) ? '0 0 8px var(--color-success)' : 'none'
                                  }} />
                                  <span style={{ fontSize: '10px', color: activePersistentInstances.includes(status.provider_id) ? 'var(--color-success)' : 'var(--text-secondary)' }}>
                                    {activePersistentInstances.includes(status.provider_id) ? 'Connected (Idle)' : 'Persistent Standby'}
                                  </span>
                                </div>
                                <button
                                  onClick={() => handleTogglePersistent(status.provider_id, activePersistentInstances.includes(status.provider_id))}
                                  disabled={actionLoading[status.provider_id] || !status.available}
                                  style={{
                                    padding: '2px 8px',
                                    fontSize: '9px',
                                    fontWeight: '600',
                                    borderRadius: '3px',
                                    border: '1px solid var(--border-muted)',
                                    cursor: 'pointer',
                                    background: activePersistentInstances.includes(status.provider_id) ? 'rgba(244, 63, 94, 0.1)' : 'rgba(59, 130, 246, 0.1)',
                                    color: activePersistentInstances.includes(status.provider_id) ? '#f43f5e' : 'var(--color-primary)',
                                    borderColor: activePersistentInstances.includes(status.provider_id) ? '#f43f5e' : 'var(--color-primary)',
                                    opacity: status.available ? 1 : 0.5,
                                    transition: 'all 0.2s'
                                  }}
                                >
                                  {actionLoading[status.provider_id] ? '...' : activePersistentInstances.includes(status.provider_id) ? 'Disconnect' : 'Connect'}
                                </button>
                              </div>
                            )}
                          </div>
                        );
                      })}
                    </div>
                  )}
                </div>
              </div>
            )}
          </div>
        )}

        {/* Checklist Tab */}
        {activeTab === 'checklist' && (
          <div style={{ display: 'flex', flexDirection: 'column', flex: 1, overflow: 'hidden', padding: '16px' }}>
            <div style={{ display: 'flex', gap: '8px', marginBottom: '16px' }}>
              <input
                type="text"
                placeholder="Add new task..."
                id="new-checklist-item-input"
                className="settings-input"
                style={{ flex: 1, height: '36px' }}
                onKeyDown={(e) => {
                  if (e.key === 'Enter') {
                    const val = (e.target as HTMLInputElement).value;
                    if (val.trim()) {
                      handleAddChecklistItem(val);
                      (e.target as HTMLInputElement).value = '';
                    }
                  }
                }}
              />
              <button
                className="btn-new-session"
                onClick={() => {
                  const inputEl = document.getElementById('new-checklist-item-input') as HTMLInputElement;
                  if (inputEl && inputEl.value.trim()) {
                    handleAddChecklistItem(inputEl.value);
                    inputEl.value = '';
                  }
                }}
                style={{ height: '36px', padding: '0 16px', display: 'flex', alignItems: 'center', gap: '4px' }}
              >
                Add
              </button>
            </div>

            <div style={{ flex: 1, overflowY: 'auto', display: 'flex', flexDirection: 'column', gap: '8px' }}>
              {checklistItems.length === 0 ? (
                <div style={{ display: 'flex', flexDirection: 'column', alignItems: 'center', justifyContent: 'center', height: '100px', color: 'var(--text-muted)' }}>
                  <ClipboardList size={24} style={{ marginBottom: '8px' }} />
                  <span style={{ fontSize: '12px' }}>No items in the todo list</span>
                </div>
              ) : (
                checklistItems.map((item) => (
                  <div
                    key={item.id}
                    className="glass-panel"
                    style={{
                      display: 'flex',
                      alignItems: 'center',
                      justifyContent: 'space-between',
                      padding: '10px 12px',
                      background: item.completed ? 'rgba(255, 255, 255, 0.01)' : 'rgba(255, 255, 255, 0.03)',
                      borderColor: item.completed ? 'rgba(255, 255, 255, 0.05)' : 'var(--border-muted)',
                      opacity: item.completed ? 0.6 : 1,
                      transition: 'all 0.2s',
                    }}
                  >
                    <div style={{ display: 'flex', alignItems: 'center', gap: '10px', flex: 1, minWidth: 0 }}>
                      <input
                        type="checkbox"
                        checked={item.completed}
                        onChange={() => handleToggleChecklistItem(item.id)}
                        style={{ width: '16px', height: '16px', cursor: 'pointer' }}
                      />
                      <span
                        style={{
                          fontSize: '13px',
                          textDecoration: item.completed ? 'line-through' : 'none',
                          color: item.completed ? 'var(--text-muted)' : 'var(--text-primary)',
                          textOverflow: 'ellipsis',
                          overflow: 'hidden',
                          whiteSpace: 'nowrap',
                          flex: 1
                        }}
                      >
                        {item.text}
                      </span>
                    </div>
                    <button
                      onClick={() => handleDeleteChecklistItem(item.id)}
                      style={{ background: 'none', border: 'none', cursor: 'pointer', color: 'var(--color-error)', display: 'flex', alignItems: 'center', padding: '4px' }}
                    >
                      <Trash2 size={13} />
                    </button>
                  </div>
                ))
              )}
            </div>
          </div>
        )}

        {/* Summary Tab */}
        {activeTab === 'summary' && (
          <div style={{ display: 'flex', flexDirection: 'column', flex: 1, overflow: 'hidden', padding: '16px' }}>
            <div style={{ display: 'flex', justifyContent: 'space-between', alignItems: 'center', marginBottom: '16px' }}>
              <h3 style={{ fontSize: '14px', fontWeight: 600, color: 'var(--text-primary)' }}>Session Summary</h3>
              {!editingSummary && selectedSession && (
                <button
                  onClick={() => setEditingSummary(true)}
                  className="btn-new-session"
                  style={{ padding: '4px 10px', height: '28px', display: 'flex', alignItems: 'center', gap: '4px', fontSize: '11px' }}
                >
                  <Edit size={12} />
                  Edit
                </button>
              )}
            </div>

            {editingSummary ? (
              <div style={{ display: 'flex', flexDirection: 'column', flex: 1, gap: '12px' }}>
                <textarea
                  value={summaryText}
                  onChange={(e) => setSummaryText(e.target.value)}
                  className="settings-input"
                  style={{ flex: 1, minHeight: '150px', padding: '12px', fontSize: '13px', lineHeight: '1.5', fontFamily: 'var(--font-sans)', resize: 'none' }}
                  placeholder="Summarize this coding session..."
                />
                <div style={{ display: 'flex', gap: '8px', justifyContent: 'flex-end' }}>
                  <button
                    onClick={() => setEditingSummary(false)}
                    style={{
                      padding: '6px 14px',
                      borderRadius: '4px',
                      background: 'rgba(255,255,255,0.05)',
                      border: '1px solid var(--border-muted)',
                      color: 'var(--text-secondary)',
                      fontSize: '12px',
                      cursor: 'pointer'
                    }}
                  >
                    Cancel
                  </button>
                  <button
                    onClick={() => {
                      if (selectedSession) {
                        onUpdateSessionSummary(selectedSession.session_id, summaryText.trim() || null);
                      }
                      setEditingSummary(false);
                    }}
                    style={{
                      padding: '6px 14px',
                      borderRadius: '4px',
                      background: 'var(--color-primary)',
                      border: 'none',
                      color: '#fff',
                      fontSize: '12px',
                      cursor: 'pointer'
                    }}
                  >
                    Save
                  </button>
                </div>
              </div>
            ) : (
              <div style={{ flex: 1, overflowY: 'auto' }}>
                {selectedSession?.summary ? (
                  <div
                    style={{
                      fontSize: '13px',
                      lineHeight: '1.6',
                      color: 'var(--text-primary)',
                      whiteSpace: 'pre-wrap',
                      background: 'rgba(255,255,255,0.01)',
                      border: '1px solid var(--border-muted)',
                      borderRadius: '4px',
                      padding: '12px'
                    }}
                  >
                    {selectedSession.summary}
                  </div>
                ) : (
                  <div style={{ display: 'flex', flexDirection: 'column', alignItems: 'center', justifyContent: 'center', height: '150px', color: 'var(--text-muted)', border: '1px dashed var(--border-muted)', borderRadius: '4px' }}>
                    <FileText size={24} style={{ marginBottom: '8px' }} />
                    <span style={{ fontSize: '12px', marginBottom: '12px' }}>No session summary has been recorded.</span>
                    {selectedSession && (
                      <button
                        onClick={() => setEditingSummary(true)}
                        className="btn-new-session"
                        style={{ padding: '6px 12px', height: '32px', fontSize: '12px' }}
                      >
                        Create Summary
                      </button>
                    )}
                  </div>
                )}
              </div>
            )}
          </div>
        )}

        {/* 3. Searchable Telemetry logs */}
        {activeTab === 'telemetry' && (
          <div style={{ display: 'flex', flexDirection: 'column', flex: 1, overflow: 'hidden' }}>
            
            {/* Search and filter controls */}
            <div style={{ padding: '8px 12px', borderBottom: '1px solid var(--border-muted)', background: 'rgba(0,0,0,0.1)', display: 'flex', flexDirection: 'column', gap: '8px' }}>
              <div style={{ position: 'relative', display: 'flex', alignItems: 'center' }}>
                <Search size={12} style={{ position: 'absolute', left: '8px', color: 'var(--text-muted)' }} />
                <input 
                  type="text" 
                  placeholder="Search logs..." 
                  value={logSearch}
                  onChange={(e) => setLogSearch(e.target.value)}
                  style={{
                    width: '100%',
                    background: 'rgba(0, 0, 0, 0.25)',
                    border: '1px solid var(--border-muted)',
                    borderRadius: '4px',
                    padding: '4px 8px 4px 26px',
                    fontSize: '11px',
                    color: 'var(--text-primary)',
                    outline: 'none',
                  }}
                />
              </div>
              
              {/* Tag selectors */}
              <div style={{ display: 'flex', gap: '4px', flexWrap: 'wrap' }}>
                {(['all', 'core', 'peer', 'sys', 'info'] as const).map((tag) => (
                  <button 
                    key={tag}
                    onClick={() => setSelectedLogTag(tag)}
                    style={{
                      padding: '2px 8px',
                      fontSize: '9px',
                      borderRadius: '3px',
                      border: 'none',
                      cursor: 'pointer',
                      background: selectedLogTag === tag ? 'var(--color-primary)' : 'rgba(255, 255, 255, 0.05)',
                      color: selectedLogTag === tag ? '#fff' : 'var(--text-secondary)',
                      textTransform: 'uppercase',
                      fontWeight: 'bold',
                      transition: 'all 0.15s',
                    }}
                  >
                    {tag}
                  </button>
                ))}
              </div>
            </div>

            {/* Logs feed */}
            <div className="telemetry-feed" style={{ flex: 1, padding: '12px', overflowY: 'auto', fontFamily: 'monospace', fontSize: '11px' }}>
              {filteredLogs.length === 0 ? (
                <span style={{ color: 'var(--text-muted)', fontStyle: 'italic' }}>
                  {telemetryLogs.length === 0 ? 'No logs in this session' : 'No logs match search criteria'}
                </span>
              ) : (
                filteredLogs.map((log, index) => (
                  <div key={index} className={`telemetry-log-item log-${log.tag}`} style={{ marginBottom: '4px', lineHeight: '1.4' }}>
                    <span style={{ color: 'var(--text-muted)' }}>[{log.timestamp}]</span>{' '}
                    <span className={`log-tag-badge tag-${log.tag}`} style={{ textTransform: 'uppercase', fontWeight: 'bold', marginRight: '4px', fontSize: '9px' }}>
                      {log.tag}
                    </span>{' '}
                    <span>{log.message}</span>
                  </div>
                ))
              )}
              <div ref={telemetryEndRef} />
            </div>
          </div>
        )}
      </div>
    </div>
  );
};
export default ControlCenter;
