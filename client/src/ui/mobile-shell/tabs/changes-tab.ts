/**
 * Changes tab (WP-M6 / F10-26): the session's tracked file changes, full
 * screen, read-only.
 *
 * Reuses the right-drawer `ChangesPanel` in its `mobile` mode: the diff
 * overlay fills the screen, renders unified only, and offers neither
 * "Revert" (`POST …/changes/revert` is not allow-listed for the remote
 * scope) nor "Open in Explorer" (no explorer on the phone). The session
 * comes from `MobileSessionHost` (last opened / picked).
 */

import { ChangeDetectionStrategy, Component } from '@angular/core';

import { ChangesPanel } from '../../right-drawer/panels/changes-panel';
import { MobileSessionHost } from '../../../views/mobile/remote-sessions/session-host';

@Component({
  selector: 'app-changes-tab',
  changeDetection: ChangeDetectionStrategy.OnPush,
  imports: [MobileSessionHost, ChangesPanel],
  template: `
    <app-mobile-session-host data-testid="changes-tab">
      <app-changes-panel [mobile]="true" />
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

      app-changes-panel {
        display: block;
        font-size: var(--fs-13);
      }
    `,
  ],
})
export class ChangesTab {}
