/**
 * Settings -> Remote panel (WP-M4 / F10-13).
 *
 * Turns WP-M1's second listener on/off and manages paired devices. Every
 * action here is a plain REST call against this same engine's *local*
 * listener (`EngineClient`/`ENGINE_API`) - no new transport, no new crypto.
 * State comes from `RemoteDesktopStore` (F10-15), which the topbar pill
 * (F10-14) also reads, so there is exactly one fetch + one set of bus-event
 * listeners behind both surfaces.
 */

import { Component, OnDestroy, OnInit, computed, inject, signal } from '@angular/core';
import { DomSanitizer, SafeHtml } from '@angular/platform-browser';

import { ENGINE_API } from '../../../core/engine-api';
import { RemoteApiError, RemoteDevice, RemotePairStart } from '../../../core/engine.dtos';
import { RemoteDesktopStore } from '../../../core/remote-desktop.store';
import { I18nService } from '../../../i18n/i18n.service';
import { ToastStore } from '../../../ui/toast/toast.store';
import { encodeQr, qrToSvg } from './qrcode';

@Component({
  selector: 'app-settings-remote',
  imports: [],
  templateUrl: './remote-tab.html',
  styleUrls: ['../settings-shared.css', './remote-tab.css'],
})
export class RemoteTab implements OnInit, OnDestroy {
  private readonly engine = inject(ENGINE_API);
  private readonly i18n = inject(I18nService);
  private readonly toast = inject(ToastStore);
  private readonly sanitizer = inject(DomSanitizer);

  readonly store = inject(RemoteDesktopStore);
  readonly t = this.i18n.t.bind(this.i18n);

  readonly toggleBusy = signal(false);
  readonly allowLanBusy = signal(false);
  /** Set once the user flips the switch; cleared once it matches the live status again. */
  readonly pendingAllowLan = signal<boolean | null>(null);

  readonly pairStart = signal<RemotePairStart | null>(null);
  readonly pairBusy = signal(false);
  readonly pairError = signal<string | null>(null);
  private readonly now = signal(Date.now());

  readonly pairRequestBusy = signal(false);
  readonly pairRequestError = signal<string | null>(null);

  /** Device id armed for a second confirming click (F10-13: "revoke with a confirm step"). */
  readonly revokeArmedId = signal<string | null>(null);
  readonly revokeBusy = signal<string | null>(null);

  private tickHandle: ReturnType<typeof setInterval> | null = null;

  readonly allowLanEffective = computed(
    () => this.pendingAllowLan() ?? this.store.status()?.allowLan ?? false,
  );
  readonly allowLanPendingRestart = computed(() => {
    const pending = this.pendingAllowLan();
    return pending !== null && pending !== (this.store.status()?.allowLan ?? false);
  });

  /**
   * `[innerHTML]` runs Angular's HTML sanitizer, which strips `<svg>`
   * wholesale (it is not on the default safe-HTML element list) - the QR
   * markup is entirely our own generated output (never user input), so it is
   * marked trusted explicitly rather than losing the picture silently.
   */
  readonly qrSvg = computed<SafeHtml | null>(() => {
    const start = this.pairStart();
    if (!start) {
      return null;
    }
    const matrix = encodeQr(this.pairingUri(start));
    return matrix ? this.sanitizer.bypassSecurityTrustHtml(qrToSvg(matrix, 4)) : null;
  });

  readonly secondsLeft = computed(() => {
    const start = this.pairStart();
    if (!start) {
      return 0;
    }
    return Math.max(0, Math.round((start.expiresAt - this.now()) / 1000));
  });

  readonly pairCodeExpired = computed(() => this.pairStart() !== null && this.secondsLeft() <= 0);

  readonly hasEligibleInterface = computed(() => (this.store.status()?.endpoints.length ?? 0) > 0);

  ngOnInit(): void {
    void this.store.ensure();
    this.tickHandle = setInterval(() => this.now.set(Date.now()), 1000);
  }

  ngOnDestroy(): void {
    if (this.tickHandle !== null) {
      clearInterval(this.tickHandle);
      this.tickHandle = null;
    }
  }

  async toggleEnabled(): Promise<void> {
    if (this.toggleBusy()) {
      return;
    }
    const wasEnabled = this.store.enabled();
    this.toggleBusy.set(true);
    try {
      const status = wasEnabled ? await this.engine.disableRemote() : await this.engine.enableRemote();
      this.store.status.set(status);
      this.pendingAllowLan.set(null);
      this.toast.show(this.t(wasEnabled ? 'remote.toastDisabled' : 'remote.toastEnabled'), {
        kind: 'success',
      });
      if (wasEnabled) {
        this.pairStart.set(null);
      } else {
        await this.store.refresh();
      }
    } catch (err) {
      this.toast.show(this.describeError(err), { kind: 'danger' });
    } finally {
      this.toggleBusy.set(false);
    }
  }

  async setAllowLan(value: boolean): Promise<void> {
    if (this.allowLanBusy()) {
      return;
    }
    this.allowLanBusy.set(true);
    try {
      const directory = this.engine.readLastDirectory() ?? '';
      await this.engine.putConfig(directory, { remote: { allow_lan: value } }, { scope: 'global' });
      this.pendingAllowLan.set(value);
    } catch (err) {
      this.toast.show(this.describeError(err), { kind: 'danger' });
    } finally {
      this.allowLanBusy.set(false);
    }
  }

  /** WP-M1 open issue #4: `allow_lan`/`port` only apply on the next `enable`. */
  async reapplyNow(): Promise<void> {
    if (this.toggleBusy()) {
      return;
    }
    this.toggleBusy.set(true);
    try {
      await this.engine.disableRemote();
      const status = await this.engine.enableRemote();
      this.store.status.set(status);
      this.pendingAllowLan.set(null);
      await this.store.refresh();
    } catch (err) {
      this.toast.show(this.describeError(err), { kind: 'danger' });
    } finally {
      this.toggleBusy.set(false);
    }
  }

  async startPairing(): Promise<void> {
    if (this.pairBusy()) {
      return;
    }
    this.pairBusy.set(true);
    this.pairError.set(null);
    try {
      this.pairStart.set(await this.engine.startPairing());
    } catch (err) {
      this.pairError.set(this.describeError(err));
    } finally {
      this.pairBusy.set(false);
    }
  }

  dismissPairStart(): void {
    this.pairStart.set(null);
    this.pairError.set(null);
  }

  async confirmPairRequest(): Promise<void> {
    const request = this.store.pairRequest();
    if (!request || this.pairRequestBusy()) {
      return;
    }
    this.pairRequestBusy.set(true);
    this.pairRequestError.set(null);
    try {
      const device = await this.engine.confirmPairing(request.pairId);
      this.store.clearPairRequest();
      this.pairStart.set(null);
      await this.store.refresh();
      this.toast.show(this.t('remote.toastDevicePaired', { device: device.name }), { kind: 'success' });
    } catch (err) {
      this.pairRequestError.set(this.describeError(err));
    } finally {
      this.pairRequestBusy.set(false);
    }
  }

  async rejectPairRequest(): Promise<void> {
    const request = this.store.pairRequest();
    if (!request || this.pairRequestBusy()) {
      return;
    }
    this.pairRequestBusy.set(true);
    this.pairRequestError.set(null);
    try {
      await this.engine.rejectPairing(request.pairId);
      this.store.clearPairRequest();
    } catch (err) {
      this.pairRequestError.set(this.describeError(err));
    } finally {
      this.pairRequestBusy.set(false);
    }
  }

  /** First click arms the row, a second click on the same device revokes it. */
  armOrRevoke(device: RemoteDevice): void {
    if (this.revokeArmedId() !== device.id) {
      this.revokeArmedId.set(device.id);
      return;
    }
    void this.revoke(device);
  }

  cancelRevoke(): void {
    this.revokeArmedId.set(null);
  }

  private async revoke(device: RemoteDevice): Promise<void> {
    this.revokeBusy.set(device.id);
    const before = this.store.devices();
    this.store.devices.set(before.filter((d) => d.id !== device.id));
    try {
      await this.engine.revokeDevice(device.id);
    } catch (err) {
      this.store.devices.set(before);
      this.toast.show(this.describeError(err), { kind: 'danger' });
    } finally {
      this.revokeBusy.set(null);
      this.revokeArmedId.set(null);
    }
  }

  lastSeenLabel(device: RemoteDevice): string {
    if (!device.lastSeen) {
      return this.t('remote.deviceNeverSeen');
    }
    return this.t('remote.deviceLastSeen', { time: new Date(device.lastSeen).toLocaleString() });
  }

  deviceSubtitle(device: RemoteDevice): string {
    const parts = [device.model, device.platform].filter((p) => p && p.trim().length > 0);
    return parts.length > 0 ? parts.join(' · ') : this.t('remote.deviceUnknown');
  }

  private pairingUri(start: RemotePairStart): string {
    return `bebok://pair?v=1&ep=${start.endpoints.join(',')}&code=${start.code}&fp=${start.fingerprint}`;
  }

  private describeError(err: unknown): string {
    if (err instanceof RemoteApiError) {
      switch (err.code) {
        case 'remote_disabled':
          return this.t('remote.errorRemoteDisabled');
        case 'pair_not_requested':
          return this.t('remote.errorPairNotRequested');
        case 'pair_rejected':
        case 'pair_invalid_code':
        case 'pair_locked':
        case 'pair_expired':
        case 'pair_timeout':
          return this.t('remote.errorGeneric', { message: err.message });
        default:
          return this.t('remote.errorGeneric', { message: err.message });
      }
    }
    return this.t('remote.errorGeneric', {
      message: err instanceof Error ? err.message : String(err),
    });
  }
}
