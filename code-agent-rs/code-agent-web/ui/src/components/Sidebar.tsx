import { useState, useCallback } from 'react';

interface SidebarProps {
  threads: string[];
  activeThreadId: string | null;
  error: string | null;
  apiKey: string;
  threadNames: Record<string, string>;
  onCreateThread: () => void;
  onSelectThread: (tid: string) => void;
  onDeleteThread: (tid: string) => void;
  onRenameThread: (tid: string, name: string) => void;
  onApiKeyChange: (key: string) => void;
}

export default function Sidebar({
  threads,
  activeThreadId,
  error,
  apiKey,
  threadNames,
  onCreateThread,
  onSelectThread,
  onDeleteThread,
  onRenameThread,
  onApiKeyChange,
}: SidebarProps) {
  const [localKey, setLocalKey] = useState(apiKey);
  const [renamingId, setRenamingId] = useState<string | null>(null);
  const [renameValue, setRenameValue] = useState('');

  const handleKeyChange = (value: string) => {
    setLocalKey(value);
    onApiKeyChange(value);
  };

  const startRename = useCallback((tid: string) => {
    setRenamingId(tid);
    setRenameValue(threadNames[tid] || shortenId(tid));
  }, [threadNames]);

  const commitRename = useCallback((tid: string) => {
    const trimmed = renameValue.trim();
    if (trimmed) {
      onRenameThread(tid, trimmed);
    }
    setRenamingId(null);
    setRenameValue('');
  }, [renameValue, onRenameThread]);

  const cancelRename = useCallback(() => {
    setRenamingId(null);
    setRenameValue('');
  }, []);

  const displayName = (tid: string) => {
    return threadNames[tid] || shortenId(tid);
  };

  return (
    <div id="sidebar">
      <div id="sidebar-header">
        <h2>Threads</h2>
        <span id="thread-count">{threads.length}</span>
      </div>

      <button id="new-thread-btn" onClick={onCreateThread}>
        <svg viewBox="0 0 16 16" fill="currentColor" width="14" height="14">
          <path d="M8 2a.75.75 0 0 1 .75.75v4.5h4.5a.75.75 0 0 1 0 1.5h-4.5v4.5a.75.75 0 0 1-1.5 0v-4.5h-4.5a.75.75 0 0 1 0-1.5h4.5v-4.5A.75.75 0 0 1 8 2z" />
        </svg>
        New Thread
      </button>

      {/* API Key input */}
      <div style={{ padding: '0 var(--space-lg) var(--space-sm)' }}>
        <input
          type="password"
          placeholder="API Key (Bearer token)"
          value={localKey}
          onChange={(e) => handleKeyChange(e.target.value)}
          style={{
            width: '100%',
            background: 'var(--bg-input)',
            color: 'var(--text-primary)',
            border: '1px solid var(--border)',
            borderRadius: 'var(--radius-sm)',
            padding: 'var(--space-xs) var(--space-sm)',
            fontSize: '11px',
            fontFamily: 'var(--font-sans)',
          }}
        />
      </div>

      {/* Error display */}
      {error && (
        <div id="sidebar-error" style={{ display: 'block' }}>
          {error}
        </div>
      )}

      {/* Thread list */}
      <div id="thread-list">
        {threads.map((tid) => (
          <div
            key={tid}
            className={`thread-item${tid === activeThreadId ? ' active' : ''}`}
            onClick={() => onSelectThread(tid)}
          >
            <span className="thread-item-icon" />
            {renamingId === tid ? (
              <input
                className="thread-item-rename-input"
                value={renameValue}
                onChange={(e) => setRenameValue(e.target.value)}
                onBlur={() => commitRename(tid)}
                onKeyDown={(e) => {
                  if (e.key === 'Enter') {
                    e.preventDefault();
                    commitRename(tid);
                  } else if (e.key === 'Escape') {
                    e.preventDefault();
                    cancelRename();
                  }
                }}
                autoFocus
                onClick={(e) => e.stopPropagation()}
              />
            ) : (
              <span
                className="thread-item-label"
                title={displayName(tid)}
                onDoubleClick={(e) => {
                  e.stopPropagation();
                  startRename(tid);
                }}
              >
                {displayName(tid)}
              </span>
            )}
            <span
              className="thread-item-del"
              title="Delete thread"
              onClick={(e) => {
                e.stopPropagation();
                onDeleteThread(tid);
              }}
            >
              &times;
            </span>
          </div>
        ))}
      </div>

      {/* Empty state */}
      {threads.length === 0 && (
        <div id="sidebar-empty" style={{ display: 'flex' }}>
          <svg width="32" height="32" viewBox="0 0 16 16" fill="currentColor" opacity="0.2">
            <path d="M2 2.75C2 1.784 2.784 1 3.75 1h8.5c.966 0 1.75.784 1.75 1.75v7.5A1.75 1.75 0 0 1 12.25 12H9.06l-2.573 2.573A1.458 1.458 0 0 1 4 13.543V12H3.75A1.75 1.75 0 0 1 2 10.25z" />
          </svg>
          No threads yet
        </div>
      )}
    </div>
  );
}

function shortenId(tid: string): string {
  // For UUID-like strings (has dashes, longer than 16 chars): show first 8 + "..."
  if (tid.includes('-') && tid.length > 16) {
    return tid.substring(0, 8) + '...';
  }
  // For shorter strings: show as-is unless excessively long
  if (tid.length > 20) {
    return tid.substring(0, 17) + '...';
  }
  return tid;
}
