/**
 * Code Agent VS Code Extension — Entry Point.
 *
 * Activation lifecycle:
 * 1. Register commands (code-agent.start, .chat, .fix, .stop)
 * 2. Start App Server as child process
 * 3. JSON-RPC initialize handshake
 * 4. Create TreeView for sessions
 * 5. Set up chat webview
 * 6. Set up status bar
 */

import * as vscode from 'vscode';
import { randomUUID } from 'crypto';
import { AppServerClient, ConnectionState } from './appServer';
import type {
  Message,
  ResponseEvent,
  CreateThreadResult,
  SubmitTurnResult,
  GetThreadResult,
  ListThreadsResult,
} from './appServer/types';
import { StatusBarWidget } from './statusBar';
import { DiffManager } from './editor';
import { ChatWebviewProvider, ChatMessage } from './chat';

// ---------------------------------------------------------------------------
// Constants
// ---------------------------------------------------------------------------

const DEFAULT_THREAD_ID = 'default';

// ---------------------------------------------------------------------------
// Activation
// ---------------------------------------------------------------------------

export async function activate(context: vscode.ExtensionContext): Promise<void> {
  const config = vscode.workspace.getConfiguration('codeAgent');
  const appServerPath =
    config.get<string>('appServerPath') || getDefaultAppServerPath(context);

  // ── State ──────────────────────────────────────────────────────────
  const client = new AppServerClient({
    binaryPath: appServerPath,
    args: getAppServerArgs(config),
    reconnectDelay: 2000,
    maxReconnectAttempts: 10,
  });

  const statusBar = new StatusBarWidget();
  const diffManager = new DiffManager();
  const threadDataProvider = new ThreadDataProvider(client);

  // Chat webview provider
  let chatProvider: ChatWebviewProvider | undefined;

  // Track streaming state
  let currentAssistantMsgId: string | undefined;
  let streamingContent = '';

  context.subscriptions.push(statusBar);
  context.subscriptions.push(diffManager);

  // ── Initialize App Server connection ───────────────────────────────
  client.on('connectionChange', (state: ConnectionState) => {
    statusBar.setConnectionState(state);
  });

  client.on('event', (event: ResponseEvent) => {
    handleServerEvent(event);
  });

  client.on('error', (err: Error) => {
    vscode.window.showWarningMessage(`Code Agent: ${err.message}`);
  });

  // ── Register TreeView ──────────────────────────────────────────────
  const treeView = vscode.window.createTreeView('code-agent.sessions', {
    treeDataProvider: threadDataProvider,
    showCollapseAll: false,
  });
  context.subscriptions.push(treeView);

  // ── Register chat webview provider ─────────────────────────────────
  chatProvider = new ChatWebviewProvider(
    context.extensionUri,
    handleWebviewMessage,
  );

  const webviewRegistration = vscode.window.registerWebviewViewProvider(
    ChatWebviewProvider.viewType,
    chatProvider,
  );
  context.subscriptions.push(webviewRegistration);

  // ── Register commands ──────────────────────────────────────────────
  context.subscriptions.push(
    vscode.commands.registerCommand('code-agent.start', () =>
      startAgent(client, statusBar, config),
    ),
  );

  context.subscriptions.push(
    vscode.commands.registerCommand('code-agent.chat', () => {
      vscode.commands.executeCommand(
        'workbench.view.extension.code-agent',
      );
    }),
  );

  context.subscriptions.push(
    vscode.commands.registerCommand('code-agent.fix', () =>
      handleFixCommand(client, diffManager, chatProvider),
    ),
  );

  context.subscriptions.push(
    vscode.commands.registerCommand('code-agent.stop', () => {
      client.disconnect();
      statusBar.setConnectionState('disconnected');
    }),
  );

  context.subscriptions.push(
    vscode.commands.registerCommand('code-agent.newThread', async () => {
      const threadId = await vscode.window.showInputBox({
        prompt: 'Enter a name for the new thread',
        placeHolder: 'my-thread',
      });
      if (threadId) {
        await createThread(client, threadId);
        threadDataProvider.refresh();
      }
    }),
  );

  context.subscriptions.push(
    vscode.commands.registerCommand(
      'code-agent.forkThread',
      async (item?: ThreadItem) => {
        const sourceId = item?.threadId || DEFAULT_THREAD_ID;
        const newId = `${sourceId}-fork-${randomUUID().slice(0, 6)}`;
        try {
          await client.request('threads/fork', {
            thread_id: sourceId,
            new_thread_id: newId,
          });
          threadDataProvider.refresh();
          vscode.window.showInformationMessage(
            `Thread forked: ${newId}`,
          );
        } catch (err) {
          vscode.window.showErrorMessage(
            `Fork failed: ${(err as Error).message}`,
          );
        }
      },
    ),
  );

  context.subscriptions.push(
    vscode.commands.registerCommand(
      'code-agent.archiveThread',
      async (item?: ThreadItem) => {
        const threadId = item?.threadId;
        if (!threadId) return;
        try {
          await client.request('threads/archive', { thread_id: threadId });
          threadDataProvider.refresh();
          vscode.window.showInformationMessage(
            `Thread archived: ${threadId}`,
          );
        } catch (err) {
          vscode.window.showErrorMessage(
            `Archive failed: ${(err as Error).message}`,
          );
        }
      },
    ),
  );

  // ── Auto-start ─────────────────────────────────────────────────────
  if (config.get<boolean>('autoStart', true)) {
    await startAgent(client, statusBar, config);
  }

  // ── Webview message handler ────────────────────────────────────────
  async function handleWebviewMessage(msg: {
    type: string;
    [key: string]: unknown;
  }): Promise<void> {
    switch (msg.type) {
      case 'sendMessage':
        await handleSendMessage(client, chatProvider!, msg.content as string);
        break;
      case 'acceptDiff':
        await diffManager.acceptDiff(msg.diffId as string);
        break;
      case 'rejectDiff':
        diffManager.clearDiff(msg.diffId as string);
        break;
      case 'newThread':
        vscode.commands.executeCommand('code-agent.newThread');
        break;
      case 'forkThread':
        vscode.commands.executeCommand('code-agent.forkThread');
        break;
      case 'submitFeedback':
        await handleSubmitFeedback(client, msg);
        break;
      case 'skipFeedback':
        // No action needed — feedback was skipped
        break;
    }
  }

  // ── Server event handler ───────────────────────────────────────────
  function handleServerEvent(event: ResponseEvent): void {
    // Extract event type
    const key = Object.keys(event)[0] as keyof ResponseEvent;

    switch (key) {
      case 'turn_started': {
        const data = (event as { turn_started: { turn_id: string } }).turn_started;
        if (chatProvider) {
          chatProvider.postMessage({
            type: 'systemMessage',
            content: `Turn started: ${data.turn_id.slice(0, 8)}...`,
          });
        }
        break;
      }

      case 'agent_message_delta': {
        const data = (event as { agent_message_delta: { content: string } }).agent_message_delta;
        streamingContent += data.content;
        if (!currentAssistantMsgId) {
          currentAssistantMsgId = `msg-${randomUUID()}`;
        }
        if (chatProvider) {
          chatProvider.updateMessage(currentAssistantMsgId, streamingContent, true);
        }
        break;
      }

      case 'tool_call_begin': {
        const data = (event as { tool_call_begin: { tool_call: { id: string; name: string; arguments: Record<string, unknown> } } }).tool_call_begin;
        if (chatProvider && currentAssistantMsgId) {
          chatProvider.addMessage({
            id: `tool-${data.tool_call.id}`,
            role: 'tool',
            content: `Calling: ${data.tool_call.name}`,
            toolCall: {
              id: data.tool_call.id,
              name: data.tool_call.name,
              arguments: JSON.stringify(data.tool_call.arguments, null, 2),
            },
            timestamp: Date.now(),
          });
        }
        break;
      }

      case 'tool_call_end': {
        const data = (event as { tool_call_end: { tool_call_id: string; result: { output?: string; error?: string } } }).tool_call_end;
        if (chatProvider && currentAssistantMsgId) {
          chatProvider.addToolResult(
            `tool-${data.tool_call_id}`,
            data.tool_call_id,
            {
              output: data.result.output,
              error: data.result.error,
            },
          );
        }
        break;
      }

      case 'turn_complete': {
        const data = (event as { turn_complete: { turn_id: string; final_message: Message } }).turn_complete;
        if (chatProvider && currentAssistantMsgId) {
          chatProvider.updateMessage(currentAssistantMsgId, streamingContent, false);
          currentAssistantMsgId = undefined;
          streamingContent = '';
          chatProvider.setProcessing(false);
          // Show feedback widget after each turn
          chatProvider.postMessage({
            type: 'showFeedback',
            sessionId: client.getSessionId?.(),
            turnId: data.turn_id,
            contextSnapshot: {
              turnCount: statusBar.getTurnCount?.(),
              promptTokens: statusBar.getPromptTokens?.(),
              completionTokens: statusBar.getCompletionTokens?.(),
            },
          });
        }
        threadDataProvider.refresh();
        break;
      }

      case 'error': {
        const data = (event as { error: { message: string } }).error;
        if (chatProvider) {
          chatProvider.addMessage({
            id: `error-${randomUUID()}`,
            role: 'system',
            content: `Error: ${data.message}`,
            timestamp: Date.now(),
          });
          chatProvider.setProcessing(false);
        }
        vscode.window.showErrorMessage(`Code Agent: ${data.message}`);
        break;
      }

      case 'token_usage': {
        const data = (event as { token_usage: { prompt_tokens: number; completion_tokens: number } }).token_usage;
        statusBar.setTokenUsage(data.prompt_tokens, data.completion_tokens);
        break;
      }
    }
  }
}

// ---------------------------------------------------------------------------
// Deactivation
// ---------------------------------------------------------------------------

export function deactivate(): void {
  // Cleanup is handled by context.subscriptions disposal
}

// ---------------------------------------------------------------------------
// Command handlers
// ---------------------------------------------------------------------------

async function startAgent(
  client: AppServerClient,
  statusBar: StatusBarWidget,
  config: vscode.WorkspaceConfiguration,
): Promise<void> {
  try {
    await client.connect();

    // JSON-RPC initialize handshake
    const initResult = await client.request('initialize', {
      protocol_version: '0.1.0',
      capabilities: {
        streaming: true,
        threads: true,
        forking: true,
        archiving: true,
      },
      client_info: {
        name: 'code-agent-vscode',
        version: '0.1.0',
      },
    });

    const result = initResult as { server_info?: { name?: string; version?: string }; capabilities?: Record<string, unknown> };
    statusBar.setModelName(
      (result as any)?.server_info?.name || 'code-agent-app-server',
    );

    // Send initialized notification
    client.sendNotification('notifications/initialized');

    // Create default thread
    const maxIterations = config.get<number>('maxIterations', 20);
    await client.request('threads/create', {
      thread_id: DEFAULT_THREAD_ID,
      system_instructions:
        'You are an AI coding assistant integrated into VS Code. Help the user write, edit, and understand code.',
      max_iterations: maxIterations,
    });

    statusBar.setConnectionState('connected');
  } catch (err) {
    statusBar.setConnectionState('disconnected');
    throw err;
  }
}

async function handleSendMessage(
  client: AppServerClient,
  chatProvider: ChatWebviewProvider,
  content: string,
): Promise<void> {
  if (!client.isConnected()) {
    vscode.window.showWarningMessage(
      'Code Agent is not connected. Run "Start Agent" first.',
    );
    return;
  }

  const userMsgId = `user-${randomUUID()}`;
  chatProvider.addMessage({
    id: userMsgId,
    role: 'user',
    content,
    timestamp: Date.now(),
  });

  chatProvider.setProcessing(true);

  try {
    const message: Message = {
      user_message: { content },
    };

    const editor = vscode.window.activeTextEditor;
    let contextMsg: Message | undefined;
    if (editor) {
      const selection = editor.document.getText(editor.selection);
      const filePath = editor.document.uri.fsPath;
      if (selection) {
        contextMsg = {
          user_message: {
            content: `Current file: ${filePath}\nSelected code:\n\`\`\`\n${selection}\n\`\`\``,
          },
        };
      } else if (editor.document.lineCount < 200) {
        const fullText = editor.document.getText();
        contextMsg = {
          user_message: {
            content: `Current file: ${filePath}\nFile content:\n\`\`\`\n${fullText}\n\`\`\``,
          },
        };
      }
    }

    const messages: Message[] = contextMsg ? [contextMsg, message] : [message];

    await client.request('threads/submitTurn', {
      thread_id: DEFAULT_THREAD_ID,
      messages,
    });

    // Response events will arrive as streaming events via the 'event' listener
  } catch (err) {
    chatProvider.setProcessing(false);
    vscode.window.showErrorMessage(
      `Failed to send message: ${(err as Error).message}`,
    );
  }
}

async function handleFixCommand(
  client: AppServerClient,
  diffManager: DiffManager,
  chatProvider?: ChatWebviewProvider,
): Promise<void> {
  const editor = vscode.window.activeTextEditor;
  if (!editor) {
    vscode.window.showWarningMessage('No active editor.');
    return;
  }

  const selection = editor.document.getText(editor.selection);
  const filePath = editor.document.uri.fsPath;

  if (!selection) {
    vscode.window.showWarningMessage(
      'Please select code to fix first.',
    );
    return;
  }

  if (!client.isConnected()) {
    vscode.window.showWarningMessage(
      'Code Agent is not connected. Run "Start Agent" first.',
    );
    return;
  }

  const fixPrompt = `Fix the following code. Explain what the issues are and provide the corrected code.\n\nFile: ${filePath}\n\n\`\`\`\n${selection}\n\`\`\``;

  if (chatProvider) {
    chatProvider.addMessage({
      id: `user-${randomUUID()}`,
      role: 'user',
      content: `Fix this code in ${filePath}:`,
      timestamp: Date.now(),
    });
    chatProvider.setProcessing(true);
  }

  try {
    const message: Message = {
      user_message: { content: fixPrompt },
    };

    await client.request('threads/submitTurn', {
      thread_id: DEFAULT_THREAD_ID,
      messages: [message],
    });
  } catch (err) {
    if (chatProvider) {
      chatProvider.setProcessing(false);
    }
    vscode.window.showErrorMessage(
      `Fix command failed: ${(err as Error).message}`,
    );
  }
}

async function createThread(
  client: AppServerClient,
  threadId: string,
): Promise<CreateThreadResult> {
  const config = vscode.workspace.getConfiguration('codeAgent');
  const maxIterations = config.get<number>('maxIterations', 20);

  const result = await client.request('threads/create', {
    thread_id: threadId,
    system_instructions:
      'You are an AI coding assistant integrated into VS Code.',
    max_iterations: maxIterations,
  });

  return result as CreateThreadResult;
}

// ---------------------------------------------------------------------------
// Thread TreeView
// ---------------------------------------------------------------------------

interface ThreadItem {
  threadId: string;
  label: string;
  sessionId?: string;
  turnCount?: number;
  status?: string;
}

class ThreadDataProvider implements vscode.TreeDataProvider<vscode.TreeItem> {
  private _onDidChangeTreeData = new vscode.EventEmitter<
    vscode.TreeItem | undefined | null | void
  >();
  readonly onDidChangeTreeData = this._onDidChangeTreeData.event;

  constructor(private readonly client: AppServerClient) {}

  refresh(): void {
    this._onDidChangeTreeData.fire();
  }

  getTreeItem(element: vscode.TreeItem): vscode.TreeItem {
    return element;
  }

  async getChildren(): Promise<vscode.TreeItem[]> {
    if (!this.client.isConnected()) {
      const item = new vscode.TreeItem(
        'Not connected',
        vscode.TreeItemCollapsibleState.None,
      );
      item.iconPath = new vscode.ThemeIcon('debug-disconnect');
      return [item];
    }

    try {
      const result = (await this.client.request('threads/list')) as ListThreadsResult;

      if (!result.threads || result.threads.length === 0) {
        const item = new vscode.TreeItem(
          'No threads',
          vscode.TreeItemCollapsibleState.None,
        );
        item.iconPath = new vscode.ThemeIcon('info');
        return [item];
      }

      const items: vscode.TreeItem[] = [];

      for (const threadId of result.threads) {
        try {
          const details = (await this.client.request('threads/get', {
            thread_id: threadId,
          })) as GetThreadResult;

          const label = threadId === DEFAULT_THREAD_ID ? 'Default' : threadId;
          const turns = details.turn_count ? ` (${details.turn_count} turns)` : '';
          const item = new vscode.TreeItem(
            `${label}${turns}`,
            vscode.TreeItemCollapsibleState.None,
          );
          item.iconPath = new vscode.ThemeIcon('comment-discussion');
          item.description = details.status || 'active';
          item.tooltip = `Thread: ${threadId}\nSession: ${details.session_id}\nTurns: ${details.turn_count}\nStatus: ${details.status}`;
          item.contextValue = 'thread';
          item.command = {
            command: 'code-agent.chat',
            title: 'Open Chat',
          };
          items.push(item);
        } catch {
          const item = new vscode.TreeItem(
            threadId,
            vscode.TreeItemCollapsibleState.None,
          );
          item.iconPath = new vscode.ThemeIcon('comment-discussion');
          item.contextValue = 'thread';
          items.push(item);
        }
      }

      return items;
    } catch {
      const item = new vscode.TreeItem(
        'Error loading threads',
        vscode.TreeItemCollapsibleState.None,
      );
      item.iconPath = new vscode.ThemeIcon('error');
      return [item];
    }
  }
}

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

async function handleSubmitFeedback(
  client: AppServerClient,
  msg: { [key: string]: unknown },
): Promise<void> {
  try {
    await client.request('feedback/submit', {
      session_id: msg.sessionId,
      turn_id: msg.turnId,
      rating: msg.rating,
      comment: msg.comment || '',
      tags: msg.tags || [],
      context_snapshot: msg.contextSnapshot || {},
    });
  } catch (err) {
    vscode.window.showErrorMessage(
      `Failed to submit feedback: ${(err as Error).message}`,
    );
  }
}

function getDefaultAppServerPath(context: vscode.ExtensionContext): string {
  // Check for a bundled binary first
  const platform = process.platform;
  const ext = platform === 'win32' ? '.exe' : '';
  const bundledPaths = [
    vscode.Uri.joinPath(context.extensionUri, 'bin', `code-agent-app-server${ext}`),
  ];

  // Fall back to PATH lookup
  const pathEnv = process.env.PATH || '';

  // On Windows, look for the cargo-built binary
  return `code-agent-app-server${ext}`;
}

function getAppServerArgs(config: vscode.WorkspaceConfiguration): string[] {
  const args: string[] = [];
  const modelName = config.get<string>('modelName');
  if (modelName) {
    args.push('--model', modelName);
  }
  return args;
}
