/**
 * Sessions + agents of the currently selected project directory.
 *
 * The sidebar (F1-5/F1-6) and the Start screen (F2-1) both render this list,
 * so it lives in one shell-scoped store instead of being fetched twice. The
 * engine stays the source of truth: this only caches the last response and
 * refreshes when the engine reports a session-level change or the SSE stream
 * reconnects.
 */

import { Injectable, effect, inject, signal, untracked } from '@angular/core';

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
  /**
   * Monotonically increasing request marker.  A directory switch can happen
   * while the previous project's list request is still in flight; only the
   * most recent request is allowed to update the shared sidebar cache.
   */
  private refreshVersion = 0;

  constructor() {
    // A Bebok launch refreshes the index for the last selected project.
    // This is intentionally fire-and-forget: indexing must never block the
    // initial session list, and a missing/disabled index plugin is harmless.
    const initialDirectory = this.directory();
    if (initialDirectory) {
      void this.refreshCodeIndex(initialDirectory);
    }

    // Engine-side session changes (create/delete/title) invalidate the cache.
    this.events.onEvent((event) => {
      if (event.type.startsWith('session.')) {
        void this.refresh();
      }
    });
    // A (re)connect means the previous list may be stale or was never loaded.
    effect(() => {
      void this.events.reconnectVersion();
      // Selection itself calls `refresh()`. Reading the directory untracked
      // here keeps a change of project from starting a duplicate request;
      // this effect is only for SSE reconnects.
      const dir = untracked(() => this.directory());
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
    const changedDirectory = directory !== this.directory();
    // Clear synchronously, before the new directory's request completes. This
    // keeps sessions from another project out of the sidebar during loading.
    if (changedDirectory) {
      this.refreshVersion += 1;
      this.sessions.set([]);
      this.agents.set([]);
      this.lastLoaded = null;
      this.loading.set(false);
      this.error.set(null);
    }
    this.directory.set(directory);
    if (directory) {
      this.engine.saveDirectory(directory);
      if (changedDirectory) {
        // Debug trace: project switch triggers code-index rebuild + refresh.
        console.info(`[project] switched to ${directory} — rebuilding code index + refreshing`);
        void this.refreshCodeIndex(directory);
      }
    } else {
      this.sessions.set([]);
      this.agents.set([]);
    }
    await this.refresh();
  }

  /** Rebuild the selected project's code index, then refresh its status. */
  private async refreshCodeIndex(directory: string): Promise<void> {
    try {
      await this.engine.connect();
      // Manual disable→enable in Settings is what revives a stale index, so
      // a project switch (and launch) replays it programmatically: toggle
      // OFF unregisters the plugin from the global host, toggle ON registers
      // it back from this project's slot dir. Best-effort — indexing must
      // not prevent Bebok or project switching from loading.
      await this.rebindProjectPlugins(directory);
      const rebuild = await this.engine.rebuildIndex(directory);
      console.info(`[project] index rebuild for ${directory}:`, rebuild);
      const status = await this.engine.getIndexStatus(directory);
      console.info(`[project] index status for ${directory}:`, status);
    } catch (err) {
      // Indexing is best-effort: a missing, disabled or unavailable plugin
      // must not prevent Bebok or project switching from loading.
      console.warn(`[project] code index refresh failed for ${directory}`, err);
    }
  }

  /**
   * Programmatic disable→enable of this project's enabled plugins: replays
   * the manual Settings toggle that revives a stale index (toggle OFF
   * unregisters the plugin globally, toggle ON re-registers it from this
   * project's slot dir). Failures are logged and swallowed — best-effort.
   */
  private async rebindProjectPlugins(directory: string): Promise<void> {
    let declared;
    try {
      declared = (await this.engine.listPlugins(directory)).declared ?? [];
    } catch (err) {
      console.warn(`[project] plugin list failed for ${directory}`, err);
      return;
    }
    for (const plugin of declared) {
      if (!plugin.enabled) {
        continue;
      }
      try {
        await this.engine.togglePlugin(directory, plugin.name, false);
        await this.engine.togglePlugin(directory, plugin.name, true);
        console.info(`[project] rebound plugin ${plugin.name} for ${directory}`);
      } catch (err) {
        console.warn(`[project] rebind failed for plugin ${plugin.name}`, err);
      }
    }
  }

  async refresh(): Promise<void> {
    const dir = this.directory();
    if (!dir || !this.engine.connected()) {
      return;
    }
    const version = ++this.refreshVersion;
    this.loading.set(true);
    this.error.set(null);
    try {
      const [sessions, agents] = await Promise.all([
        this.engine.listSessions(dir),
        this.engine.listAgents(dir),
      ]);
      // Ignore a response for an older selection (or an older refresh of the
      // same selection). Without this guard a slow request can put Project A
      // sessions back into the sidebar after the user chose Project B.
      if (version !== this.refreshVersion || dir !== this.directory()) {
        return;
      }
      this.sessions.set(sessions);
      this.agents.set(agents);
      this.lastLoaded = dir;
    } catch (err) {
      if (version !== this.refreshVersion || dir !== this.directory()) {
        return;
      }
      this.error.set(err instanceof Error ? err.message : String(err));
    } finally {
      if (version === this.refreshVersion && dir === this.directory()) {
        this.loading.set(false);
      }
    }
  }

  /** Drop a session from the cache after the engine deleted it. */
  forget(sessionId: string): void {
    this.sessions.update((list) => list.filter((s) => s.id !== sessionId));
  }
}
