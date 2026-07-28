import { render, screen } from '@testing-library/react';
import { describe, it, expect } from 'vitest';
import MessageBubble from '../components/MessageBubble';

describe('MessageBubble', () => {
  it('renders user message with badge', () => {
    render(<MessageBubble role="user" content="Hello" />);
    expect(screen.getByText('User')).toBeInTheDocument();
    expect(screen.getByText('Hello')).toBeInTheDocument();
  });

  it('renders assistant message with badge', () => {
    render(<MessageBubble role="assistant" content="Hi there!" />);
    expect(screen.getByText('Assistant')).toBeInTheDocument();
    expect(screen.getByText('Hi there!')).toBeInTheDocument();
  });

  it('renders system message with badge', () => {
    render(<MessageBubble role="system" content="System message" />);
    expect(screen.getByText('System')).toBeInTheDocument();
    expect(screen.getByText('System message')).toBeInTheDocument();
  });

  it('renders error message with badge', () => {
    render(<MessageBubble role="error" content="Something went wrong" />);
    expect(screen.getByText('Error')).toBeInTheDocument();
    expect(screen.getByText('Something went wrong')).toBeInTheDocument();
  });

  it('sets data-role attribute', () => {
    const { container } = render(<MessageBubble role="user" content="test" />);
    expect(container.querySelector('[data-role="user"]')).toBeInTheDocument();
  });

  it('renders assistant content as markdown with bold', () => {
    render(<MessageBubble role="assistant" content="Hello **world**" isMarkdown />);
    // Markdown bold renders as <strong>
    const body = document.querySelector('.msg-body');
    expect(body?.innerHTML).toContain('<strong>');
  });

  it('renders assistant content as markdown with code', () => {
    render(<MessageBubble role="assistant" content="Use `code` here" isMarkdown />);
    const body = document.querySelector('.msg-body');
    expect(body?.innerHTML).toContain('<code>');
  });

  it('renders user content as plain text (escaped)', () => {
    render(<MessageBubble role="user" content="<script>alert('xss')</script>" />);
    const body = document.querySelector('.msg-body');
    // Should be HTML-escaped, not rendered as a script tag
    expect(body?.innerHTML).not.toContain('<script>');
    expect(body?.textContent).toContain('alert');
  });

  it('escapes HTML in markdown content', () => {
    render(<MessageBubble role="assistant" content="<img src=x onerror=alert(1)>" isMarkdown />);
    const body = document.querySelector('.msg-body');
    // The content should be escaped, so the tag should appear as text
    expect(body?.textContent).toContain('onerror');
    expect(body?.innerHTML).not.toContain('<img');
  });
});
