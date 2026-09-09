/**
 * Session activity store (M6): tracks which sessions currently have a running
 * turn, driven entirely by the engine's SSE events (`session.updated` with
 * `running: true/false`). Lives at the app root so a chat keeps "working" in the
 * background and the nav can show a spinner while it runs.
 */

import { Injectable, inject, signal } from '@angular/core';

import { EventsStore } from './events.store';
import { EngineEvent } from './engine.dtos';

@Injectable({ providedIn: 'root' })
export class SessionActivityStore {
  private readonly events = inject(EventsStore);

  readonly runningSessions = signal<ReadonlySet<string>>(new Set());

  constructor() {
    this.events.onEvent((ev) => this.handle(ev));
  }

  isRunning(sessionID: string): boolean {
    return this.runningSessions().has(sessionID);
  }

  hasAnyRunning(): boolean {
    return this.runningSessions().size > 0;
  }

  private handle(ev: EngineEvent): void {
    if (ev.type !== 'session.updated') {
      return;
    }
    const running = ev.properties?.['running'] === true;
    this.runningSessions.update((set) => {
      const next = new Set(set);
      if (running) {
        next.add(ev.sessionID);
      } else {
        next.delete(ev.sessionID);
      }
      return next;
    });
  }
}
