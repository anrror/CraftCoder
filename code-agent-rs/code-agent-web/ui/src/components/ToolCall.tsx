import { useState } from 'react';

interface ToolCallProps {
  name: string;
  args: unknown;
  result?: unknown;
  running?: boolean;
}

export default function ToolCall({ name, args, result, running }: ToolCallProps) {
  const [open, setOpen] = useState(false);

  const argsText = typeof args === 'string' ? args : JSON.stringify(args, null, 2);
  const argsPreview = typeof args === 'object' ? JSON.stringify(args).slice(0, 100) : String(args || '');

  let resultText: string;
  if (running) {
    resultText = 'Running...';
  } else if (result === null || result === undefined) {
    resultText = '(pending)';
  } else if (typeof result === 'object' && result !== null) {
    const r = result as Record<string, unknown>;
    resultText = String(r.output || r.error || JSON.stringify(result, null, 2));
  } else {
    resultText = String(result);
  }

  const displayName = running ? `${name} (running...)` : name;

  const toggleOpen = () => setOpen((prev) => !prev);

  return (
    <div className="tool-call-wrapper">
      <button
        className={`tool-call-toggle${open ? ' open' : ''}`}
        onClick={toggleOpen}
      >
        <span className="tool-icon">&#x2699;</span>
        <span className="tool-name">{escapeHtml(displayName)}</span>
        <span style={{
          color: 'var(--text-muted)',
          fontSize: '11px',
          overflow: 'hidden',
          textOverflow: 'ellipsis',
          whiteSpace: 'nowrap',
          flex: 1,
        }}>
          {escapeHtml(argsPreview)}
        </span>
        <span className="tool-chevron">&#x25B6;</span>
      </button>
      <div className={`tool-call-body${open ? ' open' : ''}`}>
        <div className="tool-section">
          <div className="tool-section-label">Arguments</div>
          <div className="tool-section-content">{escapeHtml(argsText)}</div>
        </div>
        <div className="tool-section">
          <div className="tool-section-label">Result</div>
          <div className="tool-section-content">{escapeHtml(resultText)}</div>
        </div>
      </div>
    </div>
  );
}

function escapeHtml(str: string): string {
  const div = document.createElement('div');
  div.textContent = str;
  return div.innerHTML;
}
