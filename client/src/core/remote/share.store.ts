/**
 * Joined share links (1.8): paste a `bebok://share?...` link and the
 * session becomes a `share` engine target - "Shared sessions" in the
 * sidebar / the phone's engine sheet. Opening one switches the active engine
 * to that desktop (through whichever of its endpoints answers) and navigates
 * to the chat; `leave()` switches back to the platform engine.
 */

import { Injectable, computed, inject, signal } from '@angular/core';
import { Router } from '@angular/router';

import { EngineClient, PLATFORM_TARGET_ID } from '../engine-client.service';
import { EngineTarget, EngineTargetStore } from '../engine-target.store';
import { ShareInvite, ShareUrlError, parseShareUrl, shareTargetId } from './share-protocol';

@Injectable({ providedIn: 'root' })
export class ShareStore {
  private readonly engine = inject(EngineClient);
  private readonly targets = inject(EngineTargetStore);
  private readonly router = inject(Router);

  readonly joining = signal(false);
  readonly error = signal<string | null>(null);

  /** Joined shares, newest first. */
  readonly shares = computed(() =>
    [...this.targets.shares()].sort((a, b) => (b.lastOk ?? 0) - (a.lastOk ?? 0)),
  );

  /** The active engine is a joined share. */
  readonly activeShare = computed<EngineTarget | null>(() => {
    const active = this.targets.active();
    return active?.kind === 'share' ? active : null;
  });

  /** Parse, register and open a pasted link. Returns the target on success. */
  async join(link: string): Promise<EngineTarget | null> {
    if (this.joining()) {
      return null;
    }
    this.joining.set(true);
    this.error.set(null);
    try {
      const invite = parseShareUrl(link);
      const target = this.register(invite);
      await this.open(target.id);
      return target;
    } catch (err) {
      this.error.set(err instanceof ShareUrlError ? err.message : describe(err));
      return null;
    } finally {
      this.joining.set(false);
    }
  }

  register(invite: ShareInvite): EngineTarget {
    return this.targets.upsert({
      id: shareTargetId(invite.sessionId),
      kind: 'share',
      label: invite.engineName ?? 'Shared session',
      baseUrl: invite.endpoints[0],
      endpoints: invite.endpoints,
      token: invite.token,
      sessionId: invite.sessionId,
    });
  }

  /** Switch to the share's engine (racing its endpoints) and open the chat. */
  async open(targetId: string): Promise<void> {
    const target = await this.engine.switchTarget(targetId);
    if (target.sessionId) {
      await this.router.navigate(['/chat', target.sessionId]);
    }
  }

  /** Back to this device's own engine (sidecar / embedded / saved address). */
  async leave(): Promise<void> {
    const own =
      this.targets.byId(PLATFORM_TARGET_ID['sidecar']) ??
      this.targets.byId(PLATFORM_TARGET_ID['embedded']) ??
      this.targets.byId(PLATFORM_TARGET_ID['remote-url']);
    if (own) {
      await this.engine.switchTarget(own.id);
    }
    await this.router.navigate(['/']);
  }

  forget(targetId: string): void {
    const wasActive = this.targets.activeId() === targetId;
    this.targets.remove(targetId);
    if (wasActive) {
      void this.leave();
    }
  }
}

function describe(err: unknown): string {
  return err instanceof Error ? err.message : String(err);
}
