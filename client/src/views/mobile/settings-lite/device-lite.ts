/**
 * "This device" section of Settings-lite (WP-M5 / F10-19): the active
 * engine target, the quick-session directory, a way back to onboarding, and
 * a developer switch for WP-M1's deterministic mock provider
 * (`BEBOK_PROVIDER_MOCK=1`, F10-21) that the native launcher only honours in
 * debuggable builds. Toggling it restarts the embedded engine so the flag
 * takes effect immediately.
 */

import { ChangeDetectionStrategy, Component, computed, inject, signal } from '@angular/core';
import { Router } from '@angular/router';

import { EngineClient } from '../../../core/engine-client.service';
import {
  EngineLauncher,
  EngineWorkTracker,
  readMockProviderFlag,
  writeMockProviderFlag,
} from '../../../core/engine-launcher';
import { EngineTargetStore } from '../../../core/engine-target.store';
import { EventsStore } from '../../../core/events.store';
import { I18nService } from '../../../i18n/i18n.service';
import type { MessageKey } from '../../../i18n';
import { readCachedQuickDir } from '../../mobile/chat-home/quick-directory';
import { writeOnboarded } from '../../mobile/onboarding/onboarding';
import { LITE_STYLES } from './lite-shared';

@Component({
  selector: 'app-settings-lite-device',
  changeDetection: ChangeDetectionStrategy.OnPush,
  template: `
    <div class="card" data-testid="device-lite-engine">
      <h3>{{ t('mobile.engine.title') }}</h3>
      @if (targets.active(); as target) {
        <p class="hint">
          {{ target.label }} · {{ kindLabel(target.kind) }}
          <br />
          <span class="mono">{{ target.baseUrl }}</span>
        </p>
      } @else {
        <p class="hint">{{ t('mobile.engine.none') }}</p>
      }
      <p class="hint" data-testid="device-lite-status">{{ statusLabel() }}</p>
      @if (quickDir(); as dir) {
        <p class="hint">
          {{ t('mobile.settings.quickDir') }}
          <br />
          <span class="mono">{{ dir }}</span>
        </p>
      }
      <button type="button" class="btn" (click)="reopenOnboarding()" data-testid="device-lite-onboarding">
        {{ t('mobile.settings.reopenOnboarding') }}
      </button>
    </div>

    @if (isCapacitor) {
      <div class="card" data-testid="device-lite-developer">
        <h3>{{ t('mobile.settings.developer') }}</h3>
        <label class="toggle">
          <input
            type="checkbox"
            [checked]="mock()"
            [disabled]="restarting()"
            (change)="setMock($any($event.target).checked)"
            data-testid="device-lite-mock"
          />
          <span>{{ t('mobile.settings.mockProvider') }}</span>
        </label>
        <p class="hint">{{ t('mobile.settings.mockProviderHint') }}</p>
        @if (restarting()) {
          <p class="hint">{{ t('mobile.onboarding.starting') }}</p>
        }
        @if (error(); as err) {
          <p class="error">{{ err }}</p>
        }
      </div>
    }
  `,
  styles: [LITE_STYLES],
})
export class DeviceLite {
  private readonly engine = inject(EngineClient);
  private readonly events = inject(EventsStore);
  private readonly work = inject(EngineWorkTracker);
  private readonly router = inject(Router);
  private readonly i18n = inject(I18nService);

  readonly targets = inject(EngineTargetStore);
  readonly t = this.i18n.t.bind(this.i18n);
  readonly isCapacitor = this.engine.isCapacitor;

  readonly mock = signal(readMockProviderFlag());
  readonly restarting = signal(false);
  readonly error = signal<string | null>(null);
  readonly statusLabel = computed(() => this.t(`status.${this.events.state()}` as MessageKey));
  readonly quickDir = computed(() => readCachedQuickDir(this.targets.activeId() ?? 'default'));

  kindLabel(kind: string): string {
    switch (kind) {
      case 'embedded':
        return this.t('mobile.engine.kindEmbedded');
      case 'desktop':
        return this.t('mobile.engine.kindDesktop');
      case 'sidecar':
        return this.t('mobile.engine.kindSidecar');
      default:
        return this.t('mobile.engine.kindRemoteUrl');
    }
  }

  reopenOnboarding(): void {
    writeOnboarded(false);
    void this.router.navigate(['/m/chat']);
  }

  /** Persist the flag and restart the embedded engine with the new env. */
  async setMock(on: boolean): Promise<void> {
    writeMockProviderFlag(on);
    this.mock.set(on);
    if (!this.isCapacitor || this.targets.active()?.kind !== 'embedded') {
      return;
    }
    this.restarting.set(true);
    this.error.set(null);
    try {
      this.work.endAll();
      await EngineLauncher.stop();
      await this.engine.reconnect();
      this.events.restart();
    } catch (err) {
      this.error.set(err instanceof Error ? err.message : String(err));
    } finally {
      this.restarting.set(false);
    }
  }
}
