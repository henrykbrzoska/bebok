/**
 * Pairing flow (WP-M6 / F10-22, F10-23): scan/deep link -> probe -> confirm
 * -> long-poll -> registered `desktop` target -> `switchTarget`.
 *
 * State machine (`PairingFlow.state`):
 *
 *   idle -> probing -> confirm -> waiting -> paired
 *                 \-> error (probe failed)   \-> error (engine refused / timed out)
 *
 * `error.code` tells the UI what happened: `pair_timeout` and `unreachable`
 * are retryable with the *same* code (408: the desktop just did not click in
 * time), `pair_expired` / `pair_invalid_code` / `pair_locked` /
 * `pair_rejected` send the user back to scanning with the reason. On 200 the
 * flow does the whole WP-M2 integration - `EngineTargetStore.upsert({ kind:
 * 'desktop', … })` then `EngineClient.switchTarget(id)` - exactly once, and
 * only on success.
 */

import { Injectable, computed, inject, signal } from '@angular/core';

import { ENGINE_API } from '../engine-api';
import { EngineTargetStore } from '../engine-target.store';
import { ProbeError, ProbeStatus, isAllowedRemoteEndpoint, probeEndpoints } from './endpoint-probe';
import { PairError, PairErrorCode, PairInvite, normalizePairCode } from './pair-protocol';

export type PairingPhase = 'idle' | 'probing' | 'confirm' | 'waiting' | 'paired' | 'error';

/** Everything the UI may need to say about a failure. */
export interface PairingFailure {
  /** `probe_*` for reachability, else the engine's pairing code. */
  code: PairErrorCode | 'probe_no_route' | 'probe_bad_endpoint' | 'endpoint_not_private';
  detail: string;
  /** The same code can be presented again on the same endpoint. */
  retryable: boolean;
}

export interface PairingCandidate {
  /** Winning endpoint (clean base URL). */
  endpoint: string;
  engineName: string;
  fingerprint: string | null;
  code: string;
  /** Endpoints from the invite (for the retry/back path). */
  endpoints: string[];
  probeMs: number;
}

/** Target id for a paired desktop (the engine's device id is unique per engine). */
export function desktopTargetId(deviceId: string): string {
  return `desktop:${deviceId}`;
}

@Injectable({ providedIn: 'root' })
export class PairingFlow {
  private readonly engine = inject(ENGINE_API);
  private readonly targets = inject(EngineTargetStore);

  readonly phase = signal<PairingPhase>('idle');
  readonly candidate = signal<PairingCandidate | null>(null);
  readonly failure = signal<PairingFailure | null>(null);
  /** Result of the last successful pairing (target id). */
  readonly pairedTargetId = signal<string | null>(null);
  /** When the long-poll started (elapsed-time display). */
  readonly waitingSince = signal<number | null>(null);

  readonly busy = computed(() => this.phase() === 'probing' || this.phase() === 'waiting');

  /** Overrides for specs (fetch used by the probe, device metadata). */
  probeFetch: typeof fetch | undefined;
  probeTimeoutMs: number | undefined;
  deviceModel = '';
  devicePlatform = 'android';

  private abort: AbortController | null = null;

  reset(): void {
    this.abort?.abort();
    this.abort = null;
    this.phase.set('idle');
    this.candidate.set(null);
    this.failure.set(null);
    this.waitingSince.set(null);
  }

  /**
   * Step 1: find the reachable endpoint of an invite. Resolves into `confirm`
   * (candidate set) or `error`.
   */
  async probe(invite: Pick<PairInvite, 'endpoints' | 'code' | 'fingerprint'>): Promise<boolean> {
    this.reset();
    const code = normalizePairCode(invite.code);
    const allowed = invite.endpoints.filter(isAllowedRemoteEndpoint);
    if (allowed.length === 0) {
      this.fail({
        code: 'endpoint_not_private',
        detail: invite.endpoints.join(', '),
        retryable: false,
      });
      return false;
    }
    this.phase.set('probing');
    const controller = new AbortController();
    this.abort = controller;
    try {
      const outcome = await probeEndpoints(allowed, {
        fetch: this.probeFetch,
        timeoutMs: this.probeTimeoutMs,
        expectFingerprint: invite.fingerprint,
        signal: controller.signal,
      });
      if (controller.signal.aborted) {
        return false;
      }
      const status: ProbeStatus = outcome.status;
      this.candidate.set({
        endpoint: outcome.endpoint,
        engineName: status.engineName || outcome.endpoint,
        fingerprint: status.fingerprint ?? invite.fingerprint ?? null,
        code,
        endpoints: allowed,
        probeMs: outcome.elapsedMs,
      });
      this.phase.set('confirm');
      return true;
    } catch (err) {
      if (controller.signal.aborted) {
        return false;
      }
      const probe = err instanceof ProbeError ? err : null;
      this.fail({
        code: probe?.kind === 'bad_endpoint' ? 'probe_bad_endpoint' : 'probe_no_route',
        detail: probe?.message ?? (err instanceof Error ? err.message : String(err)),
        retryable: true,
      });
      return false;
    } finally {
      if (this.abort === controller) {
        this.abort = null;
      }
    }
  }

  /**
   * Step 2: present the code and wait for the desktop (long-poll). On 200:
   * upsert the `desktop` target, switch to it, `paired`.
   */
  async pair(deviceName: string): Promise<boolean> {
    const candidate = this.candidate();
    if (!candidate || this.phase() === 'waiting') {
      return false;
    }
    this.failure.set(null);
    this.phase.set('waiting');
    this.waitingSince.set(Date.now());
    const controller = new AbortController();
    this.abort = controller;
    try {
      const result = await this.engine.pairWithDesktop(
        candidate.endpoint,
        candidate.code,
        deviceName,
        {
          model: this.deviceModel || undefined,
          platform: this.devicePlatform || undefined,
          signal: controller.signal,
        },
      );
      if (controller.signal.aborted) {
        return false;
      }
      const id = desktopTargetId(result.deviceId);
      this.targets.upsert({
        id,
        kind: 'desktop',
        label: result.engineName || candidate.engineName,
        baseUrl: candidate.endpoint,
        endpoints: [candidate.endpoint, ...candidate.endpoints],
        token: result.token,
      });
      await this.engine.switchTarget(id);
      this.pairedTargetId.set(id);
      this.phase.set('paired');
      return true;
    } catch (err) {
      if (controller.signal.aborted) {
        return false;
      }
      const pairErr = err instanceof PairError ? err : null;
      this.fail({
        code: pairErr?.code ?? 'unknown',
        detail: err instanceof Error ? err.message : String(err),
        retryable: pairErr?.retryable ?? false,
      });
      return false;
    } finally {
      this.waitingSince.set(null);
      if (this.abort === controller) {
        this.abort = null;
      }
    }
  }

  /** Cancel the long-poll (the engine abandons the request on its side later). */
  cancel(): void {
    if (this.phase() === 'waiting' || this.phase() === 'probing') {
      this.abort?.abort();
      this.abort = null;
      this.phase.set(this.candidate() ? 'confirm' : 'idle');
      this.waitingSince.set(null);
    }
  }

  /** Back to `confirm` after a retryable failure (same code, same endpoint). */
  retry(): void {
    if (this.candidate() && this.failure()?.retryable) {
      this.failure.set(null);
      this.phase.set('confirm');
    }
  }

  private fail(failure: PairingFailure): void {
    this.failure.set(failure);
    this.phase.set('error');
  }
}
