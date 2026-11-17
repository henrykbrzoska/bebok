/**
 * Chat context-menu providers (WP-CTXMENU).
 *
 * Targets: user bubbles (`chat.message-user`), assistant messages
 * (`chat.message-assistant`) and tool rows (`chat.tool`). The chat view
 * passes message text plus callbacks (copy, rollback, ask-about) through
 * the payload; providers only build items.
 */

import type { ContextMenuItem, ContextMenuProvider } from '../../ui/context-menu/context-menu.types';

export interface ChatMessagePayload {
  messageId: string;
  role: 'user' | 'assistant';
  /** Plain text of the message (joined parts) for copy / quote. */
  text: string;
  canRollback: boolean;
  actions: {
    copyText: () => void;
    rollback?: () => void;
    quoteInComposer?: (text: string) => void;
  };
}

export interface ChatToolPayload {
  toolName: string;
  title: string;
  output: string;
  actions: {
    copyOutput: () => void;
    copyInput?: () => void;
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

export const CHAT_USER_PROVIDER: ContextMenuProvider<ChatMessagePayload> = {
  id: 'chat.message-user',
  build: ({ data, selection }): ContextMenuItem[] => {
    const items = selectionItem(selection);
    items.push({
      id: 'copy',
      labelKey: 'ctx.copyMessage',
      icon: '⧉',
      separated: items.length > 0,
      run: () => data.actions.copyText(),
    });
    if (data.canRollback && data.actions.rollback) {
      items.push({
        id: 'rollback',
        labelKey: 'chat.rollback',
        icon: '↩',
        run: () => data.actions.rollback?.(),
      });
    }
    return items;
  },
};

export const CHAT_ASSISTANT_PROVIDER: ContextMenuProvider<ChatMessagePayload> = {
  id: 'chat.message-assistant',
  build: ({ data, selection }): ContextMenuItem[] => {
    const items = selectionItem(selection);
    items.push({
      id: 'copy',
      labelKey: 'ctx.copyMessage',
      icon: '⧉',
      separated: items.length > 0,
      run: () => data.actions.copyText(),
    });
    if (data.actions.quoteInComposer) {
      items.push({
        id: 'quote',
        labelKey: 'ctx.quoteInComposer',
        icon: '❝',
        disabled: !data.text,
        run: () => data.actions.quoteInComposer?.(data.text),
      });
    }
    return items;
  },
};

export const CHAT_TOOL_PROVIDER: ContextMenuProvider<ChatToolPayload> = {
  id: 'chat.tool',
  build: ({ data }): ContextMenuItem[] => [
    {
      id: 'copy-output',
      labelKey: 'ctx.copyToolOutput',
      icon: '⧉',
      disabled: !data.output,
      run: () => data.actions.copyOutput(),
    },
    ...(data.actions.copyInput
      ? [
          {
            id: 'copy-input',
            labelKey: 'ctx.copyToolInput',
            icon: '⧉',
            run: () => data.actions.copyInput?.(),
          } as ContextMenuItem,
        ]
      : []),
  ],
};
