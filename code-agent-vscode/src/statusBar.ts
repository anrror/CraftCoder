/**
 * Status bar widget for the Code Agent VS Code extension.
 *
 * Displays:
 * - Connection indicator (connected/disconnected/connecting)
 * - Active model name
 * - Token usage (prompt/completion)
 */

import * as vscode from 'vscode';
import { ConnectionState } from './appServer';

export class StatusBarWidget {
  private readonly item: vscode.StatusBarItem;
  private connectionState: ConnectionState = 'disconnected';
  private modelName = '';
  private promptTokens = 0;
  private completionTokens = 0;

  constructor() {
    // Priority 100 places it near the end of the status bar (left side)
    this.item = vscode.window.createStatusBarItem(
      vscode.StatusBarAlignment.Left,
      100,
    );
    this.item.name = 'Code Agent';
    this.item.tooltip = 'Code Agent - Click to open chat';
    this.item.command = 'code-agent.chat';
    this.refresh();
    this.item.show();
  }

  /** Update connection state and refresh display. */
  setConnectionState(state: ConnectionState): void {
    this.connectionState = state;
    this.refresh();
  }

  /** Update active model name. */
  setModelName(name: string): void {
    this.modelName = name;
    this.refresh();
  }

  /** Update token usage counters. */
  setTokenUsage(promptTokens: number, completionTokens: number): void {
    this.promptTokens = promptTokens;
    this.completionTokens = completionTokens;
    this.refresh();
  }

  /** Get the current turn count (approximated from token usage). */
  getTurnCount(): number {
    // Approximate: each turn typically has some tokens
    return this.promptTokens > 0 ? Math.max(1, Math.floor(this.promptTokens / 500)) : 0;
  }

  /** Get prompt tokens. */
  getPromptTokens(): number {
    return this.promptTokens;
  }

  /** Get completion tokens. */
  getCompletionTokens(): number {
    return this.completionTokens;
  }

  /** Dispose of the status bar item. */
  dispose(): void {
    this.item.dispose();
  }

  // -----------------------------------------------------------------------
  // Private
  // -----------------------------------------------------------------------

  private refresh(): void {
    const icon = this.connectionIcon();
    const tokens = this.promptTokens + this.completionTokens;

    let text = `$(${icon}) Code Agent`;

    if (this.modelName && this.connectionState === 'connected') {
      text += ` · ${this.modelName}`;
    }

    if (tokens > 0) {
      text += ` · ${this.formatTokens(tokens)}`;
    }

    this.item.text = text;

    // Color based on state
    switch (this.connectionState) {
      case 'connected':
        this.item.backgroundColor = undefined;
        break;
      case 'connecting':
        this.item.backgroundColor = new vscode.ThemeColor(
          'statusBarItem.warningBackground',
        );
        break;
      case 'disconnected':
        this.item.backgroundColor = new vscode.ThemeColor(
          'statusBarItem.errorBackground',
        );
        break;
    }

    // Tooltip details
    const tooltip = new vscode.MarkdownString();
    tooltip.isTrusted = true;
    tooltip.supportHtml = true;
    tooltip.appendMarkdown(`**Code Agent**  \n\n`);
    tooltip.appendMarkdown(`- State: **${this.connectionState}**\n`);
    tooltip.appendMarkdown(`- Model: ${this.modelName || 'N/A'}\n`);
    tooltip.appendMarkdown(
      `- Prompt tokens: ${this.promptTokens.toLocaleString()}\n`,
    );
    tooltip.appendMarkdown(
      `- Completion tokens: ${this.completionTokens.toLocaleString()}\n`,
    );
    tooltip.appendMarkdown('\n*Click to open chat*');
    this.item.tooltip = tooltip;
  }

  private connectionIcon(): string {
    switch (this.connectionState) {
      case 'connected':
        return 'vm-active';
      case 'connecting':
        return 'sync~spin';
      case 'disconnected':
        return 'vm-outline';
    }
  }

  private formatTokens(n: number): string {
    if (n >= 1_000_000) {
      return `${(n / 1_000_000).toFixed(1)}M`;
    }
    if (n >= 1_000) {
      return `${(n / 1_000).toFixed(1)}K`;
    }
    return n.toString();
  }
}
