/**
 * Non-modal "new version available" bar (bottom-right, above the toasts).
 *
 * Desktop shell: "Install & restart" drives `UpdateStore.install()` with a
 * progress bar, then "Restart now" once the bundle is in place (Windows
 * restarts on its own - the installer exits the app). Browser mode and dev
 * builds only get a "Download" link to the release page. "Later" hides the
 * bar for this version; the About chip keeps its badge.
 */

import { ChangeDetectionStrategy, Component, inject } from '@angular/core';

import { UpdateStore } from '../../core/update.store';
import { I18nService } from '../../i18n/i18n.service';

@Component({
  selector: 'app-update-banner',
  changeDetection: ChangeDetectionStrategy.OnPush,
  template: `
    @if (update.bannerVisible()) {
      <aside class="bar" role="status" aria-live="polite" data-testid="update-banner">
        @if (update.phase() === 'ready') {
          <span class="text">{{
            t('update.ready', { version: update.available()?.version ?? '' })
          }}</span>
          <div class="actions">
            <button
              type="button"
              class="primary"
              (click)="update.relaunch()"
              data-testid="update-restart"
            >
              {{ t('update.restart') }}
            </button>
          </div>
        } @else if (update.available(); as available) {
          <span class="text">
            {{
              t('update.available', {
                version: available.version,
                current: available.currentVersion,
              })
            }}
          </span>
          @if (update.phase() === 'downloading' || update.phase() === 'installing') {
            <div
              class="progress"
              role="progressbar"
              [attr.aria-valuenow]="update.progressPercent()"
            >
              <div class="progress-fill" [style.width.%]="update.progressPercent() ?? 100"></div>
            </div>
            <span class="muted">
              {{
                update.phase() === 'installing'
                  ? t('update.installing')
                  : t('update.downloading', { percent: update.progressPercent() ?? 0 })
              }}
            </span>
          } @else {
            @if (update.error(); as message) {
              <span class="error" role="alert">{{
                update.feedMissing() ? t('update.noManifest') : t('update.error', { message })
              }}</span>
            }
            @if (available.installable && update.installBlocked()) {
              <span class="muted" data-testid="update-blocked">{{ t('update.busy') }}</span>
            }
            <div class="actions">
              @if (available.installable) {
                <button
                  type="button"
                  class="primary"
                  [disabled]="update.installBlocked()"
                  (click)="update.install()"
                  data-testid="update-install"
                >
                  {{ t('update.install') }}
                </button>
              } @else {
                <button
                  type="button"
                  class="primary"
                  (click)="update.openReleasePage()"
                  data-testid="update-download"
                >
                  {{ t('update.download') }}
                </button>
              }
              <button type="button" class="ghost" (click)="update.openReleasePage()">
                {{ t('update.notes') }}
              </button>
              <button
                type="button"
                class="ghost"
                (click)="update.dismiss()"
                data-testid="update-later"
              >
                {{ t('update.later') }}
              </button>
            </div>
          }
        }
      </aside>
    }
  `,
  styles: `
    .bar {
      position: fixed;
      right: var(--space-16);
      bottom: var(--space-16);
      z-index: 900;
      display: flex;
      flex-direction: column;
      gap: var(--space-9);
      width: min(420px, calc(100vw - 2 * var(--space-16)));
      padding: var(--space-12) var(--space-14);
      background: var(--surface);
      border: 1px solid var(--accent);
      border-radius: var(--radius-card);
      box-shadow: 0 8px 24px rgba(0, 0, 0, 0.25);
      font-size: var(--fs-12-5);
      color: var(--text);
    }
    .muted {
      color: var(--text-muted);
      font-size: var(--fs-11-5);
    }
    .error {
      color: var(--danger);
      font-size: var(--fs-11-5);
    }
    .progress {
      height: 4px;
      overflow: hidden;
      background: var(--surface-3);
      border-radius: 2px;
    }
    .progress-fill {
      height: 100%;
      background: var(--accent);
      transition: width 120ms linear;
    }
    .actions {
      display: flex;
      flex-wrap: wrap;
      gap: var(--space-6);
    }
    .actions button {
      border-radius: var(--radius-control);
      padding: var(--space-6) var(--space-12);
      font-size: var(--fs-12);
      cursor: pointer;
    }
    .actions .ghost {
      background: transparent;
      border: 1px solid var(--border);
      color: var(--text-muted);
    }
    .actions .ghost:hover {
      color: var(--text);
      background: var(--surface-2);
    }
    .actions .primary[disabled] {
      opacity: 0.55;
      cursor: not-allowed;
    }
  `,
})
export class UpdateBanner {
  readonly update = inject(UpdateStore);
  private readonly i18n = inject(I18nService);
  readonly t = this.i18n.t.bind(this.i18n);
}
