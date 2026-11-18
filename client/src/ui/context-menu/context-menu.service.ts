/**
 * Context menu service (WP-CTXMENU): owns the single open menu.
 *
 * The menu itself is one DOM overlay (`ContextMenuComponent`, mounted in
 * `AppShell`), so it renders identically in the web browser and in the
 * Tauri webview — no native-menu plugin involved. Views register a
 * `ContextMenuProvider` per right-click target (e.g. `'explorer.file'`,
 * `'chat.message'`) and attach it with the `appContextMenu` directive;
 * the service only handles open / position / close mechanics plus a
 * clipboard helper with a non-secure-context fallback.
 */

import { Injectable, signal } from '@angular/core';

import type { ContextMenuContext, ContextMenuProvider } from './context-menu.types';

export interface OpenMenu {
  providerId: string;
  x: number;
  y: number;
  data: unknown;
  selection: string;
}

@Injectable({ providedIn: 'root' })
export class ContextMenuService {
  private readonly providers = new Map<string, ContextMenuProvider<never>>();

  /** The currently open menu, or null when closed. */
  readonly openMenu = signal<OpenMenu | null>(null);

  register<T>(provider: ContextMenuProvider<T>): void {
    this.providers.set(provider.id, provider as ContextMenuProvider<never>);
  }

  provider(id: string): ContextMenuProvider<unknown> | null {
    return (this.providers.get(id) as ContextMenuProvider<unknown> | undefined) ?? null;
  }

  /** Open the menu for `providerId` at the mouse position. Always
   *  suppresses the native menu (also in the Tauri webview); returns false
   *  when no provider is registered so the directive can still swallow the
   *  event instead of showing the webview's Back/Forward/Reload menu. */
  open(providerId: string, event: MouseEvent, data: unknown = undefined): boolean {
    event.preventDefault();
    event.stopPropagation();
    if (!this.providers.has(providerId)) {
      return false;
    }
    this.openMenu.set({
      providerId,
      x: event.clientX,
      y: event.clientY,
      data,
      selection: selectedTextInside(event),
    });
    return true;
  }

  close(): void {
    this.openMenu.set(null);
  }

  readonly isOpen = this.openMenu.asReadonly;

  /**
   * `navigator.clipboard.writeText` with a textarea + `execCommand` fallback
   * (non-secure contexts, older webviews). Returns whether the copy worked.
   */
  async copyText(text: string): Promise<boolean> {
    if (!text) {
      return false;
    }
    try {
      await navigator.clipboard.writeText(text);
      return true;
    } catch {
      /* fall through to the legacy path */
    }
    try {
      const area = document.createElement('textarea');
      area.value = text;
      area.setAttribute('readonly', '');
      area.style.position = 'fixed';
      area.style.opacity = '0';
      document.body.appendChild(area);
      area.select();
      const ok = document.execCommand('copy');
      area.remove();
      return ok;
    } catch {
      return false;
    }
  }

  /** Best-effort paste via the async Clipboard API (null when unavailable). */
  async readText(): Promise<string | null> {
    try {
      return await navigator.clipboard.readText();
    } catch {
      return null;
    }
  }
}

/** Text the user had selected inside the right-clicked element, if any. */
function selectedTextInside(event: MouseEvent): string {
  try {
    const selection = window.getSelection();
    if (!selection || selection.isCollapsed) {
      return '';
    }
    const target = event.target as Node | null;
    if (target && selection.containsNode(target, true)) {
      return selection.toString();
    }
    return '';
  } catch {
    return '';
  }
}
