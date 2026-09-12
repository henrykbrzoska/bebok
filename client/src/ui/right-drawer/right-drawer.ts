/**
 * Right drawer (WP-SHELL / F1-10) - skeleton only.
 *
 * 280px column on the Chat screen with its own scroll and a 6px pointer-drag
 * resize handle on its left edge. The pill row at the top toggles the three
 * stacked sections independently (multi-select, NOT exclusive tabs); each open
 * section renders its own header bar plus a ✕ that maps to the same toggle.
 *
 * The section *bodies* are placeholder slots - WP-CHAT fills
 * `panels/{session,explorer,terminal}-panel.ts` with real content.
 */

import { ChangeDetectionStrategy, Component, inject } from '@angular/core';

import { I18nService } from '../../i18n/i18n.service';
import { ShellStore } from '../shell/shell.store';
import { AgentsPanel } from './panels/agents-panel';
import { ChangesPanel } from './panels/changes-panel';
import { ExplorerPanel } from './panels/explorer-panel';
import { SessionPanel } from './panels/session-panel';
import { TerminalPanel } from './panels/terminal-panel';

type PanelId = 'session' | 'explorer' | 'terminal' | 'agents' | 'changes';

@Component({
  selector: 'app-right-drawer',
  changeDetection: ChangeDetectionStrategy.OnPush,
  imports: [SessionPanel, ExplorerPanel, TerminalPanel, AgentsPanel, ChangesPanel],
  templateUrl: './right-drawer.html',
  styleUrl: './right-drawer.css',
})
export class RightDrawer {
  readonly shell = inject(ShellStore);
  private readonly i18n = inject(I18nService);

  readonly t = this.i18n.t.bind(this.i18n);
  readonly panels = this.shell.rightDrawerPanels;
  readonly width = this.shell.rightDrawerWidth;

  readonly pills: {
    id: PanelId;
    labelKey:
      | 'drawer.session'
      | 'drawer.explorer'
      | 'drawer.terminal'
      | 'drawer.agents'
      | 'drawer.changes';
  }[] = [
    { id: 'session', labelKey: 'drawer.session' },
    { id: 'explorer', labelKey: 'drawer.explorer' },
    { id: 'terminal', labelKey: 'drawer.terminal' },
    { id: 'agents', labelKey: 'drawer.agents' },
    { id: 'changes', labelKey: 'drawer.changes' },
  ];

  private dragPointerId: number | null = null;
  private dragStartX = 0;
  private dragStartWidth = 0;

  isOpen(panel: PanelId): boolean {
    return this.panels()[panel];
  }

  toggle(panel: PanelId): void {
    this.shell.toggleRightDrawerPanel(panel);
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
}
