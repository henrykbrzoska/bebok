/**
 * Explorer context-menu providers (WP-CTXMENU).
 *
 * Two targets: tree rows (`explorer.file`) and the content pane header
 * (`explorer.pane`). The view passes its own callbacks through the payload,
 * so the providers stay free of engine calls — every side effect (open,
 * edit, preview, copy path) lives in `ExplorerView`.
 *
 * Forward-looking (IDE): the file provider already branches on `isDir`,
 * `ext` and `writable`, so later work (new file/folder, rename, delete,
 * "ask agent about this file") slots in as new items without touching the
 * overlay or the directive.
 */

import type { ContextMenuItem, ContextMenuProvider } from '../../ui/context-menu/context-menu.types';

export interface ExplorerFilePayload {
  path: string;
  name: string;
  isDir: boolean;
  expanded?: boolean;
  actions: {
    open: () => void;
    toggleExpand?: () => void;
    openInPreview: () => void;
    startEdit: () => void;
    copyPath: () => void;
    copyName: () => void;
    copyRelativePath: () => void;
    newFile: () => void;
    newFolder: () => void;
    startRename: () => void;
    requestDelete: () => void;
    revealInTree?: () => void;
  };
}

export interface ExplorerPanePayload {
  path: string | null;
  isHtml: boolean;
  actions: {
    openInPreview: () => void;
    startEdit: () => void;
    copyPath: () => void;
    copyContent: () => void;
    copyRelativePath: () => void;
    revealInTree: () => void;
  };
}

function extOf(path: string): string {
  const base = path.split(/[\\/]/).pop() ?? path;
  const dot = base.lastIndexOf('.');
  return dot > 0 ? base.slice(dot + 1).toLowerCase() : '';
}

export const EXPLORER_FILE_PROVIDER: ContextMenuProvider<ExplorerFilePayload> = {
  id: 'explorer.file',
  build: ({ data, selection }): ContextMenuItem[] => {
    const ext = extOf(data.path);
    const previewable = ['md', 'markdown', 'txt', 'json', 'yaml', 'yml', 'html', 'htm'].includes(
      ext,
    );
    const items: ContextMenuItem[] = selection
      ? [
          {
            id: 'copy-selection',
            labelKey: 'ctx.copySelection',
            icon: '⧉',
            run: () => navigator.clipboard?.writeText(selection).catch(() => {}),
          },
        ]
      : [];
    if (data.isDir) {
      items.push({
        id: 'toggle',
        labelKey: data.expanded ? 'ctx.collapseFolder' : 'ctx.expandFolder',
        icon: data.expanded ? '▾' : '▸',
        separated: items.length > 0,
        run: () => {
          // NB: `toggleExpand` returns void, so `?.() ?? open()` would run
          // BOTH (toggle twice = no visible change). Call exactly one.
          if (data.actions.toggleExpand) {
            data.actions.toggleExpand();
          } else {
            data.actions.open();
          }
        },
      });
      items.push({
        id: 'new-file',
        labelKey: 'ctx.newFile',
        icon: '+',
        run: () => data.actions.newFile(),
      });
      items.push({
        id: 'new-folder',
        labelKey: 'ctx.newFolder',
        icon: '+',
        run: () => data.actions.newFolder(),
      });
      items.push({
        id: 'copy-path',
        labelKey: 'ctx.copyPath',
        icon: '⧉',
        separated: true,
        run: () => data.actions.copyPath(),
      });
      items.push({
        id: 'rename',
        labelKey: 'ctx.rename',
        icon: '✎',
        run: () => data.actions.startRename(),
      });
      items.push({
        id: 'delete',
        labelKey: 'ctx.delete',
        icon: '🗑',
        run: () => data.actions.requestDelete(),
      });
      return items;
    }
    items.push({
      id: 'open',
      labelKey: 'ctx.openFile',
      icon: '📄',
      separated: items.length > 0,
      run: () => data.actions.open(),
    });
    if (previewable) {
      items.push({
        id: 'open-preview',
        labelKey: 'explorer.openInPreview',
        icon: '👁',
        run: () => data.actions.openInPreview(),
      });
    }
    items.push({
      id: 'edit',
      labelKey: 'explorer.edit',
      icon: '✎',
      run: () => data.actions.startEdit(),
    });
    items.push({
      id: 'copy-path',
      labelKey: 'ctx.copyPath',
      icon: '⧉',
      separated: true,
      run: () => data.actions.copyPath(),
    });
    items.push({
      id: 'copy-name',
      labelKey: 'ctx.copyFileName',
      icon: '⧉',
      run: () => data.actions.copyName(),
    });
    items.push({
      id: 'copy-relative-path',
      labelKey: 'ctx.copyRelativePath',
      icon: '⧉',
      run: () => data.actions.copyRelativePath(),
    });
    items.push({
      id: 'rename',
      labelKey: 'ctx.rename',
      icon: '✎',
      separated: true,
      run: () => data.actions.startRename(),
    });
    items.push({
      id: 'delete',
      labelKey: 'ctx.delete',
      icon: '🗑',
      run: () => data.actions.requestDelete(),
    });
    return items;
  },
};

export const EXPLORER_PANE_PROVIDER: ContextMenuProvider<ExplorerPanePayload> = {
  id: 'explorer.pane',
  build: ({ data, selection }): ContextMenuItem[] => {
    const items: ContextMenuItem[] = selection
      ? [
          {
            id: 'copy-selection',
            labelKey: 'ctx.copySelection',
            icon: '⧉',
            run: () => navigator.clipboard?.writeText(selection).catch(() => {}),
          },
        ]
      : [];
    if (!data.path) {
      return items;
    }
    items.push({
      id: 'open-preview',
      labelKey: 'explorer.openInPreview',
      icon: '👁',
      separated: items.length > 0,
      run: () => data.actions.openInPreview(),
    });
    items.push({
      id: 'edit',
      labelKey: 'explorer.edit',
      icon: '✎',
      run: () => data.actions.startEdit(),
    });
    items.push({
      id: 'copy-path',
      labelKey: 'ctx.copyPath',
      icon: '⧉',
      separated: true,
      run: () => data.actions.copyPath(),
    });
    items.push({
      id: 'copy-content',
      labelKey: 'ctx.copyFileContent',
      icon: '⧉',
      run: () => data.actions.copyContent(),
    });
    items.push({
      id: 'copy-relative-path',
      labelKey: 'ctx.copyRelativePath',
      icon: '⧉',
      run: () => data.actions.copyRelativePath(),
    });
    items.push({
      id: 'reveal-in-tree',
      labelKey: 'ctx.revealInTree',
      icon: '◎',
      run: () => data.actions.revealInTree(),
    });
    return items;
  },
};

/**
 * Payload for empty tree background (`explorer.tree`). Right-clicking the
 * gap below the rows (or the empty-tree note) offers tree-wide actions:
 * create at the project root, refresh, expand/collapse all, find in files.
 */
export interface ExplorerTreePayload {
  actions: {
    newFile: () => void;
    newFolder: () => void;
    refresh: () => void;
    expandAll: () => void;
    collapseAll: () => void;
    findInFiles: () => void;
  };
}

export const EXPLORER_TREE_PROVIDER: ContextMenuProvider<ExplorerTreePayload> = {
  id: 'explorer.tree',
  build: ({ data }): ContextMenuItem[] => [
    {
      id: 'new-file',
      labelKey: 'ctx.newFile',
      icon: '+',
      run: () => data.actions.newFile(),
    },
    {
      id: 'new-folder',
      labelKey: 'ctx.newFolder',
      icon: '+',
      run: () => data.actions.newFolder(),
    },
    {
      id: 'refresh',
      labelKey: 'explorer.refreshTree',
      icon: '⟳',
      separated: true,
      run: () => data.actions.refresh(),
    },
    {
      id: 'expand-all',
      labelKey: 'explorer.expandAll',
      icon: '▾▾',
      run: () => data.actions.expandAll(),
    },
    {
      id: 'collapse-all',
      labelKey: 'explorer.collapseAll',
      icon: '▸▸',
      run: () => data.actions.collapseAll(),
    },
    {
      id: 'find-in-files',
      labelKey: 'findInFiles.title',
      icon: '⌕',
      hint: 'Ctrl+Shift+F',
      separated: true,
      run: () => data.actions.findInFiles(),
    },
  ],
};

/**
 * Payload for the editor textarea (`explorer.editor`). State is read lazily
 * through `state()` at right-click time - a binding-time snapshot would show
 * the caret/selection of the previous change detection, not of the click.
 */
export interface ExplorerEditorPayload {
  state: () => {
    hasSelection: boolean;
    /** Identifier under the caret, when go-to-definition applies. */
    symbol: string | null;
    dirty: boolean;
  };
  actions: {
    gotoDefinition: () => void;
    cut: () => void;
    copy: () => void;
    paste: () => void;
    selectAll: () => void;
    save: () => void;
    find: () => void;
  };
}

export const EXPLORER_EDITOR_PROVIDER: ContextMenuProvider<ExplorerEditorPayload> = {
  id: 'explorer.editor',
  build: ({ data }): ContextMenuItem[] => {
    const st = data.state();
    const items: ContextMenuItem[] = [];
    if (st.symbol) {
      items.push({
        id: 'goto-definition',
        labelKey: 'explorer.gotoDefinition',
        labelParams: { symbol: st.symbol },
        icon: '↗',
        hint: 'Ctrl+Click',
        run: () => data.actions.gotoDefinition(),
      });
    }
    items.push({
      id: 'cut',
      labelKey: 'ctx.cut',
      icon: '✂',
      hint: 'Ctrl+X',
      separated: items.length > 0,
      disabled: !st.hasSelection,
      disabledReasonKey: 'ctx.noSelection',
      run: () => data.actions.cut(),
    });
    items.push({
      id: 'copy',
      labelKey: 'ctx.copySelection',
      icon: '⧉',
      hint: 'Ctrl+C',
      disabled: !st.hasSelection,
      disabledReasonKey: 'ctx.noSelection',
      run: () => data.actions.copy(),
    });
    items.push({
      id: 'paste',
      labelKey: 'ctx.paste',
      icon: '📋',
      hint: 'Ctrl+V',
      run: () => data.actions.paste(),
    });
    items.push({
      id: 'select-all',
      labelKey: 'ctx.selectAll',
      icon: '▭',
      hint: 'Ctrl+A',
      run: () => data.actions.selectAll(),
    });
    items.push({
      id: 'find',
      labelKey: 'explorer.find',
      icon: '⌕',
      hint: 'Ctrl+F',
      separated: true,
      run: () => data.actions.find(),
    });
    items.push({
      id: 'save',
      labelKey: 'explorer.save',
      icon: '💾',
      hint: 'Ctrl+S',
      disabled: !st.dirty,
      run: () => data.actions.save(),
    });
    return items;
  },
};
