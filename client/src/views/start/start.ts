/**
 * Start screen: the merged Connect + Project-directory view (WP-SHELL /
 * F0-1, F2-1, F2-2).
 *
 * The client always tries to reach the engine at startup (saved address, or
 * the default `http://127.0.0.1:8787`) - there is no silent "idle" state any
 * more. While the probe runs the status row shows "connecting…" in
 * `--warning`; when it fails the address form reappears with the attempted
 * URL and a Retry button in `--danger`. The address form is therefore only
 * shown after a failed attempt, never by default.
 */

import { Component, OnDestroy, OnInit, computed, inject, signal } from '@angular/core';
import { FormsModule } from '@angular/forms';
import { Router, RouterLink } from '@angular/router';

import { EngineClient } from '../../core/engine-client.service';
import { OpenSessionsStore } from '../../core/open-sessions.store';
import { EventsStore } from '../../core/events.store';
import { AgentInfo, SessionMeta } from '../../core/engine.dtos';
import { I18nService } from '../../i18n/i18n.service';
import { ProjectSessionsStore } from '../../ui/shell/project-sessions.store';
import { StatusDot, type StatusTone } from '../../ui/status-dot/status-dot';

export type ConnectionPhase = 'connecting' | 'live' | 'error';

@Component({
  selector: 'app-start',
  imports: [FormsModule, RouterLink, StatusDot],
  templateUrl: './start.html',
  styleUrl: './start.css',
})
export class StartView implements OnInit, OnDestroy {
  private readonly engine = inject(EngineClient);
  private readonly events = inject(EventsStore);
  private readonly router = inject(Router);
  private readonly i18n = inject(I18nService);
  private readonly openSessions = inject(OpenSessionsStore);
  private readonly project = inject(ProjectSessionsStore);

  readonly t = this.i18n.t.bind(this.i18n);

  readonly isTauri = this.engine.isTauri;

  /** connection phase */
  readonly connecting = signal(true);
  readonly connected = signal(false);
  readonly error = signal<string | null>(null);

  /** engine address form (revealed after a failed attempt, or on demand) */
  readonly remoteBaseUrl = signal('');
  readonly showAddressForm = signal(false);

  /** working directory + sessions (shared with the sidebar) */
  readonly directory = signal<string | null>(null);
  readonly sessions = this.project.sessions;
  readonly agents = this.project.agents;
  readonly loadingSessions = this.project.loading;
  readonly selectedAgent = signal('code');
  readonly creating = signal(false);
  /** Session id pending delete confirmation (two-step confirm). */
  readonly confirmDeleteId = signal<string | null>(null);
  readonly deletingId = signal<string | null>(null);
  private deleteTimer: ReturnType<typeof setTimeout> | null = null;

  /** One of the three states the handoff asks for: connecting / live / error. */
  readonly phase = computed<ConnectionPhase>(() => {
    if (this.error()) {
      return 'error';
    }
    if (this.connecting() || !this.connected()) {
      return 'connecting';
    }
    return 'live';
  });

  readonly statusTone = computed<StatusTone>(() => {
    switch (this.phase()) {
      case 'live':
        return 'success';
      case 'error':
        return 'danger';
      default:
        return 'warning';
    }
  });

  readonly statusLabel = computed(() => {
    switch (this.phase()) {
      case 'live':
        return this.t('status.live');
      case 'error':
        return this.t('start.connectError', { url: this.attemptedUrl() });
      default:
        return this.t('start.connectingTo', { url: this.attemptedUrl() });
    }
  });

  /** The address the last attempt used (shown in the error message). */
  readonly attemptedUrl = signal('');

  async ngOnInit(): Promise<void> {
    const defaults = this.engine.remoteDefaults();
    this.remoteBaseUrl.set(defaults.baseUrl);
    this.attemptedUrl.set(defaults.baseUrl);

    const last = this.engine.readLastDirectory();
    if (last) {
      this.directory.set(last);
    }

    // F0-1: always attempt a connection at startup, on every platform.
    await this.connect();
  }

  /**
   * Resolve a connection for the current platform, probe it and wire the SSE
   * stream. Failure is explicit (message + Retry + editable address).
   */
  async connect(): Promise<void> {
    this.connecting.set(true);
    this.error.set(null);
    try {
      const connection = await this.engine.connect();
      this.attemptedUrl.set(connection.baseUrl);
      await this.engine.ping();
      this.events.start();
      this.connected.set(true);
      this.showAddressForm.set(false);
      if (this.directory()) {
        await this.project.select(this.directory(), true);
      }
    } catch (err) {
      this.connected.set(false);
      this.showAddressForm.set(true);
      this.error.set(this.describe(err));
    } finally {
      this.connecting.set(false);
    }
  }

  /** Retry with the address currently in the form (http/browser mode). */
  async retry(): Promise<void> {
    if (!this.isTauri()) {
      const baseUrl = this.remoteBaseUrl().trim() || 'http://127.0.0.1:8787';
      const normalized = baseUrl.replace(/\/+$/, '');
      this.engine.reconfigure({ kind: 'http', baseUrl: normalized });
      this.attemptedUrl.set(normalized);
      this.events.restart();
    }
    await this.connect();
  }

  toggleAddressForm(): void {
    this.showAddressForm.update((shown) => !shown);
  }

  async browse(): Promise<void> {
    this.error.set(null);
    const picked = await this.engine.pickDirectory(this.i18n.t('dialog.pickDirectory'));
    if (!picked) {
      return;
    }
    this.directory.set(picked);
    await this.project.select(picked, true);
  }

  applyDirectory(): void {
    const value = this.directory()?.trim();
    if (!value) {
      return;
    }
    void this.project.select(value, true);
  }

  async refreshSessions(): Promise<void> {
    await this.project.refresh();
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

  agentLabel(agent: AgentInfo): string {
    return agent.model ? `${agent.name} · ${agent.model}` : agent.name;
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
      this.project.forget(session.id);
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

  sessionMeta(session: SessionMeta): string {
    const tokens = session.usage.input_tokens + session.usage.output_tokens;
    return `${session.agent} · ${tokens} tok · ${this.formatDate(session.created_at)}`;
  }

  formatDate(ms: number): string {
    return new Date(ms).toLocaleString();
  }
}
