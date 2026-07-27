import { useState, useRef, useEffect, useMemo, useCallback } from 'react';
import type { ParsedEvent } from '../api/types';
import MessageBubble from './MessageBubble';
import ToolCall from './ToolCall';
import TokenUsage from './TokenUsage';

// ── Types ───────────────────────────────────────────────────────────────────

type DisplayItem =
  | { kind: 'message'; key: string; role: 'user' | 'assistant' | 'system' | 'error'; content: string; timestamp?: string }
  | { kind: 'tool_call'; key: string; name: string; args: unknown; result?: unknown; running: boolean }
  | { kind: 'token_usage'; key: string; promptTokens?: number; completionTokens?: number; totalTokens?: number };

// ── Event Processing ────────────────────────────────────────────────────────

function processEvents(events: ParsedEvent[]): DisplayItem[] {
  const items: DisplayItem[] = [];
  let idx = 0;

  for (const event of events) {
    const keys = Object.keys(event);
    const type = keys[0] || '';
    const data = (event[type] || {}) as Record<string, unknown>;

    switch (type) {
      case 'turn_started':
        items.push({
          kind: 'message',
          key: `sys-${idx++}`,
          role: 'system',
          content: `Turn started${data.turn_id ? ': ' + String(data.turn_id) : ''}`,
        });
        break;

      case 'agent_message_delta': {
        const delta = String(data.content || '');
        const last = items.length > 0 ? items[items.length - 1] : null;
        if (last && last.kind === 'message' && last.role === 'assistant') {
          last.content += delta;
        } else {
          items.push({
            kind: 'message',
            key: `asst-${idx++}`,
            role: 'assistant',
            content: delta,
          });
        }
        break;
      }

      case 'tool_call_begin': {
        const tc = (data.tool_call || data) as Record<string, unknown>;
        items.push({
          kind: 'tool_call',
          key: `tc-${idx++}`,
          name: String(tc.name || 'unknown'),
          args: tc.arguments || {},
          running: true,
        });
        break;
      }

      case 'tool_call_end': {
        // Find last tool call and update it
        for (let i = items.length - 1; i >= 0; i--) {
          const item = items[i];
          if (item.kind === 'tool_call') {
            item.result = (data.result || data) as Record<string, unknown>;
            item.running = false;
            break;
          }
        }
        break;
      }

      case 'turn_complete':
        items.push({
          kind: 'message',
          key: `sys-${idx++}`,
          role: 'system',
          content: 'Turn complete',
        });
        break;

      case 'error':
        items.push({
          kind: 'message',
          key: `err-${idx++}`,
          role: 'error',
          content: String(data.message || data.error || 'Unknown error'),
        });
        break;

      case 'token_usage': {
        const tu = data as { prompt_tokens?: number; completion_tokens?: number; total_tokens?: number };
        items.push({
          kind: 'token_usage',
          key: `tu-${idx++}`,
          promptTokens: tu.prompt_tokens,
          completionTokens: tu.completion_tokens,
          totalTokens: tu.total_tokens,
        });
        break;
      }

      case 'user_message':
        // Locally-added user message
        items.push({
          kind: 'message',
          key: `usr-${idx++}`,
          role: 'user',
          content: String(data.content || ''),
        });
        break;

      default:
        // Unknown event — show as system message
        items.push({
          kind: 'message',
          key: `uk-${idx++}`,
          role: 'system',
          content: JSON.stringify(event, null, 2),
        });
    }
  }

  return items;
}

// ── Props ───────────────────────────────────────────────────────────────────

interface ChatAreaProps {
  activeThreadId: string | null;
  events: ParsedEvent[];
  streaming: boolean;
  loadingHistory: boolean;
  onSend: (message: string) => void;
  onCancel: () => void;
}

// ── Component ───────────────────────────────────────────────────────────────

export default function ChatArea({
  activeThreadId,
  events,
  streaming,
  loadingHistory,
  onSend,
  onCancel,
}: ChatAreaProps) {
  const [input, setInput] = useState('');
  const messagesEndRef = useRef<HTMLDivElement>(null);
  const textareaRef = useRef<HTMLTextAreaElement>(null);

  // Process events into display items
  const displayItems = useMemo(() => processEvents(events), [events]);

  // Auto-scroll when items change
  useEffect(() => {
    if (messagesEndRef.current) {
      messagesEndRef.current.scrollIntoView({ behavior: 'smooth' });
    }
  }, [displayItems]);

  // Focus textarea when thread changes
  useEffect(() => {
    if (activeThreadId && textareaRef.current) {
      textareaRef.current.focus();
    }
  }, [activeThreadId]);

  // Auto-resize textarea
  const handleInputChange = useCallback((e: React.ChangeEvent<HTMLTextAreaElement>) => {
    setInput(e.target.value);
    const ta = e.target;
    ta.style.height = 'auto';
    ta.style.height = Math.min(ta.scrollHeight, 150) + 'px';
  }, []);

  // Handle send
  const handleSend = useCallback(() => {
    const text = input.trim();
    if (!text || !activeThreadId || streaming) return;
    setInput('');
    // Reset textarea height
    if (textareaRef.current) {
      textareaRef.current.style.height = 'auto';
    }
    onSend(text);
  }, [input, activeThreadId, streaming, onSend]);

  // Handle keydown
  const handleKeyDown = useCallback((e: React.KeyboardEvent<HTMLTextAreaElement>) => {
    if (e.key === 'Enter' && !e.shiftKey) {
      e.preventDefault();
      handleSend();
    }
  }, [handleSend]);

  // Compute turn count from events
  const turnCount = useMemo(() => {
    return events.filter((e) => {
      const keys = Object.keys(e);
      return keys[0] === 'turn_started';
    }).length;
  }, [events]);

  const hasContent = displayItems.length > 0;
  const inputDisabled = !activeThreadId || streaming;

  return (
    <div id="main">
      {/* Chat header */}
      <div id="chat-header">
        {activeThreadId ? (
          <>
            <span className="thread-id-display">{activeThreadId}</span>
            <span className="thread-info-spacer" />
            <span className="thread-info-badge">{turnCount} turns</span>
          </>
        ) : (
          <>
            <span className="thread-id-display">No thread selected</span>
            <span className="thread-info-spacer" />
          </>
        )}
      </div>

      {/* Messages area */}
      <div id="messages">
        {loadingHistory && (
          <div style={{ textAlign: 'center', color: 'var(--text-muted)', padding: 'var(--space-xl)' }}>
            Loading history...
          </div>
        )}

        {!loadingHistory && !hasContent && (
          <div className="empty-state">
            <div className="empty-state-icon">
              <svg viewBox="0 0 24 24" fill="none" stroke="currentColor" strokeWidth="1.5">
                <path d="M8.625 9.75a.375.375 0 1 1-.75 0 .375.375 0 0 1 .75 0m0 0H8.25m4.125 0a.375.375 0 1 1-.75 0 .375.375 0 0 1 .75 0m0 0H12m4.125 0a.375.375 0 1 1-.75 0 .375.375 0 0 1 .75 0m0 0h-.375m-13.5 3.01c0 1.6 1.123 2.994 2.707 3.227 1.087.16 2.185.283 3.293.369V21l4.184-4.183a1.14 1.14 0 0 1 .778-.332 48.294 48.294 0 0 0 5.83-.498c1.585-.233 2.708-1.626 2.708-3.228V6.741c0-1.602-1.123-2.995-2.707-3.228A48.394 48.394 0 0 0 12 3c-2.392 0-4.744.175-7.043.513C3.373 3.746 2.25 5.14 2.25 6.741z" />
              </svg>
            </div>
            <div className="empty-state-text">
              {activeThreadId ? 'Thread is ready. Send a message to start.' : 'Select or create a thread'}
            </div>
            {!activeThreadId && (
              <div className="empty-state-hint">Create a new thread to start chatting with the agent</div>
            )}
          </div>
        )}

        {/* Render display items */}
        {displayItems.map((item) => {
          switch (item.kind) {
            case 'message':
              return (
                <MessageBubble
                  key={item.key}
                  role={item.role}
                  content={item.content}
                  timestamp={item.timestamp}
                  isMarkdown={item.role === 'assistant'}
                />
              );
            case 'tool_call':
              return (
                <ToolCall
                  key={item.key}
                  name={item.name}
                  args={item.args}
                  result={item.result}
                  running={item.running}
                />
              );
            case 'token_usage':
              return (
                <TokenUsage
                  key={item.key}
                  promptTokens={item.promptTokens}
                  completionTokens={item.completionTokens}
                  totalTokens={item.totalTokens}
                />
              );
            default:
              return null;
          }
        })}

        {/* Streaming cursor */}
        {streaming && hasContent && (
          <span className="streaming-cursor" />
        )}

        <div ref={messagesEndRef} />
      </div>

      {/* Input area */}
      <div id="input-area">
        <div id="input-row">
          <textarea
            ref={textareaRef}
            id="input"
            placeholder="Type a message... (Enter to send, Shift+Enter for new line)"
            rows={1}
            disabled={inputDisabled}
            value={input}
            onChange={handleInputChange}
            onKeyDown={handleKeyDown}
          />
          <button
            id="send-btn"
            disabled={inputDisabled || !input.trim()}
            onClick={handleSend}
          >
            Send
          </button>
          <button
            id="cancel-btn"
            className={streaming ? 'visible' : ''}
            title="Cancel streaming"
            onClick={onCancel}
          >
            Stop
          </button>
        </div>
        <div id="input-hint">Shift+Enter for new line</div>
      </div>
    </div>
  );
}
