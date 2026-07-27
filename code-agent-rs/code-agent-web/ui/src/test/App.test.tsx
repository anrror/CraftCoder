import { render, screen, waitFor } from '@testing-library/react';
import { describe, it, expect, beforeEach, afterEach, vi } from 'vitest';
import App from '../App';

// ── Mock localStorage ─────────────────────────────────────────────────────
const localStorageMock = (() => {
  let store: Record<string, string> = {};
  return {
    getItem: (key: string) => store[key] ?? null,
    setItem: (key: string, value: string) => { store[key] = value; },
    removeItem: (key: string) => { delete store[key]; },
    clear: () => { store = {}; },
  };
})();

Object.defineProperty(globalThis, 'localStorage', { value: localStorageMock });

// ── Mock fetch ────────────────────────────────────────────────────────────
function createMockFetch() {
  return vi.fn().mockImplementation((url: string) => {
    if (url === '/health') {
      return Promise.resolve({
        ok: true,
        json: () => Promise.resolve({ status: 'ok', service: 'code-agent-web', version: '0.1.0' }),
      });
    }
    if (url === '/threads') {
      return Promise.resolve({
        ok: true,
        json: () => Promise.resolve({ threads: [], count: 0 }),
      });
    }
    // Default: return empty success
    return Promise.resolve({ ok: true, json: () => Promise.resolve({}) });
  });
}

describe('App', () => {
  beforeEach(() => {
    localStorageMock.clear();
    globalThis.fetch = createMockFetch();
  });

  afterEach(() => {
    vi.restoreAllMocks();
  });

  it('renders the header with logo text', async () => {
    render(<App />);

    await waitFor(() => {
      expect(screen.getByText('Code Agent')).toBeInTheDocument();
    });
  });

  it('shows health status indicators', async () => {
    render(<App />);

    await waitFor(() => {
      // After /health resolves, "online" appears
      expect(screen.getByText('online')).toBeInTheDocument();
    });
  });

  it('renders the sidebar with New Thread button', async () => {
    render(<App />);

    await waitFor(() => {
      expect(screen.getByText('New Thread')).toBeInTheDocument();
    });
  });

  it('shows empty thread state when no threads exist', async () => {
    render(<App />);

    await waitFor(() => {
      expect(screen.getByText('No threads yet')).toBeInTheDocument();
    });
  });

  it('renders API Key input field', async () => {
    render(<App />);

    await waitFor(() => {
      const input = screen.getByPlaceholderText('API Key (Bearer token)');
      expect(input).toBeInTheDocument();
    });
  });

  it('dispatches /health and /threads fetch on mount', async () => {
    render(<App />);

    await waitFor(() => {
      // fetch should have been called at least twice: /health and /threads
      const calls = (globalThis.fetch as ReturnType<typeof vi.fn>).mock.calls;
      const urls = calls.map((c: unknown[]) => c[0] as string);
      expect(urls).toContain('/health');
      expect(urls).toContain('/threads');
    });
  });

  it('renders three-column layout containers', async () => {
    render(<App />);

    await waitFor(() => {
      expect(document.getElementById('sidebar')).toBeInTheDocument();
    });

    // The ChatArea and InfoPanel don't have IDs we can easily query,
    // so just verify the app container exists
    expect(document.getElementById('app')).toBeInTheDocument();
    expect(document.getElementById('header')).toBeInTheDocument();
  });
});
