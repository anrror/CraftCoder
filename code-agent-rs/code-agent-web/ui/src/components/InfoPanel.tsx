import { useMemo } from 'react';
import type { ParsedEvent } from '../api/types';

interface InfoPanelProps {
  activeThreadId: string | null;
  events: ParsedEvent[];
  healthOnline: boolean;
  visible: boolean;
  onToggle: () => void;
}

export default function InfoPanel({ activeThreadId, events, healthOnline, visible, onToggle }: InfoPanelProps) {
  const stats = useMemo(() => {
    if (!activeThreadId || events.length === 0) return null;

    const turnCount = events.filter((e) => {
      const keys = Object.keys(e);
      return keys[0] === 'turn_started';
    }).length;
    const msgCount = events.filter((e) => {
      const keys = Object.keys(e);
      return keys[0] === 'agent_message_delta';
    }).length;
    const toolCount = events.filter((e) => {
      const keys = Object.keys(e);
      return keys[0] === 'tool_call_begin';
    }).length;

    return { turnCount, msgCount, toolCount, totalEvents: events.length };
  }, [activeThreadId, events]);

  if (!visible) return null;

  return (
    <div id="info-panel" className={!activeThreadId ? 'empty' : ''}>
      <div id="info-panel-header">
        <h2>Thread Info</h2>
        <button id="info-panel-toggle" title="Toggle panel" onClick={onToggle}>
          &times;
        </button>
      </div>
      <div id="info-panel-content">
        {!activeThreadId ? (
          <span style={{ color: 'var(--text-muted)', fontSize: '12px' }}>
            Select a thread to view details
          </span>
        ) : (
          <>
            <div className="info-section">
              <h3>Thread</h3>
              <div className="info-field">
                <span className="field-label">ID</span>
                <span className="field-value">{activeThreadId}</span>
              </div>
            </div>

            {stats && (
              <div className="info-section">
                <h3>Statistics</h3>
                <div className="info-field">
                  <span className="field-label">Turns</span>
                  <span className="field-value">{stats.turnCount}</span>
                </div>
                <div className="info-field">
                  <span className="field-label">Messages</span>
                  <span className="field-value">{stats.msgCount}</span>
                </div>
                <div className="info-field">
                  <span className="field-label">Tool Calls</span>
                  <span className="field-value">{stats.toolCount}</span>
                </div>
                <div className="info-field">
                  <span className="field-label">Events</span>
                  <span className="field-value">{stats.totalEvents}</span>
                </div>
              </div>
            )}

            <div className="info-section">
              <h3>Connection</h3>
              <div className="info-field">
                <span className="field-label">Server</span>
                <span className="field-value" style={{
                  color: healthOnline ? 'var(--success-color)' : 'var(--error-color)',
                }}>
                  {healthOnline ? 'Online' : 'Offline'}
                </span>
              </div>
            </div>
          </>
        )}
      </div>
    </div>
  );
}
