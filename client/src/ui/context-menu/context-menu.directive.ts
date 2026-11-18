/**
 * `appContextMenu` directive (WP-CTXMENU): attaches a registered provider to
 * any element. Right-click opens the shared overlay menu via
 * `ContextMenuService`; the view passes its callbacks through
 * `[appContextMenuData]`.
 *
 * ```html
 * <div [appContextMenu]="'explorer.file'" [appContextMenuData]="{ node, actions }">…</div>
 * ```
 */

import { Directive, HostListener, inject, input } from '@angular/core';

import { ContextMenuService } from './context-menu.service';

@Directive({
  selector: '[appContextMenu]',
  standalone: true,
})
export class ContextMenuDirective {
  /** Id of a provider registered on `ContextMenuService`. */
  readonly appContextMenu = input.required<string>();
  /** View payload forwarded to the provider's `build()`. */
  readonly appContextMenuData = input<unknown>(undefined);
  /** When false the browser's native menu is left alone. */
  readonly appContextMenuEnabled = input(true);

  private readonly menus = inject(ContextMenuService);

  @HostListener('contextmenu', ['$event'])
  onContextMenu(event: MouseEvent): void {
    if (!this.appContextMenuEnabled()) {
      return;
    }
    // `open()` always calls preventDefault first, so the native menu
    // (browser's, or the Tauri webview's Back/Forward/Reload) never wins —
    // even when the provider id turns out to be unknown.
    this.menus.open(this.appContextMenu(), event, this.appContextMenuData());
  }
}
