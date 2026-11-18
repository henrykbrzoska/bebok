/**
 * Debug-log and settings context-menu providers (WP-CTXMENU).
 *
 * Debug targets: log rows (`debug.row`) and LLM call cards
 * (`debug.call`). Settings targets: the raw-JSON textarea
 * (`settings.raw`) and permission-rule rows (`settings.rule`).
 */

import type { ContextMenuItem, ContextMenuProvider } from '../../ui/context-menu/context-menu.types';

export interface DebugRowPayload {
  text: string;
  actions: {
    copyRow: () => void;
    copyPath?: () => void;
    setFilter: (text: string) => void;
  };
}

export interface DebugCallPayload {
  model: string;
  requestJson: string;
  responseJson: string;
  actions: {
    copyRequest: () => void;
    copyResponse: () => void;
  };
}

export interface SettingsRawPayload {
  hasSelection: boolean;
  canPaste: boolean;
  actions: {
    copy: () => void;
    paste: () => void;
    format: () => void;
    selectAll: () => void;
  };
}

export interface SettingsRulePayload {
  description: string;
  actions: {
    copyRule: () => void;
    deleteRule?: () => void;
  };
}

function selectionItem(selection: string): ContextMenuItem[] {
  return selection
    ? [
        {
          id: 'copy-selection',
          labelKey: 'ctx.copySelection',
          icon: '⧉',
          run: () => navigator.clipboard?.writeText(selection).catch(() => {}),
        },
      ]
    : [];
}

export const DEBUG_ROW_PROVIDER: ContextMenuProvider<DebugRowPayload> = {
  id: 'debug.row',
  build: ({ data, selection }): ContextMenuItem[] => {
    const items = selectionItem(selection);
    items.push({
      id: 'copy-row',
      labelKey: 'ctx.copyRow',
      icon: '⧉',
      separated: items.length > 0,
      run: () => data.actions.copyRow(),
    });
    if (data.actions.copyPath) {
      items.push({
        id: 'copy-path',
        labelKey: 'ctx.copyPath',
        icon: '⧉',
        run: () => data.actions.copyPath?.(),
      });
    }
    items.push({
      id: 'filter',
      labelKey: 'ctx.filterByThis',
      icon: '🔍',
      run: () => data.actions.setFilter(data.text),
    });
    return items;
  },
};

export const DEBUG_CALL_PROVIDER: ContextMenuProvider<DebugCallPayload> = {
  id: 'debug.call',
  build: ({ data }): ContextMenuItem[] => [
    {
      id: 'copy-request',
      labelKey: 'ctx.copyRequestJson',
      icon: '⧉',
      run: () => data.actions.copyRequest(),
    },
    {
      id: 'copy-response',
      labelKey: 'ctx.copyResponseJson',
      icon: '⧉',
      run: () => data.actions.copyResponse(),
    },
  ],
};

export const SETTINGS_RAW_PROVIDER: ContextMenuProvider<SettingsRawPayload> = {
  id: 'settings.raw',
  build: ({ data }): ContextMenuItem[] => [
    {
      id: 'copy',
      labelKey: 'ctx.copySelection',
      icon: '⧉',
      hint: 'Ctrl+C',
      disabled: !data.hasSelection,
      disabledReasonKey: 'ctx.noSelection',
      run: () => data.actions.copy(),
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
      id: 'select-all',
      labelKey: 'ctx.selectAll',
      icon: '☰',
      hint: 'Ctrl+A',
      separated: true,
      run: () => data.actions.selectAll(),
    },
    {
      id: 'format',
      labelKey: 'settings.rawFormat',
      icon: '✨',
      run: () => data.actions.format(),
    },
  ],
};

export const SETTINGS_RULE_PROVIDER: ContextMenuProvider<SettingsRulePayload> = {
  id: 'settings.rule',
  build: ({ data }): ContextMenuItem[] => {
    const items: ContextMenuItem[] = [
      {
        id: 'copy-rule',
        labelKey: 'ctx.copyRule',
        icon: '⧉',
        run: () => data.actions.copyRule(),
      },
    ];
    if (data.actions.deleteRule) {
      items.push({
        id: 'delete-rule',
        labelKey: 'ctx.deleteRule',
        icon: '🗑',
        separated: true,
        run: () => data.actions.deleteRule?.(),
      });
    }
    return items;
  },
};
