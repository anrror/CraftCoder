import { useRef, useCallback, useState } from 'react';
import type { ParsedEvent } from '../api/types';
import { getApiKey } from '../api/client';

const BASE = '';

export interface UseSSEReturn {
  send: (threadId: string, message: string) => void;
  cancel: () => void;
  streaming: boolean;
  error: string | null;
}

export function useSSE(onEvent: (event: ParsedEvent) => void): UseSSEReturn {
  const [streaming, setStreaming] = useState(false);
  const [error, setError] = useState<string | null>(null);
  const abortRef = useRef<AbortController | null>(null);

  const cancel = useCallback(() => {
    if (abortRef.current) {
      abortRef.current.abort();
      abortRef.current = null;
    }
  }, []);

  const send = useCallback(async (threadId: string, message: string) => {
    setError(null);
    setStreaming(true);

    const controller = new AbortController();
    abortRef.current = controller;

    try {
      // Use authHeaders WITHOUT Content-Type since SSE is a GET-like POST
      const headers: Record<string, string> = {};
      const key = getApiKey();
      if (key) {
        headers['Authorization'] = `Bearer ${key}`;
      }
      headers['Content-Type'] = 'application/json';

      const res = await fetch(`${BASE}/threads/${encodeURIComponent(threadId)}/turns`, {
        method: 'POST',
        headers,
        body: JSON.stringify({ message }),
        signal: controller.signal,
      });

      if (!res.ok) {
        let msg = `HTTP ${res.status}`;
        try {
          const b = await res.json();
          msg = (b as { error?: string }).error || msg;
        } catch {
          // ignore
        }
        throw new Error(msg);
      }

      const reader = res.body?.getReader();
      if (!reader) {
        throw new Error('No response body');
      }

      const decoder = new TextDecoder();
      let buffer = '';
      let eventData = '';

      while (true) {
        const { done, value } = await reader.read();
        if (done) break;

        buffer += decoder.decode(value, { stream: true });
        const lines = buffer.split('\n');
        buffer = lines.pop() || '';

        for (const line of lines) {
          // Axum SSE wraps the inner SSE: "data: event: xxx\ndata: {...}\n\n"
          if (line.startsWith('data: ')) {
            const inner = line.slice(6);

            if (inner.startsWith('event: ')) {
              // event type line — reset data
              eventData = '';
            } else if (inner.startsWith('data: ')) {
              eventData = inner.slice(6).trim();
            } else {
              // The entire inner data might be the event (no "event:" prefix)
              eventData = inner.trim();
            }
          } else if (line === '' && eventData) {
            // End of inner SSE event — process it
            try {
              const parsed: ParsedEvent = JSON.parse(eventData);
              onEvent(parsed);
            } catch {
              // Non-JSON data, ignore
            }
            eventData = '';
          }
        }
      }

      // Flush remaining buffer
      if (buffer.trim() && eventData) {
        try {
          const parsed: ParsedEvent = JSON.parse(eventData);
          onEvent(parsed);
        } catch {
          // ignore
        }
      }
    } catch (err: unknown) {
      if (err instanceof DOMException && err.name === 'AbortError') {
        onEvent({ error: { message: 'Stream cancelled' } });
      } else {
        const message = err instanceof Error ? err.message : 'Unknown stream error';
        setError(message);
        onEvent({ error: { message: `Stream error: ${message}` } });
      }
    } finally {
      setStreaming(false);
      abortRef.current = null;
    }
  }, [onEvent]);

  return { send, cancel, streaming, error };
}
