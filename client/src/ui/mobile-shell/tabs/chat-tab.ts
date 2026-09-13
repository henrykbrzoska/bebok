/**
 * Chat tab (WP-M2 / F10-8 placeholder; WP-M5 fills it in).
 *
 * `/m/chat/:sessionID` embeds the existing `ChatView` (it reads the session
 * id from the same `ActivatedRoute`, so no wrapper plumbing is needed);
 * `/m/chat` offers a "New chat" button that creates a session against the
 * active engine target and navigates to it. The project directory is the
 * last one used, else the first registered project, else a typed path.
 */

import { ChangeDetectionStrategy, Component, computed, inject, signal } from '@angular/core';
import { toSignal } from '@angular/core/rxjs-interop';
import { FormsModule } from '@angular/forms';
import { ActivatedRoute, Router } from '@angular/router';
import { map } from 'rxjs';

import { EngineClient } from '../../../core/engine-client.service';
import { ProjectsStore } from '../../../core/projects.store';
import { I18nService } from '../../../i18n/i18n.service';
import { ChatView } from '../../../views/chat/chat';

@Component({
  selector: 'app-chat-tab',
  changeDetection: ChangeDetectionStrategy.OnPush,
  imports: [ChatView, FormsModule],
  template: `
    @if (sessionID()) {
      <div class="m-chat-host">
        <app-chat />
      </div>
    } @else {
      <section class="m-empty" data-testid="chat-tab-empty">
        <h2>{{ t('mobile.chat.emptyTitle') }}</h2>
        <p>{{ t('mobile.chat.emptyHint') }}</p>
        @if (!knownDirectory()) {
          <label class="m-field">
            <span>{{ t('start.projectDir') }}</span>
            <input
              type="text"
              class="mono-input"
              [ngModel]="typedDirectory()"
              (ngModelChange)="typedDirectory.set($event)"
              [placeholder]="t('start.dirPlaceholder')"
              spellcheck="false"
              data-testid="chat-tab-directory"
            />
          </label>
        } @else {
          <p class="m-dir" data-testid="chat-tab-known-directory">{{ knownDirectory() }}</p>
        }
        <button
          type="button"
          class="btn-primary"
          [disabled]="creating() || !directory()"
          (click)="newChat()"
          data-testid="chat-tab-new"
        >
          {{ creating() ? t('start.creating') : t('nav.newChat') }}
        </button>
        @if (error(); as err) {
          <p class="m-error" role="alert">{{ err }}</p>
        }
      </section>
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

      .m-chat-host {
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

      .m-field {
        display: flex;
        flex-direction: column;
        gap: var(--space-6);
        text-align: left;
        font-size: var(--fs-12);
        color: var(--text-muted);
      }

      .m-dir {
        font-family: var(--font-mono);
        font-size: var(--fs-12);
        overflow-wrap: anywhere;
      }

      .m-error {
        color: var(--danger);
        font-size: var(--fs-12-5);
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
    `,
  ],
})
export class ChatTab {
  private readonly route = inject(ActivatedRoute);
  private readonly router = inject(Router);
  private readonly engine = inject(EngineClient);
  private readonly projects = inject(ProjectsStore);
  private readonly i18n = inject(I18nService);

  readonly t = this.i18n.t.bind(this.i18n);

  readonly sessionID = toSignal(this.route.paramMap.pipe(map((p) => p.get('sessionID') ?? '')), {
    initialValue: this.route.snapshot.paramMap.get('sessionID') ?? '',
  });

  readonly creating = signal(false);
  readonly error = signal<string | null>(null);
  readonly typedDirectory = signal('');

  /** Last used directory, else the first registered project. */
  readonly knownDirectory = computed(
    () => this.engine.readLastDirectory() || this.projects.projects()[0]?.path || '',
  );
  readonly directory = computed(() => this.knownDirectory() || this.typedDirectory().trim());

  async newChat(): Promise<void> {
    const directory = this.directory();
    if (!directory || this.creating()) {
      return;
    }
    this.creating.set(true);
    this.error.set(null);
    try {
      const created = await this.engine.createSession(directory);
      this.engine.saveDirectory(directory);
      await this.router.navigate(['/m/chat', created.sessionID]);
    } catch (err) {
      this.error.set(err instanceof Error ? err.message : String(err));
    } finally {
      this.creating.set(false);
    }
  }
}
