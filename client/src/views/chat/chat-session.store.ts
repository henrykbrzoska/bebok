/**
 * Chat session state shared with the right drawer (F2-12).
 *
 * The drawer is rendered by the app shell, *outside* the chat component tree,
 * so the Session panel cannot receive the transcript as an input. Rather than
 * re-fetching the session (and duplicating the engine layer), `ChatView`
 * publishes what it already holds here and the panel derives its numbers from
 * the same signals. Nothing in this store talks to the engine.
 */

import { Injectable, computed, signal } from '@angular/core';

import { Message, SessionMeta, UsagePart } from '../../core/engine.dtos';

export interface TokenTotals {
  input: number;
  output: number;
  cacheRead: number;
  cacheWrite: number;
  cost: number;
}

/** Catalog fallback for models with no known window (mirrors the engine's 64k). */
export const DEFAULT_CONTEXT_WINDOW = 64_000;
/** Meter turns `--warning` at this fill level ... */
export const CONTEXT_WARNING_PERCENT = 80;
/** ... and `--danger` at this one. */
export const CONTEXT_DANGER_PERCENT = 95;

export type ContextLevel = 'ok' | 'warning' | 'danger';

/** `42k` / `1.2M` / `950` - compact token count for the meter. */
export function formatTokens(value: number): string {
  if (value >= 1_000_000) {
    return `${(value / 1_000_000).toFixed(1).replace(/\.0$/, '')}M`;
  }
  if (value >= 1000) {
    return `${Math.round(value / 1000)}k`;
  }
  return String(Math.round(value));
}

/** Meter colour band for a fill percentage (null = no data yet). */
export function contextLevelFor(percent: number | null): ContextLevel {
  if (percent === null) {
    return 'ok';
  }
  if (percent >= CONTEXT_DANGER_PERCENT) {
    return 'danger';
  }
  return percent >= CONTEXT_WARNING_PERCENT ? 'warning' : 'ok';
}

/** One touched file plus its line deltas, derived from the tool calls. */
export interface FileChange {
  path: string;
  added: number;
  removed: number;
}

/** Number of lines in a string ('' counts as zero). */
function lineCount(value: unknown): number {
  return typeof value === 'string' && value.length > 0 ? value.split('\n').length : 0;
}

@Injectable({ providedIn: 'root' })
export class ChatSessionStore {
  /** Published by `ChatView` for the session currently on screen. */
  readonly meta = signal<SessionMeta | null>(null);
  readonly messages = signal<Message[]>([]);
  readonly running = signal(false);

  /** Transcript model filter (null = every model), driven from the panel. */
  readonly filterModel = signal<string | null>(null);

  readonly directory = computed(() => this.meta()?.directory ?? null);

  /**
   * Context meter (F6-3). Sourced from `meta()` - the engine records the last
   * LLM call's input size and resolves the window from its model catalog -
   * not re-derived from message parts, since this is a property of the
   * session's model, not a sum over history.
   */
  readonly contextUsed = computed<number | null>(() => {
    const used = this.meta()?.context_used;
    return typeof used === 'number' && used >= 0 ? used : null;
  });

  readonly contextWindow = computed<number>(() => {
    const window = this.meta()?.context_window;
    return typeof window === 'number' && window > 0 ? window : DEFAULT_CONTEXT_WINDOW;
  });

  /** Whole-number fill percentage, or null before the first turn. */
  readonly contextPercent = computed<number | null>(() => {
    const used = this.contextUsed();
    if (used === null) {
      return null;
    }
    return Math.round((used / this.contextWindow()) * 100);
  });

  readonly contextLevel = computed<ContextLevel>(() => contextLevelFor(this.contextPercent()));

  /** `42k / 200k · 21%` for the toolbar and the drawer; empty before a turn. */
  readonly contextLabel = computed<string>(() => {
    const used = this.contextUsed();
    const percent = this.contextPercent();
    if (used === null || percent === null) {
      return '';
    }
    return `${formatTokens(used)} / ${formatTokens(this.contextWindow())} · ${percent}%`;
  });

  readonly modelsUsed = computed<string[]>(() => {
    const seen = new Set<string>();
    for (const message of this.messages()) {
      const model = message.meta?.model;
      if (model) {
        seen.add(model);
      }
    }
    return [...seen];
  });

  readonly totals = computed<TokenTotals>(() => {
    const totals: TokenTotals = {
      input: 0,
      output: 0,
      cacheRead: 0,
      cacheWrite: 0,
      cost: 0,
    };
    for (const message of this.messages()) {
      for (const part of message.parts) {
        if (part.type !== 'usage') {
          continue;
        }
        const usage = part as UsagePart;
        totals.input += usage.input_tokens ?? 0;
        totals.output += usage.output_tokens ?? 0;
        totals.cacheRead += usage.cache_read_input_tokens ?? 0;
        totals.cacheWrite += usage.cache_creation_input_tokens ?? 0;
        totals.cost += usage.cost ?? 0;
      }
    }
    return totals;
  });

  readonly cacheRate = computed<string>(() => {
    const totals = this.totals();
    const total = totals.input + totals.cacheRead + totals.cacheWrite;
    if (total === 0) {
      return '–';
    }
    return `${((totals.cacheRead / total) * 100).toFixed(1)}%`;
  });

  readonly costLabel = computed(() =>
    this.totals().cost > 0 ? `$${this.totals().cost.toFixed(4)}` : '$0.0000',
  );

  /**
   * Files touched by this session with their line deltas. `write_file` and
   * `append_file` only add lines; `edit_file` swaps `old_string` for
   * `new_string`, so both sides count. Failed calls are ignored.
   */
  readonly filesChanged = computed<FileChange[]>(() => {
    const byPath = new Map<string, FileChange>();
    for (const message of this.messages()) {
      for (const part of message.parts) {
        if (part.type !== 'tool' || part.state.state === 'error') {
          continue;
        }
        const input = part.state.input as Record<string, unknown> | undefined;
        const path = input?.['path'];
        if (typeof path !== 'string' || !path) {
          continue;
        }
        let added = 0;
        let removed = 0;
        if (part.name === 'write_file' || part.name === 'append_file') {
          added = lineCount(input['content']);
        } else if (part.name === 'edit_file') {
          added = lineCount(input['new_string']);
          removed = lineCount(input['old_string']);
        } else {
          continue;
        }
        const entry = byPath.get(path) ?? { path, added: 0, removed: 0 };
        entry.added += added;
        entry.removed += removed;
        byPath.set(path, entry);
      }
    }
    return [...byPath.values()].slice(0, 40);
  });

  /** Reset when the chat view is torn down or switches to a fresh session. */
  clear(): void {
    this.meta.set(null);
    this.messages.set([]);
    this.running.set(false);
    this.filterModel.set(null);
  }
}
