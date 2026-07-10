/**
 * WebView-based chat panel for the Code Agent VS Code extension.
 *
 * Provides a chat interface with:
 * - Streaming markdown + code block rendering
 * - Expandable tool call visualization
 * - Diff display with accept/reject buttons
 * - Message history with thread management
 */

import * as vscode from 'vscode';
import { randomUUID } from 'crypto';

// ---------------------------------------------------------------------------
// Types
// ---------------------------------------------------------------------------

export interface ChatMessage {
  id: string;
  role: 'user' | 'assistant' | 'tool' | 'system';
  content: string;
  isStreaming?: boolean;
  toolCall?: {
    id: string;
    name: string;
    arguments: string;
    result?: {
      output?: string;
      error?: string;
    };
  };
  timestamp: number;
}

export interface DiffAction {
  diffId: string;
  action: 'accept' | 'reject';
}

// ---------------------------------------------------------------------------
// ChatWebviewProvider
// ---------------------------------------------------------------------------

export class ChatWebviewProvider implements vscode.WebviewViewProvider {
  public static readonly viewType = 'code-agent.chatView';
  private _view?: vscode.WebviewView;

  constructor(
    private readonly extensionUri: vscode.Uri,
    private readonly messageSender: (message: {
      type: string;
      [key: string]: unknown;
    }) => void,
  ) {}

  /** Called by VS Code when the webview is created or revealed. */
  resolveWebviewView(
    webviewView: vscode.WebviewView,
    _context: vscode.WebviewViewResolveContext,
    _token: vscode.CancellationToken,
  ): void {
    this._view = webviewView;

    webviewView.webview.options = {
      enableScripts: true,
      localResourceRoots: [this.extensionUri],
    };

    webviewView.webview.html = this.getHtmlContent(webviewView.webview);

    // Handle messages from the webview
    webviewView.webview.onDidReceiveMessage((msg) => {
      switch (msg.type) {
        case 'sendMessage':
          this.messageSender(msg);
          break;
        case 'acceptDiff':
          this.messageSender({ type: 'acceptDiff', diffId: msg.diffId });
          break;
        case 'rejectDiff':
          this.messageSender({ type: 'rejectDiff', diffId: msg.diffId });
          break;
        case 'newThread':
          this.messageSender({ type: 'newThread' });
          break;
        case 'forkThread':
          this.messageSender({ type: 'forkThread' });
          break;
      }
    });
  }

  /** Send a chat message to the webview for display. */
  postMessage(msg: {
    type: string;
    [key: string]: unknown;
  }): void {
    this._view?.webview.postMessage(msg);
  }

  /** Add a new message to the chat. */
  addMessage(message: ChatMessage): void {
    this._view?.webview.postMessage({
      type: 'addMessage',
      message,
    });
  }

  /** Update an existing message (for streaming). */
  updateMessage(id: string, content: string, isStreaming = false): void {
    this._view?.webview.postMessage({
      type: 'updateMessage',
      id,
      content,
      isStreaming,
    });
  }

  /** Add a tool call result to a message. */
  addToolResult(messageId: string, toolCallId: string, result: { output?: string; error?: string }): void {
    this._view?.webview.postMessage({
      type: 'addToolResult',
      messageId,
      toolCallId,
      result,
    });
  }

  /** Show a diff block for accept/reject. */
  showDiff(diffId: string, fileName: string, hunks: unknown[]): void {
    this._view?.webview.postMessage({
      type: 'showDiff',
      diffId,
      fileName,
      hunks,
    });
  }

  /** Clear the chat history. */
  clearChat(): void {
    this._view?.webview.postMessage({ type: 'clearChat' });
  }

  /** Set loading/processing state. */
  setProcessing(processing: boolean): void {
    this._view?.webview.postMessage({ type: 'setProcessing', processing });
  }

  // -----------------------------------------------------------------------
  // HTML content
  // -----------------------------------------------------------------------

  private getHtmlContent(webview: vscode.Webview): string {
    // Use a nonce to allow inline scripts
    const nonce = randomUUID();

    return `<!DOCTYPE html>
<html lang="en">
<head>
  <meta charset="UTF-8">
  <meta name="viewport" content="width=device-width, initial-scale=1.0">
  <meta http-equiv="Content-Security-Policy" content="default-src 'none'; style-src ${webview.cspSource} 'unsafe-inline'; script-src 'nonce-${nonce}'; img-src ${webview.cspSource} https:; font-src ${webview.cspSource};">
  <title>Code Agent Chat</title>
  <style>
    * { box-sizing: border-box; margin: 0; padding: 0; }
    html, body { height: 100%; font-family: var(--vscode-font-family); font-size: var(--vscode-font-size); color: var(--vscode-foreground); background: var(--vscode-editor-background); }
    body { display: flex; flex-direction: column; overflow: hidden; }

    /* --- Header --- */
    .chat-header {
      padding: 10px 16px;
      border-bottom: 1px solid var(--vscode-panel-border);
      display: flex;
      align-items: center;
      justify-content: space-between;
      flex-shrink: 0;
    }
    .chat-header h2 { font-size: 14px; font-weight: 600; }
    .chat-header .actions { display: flex; gap: 4px; }
    .chat-header button {
      background: none; border: 1px solid var(--vscode-panel-border);
      color: var(--vscode-foreground); padding: 4px 8px; border-radius: 4px;
      cursor: pointer; font-size: 12px;
    }
    .chat-header button:hover { background: var(--vscode-toolbar-hoverBackground); }

    /* --- Messages area --- */
    .messages { flex: 1; overflow-y: auto; padding: 12px 16px; display: flex; flex-direction: column; gap: 12px; }
    .messages::-webkit-scrollbar { width: 6px; }
    .messages::-webkit-scrollbar-thumb { background: var(--vscode-scrollbarSlider-background); border-radius: 3px; }

    /* --- Message bubbles --- */
    .message { max-width: 100%; animation: fadeIn 0.2s ease; }
    .message-header { font-size: 11px; font-weight: 600; margin-bottom: 4px; opacity: 0.7; }
    .message-content { line-height: 1.5; word-wrap: break-word; }

    .message.user { align-self: flex-end; }
    .message.user .message-content {
      background: var(--vscode-button-background); color: var(--vscode-button-foreground);
      padding: 8px 12px; border-radius: 12px 12px 2px 12px; max-width: 85%;
    }

    .message.assistant { align-self: flex-start; width: 100%; }
    .message.assistant .message-content {
      padding: 8px 0; max-width: 100%;
    }

    .message.system { align-self: center; }
    .message.system .message-content {
      font-size: 11px; opacity: 0.6; font-style: italic; text-align: center;
    }

    .message.tool { align-self: flex-start; width: 100%; }

    /* --- Tool call --- */
    .tool-call {
      border: 1px solid var(--vscode-panel-border); border-radius: 6px;
      margin-top: 8px; overflow: hidden;
    }
    .tool-call-header {
      padding: 6px 10px; background: var(--vscode-editor-lineHighlightBackground);
      display: flex; align-items: center; justify-content: space-between;
      cursor: pointer; font-size: 12px;
    }
    .tool-call-header .name { font-weight: 600; color: var(--vscode-symbolIcon-methodForeground); }
    .tool-call-body { padding: 8px 10px; display: none; font-size: 12px; }
    .tool-call-body.expanded { display: block; }
    .tool-call-args { background: var(--vscode-textCodeBlock-background); padding: 8px; border-radius: 4px; font-family: var(--vscode-editor-font-family); font-size: 11px; overflow-x: auto; margin-bottom: 6px; }
    .tool-call-result { background: var(--vscode-textCodeBlock-background); padding: 8px; border-radius: 4px; font-family: var(--vscode-editor-font-family); font-size: 11px; overflow-x: auto; max-height: 200px; overflow-y: auto; margin-top: 6px; }
    .tool-call-result.error { border-left: 2px solid var(--vscode-errorForeground); }
    .tool-call-result.success { border-left: 2px solid var(--vscode-testing-iconPassed); }

    /* --- Code blocks --- */
    .code-block {
      background: var(--vscode-textCodeBlock-background);
      border-radius: 6px; margin: 8px 0; overflow: hidden;
    }
    .code-block-header {
      padding: 4px 10px; background: var(--vscode-editor-lineHighlightBackground);
      font-size: 11px; display: flex; justify-content: space-between; align-items: center;
    }
    .code-block-body { padding: 10px; overflow-x: auto; }
    .code-block-body pre { margin: 0; font-family: var(--vscode-editor-font-family); font-size: 12px; line-height: 1.5; white-space: pre-wrap; }
    .code-block button {
      background: var(--vscode-button-secondaryBackground);
      color: var(--vscode-button-secondaryForeground);
      border: none; padding: 2px 6px; border-radius: 3px; cursor: pointer; font-size: 11px;
    }
    .code-block button:hover { background: var(--vscode-button-secondaryHoverBackground); }

    /* --- Inline code --- */
    code:not(pre code) {
      background: var(--vscode-textCodeBlock-background);
      padding: 2px 4px; border-radius: 3px; font-family: var(--vscode-editor-font-family); font-size: 0.9em;
    }

    /* --- Diff block --- */
    .diff-block {
      border: 1px solid var(--vscode-panel-border); border-radius: 6px;
      margin-top: 8px; overflow: hidden;
    }
    .diff-block-header {
      padding: 6px 10px; background: var(--vscode-editor-lineHighlightBackground);
      display: flex; align-items: center; justify-content: space-between;
      font-size: 12px;
    }
    .diff-block-body { max-height: 300px; overflow-y: auto; font-family: var(--vscode-editor-font-family); font-size: 11px; }
    .diff-line { padding: 2px 10px; white-space: pre; }
    .diff-line.added { background: var(--vscode-diffEditor-insertedTextBackground); color: var(--vscode-diffEditor-insertedTextColor); }
    .diff-line.removed { background: var(--vscode-diffEditor-removedTextBackground); color: var(--vscode-diffEditor-removedTextColor); }
    .diff-block-actions { padding: 6px 10px; display: flex; gap: 6px; border-top: 1px solid var(--vscode-panel-border); }
    .diff-block-actions button {
      padding: 4px 10px; border: none; border-radius: 4px; cursor: pointer; font-size: 12px; font-weight: 500;
    }
    .btn-accept { background: var(--vscode-testing-iconPassed); color: white; }
    .btn-accept:hover { opacity: 0.85; }
    .btn-reject { background: var(--vscode-testing-iconFailed); color: white; }
    .btn-reject:hover { opacity: 0.85; }

    /* --- Input area --- */
    .input-area {
      padding: 10px 16px; border-top: 1px solid var(--vscode-panel-border);
      display: flex; gap: 8px; flex-shrink: 0;
    }
    .input-area textarea {
      flex: 1; resize: none; background: var(--vscode-input-background);
      color: var(--vscode-input-foreground); border: 1px solid var(--vscode-input-border);
      border-radius: 6px; padding: 8px 10px; font-family: var(--vscode-font-family);
      font-size: var(--vscode-font-size); outline: none; min-height: 36px; max-height: 120px;
    }
    .input-area textarea:focus { border-color: var(--vscode-focusBorder); }
    .input-area button {
      padding: 8px 14px; background: var(--vscode-button-background);
      color: var(--vscode-button-foreground); border: none; border-radius: 6px;
      cursor: pointer; font-size: 13px; font-weight: 500; white-space: nowrap;
    }
    .input-area button:hover { background: var(--vscode-button-hoverBackground); }
    .input-area button:disabled { opacity: 0.5; cursor: not-allowed; }

    /* --- Typing indicator --- */
    .typing-indicator { padding: 6px 0; font-size: 11px; opacity: 0.7; font-style: italic; }

    /* --- Streaming cursor --- */
    .streaming-cursor::after { content: '▌'; animation: blink 1s infinite; }
    @keyframes blink { 0%, 50% { opacity: 1; } 51%, 100% { opacity: 0; } }
    @keyframes fadeIn { from { opacity: 0; transform: translateY(4px); } to { opacity: 1; transform: translateY(0); } }

    /* --- Feedback widget --- */
    .feedback-widget {
      border: 1px solid var(--vscode-panel-border); border-radius: 6px;
      margin: 8px 0; padding: 10px; background: var(--vscode-editor-lineHighlightBackground);
    }
    .feedback-widget .fb-header { font-size: 12px; font-weight: 600; margin-bottom: 8px; }
    .feedback-widget .fb-rating { display: flex; align-items: center; gap: 8px; margin-bottom: 8px; }
    .feedback-widget .fb-rating button {
      font-size: 20px; padding: 4px 8px; border: 1px solid var(--vscode-panel-border);
      border-radius: 4px; cursor: pointer; background: transparent;
      transition: background 0.15s;
    }
    .feedback-widget .fb-rating button:hover { background: var(--vscode-toolbar-hoverBackground); }
    .feedback-widget .fb-rating button.selected { background: var(--vscode-button-background); }
    .feedback-widget .fb-rating-label { font-size: 11px; opacity: 0.7; }
    .feedback-widget .fb-fields { display: flex; flex-direction: column; gap: 6px; margin-bottom: 8px; }
    .feedback-widget .fb-comment {
      background: var(--vscode-input-background); color: var(--vscode-input-foreground);
      border: 1px solid var(--vscode-input-border); border-radius: 4px;
      padding: 6px 8px; font-family: var(--vscode-font-family); font-size: 12px; resize: vertical;
    }
    .feedback-widget .fb-tags {
      background: var(--vscode-input-background); color: var(--vscode-input-foreground);
      border: 1px solid var(--vscode-input-border); border-radius: 4px;
      padding: 4px 8px; font-family: var(--vscode-font-family); font-size: 12px;
    }
    .feedback-widget .fb-actions { display: flex; gap: 6px; }
    .feedback-widget .fb-actions button {
      padding: 4px 12px; border: none; border-radius: 4px; cursor: pointer; font-size: 12px; font-weight: 500;
    }
    .feedback-widget .fb-submit {
      background: var(--vscode-button-background); color: var(--vscode-button-foreground);
    }
    .feedback-widget .fb-submit:hover { background: var(--vscode-button-hoverBackground); }
    .feedback-widget .fb-submit:disabled { opacity: 0.5; cursor: not-allowed; }
    .feedback-widget .fb-skip {
      background: var(--vscode-button-secondaryBackground); color: var(--vscode-button-secondaryForeground);
    }
    .feedback-widget .fb-skip:hover { background: var(--vscode-button-secondaryHoverBackground); }

    /* --- Empty state --- */
    .empty-state {
      flex: 1; display: flex; flex-direction: column; align-items: center;
      justify-content: center; opacity: 0.5; gap: 8px; padding: 20px;
    }
    .empty-state .icon { font-size: 32px; }
    .empty-state .text { font-size: 13px; text-align: center; }
  </style>
</head>
<body>
  <div class="chat-header">
    <h2>Code Agent</h2>
    <div class="actions">
      <button onclick="newThread()" title="New Thread">+ New</button>
      <button onclick="clearChat()" title="Clear">Clear</button>
    </div>
  </div>

  <div class="messages" id="messages">
    <div class="empty-state" id="emptyState">
      <div class="icon">$(hubot)</div>
      <div class="text">Ask the AI Coding Agent to help with your code.<br/>Start a conversation or select code and use "Fix This".</div>
    </div>
  </div>

  <div id="feedbackContainer"></div>

  <div class="input-area">
    <textarea id="input" placeholder="Ask anything about your code..." rows="1" onkeydown="handleKeyDown(event)"></textarea>
    <button id="sendBtn" onclick="sendMessage()" disabled>Send</button>
  </div>

  <script nonce="${nonce}">
    const vscode = acquireVsCodeApi();
    const messagesEl = document.getElementById('messages');
    const inputEl = document.getElementById('input');
    const sendBtn = document.getElementById('sendBtn');
    const emptyStateEl = document.getElementById('emptyState');
    let processing = false;
    let streamingMessageId = null;

    // --- Feedback widget ---
    const feedbackWidget = createFeedbackWidget();
    document.getElementById('feedbackContainer').appendChild(feedbackWidget);

    function createFeedbackWidget() {
      const div = document.createElement('div');
      div.className = 'feedback-widget';
      div.style.display = 'none';
      div.innerHTML = ''
        + '<div class="fb-header">Rate this response</div>'
        + '<div class="fb-rating">'
        + '  <button class="fb-thumbs-up" title="Thumbs Up">👍</button>'
        + '  <button class="fb-thumbs-down" title="Thumbs Down">👎</button>'
        + '  <span class="fb-rating-label">Select rating above</span>'
        + '</div>'
        + '<div class="fb-fields">'
        + '  <textarea class="fb-comment" placeholder="Optional comment..." rows="2"></textarea>'
        + '  <input class="fb-tags" type="text" placeholder="Optional tags (comma-separated)" />'
        + '</div>'
        + '<div class="fb-actions">'
        + '  <button class="fb-submit" disabled>Submit</button>'
        + '  <button class="fb-skip">Skip</button>'
        + '</div>';

      let rating = null;
      const thumbsUp = div.querySelector('.fb-thumbs-up');
      const thumbsDown = div.querySelector('.fb-thumbs-down');
      const submitBtn = div.querySelector('.fb-submit');
      const skipBtn = div.querySelector('.fb-skip');
      const commentEl = div.querySelector('.fb-comment');
      const tagsEl = div.querySelector('.fb-tags');
      const labelEl = div.querySelector('.fb-rating-label');

      let currentSessionId = '';
      let currentTurnId = '';
      let currentContext = {};

      function updateButtons() {
        thumbsUp.classList.toggle('selected', rating === 'thumbs_up');
        thumbsDown.classList.toggle('selected', rating === 'thumbs_down');
        labelEl.textContent = rating === 'thumbs_up' ? '👍 Thumbs Up'
          : rating === 'thumbs_down' ? '👎 Thumbs Down'
          : 'Select rating above';
        submitBtn.disabled = rating === null;
      }

      thumbsUp.addEventListener('click', () => { rating = 'thumbs_up'; updateButtons(); });
      thumbsDown.addEventListener('click', () => { rating = 'thumbs_down'; updateButtons(); });

      submitBtn.addEventListener('click', () => {
        if (!rating) return;
        const tags = tagsEl.value.split(',').map(function(t) { return t.trim(); }).filter(function(t) { return t.length > 0; });
        vscode.postMessage({
          type: 'submitFeedback',
          sessionId: currentSessionId,
          turnId: currentTurnId,
          rating: rating,
          comment: commentEl.value.trim(),
          tags: tags,
          contextSnapshot: currentContext,
        });
        div.style.display = 'none';
      });

      skipBtn.addEventListener('click', () => {
        vscode.postMessage({ type: 'skipFeedback' });
        div.style.display = 'none';
      });

      div.showFeedback = function(sessionId, turnId, context) {
        currentSessionId = sessionId;
        currentTurnId = turnId;
        currentContext = context || {};
        rating = null;
        commentEl.value = '';
        tagsEl.value = '';
        updateButtons();
        div.style.display = 'block';
      };

      return div;
    }

    inputEl.addEventListener('input', () => {
      sendBtn.disabled = !inputEl.value.trim() || processing;
    });

    // --- Markdown rendering ---
    function renderMarkdown(text) {
      if (!text) return '';
      // Escape HTML
      var html = text
        .replace(/&/g, '&amp;').replace(/</g, '&lt;').replace(/>/g, '&gt;');

      // Code blocks - use RegExp constructor to avoid escaping issues
      html = html.replace(new RegExp('\x60\x60\x60(\\w*)\\n([\\s\\S]*?)\x60\x60\x60', 'g'), function(match, lang, code) {
        return '<div class="code-block">'
          + (lang ? '<div class="code-block-header"><span>' + lang + '</span><button onclick="copyCode(this)">Copy</button></div>' : '')
          + '<div class="code-block-body"><pre>' + code + '</pre></div>'
          + '</div>';
      });

      // Inline code
      html = html.replace(new RegExp('\x60([^\x60]+)\x60', 'g'), '<code>$1</code>');

      // Bold (**...**)
      html = html.replace(/\\*\\*(.+?)\\*\\*/g, '<strong>$1</strong>');

      // Italic (*...*)
      html = html.replace(/\\*(.+?)\\*/g, '<em>$1</em>');

      // Line breaks
      html = html.replace(/\\n/g, '<br/>');

      return html;
    }

    function copyCode(btn) {
      const pre = btn.closest('.code-block').querySelector('pre');
      navigator.clipboard.writeText(pre.textContent);
      btn.textContent = 'Copied!';
      setTimeout(() => { btn.textContent = 'Copy'; }, 1500);
    }

    // --- Message rendering ---
    function createMessageEl(msg) {
      const el = document.createElement('div');
      el.className = 'message ' + msg.role;
      el.id = 'msg-' + msg.id;

      if (msg.role === 'system') {
        el.innerHTML = '<div class="message-content">' + msg.content + '</div>';
        return el;
      }

      const header = '<div class="message-header">' + (msg.role === 'user' ? 'You' : 'Agent') + '</div>';
      const contentClass = msg.isStreaming ? 'message-content streaming-cursor' : 'message-content';
      const content = '<div class="' + contentClass + '">' + renderMarkdown(msg.content) + '</div>';

      el.innerHTML = header + content;

      if (msg.toolCall) {
        el.appendChild(createToolCallEl(msg.toolCall, msg.id));
      }

      return el;
    }

    function createToolCallEl(tc, msgId) {
      const el = document.createElement('div');
      el.className = 'tool-call';

      const resultIcon = tc.result ? (tc.result.error ? '✗' : '✓') : '⟳';
      const resultClass = tc.result ? (tc.result.error ? 'error' : 'success') : '';

      el.innerHTML = '<div class="tool-call-header" onclick="toggleToolCall(this)">'
        + '<span>' + resultIcon + ' <span class="name">' + tc.name + '</span></span>'
        + '<span>' + (tc.id ? tc.id.slice(0, 8) : '') + ' ▾</span>'
        + '</div>'
        + '<div class="tool-call-body' + (resultClass ? ' ' + resultClass : '') + '">'
        + '<div class="tool-call-args"><strong>Arguments:</strong> ' + tc.arguments + '</div>'
        + (tc.result ? '<div class="tool-call-result ' + (tc.result.error ? 'error' : 'success') + '"><strong>' + (tc.result.error ? 'Error:' : 'Result:') + '</strong> ' + (tc.result.output || tc.result.error || '(empty)') + '</div>' : '')
        + '</div>';

      return el;
    }

    function toggleToolCall(header) {
      const body = header.nextElementSibling;
      body.classList.toggle('expanded');
    }

    function renderDiffHunk(hunk) {
      let html = '';
      if (hunk.oldText) {
        for (const line of hunk.oldText.split('\\n')) {
          html += '<div class="diff-line removed">- ' + line + '</div>';
        }
      }
      if (hunk.newText) {
        for (const line of hunk.newText.split('\\n')) {
          html += '<div class="diff-line added">+ ' + line + '</div>';
        }
      }
      return html;
    }

    // --- Message handling from extension ---
    window.addEventListener('message', event => {
      const msg = event.data;
      switch (msg.type) {
        case 'addMessage':
          addOrUpdateMessage(msg.message);
          break;
        case 'updateMessage':
          updateStreamingMessage(msg.id, msg.content, msg.isStreaming);
          break;
        case 'addToolResult':
          addToolResultToMessage(msg.messageId, msg.toolCallId, msg.result);
          break;
        case 'showDiff':
          showDiffBlock(msg.diffId, msg.fileName, msg.hunks);
          break;
        case 'clearChat':
          clearAllMessages();
          break;
        case 'setProcessing':
          setProcessing(msg.processing);
          break;
        case 'showFeedback':
          feedbackWidget.showFeedback(msg.sessionId, msg.turnId, msg.contextSnapshot || {});
          break;
      }
    });

    function addOrUpdateMessage(message) {
      emptyStateEl.style.display = 'none';
      const existing = document.getElementById('msg-' + message.id);
      if (existing) {
        existing.replaceWith(createMessageEl(message));
      } else {
        messagesEl.appendChild(createMessageEl(message));
      }
      scrollToBottom();
    }

    function updateStreamingMessage(id, content, isStreaming) {
      emptyStateEl.style.display = 'none';
      const el = document.getElementById('msg-' + id);
      if (!el) {
        // Create a new streaming message
        const msg = { id, role: 'assistant', content, isStreaming, timestamp: Date.now() };
        messagesEl.appendChild(createMessageEl(msg));
        streamingMessageId = id;
        scrollToBottom();
        return;
      }
      const contentEl = el.querySelector('.message-content');
      if (contentEl) {
        contentEl.innerHTML = renderMarkdown(content);
        if (isStreaming) {
          contentEl.classList.add('streaming-cursor');
        } else {
          contentEl.classList.remove('streaming-cursor');
        }
      }
      streamingMessageId = isStreaming ? id : null;
      scrollToBottom();
    }

    function addToolResultToMessage(messageId, toolCallId, result) {
      const el = document.getElementById('msg-' + messageId);
      if (!el) return;
      const existing = el.querySelector('.tool-call');
      if (existing) {
        const header = existing.querySelector('.tool-call-header span:first-child');
        if (header) {
          header.textContent = result.error ? '✗' + header.textContent.slice(1) : '✓' + header.textContent.slice(1);
        }
        const body = existing.querySelector('.tool-call-body');
        if (body) {
          const resultEl = document.createElement('div');
          resultEl.className = 'tool-call-result ' + (result.error ? 'error' : 'success');
          resultEl.innerHTML = '<strong>' + (result.error ? 'Error:' : 'Result:') + '</strong> ' + (result.output || result.error || '(empty)');
          body.appendChild(resultEl);
          body.classList.add(result.error ? 'error' : 'success');
        }
      }
    }

    function showDiffBlock(diffId, fileName, hunks) {
      const el = document.createElement('div');
      el.className = 'message assistant';
      el.id = 'diff-' + diffId;

      let diffHtml = '<div class="diff-block">'
        + '<div class="diff-block-header"><span>File: ' + fileName + '</span></div>'
        + '<div class="diff-block-body">';

      for (const hunk of hunks) {
        diffHtml += renderDiffHunk(hunk);
      }

      diffHtml += '</div>'
        + '<div class="diff-block-actions">'
        + '<button class="btn-accept" onclick="acceptDiff(\\'' + diffId + '\\')">Accept</button>'
        + '<button class="btn-reject" onclick="rejectDiff(\\'' + diffId + '\\')">Reject</button>'
        + '</div>'
        + '</div>';

      el.innerHTML = '<div class="message-header">Agent</div><div class="message-content">' + diffHtml + '</div>';
      messagesEl.appendChild(el);
      emptyStateEl.style.display = 'none';
      scrollToBottom();
    }

    function clearAllMessages() {
      while (messagesEl.firstChild) {
        if (messagesEl.firstChild === emptyStateEl) break;
        messagesEl.removeChild(messagesEl.firstChild);
      }
      emptyStateEl.style.display = 'flex';
      streamingMessageId = null;
    }

    function setProcessing(val) {
      processing = val;
      sendBtn.disabled = !inputEl.value.trim() || processing;
      if (processing) {
        inputEl.placeholder = 'Agent is thinking...';
      } else {
        inputEl.placeholder = 'Ask anything about your code...';
        inputEl.focus();
      }
    }

    function scrollToBottom() {
      messagesEl.scrollTop = messagesEl.scrollHeight;
    }

    // --- User actions ---
    function sendMessage() {
      const text = inputEl.value.trim();
      if (!text || processing) return;

      vscode.postMessage({ type: 'sendMessage', content: text });
      inputEl.value = '';
      sendBtn.disabled = true;
    }

    function handleKeyDown(event) {
      if (event.key === 'Enter' && !event.shiftKey) {
        event.preventDefault();
        sendMessage();
      }
    }

    function newThread() {
      vscode.postMessage({ type: 'newThread' });
    }

    function clearChat() {
      vscode.postMessage({ type: 'clearChat' });
    }

    function acceptDiff(diffId) {
      vscode.postMessage({ type: 'acceptDiff', diffId });
      const el = document.getElementById('diff-' + diffId);
      if (el) el.remove();
    }

    function rejectDiff(diffId) {
      vscode.postMessage({ type: 'rejectDiff', diffId });
      const el = document.getElementById('diff-' + diffId);
      if (el) el.remove();
    }
  </script>
</body>
</html>`;
  }
}
