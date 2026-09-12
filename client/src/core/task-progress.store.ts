/**
 * WP-DELEGATION (F8-2): live progress of delegated sub-agents.
 *
 * The engine publishes `task.started` / `task.progress` (throttled to at most
 * one per second per child) / `task.ended` / `task.aborted` on the *parent*
 * session. This root store folds them into one map keyed by task id so any
 * view can show "what is that child doing right now" without its own SSE
 * plumbing: the chat's `task`/`fleet` tool rows (a one-line sub-row), the
 * Agents drawer panel and the read-only transcript overlay.
 *
 * Entries stay after the child finished (status `done`/`failed`/`aborted`)
 * so a row can keep showing the final line; they are dropped when the parent
 * session's turn settles plus a grace period, or when the map grows past
 * `MAX_ENTRIES` (oldest first). Nothing here talks to the engine.
 */

import { DestroyRef, Injectable, computed, inject, signal } from '@angular/core';

import { EngineEvent, TaskProgress } from './engine.dtos';
import { EventsStore } from './events.store';

export type LiveTaskStatus = 'queued' | 'running' | 'done' | 'failed' | 'aborted';

export interface LiveTaskEntry {
  taskID: string;
  /** Parent session the child was spawned from. */
  sessionID: string;
  childSessionID: string;
  name: string;
  agent: string;
  /** First 80 chars of the delegated prompt (from `task.started`). */
  description: string;
  status: LiveTaskStatus;
  progress: TaskProgress | null;
  tokens: { input: number; output: number };
  startedAt: number;
  /** Unix ms of the last event that touched this entry. */
  updatedAt: number;
  background: boolean;
}

/** Upper bound on remembered entries (finished ones are evicted first). */
export const MAX_ENTRIES = 200;

/** How long a finished entry survives after its parent turn settled. */
export const FINISHED_GRACE_MS = 60_000;

/** Characters of the prompt the engine keeps as `description`. */
export const DESCRIPTION_CHARS = 80;

/**
 * Engine-side child-name normalisation (`SessionState::allocate_child_name`):
 * lowercase, non-alphanumerics -> `-`, trimmed, <= 32 chars. Used to match a
 * tool call's `name` argument against the name the engine actually assigned.
 */
export function normalizeChildName(raw: string): string {
  let out = raw.toLowerCase().replace(/[^a-z0-9]+/g, '-').replace(/^-+|-+$/g, '');
  if (out.length > 32) {
    out = out.slice(0, 32).replace(/^-+|-+$/g, '');
  }
  return out;
}

/** `description` the engine derives from a `task` prompt argument. */
export function descriptionOf(prompt: string): string {
  return Array.from(prompt.trim()).slice(0, DESCRIPTION_CHARS).join('');
}

function str(v: unknown): string {
  return typeof v === 'string' ? v : '';
}

function num(v: unknown): number {
  return typeof v === 'number' && Number.isFinite(v) ? v : 0;
}

@Injectable({ providedIn: 'root' })
export class TaskProgressStore {
  private readonly events = inject(EventsStore);
  private readonly destroyRef = inject(DestroyRef);

  private readonly entriesMap = signal<Map<string, LiveTaskEntry>>(new Map());

  /** Every remembered entry, newest first. */
  readonly entries = computed<LiveTaskEntry[]>(() =>
    [...this.entriesMap().values()].sort((a, b) => b.startedAt - a.startedAt),
  );

  private readonly timers = new Map<string, number>();

  constructor() {
    const unsubscribe = this.events.onEvent((ev) => this.handleEvent(ev));
    this.destroyRef.onDestroy(() => {
      unsubscribe();
      for (const t of this.timers.values()) {
        window.clearTimeout(t);
      }
    });
  }

  /** Entries spawned by one parent session (newest first). */
  forSession(sessionID: string): LiveTaskEntry[] {
    return this.entries().filter((e) => e.sessionID === sessionID);
  }

  /** Live (queued/running) entries of one parent session. */
  liveForSession(sessionID: string): LiveTaskEntry[] {
    return this.forSession(sessionID).filter(
      (e) => e.status === 'queued' || e.status === 'running',
    );
  }

  byTaskID(taskID: string): LiveTaskEntry | null {
    return this.entriesMap().get(taskID) ?? null;
  }

  byChildSession(childSessionID: string): LiveTaskEntry | null {
    return this.entries().find((e) => e.childSessionID === childSessionID) ?? null;
  }

  /**
   * Find the entry a `task` tool call refers to. A tool part does not know
   * its task id, so this matches what the engine derives from the call's
   * arguments: the explicit `name` (normalised, `-2`/`-3` suffix tolerated)
   * or, failing that, the first 80 characters of the prompt.
   */
  matchTaskCall(input: Record<string, unknown> | undefined, sessionID?: string): LiveTaskEntry | null {
    if (!input) {
      return null;
    }
    const pool = sessionID ? this.forSession(sessionID) : this.entries();
    const name = normalizeChildName(str(input['name']));
    if (name) {
      const byName = pool.find((e) => e.name === name || /^-\d+$/.test(e.name.slice(name.length)) && e.name.startsWith(name));
      if (byName) {
        return byName;
      }
    }
    const description = descriptionOf(str(input['prompt']));
    if (!description) {
      return null;
    }
    return pool.find((e) => e.description === description) ?? null;
  }

  /** Apply one engine event (public for tests). */
  handleEvent(ev: EngineEvent): void {
    const props = ev.properties;
    switch (ev.type) {
      case 'task.started': {
        if (!props) {
          return;
        }
        const taskID = str(props['taskID']);
        if (!taskID) {
          return;
        }
        const status = str(props['status']) === 'queued' ? 'queued' : 'running';
        this.upsert(taskID, (prev) => ({
          taskID,
          sessionID: ev.sessionID,
          childSessionID: str(props['childSessionID']),
          name: str(props['name']),
          agent: str(props['agent']),
          description: str(props['description']),
          status,
          progress: prev?.progress ?? null,
          tokens: prev?.tokens ?? { input: 0, output: 0 },
          startedAt: num(props['startedAt']) || Date.now(),
          updatedAt: Date.now(),
          background: props['background'] === true,
        }));
        break;
      }
      case 'task.progress': {
        if (!props) {
          return;
        }
        const taskID = str(props['taskID']);
        if (!taskID) {
          return;
        }
        const tokens = (props['tokens'] ?? {}) as Record<string, unknown>;
        const progress = props['progress'] as TaskProgress | undefined;
        this.upsert(taskID, (prev) => ({
          taskID,
          sessionID: ev.sessionID,
          childSessionID: str(props['childSessionID']) || prev?.childSessionID || '',
          name: str(props['name']) || prev?.name || '',
          agent: str(props['agent']) || prev?.agent || '',
          description: prev?.description ?? '',
          status: str(props['status']) === 'queued' ? 'queued' : 'running',
          progress: progress && typeof progress === 'object' ? progress : (prev?.progress ?? null),
          tokens: { input: num(tokens['input']), output: num(tokens['output']) },
          startedAt: prev?.startedAt ?? Date.now(),
          updatedAt: Date.now(),
          background: prev?.background ?? false,
        }));
        break;
      }
      case 'task.ended': {
        if (!props) {
          return;
        }
        const taskID = str(props['taskID']);
        const prev = this.entriesMap().get(taskID);
        if (!taskID || !prev) {
          return;
        }
        const raw = str(props['status']);
        const status: LiveTaskStatus =
          raw === 'completed' ? 'done' : raw === 'aborted' ? 'aborted' : 'failed';
        const tokens = (props['tokens'] ?? {}) as Record<string, unknown>;
        this.upsert(taskID, () => ({
          ...prev,
          status,
          tokens:
            typeof tokens['input'] === 'number'
              ? { input: num(tokens['input']), output: num(tokens['output']) }
              : prev.tokens,
          updatedAt: Date.now(),
        }));
        this.scheduleEviction(taskID);
        break;
      }
      case 'task.aborted': {
        // The engine follows up with `task.ended{status:"aborted"}`; nothing
        // to do here beyond an optimistic status flip.
        const taskID = str(props?.['taskID']);
        const prev = taskID ? this.entriesMap().get(taskID) : undefined;
        if (prev && (prev.status === 'queued' || prev.status === 'running')) {
          this.upsert(taskID, () => ({ ...prev, status: 'aborted', updatedAt: Date.now() }));
          this.scheduleEviction(taskID);
        }
        break;
      }
      default:
        break;
    }
  }

  private upsert(taskID: string, make: (prev: LiveTaskEntry | undefined) => LiveTaskEntry): void {
    this.entriesMap.update((map) => {
      const next = new Map(map);
      next.set(taskID, make(map.get(taskID)));
      if (next.size > MAX_ENTRIES) {
        const finished = [...next.values()]
          .filter((e) => e.status !== 'queued' && e.status !== 'running')
          .sort((a, b) => a.updatedAt - b.updatedAt);
        for (const e of finished) {
          if (next.size <= MAX_ENTRIES) {
            break;
          }
          next.delete(e.taskID);
        }
      }
      return next;
    });
  }

  private scheduleEviction(taskID: string): void {
    const existing = this.timers.get(taskID);
    if (existing !== undefined) {
      window.clearTimeout(existing);
    }
    const timer = window.setTimeout(() => {
      this.timers.delete(taskID);
      this.entriesMap.update((map) => {
        if (!map.has(taskID)) {
          return map;
        }
        const next = new Map(map);
        next.delete(taskID);
        return next;
      });
    }, FINISHED_GRACE_MS);
    this.timers.set(taskID, timer);
  }
}
