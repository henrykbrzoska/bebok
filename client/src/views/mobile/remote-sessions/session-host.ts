/**
 * Session host for the read-only tabs (WP-M6 / F10-26).
 *
 * The Changes / Agents / Processes panels were written for the desktop's
 * right drawer, where `ChatView` publishes the open session into
 * `ChatSessionStore`. On the phone those tabs are routes of their own with
 * no chat on screen, so this host does the publishing instead: it takes the
 * session from `MobileSessionContext` (last opened, or picked here), loads
 * its metadata into the store, keeps `running` in step with `session.updated`
 * events, and clears the store again when the tab goes away. A header shows
 * the session and opens a picker sheet with the same list the Remote tab
 * uses.
 */

import {
  ChangeDetectionStrategy,
  Component,
  DestroyRef,
  computed,
  effect,
  inject,
  signal,
  untracked,
} from '@angular/core';

import { EngineClient } from '../../../core/engine-client.service';
import { EngineEvent } from '../../../core/engine.dtos';
import { EventsStore } from '../../../core/events.store';
import { RemoteStore } from '../../../core/remote/remote.store';
import { MobileSessionContext } from '../../../core/remote/session-context';
import { I18nService } from '../../../i18n/i18n.service';
import { Sheet } from '../../../ui/mobile-shell/sheet';
import { ChatSessionStore } from '../../chat/chat-session.store';
import { RemoteSessionList } from './remote-session-list';

@Component({
  selector: 'app-mobile-session-host',
  changeDetection: ChangeDetectionStrategy.OnPush,
  imports: [Sheet, RemoteSessionList],
  template: `
    <header class="host-head" data-testid="session-host-head">
      <button
        type="button"
        class="pick"
        (click)="pickerOpen.set(true)"
        data-testid="session-host-pick"
        [attr.aria-label]="t('mobile.session.pick')"
      >
        <span class="pick-label">{{ t('mobile.session.current') }}</span>
        <span class="pick-title">{{ title() }}</span>
        <span class="pick-chevron" aria-hidden="true">▾</span>
      </button>
    </header>

    @if (!sessionID()) {
      <section class="empty" data-testid="session-host-empty">
        <p>{{ t('mobile.session.none') }}</p>
        <button type="button" class="btn-secondary" (click)="pickerOpen.set(true)">
          {{ t('mobile.session.pick') }}
        </button>
      </section>
    } @else if (error(); as err) {
      <section class="empty" data-testid="session-host-error">
        <p class="error">{{ err }}</p>
      </section>
    } @else if (!ready()) {
      <section class="empty" data-testid="session-host-loading">
        <p>{{ t('mobile.remote.loading') }}</p>
      </section>
    } @else {
      <div class="host-body">
        <ng-content />
      </div>
    }

    <app-sheet [open]="pickerOpen()" [title]="t('mobile.session.pick')" (close)="pickerOpen.set(false)">
      @if (pickerOpen()) {
        <app-remote-session-list [current]="sessionID()" (open)="pick($event)" />
      }
    </app-sheet>
  `,
  styles: [
    `
      :host {
        display: flex;
        flex-direction: column;
        flex: 1 1 auto;
        min-height: 0;
      }

      .host-head {
        flex: none;
        border-bottom: 1px solid var(--border);
        background: var(--surface);
      }

      .pick {
        display: flex;
        align-items: center;
        gap: var(--space-8);
        width: 100%;
        min-height: 44px;
        padding: 0 var(--space-16);
        border: 0;
        background: transparent;
        color: var(--text);
        font: inherit;
        text-align: left;
        cursor: pointer;
      }

      .pick-label {
        flex: none;
        font-size: var(--fs-11);
        text-transform: uppercase;
        letter-spacing: var(--label-tracking);
        color: var(--text-faint);
      }

      .pick-title {
        flex: 1 1 auto;
        min-width: 0;
        overflow: hidden;
        text-overflow: ellipsis;
        white-space: nowrap;
        font-size: var(--fs-13);
      }

      .pick-chevron {
        flex: none;
        color: var(--text-muted);
      }

      .empty {
        flex: 1 1 auto;
        display: flex;
        flex-direction: column;
        align-items: center;
        justify-content: center;
        gap: var(--space-12);
        padding: var(--space-24);
        text-align: center;
        color: var(--text-muted);
      }

      .empty p {
        margin: 0;
      }

      .error {
        color: var(--danger);
        overflow-wrap: anywhere;
      }

      .host-body {
        flex: 1 1 auto;
        min-height: 0;
        overflow-y: auto;
        display: flex;
        flex-direction: column;
      }

      .btn-secondary {
        min-height: 44px;
        padding: 0 var(--space-16);
        border: 1px solid var(--border-strong);
        border-radius: var(--radius-control);
        background: transparent;
        color: var(--text);
        font: inherit;
        font-weight: 600;
        cursor: pointer;
      }
    `,
  ],
})
export class MobileSessionHost {
  private readonly engine = inject(EngineClient);
  private readonly events = inject(EventsStore);
  private readonly store = inject(ChatSessionStore);
  private readonly context = inject(MobileSessionContext);
  private readonly remote = inject(RemoteStore);
  private readonly i18n = inject(I18nService);
  private readonly destroyRef = inject(DestroyRef);

  readonly t = this.i18n.t.bind(this.i18n);

  readonly sessionID = computed(() => this.context.sessionID());
  readonly pickerOpen = signal(false);
  readonly error = signal<string | null>(null);
  /** True once the store holds the meta of `sessionID()`. */
  readonly ready = computed(() => {
    const id = this.sessionID();
    return !!id && this.store.meta()?.id === id;
  });

  readonly title = computed(() => {
    const meta = this.store.meta();
    if (meta) {
      return meta.title || meta.alias || meta.id.slice(0, 8);
    }
    return this.context.title() ?? this.sessionID()?.slice(0, 8) ?? this.t('mobile.session.none');
  });

  private loadSeq = 0;
  private published = false;

  constructor() {
    effect(() => {
      const id = this.sessionID();
      const version = this.events.reconnectVersion();
      untracked(() => {
        void version;
        void this.load(id);
      });
    });
    // The remembered session lives on a paired desktop that is not active
    // (typical after a relaunch): follow it there, like the Remote tab does.
    effect(() => {
      const targetId = this.context.rememberedTargetId();
      untracked(() => {
        if (targetId && this.remote.desktops().some((t) => t.id === targetId)) {
          void this.remote.connect(targetId);
        }
      });
    });
    const unsubscribe = this.events.onEvent((ev) => this.handleEvent(ev));
    this.destroyRef.onDestroy(() => {
      unsubscribe();
      if (this.published) {
        this.store.clear();
      }
    });
  }

  pick(id: string): void {
    this.pickerOpen.set(false);
    if (this.published) {
      // The context prefers the published (live) session; release it first.
      this.store.clear();
      this.published = false;
    }
    this.context.remember(id);
  }

  private async load(id: string | null): Promise<void> {
    const seq = ++this.loadSeq;
    this.error.set(null);
    if (!id) {
      if (this.published) {
        this.store.clear();
        this.published = false;
      }
      return;
    }
    if (this.store.meta()?.id === id) {
      return;
    }
    if (!this.engine.connected()) {
      return;
    }
    try {
      const meta = await this.engine.sessionMeta(id);
      if (seq !== this.loadSeq) {
        return;
      }
      this.store.meta.set(meta);
      this.store.running.set(meta.running === true);
      this.published = true;
      this.context.remember(meta.id, meta.title || meta.alias || null, meta.directory);
    } catch (err) {
      if (seq === this.loadSeq) {
        this.error.set(err instanceof Error ? err.message : String(err));
      }
    }
  }

  private handleEvent(ev: EngineEvent): void {
    const id = this.sessionID();
    if (!id || ev.sessionID !== id || !this.published) {
      return;
    }
    if (ev.type === 'session.updated') {
      const running = ev.properties?.['running'];
      if (typeof running === 'boolean') {
        this.store.running.set(running);
      } else {
        void this.refreshMeta(id);
      }
    } else if (ev.type === 'turn.start' || ev.type === 'turn.started') {
      this.store.running.set(true);
    } else if (ev.type === 'turn.end' || ev.type === 'turn.ended') {
      this.store.running.set(false);
    }
  }

  private async refreshMeta(id: string): Promise<void> {
    try {
      const meta = await this.engine.sessionMeta(id);
      if (this.sessionID() === id) {
        this.store.meta.set(meta);
        this.store.running.set(meta.running === true);
      }
    } catch {
      /* keep what we have */
    }
  }
}
