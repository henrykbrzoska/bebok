/**
 * Terminal view (M5): a tab bar of engine-owned PTY sessions.
 *
 * Each tab is one `ptyId`. Tabs are created lazily (the "+" button); existing
 * sessions are listed from the engine so a GUI restart can reattach to the same
 * terminals (the PTYs keep running in the engine). Closing a tab only detaches
 * the client - the PTY survives.
 */

import { Component, OnInit, inject, signal } from '@angular/core';
import { ActivatedRoute, RouterLink } from '@angular/router';

import { EngineClient } from '../../core/engine-client.service';
import { I18nService } from '../../i18n/i18n.service';
import { TerminalTab, type TerminalTabStatus } from './terminal-tab';

interface TabMeta {
  ptyId: string;
  title: string;
  status: TerminalTabStatus;
}

@Component({
  selector: 'app-terminal',
  imports: [RouterLink, TerminalTab],
  templateUrl: './terminal.html',
  styleUrl: './terminal.css',
})
export class TerminalView implements OnInit {
  private readonly engine = inject(EngineClient);
  private readonly route = inject(ActivatedRoute);
  private readonly i18n = inject(I18nService);

  readonly t = this.i18n.t.bind(this.i18n);

  readonly directory = signal<string | null>(null);
  readonly tabs = signal<TabMeta[]>([]);
  readonly activePtyId = signal<string | null>(null);
  readonly loading = signal(false);
  readonly error = signal<string | null>(null);

  private nextTabNumber = 1;

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
      if (!this.activePtyId() && existing.length > 0) {
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
      this.activePtyId.set(created.ptyId);
    } catch (err) {
      this.error.set(this.describe(err));
    }
  }

  activate(ptyId: string): void {
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
