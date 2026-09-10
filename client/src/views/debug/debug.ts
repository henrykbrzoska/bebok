/**
 * Debug view (M6): a live log of LLM API calls (engine -> provider) and HTTP
 * requests to the engine (client -> server), with their responses. The log is
 * a single `debug.log` file owned by the engine, cleared on every app start and
 * capped (old entries dropped first).
 *
 * On top of the flat log, the engine keeps the last 2 full LLM
 * request/response JSON payloads (memory-only ring, never written to disk).
 * They are shown as collapsible panels below.
 */

import { Component, OnDestroy, OnInit, inject, signal } from '@angular/core';
import { RouterLink } from '@angular/router';

import { EngineClient } from '../../core/engine-client.service';
import { prettyJson } from '../../core/format';
import { DebugEntry, DebugLlmCall } from '../../core/engine.dtos';
import { I18nService } from '../../i18n/i18n.service';

const MAX_JSON_CHARS = 50000;

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
  readonly calls = signal<DebugLlmCall[]>([]);
  readonly maxChars = signal(10000);
  readonly error = signal<string | null>(null);
  readonly autoRefresh = signal(true);
  /** Ids of LLM calls whose JSON panel is expanded. */
  readonly expanded = signal<Set<number>>(new Set());

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
      this.calls.set(res.calls ?? []);
      this.maxChars.set(res.maxChars);
    } catch (err) {
      this.error.set(this.describe(err));
    }
  }

  async clear(): Promise<void> {
    try {
      await this.engine.clearDebugLog();
      this.entries.set([]);
      this.calls.set([]);
      this.expanded.set(new Set());
    } catch (err) {
      this.error.set(this.describe(err));
    }
  }

  isExpanded(id: number): boolean {
    return this.expanded().has(id);
  }

  toggle(id: number): void {
    const next = new Set(this.expanded());
    if (next.has(id)) {
      next.delete(id);
    } else {
      next.add(id);
    }
    this.expanded.set(next);
  }

  /** Pretty JSON, truncated client-side to avoid DOM blowup on huge payloads. */
  jsonText(value: unknown): string {
    const text = prettyJson(value);
    if (text.length > MAX_JSON_CHARS) {
      return text.slice(0, MAX_JSON_CHARS) + '\n…[truncated ' + (text.length - MAX_JSON_CHARS) + ' chars]';
    }
    return text;
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
