/**
 * Chat tab (WP-M2 / F10-8 shell; WP-M5 body).
 *
 * `/m/chat/:sessionID` embeds the existing `ChatView` (it reads the session
 * id from the same `ActivatedRoute`, so no wrapper plumbing is needed).
 * `/m/chat` shows either the onboarding (F10-16: first launch, i.e. no
 * `bebok.mobile.onboarded` flag, or whenever the embedded engine reported a
 * launch failure through `EngineTargetStore.platformError`) or the chat home
 * (F10-18: recent sessions, agent picker, quick session).
 */

import { ChangeDetectionStrategy, Component, computed, inject, signal } from '@angular/core';
import { toSignal } from '@angular/core/rxjs-interop';
import { ActivatedRoute } from '@angular/router';
import { map } from 'rxjs';

import { EngineTargetStore } from '../../../core/engine-target.store';
import { ChatView } from '../../../views/chat/chat';
import { ChatHomeView } from '../../../views/mobile/chat-home/chat-home';
import {
  OnboardingView,
  readOnboarded,
  writeOnboarded,
} from '../../../views/mobile/onboarding/onboarding';

@Component({
  selector: 'app-chat-tab',
  changeDetection: ChangeDetectionStrategy.OnPush,
  imports: [ChatView, ChatHomeView, OnboardingView],
  template: `
    @if (sessionID()) {
      <div class="m-chat-host">
        <app-chat />
      </div>
    } @else if (showOnboarding()) {
      <app-mobile-onboarding (done)="finishOnboarding()" />
    } @else {
      <app-chat-home (setup)="reopenOnboarding()" />
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
    `,
  ],
})
export class ChatTab {
  private readonly route = inject(ActivatedRoute);
  private readonly targets = inject(EngineTargetStore);

  readonly sessionID = toSignal(this.route.paramMap.pipe(map((p) => p.get('sessionID') ?? '')), {
    initialValue: this.route.snapshot.paramMap.get('sessionID') ?? '',
  });

  /** Persisted "a choice was made" flag (mirrors `bebok.mobile.onboarded`). */
  readonly onboarded = signal(readOnboarded());

  /** First launch, or the embedded engine failed: onboarding wins. */
  readonly showOnboarding = computed(
    () => !this.onboarded() || this.targets.platformError() !== null,
  );

  finishOnboarding(): void {
    writeOnboarded(true);
    this.onboarded.set(true);
  }

  reopenOnboarding(): void {
    writeOnboarded(false);
    this.onboarded.set(false);
  }
}
