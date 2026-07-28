import { useState, useEffect, useCallback, useRef } from 'react';
import type { ParsedEvent } from './api/types';
import { apiFetch, getApiKey, setApiKey } from './api/client';
import { useSSE } from './hooks/useSSE';
import { ToastProvider, useToast } from './components/Toast';
import Sidebar from './components/Sidebar';
import ChatArea from './components/ChatArea';
import InfoPanel from './components/InfoPanel';
import SettingsPanel, { loadSettings, saveSettings } from './components/SettingsPanel';
import type { AgentSettings } from './components/SettingsPanel';

const BASE = '';

function AppInner() {
  // ── State ──────────────────────────────────────────────────────────────
  const [theme, setTheme] = useState<'dark' | 'light'>(() => {
    return (localStorage.getItem('theme') as 'dark' | 'light') || 'dark';
  });

  // Sync data-theme attribute and localStorage
  useEffect(() => {
    document.documentElement.setAttribute('data-theme', theme);
    localStorage.setItem('theme', theme);
  }, [theme]);

  const toggleTheme = useCallback(() => {
    setTheme((prev) => (prev === 'dark' ? 'light' : 'dark'));
  }, []);

  const [threads, setThreads] = useState<string[]>([]);
  const [threadNames, setThreadNames] = useState<Record<string, string>>({});
  const [activeThreadId, setActiveThreadId] = useState<string | null>(null);
  const [eventHistory, setEventHistory] = useState<Record<string, ParsedEvent[]>>({});
  const [apiKey, setApiKeyState] = useState<string>(getApiKey);
  const [healthOnline, setHealthOnline] = useState(false);
  const [loadingHistory, setLoadingHistory] = useState(false);
  const [sidebarError, setSidebarError] = useState<string | null>(null);
  const [settings, setSettings] = useState<AgentSettings>(loadSettings);
  const [settingsOpen, setSettingsOpen] = useState(false);
  const [infoPanelVisible, setInfoPanelVisible] = useState(true);
  const [connStatus, setConnStatus] = useState('connecting...');

  const { addToast } = useToast();
  const activeThreadIdRef = useRef(activeThreadId);
  activeThreadIdRef.current = activeThreadId;
  const prevStreamingRef = useRef(false);

  // ── SSE Event Handler ──────────────────────────────────────────────────
  const handleSSEEvent = useCallback((event: ParsedEvent) => {
    const tid = activeThreadIdRef.current;
    if (!tid) return;
    setEventHistory((prev) => {
      const threadEvents = prev[tid] ? [...prev[tid], event] : [event];
      return { ...prev, [tid]: threadEvents };
    });
  }, []);

  const { send: sseSend, cancel: sseCancel, streaming } = useSSE(handleSSEEvent);

  // ── Track streaming end → refresh events ──────────────────────────────
  useEffect(() => {
    if (prevStreamingRef.current && !streaming && activeThreadId) {
      // Stream just ended — refresh events from API
      refreshEvents(activeThreadId);
    }
    prevStreamingRef.current = streaming;
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [streaming]);

  // ── API Helpers ────────────────────────────────────────────────────────
  const checkHealth = useCallback(async () => {
    try {
      const res = await fetch(`${BASE}/health`);
      if (res.ok) {
        setHealthOnline(true);
        setConnStatus('connected');
      } else {
        throw new Error('unhealthy');
      }
    } catch {
      setHealthOnline(false);
      setConnStatus('server offline');
    }
  }, []);

  const loadThreads = useCallback(async () => {
    try {
      const res = await apiFetch('/threads');
      const data = await res.json() as { threads: string[]; count: number };
      const newThreads = data.threads || [];
      setThreads(newThreads);
      setSidebarError(null);
    } catch (err: unknown) {
      const msg = err instanceof Error ? err.message : 'Unknown error';
      setThreads([]);
      setSidebarError(`Failed to load threads: ${msg}`);
    }
  }, []);

  const refreshEvents = useCallback(async (tid: string) => {
    try {
      const res = await apiFetch(`/threads/${encodeURIComponent(tid)}/events`);
      const data = await res.json() as { thread_id: string; events: ParsedEvent[] };
      setEventHistory((prev) => ({
        ...prev,
        [tid]: data.events || [],
      }));
    } catch {
      // silently fail — events already captured from stream
    }
  }, []);

  // ── Thread Actions ─────────────────────────────────────────────────────
  const handleCreateThread = useCallback(async () => {
    try {
      const body: Record<string, unknown> = {
        system_instructions: settings.systemInstructions,
        max_iterations: settings.maxIterations,
      };
      if (settings.model) {
        body.model = settings.model;
      }
      if (settings.temperature !== undefined) {
        body.temperature = settings.temperature;
      }
      const res = await apiFetch('/threads', {
        method: 'POST',
        body: JSON.stringify(body),
      });
      const data = await res.json() as { thread_id: string; session_id: string };
      setThreads((prev) => [...prev, data.thread_id]);
      addToast('Thread created', 'success');
      handleSelectThread(data.thread_id);
    } catch (err: unknown) {
      const msg = err instanceof Error ? err.message : 'Unknown error';
      addToast(`Failed to create thread: ${msg}`, 'error');
    }
  }, [addToast, settings]);

  const handleSelectThread = useCallback(async (tid: string) => {
    setActiveThreadId(tid);
    setLoadingHistory(true);

    try {
      const res = await apiFetch(`/threads/${encodeURIComponent(tid)}/events`);
      const data = await res.json() as { thread_id: string; events: ParsedEvent[] };
      const events = data.events || [];
      setEventHistory((prev) => ({ ...prev, [tid]: events }));
    } catch {
      setEventHistory((prev) => ({ ...prev, [tid]: [] }));
    } finally {
      setLoadingHistory(false);
    }
  }, []);

  const handleDeleteThread = useCallback(async (tid: string) => {
    try {
      await apiFetch(`/threads/${encodeURIComponent(tid)}`, { method: 'DELETE' });
      setThreads((prev) => prev.filter((t) => t !== tid));
      setEventHistory((prev) => {
        const next = { ...prev };
        delete next[tid];
        return next;
      });
      if (activeThreadId === tid) {
        setActiveThreadId(null);
      }
      addToast('Thread deleted', 'success');
    } catch (err: unknown) {
      const msg = err instanceof Error ? err.message : 'Unknown error';
      addToast(`Failed to delete thread: ${msg}`, 'error');
    }
  }, [activeThreadId, addToast]);

  const handleRenameThread = useCallback((tid: string, name: string) => {
    setThreadNames((prev) => ({ ...prev, [tid]: name }));
  }, []);

  // ── Message Send ───────────────────────────────────────────────────────
  const handleSend = useCallback((message: string) => {
    if (!activeThreadId || streaming) return;

    // Add user message to event history locally
    const userEvent: ParsedEvent = { user_message: { content: message } };
    setEventHistory((prev) => {
      const threadEvents = prev[activeThreadId] ? [...prev[activeThreadId], userEvent] : [userEvent];
      return { ...prev, [activeThreadId]: threadEvents };
    });

    sseSend(activeThreadId, message);
  }, [activeThreadId, streaming, sseSend]);

  // ── API Key ────────────────────────────────────────────────────────────
  const handleApiKeyChange = useCallback((key: string) => {
    setApiKey(key);
    setApiKeyState(key);
  }, []);

  // ── Health & Thread Polling ────────────────────────────────────────────
  useEffect(() => {
    // Initial health check
    checkHealth().then(() => {
      loadThreads();
    });

    // Health poll every 30s
    const healthInterval = setInterval(checkHealth, 30000);

    // Thread poll every 10s (only when not streaming)
    const threadInterval = setInterval(async () => {
      if (streaming) return;
      try {
        const res = await apiFetch('/threads');
        const data = await res.json() as { threads: string[]; count: number };
        const newThreads = data.threads || [];
        setThreads((prev) => {
          const changed = newThreads.length !== prev.length ||
            newThreads.some((t, i) => t !== prev[i]);
          return changed ? newThreads : prev;
        });
      } catch {
        // silently ignore polling errors
      }
    }, 10000);

    return () => {
      clearInterval(healthInterval);
      clearInterval(threadInterval);
    };
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, []);

  // ── Derived State ──────────────────────────────────────────────────────
  const activeEvents = activeThreadId ? (eventHistory[activeThreadId] || []) : [];

  // ── UI Handlers ────────────────────────────────────────────────────────
  const handleCancel = useCallback(() => {
    sseCancel();
  }, [sseCancel]);

  const handleSaveSettings = useCallback((newSettings: AgentSettings) => {
    setSettings(newSettings);
    saveSettings(newSettings);
  }, []);

  // ── Render ─────────────────────────────────────────────────────────────
  return (
    <>
      {/* Header */}
      <div id="header">
        <div id="header-logo"><span>&#x25B6;</span> Code Agent</div>
        <div id="header-spacer" />
        <button
          id="theme-toggle"
          onClick={toggleTheme}
          title={theme === 'dark' ? 'Switch to light mode' : 'Switch to dark mode'}
          aria-label="Toggle theme"
        >
          {theme === 'dark' ? '\u2600' : '\u263E'}
        </button>
        <button
          id="settings-btn"
          onClick={() => setSettingsOpen(true)}
          title="Settings"
          aria-label="Settings"
        >
          {'\u2699'}
        </button>
        <div id="health-dot" className={healthOnline ? 'online' : 'offline'} title="Server health" />
        <span id="health-label">{healthOnline ? 'online' : 'offline'}</span>
        <span id="conn-status" style={connStatus === 'connecting...' ? { color: 'var(--warning-color)' } : connStatus === 'server offline' ? { color: 'var(--error-color)' } : streaming ? { color: 'var(--accent)' } : undefined}>
          {streaming ? 'streaming' : connStatus}
        </span>
      </div>

      {/* Three-column layout */}
      <div id="app">
        <Sidebar
          threads={threads}
          activeThreadId={activeThreadId}
          error={sidebarError}
          apiKey={apiKey}
          threadNames={threadNames}
          onCreateThread={handleCreateThread}
          onSelectThread={handleSelectThread}
          onDeleteThread={handleDeleteThread}
          onRenameThread={handleRenameThread}
          onApiKeyChange={handleApiKeyChange}
        />

        <ChatArea
          activeThreadId={activeThreadId}
          events={activeEvents}
          streaming={streaming}
          loadingHistory={loadingHistory}
          onSend={handleSend}
          onCancel={handleCancel}
        />

        <InfoPanel
          activeThreadId={activeThreadId}
          events={activeEvents}
          healthOnline={healthOnline}
          visible={infoPanelVisible}
          onToggle={() => setInfoPanelVisible((v) => !v)}
        />
      </div>

      <SettingsPanel
        open={settingsOpen}
        settings={settings}
        onSave={handleSaveSettings}
        onClose={() => setSettingsOpen(false)}
      />
    </>
  );
}

export default function App() {
  return (
    <ToastProvider>
      <AppInner />
    </ToastProvider>
  );
}
