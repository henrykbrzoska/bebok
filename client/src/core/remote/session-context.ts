/**
 * Which session the phone is "looking at" (WP-M6 / F10-24, F10-26).
 *
 * The desktop keeps one `ChatView` alive and the right-drawer panels read its
 * `ChatSessionStore`. On the phone every tab is its own route, so the
 * Changes / Agents / Processes tabs open without a chat on screen and need
 * to know which session to show. This store remembers the last session the
 * user opened (any tab, any engine target) - mirrored from
 * `ChatSessionStore.meta()` whenever a chat is visible - and persists it so
 * a relaunched app lands on the same one.
 */

import { Injectable, computed, effect, inject, signal, untracked } from '@angular/core';

import { EngineTargetStore } from '../engine-target.store';
import { ChatSessionStore } from '../../views/chat/chat-session.store';

const KEY = 'bebok.mobile.lastSession';

interface Remembered {
  targetId: string | null;
  sessionID: string;
  title: string | null;
  directory: string | null;
}

function read(): Remembered | null {
  try {
    const raw = localStorage.getItem(KEY);
    if (!raw) {
      return null;
    }
    const parsed = JSON.parse(raw) as Partial<Remembered>;
    if (typeof parsed.sessionID !== 'string' || !parsed.sessionID) {
      return null;
    }
    return {
      targetId: typeof parsed.targetId === 'string' ? parsed.targetId : null,
      sessionID: parsed.sessionID,
      title: typeof parsed.title === 'string' ? parsed.title : null,
      directory: typeof parsed.directory === 'string' ? parsed.directory : null,
    };
  } catch {
    return null;
  }
}

@Injectable({ providedIn: 'root' })
export class MobileSessionContext {
  private readonly chat = inject(ChatSessionStore);
  private readonly targets = inject(EngineTargetStore);

  private readonly remembered = signal<Remembered | null>(read());

  /** Session id the read-only tabs should show (null = nothing yet). */
  readonly sessionID = computed(() => {
    const live = this.chat.meta();
    if (live) {
      return live.id;
    }
    const remembered = this.remembered();
    if (!remembered) {
      return null;
    }
    // A session remembered for another engine target is not offered.
    const active = this.targets.activeId();
    return remembered.targetId === null || active === null || remembered.targetId === active
      ? remembered.sessionID
      : null;
  });

  /**
   * Target the remembered session belongs to when it is *not* the active one
   * (a paired desktop after a relaunch, which boots on the phone's own
   * engine). The read-only tabs use it to switch back to that desktop.
   */
  readonly rememberedTargetId = computed<string | null>(() => {
    if (this.chat.meta()) {
      return null;
    }
    const remembered = this.remembered();
    if (!remembered || remembered.targetId === null) {
      return null;
    }
    return remembered.targetId === this.targets.activeId() ? null : remembered.targetId;
  });

  readonly title = computed(() => {
    const live = this.chat.meta();
    if (live) {
      return live.title || live.alias || null;
    }
    return this.remembered()?.title ?? null;
  });

  readonly directory = computed(() => this.chat.meta()?.directory ?? this.remembered()?.directory ?? null);

  constructor() {
    effect(() => {
      const meta = this.chat.meta();
      if (!meta) {
        return;
      }
      untracked(() =>
        this.remember(meta.id, meta.title || meta.alias || null, meta.directory ?? null),
      );
    });
  }

  remember(sessionID: string, title: string | null = null, directory: string | null = null): void {
    const entry: Remembered = {
      targetId: this.targets.activeId(),
      sessionID,
      title,
      directory,
    };
    const current = this.remembered();
    if (
      current &&
      current.sessionID === entry.sessionID &&
      current.targetId === entry.targetId &&
      current.title === entry.title &&
      current.directory === entry.directory
    ) {
      return;
    }
    this.remembered.set(entry);
    try {
      localStorage.setItem(KEY, JSON.stringify(entry));
    } catch {
      /* memory only */
    }
  }

  forget(): void {
    this.remembered.set(null);
    try {
      localStorage.removeItem(KEY);
    } catch {
      /* ignore */
    }
  }
}
