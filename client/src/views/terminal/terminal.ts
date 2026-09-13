/**
 * Terminal view (M5): a tab bar of engine-owned PTY sessions.
 *
 * Each tab is one `ptyId`. Tabs are created lazily (the "+" button); existing
 * sessions are listed from the engine so a GUI restart can reattach to the same
 * terminals (the PTYs keep running in the engine). Closing a tab only detaches
 * the client - the PTY survives.
 */

import { Component, OnDestroy, OnInit, computed, effect, inject, signal } from '@angular/core';
import { ActivatedRoute } from '@angular/router';

import { EngineClient } from '../../core/engine-client.service';
import { ProcessInfo } from '../../core/engine.dtos';
import { ProcessesStore, type ProcessTone, formatUptime, processTone } from '../../core/processes.store';
import { I18nService } from '../../i18n/i18n.service';
import { ChatSessionStore } from '../chat/chat-session.store';
import { ProcessLogTab } from './process-log-tab';
import { TerminalTab, type TerminalTabStatus } from './terminal-tab';

/** Registry list poll while the screen is visible (F9-14). */
export const PROCESS_POLL_MS = 5_000;
/** Uptime tick. */
const CLOCK_MS = 1_000;


interface TabMeta {
  ptyId: string;
  title: string;
  status: TerminalTabStatus;
}

@Component({
  selector: 'app-terminal',
  imports: [TerminalTab, ProcessLogTab],
  templateUrl: './terminal.html',
  styleUrl: './terminal.css',
})
export class TerminalView implements OnInit, OnDestroy {
  private readonly engine = inject(EngineClient);
  private readonly route = inject(ActivatedRoute);
  private readonly i18n = inject(I18nService);
  private readonly session = inject(ChatSessionStore);
  readonly procs = inject(ProcessesStore);

  readonly t = this.i18n.t.bind(this.i18n);

  readonly directory = signal<string | null>(null);
  readonly tabs = signal<TabMeta[]>([]);
  readonly activePtyId = signal<string | null>(null);
  readonly loading = signal(false);
  readonly error = signal<string | null>(null);

  /** Shell-style prompt shown in the empty state (`PS <cwd»`). */
  readonly promptPrefix = computed(() => `PS ${this.directory() ?? ''}>`);

  // --- F9-14: background processes ------------------------------------------

  /** Registry rows of the resolved session (running first). */
  readonly processes = this.procs.processes;
  /** Ticks every second while the screen is alive (uptime labels). */
  readonly now = signal(Date.now());
  /** Open log tabs (process ids) in the tab strip, after the PTY tabs. */
  readonly logTabs = signal<string[]>([]);
  /** Active log tab; `activePtyId` is null while one is shown. */
  readonly activeLogId = signal<string | null>(null);
  readonly activeProcess = computed<ProcessInfo | null>(() => {
    const id = this.activeLogId();
    return id ? this.procs.byId(id) : null;
  });

  /**
   * Session whose registry is listed: the chat's active session when it
   * belongs to this directory, else the newest root session of the directory.
   */
  private readonly resolvedSessionId = signal<string | null>(null);

  private nextTabNumber = 1;
  private pollTimer: number | undefined;
  private clockTimer: number | undefined;
  private readonly onVisibility = (): void => {
    if (!document.hidden) {
      void this.refreshProcesses();
    }
  };

  constructor() {
    // Follow the chat's active session while it matches this directory.
    effect(() => {
      const meta = this.session.meta();
      const dir = this.directory();
      if (meta && (!dir || meta.directory === dir)) {
        this.resolvedSessionId.set(meta.id);
      }
    });
    effect(() => {
      const id = this.resolvedSessionId();
      if (id && this.procs.sessionId() !== id) {
        void this.procs.load(id);
      }
    });
    // Drawer handoff: a selection made elsewhere opens that log tab here.
    effect(() => {
      const selected = this.procs.selected();
      if (selected && this.activeLogId() !== selected) {
        this.openLog(selected);
      }
    });
  }

  /** Status word paired with every tab's colored dot (never color alone). */
  statusLabel(status: TerminalTabStatus): string {
    switch (status) {
      case 'live':
        return this.i18n.t('term.statusLive');
      case 'connecting':
        return this.i18n.t('term.statusConnecting');
      case 'error':
        return this.i18n.t('term.statusError');
      default:
        return this.i18n.t('term.statusExited');
    }
  }

  async ngOnInit(): Promise<void> {
    this.directory.set(
      this.route.snapshot.queryParamMap.get('directory') ?? this.engine.readLastDirectory(),
    );
    if (!this.engine.connected()) {
      try {
        await this.engine.connect();
      } catch (err) {
        this.error.set(this.describe(err));
        return;
      }
    }
    await this.refresh();
    await this.resolveSession();
    await this.refreshProcesses();
    this.clockTimer = window.setInterval(() => this.now.set(Date.now()), CLOCK_MS);
    this.pollTimer = window.setInterval(() => {
      if (!document.hidden) {
        void this.refreshProcesses();
      }
    }, PROCESS_POLL_MS);
    document.addEventListener('visibilitychange', this.onVisibility);
  }

  ngOnDestroy(): void {
    if (this.clockTimer !== undefined) {
      window.clearInterval(this.clockTimer);
    }
    if (this.pollTimer !== undefined) {
      window.clearInterval(this.pollTimer);
    }
    document.removeEventListener('visibilitychange', this.onVisibility);
  }

  // --- F9-14: processes ------------------------------------------------------

  /** Pick the session to list when the chat has none for this directory. */
  private async resolveSession(): Promise<void> {
    if (this.resolvedSessionId()) {
      return;
    }
    const dir = this.directory();
    if (!dir) {
      return;
    }
    try {
      const sessions = await this.engine.listSessions(dir);
      const roots = sessions.filter((s) => !s.parent);
      const newest = [...(roots.length ? roots : sessions)].sort(
        (a, b) => (b.updated_at ?? 0) - (a.updated_at ?? 0),
      )[0];
      if (newest && !this.resolvedSessionId()) {
        this.resolvedSessionId.set(newest.id);
      }
    } catch {
      /* no session list: the processes section stays empty */
    }
  }

  /** Manual refresh button + the 5 s poll. */
  async refreshProcesses(): Promise<void> {
    if (!this.resolvedSessionId()) {
      await this.resolveSession();
    }
    const id = this.resolvedSessionId();
    if (id) {
      await this.procs.load(id);
    }
  }

  tone(p: ProcessInfo): ProcessTone {
    return processTone(p);
  }

  processStatusLabel(p: ProcessInfo): string {
    if (p.status === 'running') {
      return this.i18n.t('processes.running');
    }
    const exited = this.i18n.t('processes.exited');
    return p.exit_code !== null && p.exit_code !== undefined
      ? `${exited} · ${this.i18n.t('processes.exitCode', { code: p.exit_code })}`
      : exited;
  }

  uptime(p: ProcessInfo): string {
    const end = p.status === 'running' ? this.now() : p.ended_at ?? this.now();
    return formatUptime(p.started_at, end);
  }

  /** `localhost:4200` from the url, else `:4200` from the port, for the badge. */
  portBadge(p: ProcessInfo): string {
    if (p.url) {
      try {
        return new URL(p.url).host || p.url;
      } catch {
        return p.url;
      }
    }
    return p.port ? `:${p.port}` : '';
  }

  /** Tab title for a log tab (the command, ellipsised by CSS). */
  logTabTitle(id: string): string {
    return this.procs.byId(id)?.command ?? this.i18n.t('processes.log');
  }

  /** Row click / drawer handoff: open (or focus) the read-only log tab. */
  openLog(id: string): void {
    if (!this.logTabs().includes(id)) {
      this.logTabs.update((list) => [...list, id]);
    }
    this.activePtyId.set(null);
    this.activeLogId.set(id);
    this.procs.select(id);
  }

  closeLog(id: string): void {
    this.logTabs.update((list) => list.filter((x) => x !== id));
    if (this.activeLogId() !== id) {
      return;
    }
    const remaining = this.logTabs();
    if (remaining.length > 0) {
      this.openLog(remaining[remaining.length - 1]);
      return;
    }
    this.activeLogId.set(null);
    this.procs.select(null);
    const ptys = this.tabs();
    this.activePtyId.set(ptys.length > 0 ? ptys[ptys.length - 1].ptyId : null);
  }

  /** List existing terminal sessions from the engine (reattach targets). */
  async refresh(): Promise<void> {
    this.loading.set(true);
    this.error.set(null);
    try {
      const ptys = await this.engine.listPtys();
      const existing: TabMeta[] = ptys.map((p, i) => ({
        ptyId: p.pty_id,
        title: p.title ?? p.command,
        status: p.exited ? 'exited' : 'connecting',
      }));
      this.nextTabNumber = Math.max(this.nextTabNumber, existing.length + 1);
      this.tabs.set(existing);
      if (!this.activePtyId() && !this.activeLogId() && existing.length > 0) {
        this.activePtyId.set(existing[0].ptyId);
      }
    } catch (err) {
      this.error.set(this.describe(err));
    } finally {
      this.loading.set(false);
    }
  }

  async addTerminal(): Promise<void> {
    const dir = this.directory();
    if (!dir) {
      this.error.set(this.i18n.t('term.noDirectory'));
      return;
    }
    this.error.set(null);
    try {
      const created = await this.engine.createPty(dir);
      const title = this.i18n.t('term.tabTitle', { n: this.nextTabNumber++ });
      this.tabs.update((list) => [
        ...list,
        { ptyId: created.ptyId, title, status: 'connecting' },
      ]);
      this.activate(created.ptyId);
    } catch (err) {
      this.error.set(this.describe(err));
    }
  }

  activate(ptyId: string): void {
    this.activeLogId.set(null);
    this.procs.select(null);
    this.activePtyId.set(ptyId);
  }

  closeTab(ptyId: string): void {
    this.tabs.update((list) => list.filter((t) => t.ptyId !== ptyId));
    if (this.activePtyId() === ptyId) {
      const remaining = this.tabs();
      this.activePtyId.set(
        remaining.length > 0 ? remaining[remaining.length - 1].ptyId : null,
      );
    }
    // Deliberately no kill: the PTY keeps running in the engine.
  }

  onStatus(ptyId: string, status: TerminalTabStatus): void {
    this.tabs.update((list) =>
      list.map((t) => (t.ptyId === ptyId ? { ...t, status } : t)),
    );
  }

  describe(err: unknown): string {
    return err instanceof Error ? err.message : String(err);
  }
}
