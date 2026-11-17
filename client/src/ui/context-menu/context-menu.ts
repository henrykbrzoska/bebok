/**
 * Context menu overlay (WP-CTXMENU): the single floating menu for the whole
 * app, mounted once in `AppShell`. Positioned at the click point and clamped
 * into the viewport; closes on Escape, outside click, scroll (capture),
 * resize or navigation. Submenus open to the right (flipped left when there
 * is no room). Keyboard: arrows move, Enter runs, Escape closes.
 */

import {
  ChangeDetectionStrategy,
  Component,
  ElementRef,
  computed,
  effect,
  inject,
  signal,
  viewChild,
} from '@angular/core';

import { I18nService } from '../../i18n/i18n.service';
import { ContextMenuService } from './context-menu.service';
import type { ContextMenuItem } from './context-menu.types';

const MENU_WIDTH = 260;
const ITEM_HEIGHT = 32;
const EDGE = 8;

@Component({
  selector: 'app-context-menu',
  changeDetection: ChangeDetectionStrategy.OnPush,
  templateUrl: './context-menu.html',
  styleUrl: './context-menu.css',
  host: {
    '(document:keydown)': 'onDocumentKeydown($event)',
    '(document:click)': 'onDocumentClick($event)',
    '(document:contextmenu)': 'onDocumentContextMenu($event)',
  },
})
export class ContextMenuComponent {
  private readonly menus = inject(ContextMenuService);
  private readonly i18n = inject(I18nService);
  private readonly host = inject(ElementRef<HTMLElement>);

  readonly t = this.i18n.t.bind(this.i18n);
  readonly menuRef = viewChild<ElementRef<HTMLElement>>('menu');

  private readonly openSubmenuId = signal<string | null>(null);
  private readonly focusIndex = signal(0);

  /** Items built by the provider for the current right-click. */
  readonly items = computed<ContextMenuItem[]>(() => {
    const open = this.menus.openMenu();
    if (!open) {
      return [];
    }
    const provider = this.menus.provider(open.providerId);
    if (!provider) {
      return [];
    }
    try {
      return provider.build({ data: open.data, selection: open.selection });
    } catch {
      return [];
    }
  });

  /** Flat list of focusable (leaf, enabled) items for keyboard navigation. */
  private readonly focusable = computed<ContextMenuItem[]>(() =>
    this.items().filter((item) => !item.disabled && !item.children),
  );

  readonly position = computed(() => {
    const open = this.menus.openMenu();
    if (!open) {
      return { x: 0, y: 0 };
    }
    const height = Math.max(1, this.items().length) * ITEM_HEIGHT + 12;
    const maxX = Math.max(EDGE, window.innerWidth - MENU_WIDTH - EDGE);
    const maxY = Math.max(EDGE, window.innerHeight - height - EDGE);
    return {
      x: Math.min(Math.max(EDGE, open.x), maxX),
      y: Math.min(Math.max(EDGE, open.y), maxY),
    };
  });

  /** Submenu of the currently hovered parent, or null. */
  readonly submenu = computed<ContextMenuItem[] | null>(() => {
    const id = this.openSubmenuId();
    if (!id) {
      return null;
    }
    return this.items().find((item) => item.id === id)?.children ?? null;
  });

  constructor() {
    effect(() => {
      // Reset hover/focus state on every open; focus the menu for keys.
      if (this.menus.openMenu()) {
        this.openSubmenuId.set(null);
        this.focusIndex.set(0);
        queueMicrotask(() => this.menuRef()?.nativeElement.focus());
      }
    });
    effect(() => {
      // Close on scroll (capture: any scrollable ancestor) and on resize.
      const open = this.menus.openMenu();
      if (!open) {
        return;
      }
      const close = (): void => this.menus.close();
      window.addEventListener('scroll', close, true);
      window.addEventListener('resize', close);
      return () => {
        window.removeEventListener('scroll', close, true);
        window.removeEventListener('resize', close);
      };
    });
  }

  label(item: ContextMenuItem): string {
    return this.t(item.labelKey, item.labelParams);
  }

  disabledReason(item: ContextMenuItem): string {
    return item.disabledReasonKey ? this.t(item.disabledReasonKey) : '';
  }

  onItemEnter(item: ContextMenuItem): void {
    this.openSubmenuId.set(item.children ? item.id : null);
  }

  onItemClick(item: ContextMenuItem): void {
    if (item.disabled || item.children) {
      return;
    }
    this.menus.close();
    try {
      void item.run?.();
    } catch {
      /* provider callbacks report their own errors */
    }
  }

  onSubItemClick(item: ContextMenuItem): void {
    if (item.disabled) {
      return;
    }
    this.menus.close();
    try {
      void item.run?.();
    } catch {
      /* provider callbacks report their own errors */
    }
  }

  onDocumentKeydown(event: KeyboardEvent): void {
    if (!this.menus.openMenu()) {
      return;
    }
    const focusable = this.focusable();
    switch (event.key) {
      case 'Escape':
        event.preventDefault();
        this.menus.close();
        break;
      case 'ArrowDown':
        event.preventDefault();
        this.focusIndex.update((i) => (focusable.length === 0 ? 0 : (i + 1) % focusable.length));
        this.focusCurrent();
        break;
      case 'ArrowUp':
        event.preventDefault();
        this.focusIndex.update((i) =>
          focusable.length === 0 ? 0 : (i - 1 + focusable.length) % focusable.length,
        );
        this.focusCurrent();
        break;
      case 'Enter':
      case ' ':
        event.preventDefault();
        focusable[this.focusIndex()]?.run?.();
        this.menus.close();
        break;
    }
  }

  private focusCurrent(): void {
    const root: Element = this.host.nativeElement;
    const el = root.querySelector<HTMLElement>(`[data-ctx-index="${this.focusIndex()}"]`);
    el?.focus();
  }

  /** Outside click closes; clicks inside the menu are left to the buttons. */
  onDocumentClick(event: MouseEvent): void {
    if (!this.menus.openMenu()) {
      return;
    }
    if (!this.host.nativeElement.contains(event.target as Node)) {
      this.menus.close();
    }
  }

  /** A second right-click elsewhere moves the menu; on the menu it is ignored. */
  onDocumentContextMenu(event: MouseEvent): void {
    if (!this.menus.openMenu()) {
      // No provider claimed this right-click (plain text, gaps between
      // rows): still swallow the native menu — in the Tauri webview it
      // would otherwise show the useless Back/Forward/Reload popup.
      // Text inputs keep their native menu (spellcheck, IME) unless a
      // provider is attached to them.
      const target = event.target as HTMLElement | null;
      const editable = target?.closest?.('input, textarea, [contenteditable="true"]');
      if (!editable || target?.closest?.('[appContextMenu]')) {
        event.preventDefault();
      }
      return;
    }
    if (this.host.nativeElement.contains(event.target as Node)) {
      event.preventDefault();
    }
  }
}
