/**
 * Events store (SPEC §3.11, §7): one persistent SSE subscription to the
 * engine's global `GET /event` stream, parsed from a fetch body stream.
 *
 * A streaming `fetch` reader gives us the one-way event channel. The store
 * fans events out to subscribers, which filter by `directory` and `sessionID`.
 * On every fresh connect a `reconnectVersion` bump lets active views re-sync
 * their transcript so events that fell into a gap are not lost.
 * No polling anywhere.
 *
 * F10-30 (WP-M1 contract, F10-4): every frame carries `id: <seq>`. A
 * reconnect against the *same* engine sends `Last-Event-ID: <seq>` and the
 * engine replays what was missed, so the views keep their state and no full
 * refresh is needed; when the gap is older than the engine's ring buffer (or
 * the engine restarted) the stream starts with `event: resync` (empty data),
 * which bumps `reconnectVersion` like a fresh connect. A remote-scoped stream
 * ends with `event: revoked` when the desktop removed this device: the store
 * parks itself in `unauthorized` (the same state a 401 produces) so
 * `RemoteStore` runs its "device removed" flow immediately instead of after
 * the next reconnect's 401.
 */

import { Injectable, effect, inject, signal, untracked } from '@angular/core';

import { authFetch } from './auth.interceptor';
import { ENGINE_API } from './engine-api';
import { EngineTargetStore } from './engine-target.store';
import { EngineEvent } from './engine.dtos';

/**
 * `error` (F0-1) is the explicit "we tried and could not reach the engine"
 * state: the client always attempts to connect at startup, so a silent `idle`
 * would hide a real failure from the user. The store keeps retrying in the
 * background and moves to `reconnecting` once a stream that was live drops.
 */
export type SseState = 'idle' | 'connecting' | 'live' | 'reconnecting' | 'error' | 'unauthorized';

const RECONNECT_DELAY_MS = 1500;

type Listener = (event: EngineEvent) => void;

/** One parsed SSE block (`id:` / `event:` / `data:` lines; comments skipped). */
export interface SseFrame {
  id: string | null;
  event: string | null;
  data: string | null;
}

/** Parse one SSE block per the spec's line grammar (`\r\n` already normalised). */
export function parseSseBlock(block: string): SseFrame {
  const frame: SseFrame = { id: null, event: null, data: null };
  for (const line of block.split('\n')) {
    if (line === '' || line.startsWith(':')) {
      continue;
    }
    const colon = line.indexOf(':');
    const field = colon < 0 ? line : line.slice(0, colon);
    let value = colon < 0 ? '' : line.slice(colon + 1);
    if (value.startsWith(' ')) {
      value = value.slice(1);
    }
    switch (field) {
      case 'data':
        frame.data = frame.data === null ? value : `${frame.data}\n${value}`;
        break;
      case 'event':
        frame.event = value;
        break;
      case 'id':
        // The spec ignores ids containing NUL; the engine's are plain seqs.
        if (!value.includes(String.fromCharCode(0))) {
          frame.id = value;
        }
        break;
      default:
        break;
    }
  }
  return frame;
}

@Injectable({ providedIn: 'root' })
export class EventsStore {
  private readonly engine = inject(ENGINE_API);
  private readonly targets = inject(EngineTargetStore);

  readonly state = signal<SseState>('idle');
  /**
   * Bumped whenever open views must re-sync from the engine: a fresh stream
   * (first connect, target switch, reconnect without a resumable id) or an
   * `event: resync` from the engine. A reconnect that resumed with
   * `Last-Event-ID` does *not* bump it - the replayed events fill the gap.
   */
  readonly reconnectVersion = signal(0);
  /** Bumped when the engine ended a stream with `event: revoked` (F10-30). */
  readonly revokedVersion = signal(0);

  private listeners = new Set<Listener>();
  private started = false;
  private stopped = false;
  private running = false;
  private controller: AbortController | null = null;
  /** True once a stream was established at least once in this session. */
  private everLive = false;
  /** The target the running stream was opened against (F10-7). */
  private streamTarget: string | null = null;
  /**
   * Bumped by every `start()`. A `run()` loop that wakes from its retry sleep
   * after a `restart()` sees a newer generation and exits instead of racing
   * the fresh loop (two streams against two targets otherwise).
   */
  private generation = 0;
  /**
   * Last `id:` seen, together with the engine it came from (`target|baseUrl`
   * - a sidecar/embedded engine that restarted on a new port must not be
   * resumed with the old engine's seq). Cleared by `restart()`.
   */
  private lastEventId: string | null = null;
  private lastEventIdSource: string | null = null;
  /** Set by `event: revoked`; read by `run()` once the aborted read unwinds. */
  private revoked = false;
  /** Retry back-off between stream attempts; specs shorten it. */
  reconnectDelayMs = RECONNECT_DELAY_MS;

  constructor() {
    // F10-7: `EngineClient.switchTarget()` changes the active target; the
    // stream against the old engine is aborted and a new one opened against
    // the new base URL/token - `reconnectVersion` bumps, views `refreshFull`.
    effect(() => {
      const active = this.targets.activeId();
      untracked(() => {
        if (this.started && this.streamTarget !== null && active !== this.streamTarget) {
          this.restart();
        }
      });
    });
  }

  /** Subscribe to the global event stream. Returns an unsubscribe function. */
  onEvent(listener: Listener): () => void {
    this.listeners.add(listener);
    return () => this.listeners.delete(listener);
  }

  /** Ensure the event stream is open (idempotent across views). */
  start(): void {
    if (this.started || this.running) {
      return;
    }
    this.started = true;
    this.stopped = false;
    this.running = true;
    void this.run();
  }

  /** Stop the stream (used when the user reconfigures the remote engine). */
  stop(): void {
    this.stopped = true;
    this.running = false;
    this.controller?.abort();
    this.controller = null;
    this.state.set('idle');
  }

  /** Reconnect after a config change: stop, drop credentials, start fresh. */
  restart(): void {
    this.stop();
    this.started = false;
    // A new target address: past success says nothing about the new one,
    // and its event seqs mean nothing there either.
    this.everLive = false;
    this.lastEventId = null;
    this.lastEventIdSource = null;
    this.start();
  }

  /** The `Last-Event-ID` a reconnect would send right now (specs/diagnostics). */
  resumeId(): string | null {
    return this.lastEventId;
  }

  private async run(): Promise<void> {
    const gen = ++this.generation;
    while (!this.stopped && gen === this.generation) {
      let connection = this.engine.connection();
      if (!connection) {
        this.state.set('connecting');
        try {
          connection = await this.engine.connect();
        } catch (err) {
          // Never reached the engine at all -> surface an error, keep retrying.
          this.state.set(this.everLive ? 'reconnecting' : 'error');
          await this.sleep(this.reconnectDelayMs);
          continue;
        }
      }

      const controller = new AbortController();
      this.controller = controller;
      this.streamTarget = this.targets.activeId();
      this.revoked = false;
      this.state.set('connecting');

      // F10-30: resume only against the engine the id came from.
      const source = `${this.streamTarget ?? ''}|${connection.baseUrl}`;
      const resumeId = this.lastEventIdSource === source ? this.lastEventId : null;
      const headers: Record<string, string> = { Accept: 'text/event-stream' };
      if (resumeId !== null) {
        headers['Last-Event-ID'] = resumeId;
      }

      try {
        const res = await authFetch(`${connection.baseUrl}/event`, {
          method: 'GET',
          headers,
          signal: controller.signal,
        });
        if (res.status === 401) {
          // Our token is not the running engine's token: retrying cannot
          // help until the user supplies the new address. Park the stream
          // (`restart()` after `EngineClient.reconnect()` resumes it).
          this.state.set('unauthorized');
          this.started = false;
          break;
        }
        if (!res.ok || !res.body) {
          throw new Error(`SSE /event -> ${res.status}`);
        }

        this.state.set('live');
        this.everLive = true;
        this.lastEventIdSource = source;
        if (this.streamTarget !== null) {
          this.targets.markOk(this.streamTarget);
        }
        if (resumeId === null) {
          // Fresh stream: nothing was replayed, views must re-sync.
          this.reconnectVersion.update((v) => v + 1);
        }
        await this.readStream(res.body, controller.signal);

        // Stream ended cleanly (server restarted/closed) -> reconnect.
      } catch (err) {
        if (this.stopped || controller.signal.aborted) {
          if (this.revoked && !this.stopped) {
            this.parkRevoked();
          }
          break;
        }
        this.state.set(this.everLive ? 'reconnecting' : 'error');
      } finally {
        if (this.controller === controller) {
          this.controller = null;
        }
      }

      if (this.revoked && !this.stopped) {
        // The stream ended after `event: revoked` without an abort race.
        this.parkRevoked();
        break;
      }
      if (!this.stopped && gen === this.generation) {
        await this.sleep(this.reconnectDelayMs);
      }
    }
    if (gen === this.generation) {
      this.running = false;
    }
  }

  /**
   * `event: revoked`: the device token is dead. Park exactly like a 401 so
   * `RemoteStore`'s revocation effect (and `switchAndResume`) take over; the
   * old engine's seq is useless from here on.
   */
  private parkRevoked(): void {
    this.revoked = false;
    this.lastEventId = null;
    this.lastEventIdSource = null;
    this.state.set('unauthorized');
    this.started = false;
    this.revokedVersion.update((v) => v + 1);
  }

  private async readStream(body: ReadableStream<Uint8Array>, signal: AbortSignal): Promise<void> {
    const reader = body.getReader();
    const decoder = new TextDecoder();
    let buffer = '';
    try {
      for (;;) {
        if (signal.aborted) {
          return;
        }
        const { done, value } = await reader.read();
        if (done) {
          return;
        }
        buffer += decoder.decode(value, { stream: true }).replace(/\r\n/g, '\n');
        let sep: number;
        while ((sep = buffer.indexOf('\n\n')) >= 0) {
          const block = buffer.slice(0, sep);
          buffer = buffer.slice(sep + 2);
          this.dispatch(block);
          if (this.revoked) {
            return;
          }
        }
      }
    } finally {
      reader.releaseLock();
    }
  }

  /**
   * Handle one SSE block: remember `id:`, act on the engine's named events
   * (`resync`, `revoked`), fan out unnamed `data:` frames as engine events.
   */
  private dispatch(block: string): void {
    const frame = parseSseBlock(block);
    if (frame.id !== null) {
      this.lastEventId = frame.id;
    }
    if (frame.event === 'resync') {
      // The gap is older than the engine's buffer (or the engine restarted):
      // the views must refresh fully, exactly like after a fresh connect.
      this.reconnectVersion.update((v) => v + 1);
      return;
    }
    if (frame.event === 'revoked') {
      this.revoked = true;
      this.controller?.abort();
      return;
    }
    if (frame.event !== null && frame.event !== 'message') {
      // Unknown named event from a newer engine: ignore, never mis-parse.
      return;
    }
    if (frame.data === null || frame.data === '') {
      return;
    }
    let event: EngineEvent;
    try {
      event = JSON.parse(frame.data) as EngineEvent;
    } catch {
      return;
    }
    for (const listener of [...this.listeners]) {
      try {
        listener(event);
      } catch (err) {
        console.error('events.store listener failed', err);
      }
    }
  }

  private sleep(ms: number): Promise<void> {
    return new Promise((resolve) => setTimeout(resolve, ms));
  }
}
