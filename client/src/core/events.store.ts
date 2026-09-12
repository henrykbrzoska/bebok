/**
 * Events store (SPEC §3.11, §7): one persistent SSE subscription to the
 * engine's global `GET /event` stream, parsed from a fetch body stream.
 *
 * A streaming `fetch` reader gives us the one-way event channel. The store
 * fans events out to subscribers, which filter by `directory` and `sessionID`.
 * On every (re)connect a `reconnectVersion` bump lets active views re-sync
 * their transcript so events that fell into a reconnect gap are not lost.
 * No polling anywhere.
 */

import { Injectable, inject, signal } from '@angular/core';

import { authFetch } from './auth.interceptor';
import { EngineClient } from './engine-client.service';
import { EngineEvent } from './engine.dtos';

/**
 * `error` (F0-1) is the explicit "we tried and could not reach the engine"
 * state: the client always attempts to connect at startup, so a silent `idle`
 * would hide a real failure from the user. The store keeps retrying in the
 * background and moves to `reconnecting` once a stream that was live drops.
 */
export type SseState = 'idle' | 'connecting' | 'live' | 'reconnecting' | 'error';

const RECONNECT_DELAY_MS = 1500;

type Listener = (event: EngineEvent) => void;

@Injectable({ providedIn: 'root' })
export class EventsStore {
  private readonly engine = inject(EngineClient);

  readonly state = signal<SseState>('idle');
  /** Bumped every time the SSE stream is (re)established. */
  readonly reconnectVersion = signal(0);

  private listeners = new Set<Listener>();
  private started = false;
  private stopped = false;
  private running = false;
  private controller: AbortController | null = null;
  /** True once a stream was established at least once in this session. */
  private everLive = false;

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
    // A new target address: past success says nothing about the new one.
    this.everLive = false;
    this.start();
  }

  private async run(): Promise<void> {
    while (!this.stopped) {
      let connection = this.engine.connection();
      if (!connection) {
        this.state.set('connecting');
        try {
          connection = await this.engine.connect();
        } catch (err) {
          // Never reached the engine at all -> surface an error, keep retrying.
          this.state.set(this.everLive ? 'reconnecting' : 'error');
          await this.sleep(RECONNECT_DELAY_MS);
          continue;
        }
      }

      const controller = new AbortController();
      this.controller = controller;
      this.state.set('connecting');

      try {
        const res = await authFetch(`${connection.baseUrl}/event`, {
          method: 'GET',
          headers: { Accept: 'text/event-stream' },
          signal: controller.signal,
        });
        if (!res.ok || !res.body) {
          throw new Error(`SSE /event -> ${res.status}`);
        }

        this.state.set('live');
        this.everLive = true;
        this.reconnectVersion.update((v) => v + 1);
        await this.readStream(res.body, controller.signal);

        // Stream ended cleanly (server restarted/closed) -> reconnect.
      } catch (err) {
        if (this.stopped || controller.signal.aborted) {
          break;
        }
        this.state.set(this.everLive ? 'reconnecting' : 'error');
      } finally {
        this.controller = null;
      }

      if (!this.stopped) {
        await this.sleep(RECONNECT_DELAY_MS);
      }
    }
    this.running = false;
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
        }
      }
    } finally {
      reader.releaseLock();
    }
  }

  /** Parse one SSE block (empty `data:` lines are ignored, `:` comments too). */
  private dispatch(block: string): void {
    let data: string | null = null;
    for (const line of block.split('\n')) {
      if (line.startsWith(':')) {
        continue;
      }
      if (line.startsWith('data:')) {
        const value = line.slice(5).trimStart();
        data = data === null ? value : `${data}\n${value}`;
      }
    }
    if (data === null || data === '') {
      return;
    }
    let event: EngineEvent;
    try {
      event = JSON.parse(data) as EngineEvent;
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
