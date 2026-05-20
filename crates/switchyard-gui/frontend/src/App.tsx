import { useState, useEffect, useRef } from 'react';
import { invoke } from '@tauri-apps/api/core';
import { listen } from '@tauri-apps/api/event';
import type { 
  SwitchyardConfig, 
  Session, 
  Turn, 
  TelemetryLog, 
  ProviderStatus, 
  ProviderConfig 
} from './types';
import { Sidebar } from './components/Sidebar';
import { ChatArea } from './components/ChatArea';
import { ControlCenter } from './components/ControlCenter';
import { SettingsModal } from './components/SettingsModal';
import { updateSessionPeers } from './services/api';
import { ArtifactDrawer } from './components/ArtifactDrawer';
import { renderMessageBody, isSystemStatusText, renderTurnEvents } from './components/ui/RenderHelpers';

function App() {
  const [sessions, setSessions] = useState<Session[]>([]);
  const [selectedSession, setSelectedSession] = useState<Session | null>(null);
  const [turns, setTurns] = useState<Turn[]>([]);
  const [inputText, setInputText] = useState('');
  const [isGenerating, setIsGenerating] = useState(false);
  const [messageQueue, setMessageQueue] = useState<string[]>([]);
  
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
  const [showSettings, setShowSettings] = useState(false);
  const [settingsTab, setSettingsTab] = useState<string>('general');

  // New Session Creator State
  const [newSessionProvider, setNewSessionProvider] = useState('codex');

  // Persistence & Artifact Drawer State
  const [activePersistentInstances, setActivePersistentInstances] = useState<string[]>([]);
  const [isArtifactDrawerOpen, setIsArtifactDrawerOpen] = useState(false);

  const loadActiveInstances = async () => {
    try {
      const active = await invoke<string[]>('list_active_instances');
      setActivePersistentInstances(active);
    } catch (e) {
      console.error('Failed to load active instances:', e);
    }
  };

  const handleStartPersistentInstance = async (provider: string) => {
    try {
      await invoke('start_instance', { name: provider });
      await loadActiveInstances();
    } catch (e: any) {
      console.error(`Failed to start persistent instance ${provider}:`, e);
      throw new Error(e.message || String(e));
    }
  };

  const handleStopPersistentInstance = async (provider: string) => {
    try {
      await invoke('stop_instance', { name: provider });
      await loadActiveInstances();
    } catch (e: any) {
      console.error(`Failed to stop persistent instance ${provider}:`, e);
      throw new Error(e.message || String(e));
    }
  };

  // Initial loading
  useEffect(() => {
    loadSessions();
    loadAppConfig();
    loadActiveInstances();
  }, []);

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
      console.log('Setting up Tauri event listener for runtime_event...');
      const refreshTurns = async () => {
        if (!active) return;
        const session = selectedSessionRef.current;
        console.log('refreshTurns called, current session:', session?.session_id);
        if (!session) return;
        try {
          const turnList = await invoke<Turn[]>('get_session_turns', { sessionId: session.session_id });
          if (!active) return;
          console.log(`Loaded ${turnList.length} turns for session ${session.session_id}`);
          setTurns(turnList);
          const eventList = await invoke<any[]>('get_session_events', { sessionId: session.session_id });
          if (!active) return;
          setSessionEvents(eventList);
        } catch (e) {
          console.error('Error fetching session turns/events:', e);
        }
      };

      try {
        const u = await listen<any>('runtime_event', (event) => {
          if (!active) return;
          console.log('Received runtime_event event:', event);
          const payload = event.payload;
          const type = payload.event;
          const data = payload.data;
          const now = new Date().toLocaleTimeString();
          console.log(`Event type: ${type}, data:`, data);
  
        switch (type) {
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
            if (isSystemStatusText(data.text)) {
              addLog(now, 'core', data.text);
            } else {
              setActiveCoreText((prev) => prev + data.text);
            }
            if (data.payload) {
              setSessionEvents((prev) => {
                const payload = data.payload;
                const item = payload.item || payload;
                const id = item.id || payload.id;
                if (id) {
                  const existingIdx = prev.findIndex((e) => {
                    const eItem = e.payload?.item || e.payload;
                    const eId = eItem?.id || e.payload?.id;
                    return e.turn_id === data.turn_id && eId === id;
                  });
                  if (existingIdx !== -1) {
                    const next = [...prev];
                    next[existingIdx] = {
                      ...next[existingIdx],
                      payload: payload,
                    };
                    return next;
                  }
                }
                return [
                  ...prev,
                  {
                    event_id: Math.random().toString(),
                    turn_id: data.turn_id,
                    event_type: 'item_updated',
                    provider: data.provider,
                    timestamp: new Date().toISOString(),
                    payload: payload,
                  },
                ];
              });
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
            if (isSystemStatusText(data.text)) {
              addLog(now, 'peer', data.text);
            } else {
              setActivePeerText((prev) => prev + data.text);
            }
            if (data.payload) {
              setSessionEvents((prev) => {
                const payload = data.payload;
                const item = payload.item || payload;
                const id = item.id || payload.id;
                if (id) {
                  const existingIdx = prev.findIndex((e) => {
                    const eItem = e.payload?.item || e.payload;
                    const eId = eItem?.id || e.payload?.id;
                    return e.turn_id === data.turn_id && eId === id;
                  });
                  if (existingIdx !== -1) {
                    const next = [...prev];
                    next[existingIdx] = {
                      ...next[existingIdx],
                      payload: payload,
                    };
                    return next;
                  }
                }
                return [
                  ...prev,
                  {
                    event_id: Math.random().toString(),
                    turn_id: data.turn_id,
                    event_type: 'item_updated',
                    provider: data.provider,
                    timestamp: new Date().toISOString(),
                    payload: payload,
                  },
                ];
              });
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
                  const arr = prev[data.turn_id] || [];
                  return { ...prev, [data.turn_id]: [...arr, ...lines] };
                });
              }
            }
            break;

          case 'PeerTerminalOutput':
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

          case 'HyardJobObserved':
            setHyardJobs((prev) => ({
              ...prev,
              [data.job.job_id]: {
                ...data.job,
                observed_at: data.observed_at,
              },
            }));
            addLog(now, 'sys', `[HYARD] Observed background job ${data.job.job_id} (${data.job.provider}) status: ${data.job.status}`);
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
            setActiveCoreText('');
            setActivePeerText('');
            addLog(now, 'sys', `Routed turn completed successfully.`);
            refreshTurns();
            break;

          case 'TurnFailed':
            setActiveNodes([]);
            setActiveTurnIds([]);
            setActiveCoreText('');
            setActivePeerText('');
            addLog(now, 'sys', `Turn failed: ${data.error}`);
            refreshTurns();
            break;
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
    setSelectedAgentTurnId(null);
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

  const dispatchMessage = async (sessionForSend: Session, message: string) => {
    setIsGenerating(true);
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
      user_message: message,
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
        provider: sessionForSend.active_core
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
      setActiveCoreText('');
      setActivePeerText('');
      // Reload turns database state
      const updatedTurns = await invoke<Turn[]>('get_session_turns', { sessionId: sessionForSend.session_id });
      setTurns(updatedTurns);
      // Reload sessions list to refresh updated times
      loadSessions();
      // Reload session events
      try {
        const eventList = await invoke<any[]>('get_session_events', { sessionId: sessionForSend.session_id });
        setSessionEvents(eventList);
      } catch (e) {
        console.error(e);
      }
    }
  };

  const handleSend = () => {
    const text = inputText.trim();
    if (!text || !selectedSession) return;

    setInputText('');

    if (isGenerating) {
      setMessageQueue((prev) => {
        const next = [...prev, text];
        addLog(new Date().toLocaleTimeString(), 'sys', `Queued message (${next.length} pending)`);
        return next;
      });
    } else {
      dispatchMessage(selectedSession, text);
    }
  };

  // Drain one queued message whenever the previous turn finishes.
  useEffect(() => {
    if (isGenerating || messageQueue.length === 0 || !selectedSession) return;
    const [next, ...rest] = messageQueue;
    setMessageQueue(rest);
    dispatchMessage(selectedSession, next);
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [isGenerating, messageQueue, selectedSession]);

  const handleClearQueue = () => {
    if (messageQueue.length === 0) return;
    setMessageQueue([]);
    addLog(new Date().toLocaleTimeString(), 'sys', 'Cleared queued messages');
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

  const handleTogglePeer = async (peerName: string) => {
    if (!selectedSession) return;
    const isEnabled = selectedSession.enabled_peers.includes(peerName);
    let nextPeers = [...selectedSession.enabled_peers];
    if (isEnabled) {
      nextPeers = nextPeers.filter((p) => p !== peerName);
    } else {
      nextPeers.push(peerName);
    }
    
    try {
      await updateSessionPeers(selectedSession.session_id, nextPeers);
      const nextSession = { ...selectedSession, enabled_peers: nextPeers };
      setSelectedSession(nextSession);
      setSessions((prev) => prev.map((s) => s.session_id === selectedSession.session_id ? nextSession : s));
      addLog(new Date().toLocaleTimeString(), 'sys', `Successfully ${isEnabled ? 'disabled' : 'enabled'} peer: ${peerName}`);
    } catch (e) {
      console.error('Failed to update session peers:', e);
      alert('Failed to update session peers: ' + e);
    }
  };

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

  const handleExportSessionTrace = async (sessionId: string) => {
    try {
      const traceJson = await invoke<string>('export_session_trace', { sessionId });
      const blob = new Blob([traceJson], { type: 'application/json' });
      const url = URL.createObjectURL(blob);
      const a = document.createElement('a');
      a.href = url;
      const name = sessions.find(s => s.session_id === sessionId)?.name || sessionId.substring(0, 8);
      a.download = `switchyard-trace-${name}.json`;
      a.click();
      URL.revokeObjectURL(url);
      addLog(new Date().toLocaleTimeString(), 'sys', `Exported trace for session: ${name}`);
    } catch (e) {
      console.error('Failed to export trace:', e);
      alert('Failed to export trace: ' + e);
    }
  };

  const handleImportSessionTrace = async (traceJson: string) => {
    try {
      const session = await invoke<Session>('import_session_trace', { traceJson });
      setSessions((prev) => [session, ...prev]);
      selectSession(session);
      addLog(new Date().toLocaleTimeString(), 'sys', `Successfully imported session trace: ${session.name || session.session_id}`);
    } catch (e) {
      console.error('Failed to import trace:', e);
      alert('Failed to import trace: ' + e);
    }
  };

  return (
    <div className="app-container">
      {/* 1. Sidebar Panel */}
      <Sidebar 
        sessions={sessions}
        selectedSession={selectedSession}
        config={config}
        newSessionProvider={newSessionProvider}
        setNewSessionProvider={setNewSessionProvider}
        onCreateSession={createNewSession}
        onSelectSession={selectSession}
        onTogglePeer={handleTogglePeer}
        onOpenSettings={() => setShowSettings(true)}
        onDeleteSession={handleDeleteSession}
        onRenameSession={handleRenameSession}
        onExportSessionTrace={handleExportSessionTrace}
        onImportSessionTrace={handleImportSessionTrace}
      />

      {/* 2. Main Chat Area */}
      <ChatArea
        selectedSession={selectedSession}
        isGenerating={isGenerating}
        turns={turns}
        inputText={inputText}
        setInputText={setInputText}
        handleSend={handleSend}
        handleCancel={handleCancel}
        activeCoreText={activeCoreText}
        activeCoreTurnId={activeCoreTurnId}
        activePeerName={activePeerName}
        activePeerTurnId={activePeerTurnId}
        activePeerText={activePeerText}
        sessionEvents={sessionEvents}
        realtimeTerminalLines={realtimeTerminalLines}
        renderMessageBody={renderMessageBody}
        renderTurnEvents={renderTurnEvents}
        queuedMessages={messageQueue}
        onClearQueue={handleClearQueue}
      />

      {/* 3. Control Center (Tabs: Topology, Agents, Telemetry) */}
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
        renderTurnEvents={renderTurnEvents}
        providerStatuses={providerStatuses}
        providerStatusLoading={providerStatusLoading}
        providerStatusError={providerStatusError}
        refreshProviderStatuses={refreshProviderStatuses}
        activePersistentInstances={activePersistentInstances}
        onStartPersistentInstance={handleStartPersistentInstance}
        onStopPersistentInstance={handleStopPersistentInstance}
        selectedSession={selectedSession}
        onUpdateSessionSummary={handleUpdateSessionSummary}
        onUpdateSessionChecklist={handleUpdateSessionChecklist}
      />

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

      {/* Artifact Explorer Drawer */}
      <ArtifactDrawer 
        isOpen={isArtifactDrawerOpen}
        onToggle={() => setIsArtifactDrawerOpen(!isArtifactDrawerOpen)}
      />
    </div>
  );
}

export default App;
