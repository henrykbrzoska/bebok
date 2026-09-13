/**
 * Right drawer (WP-SHELL / F1-10, redesigned in F9-2).
 *
 * A 320px (default) column on the Chat screen with a 6px pointer-drag resize
 * handle on its left edge. The pill row at the top toggles the stacked
 * sections independently (multi-select, NOT exclusive tabs).
 *
 * F9-2 layout: the pill row is a fixed strip; everything below it is ONE
 * scroll container (`.panels`) so a wheel anywhere over the drawer (outside a
 * panel's own inner scroller) moves the whole stack. Each open section has a
 * sticky header (`position: sticky; top: 0`) that stays pinned while its body
 * scrolls underneath; the header is a button that collapses/expands the body
 * (chevron, state remembered per panel in `UiPrefsStore.rightDrawerCollapsed`)
 * and carries the ✕ that maps to the same pill toggle. A collapsed section
 * keeps its component alive (`[hidden]`, not destroyed) so the terminal,
 * browser and agents panels do not lose state when folded away.
 *
 * Programmatic opens (Preview from a chat link, Browser when a browser tool
 * fires, Agents when a sub-agent spawns) go through
 * `ShellStore.revealRightDrawerPanel` which arms the pill + expanded state and
 * raises a `rightDrawerReveal` request; `afterRenderEffect` below answers it
 * once the section is in the DOM by scrolling it to the top of the drawer.
 */

import {
  ChangeDetectionStrategy,
  Component,
  DestroyRef,
  ElementRef,
  afterRenderEffect,
  inject,
  untracked,
} from '@angular/core';

import { RightDrawerPanelId } from '../../core/ui-prefs.store';
import { I18nService } from '../../i18n/i18n.service';
import { ShellStore } from '../shell/shell.store';
import { AgentsPanel } from './panels/agents-panel';
import { BrowserPanel } from './panels/browser-panel';
import { ChangesPanel } from './panels/changes-panel';
import { ExplorerPanel } from './panels/explorer-panel';
import { PreviewPanel } from './panels/preview-panel';
import { SessionPanel } from './panels/session-panel';
import { TerminalPanel } from './panels/terminal-panel';

type PanelId = RightDrawerPanelId;

/** A reveal older than this when the drawer mounts is a leftover, not a request. */
const REVEAL_STALE_MS = 3000;
/** How long a revealed section is kept scrolled into view while it loads. */
const REVEAL_SETTLE_MS = 2000;

@Component({
  selector: 'app-right-drawer',
  changeDetection: ChangeDetectionStrategy.OnPush,
  imports: [
    SessionPanel,
    ExplorerPanel,
    TerminalPanel,
    AgentsPanel,
    ChangesPanel,
    PreviewPanel,
    BrowserPanel,
  ],
  templateUrl: './right-drawer.html',
  styleUrl: './right-drawer.css',
})
export class RightDrawer {
  readonly shell = inject(ShellStore);
  private readonly i18n = inject(I18nService);
  private readonly host = inject<ElementRef<HTMLElement>>(ElementRef);

  readonly t = this.i18n.t.bind(this.i18n);
  readonly panels = this.shell.rightDrawerPanels;
  readonly collapsed = this.shell.rightDrawerCollapsed;
  readonly width = this.shell.rightDrawerWidth;

  readonly pills: {
    id: PanelId;
    labelKey:
      | 'drawer.session'
      | 'drawer.explorer'
      | 'drawer.terminal'
      | 'drawer.agents'
      | 'drawer.changes'
      | 'drawer.preview'
      | 'drawer.browser';
  }[] = [
    { id: 'session', labelKey: 'drawer.session' },
    { id: 'explorer', labelKey: 'drawer.explorer' },
    { id: 'terminal', labelKey: 'drawer.terminal' },
    { id: 'agents', labelKey: 'drawer.agents' },
    { id: 'changes', labelKey: 'drawer.changes' },
    { id: 'preview', labelKey: 'drawer.preview' },
    { id: 'browser', labelKey: 'drawer.browser' },
  ];

  private dragPointerId: number | null = null;
  private dragStartX = 0;
  private dragStartWidth = 0;

  /** Last reveal nonce answered; seeded so a stale request is not replayed
   *  on mount, but a reveal that opened this very drawer still scrolls. */
  private lastRevealNonce = 0;
  private revealObserver: ResizeObserver | null = null;

  constructor() {
    const initial = this.shell.rightDrawerReveal();
    if (initial && Date.now() - initial.at > REVEAL_STALE_MS) {
      this.lastRevealNonce = initial.nonce;
    }
    inject(DestroyRef).onDestroy(() => this.revealObserver?.disconnect());
    afterRenderEffect(() => {
      const req = this.shell.rightDrawerReveal();
      if (!req || req.nonce === this.lastRevealNonce) {
        return;
      }
      // The section renders in the same pass (its pill is already on), so
      // the element exists by the time this after-render callback runs.
      this.lastRevealNonce = req.nonce;
      untracked(() => this.scrollPanelIntoView(req.panel));
    });
  }

  isOpen(panel: PanelId): boolean {
    return this.panels()[panel];
  }

  isCollapsed(panel: PanelId): boolean {
    return this.collapsed()[panel];
  }

  toggle(panel: PanelId): void {
    this.shell.toggleRightDrawerPanel(panel);
  }

  /** F9-2: header click - fold the body away or bring it back. */
  toggleCollapsed(panel: PanelId): void {
    this.shell.toggleRightDrawerPanelCollapsed(panel);
  }

  /** ✕ inside the header button: close the section, do not also collapse it. */
  close(panel: PanelId, event: Event): void {
    event.stopPropagation();
    this.toggle(panel);
  }

  /** Real pointer-drag resize: dragging the left edge leftwards widens it. */
  onResizeStart(event: PointerEvent): void {
    event.preventDefault();
    const handle = event.target as HTMLElement;
    this.dragPointerId = event.pointerId;
    this.dragStartX = event.clientX;
    this.dragStartWidth = this.width();
    handle.setPointerCapture(event.pointerId);
  }

  onResizeMove(event: PointerEvent): void {
    if (this.dragPointerId !== event.pointerId) {
      return;
    }
    this.shell.setRightDrawerWidth(this.dragStartWidth - (event.clientX - this.dragStartX));
  }

  onResizeEnd(event: PointerEvent): void {
    if (this.dragPointerId !== event.pointerId) {
      return;
    }
    (event.target as HTMLElement).releasePointerCapture(event.pointerId);
    this.dragPointerId = null;
  }

  private scrollPanelIntoView(panel: PanelId): void {
    const root = this.host.nativeElement;
    const scroller = root.querySelector<HTMLElement>('.panels');
    const section = root.querySelector<HTMLElement>(`[data-panel="${panel}"]`);
    if (!scroller || !section) {
      return;
    }
    // Scroll the drawer's own container (never the document - F9-1): put
    // the section's sticky header at the top of the visible stack. `.panels`
    // is `position: relative`, so `offsetTop` is already relative to it.
    const settle = (): void => scroller.scrollTo({ top: section.offsetTop, behavior: 'smooth' });
    settle();
    // A revealed panel usually grows right after it appears (the Preview
    // fetches its file, the Agents list its rows); keep it in view while it
    // settles instead of leaving the freshly loaded body below the fold.
    this.revealObserver?.disconnect();
    if (typeof ResizeObserver === 'undefined') {
      return;
    }
    const observer = new ResizeObserver(() => settle());
    observer.observe(section);
    this.revealObserver = observer;
    window.setTimeout(() => {
      observer.disconnect();
      if (this.revealObserver === observer) {
        this.revealObserver = null;
      }
    }, REVEAL_SETTLE_MS);
  }
}
