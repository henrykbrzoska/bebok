/**
 * Sessions + agents of the currently selected project directory.
 *
 * The sidebar (F1-5/F1-6) and the Start screen (F2-1) both render this list,
 * so it lives in one shell-scoped store instead of being fetched twice. The
 * engine stays the source of truth: this only caches the last response and
 * refreshes when the engine reports a session-level change or the SSE stream
 * reconnects.
 */

import { Injectable, effect, inject, signal } from '@angular/core';

import { EngineClient } from '../../core/engine-client.service';
import { EventsStore } from '../../core/events.store';
import { AgentInfo, SessionMeta } from '../../core/engine.dtos';

@Injectable({ providedIn: 'root' })
export class ProjectSessionsStore {
  private readonly engine = inject(EngineClient);
  private readonly events = inject(EventsStore);

  readonly directory = signal<string | null>(this.engine.readLastDirectory());
  readonly sessions = signal<SessionMeta[]>([]);
  readonly agents = signal<AgentInfo[]>([]);
  readonly loading = signal(false);
  readonly error = signal<string | null>(null);

  private lastLoaded: string | null = null;

  constructor() {
    // Engine-side session changes (create/delete/title) invalidate the cache.
    this.events.onEvent((event) => {
      if (event.type.startsWith('session.')) {
        void this.refresh();
      }
    });
    // A (re)connect means the previous list may be stale or was never loaded.
    effect(() => {
      void this.events.reconnectVersion();
      const dir = this.directory();
      if (dir) {
        void this.refresh();
      }
    });
  }

  /** Point the store at a directory and load it (no-op when unchanged). */
  async select(directory: string | null, force = false): Promise<void> {
    if (!force && directory === this.directory() && this.lastLoaded === directory) {
      return;
    }
    this.directory.set(directory);
    if (directory) {
      this.engine.saveDirectory(directory);
    } else {
      this.sessions.set([]);
      this.agents.set([]);
    }
    await this.refresh();
  }

  async refresh(): Promise<void> {
    const dir = this.directory();
    if (!dir || !this.engine.connected()) {
      return;
    }
    this.loading.set(true);
    this.error.set(null);
    try {
      const [sessions, agents] = await Promise.all([
        this.engine.listSessions(dir),
        this.engine.listAgents(dir),
      ]);
      this.sessions.set(sessions);
      this.agents.set(agents);
      this.lastLoaded = dir;
    } catch (err) {
      this.error.set(err instanceof Error ? err.message : String(err));
    } finally {
      this.loading.set(false);
    }
  }

  /** Drop a session from the cache after the engine deleted it. */
  forget(sessionId: string): void {
    this.sessions.update((list) => list.filter((s) => s.id !== sessionId));
  }
}
