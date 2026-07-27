import { render, screen, fireEvent } from '@testing-library/react';
import { describe, it, expect, vi } from 'vitest';
import ChatArea from '../components/ChatArea';
import type { ParsedEvent } from '../api/types';

const emptyEvents: ParsedEvent[] = [];

const sampleEvents: ParsedEvent[] = [
  { turn_started: { turn_id: 'turn-001' } },
  { agent_message_delta: { content: 'Hel' } },
  { agent_message_delta: { content: 'lo!' } },
  { tool_call_begin: { tool_call: { name: 'read_file', arguments: { path: 'test.txt' } } } },
  { tool_call_end: { result: { output: 'file content' }, tool_call_id: 'call_001' } },
  { token_usage: { prompt_tokens: 10, completion_tokens: 20, total_tokens: 30 } },
  { turn_complete: {} },
];

describe('ChatArea', () => {
  it('shows empty state when no thread selected', () => {
    render(
      <ChatArea
        activeThreadId={null}
        events={emptyEvents}
        streaming={false}
        loadingHistory={false}
        onSend={vi.fn()}
        onCancel={vi.fn()}
      />,
    );

    expect(screen.getByText('Select or create a thread')).toBeInTheDocument();
  });

  it('shows thread ready state when thread selected with no events', () => {
    render(
      <ChatArea
        activeThreadId="thread-1"
        events={emptyEvents}
        streaming={false}
        loadingHistory={false}
        onSend={vi.fn()}
        onCancel={vi.fn()}
      />,
    );

    expect(screen.getByText(/Thread is ready/)).toBeInTheDocument();
  });

  it('shows thread ID in header', () => {
    render(
      <ChatArea
        activeThreadId="thread-abc"
        events={emptyEvents}
        streaming={false}
        loadingHistory={false}
        onSend={vi.fn()}
        onCancel={vi.fn()}
      />,
    );

    expect(screen.getByText('thread-abc')).toBeInTheDocument();
  });

  it('renders events as display items', () => {
    render(
      <ChatArea
        activeThreadId="thread-1"
        events={sampleEvents}
        streaming={false}
        loadingHistory={false}
        onSend={vi.fn()}
        onCancel={vi.fn()}
      />,
    );

    // Turn started → system message
    expect(screen.getByText(/Turn started/)).toBeInTheDocument();

    // agent_message_delta concatenated → "Hello!"
    expect(screen.getByText('Hello!')).toBeInTheDocument();

    // tool_call_begin
    expect(screen.getByText('read_file')).toBeInTheDocument();

    // token_usage
    expect(screen.getByText('10')).toBeInTheDocument();
    expect(screen.getByText('20')).toBeInTheDocument();

    // turn_complete
    expect(screen.getByText('Turn complete')).toBeInTheDocument();
  });

  it('shows loading state', () => {
    render(
      <ChatArea
        activeThreadId="thread-1"
        events={emptyEvents}
        streaming={false}
        loadingHistory={true}
        onSend={vi.fn()}
        onCancel={vi.fn()}
      />,
    );

    expect(screen.getByText('Loading history...')).toBeInTheDocument();
  });

  it('shows streaming cursor when streaming', () => {
    const { container } = render(
      <ChatArea
        activeThreadId="thread-1"
        events={sampleEvents}
        streaming={true}
        loadingHistory={false}
        onSend={vi.fn()}
        onCancel={vi.fn()}
      />,
    );

    expect(container.querySelector('.streaming-cursor')).toBeInTheDocument();
  });

  it('hides streaming cursor when not streaming', () => {
    const { container } = render(
      <ChatArea
        activeThreadId="thread-1"
        events={sampleEvents}
        streaming={false}
        loadingHistory={false}
        onSend={vi.fn()}
        onCancel={vi.fn()}
      />,
    );

    expect(container.querySelector('.streaming-cursor')).not.toBeInTheDocument();
  });

  it('sends message on Enter', () => {
    const onSend = vi.fn();
    render(
      <ChatArea
        activeThreadId="thread-1"
        events={emptyEvents}
        streaming={false}
        loadingHistory={false}
        onSend={onSend}
        onCancel={vi.fn()}
      />,
    );

    const textarea = screen.getByPlaceholderText(/Type a message/);
    fireEvent.change(textarea, { target: { value: 'Hello agent' } });
    fireEvent.keyDown(textarea, { key: 'Enter' });

    expect(onSend).toHaveBeenCalledWith('Hello agent');
  });

  it('does not send empty message', () => {
    const onSend = vi.fn();
    render(
      <ChatArea
        activeThreadId="thread-1"
        events={emptyEvents}
        streaming={false}
        loadingHistory={false}
        onSend={onSend}
        onCancel={vi.fn()}
      />,
    );

    const textarea = screen.getByPlaceholderText(/Type a message/);
    fireEvent.keyDown(textarea, { key: 'Enter' });

    expect(onSend).not.toHaveBeenCalled();
  });

  it('disables input when no thread selected', () => {
    render(
      <ChatArea
        activeThreadId={null}
        events={emptyEvents}
        streaming={false}
        loadingHistory={false}
        onSend={vi.fn()}
        onCancel={vi.fn()}
      />,
    );

    expect(screen.getByPlaceholderText(/Type a message/)).toBeDisabled();
  });

  it('disables input when streaming', () => {
    render(
      <ChatArea
        activeThreadId="thread-1"
        events={emptyEvents}
        streaming={true}
        loadingHistory={false}
        onSend={vi.fn()}
        onCancel={vi.fn()}
      />,
    );

    expect(screen.getByPlaceholderText(/Type a message/)).toBeDisabled();
  });

  it('shows turn count in header', () => {
    render(
      <ChatArea
        activeThreadId="thread-1"
        events={sampleEvents}
        streaming={false}
        loadingHistory={false}
        onSend={vi.fn()}
        onCancel={vi.fn()}
      />,
    );

    expect(screen.getByText('1 turns')).toBeInTheDocument();
  });

  it('shows Stop button when streaming', () => {
    render(
      <ChatArea
        activeThreadId="thread-1"
        events={emptyEvents}
        streaming={true}
        loadingHistory={false}
        onSend={vi.fn()}
        onCancel={vi.fn()}
      />,
    );

    const stopBtn = screen.getByText('Stop');
    expect(stopBtn).toBeInTheDocument();
    expect(stopBtn.className).toContain('visible');
  });

  it('calls onCancel when Stop is clicked', () => {
    const onCancel = vi.fn();
    render(
      <ChatArea
        activeThreadId="thread-1"
        events={emptyEvents}
        streaming={true}
        loadingHistory={false}
        onSend={vi.fn()}
        onCancel={onCancel}
      />,
    );

    fireEvent.click(screen.getByText('Stop'));
    expect(onCancel).toHaveBeenCalledOnce();
  });
});
