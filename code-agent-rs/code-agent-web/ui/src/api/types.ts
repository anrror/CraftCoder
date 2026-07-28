// ── Request Types ────────────────────────────────────────────────────────────

export interface CreateThreadRequest {
  thread_id?: string;
  system_instructions?: string;
  max_iterations?: number;
}

export interface SubmitTurnRequest {
  message: string;
}

// ── Response Types ──────────────────────────────────────────────────────────

export interface ThreadListResponse {
  threads: string[];
  count: number;
}

export interface CreateThreadResponse {
  thread_id: string;
  session_id: string;
}

export interface DeleteThreadResponse {
  thread_id: string;
  deleted: boolean;
}

export interface EventsResponse {
  thread_id: string;
  events: Record<string, unknown>[];
}

export interface HealthResponse {
  status: string;
  service: string;
  version: string;
}

// ── SSE Event Types (ResponseEvent variants) ────────────────────────────────

export interface TurnStartedData {
  turn_id?: string;
}

export interface AgentMessageDeltaData {
  content?: string;
}

export interface ToolCallData {
  name: string;
  arguments: Record<string, unknown>;
}

export interface ToolCallBeginData {
  tool_call?: ToolCallData;
}

export interface ToolResultData {
  output?: string;
  error?: string;
}

export interface ToolCallEndData {
  result?: ToolResultData;
  tool_call_id?: string;
}

export interface TokenUsageData {
  prompt_tokens?: number;
  completion_tokens?: number;
  total_tokens?: number;
}

export interface ErrorData {
  message?: string;
  error?: string;
}

// ── Parsed SSE Event ────────────────────────────────────────────────────────

/** A parsed SSE event where the key is the variant name and the value is the data object */
export type ParsedEvent = Record<string, unknown>;

/** Known SSE event type names */
export type EventType =
  | 'turn_started'
  | 'agent_message_delta'
  | 'tool_call_begin'
  | 'tool_call_end'
  | 'turn_complete'
  | 'error'
  | 'token_usage';
