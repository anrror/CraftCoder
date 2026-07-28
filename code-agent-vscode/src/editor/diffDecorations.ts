/**
 * Inline diff decorations for displaying agent-proposed code changes.
 *
 * Applies VS Code decoration types to highlight additions (green background),
 * deletions (red background), and provides accept/reject commands.
 */

import * as vscode from 'vscode';

// ---------------------------------------------------------------------------
// Decoration types
// ---------------------------------------------------------------------------

const addedDecoration = vscode.window.createTextEditorDecorationType({
  backgroundColor: { id: 'diffEditor.insertedTextBackground' },
  border: '1px solid',
  borderColor: { id: 'diffEditor.insertedLineBackground' },
  isWholeLine: true,
  overviewRulerColor: { id: 'charts.green' },
  overviewRulerLane: vscode.OverviewRulerLane.Right,
});

const removedDecoration = vscode.window.createTextEditorDecorationType({
  backgroundColor: { id: 'diffEditor.removedTextBackground' },
  border: '1px solid',
  borderColor: { id: 'diffEditor.removedLineBackground' },
  isWholeLine: true,
  overviewRulerColor: { id: 'charts.red' },
  overviewRulerLane: vscode.OverviewRulerLane.Right,
});

const modifiedDecoration = vscode.window.createTextEditorDecorationType({
  backgroundColor: { id: 'diffEditor.removedTextBackground' },
  border: '1px solid',
  borderColor: { id: 'diffEditor.removedLineBackground' },
  isWholeLine: true,
  overviewRulerColor: { id: 'charts.blue' },
  overviewRulerLane: vscode.OverviewRulerLane.Right,
  after: {
    contentText: ' ← Modified',
    color: { id: 'editorCodeLens.foreground' },
    fontStyle: 'italic',
  },
});

// ---------------------------------------------------------------------------
// Types
// ---------------------------------------------------------------------------

export interface DiffHunk {
  /** 0-based line index in the current document. */
  line: number;
  /** The original text (before change). */
  oldText: string;
  /** The new text (after change). */
  newText: string;
  /** Whether this is an insertion (no old text), deletion (no new text), or modification. */
  kind: 'added' | 'removed' | 'modified';
  /** Optional commit hash or provenance info. */
  source?: string;
}

export interface DiffBlock {
  /** Identifier for this diff block (used for accept/reject). */
  id: string;
  /** The URI of the file being modified. */
  uri: vscode.Uri;
  /** The hunks that make up this diff. */
  hunks: DiffHunk[];
}

// ---------------------------------------------------------------------------
// DiffManager
// ---------------------------------------------------------------------------

export class DiffManager {
  private activeDiffs = new Map<string, DiffBlock>();

  /** Apply diff decorations to the given editor. */
  applyDiff(block: DiffBlock): void {
    this.activeDiffs.set(block.id, block);

    const editor = vscode.window.visibleTextEditors.find(
      (e) => e.document.uri.toString() === block.uri.toString(),
    );
    if (!editor) return;

    const addedRanges: vscode.Range[] = [];
    const removedRanges: vscode.Range[] = [];
    const modifiedRanges: vscode.Range[] = [];

    for (const hunk of block.hunks) {
      const range = new vscode.Range(hunk.line, 0, hunk.line + 1, 0);
      switch (hunk.kind) {
        case 'added':
          addedRanges.push(range);
          break;
        case 'removed':
          removedRanges.push(range);
          break;
        case 'modified':
          modifiedRanges.push(range);
          break;
      }
    }

    editor.setDecorations(addedDecoration, addedRanges);
    editor.setDecorations(removedDecoration, removedRanges);
    editor.setDecorations(modifiedDecoration, modifiedRanges);
  }

  /** Accept all hunks in a diff block (apply changes to the document). */
  async acceptDiff(blockId: string): Promise<void> {
    const block = this.activeDiffs.get(blockId);
    if (!block) return;

    const doc = await vscode.workspace.openTextDocument(block.uri);
    const edit = new vscode.WorkspaceEdit();

    // Apply changes in reverse order to avoid offset issues
    const sorted = [...block.hunks].sort((a, b) => b.line - a.line);

    for (const hunk of sorted) {
      const range = doc.lineAt(hunk.line).range;
      if (hunk.kind === 'removed') {
        edit.delete(block.uri, range);
      } else {
        edit.replace(block.uri, range, hunk.newText);
      }
    }

    await vscode.workspace.applyEdit(edit);
    this.clearDiff(blockId);
  }

  /** Reject a diff block (remove decorations without applying). */
  clearDiff(blockId: string): void {
    this.activeDiffs.delete(blockId);
    this.refreshEditors();
  }

  /** Clear all diffs. */
  clearAll(): void {
    this.activeDiffs.clear();
    this.refreshEditors();
  }

  /** Get an active diff by ID. */
  getDiff(blockId: string): DiffBlock | undefined {
    return this.activeDiffs.get(blockId);
  }

  /** List all active diff IDs. */
  listDiffs(): string[] {
    return Array.from(this.activeDiffs.keys());
  }

  /** Dispose all decorations. */
  dispose(): void {
    this.clearAll();
    addedDecoration.dispose();
    removedDecoration.dispose();
    modifiedDecoration.dispose();
  }

  // -----------------------------------------------------------------------
  // Private
  // -----------------------------------------------------------------------

  private refreshEditors(): void {
    for (const editor of vscode.window.visibleTextEditors) {
      const uri = editor.document.uri.toString();
      const addedRanges: vscode.Range[] = [];
      const removedRanges: vscode.Range[] = [];
      const modifiedRanges: vscode.Range[] = [];

      for (const block of this.activeDiffs.values()) {
        if (block.uri.toString() !== uri) continue;

        for (const hunk of block.hunks) {
          const range = new vscode.Range(hunk.line, 0, hunk.line + 1, 0);
          switch (hunk.kind) {
            case 'added':
              addedRanges.push(range);
              break;
            case 'removed':
              removedRanges.push(range);
              break;
            case 'modified':
              modifiedRanges.push(range);
              break;
          }
        }
      }

      editor.setDecorations(addedDecoration, addedRanges);
      editor.setDecorations(removedDecoration, removedRanges);
      editor.setDecorations(modifiedDecoration, modifiedRanges);
    }
  }
}
