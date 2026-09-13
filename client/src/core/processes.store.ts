/**
 * F9-14: background process registry, client side.
 *
 * The engine keeps every `bash background:true` child in a registry and
 * publishes `process.output` (coalesced log chunks) / `process.exited` on the
 * global SSE stream. This root store holds the list for one session (plus its
 * descendants - the endpoint already includes them), folds the events into
 * it and keeps a bounded per-process output buffer so a log tab can be opened
 * at any time and show what streamed in since the app started. It also
 * carries the "which log tab is selected" handoff between the right-drawer
 * Terminal panel and the Terminal screen.
 *
 * Only `load()`/`reload()`/`kill()` talk to the engine.
 */

import { DestroyRef, Injectable, Signal, WritableSignal, computed, inject, signal } from '@angular/core';
import { Subject } from 'rxjs';

import { ENGINE_API } from './engine-api';
import {
  EngineEvent,
  ProcessExitedEvent,
  ProcessInfo,
  ProcessOutputEvent,
} from './engine.dtos';
import { EventsStore } from './events.store';

/** Upper bound of one process' remembered output (characters ~ bytes). */
export const OUTPUT_RING_BYTES = 200 * 1024;

/** How far past the cap `trimRing` may look for a newline to cut on. */
const LINE_CUT_SLACK = 4096;

/** Debounce for the list reload triggered by events about unknown processes. */
const RELOAD_DEBOUNCE_MS = 750;

export interface ProcessChunk {
  id: string;
  chunk: string;
  at: number;
}

/**
 * `12s` / `3m 05s` / `2h 03m` / `1d 4h` for a process started at `startedAt`
 * (Unix ms) as seen at `now`. Never negative.
 */
export function formatUptime(startedAt: number, now: number): string {
  const total = Math.max(0, Math.floor((now - startedAt) / 1000));
  const days = Math.floor(total / 86_400);
  const hours = Math.floor((total % 86_400) / 3600);
  const minutes = Math.floor((total % 3600) / 60);
  const seconds = total % 60;
  const pad = (n: number): string => String(n).padStart(2, '0');
  if (days > 0) {
    return `${days}d ${hours}h`;
  }
  if (hours > 0) {
    return `${hours}h ${pad(minutes)}m`;
  }
  if (minutes > 0) {
    return `${minutes}m ${pad(seconds)}s`;
  }
  return `${seconds}s`;
}

export type ProcessTone = 'running' | 'exited' | 'failed';

/** Status dot tone: running=success, exited 0=muted, exited non-zero=danger. */
export function processTone(p: Pick<ProcessInfo, 'status' | 'exit_code'>): ProcessTone {
  if (p.status === 'running') {
    return 'running';
  }
  return p.exit_code ? 'failed' : 'exited';
}

export interface DetectedUrl {
  url: string;
  port: number;
}

const LOCAL_URL_RE =
  /https?:\/\/(?:localhost|127\.0\.0\.1|0\.0\.0\.0|\[::1?\]|::1)(?::(\d{1,5}))?(?:\/[^\s"'<>)\]]*)?/i;
const PORT_RE = /\bport\s*[:=]?\s*(\d{2,5})\b/i;

/**
 * Client-side fallback for the engine's port/url detection: the first
 * `http://localhost:<port>` / `http://127.0.0.1:<port>` (any path) or a bare
 * "port <n>" mention (turned into `http://localhost:<n>`). Null when nothing
 * looks like a local server.
 */
export function detectUrl(text: string): DetectedUrl | null {
  if (!text) {
    return null;
  }
  const url = LOCAL_URL_RE.exec(text);
  if (url) {
    const port = url[1] ? Number(url[1]) : url[0].toLowerCase().startsWith('https') ? 443 : 80;
    return { url: url[0].replace(/[.,;:]+$/, ''), port };
  }
  const port = PORT_RE.exec(text);
  if (port) {
    const n = Number(port[1]);
    if (n > 0 && n < 65_536) {
      return { url: `http://localhost:${n}`, port: n };
    }
  }
  return null;
}

function str(v: unknown): string {
  return typeof v === 'string' ? v : '';
}

@Injectable({ providedIn: 'root' })
export class ProcessesStore {
  private readonly engine = inject(ENGINE_API);
  private readonly events = inject(EventsStore);
  private readonly destroyRef = inject(DestroyRef);

  /** Session whose processes are currently listed (null = nothing loaded). */
  readonly sessionId = signal<string | null>(null);
  /** Rows of the loaded session and its descendants, newest first. */
  readonly processes = signal<ProcessInfo[]>([]);
  readonly loading = signal(false);
  readonly error = signal<string | null>(null);

  readonly running = computed(() => this.processes().filter((p) => p.status === 'running'));

  /**
   * Log tab the Terminal screen should show. Set by the drawer panel (then
   * navigates) and by the screen itself when the user activates a log tab;
   * null while a PTY tab (or nothing) is active.
   */
  readonly selected = signal<string | null>(null);

  /** Every `process.output` chunk, after it was folded into the buffer. */
  readonly chunks$ = new Subject<ProcessChunk>();

  private readonly buffers = new Map<string, WritableSignal<string>>();
  private reloadTimer: number | undefined;
  private loadSeq = 0;

  constructor() {
    const unsubscribe = this.events.onEvent((ev) => this.handleEvent(ev));
    this.destroyRef.onDestroy(() => {
      unsubscribe();
      if (this.reloadTimer !== undefined) {
        window.clearTimeout(this.reloadTimer);
      }
    });
  }

  /** Fetch the list for `sessionId` (replaces whatever was loaded). */
  async load(sessionId: string): Promise<void> {
    const seq = ++this.loadSeq;
    if (this.sessionId() !== sessionId) {
      this.sessionId.set(sessionId);
      this.processes.set([]);
    }
    this.loading.set(true);
    this.error.set(null);
    try {
      const list = await this.engine.sessionProcesses(sessionId);
      if (seq !== this.loadSeq) {
        return; // a newer load() won
      }
      this.processes.set(sort(list.map((p) => this.decorate(p))));
    } catch (err) {
      if (seq === this.loadSeq) {
        this.error.set(err instanceof Error ? err.message : String(err));
      }
    } finally {
      if (seq === this.loadSeq) {
        this.loading.set(false);
      }
    }
  }

  /** Re-fetch the current session's list (no-op when nothing is loaded). */
  async reload(): Promise<void> {
    const id = this.sessionId();
    if (id) {
      await this.load(id);
    }
  }

  /** Forget the list (e.g. no active session). */
  clear(): void {
    this.loadSeq++;
    this.sessionId.set(null);
    this.processes.set([]);
    this.loading.set(false);
    this.error.set(null);
  }

  byId(id: string): ProcessInfo | null {
    return this.processes().find((p) => p.id === id) ?? null;
  }

  select(id: string | null): void {
    this.selected.set(id);
  }

  /** `POST /processes/{id}/kill`; the returned row replaces the local one. */
  async kill(id: string): Promise<ProcessInfo> {
    const info = await this.engine.killProcess(id);
    this.upsert({ ...info, status: info.status ?? 'exited' });
    return info;
  }

  /**
   * Output of one process as accumulated from `process.output` (and
   * `seed()`), capped at `OUTPUT_RING_BYTES` - the oldest part is dropped.
   */
  output(id: string): Signal<string> {
    return this.bufferFor(id).asReadonly();
  }

  /**
   * Replace the buffer with a log snapshot (`GET /processes/{id}/log`), so a
   * freshly opened tab starts from the file rather than from whatever
   * streamed in since the app started.
   */
  seed(id: string, text: string): void {
    this.bufferFor(id).set(trimRing(text));
    this.applyDetection(id, text);
  }

  /** Append one chunk (public for tests and for the log fetch race handling). */
  append(id: string, chunk: string, at = Date.now()): void {
    if (!chunk) {
      return;
    }
    this.bufferFor(id).update((current) => trimRing(current + chunk));
    this.applyDetection(id, chunk);
    this.chunks$.next({ id, chunk, at });
  }

  /** Insert or replace one row, keeping the newest-first order. */
  upsert(info: ProcessInfo): void {
    const decorated = this.decorate(info);
    this.processes.update((list) => {
      const index = list.findIndex((p) => p.id === info.id);
      if (index < 0) {
        return sort([...list, decorated]);
      }
      const next = list.slice();
      next[index] = { ...list[index], ...decorated };
      return next;
    });
  }

  private handleEvent(ev: EngineEvent): void {
    if (ev.type === 'process.output') {
      const props = (ev.properties ?? {}) as Partial<ProcessOutputEvent>;
      const id = str(props.id);
      if (!id) {
        return;
      }
      this.append(id, str(props.chunk), typeof props.at === 'number' ? props.at : Date.now());
      if (this.sessionId() && !this.byId(id)) {
        // A process we have not listed yet (freshly spawned) - refresh soon.
        this.scheduleReload();
      }
      return;
    }
    if (ev.type === 'process.exited') {
      const props = (ev.properties ?? {}) as Partial<ProcessExitedEvent>;
      const id = str(props.id);
      if (!id) {
        return;
      }
      const code = typeof props.code === 'number' ? props.code : null;
      const known = this.byId(id);
      if (known) {
        this.upsert({ ...known, status: 'exited', exit_code: code, ended_at: Date.now() });
      }
      if (this.sessionId()) {
        this.scheduleReload();
      }
    }
  }

  private scheduleReload(): void {
    if (this.reloadTimer !== undefined) {
      return;
    }
    this.reloadTimer = window.setTimeout(() => {
      this.reloadTimer = undefined;
      void this.reload();
    }, RELOAD_DEBOUNCE_MS);
  }

  private bufferFor(id: string): WritableSignal<string> {
    let buffer = this.buffers.get(id);
    if (!buffer) {
      buffer = signal('');
      this.buffers.set(id, buffer);
    }
    return buffer;
  }

  /** Fill `port`/`url` from the text when the engine did not detect them. */
  private applyDetection(id: string, text: string): void {
    const known = this.byId(id);
    if (!known || known.url || known.port) {
      return;
    }
    const detected = detectUrl(text);
    if (detected) {
      this.upsert({ ...known, url: detected.url, port: detected.port });
    }
  }

  /** Normalise a row: camelCase alias, and detection from the buffered output. */
  private decorate(info: ProcessInfo): ProcessInfo {
    const row: ProcessInfo = {
      ...info,
      session_id: info.session_id ?? info.sessionID ?? '',
      sessionID: info.sessionID ?? info.session_id,
    };
    if (!row.url && !row.port) {
      const detected = detectUrl(this.buffers.get(info.id)?.() ?? '');
      if (detected) {
        row.url = detected.url;
        row.port = detected.port;
      }
    }
    if (row.url === undefined && typeof row.port === 'number') {
      row.url = `http://localhost:${row.port}`;
    }
    return row;
  }
}

function sort(list: ProcessInfo[]): ProcessInfo[] {
  return [...list].sort((a, b) => {
    if (a.status !== b.status) {
      return a.status === 'running' ? -1 : 1;
    }
    return (b.started_at ?? 0) - (a.started_at ?? 0);
  });
}

/** Keep the last `OUTPUT_RING_BYTES` characters, cutting at a line boundary when cheap. */
function trimRing(text: string): string {
  if (text.length <= OUTPUT_RING_BYTES) {
    return text;
  }
  let cut = text.length - OUTPUT_RING_BYTES;
  const nl = text.indexOf('\n', cut);
  if (nl >= 0 && nl - cut < LINE_CUT_SLACK) {
    cut = nl + 1;
  }
  return text.slice(cut);
}
