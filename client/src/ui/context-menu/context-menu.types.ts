/**
 * Context menu abstraction (WP-CTXMENU).
 *
 * One DOM overlay for every runtime: the web browser and the Tauri webview
 * render the same Angular component, so menus look and behave identically
 * everywhere with no native-menu plugin involved. Each view (chat, explorer,
 * terminal, debug log, settings) registers its own provider that builds the
 * item list for the right-clicked target; the service only owns open /
 * position / close mechanics.
 *
 * Usage in a template:
 *
 * ```html
 * <div [appContextMenu]="'explorer.file'" [appContextMenuData]="{ node, actions }">…</div>
 * ```
 *
 * where `actions` are view callbacks (open, edit, copy …) the provider calls.
 * The provider itself is registered once (usually in the view component's
 * constructor via `ContextMenuService.register(...)`).
 */

import type { MessageKey } from '../../i18n';

/** What the user right-clicked: mouse position plus the view's payload. */
export interface ContextMenuContext<T = unknown> {
  /** View payload (file node, message, log row, …). */
  data: T;
  /** Text the user had selected inside the target, if any. */
  selection: string;
}

/** One row in the menu. `children` turns it into a submenu. */
export interface ContextMenuItem {
  /** Stable id for tests (`data-testid="ctx-<id>"`). */
  id: string;
  /** i18n key rendered as the label. */
  labelKey: MessageKey;
  /** Optional `{name}` interpolation for the label. */
  labelParams?: Record<string, unknown>;
  /** Material icon / glyph shown before the label (plain text, e.g. `⧉`). */
  icon?: string;
  /** Keyboard hint shown right-aligned (display only, e.g. `Ctrl+C`). */
  hint?: string;
  /** Disabled rows stay visible with the reason as `title`. */
  disabled?: boolean;
  disabledReasonKey?: MessageKey;
  /** A separator is rendered *before* this item. */
  separated?: boolean;
  /** Submenu items; a parent with children never fires `run` itself. */
  children?: ContextMenuItem[];
  /** Action to run on click (leaf items only). */
  run?: () => void | Promise<void>;
}

/**
 * Builds the item list for one right-click. Providers are pure functions of
 * the context — all side effects live in the `run` callbacks the view
 * supplies through `ContextMenuContext.data`.
 */
export interface ContextMenuProvider<T = unknown> {
  /** Unique id referenced by `[appContextMenu]` (e.g. `'explorer.file'`). */
  id: string;
  build(ctx: ContextMenuContext<T>): ContextMenuItem[];
}
