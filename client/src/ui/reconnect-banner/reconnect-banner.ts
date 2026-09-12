/**
 * Blocking "engine token rejected" prompt.
 *
 * The engine mints a fresh capability token on every launch, so after an
 * engine restart in browser mode every request quietly comes back 401 with
 * the token the client still holds. `EngineClient.unauthorized` flips on the
 * first such answer; this overlay then covers the shell (it cannot be
 * dismissed - nothing works until the token is right) and asks for the new
 * `BEBOK_READY` address, using the same address form as the Start screen.
 * On the desktop shell there is nothing to paste: the sidecar's current
 * address is re-read from Tauri instead.
 */

import { ChangeDetectionStrategy, Component, inject, signal } from '@angular/core';
import { FormsModule } from '@angular/forms';

import { EngineClient } from '../../core/engine-client.service';
import { EventsStore } from '../../core/events.store';
import { I18nService } from '../../i18n/i18n.service';

@Component({
  selector: 'app-reconnect-banner',
  changeDetection: ChangeDetectionStrategy.OnPush,
  imports: [FormsModule],
  template: `
    @if (engine.unauthorized()) {
      <div
        class="backdrop"
        role="dialog"
        aria-modal="true"
        aria-labelledby="reconnect-title"
        data-testid="reconnect-banner"
      >
        <form class="card" (submit)="reconnect($event)">
          <h2 id="reconnect-title">{{ t('reconnect.title') }}</h2>
          <p class="hint">{{ isTauri() ? t('reconnect.tauriHint') : t('reconnect.hint') }}</p>
          @if (!isTauri()) {
            <label class="field">
              <span class="field-label">{{ t('start.engineAddress') }}</span>
              <input
                type="text"
                class="mono-input"
                name="address"
                [ngModel]="address()"
                (ngModelChange)="address.set($event)"
                placeholder="http://127.0.0.1:8787/?token=…"
                spellcheck="false"
                autofocus
                data-testid="reconnect-address"
              />
            </label>
          }
          @if (error(); as message) {
            <p class="error" role="alert">{{ message }}</p>
          }
          <div class="actions">
            <button type="submit" class="primary" [disabled]="busy()" data-testid="reconnect-submit">
              {{ busy() ? t('start.connecting') : t('reconnect.button') }}
            </button>
          </div>
        </form>
      </div>
    }
  `,
  styles: `
    .backdrop {
      position: fixed;
      inset: 0;
      z-index: 1000;
      display: flex;
      align-items: center;
      justify-content: center;
      background: rgba(0, 0, 0, 0.55);
      backdrop-filter: blur(2px);
    }
    .card {
      width: min(560px, calc(100vw - 2 * var(--space-24)));
      background: var(--surface);
      border: 1px solid var(--danger);
      border-radius: var(--radius-card);
      padding: 22px;
      margin: 0;
    }
    h2 {
      margin: 0 0 var(--space-6);
      font-size: var(--fs-16);
      font-weight: 600;
    }
    .hint {
      margin: 0 0 var(--space-16);
      font-size: var(--fs-12-5);
      color: var(--text-muted);
      line-height: 1.5;
    }
    .field {
      display: block;
      margin-bottom: var(--space-14);
    }
    .field-label {
      display: block;
      font-size: var(--fs-11-5);
      color: var(--text-muted);
      margin-bottom: var(--space-6);
    }
    .mono-input {
      width: 100%;
      box-sizing: border-box;
      background: var(--bg);
      border: 1px solid var(--border);
      color: var(--text);
      border-radius: var(--radius-control);
      padding: var(--space-9) 11px;
      font-family: var(--font-mono);
      font-size: var(--fs-12-5);
      outline: none;
    }
    .mono-input:focus {
      border-color: var(--accent);
    }
    .error {
      margin: 0 0 var(--space-12);
      font-size: var(--fs-12-5);
      color: var(--danger);
    }
    .actions {
      display: flex;
      gap: var(--space-10);
    }
    .actions .primary {
      border-radius: var(--radius-control);
      padding: var(--space-9) var(--space-16);
      font-size: var(--fs-13);
    }
  `,
})
export class ReconnectBanner {
  readonly engine = inject(EngineClient);
  private readonly events = inject(EventsStore);
  private readonly i18n = inject(I18nService);
  readonly t = this.i18n.t.bind(this.i18n);

  readonly isTauri = this.engine.isTauri;
  /** The pasted `BEBOK_READY` line; prefilled with the last known address. */
  readonly address = signal(this.engine.remoteDefaults().baseUrl);
  readonly busy = signal(false);
  readonly error = signal<string | null>(null);

  async reconnect(event?: Event): Promise<void> {
    event?.preventDefault();
    if (this.busy()) {
      return;
    }
    this.busy.set(true);
    this.error.set(null);
    try {
      await this.engine.reconnect(this.isTauri() ? undefined : this.address());
      // The parked SSE stream resumes against the new token; its
      // `reconnectVersion` bump makes open views re-sync.
      this.events.restart();
    } catch (err) {
      this.error.set(
        this.engine.unauthorized()
          ? this.t('reconnect.rejected')
          : err instanceof Error
            ? err.message
            : String(err),
      );
    } finally {
      this.busy.set(false);
    }
  }
}
