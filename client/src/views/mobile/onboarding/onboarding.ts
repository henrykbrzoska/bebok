/**
 * Mobile onboarding (WP-M5 / F10-16): the first screen of the Chat tab.
 *
 * Three choices (plan §7):
 * - **Chat locally** - the embedded engine (`EngineClient.connect()` on
 *   Capacitor, the platform default elsewhere). The default; the button
 *   doubles as the *retry* when the embedded engine failed to launch
 *   (`EngineTargetStore.platformError`), which is the only error surface the
 *   store exposes.
 * - **Pair with desktop** - a route reference into WP-M6's Remote tab
 *   (`/m/remote`); nothing of the pairing flow lives here.
 * - **Enter engine address** - a persisted `remote-url` target
 *   (`EngineTargetStore.upsert` + `switchTarget`), the browser-dev path,
 *   verified with a `ping()` before it counts.
 *
 * Shown by `ChatTab` on first launch (no `bebok.mobile.onboarded` flag) and
 * whenever `platformError` is set; a completed choice emits `done` and the
 * tab persists the flag.
 */

import { ChangeDetectionStrategy, Component, computed, inject, output, signal } from '@angular/core';
import { FormsModule } from '@angular/forms';
import { Router } from '@angular/router';

import { splitEngineUrl } from '../../../core/auth.interceptor';
import { ENGINE_API } from '../../../core/engine-api';
import { EngineTargetStore } from '../../../core/engine-target.store';
import { I18nService } from '../../../i18n/i18n.service';

export const ONBOARDED_KEY = 'bebok.mobile.onboarded';

export function readOnboarded(): boolean {
  try {
    return localStorage.getItem(ONBOARDED_KEY) === '1';
  } catch {
    return false;
  }
}

export function writeOnboarded(done: boolean): void {
  try {
    if (done) {
      localStorage.setItem(ONBOARDED_KEY, '1');
    } else {
      localStorage.removeItem(ONBOARDED_KEY);
    }
  } catch {
    /* ignore */
  }
}

/** Stable target id for a typed engine address (one per host:port). */
export function remoteUrlTargetId(baseUrl: string): string {
  try {
    const url = new URL(baseUrl);
    return `remote-url:${url.host}`;
  } catch {
    return `remote-url:${baseUrl}`;
  }
}

/** Normalise a typed address: scheme defaulted to http, trailing slash cut. */
export function normalizeEngineAddress(raw: string): string {
  let value = raw.trim();
  if (!value) {
    return '';
  }
  if (!/^https?:\/\//i.test(value)) {
    if (value.includes('://')) {
      return '';
    }
    value = `http://${value}`;
  }
  try {
    const url = new URL(value);
    if (url.protocol !== 'http:' && url.protocol !== 'https:') {
      return '';
    }
  } catch {
    return '';
  }
  return value.replace(/\/+$/, '');
}

@Component({
  selector: 'app-mobile-onboarding',
  changeDetection: ChangeDetectionStrategy.OnPush,
  imports: [FormsModule],
  template: `
    <section class="ob" data-testid="onboarding">
      <header class="ob-head">
        <h2>{{ t('mobile.onboarding.title') }}</h2>
        <p>{{ t('mobile.onboarding.hint') }}</p>
      </header>

      @if (platformError(); as err) {
        <div class="ob-error" role="alert" data-testid="onboarding-platform-error">
          <strong>{{ t('mobile.onboarding.embeddedFailedTitle') }}</strong>
          <span>{{ err }}</span>
          @if (connected()) {
            <button
              type="button"
              class="ob-link"
              (click)="continueConnected()"
              data-testid="onboarding-continue"
            >
              {{ t('mobile.onboarding.continueConnected') }}
            </button>
          }
        </div>
      }

      <div class="ob-choices">
        <button
          type="button"
          class="ob-choice primary"
          (click)="chatLocally()"
          [disabled]="busy() !== null"
          data-testid="onboarding-local"
        >
          <span class="ob-choice-title">
            {{ platformError() ? t('mobile.onboarding.retryLocal') : t('mobile.onboarding.local') }}
          </span>
          <span class="ob-choice-sub">
            @if (busy() === 'local') {
              {{ t('mobile.onboarding.starting') }}
            } @else if (connected() && !platformError()) {
              {{ t('mobile.onboarding.localReady') }}
            } @else {
              {{ t('mobile.onboarding.localHint') }}
            }
          </span>
        </button>

        <button
          type="button"
          class="ob-choice"
          (click)="pairDesktop()"
          [disabled]="busy() !== null"
          data-testid="onboarding-pair"
        >
          <span class="ob-choice-title">{{ t('mobile.onboarding.pair') }}</span>
          <span class="ob-choice-sub">{{ t('mobile.onboarding.pairHint') }}</span>
        </button>

        <button
          type="button"
          class="ob-choice"
          (click)="addressOpen.set(!addressOpen())"
          [disabled]="busy() !== null"
          [attr.aria-expanded]="addressOpen()"
          data-testid="onboarding-address-toggle"
        >
          <span class="ob-choice-title">{{ t('mobile.onboarding.address') }}</span>
          <span class="ob-choice-sub">{{ t('mobile.onboarding.addressHint') }}</span>
        </button>

        @if (addressOpen()) {
          <form class="ob-address" (submit)="$event.preventDefault(); useAddress()">
            <input
              type="url"
              class="mono-input"
              inputmode="url"
              autocapitalize="off"
              autocorrect="off"
              spellcheck="false"
              [ngModel]="address()"
              (ngModelChange)="address.set($event)"
              name="address"
              [placeholder]="t('mobile.onboarding.addressPlaceholder')"
              [attr.aria-label]="t('start.engineAddress')"
              data-testid="onboarding-address"
            />
            <button
              type="submit"
              class="btn-primary"
              [disabled]="busy() !== null || !normalizedAddress()"
              data-testid="onboarding-address-connect"
            >
              {{ busy() === 'address' ? t('start.connecting') : t('start.connect') }}
            </button>
          </form>
        }
      </div>

      @if (error(); as err) {
        <p class="ob-error" role="alert" data-testid="onboarding-error">{{ err }}</p>
      }
    </section>
  `,
  styles: [
    `
      :host {
        display: flex;
        flex-direction: column;
        flex: 1 1 auto;
        min-height: 0;
        overflow-y: auto;
      }

      .ob {
        display: flex;
        flex-direction: column;
        gap: var(--space-16);
        padding: var(--space-24) var(--space-16);
      }

      .ob-head h2 {
        margin: 0 0 var(--space-6);
        font-size: var(--fs-18, 18px);
      }

      .ob-head p {
        margin: 0;
        color: var(--text-muted);
      }

      .ob-choices {
        display: flex;
        flex-direction: column;
        gap: var(--space-10);
      }

      .ob-choice {
        display: flex;
        flex-direction: column;
        align-items: flex-start;
        gap: 4px;
        min-height: 64px;
        padding: var(--space-12) var(--space-16);
        border: 1px solid var(--border);
        border-radius: var(--radius-control);
        background: var(--surface);
        color: var(--text);
        font: inherit;
        text-align: left;
        cursor: pointer;
        -webkit-tap-highlight-color: transparent;
      }

      .ob-choice:active {
        background: var(--surface-2);
      }

      .ob-choice.primary {
        border-color: var(--accent);
      }

      .ob-choice:disabled {
        opacity: 0.6;
        cursor: default;
      }

      .ob-choice-title {
        font-weight: 600;
      }

      .ob-choice-sub {
        font-size: var(--fs-12-5);
        color: var(--text-muted);
      }

      .ob-address {
        display: flex;
        flex-direction: column;
        gap: var(--space-8);
        padding: 0 var(--space-4);
      }

      .mono-input {
        min-height: 44px;
        padding: 0 var(--space-12);
        border: 1px solid var(--border);
        border-radius: var(--radius-control);
        background: var(--surface);
        color: var(--text);
        font-family: var(--font-mono);
        font-size: var(--fs-13, 13px);
      }

      .btn-primary {
        min-height: 44px;
        border: 0;
        border-radius: var(--radius-control);
        background: var(--accent);
        color: var(--bg);
        font: inherit;
        font-weight: 600;
        cursor: pointer;
      }

      .btn-primary:disabled {
        opacity: 0.5;
        cursor: default;
      }

      .ob-link {
        align-self: flex-start;
        padding: 0;
        border: 0;
        background: transparent;
        color: var(--accent);
        font: inherit;
        cursor: pointer;
      }

      .ob-error {
        display: flex;
        flex-direction: column;
        gap: 4px;
        margin: 0;
        padding: var(--space-10) var(--space-12);
        border: 1px solid var(--danger);
        border-radius: var(--radius-control);
        color: var(--danger);
        font-size: var(--fs-12-5);
        overflow-wrap: anywhere;
      }
    `,
  ],
})
export class OnboardingView {
  private readonly engine = inject(ENGINE_API);
  private readonly targets = inject(EngineTargetStore);
  private readonly router = inject(Router);
  private readonly i18n = inject(I18nService);

  readonly t = this.i18n.t.bind(this.i18n);

  /** A choice completed (the tab persists the onboarded flag). */
  readonly done = output<void>();

  readonly busy = signal<'local' | 'address' | null>(null);
  readonly error = signal<string | null>(null);
  readonly addressOpen = signal(false);
  readonly address = signal('');
  readonly normalizedAddress = computed(() => normalizeEngineAddress(this.address()));

  readonly connected = this.engine.connected;
  readonly platformError = this.targets.platformError;

  /** Chat locally: (re)connect the platform engine, then leave onboarding. */
  async chatLocally(): Promise<void> {
    this.busy.set('local');
    this.error.set(null);
    try {
      if (!this.connected() || this.platformError()) {
        if (this.platformError()) {
          // A failed embedded launch leaves `connection` unset; a retry
          // resolves the platform target again (and clears the error).
          this.engine.connection.set(null);
        }
        await this.engine.connect();
      }
      if (this.platformError()) {
        this.error.set(this.platformError());
        return;
      }
      this.done.emit();
    } catch (err) {
      this.error.set(describe(err));
    } finally {
      this.busy.set(null);
    }
  }

  /**
   * The embedded engine failed but `connect()` fell back to the previously
   * active paired desktop: let the user keep that instead of retrying.
   */
  continueConnected(): void {
    if (!this.connected()) {
      return;
    }
    this.targets.platformError.set(null);
    this.done.emit();
  }

  /** Pair with desktop: WP-M6's screen owns the flow. */
  pairDesktop(): void {
    this.done.emit();
    void this.router.navigate(['/m/remote'], { queryParams: { pair: 1 } });
  }

  /** Manual engine address: a persisted `remote-url` target, verified by a ping. */
  async useAddress(): Promise<void> {
    const raw = this.normalizedAddress();
    if (!raw || this.busy()) {
      return;
    }
    this.busy.set('address');
    this.error.set(null);
    const { baseUrl, token } = splitEngineUrl(raw);
    const id = remoteUrlTargetId(baseUrl);
    const previous = this.targets.activeId();
    try {
      this.targets.upsert({ id, kind: 'remote-url', label: baseUrl, baseUrl, token });
      await this.engine.switchTarget(id);
      await this.engine.ping();
      if (this.engine.unauthorized()) {
        throw new Error(this.t('mobile.onboarding.unauthorized'));
      }
      this.targets.markOk(id);
      this.done.emit();
    } catch (err) {
      this.error.set(describe(err));
      this.targets.remove(id);
      if (previous && previous !== id) {
        try {
          await this.engine.switchTarget(previous);
        } catch {
          /* the previous target is gone too - stay unconnected */
        }
      }
    } finally {
      this.busy.set(null);
    }
  }
}

function describe(err: unknown): string {
  return err instanceof Error ? err.message : String(err);
}
