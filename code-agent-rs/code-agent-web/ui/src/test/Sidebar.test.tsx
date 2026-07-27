import { render, screen, fireEvent } from '@testing-library/react';
import { describe, it, expect, vi } from 'vitest';
import Sidebar from '../components/Sidebar';

const mockThreads = ['thread-a', 'thread-b', 'thread-c'];

function renderSidebar(props: Partial<Parameters<typeof Sidebar>[0]> = {}) {
  return render(
    <Sidebar
      threads={props.threads ?? []}
      activeThreadId={props.activeThreadId ?? null}
      error={props.error ?? null}
      apiKey={props.apiKey ?? ''}
      threadNames={props.threadNames ?? {}}
      onCreateThread={props.onCreateThread ?? vi.fn()}
      onSelectThread={props.onSelectThread ?? vi.fn()}
      onDeleteThread={props.onDeleteThread ?? vi.fn()}
      onRenameThread={props.onRenameThread ?? vi.fn()}
      onApiKeyChange={props.onApiKeyChange ?? vi.fn()}
    />,
  );
}

describe('Sidebar', () => {
  it('renders header with thread count', () => {
    renderSidebar({ threads: mockThreads });
    expect(screen.getByText('Threads')).toBeInTheDocument();
    expect(screen.getByText('3')).toBeInTheDocument();
  });

  it('renders New Thread button', () => {
    renderSidebar();
    expect(screen.getByText('New Thread')).toBeInTheDocument();
  });

  it('shows empty state when no threads', () => {
    renderSidebar({ threads: [] });
    expect(screen.getByText('No threads yet')).toBeInTheDocument();
  });

  it('renders API Key input', () => {
    renderSidebar({ apiKey: 'sk-test' });
    const input = screen.getByPlaceholderText('API Key (Bearer token)');
    expect(input).toBeInTheDocument();
    expect(input).toHaveValue('sk-test');
  });

  it('calls onApiKeyChange when input changes', () => {
    const onApiKeyChange = vi.fn();
    renderSidebar({ onApiKeyChange });

    const input = screen.getByPlaceholderText('API Key (Bearer token)');
    fireEvent.change(input, { target: { value: 'new-key' } });

    expect(onApiKeyChange).toHaveBeenCalledWith('new-key');
  });

  it('renders thread list', () => {
    renderSidebar({ threads: mockThreads });
    mockThreads.forEach((tid) => {
      expect(screen.getByTitle(tid)).toBeInTheDocument();
    });
  });

  it('highlights active thread', () => {
    renderSidebar({ threads: mockThreads, activeThreadId: 'thread-b' });
    const items = document.querySelectorAll('.thread-item');
    expect(items[1].className).toContain('active');
  });

  it('calls onSelectThread when thread is clicked', () => {
    const onSelectThread = vi.fn();
    renderSidebar({ threads: mockThreads, onSelectThread });

    fireEvent.click(screen.getByTitle('thread-b'));
    expect(onSelectThread).toHaveBeenCalledWith('thread-b');
  });

  it('calls onDeleteThread when delete button is clicked', () => {
    const onDeleteThread = vi.fn();
    renderSidebar({ threads: mockThreads, onDeleteThread });

    const deleteBtns = document.querySelectorAll('.thread-item-del');
    fireEvent.click(deleteBtns[0]);

    expect(onDeleteThread).toHaveBeenCalledWith('thread-a');
  });

  it('calls onCreateThread when button is clicked', () => {
    const onCreateThread = vi.fn();
    renderSidebar({ onCreateThread });

    fireEvent.click(screen.getByText('New Thread'));
    expect(onCreateThread).toHaveBeenCalledOnce();
  });

  it('displays error message', () => {
    renderSidebar({ error: 'Failed to load threads' });
    expect(screen.getByText('Failed to load threads')).toBeInTheDocument();
  });
});
