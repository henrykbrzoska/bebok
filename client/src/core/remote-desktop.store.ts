/**
 * Remote desktop store (WP-M4 / F10-15).
 *
 * The single source the Settings -> Remote panel (F10-13) and the topbar
 * pill (F10-14) render from: one fetch of `GET /remote/status` +
 * `GET /remote/devices` on `ensure()`, kept live by the three bus events
 * WP-M1 publishes (`remote.status`, `remote.device.changed`,
 * `remote.pair.request`) - no polling, no duplicate fetch loops between the
 * panel and the pill.
 */

import { Injectable, computed, inject, signal } from '@angular/core';

import { ENGINE_API } from './engine-api';
import { EventsStore } from './events.store';
import {
  RemoteDevice,
  RemoteDeviceChangedEvent,
  RemotePairRequestEvent,
  RemoteStatus,
} from './engine.dtos';

@Injectable({ providedIn: 'root' })
export class RemoteDesktopStore {
  private readonly engine = inject(ENGINE_API);
  private readonly events = inject(EventsStore);

  readonly status = signal<RemoteStatus | null>(null);
  readonly devices = signal<RemoteDevice[]>([]);
  readonly loading = signal(false);
  readonly error = signal<string | null>(null);

  /** Set by `remote.pair.request`; cleared on confirm/reject or expiry. */
  readonly pairRequest = signal<RemotePairRequestEvent | null>(null);

  readonly enabled = computed(() => this.status()?.enabled ?? false);
  readonly devicesOnline = computed(() => this.status()?.devicesOnline ?? 0);

  private ensured = false;
  private unsubscribe: (() => void) | null = null;

  /** Idempotent: fetch once, then stay live via bus events (F10-15). */
  async ensure(): Promise<void> {
    if (this.ensured) {
      return;
    }
    this.ensured = true;
    this.events.start();
    this.subscribe();
    await this.refresh();
  }

  /** Full refresh - used by `ensure()` and after actions the events might race with. */
  async refresh(): Promise<void> {
    this.loading.set(true);
    try {
      const [status, devices] = await Promise.all([
        this.engine.getRemoteStatus(),
        this.engine.listDevices(),
      ]);
      this.status.set(status);
      this.devices.set(devices);
      this.error.set(null);
    } catch (err) {
      this.error.set(err instanceof Error ? err.message : String(err));
    } finally {
      this.loading.set(false);
    }
  }

  private subscribe(): void {
    if (this.unsubscribe) {
      return;
    }
    this.unsubscribe = this.events.onEvent((event) => {
      switch (event.type) {
        case 'remote.status':
          this.status.set(event.properties as unknown as RemoteStatus);
          break;
        case 'remote.device.changed':
          this.onDeviceChanged(event.properties as unknown as RemoteDeviceChangedEvent);
          break;
        case 'remote.pair.request':
          this.pairRequest.set(event.properties as unknown as RemotePairRequestEvent);
          break;
        default:
          break;
      }
    });
  }

  private onDeviceChanged(change: RemoteDeviceChangedEvent): void {
    if (change.change === 'removed') {
      this.devices.update((list) => list.filter((d) => d.id !== change.deviceId));
      return;
    }
    // "created" / "seen" - one light refetch of the device list reconciles
    // name/model/lastSeen without a second poller (status still comes from
    // the `remote.status` event alone).
    void this.engine
      .listDevices()
      .then((devices) => this.devices.set(devices))
      .catch(() => {
        /* keep the stale list; the next event may resolve it */
      });
  }

  /** Cleared by the panel once the modal is dismissed (confirm/reject/expiry). */
  clearPairRequest(): void {
    this.pairRequest.set(null);
  }
}
