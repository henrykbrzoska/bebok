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
  /**
   * Sum of the *known* per-turn costs. `null` only when no usage part carried
   * a price (unknown-pricing model, or no turn yet) - a session mixing known
   * and unknown pricing shows the known part. A genuine `0` stays `0`.
   */
  cost: number | null;
}

/** Shown wherever a cost would be, when the pricing is unknown (F6-5). */
export const UNKNOWN_COST_LABEL = '—';

/** `$0.0125` for a known cost (including a genuine zero), `—` for unknown. */
export function formatCost(cost: number | null): string {
  return cost === null ? UNKNOWN_COST_LABEL : `$${cost.toFixed(4)}`;
}

/** Catalog fallback for models with no known window (mirrors the engine's 64k). */
export const DEFAULT_CONTEXT_WINDOW = 64_000;
/** Meter turns `--warning` at this fill level ... */
export const CONTEXT_WARNING_PERCENT = 80;
/** ... and `--danger` at this one. */
export const CONTEXT_DANGER_PERCENT = 95;

export type ContextLevel = 'ok' | 'warning' | 'danger';

/**
 * Auto-compaction (F6-4): before the next prompt is sent, a session whose
 * meter is at or above this fill level is compacted first. Client-side UI
 * preference (localStorage), not an engine/config field: it gates a purely
 * client-driven action and needs no engine restart to change. 0 disables.
 */
export const DEFAULT_AUTO_COMPACT_PERCENT = 85;
const KEY_AUTO_COMPACT_PERCENT = 'bebok.chat.autoCompactPercent';
/** The engine refuses to compact a transcript shorter than this. */
export const MIN_COMPACTABLE_MESSAGES = 4;

function readAutoCompactPercent(): number {
  try {
    const raw = localStorage.getItem(KEY_AUTO_COMPACT_PERCENT);
    if (raw === null) {
      return DEFAULT_AUTO_COMPACT_PERCENT;
    }
    const value = Number(raw);
    return Number.isFinite(value) ? Math.min(100, Math.max(0, Math.round(value))) : DEFAULT_AUTO_COMPACT_PERCENT;
  } catch {
    return DEFAULT_AUTO_COMPACT_PERCENT;
  }
}

/**
 * Threshold boundary for auto-compaction: fires at or above `threshold`
 * percent, never before the first turn (null) and never when disabled (0).
 */
export function shouldAutoCompact(percent: number | null, threshold: number): boolean {
  if (percent === null || threshold <= 0) {
    return false;
  }
  return percent >= threshold;
}

/**
 * Budget handed to `POST /session/{id}/compact`: the engine keeps a tail that
 * fits half of it, so half the window leaves the fork at roughly a quarter.
 */
export function compactBudgetFor(contextWindow: number): number {
  return Math.max(1, Math.floor(contextWindow / 2));
}

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

  /** Auto-compaction threshold (percent of the window; 0 = off). */
  readonly autoCompactPercent = signal(readAutoCompactPercent());

  /** True when the next prompt should be preceded by a compaction. */
  readonly needsAutoCompact = computed(() =>
    shouldAutoCompact(this.contextPercent(), this.autoCompactPercent()),
  );

  /** Budget for a compaction request derived from the live window. */
  readonly compactBudget = computed(() => compactBudgetFor(this.contextWindow()));

  /** The engine needs a few messages before it can summarize anything. */
  readonly canCompact = computed(() => this.messages().length >= MIN_COMPACTABLE_MESSAGES);

  setAutoCompactPercent(percent: number): void {
    const clamped = Math.min(100, Math.max(0, Math.round(percent)));
    this.autoCompactPercent.set(clamped);
    try {
      localStorage.setItem(KEY_AUTO_COMPACT_PERCENT, String(clamped));
    } catch {
      /* storage unavailable - keep the choice in memory only */
    }
  }

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
      cost: null,
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
        // Unknown pricing (`cost` null/missing) must not coerce to $0: only
        // known figures are summed, so the total is null until one arrives.
        if (typeof usage.cost === 'number' && Number.isFinite(usage.cost)) {
          totals.cost = (totals.cost ?? 0) + usage.cost;
        }
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

  /** `$X.XXXX` when the pricing is known (a real `$0.0000` included), else `—`. */
  readonly costLabel = computed(() => formatCost(this.totals().cost));

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
