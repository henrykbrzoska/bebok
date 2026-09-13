/**
 * Agents tab (WP-M6 / F10-26): sub-agents of the session plus its
 * background processes, read-only, behind a two-way segmented control.
 *
 * `AgentsPanel` is the desktop's drawer panel unchanged (its transcript
 * overlay already fits a phone); `MobileProcesses` is the phone's list +
 * log replacement for the desktop Terminal panel (no kill, no PTY). Both
 * read the session `MobileSessionHost` publishes.
 */

import { ChangeDetectionStrategy, Component, inject, signal } from '@angular/core';

import { I18nService } from '../../../i18n/i18n.service';
import { MobileProcesses } from '../../../views/mobile/remote-sessions/processes-list';
import { MobileSessionHost } from '../../../views/mobile/remote-sessions/session-host';
import { AgentsPanel } from '../../right-drawer/panels/agents-panel';

export type AgentsSegment = 'agents' | 'processes';

@Component({
  selector: 'app-agents-tab',
  changeDetection: ChangeDetectionStrategy.OnPush,
  imports: [MobileSessionHost, AgentsPanel, MobileProcesses],
  template: `
    <app-mobile-session-host data-testid="agents-tab">
      <div class="segments" role="tablist">
        <button
          type="button"
          role="tab"
          class="segment"
          [class.on]="segment() === 'agents'"
          [attr.aria-selected]="segment() === 'agents'"
          (click)="segment.set('agents')"
          data-testid="agents-segment-agents"
        >
          {{ t('mobile.tab.agents') }}
        </button>
        <button
          type="button"
          role="tab"
          class="segment"
          [class.on]="segment() === 'processes'"
          [attr.aria-selected]="segment() === 'processes'"
          (click)="segment.set('processes')"
          data-testid="agents-segment-processes"
        >
          {{ t('mobile.processes.title') }}
        </button>
      </div>
      @if (segment() === 'agents') {
        <app-agents-panel />
      } @else {
        <app-mobile-processes />
      }
    </app-mobile-session-host>
  `,
  styles: [
    `
      :host {
        display: flex;
        flex-direction: column;
        flex: 1 1 auto;
        min-height: 0;
      }

      .segments {
        flex: none;
        display: flex;
        gap: var(--space-6);
        padding: var(--space-8) var(--space-12);
        border-bottom: 1px solid var(--border);
      }

      .segment {
        flex: 1 1 0;
        min-height: 36px;
        border: 1px solid var(--border-strong);
        border-radius: 999px;
        background: transparent;
        color: var(--text-muted);
        font: inherit;
        font-size: var(--fs-12-5);
        cursor: pointer;
      }

      .segment.on {
        border-color: var(--accent);
        color: var(--accent);
      }

      app-agents-panel {
        display: block;
        font-size: var(--fs-13);
      }
    `,
  ],
})
export class AgentsTab {
  private readonly i18n = inject(I18nService);
  readonly t = this.i18n.t.bind(this.i18n);
  readonly segment = signal<AgentsSegment>('agents');
}
