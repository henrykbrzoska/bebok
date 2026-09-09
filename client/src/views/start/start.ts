/**
 * Start view (M3): connect to an engine, pick a working directory (native
 * Tauri dialog or typed path in http mode) and either open an existing session
 * or create a new one. All state lives in the engine; this view only calls the
 * engine API.
 */

import { Component, OnDestroy, OnInit, inject, signal } from '@angular/core';
import { FormsModule } from '@angular/forms';
import { Router, RouterLink } from '@angular/router';

import { EngineClient } from '../../core/engine-client.service';
import { OpenSessionsStore } from '../../core/open-sessions.store';
import { EventsStore } from '../../core/events.store';
import { AgentInfo, SessionMeta } from '../../core/engine.dtos';
import { I18nService } from '../../i18n/i18n.service';

@Component({
  selector: 'app-start',
  imports: [FormsModule, RouterLink],
  templateUrl: './start.html',
  styleUrl: './start.css',
})
export class StartView implements OnInit, OnDestroy {
  private readonly engine = inject(EngineClient);
  private readonly events = inject(EventsStore);
  private readonly router = inject(Router);
  private readonly i18n = inject(I18nService);
  private readonly openSessions = inject(OpenSessionsStore);

  readonly t = this.i18n.t.bind(this.i18n);

  readonly isTauri = this.engine.isTauri;
  readonly eventsState = this.events.state;

  /** connection phase */
  readonly connecting = signal(false);
  readonly connected = signal(false);
  readonly error = signal<string | null>(null);

  /** remote engine form (http mode only) */
  readonly remoteBaseUrl = signal('');

  /** working directory + sessions */
  readonly directory = signal<string | null>(null);
  readonly sessions = signal<SessionMeta[]>([]);
  readonly agents = signal<AgentInfo[]>([]);
  readonly selectedAgent = signal('code');
  readonly loadingSessions = signal(false);
  readonly creating = signal(false);
  /** Session id pending delete confirmation (two-step confirm). */
  readonly confirmDeleteId = signal<string | null>(null);
  readonly deletingId = signal<string | null>(null);
  private deleteTimer: ReturnType<typeof setTimeout> | null = null;

  async ngOnInit(): Promise<void> {
    const defaults = this.engine.remoteDefaults();
    this.remoteBaseUrl.set(defaults.baseUrl);

    // Restore the last directory and its session list when possible.
    const last = this.engine.readLastDirectory();
    if (last) {
      this.directory.set(last);
    }

    // Auto-connect on desktop (sidecar) and mobile (embedded engine).
    if (this.isTauri() || this.engine.isCapacitor) {
      await this.connect();
    }

    if (this.connected() && this.directory()) {
      await this.refreshSessions();
    }
  }

  async connect(): Promise<void> {
    this.connecting.set(true);
    this.error.set(null);
    try {
      await this.engine.connect();
      this.events.start();
      this.connected.set(true);
    } catch (err) {
      this.error.set(this.describe(err));
    } finally {
      this.connecting.set(false);
    }
  }

  /** (http mode) store the typed URL and reconnect. */
  async connectRemote(): Promise<void> {
    const baseUrl = this.remoteBaseUrl().trim() || 'http://127.0.0.1:8787';
    this.engine.reconfigure({
      kind: 'http',
      baseUrl: baseUrl.replace(/\/+$/, ''),
    });
    this.connecting.set(true);
    this.error.set(null);
    try {
      // Reachability probe before wiring the SSE stream.
      await this.engine.ping();
      this.events.restart();
      this.connected.set(true);
    } catch (err) {
      this.error.set(this.describe(err));
    } finally {
      this.connecting.set(false);
    }
  }

  async browse(): Promise<void> {
    this.error.set(null);
    const picked = await this.engine.pickDirectory(this.i18n.t('dialog.pickDirectory'));
    if (!picked) {
      return;
    }
    this.directory.set(picked);
    this.engine.saveDirectory(picked);
    await this.refreshSessions();
  }

  applyDirectory(): void {
    const value = this.directory()?.trim();
    if (!value) {
      return;
    }
    this.engine.saveDirectory(value);
    void this.refreshSessions();
  }

  async refreshSessions(): Promise<void> {
    const dir = this.directory();
    if (!dir) {
      return;
    }
    this.loadingSessions.set(true);
    this.error.set(null);
    try {
      const [sessions, agents] = await Promise.all([
        this.engine.listSessions(dir),
        this.engine.listAgents(dir),
      ]);
      this.sessions.set(sessions);
      this.agents.set(agents);
    } catch (err) {
      this.error.set(this.describe(err));
    } finally {
      this.loadingSessions.set(false);
    }
  }

  async startSession(): Promise<void> {
    const dir = this.directory();
    if (!dir) {
      return;
    }
    this.creating.set(true);
    this.error.set(null);
    try {
      const created = await this.engine.createSession(dir, this.selectedAgent());
      await this.router.navigate(['/chat', created.sessionID]);
    } catch (err) {
      this.error.set(this.describe(err));
      this.creating.set(false);
    }
  }

  async openSession(session: SessionMeta): Promise<void> {
    await this.router.navigate(['/chat', session.id]);
  }

  /** First click on the delete button: arm the confirmation (auto-cancels). */
  requestDeleteSession(session: SessionMeta, event: Event): void {
    event.stopPropagation();
    if (this.confirmDeleteId() === session.id) {
      return;
    }
    this.confirmDeleteId.set(session.id);
    this.error.set(null);
    if (this.deleteTimer) {
      clearTimeout(this.deleteTimer);
    }
    this.deleteTimer = setTimeout(() => this.confirmDeleteId.set(null), 6000);
  }

  /** Second click (armed): actually delete the session on the engine. */
  async confirmDeleteSession(session: SessionMeta, event: Event): Promise<void> {
    event.stopPropagation();
    if (this.deletingId() !== null) {
      return;
    }
    this.cancelDelete();
    this.deletingId.set(session.id);
    this.error.set(null);
    try {
      await this.engine.deleteSession(session.id);
      this.sessions.update((list) => list.filter((s) => s.id !== session.id));
      this.openSessions.close(session.id);
    } catch (err) {
      this.error.set(this.describe(err));
    } finally {
      this.deletingId.set(null);
    }
  }

  cancelDelete(): void {
    if (this.deleteTimer) {
      clearTimeout(this.deleteTimer);
      this.deleteTimer = null;
    }
    this.confirmDeleteId.set(null);
  }

  ngOnDestroy(): void {
    if (this.deleteTimer) {
      clearTimeout(this.deleteTimer);
    }
  }

  describe(err: unknown): string {
    return err instanceof Error ? err.message : String(err);
  }

  formatDate(ms: number): string {
    return new Date(ms).toLocaleString();
  }
}
