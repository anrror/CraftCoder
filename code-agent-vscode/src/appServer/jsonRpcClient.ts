/**
 * JSON-RPC 2.0 client over stdio for communicating with the Code Agent App Server.
 *
 * Manages the child process lifecycle, request/response correlation via numeric
 * request IDs, event notification parsing, and automatic reconnection.
 */

import { ChildProcess, spawn } from 'child_process';
import { createInterface, Interface } from 'readline';
import { EventEmitter } from 'events';
import {
  JsonRpcRequest,
  JsonRpcResponse,
  JsonRpcNotification,
  ResponseEvent,
} from './types';

// ---------------------------------------------------------------------------
// Types
// ---------------------------------------------------------------------------

export type ConnectionState = 'disconnected' | 'connecting' | 'connected';

export interface AppServerConfig {
  /** Path to the app server binary. */
  binaryPath: string;
  /** Additional arguments for the binary. */
  args?: string[];
  /** Environment variables for the child process. */
  env?: Record<string, string>;
  /** Reconnect delay in ms (default 1000). */
  reconnectDelay?: number;
  /** Max reconnection attempts (default 10, -1 for infinite). */
  maxReconnectAttempts?: number;
}

export interface AppServerClientEvents {
  /** Fired when the connection state changes. */
  connectionChange: [state: ConnectionState];
  /** Fired when the server emits an event notification. */
  event: [event: ResponseEvent, threadId?: string];
  /** Fired on any error. */
  error: [error: Error];
  /** Fired when the server closes/crashes. */
  close: [code: number | null, signal: string | null];
}

interface ResolvedConfig {
  binaryPath: string;
  args: string[];
  env: Record<string, string>;
  reconnectDelay: number;
  maxReconnectAttempts: number;
}

// ---------------------------------------------------------------------------
// AppServerClient
// ---------------------------------------------------------------------------

export declare interface AppServerClient {
  on<E extends keyof AppServerClientEvents>(
    event: E,
    listener: (...args: AppServerClientEvents[E]) => void,
  ): this;
  off<E extends keyof AppServerClientEvents>(
    event: E,
    listener: (...args: AppServerClientEvents[E]) => void,
  ): this;
  emit<E extends keyof AppServerClientEvents>(
    event: E,
    ...args: AppServerClientEvents[E]
  ): boolean;
}

export class AppServerClient extends EventEmitter {
  private config: ResolvedConfig;
  private process: ChildProcess | null = null;
  private reader: Interface | null = null;
  private _state: ConnectionState = 'disconnected';
  private requestId = 0;
  private pendingRequests = new Map<
    number,
    { resolve: (value: unknown) => void; reject: (error: Error) => void }
  >();
  private buffer = '';
  private reconnectTimer: ReturnType<typeof setTimeout> | null = null;
  private reconnectAttempts = 0;
  private destroyed = false;
  private _sessionId: string | undefined;

  constructor(config: AppServerConfig) {
    super();
    this.config = {
      binaryPath: config.binaryPath,
      args: config.args ?? [],
      env: config.env ?? {},
      reconnectDelay: config.reconnectDelay ?? 1000,
      maxReconnectAttempts: config.maxReconnectAttempts ?? 10,
    };
  }

  // -----------------------------------------------------------------------
  // Public API
  // -----------------------------------------------------------------------

  get state(): ConnectionState {
    return this._state;
  }

  /** Start the app server and connect. */
  async connect(): Promise<void> {
    if (this.destroyed) {
      throw new Error('Client has been destroyed');
    }
    if (this._state === 'connected' || this._state === 'connecting') {
      return;
    }
    return this.doConnect();
  }

  /** Send a JSON-RPC request and wait for the response. */
  async request(method: string, params?: unknown): Promise<unknown> {
    if (this._state !== 'connected') {
      throw new Error(`Not connected (state: ${this._state})`);
    }

    const id = ++this.requestId;
    const request: JsonRpcRequest = {
      jsonrpc: '2.0',
      id,
      method,
      params,
    };

    const json = JSON.stringify(request);

    return new Promise<unknown>((resolve, reject) => {
      this.pendingRequests.set(id, { resolve, reject });

      if (!this.process?.stdin?.writable) {
        reject(new Error('Process stdin not writable'));
        this.pendingRequests.delete(id);
        return;
      }

      this.process.stdin.write(json + '\n', (err) => {
        if (err) {
          reject(err);
          this.pendingRequests.delete(id);
        }
      });
    });
  }

  /** Send a notification (no response expected). */
  sendNotification(method: string, params?: unknown): void {
    if (this._state !== 'connected') {
      return;
    }

    const notification: JsonRpcNotification = {
      jsonrpc: '2.0',
      method,
      params,
    };

    this.process?.stdin?.write(JSON.stringify(notification) + '\n');
  }

  /** Disconnect and stop the child process. */
  disconnect(): void {
    if (this.destroyed) return;
    this.destroyed = true;

    this.clearReconnectTimer();
    this.cleanup();
    this.setState('disconnected');
  }

  /** Is the client currently connected? */
  isConnected(): boolean {
    return this._state === 'connected';
  }

  /** Get the current session ID (placeholder — real session ID comes from initialize). */
  getSessionId(): string | undefined {
    return this._sessionId;
  }

  // -----------------------------------------------------------------------
  // Private: connection lifecycle
  // -----------------------------------------------------------------------

  private async doConnect(): Promise<void> {
    this.setState('connecting');

    const { binaryPath, args, env } = this.config;

    this.process = spawn(binaryPath, args, {
      stdio: ['pipe', 'pipe', 'pipe'],
      env: { ...process.env, ...env },
    });

    this.process.on('error', (err) => {
      this.emit('error', new Error(`Failed to spawn app server: ${err.message}`));
      this.handleClose(null, null);
    });

    this.process.on('close', (code, signal) => {
      this.handleClose(code, signal);
    });

    if (this.process.stderr) {
      let stderr = '';
      this.process.stderr.on('data', (chunk: Buffer) => {
        stderr += chunk.toString('utf-8');
      });
      this.process.stderr.on('end', () => {
        if (stderr.trim()) {
          this.emit('error', new Error(`App server stderr: ${stderr}`));
        }
      });
    }

    this.reader = createInterface({
      input: this.process.stdout!,
      crlfDelay: Infinity,
    });

    this.reader.on('line', (line: string) => {
      this.handleLine(line);
    });

    this.reader.on('close', () => {
      // stdout closed
    });
  }

  private handleLine(line: string): void {
    const trimmed = line.trim();
    if (!trimmed) return;

    try {
      const parsed = JSON.parse(trimmed);

      // Check if it's a notification (has method, no id in the outer envelope,
      // OR the result field contains a method field — AppServer uses a
      // notification-ish format for events)
      if (parsed.method && parsed.id === undefined && !('result' in parsed)) {
        this.handleNotification(parsed);
        return;
      }

      // It's a response (has id and result/error)
      if ('id' in parsed && (parsed.result !== undefined || parsed.error)) {
        this.handleResponse(parsed as JsonRpcResponse);
        return;
      }

      // Handle AppServer's notification format where the event is in result
      if (parsed.result && typeof parsed.result === 'object' && parsed.result.method) {
        this.handleNotification(parsed);
      }
    } catch (err) {
      this.emit('error', new Error(`Failed to parse JSON line: ${(err as Error).message}`));
    }
  }

  private handleNotification(notification: JsonRpcNotification): void {
    const method = notification.method;
    const params = notification.params as Record<string, unknown> | undefined;

    if (method === 'event' && params) {
      const event = params as unknown as ResponseEvent;
      let threadId: string | undefined;

      // Extract thread_id from various event types
      if ('turn_started' in event && typeof (event as any).turn_started?.turn_id === 'string') {
        threadId = undefined; // turn_started doesn't have thread_id directly
      }

      this.emit('event', event, threadId);
    } else if (method === 'exit') {
      // Server is shutting down
      this.cleanup();
    }
  }

  private handleResponse(response: JsonRpcResponse): void {
    const id = typeof response.id === 'number' ? response.id : undefined;
    if (id === undefined) return;

    const pending = this.pendingRequests.get(id);
    if (!pending) return;

    this.pendingRequests.delete(id);

    if (response.error) {
      pending.reject(
        new Error(`JSON-RPC error ${response.error.code}: ${response.error.message}`),
      );
    } else {
      pending.resolve(response.result);
    }
  }

  private handleClose(code: number | null, signal: string | null): void {
    this.cleanup();
    this.emit('close', code, signal);
    this.setState('disconnected');

    // Reject all pending requests
    for (const [, pending] of this.pendingRequests) {
      pending.reject(new Error('App server connection closed'));
    }
    this.pendingRequests.clear();

    // Auto-reconnect if not destroyed
    if (!this.destroyed) {
      this.scheduleReconnect();
    }
  }

  private scheduleReconnect(): void {
    const { reconnectDelay, maxReconnectAttempts } = this.config;
    this.reconnectAttempts++;

    if (maxReconnectAttempts >= 0 && this.reconnectAttempts > maxReconnectAttempts) {
      this.emit(
        'error',
        new Error(`Max reconnect attempts (${maxReconnectAttempts}) reached`),
      );
      this.destroyed = true;
      return;
    }

    this.emit(
      'error',
      new Error(
        `Reconnecting in ${reconnectDelay}ms (attempt ${this.reconnectAttempts}${
          maxReconnectAttempts >= 0 ? `/${maxReconnectAttempts}` : ''
        })`,
      ),
    );

    this.reconnectTimer = setTimeout(() => {
      if (!this.destroyed) {
        this.doConnect().catch((err) => {
          this.emit('error', err);
        });
      }
    }, reconnectDelay);
  }

  private clearReconnectTimer(): void {
    if (this.reconnectTimer) {
      clearTimeout(this.reconnectTimer);
      this.reconnectTimer = null;
    }
  }

  private cleanup(): void {
    this.reconnectAttempts = 0;

    if (this.reader) {
      this.reader.close();
      this.reader = null;
    }

    if (this.process) {
      try {
        this.process.stdin?.end();
        this.process.kill();
      } catch {
        // process may already be dead
      }
      this.process = null;
    }
  }

  private setState(state: ConnectionState): void {
    if (this._state !== state) {
      this._state = state;
      this.emit('connectionChange', state);
    }
  }
}
