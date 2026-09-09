/**
 * Debug view (M6): a live log of LLM API calls (engine -> provider) and HTTP
 * requests to the engine (client -> server), with their responses. The log is
 * a single `debug.log` file owned by the engine, cleared on every app start and
 * capped (old entries dropped first).
 */

import { Component, OnDestroy, OnInit, inject, signal } from '@angular/core';
import { RouterLink } from '@angular/router';

import { EngineClient } from '../../core/engine-client.service';
import { DebugEntry } from '../../core/engine.dtos';
import { I18nService } from '../../i18n/i18n.service';

@Component({
  selector: 'app-debug',
  imports: [RouterLink],
  templateUrl: './debug.html',
  styleUrl: './debug.css',
})
export class DebugView implements OnInit, OnDestroy {
  private readonly engine = inject(EngineClient);
  private readonly i18n = inject(I18nService);

  readonly t = this.i18n.t.bind(this.i18n);

  readonly entries = signal<DebugEntry[]>([]);
  readonly maxChars = signal(10000);
  readonly error = signal<string | null>(null);
  readonly autoRefresh = signal(true);

  private timer?: number;

  async ngOnInit(): Promise<void> {
    if (!this.engine.connected()) {
      try {
        await this.engine.connect();
      } catch (err) {
        this.error.set(this.describe(err));
        return;
      }
    }
    await this.refresh();
    this.timer = window.setInterval(() => {
      if (this.autoRefresh()) {
        void this.refresh();
      }
    }, 2000);
  }

  ngOnDestroy(): void {
    if (this.timer !== undefined) {
      window.clearInterval(this.timer);
    }
  }

  async refresh(): Promise<void> {
    try {
      const res = await this.engine.debugLog();
      this.entries.set(res.entries);
      this.maxChars.set(res.maxChars);
    } catch (err) {
      this.error.set(this.describe(err));
    }
  }

  async clear(): Promise<void> {
    try {
      await this.engine.clearDebugLog();
      this.entries.set([]);
    } catch (err) {
      this.error.set(this.describe(err));
    }
  }

  formatTime(ts: number): string {
    const d = new Date(ts);
    const pad = (n: number): string => n.toString().padStart(2, '0');
    return `${pad(d.getHours())}:${pad(d.getMinutes())}:${pad(d.getSeconds())}`;
  }

  describe(err: unknown): string {
    return err instanceof Error ? err.message : String(err);
  }
}
