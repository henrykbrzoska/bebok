/**
 * Terminal context-menu providers (WP-CTXMENU).
 *
 * Targets: the xterm screen (`terminal.screen`), PTY tabs
 * (`terminal.tab`) and background-process rows (`terminal.process`).
 * xterm owns its own selection, so the screen provider reads it through
 * the payload the view supplies (`getSelection()` on the live instance).
 */

import type { ContextMenuItem, ContextMenuProvider } from '../../ui/context-menu/context-menu.types';

export interface TerminalScreenPayload {
  hasSelection: boolean;
  canPaste: boolean;
  actions: {
    copySelection: () => void;
    paste: () => void;
    clearScreen: () => void;
  };
}

export interface TerminalTabPayload {
  ptyId: string;
  title: string;
  active: boolean;
  actions: {
    activate: () => void;
    close: () => void;
    copyTitle: () => void;
  };
}

export interface TerminalProcessPayload {
  processId: string;
  command: string;
  url: string | null;
  running: boolean;
  actions: {
    openLog: () => void;
    openUrl: () => void;
    stop: () => void;
    copyCommand: () => void;
  };
}

export const TERMINAL_SCREEN_PROVIDER: ContextMenuProvider<TerminalScreenPayload> = {
  id: 'terminal.screen',
  build: ({ data }): ContextMenuItem[] => [
    {
      id: 'copy',
      labelKey: 'ctx.copySelection',
      icon: '⧉',
      hint: 'Ctrl+C',
      disabled: !data.hasSelection,
      disabledReasonKey: 'ctx.noSelection',
      run: () => data.actions.copySelection(),
    },
    {
      id: 'paste',
      labelKey: 'ctx.paste',
      icon: '📋',
      hint: 'Ctrl+V',
      disabled: !data.canPaste,
      run: () => data.actions.paste(),
    },
    {
      id: 'clear',
      labelKey: 'ctx.clearScreen',
      icon: '🧹',
      separated: true,
      run: () => data.actions.clearScreen(),
    },
  ],
};

export const TERMINAL_TAB_PROVIDER: ContextMenuProvider<TerminalTabPayload> = {
  id: 'terminal.tab',
  build: ({ data }): ContextMenuItem[] => [
    {
      id: 'activate',
      labelKey: 'ctx.switchToTab',
      icon: '→',
      disabled: data.active,
      run: () => data.actions.activate(),
    },
    {
      id: 'copy-title',
      labelKey: 'ctx.copyTitle',
      icon: '⧉',
      run: () => data.actions.copyTitle(),
    },
    {
      id: 'close',
      labelKey: 'term.closeTab',
      icon: '✕',
      separated: true,
      run: () => data.actions.close(),
    },
  ],
};

export const TERMINAL_PROCESS_PROVIDER: ContextMenuProvider<TerminalProcessPayload> = {
  id: 'terminal.process',
  build: ({ data }): ContextMenuItem[] => {
    const items: ContextMenuItem[] = [
      {
        id: 'open-log',
        labelKey: 'processes.log',
        icon: '📜',
        run: () => data.actions.openLog(),
      },
    ];
    if (data.url) {
      items.push({
        id: 'open-url',
        labelKey: 'processes.openUrl',
        icon: '↗',
        run: () => data.actions.openUrl(),
      });
    }
    items.push({
      id: 'copy-command',
      labelKey: 'ctx.copyCommand',
      icon: '⧉',
      run: () => data.actions.copyCommand(),
    });
    if (data.running) {
      items.push({
        id: 'stop',
        labelKey: 'processes.stop',
        icon: '⏹',
        separated: true,
        run: () => data.actions.stop(),
      });
    }
    return items;
  },
};
