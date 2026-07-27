const BASE = '';

const API_KEY_STORAGE_KEY = 'code_agent_api_key';

export function getApiKey(): string {
  return localStorage.getItem(API_KEY_STORAGE_KEY) || '';
}

export function setApiKey(key: string): void {
  localStorage.setItem(API_KEY_STORAGE_KEY, key);
}

export function authHeaders(extraHeaders: Record<string, string> = {}): Record<string, string> {
  const headers: Record<string, string> = {
    'Content-Type': 'application/json',
    ...extraHeaders,
  };
  const key = getApiKey();
  if (key) {
    headers['Authorization'] = `Bearer ${key}`;
  }
  return headers;
}

export async function apiFetch(path: string, options: RequestInit = {}): Promise<Response> {
  const res = await fetch(`${BASE}${path}`, {
    ...options,
    headers: authHeaders(options.headers ? Object.fromEntries(
      options.headers instanceof Headers
        ? Array.from(options.headers.entries())
        : Array.isArray(options.headers)
          ? options.headers
          : Object.entries(options.headers as Record<string, string>)
    ) : undefined),
  });

  if (!res.ok) {
    let msg = `HTTP ${res.status}`;
    try {
      const body = await res.json();
      msg = (body as { error?: string }).error || msg;
    } catch {
      // ignore parse errors
    }
    throw new Error(msg);
  }
  return res;
}
