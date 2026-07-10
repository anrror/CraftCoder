/**
 * Protocol types mirroring `code-agent-rs/protocol/src/lib.rs`.
 * Used for JSON-RPC communication with the App Server over stdio.
 */

// ---------------------------------------------------------------------------
// Identity types
// ---------------------------------------------------------------------------

export interface SessionId {
  session_id: string;
}

export interface ThreadId {
  thread_id: string;
}

export interface TurnId {
  turn_id: string;
}

// ---------------------------------------------------------------------------
// Messaging types
// ---------------------------------------------------------------------------

export type Message =
  | { user_message: { content: string } }
  | { assistant_message: { content: string } }
  | { tool_call: ToolCall }
  | { tool_result: ToolResultMessage };

export interface ToolCall {
  id: string;
  name: string;
  arguments: Record<string, unknown>;
}

export interface ToolResultMessage {
  tool_call_id: string;
  output?: string;
  error?: string;
}

// ---------------------------------------------------------------------------
// Turn lifecycle types
// ---------------------------------------------------------------------------

export interface TurnInput {
  thread_id: string;
  messages: Message[];
}

export type ResponseEvent =
  | { turn_started: { turn_id: string } }
  | { agent_message_delta: { content: string } }
  | { tool_call_begin: { tool_call: ToolCall } }
  | { tool_call_end: { tool_call_id: string; result: ToolResultMessage } }
  | { turn_complete: { turn_id: string; final_message: Message } }
  | { error: { message: string } }
  | { token_usage: { prompt_tokens: number; completion_tokens: number } };

// ---------------------------------------------------------------------------
// Configuration types
// ---------------------------------------------------------------------------

export type SessionStatus = 'active' | 'paused' | 'completed' | 'archived';
export type PermissionMode = 'auto' | 'permit' | 'block';
export type CapabilityLevel = 'read' | 'edit' | 'exec';

// ---------------------------------------------------------------------------
// JSON-RPC 2.0 envelope types
// ---------------------------------------------------------------------------

export interface JsonRpcRequest {
  jsonrpc: '2.0';
  id?: number | string | null;
  method: string;
  params?: unknown;
}

export interface JsonRpcResponse {
  jsonrpc: '2.0';
  id?: number | string | null;
  result?: unknown;
  error?: JsonRpcError;
}

export interface JsonRpcError {
  code: number;
  message: string;
  data?: unknown;
}

export interface JsonRpcNotification {
  jsonrpc: '2.0';
  method: string;
  params?: unknown;
}

// ---------------------------------------------------------------------------
// Method-specific parameter and result types
// ---------------------------------------------------------------------------

export interface InitializeParams {
  protocol_version?: string;
  capabilities?: Record<string, unknown>;
  client_info?: {
    name: string;
    version?: string;
  };
}

export interface InitializeResult {
  protocol_version: string;
  server_info: {
    name: string;
    version?: string;
  };
  capabilities: Record<string, unknown>;
}

export interface CreateThreadParams {
  thread_id: string;
  system_instructions?: string;
  max_iterations?: number;
}

export interface CreateThreadResult {
  thread_id: string;
  session_id: string;
}

export interface SubmitTurnParams {
  thread_id: string;
  messages: Message[];
}

export interface SubmitTurnResult {
  thread_id: string;
  status: 'accepted';
}

export interface GetThreadParams {
  thread_id: string;
}

export interface GetThreadResult {
  thread_id: string;
  session_id: string;
  turn_count: number;
  status: string;
  system_instructions: string;
}

export interface ListThreadsResult {
  threads: string[];
  count: number;
  max_concurrent: number;
}

export interface ForkThreadParams {
  thread_id: string;
  new_thread_id: string;
}

export interface ForkThreadResult {
  thread_id: string;
  session_id: string;
  forked_from: string;
}

export interface ArchiveThreadParams {
  thread_id: string;
}

export interface ArchiveThreadResult {
  thread_id: string;
  archived: boolean;
}

// ---------------------------------------------------------------------------
// Error codes
// ---------------------------------------------------------------------------

export const JsonRpcErrorCodes = {
  PARSE_ERROR: -32700,
  INVALID_REQUEST: -32600,
  METHOD_NOT_FOUND: -32601,
  INVALID_PARAMS: -32602,
  INTERNAL_ERROR: -32603,
  SERVER_NOT_INITIALIZED: -32002,
  THREAD_NOT_FOUND: -32003,
  THREAD_ALREADY_EXISTS: -32004,
  MAX_THREADS_REACHED: -32005,
} as const;
