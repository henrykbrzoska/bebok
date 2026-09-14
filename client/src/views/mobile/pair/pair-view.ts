/**
 * Pairing screen (WP-M6 / F10-22, F10-23): scan a QR (or take a deep link /
 * typed code + address), find the reachable endpoint, confirm
 * "Pair with {engineName}?", wait for the desktop's click, land on the paired
 * desktop.
 *
 * The state lives in `PairingFlow`; this component is the phone-shaped face
 * of it: one column, 44 px targets, every failure code its own sentence, and
 * a visible elapsed timer while the engine long-polls (up to 90 s).
 */

import {
  ChangeDetectionStrategy,
  Component,
  DestroyRef,
  computed,
  effect,
  inject,
  input,
  output,
  signal,
  untracked,
} from '@angular/core';
import { FormsModule } from '@angular/forms';

import { hostClass } from '../../../core/remote/endpoint-probe';
import {
  PAIR_CODE_RE,
  PairInvite,
  PairUrlError,
  normalizeEndpoint,
  normalizePairCode,
  parsePairUrl,
} from '../../../core/remote/pair-protocol';
import { PairingFailure, PairingFlow } from '../../../core/remote/pairing';
import { QrScanner } from '../../../core/remote/qr-scanner';
import { RemoteStore } from '../../../core/remote/remote.store';
import { I18nService } from '../../../i18n/i18n.service';
import type { MessageKey } from '../../../i18n';

const FAILURE_KEY: Record<PairingFailure['code'], MessageKey> = {
  pair_rejected: 'mobile.pair.errRejected',
  pair_invalid_code: 'mobile.pair.errInvalidCode',
  pair_already_requested: 'mobile.pair.errAlreadyRequested',
  pair_expired: 'mobile.pair.errExpired',
  pair_locked: 'mobile.pair.errLocked',
  pair_timeout: 'mobile.pair.errTimeout',
  unreachable: 'mobile.pair.errUnreachable',
  unknown: 'mobile.pair.errUnknown',
  probe_no_route: 'mobile.pair.errNoRoute',
  probe_bad_endpoint: 'mobile.pair.errBadEndpoint',
  endpoint_not_private: 'mobile.pair.errNotPrivate',
};

const URL_ERROR_KEY: Record<PairUrlError['reason'], MessageKey> = {
  not_pair_url: 'mobile.pair.urlNotPair',
  unsupported_version: 'mobile.pair.urlVersion',
  missing_code: 'mobile.pair.urlMissingCode',
  missing_endpoints: 'mobile.pair.urlMissingEndpoints',
  bad_endpoint: 'mobile.pair.urlBadEndpoint',
};

@Component({
  selector: 'app-pair-view',
  changeDetection: ChangeDetectionStrategy.OnPush,
  imports: [FormsModule],
  template: `
    <section class="pair" data-testid="pair-view" [attr.data-phase]="flow.phase()">
      @switch (flow.phase()) {
        @case ('idle') {
          <h2 class="title">{{ t('mobile.pair.title') }}</h2>
          <p class="hint">{{ t('mobile.pair.hint') }}</p>

          @if (scanner.isNative()) {
            <button
              type="button"
              class="btn-primary"
              (click)="scan()"
              [disabled]="scanning()"
              data-testid="pair-scan"
            >
              {{ scanning() ? t('mobile.pair.scanning') : t('mobile.pair.scan') }}
            </button>
          }
          @if (scanError(); as err) {
            <p class="error" role="alert" data-testid="pair-scan-error">{{ err }}</p>
          }

          <div class="divider">
            <span>{{ t('mobile.pair.or') }}</span>
          </div>

          <form class="manual" (ngSubmit)="submitManual()" data-testid="pair-manual">
            <label class="field">
              <span>{{ t('mobile.pair.code') }}</span>
              <input
                type="text"
                class="mono-input code"
                autocapitalize="characters"
                autocomplete="off"
                spellcheck="false"
                maxlength="9"
                [ngModel]="code()"
                (ngModelChange)="code.set($event)"
                name="code"
                placeholder="ABCD-EFGH"
                data-testid="pair-code"
              />
            </label>
            <label class="field">
              <span>{{ t('mobile.pair.endpoint') }}</span>
              <input
                type="text"
                class="mono-input"
                autocapitalize="off"
                autocomplete="off"
                spellcheck="false"
                inputmode="url"
                [ngModel]="endpoint()"
                (ngModelChange)="endpoint.set($event)"
                name="endpoint"
                placeholder="100.64.0.7:8790"
                data-testid="pair-endpoint"
              />
            </label>
            @if (manualError(); as err) {
              <p class="error" role="alert" data-testid="pair-manual-error">{{ err }}</p>
            }
            <button
              type="submit"
              class="btn-secondary"
              [disabled]="!code().trim() || !endpoint().trim()"
              data-testid="pair-continue"
            >
              {{ t('mobile.pair.continue') }}
            </button>
          </form>
        }

        @case ('probing') {
          <h2 class="title">{{ t('mobile.pair.probing') }}</h2>
          <p class="hint" data-testid="pair-probing">
            {{ t('mobile.pair.probingHint', { n: probingCount() }) }}
          </p>
          <span class="spinner" aria-hidden="true"></span>
          <button type="button" class="btn-secondary" (click)="cancel()" data-testid="pair-cancel">
            {{ t('mobile.pair.cancel') }}
          </button>
        }

        @case ('confirm') {
          @if (flow.candidate(); as c) {
            <h2 class="title" data-testid="pair-confirm-title">
              {{ t('mobile.pair.confirmTitle', { name: c.engineName }) }}
            </h2>
            <dl class="facts">
              <dt>{{ t('mobile.pair.endpoint') }}</dt>
              <dd class="mono" data-testid="pair-confirm-endpoint">{{ c.endpoint }}</dd>
              @if (c.fingerprint) {
                <dt>{{ t('mobile.pair.fingerprint') }}</dt>
                <dd class="mono">{{ c.fingerprint }}</dd>
              }
              <dt>{{ t('mobile.pair.code') }}</dt>
              <dd class="mono">{{ c.code }}</dd>
            </dl>
            <label class="field">
              <span>{{ t('mobile.pair.deviceName') }}</span>
              <input
                type="text"
                [ngModel]="deviceName()"
                (ngModelChange)="deviceName.set($event)"
                name="deviceName"
                maxlength="64"
                data-testid="pair-device-name"
              />
            </label>
            <button type="button" class="btn-primary" (click)="pair()" data-testid="pair-confirm">
              {{ t('mobile.pair.pair') }}
            </button>
            <button type="button" class="btn-secondary" (click)="back()" data-testid="pair-back">
              {{ t('mobile.pair.back') }}
            </button>
          }
        }

        @case ('waiting') {
          <h2 class="title">{{ t('mobile.pair.waitingTitle') }}</h2>
          <p class="hint" data-testid="pair-waiting">
            {{ t('mobile.pair.waitingHint', { name: flow.candidate()?.engineName ?? '' }) }}
          </p>
          <p class="elapsed" data-testid="pair-elapsed">{{ elapsedLabel() }}</p>
          <span class="spinner" aria-hidden="true"></span>
          <button type="button" class="btn-secondary" (click)="cancel()" data-testid="pair-cancel">
            {{ t('mobile.pair.cancel') }}
          </button>
        }

        @case ('paired') {
          <h2 class="title" data-testid="pair-done">
            {{ t('mobile.pair.pairedTitle', { name: flow.candidate()?.engineName ?? '' }) }}
          </h2>
          <p class="hint">{{ t('mobile.pair.pairedHint') }}</p>
          <button type="button" class="btn-primary" (click)="finish()" data-testid="pair-finish">
            {{ t('mobile.pair.openSessions') }}
          </button>
        }

        @case ('error') {
          @if (flow.failure(); as f) {
            <h2 class="title">{{ t('mobile.pair.errorTitle') }}</h2>
            <p class="error" role="alert" data-testid="pair-error" [attr.data-code]="f.code">
              {{ failureMessage(f) }}
            </p>
            @if (f.code === 'probe_no_route') {
              <button
                type="button"
                class="btn-secondary"
                (click)="remote.openTailscale()"
                data-testid="pair-open-tailscale"
              >
                {{ t('mobile.remote.openTailscale') }}
              </button>
            }
            @if (f.retryable && flow.candidate()) {
              <button type="button" class="btn-primary" (click)="retry()" data-testid="pair-retry">
                {{ t('mobile.pair.retry') }}
              </button>
            }
            <button type="button" class="btn-secondary" (click)="back()" data-testid="pair-back">
              {{ t('mobile.pair.startOver') }}
            </button>
          }
        }
      }
    </section>
    @if (scanner.active()) {
      <div class="scan-overlay" data-testid="pair-scan-overlay">
        <div class="scan-frame" aria-hidden="true"></div>
        <p class="scan-hint">{{ t('mobile.pair.scanHint') }}</p>
        <button
          type="button"
          class="btn-secondary scan-cancel"
          (click)="scanner.cancel()"
          data-testid="pair-scan-cancel"
        >
          {{ t('mobile.pair.scanCancel') }}
        </button>
      </div>
    }
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

      .pair {
        display: flex;
        flex-direction: column;
        gap: var(--space-12);
        padding: var(--space-24) var(--space-16);
        max-width: 480px;
        width: 100%;
        margin: 0 auto;
        box-sizing: border-box;
      }

      .title {
        margin: 0;
        font-size: var(--fs-16);
        text-align: center;
      }

      .hint {
        margin: 0;
        color: var(--text-muted);
        text-align: center;
        font-size: var(--fs-13);
      }

      .error {
        margin: 0;
        color: var(--danger);
        font-size: var(--fs-12-5);
        overflow-wrap: anywhere;
      }

      .divider {
        display: flex;
        align-items: center;
        gap: var(--space-8);
        color: var(--text-faint);
        font-size: var(--fs-11);
        text-transform: uppercase;
        letter-spacing: var(--label-tracking);
      }

      .divider::before,
      .divider::after {
        content: '';
        flex: 1 1 auto;
        border-top: 1px solid var(--border);
      }

      .manual {
        display: flex;
        flex-direction: column;
        gap: var(--space-10);
      }

      .field {
        display: flex;
        flex-direction: column;
        gap: var(--space-6);
        font-size: var(--fs-12);
        color: var(--text-muted);
      }

      .field input {
        min-height: 44px;
        padding: 0 var(--space-12);
        border: 1px solid var(--border-strong);
        border-radius: var(--radius-control);
        background: var(--surface);
        color: var(--text);
        font: inherit;
        font-size: var(--fs-14);
      }

      .field input.code {
        font-family: var(--font-mono);
        letter-spacing: 0.15em;
        text-transform: uppercase;
      }

      .facts {
        display: grid;
        grid-template-columns: auto 1fr;
        gap: var(--space-4) var(--space-12);
        margin: 0;
        padding: var(--space-12);
        border: 1px solid var(--border);
        border-radius: var(--radius-panel);
        background: var(--surface);
        font-size: var(--fs-12-5);
      }

      .facts dt {
        color: var(--text-faint);
      }

      .facts dd {
        margin: 0;
        overflow-wrap: anywhere;
      }

      .mono {
        font-family: var(--font-mono);
      }

      .elapsed {
        margin: 0;
        text-align: center;
        font-family: var(--font-mono);
        font-size: var(--fs-14);
        color: var(--text);
      }

      .spinner {
        align-self: center;
        width: 28px;
        height: 28px;
        border: 3px solid var(--border);
        border-top-color: var(--accent);
        border-radius: 50%;
        animation: spin 0.9s linear infinite;
      }

      @keyframes spin {
        to {
          transform: rotate(360deg);
        }
      }

      .btn-primary,
      .btn-secondary {
        min-height: 44px;
        border-radius: var(--radius-control);
        font: inherit;
        font-weight: 600;
        cursor: pointer;
      }

      .btn-primary {
        border: 0;
        background: var(--accent);
        color: var(--bg);
      }

      .btn-secondary {
        border: 1px solid var(--border-strong);
        background: transparent;
        color: var(--text);
      }

      .btn-primary:disabled,
      .btn-secondary:disabled {
        opacity: 0.5;
        cursor: default;
      }

      /* Drawn over the native camera preview (the rest of the page is hidden
         by body.barcode-scanner-active, see styles.css). */
      .scan-overlay {
        position: fixed;
        inset: 0;
        z-index: 1000;
        visibility: visible;
        display: flex;
        flex-direction: column;
        align-items: center;
        justify-content: center;
        gap: var(--space-16);
        padding: var(--space-24);
        background: transparent;
      }

      .scan-frame {
        width: min(70vw, 280px);
        aspect-ratio: 1;
        border: 3px solid #fff;
        border-radius: 16px;
        box-shadow: 0 0 0 100vmax rgba(0, 0, 0, 0.45);
      }

      .scan-hint {
        color: #fff;
        text-align: center;
        text-shadow: 0 1px 2px rgba(0, 0, 0, 0.8);
        margin: 0;
      }

      .scan-cancel {
        position: fixed;
        left: var(--space-24);
        right: var(--space-24);
        bottom: calc(var(--space-24) + env(safe-area-inset-bottom));
        background: rgba(0, 0, 0, 0.6);
        color: #fff;
        border-color: #fff;
      }
    `,
  ],
})
export class PairView {
  readonly flow = inject(PairingFlow);
  readonly scanner = inject(QrScanner);
  readonly remote = inject(RemoteStore);
  private readonly i18n = inject(I18nService);
  private readonly destroyRef = inject(DestroyRef);

  readonly t = this.i18n.t.bind(this.i18n);

  /** An invite from a deep link / scan handed in by the parent; probed on arrival. */
  readonly invite = input<PairInvite | null>(null);
  readonly paired = output<string>();
  readonly cancelled = output<void>();

  readonly code = signal('');
  readonly endpoint = signal('');
  readonly deviceName = signal('');
  readonly manualError = signal<string | null>(null);
  readonly scanError = signal<string | null>(null);
  readonly scanning = signal(false);
  readonly probingCount = signal(0);

  private readonly elapsed = signal(0);
  readonly elapsedLabel = computed(() => {
    const s = this.elapsed();
    return `${Math.floor(s / 60)}:${String(s % 60).padStart(2, '0')}`;
  });

  private ticker: ReturnType<typeof setInterval> | null = null;
  private lastInvite: PairInvite | null = null;

  constructor() {
    this.flow.reset();
    this.deviceName.set(this.t('mobile.pair.defaultDeviceName'));
    effect(() => {
      const invite = this.invite();
      untracked(() => {
        if (invite && invite !== this.lastInvite) {
          this.lastInvite = invite;
          void this.start(invite);
        }
      });
    });
    effect(() => {
      const since = this.flow.waitingSince();
      untracked(() => (since === null ? this.stopTicker() : this.startTicker(since)));
    });
    this.destroyRef.onDestroy(() => {
      this.stopTicker();
      this.flow.cancel();
    });
  }

  async scan(): Promise<void> {
    this.scanError.set(null);
    this.scanning.set(true);
    try {
      const outcome = await this.scanner.scan();
      if (outcome.kind === 'scanned') {
        this.acceptUrl(outcome.value);
      } else if (outcome.kind === 'unsupported') {
        this.scanError.set(this.t('mobile.pair.scanUnsupported', { reason: outcome.reason }));
      }
    } finally {
      this.scanning.set(false);
    }
  }

  /** A raw `bebok://pair?…` string (scan result, pasted link). */
  acceptUrl(url: string): void {
    try {
      const invite = parsePairUrl(url);
      void this.start(invite);
    } catch (err) {
      const key =
        err instanceof PairUrlError ? URL_ERROR_KEY[err.reason] : 'mobile.pair.urlNotPair';
      this.scanError.set(this.t(key));
    }
  }

  submitManual(): void {
    this.manualError.set(null);
    const code = normalizePairCode(this.code());
    if (!PAIR_CODE_RE.test(code)) {
      this.manualError.set(this.t('mobile.pair.codeFormat'));
      return;
    }
    const raw = this.endpoint().trim();
    // A pasted `bebok://` link in the address field is accepted too.
    if (raw.startsWith('bebok:')) {
      this.acceptUrl(raw);
      return;
    }
    const endpoint = normalizeEndpoint(raw);
    if (!endpoint) {
      this.manualError.set(this.t('mobile.pair.endpointFormat'));
      return;
    }
    if (hostClass(new URL(endpoint).hostname) === 'public' && endpoint.startsWith('http:')) {
      this.manualError.set(this.t('mobile.pair.errNotPrivate'));
      return;
    }
    void this.start({ version: 1, endpoints: [endpoint], code, fingerprint: null });
  }

  async pair(): Promise<void> {
    const name = this.deviceName().trim() || this.t('mobile.pair.defaultDeviceName');
    const ok = await this.flow.pair(name);
    if (ok && this.flow.pairedTargetId()) {
      // Stay on the "paired" card until the user taps through, but tell the
      // parent right away so it can refresh its target list.
      this.paired.emit(this.flow.pairedTargetId()!);
    }
  }

  finish(): void {
    const id = this.flow.pairedTargetId();
    this.flow.reset();
    if (id) {
      this.paired.emit(id);
    }
  }

  cancel(): void {
    this.flow.cancel();
    if (this.flow.phase() === 'idle') {
      this.cancelled.emit();
    }
  }

  retry(): void {
    this.flow.retry();
  }

  back(): void {
    this.flow.reset();
    this.lastInvite = null;
  }

  failureMessage(f: PairingFailure): string {
    const base = this.t(FAILURE_KEY[f.code]);
    return f.code === 'unknown' || f.code === 'probe_bad_endpoint' ? `${base} (${f.detail})` : base;
  }

  private async start(invite: PairInvite): Promise<void> {
    this.scanError.set(null);
    this.manualError.set(null);
    this.probingCount.set(invite.endpoints.length);
    await this.flow.probe(invite);
  }

  private startTicker(since: number): void {
    this.stopTicker();
    this.elapsed.set(Math.floor((Date.now() - since) / 1000));
    this.ticker = setInterval(() => {
      this.elapsed.set(Math.floor((Date.now() - since) / 1000));
    }, 1000);
  }

  private stopTicker(): void {
    if (this.ticker !== null) {
      clearInterval(this.ticker);
      this.ticker = null;
    }
  }
}
