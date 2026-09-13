/**
 * Bottom tab bar (WP-M2 / F10-8): Chat · Remote · Agents · Changes · More.
 *
 * 56px tall plus the bottom safe-area inset. Icons are inline SVG in the
 * sidebar's Feather style (24-unit grid, 1.75 stroke, `currentColor`); the
 * active tab is `--accent` and follows the router through `routerLinkActive`
 * (a prefix match, so `/m/chat/<id>` keeps Chat lit).
 */

import { ChangeDetectionStrategy, Component, inject } from '@angular/core';
import { DomSanitizer, SafeHtml } from '@angular/platform-browser';
import { RouterLink, RouterLinkActive } from '@angular/router';

import { I18nService } from '../../i18n/i18n.service';
import type { MessageKey } from '../../i18n';

export type MobileTabId = 'chat' | 'remote' | 'agents' | 'changes' | 'more';

export interface MobileTab {
  id: MobileTabId;
  path: string;
  label: MessageKey;
  icon: SafeHtml;
}

/** Path data per tab (Feather icons, MIT). */
const ICONS: Record<MobileTabId, string> = {
  // message-square
  chat: '<path d="M21 15a2 2 0 0 1-2 2H7l-4 4V5a2 2 0 0 1 2-2h14a2 2 0 0 1 2 2z"/>',
  // monitor + link: the paired desktop
  remote:
    '<rect x="2" y="3" width="20" height="14" rx="2" ry="2"/><line x1="8" y1="21" x2="16" y2="21"/><line x1="12" y1="17" x2="12" y2="21"/>',
  // users
  agents:
    '<path d="M17 21v-2a4 4 0 0 0-4-4H5a4 4 0 0 0-4 4v2"/><circle cx="9" cy="7" r="4"/><path d="M23 21v-2a4 4 0 0 0-3-3.87"/><path d="M16 3.13a4 4 0 0 1 0 7.75"/>',
  // git-commit-ish diff marker
  changes:
    '<circle cx="12" cy="12" r="4"/><line x1="1.05" y1="12" x2="7" y2="12"/><line x1="17.01" y1="12" x2="22.96" y2="12"/>',
  // more-horizontal
  more: '<circle cx="12" cy="12" r="1"/><circle cx="19" cy="12" r="1"/><circle cx="5" cy="12" r="1"/>',
};

export const MOBILE_TAB_ORDER: ReadonlyArray<{ id: MobileTabId; label: MessageKey }> = [
  { id: 'chat', label: 'mobile.tab.chat' },
  { id: 'remote', label: 'mobile.tab.remote' },
  { id: 'agents', label: 'mobile.tab.agents' },
  { id: 'changes', label: 'mobile.tab.changes' },
  { id: 'more', label: 'mobile.tab.more' },
];

@Component({
  selector: 'app-bottom-tabs',
  changeDetection: ChangeDetectionStrategy.OnPush,
  imports: [RouterLink, RouterLinkActive],
  template: `
    <nav class="tabs" [attr.aria-label]="t('mobile.tabsLabel')" data-testid="bottom-tabs">
      @for (tab of tabs; track tab.id) {
        <a
          class="tab"
          [routerLink]="tab.path"
          routerLinkActive="active"
          ariaCurrentWhenActive="page"
          [attr.data-tab]="tab.id"
          [attr.data-testid]="'tab-' + tab.id"
        >
          <svg
            class="icon"
            viewBox="0 0 24 24"
            fill="none"
            stroke="currentColor"
            stroke-width="1.75"
            stroke-linecap="round"
            stroke-linejoin="round"
            aria-hidden="true"
            [innerHTML]="tab.icon"
          ></svg>
          <span class="label">{{ t(tab.label) }}</span>
        </a>
      }
    </nav>
  `,
  styles: [
    `
      :host {
        display: block;
        flex: none;
      }

      .tabs {
        display: flex;
        height: calc(56px + env(safe-area-inset-bottom, 0px));
        padding-bottom: env(safe-area-inset-bottom, 0px);
        background: var(--surface);
        border-top: 1px solid var(--border);
      }

      .tab {
        flex: 1 1 0;
        min-width: 0;
        display: flex;
        flex-direction: column;
        align-items: center;
        justify-content: center;
        gap: 3px;
        color: var(--text-muted);
        text-decoration: none;
        font-size: var(--fs-11);
        line-height: 1;
        -webkit-tap-highlight-color: transparent;
        user-select: none;
      }

      .tab:active {
        color: var(--text);
      }

      .tab.active {
        color: var(--accent);
      }

      .icon {
        width: 22px;
        height: 22px;
      }

      .label {
        max-width: 100%;
        overflow: hidden;
        text-overflow: ellipsis;
        white-space: nowrap;
      }
    `,
  ],
})
export class BottomTabs {
  private readonly i18n = inject(I18nService);
  private readonly sanitizer = inject(DomSanitizer);

  readonly t = this.i18n.t.bind(this.i18n);

  readonly tabs: MobileTab[] = MOBILE_TAB_ORDER.map(({ id, label }) => ({
    id,
    label,
    path: `/m/${id}`,
    icon: this.sanitizer.bypassSecurityTrustHtml(ICONS[id]),
  }));
}
