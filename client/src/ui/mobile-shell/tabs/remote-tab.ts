/**
 * Remote tab (WP-M6 / F10-22..F10-27): the phone's window onto a paired
 * desktop engine.
 *
 * - no paired desktop      -> the pairing screen (`PairView`) is the tab,
 * - `/m/remote`            -> desktop header (link state, connect/unpair) +
 *                             the desktop's session list,
 * - `/m/remote/:sessionID` -> the session mirror (`RemoteSessionView`),
 * - `?pair=<bebok://…>`    -> pairing with that invite (deep link / e2e),
 * - device revoked         -> "device removed" card with a re-pair action.
 *
 * Entering the tab with a paired desktop that is not the active target
 * switches to it (`RemoteStore.connect()`): the user came here for the
 * desktop. The Chat tab (WP-M5) decides on its own what the phone's engine
 * is; both go through the same `switchTarget`.
 */

import { ChangeDetectionStrategy, Component, computed, effect, inject, signal, untracked } from '@angular/core';
import { toSignal } from '@angular/core/rxjs-interop';
import { ActivatedRoute, Router } from '@angular/router';
import { map } from 'rxjs';

import { PairDeepLinks } from '../../../core/remote/deep-link';
import { PairInvite, parsePairUrl } from '../../../core/remote/pair-protocol';
import { RemoteStore } from '../../../core/remote/remote.store';
import { I18nService } from '../../../i18n/i18n.service';
import type { MessageKey } from '../../../i18n';
import { PairView } from '../../../views/mobile/pair/pair-view';
import { RemoteSessionList } from '../../../views/mobile/remote-sessions/remote-session-list';
import { RemoteSessionView } from '../../../views/mobile/remote-sessions/remote-session-view';
import { Sheet } from '../sheet';

@Component({
  selector: 'app-remote-tab',
  changeDetection: ChangeDetectionStrategy.OnPush,
  imports: [PairView, RemoteSessionList, RemoteSessionView, Sheet],
  template: `
    @if (remote.revoked(); as label) {
      <section class="m-empty" data-testid="remote-revoked">
        <h2>{{ t('mobile.remote.revokedTitle') }}</h2>
        <p>{{ t('mobile.remote.revokedHint', { name: label }) }}</p>
        <button type="button" class="btn-primary" (click)="startPairing()" data-testid="remote-pair-again">
          {{ t('mobile.remote.pairAgain') }}
        </button>
        <button type="button" class="btn-secondary" (click)="dismissRevoked()">
          {{ t('mobile.remote.dismiss') }}
        </button>
      </section>
    } @else if (sessionID(); as id) {
      <app-remote-session-view [sessionID]="id" />
    } @else if (pairing()) {
      <app-pair-view [invite]="invite()" (paired)="onPaired()" (cancelled)="pairing.set(false)" />
    } @else if (!remote.desktop()) {
      <app-pair-view (paired)="onPaired()" />
    } @else {
      <header class="desk" data-testid="remote-desktop-head" [attr.data-link]="remote.link()">
        <span class="desk-dot" [attr.data-link]="remote.link()" aria-hidden="true"></span>
        <span class="desk-main">
          <span class="desk-name">{{ remote.desktop()!.label }}</span>
          <span class="desk-meta">{{ remote.desktop()!.baseUrl }} · {{ linkLabel() }}</span>
        </span>
        @if (!remote.active()) {
          <button type="button" class="desk-action" (click)="remote.connect()" data-testid="remote-connect">
            {{ t('mobile.remote.connect') }}
          </button>
        }
        <button
          type="button"
          class="desk-more"
          (click)="menuOpen.set(true)"
          [attr.aria-label]="t('mobile.overflow')"
          data-testid="remote-menu"
        >⋯</button>
      </header>

      @if (remote.offline()) {
        <div class="offline" role="status" data-testid="remote-offline">
          <span>{{ t('mobile.remote.offlineBanner') }}</span>
          <button type="button" class="link" (click)="remote.openTailscale()" data-testid="remote-open-tailscale">
            {{ t('mobile.remote.openTailscale') }}
          </button>
        </div>
      }

      <div class="body">
        <app-remote-session-list (open)="openSession($event)" />
      </div>

      <app-sheet [open]="menuOpen()" [title]="remote.desktop()!.label" (close)="menuOpen.set(false)">
        <ul class="menu">
          @if (remote.active()) {
            <li>
              <button type="button" (click)="disconnect()" data-testid="remote-disconnect">
                {{ t('mobile.remote.disconnect') }}
              </button>
            </li>
          }
          <li>
            <button type="button" (click)="startPairing()" data-testid="remote-pair-another">
              {{ t('mobile.remote.pairAnother') }}
            </button>
          </li>
          <li>
            <button type="button" class="danger" (click)="unpair()" data-testid="remote-unpair">
              {{ t('mobile.remote.unpair') }}
            </button>
          </li>
        </ul>
      </app-sheet>
    }
  `,
  styles: [
    `
      :host {
        display: flex;
        flex-direction: column;
        flex: 1 1 auto;
        min-height: 0;
      }

      .m-empty {
        flex: 1 1 auto;
        display: flex;
        flex-direction: column;
        align-items: stretch;
        justify-content: center;
        gap: var(--space-12);
        padding: var(--space-24);
        text-align: center;
      }

      .m-empty h2 {
        margin: 0;
        font-size: var(--fs-16);
      }

      .m-empty p {
        margin: 0;
        color: var(--text-muted);
      }

      .desk {
        flex: none;
        display: flex;
        align-items: center;
        gap: var(--space-10);
        min-height: 52px;
        padding: var(--space-6) var(--space-12) var(--space-6) var(--space-16);
        border-bottom: 1px solid var(--border);
        background: var(--surface);
      }

      .desk-dot {
        flex: none;
        width: 9px;
        height: 9px;
        border-radius: 50%;
        background: var(--border-strong);
      }

      .desk-dot[data-link='live'] {
        background: var(--success);
      }

      .desk-dot[data-link='connecting'] {
        background: var(--warning);
      }

      .desk-dot[data-link='offline'] {
        background: var(--danger);
      }

      .desk-main {
        flex: 1 1 auto;
        min-width: 0;
        display: flex;
        flex-direction: column;
      }

      .desk-name {
        font-size: var(--fs-14);
        font-weight: 600;
        overflow: hidden;
        text-overflow: ellipsis;
        white-space: nowrap;
      }

      .desk-meta {
        font-size: var(--fs-11-5);
        color: var(--text-faint);
        font-family: var(--font-mono);
        overflow: hidden;
        text-overflow: ellipsis;
        white-space: nowrap;
      }

      .desk-action {
        flex: none;
        min-height: 36px;
        padding: 0 var(--space-12);
        border: 1px solid var(--accent);
        border-radius: 999px;
        background: transparent;
        color: var(--accent);
        font: inherit;
        font-weight: 600;
        cursor: pointer;
      }

      .desk-more {
        flex: none;
        width: 40px;
        min-height: 40px;
        border: 0;
        background: transparent;
        color: var(--text-muted);
        font: inherit;
        font-size: var(--fs-16);
        cursor: pointer;
      }

      .offline {
        flex: none;
        display: flex;
        align-items: center;
        justify-content: space-between;
        gap: var(--space-8);
        padding: var(--space-6) var(--space-16);
        border-bottom: 1px solid var(--border);
        color: var(--warning);
        font-size: var(--fs-12);
      }

      .link {
        border: 0;
        background: transparent;
        color: var(--accent);
        font: inherit;
        font-size: var(--fs-12);
        text-decoration: underline;
        cursor: pointer;
        padding: 0;
        white-space: nowrap;
      }

      .body {
        flex: 1 1 auto;
        min-height: 0;
        overflow-y: auto;
      }

      .menu {
        list-style: none;
        margin: 0;
        padding: 0;
        display: flex;
        flex-direction: column;
      }

      .menu button {
        width: 100%;
        min-height: 48px;
        padding: 0 var(--space-4);
        border: 0;
        border-bottom: 1px solid var(--border);
        background: transparent;
        color: var(--text);
        font: inherit;
        text-align: left;
        cursor: pointer;
      }

      .menu button.danger {
        color: var(--danger);
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
    `,
  ],
})
export class RemoteTab {
  readonly remote = inject(RemoteStore);
  private readonly route = inject(ActivatedRoute);
  private readonly router = inject(Router);
  private readonly deepLinks = inject(PairDeepLinks);
  private readonly i18n = inject(I18nService);

  readonly t = this.i18n.t.bind(this.i18n);

  readonly sessionID = toSignal(this.route.paramMap.pipe(map((p) => p.get('sessionID') ?? '')), {
    initialValue: this.route.snapshot.paramMap.get('sessionID') ?? '',
  });

  /** `?pair=<url>` from a deep link / e2e (URL-encoded `bebok://pair?…`). */
  private readonly pairParam = toSignal(this.route.queryParamMap.pipe(map((p) => p.get('pair'))), {
    initialValue: this.route.snapshot.queryParamMap.get('pair'),
  });

  readonly pairing = signal(false);
  readonly invite = signal<PairInvite | null>(null);
  readonly menuOpen = signal(false);

  readonly linkLabel = computed(() => {
    const key: MessageKey = (() => {
      switch (this.remote.link()) {
        case 'live':
          return 'status.live';
        case 'connecting':
          return 'status.connecting';
        case 'offline':
          return 'mobile.remote.linkOffline';
        default:
          return 'mobile.remote.linkInactive';
      }
    })();
    return this.t(key);
  });

  constructor() {
    void this.deepLinks.install();
    // Invite handed over by URL (`?pair=`) or by the native deep-link hook.
    effect(() => {
      const raw = this.pairParam();
      const pending = this.deepLinks.pending();
      untracked(() => {
        let invite: PairInvite | null = null;
        if (raw) {
          try {
            invite = parsePairUrl(raw);
          } catch {
            invite = null;
          }
        }
        invite ??= pending ? this.deepLinks.consume() : null;
        if (invite || raw) {
          // `?pair=<bebok://…>` carries an invite; a bare `?pair=1` (WP-M5's
          // onboarding "Pair with desktop") just opens the pairing screen.
          this.invite.set(invite);
          this.pairing.set(true);
          if (raw) {
            // Consume the parameter so a reload does not re-pair.
            void this.router.navigate([], {
              relativeTo: this.route,
              queryParams: { pair: null },
              queryParamsHandling: 'merge',
              replaceUrl: true,
            });
          }
        }
      });
    });
    // A paired desktop that is not active: the tab is the reason to switch.
    effect(() => {
      const desktop = this.remote.desktop();
      const active = this.remote.active();
      const revoked = this.remote.revoked();
      untracked(() => {
        if (desktop && !active && !revoked && !this.pairing()) {
          void this.remote.connect(desktop.id);
        }
      });
    });
  }

  startPairing(): void {
    this.menuOpen.set(false);
    this.remote.acknowledgeRevoked();
    this.invite.set(null);
    this.pairing.set(true);
    if (this.sessionID()) {
      void this.router.navigate(['/m/remote']);
    }
  }

  dismissRevoked(): void {
    this.remote.acknowledgeRevoked();
    if (this.sessionID()) {
      void this.router.navigate(['/m/remote']);
    }
  }

  onPaired(): void {
    this.pairing.set(false);
    this.invite.set(null);
    void this.router.navigate(['/m/remote']);
  }

  openSession(id: string): void {
    void this.router.navigate(['/m/remote', id]);
  }

  async disconnect(): Promise<void> {
    this.menuOpen.set(false);
    await this.remote.disconnect();
    await this.router.navigate(['/m/chat']);
  }

  async unpair(): Promise<void> {
    this.menuOpen.set(false);
    const desktop = this.remote.desktop();
    if (!desktop) {
      return;
    }
    if (!confirm(this.t('mobile.remote.unpairConfirm', { name: desktop.label }))) {
      return;
    }
    await this.remote.unpair(desktop.id);
  }
}
