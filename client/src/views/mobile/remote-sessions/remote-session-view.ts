/**
 * Remote session mirror (WP-M6 / F10-24, F10-25, F10-27): the phone-shaped
 * wrapper around the shared `ChatView` for `/m/remote/:sessionID`.
 *
 * `ChatView` reads the session id from the same `ActivatedRoute`, streams
 * over the active target's SSE and owns the composer, so the mirror itself
 * only adds what a phone needs on top:
 *
 * - the permission prompt as a bottom sheet (`PermissionPopup` in `sheet`
 *   mode; the inline copy `ChatView` renders in the transcript is hidden by
 *   CSS so the ask appears once),
 * - a sticky abort bar while a turn is running (`POST /session/{id}/abort`),
 * - an offline banner with the queued/expired commands (F10-27) and the
 *   cached tail of the transcript when the desktop cannot be reached,
 * - transcript caching (last 200 messages) whenever the live transcript
 *   changes.
 *
 * Desktop-only chrome of the toolbar (compaction, the model/thinking
 * selects) is hidden by width, not removed - the same component runs on both.
 */

import {
  ChangeDetectionStrategy,
  Component,
  DestroyRef,
  computed,
  effect,
  inject,
  input,
  signal,
  untracked,
} from '@angular/core';

import { EngineClient } from '../../../core/engine-client.service';
import { EngineTargetStore } from '../../../core/engine-target.store';
import { Message } from '../../../core/engine.dtos';
import { CachedTranscript, OfflineCache, OfflineQueue, QueuedCommand } from '../../../core/remote/offline-cache';
import { RemoteStore } from '../../../core/remote/remote.store';
import { MobileSessionContext } from '../../../core/remote/session-context';
import { I18nService } from '../../../i18n/i18n.service';
import { PermissionPopup } from '../../../ui/permission-popup/permission-popup';
import { ChatView } from '../../chat/chat';
import { ChatSessionStore } from '../../chat/chat-session.store';
import { FormsModule } from '@angular/forms';

const CACHE_DEBOUNCE_MS = 800;

/** Plain-text preview of a cached message (text parts only). */
export function cachedText(message: Message): string {
  return message.parts
    .filter((p): p is Extract<Message['parts'][number], { type: 'text' }> => p.type === 'text')
    .map((p) => p.text)
    .join('\n')
    .trim();
}

@Component({
  selector: 'app-remote-session-view',
  changeDetection: ChangeDetectionStrategy.OnPush,
  imports: [ChatView, PermissionPopup, FormsModule],
  template: `
    <div class="m-remote" data-testid="remote-session-view" [attr.data-link]="remote.link()">
      @if (remote.offline()) {
        <div class="offline" role="status" data-testid="remote-offline-banner">
          <span class="offline-text">{{ t('mobile.remote.offlineBanner') }}</span>
          @if (remote.noRoute()) {
            <button type="button" class="link" (click)="remote.openTailscale()" data-testid="remote-open-tailscale">
              {{ t('mobile.remote.openTailscale') }}
            </button>
          }
        </div>
      }

      @if (queued().length > 0) {
        <ul class="queue" data-testid="remote-queue">
          @for (cmd of queued(); track cmd.id) {
            <li class="queue-item" [attr.data-state]="cmd.state">
              <span class="queue-kind">{{ kindLabel(cmd) }}</span>
              <span class="queue-text">{{ commandText(cmd) }}</span>
              <span class="queue-state">{{ stateLabel(cmd) }}</span>
              @if (cmd.state !== 'pending' && cmd.state !== 'sending') {
                <button
                  type="button"
                  class="queue-dismiss"
                  (click)="queue.dismiss(cmd.id)"
                  [attr.aria-label]="t('mobile.remote.dismiss')"
                >×</button>
              }
            </li>
          }
        </ul>
      }

      <div class="chat-host" [class.has-cached]="showCached()">
        @if (showCached()) {
          <div class="cached" data-testid="remote-cached-transcript">
            <p class="cached-head">
              {{ t('mobile.remote.cachedTranscript', { n: cached()!.messages.length }) }}
            </p>
            @for (m of cached()!.messages; track m.id) {
              @if (cachedText(m); as text) {
                <div class="cached-msg" [class.user]="m.role === 'user'">{{ text }}</div>
              }
            }
          </div>
        }
        <app-chat />
        @if (remote.offline()) {
          <form class="offline-composer" (ngSubmit)="sendQueued()" data-testid="remote-offline-composer">
            <textarea
              rows="2"
              [ngModel]="draft()"
              (ngModelChange)="draft.set($event)"
              name="draft"
              [placeholder]="t('mobile.remote.offlineComposer')"
              data-testid="remote-offline-draft"
            ></textarea>
            <button type="submit" class="queue-send" [disabled]="!draft().trim()" data-testid="remote-offline-send">
              {{ t('mobile.remote.queueSend') }}
            </button>
          </form>
        }
      </div>

      @if (chat.running()) {
        <div class="abort-bar" data-testid="remote-abort-bar">
          <span class="abort-hint">{{ t('chat.typing') }}</span>
          <button
            type="button"
            class="abort"
            (click)="abort()"
            [disabled]="aborting()"
            data-testid="remote-abort"
          >
            {{ aborting() ? t('mobile.remote.aborting') : t('chat.abort') }}
          </button>
        </div>
      }

      <app-permission-popup
        mode="sheet"
        [activeSessionID]="sessionID()"
        [directory]="chat.directory() ?? undefined"
      />
    </div>
  `,
  styles: [
    `
      :host {
        display: flex;
        flex-direction: column;
        flex: 1 1 auto;
        min-height: 0;
      }

      .m-remote {
        display: flex;
        flex-direction: column;
        flex: 1 1 auto;
        min-height: 0;
        position: relative;
      }

      .chat-host {
        display: flex;
        flex-direction: column;
        flex: 1 1 auto;
        min-height: 0;
      }

      .chat-host app-chat {
        display: flex;
        flex-direction: column;
        flex: 1 1 auto;
        min-height: 0;
      }

      /* The sheet is the phone's permission prompt: hide the inline copy. */
      .chat-host ::ng-deep app-chat app-permission-popup {
        display: none;
      }

      /* Desktop-only toolbar chrome - compaction, the effective-model badge. */
      .chat-host ::ng-deep app-chat .tb-compact,
      .chat-host ::ng-deep app-chat .tb-model-badge,
      .chat-host ::ng-deep app-chat .tb-abort {
        display: none;
      }

      .chat-host ::ng-deep app-chat .session-toolbar {
        flex-wrap: wrap;
        row-gap: var(--space-4);
        min-height: 0;
        padding: var(--space-6) var(--space-10);
      }

      .chat-host ::ng-deep app-chat .composer {
        position: sticky;
        bottom: 0;
        padding-bottom: calc(var(--space-8) + env(safe-area-inset-bottom, 0px));
      }

      /* Offline: the live composer would only fail - queue instead (F10-27). */
      .m-remote[data-link='offline'] .chat-host ::ng-deep app-chat .composer {
        display: none;
      }

      .offline-composer {
        flex: none;
        display: flex;
        gap: var(--space-8);
        align-items: flex-end;
        padding: var(--space-8) var(--space-12) calc(var(--space-8) + env(safe-area-inset-bottom, 0px));
        border-top: 1px solid var(--border);
        background: var(--bg);
      }

      .offline-composer textarea {
        flex: 1 1 auto;
        min-height: 44px;
        padding: var(--space-8) var(--space-10);
        border: 1px solid var(--border-strong);
        border-radius: var(--radius-control);
        background: var(--surface);
        color: var(--text);
        font: inherit;
        resize: none;
      }

      .queue-send {
        flex: none;
        min-height: 44px;
        padding: 0 var(--space-14);
        border: 0;
        border-radius: var(--radius-control);
        background: var(--warning);
        color: var(--bg);
        font: inherit;
        font-weight: 600;
        cursor: pointer;
      }

      .queue-send:disabled {
        opacity: 0.5;
      }

      .offline {
        flex: none;
        display: flex;
        align-items: center;
        gap: var(--space-8);
        padding: var(--space-6) var(--space-16);
        background: var(--surface);
        border-bottom: 1px solid var(--border);
        color: var(--warning);
        font-size: var(--fs-12);
      }

      .offline-text {
        flex: 1 1 auto;
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
      }

      .queue {
        flex: none;
        list-style: none;
        margin: 0;
        padding: 0;
        max-height: 30dvh;
        overflow-y: auto;
        border-bottom: 1px solid var(--border);
        background: var(--surface);
      }

      .queue-item {
        display: flex;
        align-items: center;
        gap: var(--space-8);
        padding: var(--space-6) var(--space-16);
        font-size: var(--fs-12);
        border-bottom: 1px solid var(--border);
      }

      .queue-kind {
        flex: none;
        text-transform: uppercase;
        letter-spacing: var(--label-tracking);
        font-size: var(--fs-11);
        color: var(--text-faint);
      }

      .queue-text {
        flex: 1 1 auto;
        min-width: 0;
        overflow: hidden;
        text-overflow: ellipsis;
        white-space: nowrap;
        color: var(--text);
      }

      .queue-state {
        flex: none;
        color: var(--text-muted);
      }

      .queue-item[data-state='expired'] .queue-state,
      .queue-item[data-state='failed'] .queue-state {
        color: var(--danger);
      }

      .queue-item[data-state='sent'] .queue-state {
        color: var(--success);
      }

      .queue-dismiss {
        flex: none;
        border: 0;
        background: transparent;
        color: var(--text-muted);
        font-size: var(--fs-14);
        padding: 0 var(--space-4);
        cursor: pointer;
      }

      .cached {
        flex: none;
        max-height: 55dvh;
        overflow-y: auto;
        padding: var(--space-8) var(--space-12);
        border-bottom: 1px solid var(--border);
        display: flex;
        flex-direction: column;
        gap: var(--space-6);
      }

      .cached-head {
        margin: 0;
        font-size: var(--fs-11);
        text-transform: uppercase;
        letter-spacing: var(--label-tracking);
        color: var(--text-faint);
      }

      .cached-msg {
        padding: var(--space-6) var(--space-10);
        border-radius: var(--radius-bubble);
        background: var(--surface);
        font-size: var(--fs-12-5);
        white-space: pre-wrap;
        overflow-wrap: anywhere;
        max-height: 160px;
        overflow: hidden;
      }

      .cached-msg.user {
        align-self: flex-end;
        background: var(--surface-2);
        max-width: 85%;
      }

      .abort-bar {
        position: absolute;
        left: var(--space-12);
        right: var(--space-12);
        bottom: calc(96px + env(safe-area-inset-bottom, 0px));
        display: flex;
        align-items: center;
        justify-content: space-between;
        gap: var(--space-8);
        padding: var(--space-6) var(--space-12);
        border: 1px solid var(--border-strong);
        border-radius: 999px;
        background: var(--surface);
        box-shadow: 0 6px 20px rgba(0, 0, 0, 0.35);
        z-index: 5;
        font-size: var(--fs-12);
        color: var(--text-muted);
      }

      .abort {
        min-height: 36px;
        padding: 0 var(--space-14);
        border: 1px solid var(--danger);
        border-radius: 999px;
        background: transparent;
        color: var(--danger);
        font: inherit;
        font-weight: 600;
        cursor: pointer;
      }

      .abort:disabled {
        opacity: 0.6;
      }
    `,
  ],
})
export class RemoteSessionView {
  readonly remote = inject(RemoteStore);
  readonly chat = inject(ChatSessionStore);
  readonly queue = inject(OfflineQueue);
  private readonly engine = inject(EngineClient);
  private readonly targets = inject(EngineTargetStore);
  private readonly cache = inject(OfflineCache);
  private readonly context = inject(MobileSessionContext);
  private readonly i18n = inject(I18nService);
  private readonly destroyRef = inject(DestroyRef);

  readonly t = this.i18n.t.bind(this.i18n);
  readonly cachedText = cachedText;

  readonly sessionID = input.required<string>();

  readonly aborting = signal(false);
  readonly cached = signal<CachedTranscript | null>(null);
  /** Offline composer draft (queued, never sent directly). */
  readonly draft = signal('');

  /** Commands of this session (pending, then finished ones the user can dismiss). */
  readonly queued = computed<QueuedCommand[]>(() =>
    this.queue.commands().filter((c) => c.sessionID === this.sessionID()),
  );

  /** Offline with nothing live on screen: show the cached tail. */
  readonly showCached = computed(
    () => this.remote.offline() && this.chat.messages().length === 0 && !!this.cached(),
  );

  private cacheTimer: ReturnType<typeof setTimeout> | null = null;

  constructor() {
    // Remember the session for the Changes/Agents tabs and load its cache.
    effect(() => {
      const id = this.sessionID();
      untracked(() => {
        this.context.remember(id);
        this.cached.set(null);
        void this.loadCached(id);
      });
    });
    // Persist the transcript tail as it changes (debounced).
    effect(() => {
      const messages = this.chat.messages();
      const id = this.sessionID();
      untracked(() => this.scheduleCache(id, messages));
    });
    this.destroyRef.onDestroy(() => {
      if (this.cacheTimer) {
        clearTimeout(this.cacheTimer);
      }
    });
  }

  async abort(): Promise<void> {
    const id = this.sessionID();
    if (this.aborting()) {
      return;
    }
    if (this.remote.offline()) {
      this.queue.enqueue('abort', id, {});
      return;
    }
    this.aborting.set(true);
    try {
      await this.engine.abort(id);
    } catch {
      /* the running flag follows the engine's events */
    } finally {
      this.aborting.set(false);
    }
  }

  /** Offline: queue the prompt for the next live edge (F10-27). */
  sendQueued(): void {
    const message = this.draft().trim();
    if (!message) {
      return;
    }
    this.queue.enqueue('prompt', this.sessionID(), { message });
    this.draft.set('');
  }

  kindLabel(cmd: QueuedCommand): string {
    switch (cmd.kind) {
      case 'prompt':
        return this.t('mobile.remote.queuePrompt');
      case 'permission':
        return this.t('mobile.remote.queuePermission');
      default:
        return this.t('mobile.remote.queueAbort');
    }
  }

  commandText(cmd: QueuedCommand): string {
    if (cmd.kind === 'prompt') {
      return String(cmd.payload['message'] ?? '');
    }
    if (cmd.kind === 'permission') {
      return `${String(cmd.payload['decision'] ?? '')} ${String(cmd.payload['requestID'] ?? '')}`;
    }
    return '';
  }

  stateLabel(cmd: QueuedCommand): string {
    switch (cmd.state) {
      case 'pending':
        return this.t('mobile.remote.queuePending');
      case 'sending':
        return this.t('mobile.remote.queueSending');
      case 'sent':
        return this.t('mobile.remote.queueSent');
      case 'expired':
        return this.t('mobile.remote.queueExpired');
      default:
        return this.t('mobile.remote.queueFailed');
    }
  }

  private async loadCached(id: string): Promise<void> {
    const targetId = this.targets.activeId();
    if (!targetId) {
      return;
    }
    const cached = await this.cache.getTranscript(targetId, id);
    if (cached && this.sessionID() === id) {
      this.cached.set(cached);
    }
  }

  private scheduleCache(id: string, messages: Message[]): void {
    if (messages.length === 0) {
      return;
    }
    if (this.cacheTimer) {
      clearTimeout(this.cacheTimer);
    }
    this.cacheTimer = setTimeout(() => {
      this.cacheTimer = null;
      const targetId = this.targets.activeId();
      if (targetId && this.sessionID() === id) {
        void this.cache.putTranscript(targetId, id, messages);
      }
    }, CACHE_DEBOUNCE_MS);
  }
}
