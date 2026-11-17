/**
 * Explorer context-menu providers: file rows expose the IDE operations
 * (new file/folder, rename, delete, copy relative path) and the pane
 * header exposes copy-relative-path + reveal-in-tree.
 */

import {
  EXPLORER_FILE_PROVIDER,
  EXPLORER_PANE_PROVIDER,
  EXPLORER_TREE_PROVIDER,
} from './explorer.menus';

function fileActions() {
  return {
    open: () => {},
    openInPreview: () => {},
    startEdit: () => {},
    copyPath: () => {},
    copyName: () => {},
    copyRelativePath: () => {},
    newFile: () => {},
    newFolder: () => {},
    startRename: () => {},
    requestDelete: () => {},
  };
}

describe('EXPLORER_FILE_PROVIDER (IDE operations)', () => {
  it('exposes new file/folder + rename/delete on directory rows', () => {
    const items = EXPLORER_FILE_PROVIDER.build({
      data: { path: 'src', name: 'src', isDir: true, expanded: true, actions: fileActions() },
      selection: '',
    });
    expect(items.map((i) => i.id)).toEqual([
      'toggle',
      'new-file',
      'new-folder',
      'copy-path',
      'rename',
      'delete',
    ]);
  });

  it('exposes rename/delete/copy-relative-path on file rows', () => {
    const items = EXPLORER_FILE_PROVIDER.build({
      data: {
        path: 'src/app.ts',
        name: 'app.ts',
        isDir: false,
        actions: fileActions(),
      },
      selection: '',
    });
    expect(items.map((i) => i.id)).toEqual([
      'open',
      'edit',
      'copy-path',
      'copy-name',
      'copy-relative-path',
      'rename',
      'delete',
    ]);
  });
});

describe('EXPLORER_PANE_PROVIDER (reveal + relative path)', () => {
  it('exposes copy-relative-path and reveal-in-tree', () => {
    const items = EXPLORER_PANE_PROVIDER.build({
      data: {
        path: 'src/app.ts',
        isHtml: false,
        actions: {
          openInPreview: () => {},
          startEdit: () => {},
          copyPath: () => {},
          copyContent: () => {},
          copyRelativePath: () => {},
          revealInTree: () => {},
        },
      },
      selection: '',
    });
    expect(items.map((i) => i.id)).toEqual([
      'open-preview',
      'edit',
      'copy-path',
      'copy-content',
      'copy-relative-path',
      'reveal-in-tree',
    ]);
  });
});

describe('EXPLORER_TREE_PROVIDER (empty tree background)', () => {
  it('exposes tree-wide actions in IDE order', () => {
    const seen: string[] = [];
    const items = EXPLORER_TREE_PROVIDER.build({
      data: {
        actions: {
          newFile: () => void seen.push('new-file'),
          newFolder: () => void seen.push('new-folder'),
          refresh: () => void seen.push('refresh'),
          expandAll: () => void seen.push('expand-all'),
          collapseAll: () => void seen.push('collapse-all'),
          findInFiles: () => void seen.push('find-in-files'),
        },
      },
      selection: '',
    });
    expect(items.map((i) => i.id)).toEqual([
      'new-file',
      'new-folder',
      'refresh',
      'expand-all',
      'collapse-all',
      'find-in-files',
    ]);
    for (const item of items) {
      item.run!();
    }
    expect(seen).toEqual([
      'new-file',
      'new-folder',
      'refresh',
      'expand-all',
      'collapse-all',
      'find-in-files',
    ]);
  });
});

describe('EXPLORER_FILE_PROVIDER toggle (regression)', () => {
  it('calls exactly one of toggleExpand/open (void ?? would fire both)', () => {
    const calls: string[] = [];
    const items = EXPLORER_FILE_PROVIDER.build({
      data: {
        path: 'src',
        name: 'src',
        isDir: true,
        expanded: true,
        actions: {
          ...fileActions(),
          open: () => void calls.push('open'),
          toggleExpand: () => void calls.push('toggleExpand'),
        },
      },
      selection: '',
    });
    items.find((i) => i.id === 'toggle')?.run?.();
    expect(calls).toEqual(['toggleExpand']);
  });

  it('falls back to open when toggleExpand is absent', () => {
    const calls: string[] = [];
    const actions = fileActions() as Partial<ReturnType<typeof fileActions>>;
    delete (actions as Record<string, unknown>)['toggleExpand'];
    const items = EXPLORER_FILE_PROVIDER.build({
      data: {
        path: 'src',
        name: 'src',
        isDir: true,
        expanded: true,
        actions: { ...actions, open: () => void calls.push('open') } as ReturnType<
          typeof fileActions
        >,
      },
      selection: '',
    });
    items.find((i) => i.id === 'toggle')?.run?.();
    expect(calls).toEqual(['open']);
  });
});
