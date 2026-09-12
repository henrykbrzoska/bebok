/**
 * Debug view (M6): a live log of LLM API calls (engine -> provider) and HTTP
 * requests to the engine (client -> server), with their responses. The log is
 * a single `debug.log` file owned by the engine, cleared on every app start and
 * capped (old entries dropped first).
 *
 * On top of the flat log, the engine keeps the last 2 full LLM
 * request/response JSON payloads (memory-only ring, never written to disk).
 * They are shown as collapsible panels below.
 *
 * WP-TOOLS-UI (F2-20/F2-21) restyles this to the design handoff section 7:
 * a 44px header (title, cap note, filter, auto refresh, Pause/Clear) over a
 * scrollable list of bordered rows. Since WP-AUTH (F0-6) the endpoint is gated
 * behind `BEBOK_DIAGNOSTIC` and answers 404 when it is off - that case shows a
 * friendly explanation instead of a raw error. The capability token lives in
 * the Authorization header and is never read or rendered here.
 */

import { Component, OnDestroy, OnInit, computed, inject, signal } from '@angular/core';
import { FormsModule } from '@angular/forms';

import { EngineClient } from '../../core/engine-client.service';
import { prettyJson } from '../../core/format';
import { DebugEntry, DebugLlmCall } from '../../core/engine.dtos';
import { I18nService } from '../../i18n/i18n.service';

const MAX_JSON_CHARS = 50000;

export type LogTone = 'danger' | 'success' | 'accent' | 'muted';

/** One rendered log row (F2-21). */
export interface LogRow {
  index: number;
  time: string;
  source: string;
  pill: string;
  tone: LogTone;
  path: string;
  meta: string;
}

@Component({
  selector: 'app-debug',
  imports: [FormsModule],
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
  /** Pause freezes the view: no polling and no new entries are applied. */
  readonly paused = signal(false);
  readonly filter = signal('');
  /** `/debug/log` answered 404 -> the engine runs without BEBOK_DIAGNOSTIC. */
  readonly diagnosticOff = signal(false);
  /** Ids of LLM calls whose JSON panel is expanded. */
  readonly expanded = signal<Set<number>>(new Set());

  private timer?: number;

  /** "10 000" - grouped like the handoff's "max 10 000 chars" note. */
  readonly maxCharsLabel = computed(() =>
    this.i18n.t('debug.maxChars', {
      n: this.maxChars().toString().replace(/\B(?=(\d{3})+(?!\d))/g, ' '),
    }),
  );

  /** Newest first, filtered, shaped into the row model the template renders. */
  readonly rows = computed<LogRow[]>(() => {
    const needle = this.filter().trim().toLowerCase();
    const out: LogRow[] = [];
    const entries = this.entries();
    for (let i = entries.length - 1; i >= 0; i -= 1) {
      const entry = entries[i];
      if (needle && !this.matches(entry, needle)) {
        continue;
      }
      out.push(this.toRow(entry, i));
    }
    return out;
  });

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
      if (this.autoRefresh() && !this.paused() && !this.diagnosticOff()) {
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
      this.diagnosticOff.set(false);
      this.error.set(null);
      this.entries.set(res.entries);
      this.calls.set(res.calls ?? []);
      this.maxChars.set(res.maxChars);
    } catch (err) {
      const message = this.describe(err);
      // F0-6: the endpoint is gone (404) unless the engine runs with
      // BEBOK_DIAGNOSTIC=1 - explain that instead of showing a raw error.
      if (/->\s*404\b/.test(message)) {
        this.diagnosticOff.set(true);
        this.error.set(null);
        this.entries.set([]);
        this.calls.set([]);
        return;
      }
      this.error.set(message);
    }
  }

  async clear(): Promise<void> {
    try {
      await this.engine.clearDebugLog();
      this.entries.set([]);
      this.calls.set([]);
      this.expanded.set(new Set());
    } catch (err) {
      const message = this.describe(err);
      if (/->\s*404\b/.test(message)) {
        this.diagnosticOff.set(true);
        return;
      }
      this.error.set(message);
    }
  }

  togglePause(): void {
    const next = !this.paused();
    this.paused.set(next);
    if (!next) {
      void this.refresh();
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

  private matches(entry: DebugEntry, needle: string): boolean {
    return (
      entry.title.toLowerCase().includes(needle) ||
      entry.detail.toLowerCase().includes(needle) ||
      entry.source.toLowerCase().includes(needle) ||
      entry.kind.toLowerCase().includes(needle)
    );
  }

  /**
   * HTTP entries carry `GET /path` as the title and `200 (3ms)` as the detail
   * (see the engine's request middleware); LLM entries are free-form, so their
   * pill falls back to the entry kind.
   */
  private toRow(entry: DebugEntry, index: number): LogRow {
    const status = /^(\d{3})/.exec(entry.detail)?.[1] ?? null;
    const isError = entry.kind === 'error';
    let pill: string;
    let tone: LogTone;

    if (entry.source === 'http') {
      pill = isError ? this.i18n.t('debug.pillError') : (status ?? entry.kind.toUpperCase());
      tone = isError ? 'danger' : status?.startsWith('2') ? 'success' : 'muted';
    } else {
      pill = isError ? this.i18n.t('debug.pillError') : entry.kind.toUpperCase();
      tone = isError ? 'danger' : entry.kind === 'response' ? 'success' : 'accent';
    }

    const timing = /^(\d{3})\s*\((\d+)ms\)$/.exec(entry.detail);
    return {
      index,
      time: this.formatTime(entry.ts),
      source: entry.source.toUpperCase(),
      pill,
      tone,
      path: entry.title,
      meta: timing ? `${timing[1]} · ${timing[2]}ms` : entry.detail,
    };
  }
}
