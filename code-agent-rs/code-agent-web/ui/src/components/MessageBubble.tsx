import { useState, useCallback } from 'react';
import ReactMarkdown from 'react-markdown';
import remarkGfm from 'remark-gfm';
import { Prism as SyntaxHighlighter } from 'react-syntax-highlighter';
import { oneDark } from 'react-syntax-highlighter/dist/esm/styles/prism';

// ── Types ──────────────────────────────────────────────────────────────────

interface MessageBubbleProps {
  role: 'user' | 'assistant' | 'system' | 'error';
  content: string;
  timestamp?: string;
  isMarkdown?: boolean;
}

interface CodeBlockProps {
  language: string;
  code: string;
}

// ── Code Block Component ────────────────────────────────────────────────────

function CodeBlock({ language, code }: CodeBlockProps) {
  const [copied, setCopied] = useState(false);

  const handleCopy = useCallback(() => {
    navigator.clipboard.writeText(code).then(() => {
      setCopied(true);
      setTimeout(() => setCopied(false), 2000);
    }).catch(() => {
      // Clipboard write failed — silently ignore
    });
  }, [code]);

  return (
    <div className="code-block-wrapper">
      {language && <span className="code-block-lang">{language}</span>}
      <button
        className={`code-block-copy${copied ? ' copied' : ''}`}
        onClick={handleCopy}
        aria-label="Copy code to clipboard"
      >
        {copied ? 'Copied!' : 'Copy'}
      </button>
      <SyntaxHighlighter
        style={oneDark}
        language={language || 'text'}
        PreTag="div"
        customStyle={{
          margin: 0,
          borderRadius: 'var(--radius-sm)',
          fontSize: '12px',
          lineHeight: '1.5',
          background: '#0d0d0d',
        }}
      >
        {code}
      </SyntaxHighlighter>
    </div>
  );
}

// ── Helpers ─────────────────────────────────────────────────────────────────

function escapeHtml(str: string): string {
  const div = document.createElement('div');
  div.textContent = str;
  return div.innerHTML;
}

function formatTime(): string {
  return new Date().toLocaleTimeString([], { hour: '2-digit', minute: '2-digit', second: '2-digit' });
}

const BADGE_LABELS: Record<string, string> = {
  user: 'User',
  assistant: 'Assistant',
  system: 'System',
  error: 'Error',
};

// ── Component ───────────────────────────────────────────────────────────────

export default function MessageBubble({ role, content, timestamp, isMarkdown }: MessageBubbleProps) {
  const badge = BADGE_LABELS[role] || role;
  const time = timestamp || formatTime();

  return (
    <div className={`msg ${role}`} data-role={role}>
      <div className="msg-header">
        <span className={`role-badge ${role}`}>{badge}</span>
        <span className="msg-time">{time}</span>
      </div>
      <div className="msg-body">
        {isMarkdown && role === 'assistant' ? (
          <ReactMarkdown
            remarkPlugins={[remarkGfm]}
            components={{
              code({ className, children, node: _node, ...props }) {
                const match = /language-(\w+)/.exec(className || '');
                const codeStr = String(children).replace(/\n$/, '');
                // Check if it's an inline code (no language class and single-line)
                if (!match && !codeStr.includes('\n')) {
                  return (
                    <code className={className} {...props}>
                      {children}
                    </code>
                  );
                }
                return (
                  <CodeBlock
                    language={match ? match[1] : ''}
                    code={codeStr}
                  />
                );
              },
              pre({ children }) {
                // Pre is handled by CodeBlock; just pass through children
                return <>{children}</>;
              },
            }}
          >
            {content}
          </ReactMarkdown>
        ) : (
          <span dangerouslySetInnerHTML={{ __html: escapeHtml(content) }} />
        )}
      </div>
    </div>
  );
}
