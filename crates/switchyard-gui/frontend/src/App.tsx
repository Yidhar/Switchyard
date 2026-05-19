import React, { useState, useEffect, useRef } from 'react';
import { invoke } from '@tauri-apps/api/core';
import { listen } from '@tauri-apps/api/event';
import { 
  Send, 
  Settings, 
  Plus, 
  MessageSquare, 
  Terminal, 
  Layers, 
  X, 
  RefreshCw,
  Trash
} from 'lucide-react';

// Interfaces for Switchyard Config
interface CoreConfig {
  default_provider: string;
  default_peers: string[];
}

interface ProviderConfig {
  command: string;
  args: string[];
  env: Record<string, string>;
  timeout_secs: number;
  backend: string | null;
}

interface StoreConfig {
  backend: 'jsonl' | 'sqlite';
  path: string;
}

interface SwitchyardConfig {
  core: CoreConfig;
  providers: Record<string, ProviderConfig>;
  store: StoreConfig;
}

interface Session {
  session_id: string;
  created_at: string;
  updated_at: string;
  active_core: string;
  enabled_peers: string[];
  mode: string;
  summary: string | null;
}

interface Turn {
  turn_id: string;
  session_id: string;
  origin: 'user' | 'delegate' | 'system';
  provider: string;
  role: 'core' | 'worker' | 'reviewer' | 'analyst';
  user_message: string;
  provider_response: string | null;
  error_message: string | null;
  status: 'pending' | 'running' | 'completed' | 'failed' | 'cancelled';
  started_at: string;
  completed_at: string | null;
  delegated_by: string | null;
}

interface TelemetryLog {
  timestamp: string;
  tag: 'core' | 'peer' | 'sys' | 'info';
  message: string;
}

function renderMessageBody(text: string | null, style?: React.CSSProperties) {
  if (!text) return null;

  // Split by code blocks first
  const parts = text.split(/(```[\s\S]*?```)/g);

  return (
    <div className="message-body" style={style}>
      {parts.map((part, idx) => {
        if (part.startsWith('```') && part.endsWith('```')) {
          // It's a code block
          const match = part.match(/^```(\w*)\n?([\s\S]*?)```$/);
          const language = match ? match[1] : '';
          const codeContent = match ? match[2] : part.slice(3, -3);
          
          return (
            <div key={idx} className="code-block-container" style={{ margin: '8px 0', position: 'relative' }}>
              {language && (
                <div style={{
                  position: 'absolute',
                  top: '8px',
                  right: '12px',
                  fontSize: '11px',
                  color: 'var(--text-muted)',
                  textTransform: 'uppercase',
                  fontWeight: 'bold',
                  letterSpacing: '0.5px',
                  userSelect: 'none'
                }}>
                  {language}
                </div>
              )}
              <pre style={{ margin: 0 }}>
                <code>{codeContent.trim()}</code>
              </pre>
            </div>
          );
        } else {
          // Regular text block. Render with inline formatting (inline code, bold)
          const inlineParts = part.split(/(`[^`\n]+`)/g);
          return (
            <span key={idx}>
              {inlineParts.map((subPart, subIdx) => {
                if (subPart.startsWith('`') && subPart.endsWith('`')) {
                  return (
                    <code key={subIdx}>
                      {subPart.slice(1, -1)}
                    </code>
                  );
                }

                // Check for bold parts
                const boldParts = subPart.split(/(\*\*[^*]+\*\*)/g);
                return boldParts.map((boldPart, boldIdx) => {
                  if (boldPart.startsWith('**') && boldPart.endsWith('**')) {
                    return (
                      <strong key={boldIdx}>
                        {boldPart.slice(2, -2)}
                      </strong>
                    );
                  }
                  return boldPart;
                });
              })}
            </span>
          );
        }
      })}
    </div>
  );
}

function getStatusColor(status: string) {
  if (status === 'completed' || status === 'success') return '#10b981';
  if (status === 'failed' || status === 'cancelled') return '#ef4444';
  return '#f59e0b';
}

function renderTurnEvents(turnId: string, events: any[], turns: Turn[], realtimeLines?: string[]) {
  // Filter events for this turn
  const turnEvents = events.filter((e) => e.turn_id === turnId);
  
  // Find execution telemetry event
  const telemetryEvent = turnEvents.find(
    (e) => e.event_type === 'item_updated' && e.payload?.item_type === 'execution_telemetry'
  );
  const commandLine = telemetryEvent?.payload?.execution?.command;
  const commandArgs = telemetryEvent?.payload?.execution?.args;
  
  // Gather terminal outputs from db events
  const dbTerminalLines = turnEvents
    .filter((e) => e.event_type === 'item_updated' && e.payload?.item_type === 'terminal_output')
    .map((e) => e.payload?.line)
    .filter(Boolean);

  const combinedTerminal = [...dbTerminalLines, ...(realtimeLines || [])];

  // Extract tool calls and delegate sub-agents
  const toolCalls: any[] = [];

  // 1. Gather child delegate turns
  const delegates = turns.filter((t) => t.delegated_by === turnId);
  delegates.forEach((d) => {
    toolCalls.push({
      id: d.turn_id,
      name: `Delegation to ${d.provider} (${d.role})`,
      input: d.user_message,
      status: d.status,
      output: d.provider_response || d.error_message,
    });
  });

  // 2. Gather standard tool use and result events
  turnEvents.forEach((e) => {
    if (e.event_type === 'item_updated') {
      const payload = e.payload;
      if (payload && payload.type === 'tool_use') {
        const name = payload.name;
        if (name && name.trim()) {
          toolCalls.push({
            id: payload.id || Math.random().toString(),
            name: name,
            input: payload.input,
            status: 'running',
            output: null,
          });
        }
      } else if (payload && payload.type === 'tool_result') {
        const name = payload.name;
        const existing = toolCalls.find((tc) => tc.id === payload.id || (name && tc.name === name && tc.status === 'running'));
        if (existing) {
          existing.status = 'completed';
          existing.output = payload.output;
        } else if (name && name.trim()) {
          toolCalls.push({
            id: payload.id || Math.random().toString(),
            name: name,
            status: 'completed',
            output: payload.output,
          });
        }
      }
    }
  });

  if (!commandLine && toolCalls.length === 0 && combinedTerminal.length === 0) {
    return null;
  }

  return (
    <div className="execution-details-accordion" style={{ marginTop: '10px', fontSize: '12px' }}>
      <details style={{ background: 'rgba(0,0,0,0.15)', borderRadius: '6px', border: '1px solid var(--border-muted)', overflow: 'hidden' }}>
        <summary style={{ padding: '8px 12px', cursor: 'pointer', fontWeight: 'bold', display: 'flex', alignItems: 'center', gap: '6px', userSelect: 'none', background: 'rgba(255,255,255,0.02)' }}>
          <Terminal size={14} style={{ color: 'var(--color-secondary)' }} />
          <span>Execution Details & Logs</span>
          {toolCalls.length > 0 && (
            <span style={{ fontSize: '11px', padding: '1px 5px', background: 'rgba(6, 182, 212, 0.1)', color: 'var(--color-secondary)', borderRadius: '10px', marginLeft: 'auto' }}>
              {toolCalls.length} Tool{toolCalls.length > 1 ? 's' : ''}
            </span>
          )}
        </summary>
        <div style={{ padding: '12px', display: 'flex', flexDirection: 'column', gap: '10px', borderTop: '1px solid var(--border-muted)' }}>
          {commandLine && (
            <div>
              <div style={{ fontWeight: 'bold', color: 'var(--text-secondary)', marginBottom: '4px' }}>Subprocess Command:</div>
              <code style={{ display: 'block', padding: '6px 10px', background: 'rgba(0, 0, 0, 0.3)', borderRadius: '4px', wordBreak: 'break-all', fontFamily: 'monospace' }}>
                {commandLine} {commandArgs ? commandArgs.join(' ') : ''}
              </code>
            </div>
          )}

          {toolCalls.length > 0 && (
            <div>
              <div style={{ fontWeight: 'bold', color: 'var(--text-secondary)', marginBottom: '6px' }}>Tool Executions:</div>
              <div style={{ display: 'flex', flexDirection: 'column', gap: '6px' }}>
                {toolCalls.map((tc, idx) => {
                  const hasInput = tc.input && (
                    typeof tc.input === 'string' 
                      ? tc.input.trim().length > 0 
                      : Object.keys(tc.input).length > 0
                  );
                  const hasOutput = tc.output && (
                    typeof tc.output === 'string' 
                      ? tc.output.trim().length > 0 
                      : Object.keys(tc.output).length > 0
                  );
                  return (
                    <div key={idx} style={{ padding: '8px', background: 'rgba(0,0,0,0.2)', borderLeft: `3px solid ${getStatusColor(tc.status)}`, borderRadius: '4px' }}>
                      <div style={{ display: 'flex', justifyContent: 'space-between', fontWeight: 'bold', marginBottom: '4px' }}>
                        <span style={{ color: 'var(--text-primary)' }}>{tc.name}</span>
                        <span style={{ color: getStatusColor(tc.status), fontSize: '11px', textTransform: 'capitalize' }}>{tc.status}</span>
                      </div>
                      {hasInput && (
                        <details style={{ marginTop: '4px' }}>
                          <summary style={{ cursor: 'pointer', color: 'var(--text-muted)', fontSize: '11px', outline: 'none', userSelect: 'none' }}>
                            Show Input / Parameters
                          </summary>
                          <pre style={{ margin: '4px 0 0 0', fontSize: '11px', color: 'var(--text-muted)', overflowX: 'auto', background: 'rgba(0,0,0,0.1)', padding: '6px', borderRadius: '4px' }}>
                            {typeof tc.input === 'string' ? tc.input : JSON.stringify(tc.input, null, 2)}
                          </pre>
                        </details>
                      )}
                      {hasOutput && (
                        <details style={{ marginTop: '4px' }}>
                          <summary style={{ cursor: 'pointer', color: 'var(--text-secondary)', fontSize: '11px', outline: 'none', userSelect: 'none' }}>
                            Show Output / Result
                          </summary>
                          <pre style={{ margin: '4px 0 0 0', fontSize: '11px', color: 'var(--text-secondary)', overflowX: 'auto', background: 'rgba(0,0,0,0.15)', padding: '6px', maxHeight: '200px', overflowY: 'auto', borderRadius: '4px' }}>
                            {typeof tc.output === 'string' ? tc.output : JSON.stringify(tc.output, null, 2)}
                          </pre>
                        </details>
                      )}
                    </div>
                  );
                })}
              </div>
            </div>
          )}

          {combinedTerminal.length > 0 && (
            <div>
              <div style={{ fontWeight: 'bold', color: 'var(--text-secondary)', marginBottom: '4px' }}>Subprocess Logs & Console Output:</div>
              <pre style={{ margin: 0, padding: '8px 12px', background: '#0c0f1d', borderRadius: '4px', color: '#38bdf8', fontFamily: 'monospace', fontSize: '11px', maxHeight: '180px', overflowY: 'auto', border: '1px solid #1e293b', whiteSpace: 'pre-wrap', wordBreak: 'break-all' }}>
                {combinedTerminal.join('\n')}
              </pre>
            </div>
          )}
        </div>
      </details>
    </div>
  );
}

function App() {
  // App State
  const [sessions, setSessions] = useState<Session[]>([]);
  const [selectedSession, setSelectedSession] = useState<Session | null>(null);
  const [turns, setTurns] = useState<Turn[]>([]);
  const [inputText, setInputText] = useState('');
  const [isGenerating, setIsGenerating] = useState(false);
  
  // Streaming state during active run
  const [activeCoreText, setActiveCoreText] = useState('');
  const [activePeerText, setActivePeerText] = useState('');
  const [activePeerName, setActivePeerName] = useState<string | null>(null);
  const [activeNodes, setActiveNodes] = useState<string[]>([]); // Array of active nodes (e.g. ['host', 'codex', 'claude'])
  const [activeTurnIds, setActiveTurnIds] = useState<string[]>([]); // Array of active turn IDs
  const [telemetryLogs, setTelemetryLogs] = useState<TelemetryLog[]>([]);
  const [sessionEvents, setSessionEvents] = useState<any[]>([]);
  const [realtimeTerminalLines, setRealtimeTerminalLines] = useState<Record<string, string[]>>({});
  const [activeCoreTurnId, setActiveCoreTurnId] = useState<string | null>(null);
  const [activePeerTurnId, setActivePeerTurnId] = useState<string | null>(null);

  // Settings State
  const [config, setConfig] = useState<SwitchyardConfig | null>(null);
  const [showSettings, setShowSettings] = useState(false);
  const [settingsTab, setSettingsTab] = useState<string>('general');

  // New Session Creator State
  const [newSessionProvider, setNewSessionProvider] = useState('codex');

  // Refs for autoscrolling
  const messagesEndRef = useRef<HTMLDivElement>(null);
  const telemetryEndRef = useRef<HTMLDivElement>(null);

  // Auto-scroll messages
  useEffect(() => {
    messagesEndRef.current?.scrollIntoView({ behavior: 'smooth' });
  }, [turns, activeCoreText, activePeerText]);

  // Auto-scroll telemetry
  useEffect(() => {
    telemetryEndRef.current?.scrollIntoView({ behavior: 'smooth' });
  }, [telemetryLogs]);

  // Initial loading
  useEffect(() => {
    loadSessions();
    loadAppConfig();
  }, []);

  // Ref to always capture the latest selectedSession inside the singleton listener
  const selectedSessionRef = useRef<Session | null>(null);
  useEffect(() => {
    selectedSessionRef.current = selectedSession;
  }, [selectedSession]);

  // Listen for Tauri events
  useEffect(() => {
    let unlisten: (() => void) | null = null;

    const setupListener = async () => {
      console.log('Setting up Tauri event listener for runtime_event...');
      const refreshTurns = async () => {
        const session = selectedSessionRef.current;
        console.log('refreshTurns called, current session:', session?.session_id);
        if (!session) return;
        try {
          const turnList = await invoke<Turn[]>('get_session_turns', { sessionId: session.session_id });
          console.log(`Loaded ${turnList.length} turns for session ${session.session_id}`);
          setTurns(turnList);
          const eventList = await invoke<any[]>('get_session_events', { sessionId: session.session_id });
          setSessionEvents(eventList);
        } catch (e) {
          console.error('Error fetching session turns/events:', e);
        }
      };

      try {
        unlisten = await listen<any>('runtime_event', (event) => {
          console.log('Received runtime_event event:', event);
          const payload = event.payload;
          const type = payload.event;
          const data = payload.data;
          const now = new Date().toLocaleTimeString();
          console.log(`Event type: ${type}, data:`, data);
  
        switch (type) {
          case 'CoreTurnStarted':
            setActiveNodes(['host', data.provider]);
            setActiveTurnIds([]);
            setActiveCoreTurnId(data.turn_id);
            setRealtimeTerminalLines((prev) => ({ ...prev, [data.turn_id]: [] }));
            addLog(now, 'core', `Core turn started on [${data.provider}] (ID: ${data.turn_id})`);
            refreshTurns();
            break;
          
          case 'CoreItemUpdated':
            setActiveCoreText((prev) => prev + data.text);
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
            setActivePeerText((prev) => prev + data.text);
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

          case 'PeerOutputCompleted':
            setActiveTurnIds((prev) => prev.filter((id) => id !== data.turn_id));
            refreshTurns();
            break;

          case 'CoreExecutionTelemetry':
            addLog(now, 'info', `Core Telemetry: Elapsed ${data.execution.elapsed_ms}ms, Status: ${data.execution.status_code}`);
            break;

          case 'PeerExecutionTelemetry':
            addLog(now, 'info', `Peer Telemetry: Elapsed ${data.execution.elapsed_ms}ms, Status: ${data.execution.status_code}`);
            break;

          case 'CoreTerminalOutput':
            if (data.text) {
              const lines = data.text.split('\n');
              for (const line of lines) {
                const trimmed = line.trim();
                if (trimmed) {
                  addLog(now, 'core', `[Subprocess Out]: ${trimmed}`);
                }
              }
              if (data.turn_id) {
                setRealtimeTerminalLines((prev) => {
                  const arr = prev[data.turn_id] || [];
                  return { ...prev, [data.turn_id]: [...arr, ...lines] };
                });
              }
            }
            break;

          case 'PeerTerminalOutput':
            if (data.text) {
              const lines = data.text.split('\n');
              for (const line of lines) {
                const trimmed = line.trim();
                if (trimmed) {
                  addLog(now, 'peer', `[Peer Subprocess - ${data.provider}]: ${trimmed}`);
                }
              }
              if (data.turn_id) {
                setRealtimeTerminalLines((prev) => {
                  const arr = prev[data.turn_id] || [];
                  return { ...prev, [data.turn_id]: [...arr, ...lines] };
                });
              }
            }
            break;

          case 'CallbackReceiptsInjected':
            addLog(now, 'sys', `Injected ${data.count} unread callback receipts for provider [${data.provider}]`);
            refreshTurns();
            break;

          case 'FinalizationStarted':
            setActiveNodes(['host', data.provider]);
            setActiveTurnIds([]);
            addLog(now, 'core', `Finalization phase started on [${data.provider}] (ID: ${data.turn_id})`);
            refreshTurns();
            break;

          case 'TurnCompleted':
            setActiveNodes([]);
            setActiveTurnIds([]);
            addLog(now, 'sys', `Routed turn completed successfully.`);
            refreshTurns();
            break;

          case 'TurnFailed':
            setActiveNodes([]);
            setActiveTurnIds([]);
            addLog(now, 'sys', `Turn failed: ${data.error}`);
            refreshTurns();
            break;
        }
      });
      } catch (err) {
        console.error('Error setting up Tauri event listener:', err);
      }
    };

    setupListener();

    return () => {
      if (unlisten) {
        unlisten();
      }
    };
  }, []);

  const addLog = (time: string, tag: 'core' | 'peer' | 'sys' | 'info', message: string) => {
    setTelemetryLogs((prev) => [...prev, { timestamp: time, tag, message }]);
  };

  // API wrappers
  const loadSessions = async () => {
    try {
      const res = await invoke<Session[]>('list_sessions');
      setSessions(res);
      if (res.length > 0 && !selectedSession) {
        selectSession(res[0]);
      }
    } catch (e) {
      console.error(e);
    }
  };

  const selectSession = async (session: Session) => {
    setSelectedSession(session);
    setActiveCoreText('');
    setActivePeerText('');
    setActivePeerName(null);
    setActiveNodes([]);
    setActiveTurnIds([]);
    setRealtimeTerminalLines({});
    try {
      const turnList = await invoke<Turn[]>('get_session_turns', { sessionId: session.session_id });
      setTurns(turnList);
      const eventList = await invoke<any[]>('get_session_events', { sessionId: session.session_id });
      setSessionEvents(eventList);
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

  const loadAppConfig = async () => {
    try {
      const cfg = await invoke<SwitchyardConfig>('load_config');
      setConfig(cfg);
      if (cfg && cfg.core && cfg.core.default_provider) {
        setNewSessionProvider(cfg.core.default_provider);
      }
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
    } catch (e) {
      alert('Failed to save config: ' + e);
    }
  };

  const handleSend = async () => {
    if (!inputText.trim() || !selectedSession || isGenerating) return;

    setInputText('');
    setIsGenerating(true);
    setActiveCoreText('');
    setActivePeerText('');
    setActivePeerName(null);
    setActiveNodes(['host']);
    setActiveTurnIds([]);
    setTelemetryLogs([]);

    // Add visual temp turn/message instantly for reactive feel
    const tempUserTurn: Turn = {
      turn_id: 'temp-user-id',
      session_id: selectedSession.session_id,
      origin: 'user',
      provider: 'user',
      role: 'core',
      user_message: inputText,
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
        sessionId: selectedSession.session_id,
        message: inputText,
        provider: selectedSession.active_core
      });
    } catch (e) {
      addLog(new Date().toLocaleTimeString(), 'sys', `Execution failed: ${e}`);
    } finally {
      setIsGenerating(false);
      setActiveNodes([]);
      setActivePeerName(null);
      setActiveTurnIds([]);
      setActiveCoreTurnId(null);
      setActivePeerTurnId(null);
      // Reload turns database state
      const updatedTurns = await invoke<Turn[]>('get_session_turns', { sessionId: selectedSession.session_id });
      setTurns(updatedTurns);
      // Reload sessions list to refresh updated times
      loadSessions();
      // Reload session events
      try {
        const eventList = await invoke<any[]>('get_session_events', { sessionId: selectedSession.session_id });
        setSessionEvents(eventList);
      } catch (e) {
        console.error(e);
      }
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
    const backend = prompt('Choose provider backend type (codex, claude, gemini):', 'codex');
    if (backend === null) return;
    const trimmedBackend = backend.trim().toLowerCase();
    if (!['codex', 'claude', 'gemini'].includes(trimmedBackend)) {
      alert('Backend must be one of: codex, claude, gemini');
      return;
    }

    setConfig((prev) => {
      if (!prev) return null;
      const providersCopy = { ...prev.providers };
      providersCopy[trimmed] = {
        command: trimmedBackend === 'codex' ? 'codex-cli' : trimmedBackend === 'claude' ? 'claude-cli' : 'gemini-cli',
        args: ['run'],
        env: {},
        timeout_secs: 900,
        backend: trimmedBackend
      };
      return {
        ...prev,
        providers: providersCopy
      };
    });
    setSettingsTab(trimmed);
  };

  return (
    <div className="app-container">
      {/* 1. Sidebar Panel */}
      <div className="sidebar glass-panel">
        <div className="sidebar-header">
          <div className="logo-icon">S</div>
          <div className="logo-text">Switchyard</div>
        </div>

        <div className="sidebar-actions">
          <div style={{ display: 'flex', gap: '8px', marginBottom: '12px' }}>
            <select 
              className="settings-select" 
              style={{ flex: 1 }}
              value={newSessionProvider}
              onChange={(e) => setNewSessionProvider(e.target.value)}
            >
              {config && Object.keys(config.providers).map((p) => (
                <option key={p} value={p}>
                  {p}
                </option>
              ))}
            </select>
            <button className="btn-new-session" onClick={createNewSession} style={{ padding: '8px 12px' }}>
              <Plus size={16} />
              New
            </button>
          </div>
        </div>

        <div className="session-list">
          {sessions.map((s) => (
            <div 
              key={s.session_id} 
              className={`session-item glass-panel-interactive ${selectedSession?.session_id === s.session_id ? 'active' : ''}`}
              onClick={() => selectSession(s)}
            >
              <div className="session-item-title">
                <span style={{ textOverflow: 'ellipsis', overflow: 'hidden', whiteSpace: 'nowrap', maxWidth: '140px' }}>
                  {s.session_id.substring(0, 8)}...
                </span>
                <span className={`session-item-badge badge-${s.active_core}`}>
                  {s.active_core}
                </span>
              </div>
              <div className="session-item-date">
                {new Date(s.updated_at).toLocaleString()}
              </div>
            </div>
          ))}
        </div>

        <div className="sidebar-footer">
          <button className="btn-settings" onClick={() => setShowSettings(true)}>
            <Settings size={16} />
            Settings
          </button>
          <div style={{ fontSize: '11px', color: 'var(--text-muted)' }}>
            v0.1.0-tauri
          </div>
        </div>
      </div>

      {/* 2. Main Chat Panel */}
      <div className="main-content glass-panel">
        <div className="chat-header">
          <div className="chat-header-info">
            <h2>
              {selectedSession ? `Session: ${selectedSession.session_id.substring(0, 8)}` : 'No Session Selected'}
            </h2>
            <p>
              {selectedSession ? `Active Core: ${selectedSession.active_core} | Peers: ${selectedSession.enabled_peers.join(', ') || 'None'}` : 'Create a session to begin'}
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

        <div className="chat-messages">
          {turns.length === 0 && !activeCoreText ? (
            <div className="empty-chat">
              <MessageSquare size={48} className="empty-chat-logo" />
              <div>
                <h3>Start a Conversation</h3>
                <p style={{ fontSize: '13px', marginTop: '6px' }}>Send a message to run the central orchestrator and delegate tasks to peers.</p>
              </div>
            </div>
          ) : (
            turns.map((t, idx) => {
              const isSystemFeedback = t.user_message.includes('<<<SWITCHYARD_JSON_BEGIN>>>');
              const isDelegateRequest = t.provider_response?.includes('"type":"delegate"') || t.provider_response?.includes('"type": "delegate"');

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
                            borderRadius: '8px', 
                            padding: '12px',
                            fontSize: '13px',
                            color: 'var(--text-secondary)',
                            marginBottom: '16px'
                          }}
                        >
                          <div style={{ fontWeight: 'bold', display: 'flex', alignItems: 'center', gap: '6px', color: 'var(--color-primary)', marginBottom: '8px' }}>
                            <span>Aggregated Delegation Results</span>
                            <span style={{ fontSize: '11px', padding: '2px 6px', background: 'rgba(99, 102, 241, 0.1)', color: 'var(--color-primary)', borderRadius: '12px' }}>
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
                      {(t.provider_response || t.error_message) && !isDelegateRequest && (
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
                    {(t.provider_response || t.error_message) && !isDelegateRequest && (
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
                          borderRadius: '8px', 
                          padding: '12px',
                          fontSize: '13px',
                          color: 'var(--text-secondary)',
                          marginBottom: '16px'
                        }}
                      >
                        <div style={{ fontWeight: 'bold', display: 'flex', alignItems: 'center', gap: '6px', color: 'var(--color-primary)', marginBottom: '8px' }}>
                          <span>Aggregated Delegation Results</span>
                          <span style={{ fontSize: '11px', padding: '2px 6px', background: 'rgba(99, 102, 241, 0.1)', color: 'var(--color-primary)', borderRadius: '12px' }}>
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

          <div ref={messagesEndRef} />
        </div>

        <div className="chat-input-container">
          <div className="input-wrapper">
            <textarea
              className="chat-textarea"
              placeholder="Ask Switchyard orchestrator to execute routed operations..."
              value={inputText}
              onChange={(e) => setInputText(e.target.value)}
              disabled={isGenerating || !selectedSession}
              onKeyDown={(e) => {
                if (e.key === 'Enter' && !e.shiftKey) {
                  e.preventDefault();
                  handleSend();
                }
              }}
            />
            <button className="btn-send" onClick={handleSend} disabled={isGenerating || !selectedSession || !inputText.trim()}>
              <Send size={16} />
            </button>
          </div>
        </div>
      </div>

      {/* 3. Right Topology Panel & Logger */}
      <div className="topology-panel glass-panel">
        <div className="topology-header">
          <Layers size={16} style={{ color: 'var(--color-secondary)' }} />
          <h3>Star Routing Topology</h3>
        </div>

        {/* Dynamic Topology Graph */}
        <div className="topology-view">
          {(() => {
            const coreProvider = selectedSession ? selectedSession.active_core : (config?.core?.default_provider || 'codex');

            interface TopologyNode {
              id: string;
              label: string;
              provider: string;
              role?: string;
              isActive: boolean;
            }

            // 1. Build nodes for each delegate turn in the session
            const delegateNodes: TopologyNode[] = turns
              .filter(t => t.origin === 'delegate')
              .map(t => ({
                id: t.turn_id,
                label: t.provider,
                provider: t.provider,
                role: t.role,
                isActive: t.status === 'running' || t.status === 'pending' || activeTurnIds.includes(t.turn_id)
              }));

            // 2. Add nodes for active peers that are not yet in turns database list
            const activePeers = activeNodes.filter(n => n !== 'host' && n !== coreProvider);
            const activePeerNodes: TopologyNode[] = activePeers
              .filter(p => !delegateNodes.some(dn => dn.provider === p && dn.isActive))
              .map((p, idx) => ({
                id: `active-${p}-${idx}`,
                label: p,
                provider: p,
                isActive: true
              }));

            // 3. Add default peers from config that have not been represented
            const representedProviders = new Set([...delegateNodes, ...activePeerNodes].map(n => n.provider));
            const configPeers: TopologyNode[] = (config?.core?.default_peers || [])
              .filter(p => p !== coreProvider && !representedProviders.has(p))
              .map(p => ({
                id: `config-${p}`,
                label: p,
                provider: p,
                isActive: false
              }));

            const peerNodes = [...delegateNodes, ...activePeerNodes, ...configPeers];

            const getPeerY = (index: number, total: number) => {
              if (total <= 1) return 120;
              const startY = 30;
              const endY = 210;
              return startY + index * ((endY - startY) / (total - 1));
            };

            const getNodeClass = (name: string) => {
              const lower = name.toLowerCase();
              if (lower.includes('codex')) return 'node-codex';
              if (lower.includes('claude')) return 'node-claude';
              if (lower.includes('gemini')) return 'node-gemini';
              return 'node-host';
            };

            return (
              <svg width="100%" height="240" viewBox="0 0 300 240">
                {/* Connection Paths */}
                {/* Host -> Core */}
                <path 
                  d="M 40 120 L 130 120" 
                  className={`edge-path ${activeNodes.includes('host') || activeNodes.includes(coreProvider) || isGenerating ? 'edge-active' : ''}`}
                />
                {/* Core -> Peers */}
                {peerNodes.map((node, idx) => {
                  const y = getPeerY(idx, peerNodes.length);
                  return (
                    <path 
                      key={`edge-${node.id}`}
                      d={`M 130 120 L 240 ${y}`} 
                      className={`edge-path ${node.isActive ? 'edge-active' : ''}`}
                    />
                  );
                })}

                {/* Host Node */}
                <g transform="translate(40, 120)">
                  <circle 
                    r="16" 
                    className={`node-circle node-host ${activeNodes.includes('host') ? 'node-active' : ''}`} 
                  />
                  <text y="28" className="node-label">Host</text>
                </g>

                {/* Core Node */}
                <g transform="translate(130, 120)">
                  <circle 
                    r="16" 
                    className={`node-circle ${getNodeClass(coreProvider)} ${activeNodes.includes(coreProvider) || isGenerating ? 'node-active' : ''}`} 
                  />
                  <text y="28" className="node-label" style={{ textTransform: 'capitalize' }}>{coreProvider}</text>
                </g>

                {/* Peer Nodes */}
                {peerNodes.map((node, idx) => {
                  const y = getPeerY(idx, peerNodes.length);
                  return (
                    <g key={`node-${node.id}`} transform={`translate(240, ${y})`}>
                      <circle 
                        r="16" 
                        className={`node-circle ${getNodeClass(node.provider)} ${node.isActive ? 'node-active' : ''}`} 
                      />
                      <text y="25" className="node-label" style={{ textTransform: 'capitalize' }}>{node.label}</text>
                      {node.role && (
                        <text y="35" className="node-label" style={{ fontSize: '8px', opacity: 0.5, textTransform: 'uppercase', fontWeight: 'bold' }}>
                          {node.role}
                        </text>
                      )}
                    </g>
                  );
                })}
              </svg>
            );
          })()}
        </div>

        {/* Telemetry Logger */}
        <div className="telemetry-logger">
          <div className="telemetry-header">
            <span>Execution Telemetry Logger</span>
            <Terminal size={14} />
          </div>
          <div className="telemetry-feed">
            {telemetryLogs.length === 0 ? (
              <span style={{ color: 'var(--text-muted)', fontStyle: 'italic' }}>No active run session logs</span>
            ) : (
              telemetryLogs.map((log, index) => (
                <div key={index} className={`telemetry-log-item log-${log.tag}`}>
                  [{log.timestamp}] {log.message}
                </div>
              ))
            )}
            <div ref={telemetryEndRef} />
          </div>
        </div>
      </div>

      {/* 4. Settings Dialog Overlay */}
      {showSettings && config && (
        <div className="settings-overlay">
          <div className="settings-modal glass-panel">
            <div className="settings-modal-header">
              <h2>Switchyard System Configurations</h2>
              <button className="btn-close" onClick={() => setShowSettings(false)}>
                <X size={20} />
              </button>
            </div>

            <div className="settings-modal-body">
              <div className="settings-tabs" style={{ display: 'flex', flexDirection: 'column', height: '100%', justifyContent: 'space-between' }}>
                <div style={{ display: 'flex', flexDirection: 'column', gap: '6px', overflowY: 'auto', flex: 1 }}>
                  <button 
                    className={`settings-tab-btn ${settingsTab === 'general' ? 'active' : ''}`}
                    onClick={() => setSettingsTab('general')}
                  >
                    General Core
                  </button>
                  
                  {/* Dynamic tabs for configured providers */}
                  {Object.keys(config.providers).map((pName) => (
                    <button 
                      key={pName}
                      className={`settings-tab-btn ${settingsTab === pName ? 'active' : ''}`}
                      onClick={() => setSettingsTab(pName)}
                      style={{ textTransform: 'capitalize' }}
                    >
                      {pName}
                    </button>
                  ))}

                  <button 
                    className={`settings-tab-btn ${settingsTab === 'store' ? 'active' : ''}`}
                    onClick={() => setSettingsTab('store')}
                  >
                    Database Store
                  </button>
                </div>

                {/* Add Custom Provider Button */}
                <div style={{ padding: '8px', borderTop: '1px solid var(--border-muted)' }}>
                  <button 
                    className="btn-add-row" 
                    onClick={addCustomProvider} 
                    style={{ width: '100%', display: 'flex', justifyContent: 'center', gap: '6px', padding: '10px' }}
                  >
                    <Plus size={14} />
                    Add Provider
                  </button>
                </div>
              </div>

              <div className="settings-tab-content">
                {settingsTab === 'general' && (
                  <>
                    <div className="settings-form-group">
                      <label>Default Core Provider</label>
                      <select 
                        className="settings-select"
                        value={config.core.default_provider}
                        onChange={(e) => handleSettingsFieldChange('core', 'default_provider', e.target.value)}
                      >
                        {Object.keys(config.providers).map((pName) => (
                          <option key={pName} value={pName}>{pName}</option>
                        ))}
                      </select>
                    </div>

                    <div className="settings-form-group">
                      <label>Default Peers</label>
                      <div style={{ display: 'flex', flexDirection: 'column', gap: '8px', marginTop: '4px' }}>
                        {Object.keys(config.providers).map((peer) => (
                          <label key={peer} style={{ display: 'flex', alignItems: 'center', gap: '8px', textTransform: 'none', fontSize: '13px' }}>
                            <input 
                              type="checkbox"
                              checked={config.core.default_peers.includes(peer)}
                              onChange={(e) => {
                                let list = [...config.core.default_peers];
                                if (e.target.checked) {
                                  list.push(peer);
                                } else {
                                  list = list.filter((p) => p !== peer);
                                }
                                handleSettingsFieldChange('core', 'default_peers', list);
                              }}
                            />
                            {peer}
                          </label>
                        ))}
                      </div>
                    </div>
                  </>
                )}

                {Object.keys(config.providers).includes(settingsTab) && (() => {
                  const pName = settingsTab;
                  const prov = config.providers[pName] || { command: '', args: [], env: {}, timeout_secs: 900, backend: null };
                  return (
                    <>
                      <div className="settings-form-group">
                        <label>Backend Type</label>
                        <select 
                          className="settings-select"
                          value={prov.backend || ''}
                          onChange={(e) => handleProviderFieldChange(pName, 'backend', e.target.value || null)}
                        >
                          <option value="codex">Codex Factory</option>
                          <option value="claude">Claude Factory</option>
                          <option value="gemini">Gemini Factory</option>
                        </select>
                      </div>

                      <div className="settings-form-group">
                        <label>Subprocess CLI Command</label>
                        <input 
                          type="text" 
                          className="settings-input settings-input-mono"
                          value={prov.command}
                          onChange={(e) => handleProviderFieldChange(pName, 'command', e.target.value)}
                        />
                      </div>

                      <div className="settings-form-group">
                        <label>CLI Execution Arguments (comma separated)</label>
                        <input 
                          type="text" 
                          className="settings-input settings-input-mono"
                          value={prov.args.join(', ')}
                          onChange={(e) => {
                            const args = e.target.value.split(',').map((s) => s.trim()).filter((s) => s.length > 0);
                            handleProviderFieldChange(pName, 'args', args);
                          }}
                        />
                      </div>

                      <div className="settings-form-group">
                        <label>Execution Timeout (seconds)</label>
                        <input 
                          type="number" 
                          className="settings-input"
                          value={prov.timeout_secs}
                          onChange={(e) => handleProviderFieldChange(pName, 'timeout_secs', parseInt(e.target.value) || 900)}
                        />
                      </div>

                      <div className="settings-form-group">
                        <label style={{ display: 'flex', justifyContent: 'space-between', alignItems: 'center' }}>
                          <span>Environment Variables (API Keys, etc.)</span>
                          <button className="btn-add-row" onClick={() => addEnvVar(pName)} style={{ padding: '2px 8px' }}>
                            Add Key
                          </button>
                        </label>
                        <div style={{ maxHeight: '160px', overflowY: 'auto', marginTop: '6px' }}>
                          {Object.entries(prov.env || {}).map(([key, val]) => (
                            <div key={key} className="env-editor-row">
                              <input type="text" className="settings-input settings-input-mono" value={key} readOnly />
                              <input 
                                type="text" 
                                className="settings-input" 
                                value={val} 
                                onChange={(e) => {
                                  const envCopy = { ...prov.env };
                                  envCopy[key] = e.target.value;
                                  handleProviderFieldChange(pName, 'env', envCopy);
                                }}
                              />
                              <button className="btn-remove-row" onClick={() => removeEnvVar(pName, key)}>
                                <X size={14} />
                              </button>
                            </div>
                          ))}
                        </div>
                      </div>

                      <div style={{ marginTop: '24px', display: 'flex', justifyContent: 'flex-end', borderTop: '1px solid var(--border-muted)', paddingTop: '16px' }}>
                        <button 
                          className="btn-remove-row" 
                          style={{ background: 'var(--color-error)', color: 'white', display: 'flex', alignItems: 'center', gap: '6px', padding: '8px 16px', fontSize: '13px' }}
                          onClick={() => {
                            if (confirm(`Are you sure you want to delete provider "${pName}"?`)) {
                              setConfig((prev) => {
                                if (!prev) return null;
                                const providersCopy = { ...prev.providers };
                                delete providersCopy[pName];
                                
                                // Also update core and default peers to remove it
                                let defaultProvider = prev.core.default_provider;
                                if (defaultProvider === pName) {
                                  defaultProvider = Object.keys(providersCopy)[0] || '';
                                }
                                const defaultPeers = prev.core.default_peers.filter(p => p !== pName);

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
                            }
                          }}
                        >
                          <Trash size={14} />
                          Delete Provider
                        </button>
                      </div>
                    </>
                  );
                })()}

                {settingsTab === 'store' && (
                  <>
                    <div className="settings-form-group">
                      <label>Store Engine Backend</label>
                      <select 
                        className="settings-select"
                        value={config.store.backend}
                        onChange={(e) => handleSettingsFieldChange('store', 'backend', e.target.value)}
                      >
                        <option value="jsonl">JSONL (Plain Text Stream Files)</option>
                        <option value="sqlite">SQLite Database (Single Persistent File)</option>
                      </select>
                    </div>

                    <div className="settings-form-group">
                      <label>Database Storage Path</label>
                      <input 
                        type="text" 
                        className="settings-input settings-input-mono"
                        value={config.store.path}
                        onChange={(e) => handleSettingsFieldChange('store', 'path', e.target.value)}
                      />
                    </div>
                  </>
                )}
              </div>
            </div>

            <div className="settings-modal-footer">
              <button className="btn-secondary" onClick={() => setShowSettings(false)}>
                Cancel
              </button>
              <button className="btn-primary" onClick={handleSaveConfig}>
                Save & Apply Config
              </button>
            </div>
          </div>
        </div>
      )}
    </div>
  );
}

export default App;
