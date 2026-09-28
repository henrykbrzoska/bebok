/**
 * Batch store: client side of the OpenRouter Batch API (async bulk jobs).
 *
 * The engine keeps every batch in a `BatchStore` and publishes
 * `batch.submitted` / `batch.progress` / `batch.completed` / `batch.failed`
 * on the global SSE stream. This root store holds the list for one session,
 * folds the events into it (progress + terminal states trigger a reload of
 * the record so item results appear) and exposes `submit()` / `cancel()`.
 *
 * Only `load()`/`reload()`/`submit()`/`cancel()` talk to the engine.
 */

import { DestroyRef, Injectable, Signal, computed, inject, signal } from '@angular/core';

import { EngineClient } from './engine-client.service';
import {
  BatchProgressEvent,
  BatchRecord,
  BatchSubmittedEvent,
  EngineEvent,
} from './engine.dtos';
import { EventsStore } from './events.store';

/** Terminal batch states — no further progress expected. */
const TERMINAL_STATES = new Set(['Completed', 'Failed', 'Cancelled']);

export function batchIsTerminal(state: string): boolean {
  return TERMINAL_STATES.has(state);
}

export interface BatchProgress {
  done: number;
  total: number;
}

@Injectable({ providedIn: 'root' })
export class BatchStore {
  private readonly engine = inject(EngineClient);
  private readonly events = inject(EventsStore);
  private readonly destroyRef = inject(DestroyRef);

  /** Session whose batches are currently listed (null = nothing loaded). */
  readonly sessionId = signal<string | null>(null);
  readonly directory = signal<string | null>(null);
  /** Batch records of the loaded session, newest first. */
  readonly batches = signal<BatchRecord[]>([]);
  /** Live progress per batch id, from `batch.progress` events. */
  readonly progress = signal<Record<string, BatchProgress>>({});
  readonly loading = signal(false);
  readonly error = signal<string | null>(null);

  readonly active = computed(() => this.batches().filter((b) => !batchIsTerminal(b.state)));
  readonly done = computed(() => this.batches().filter((b) => batchIsTerminal(b.state)));

  private unsubscribe: (() => void) | null = null;

  /** Start listing batches of a session (subscribes to SSE once). */
  load(sessionId: string, directory: string): void {
    if (this.sessionId() === sessionId && this.directory() === directory) {
      return;
    }
    this.sessionId.set(sessionId);
    this.directory.set(directory);
    this.batches.set([]);
    this.progress.set({});
    this.subscribe();
    void this.reload();
  }

  /** Stop listing (e.g. session closed). */
  clear(): void {
    this.sessionId.set(null);
    this.directory.set(null);
    this.batches.set([]);
    this.progress.set({});
    this.unsubscribe?.();
    this.unsubscribe = null;
  }

  /** Re-fetch the session's batches from the engine. */
  async reload(): Promise<void> {
    const sessionId = this.sessionId();
    const directory = this.directory();
    if (!sessionId || !directory) {
      return;
    }
    this.loading.set(true);
    this.error.set(null);
    try {
      const rows = await this.engine.sessionBatches(sessionId, directory);
      this.batches.set([...rows].reverse());
    } catch (e) {
      this.error.set(e instanceof Error ? e.message : String(e));
    } finally {
      this.loading.set(false);
    }
  }

  /** Submit a new batch; returns the engine batch id. */
  async submit(model: string, items: { messages: unknown[] }[]): Promise<string> {
    const sessionId = this.sessionId();
    const directory = this.directory();
    if (!sessionId || !directory) {
      throw new Error('batch store is not loaded');
    }
    const { batchID } = await this.engine.submitBatch(sessionId, { directory, model, items });
    await this.reload();
    return batchID;
  }

  /** Cancel a running batch. */
  async cancel(id: string): Promise<void> {
    const directory = this.directory();
    if (!directory) {
      return;
    }
    await this.engine.cancelBatch(id, directory);
    await this.reload();
  }

  /** Progress row for a batch (null when no event seen yet). */
  batchProgress(id: string): Signal<BatchProgress | null> {
    return computed(() => this.progress()[id] ?? null);
  }

  private subscribe(): void {
    this.unsubscribe?.();
    this.unsubscribe = this.events.onEvent((ev: EngineEvent) => this.ingest(ev));
    this.destroyRef.onDestroy(() => this.unsubscribe?.());
  }

  /** Fold one SSE event into the store (public for tests). */
  ingest(ev: EngineEvent): void {
    const sessionId = this.sessionId();
    if (!sessionId || (ev.sessionID && ev.sessionID !== sessionId)) {
      return;
    }
    switch (ev.type) {
      case 'batch.submitted': {
        const p = ev.properties as unknown as BatchSubmittedEvent;
        this.progress.update((m) => ({ ...m, [p.batchID]: { done: 0, total: p.count } }));
        void this.reload();
        break;
      }
      case 'batch.progress': {
        const p = ev.properties as unknown as BatchProgressEvent;
        this.progress.update((m) => ({ ...m, [p.batchID]: { done: p.done, total: p.total } }));
        break;
      }
      case 'batch.completed':
      case 'batch.failed': {
        const p = ev.properties as unknown as { batchID: string };
        void this.reloadSingle(p.batchID);
        break;
      }
      default:
        break;
    }
  }

  private async reloadSingle(id: string): Promise<void> {
    const directory = this.directory();
    if (!directory) {
      return;
    }
    try {
      const record = await this.engine.getBatch(id, directory);
      this.batches.update((rows) => {
        const i = rows.findIndex((r) => r.id === id);
        if (i < 0) {
          return [record, ...rows];
        }
        const next = [...rows];
        next[i] = record;
        return next;
      });
      this.progress.update((m) => {
        const next = { ...m };
        delete next[id];
        return next;
      });
    } catch {
      await this.reload();
    }
  }
}
