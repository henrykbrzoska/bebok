/**
 * Open session tabs: the list of sessions shown in the top nav, persisted in
 * localStorage so tabs survive a GUI restart. Newest opened tab goes first,
 * capped to MAX_OPEN. The engine stays the source of truth - a dead session id
 * is dropped when the tab is validated after (re)connect.
 */

import { Injectable, signal } from '@angular/core';

export interface OpenSession {
  id: string;
  title: string | null;
  alias?: string | null;
}

const STORAGE_KEY = 'bebok.openSessions';
const MAX_OPEN = 8;

function load(): OpenSession[] {
  try {
    const raw = localStorage.getItem(STORAGE_KEY);
    if (!raw) {
      return [];
    }
    const parsed: unknown = JSON.parse(raw);
    if (!Array.isArray(parsed)) {
      return [];
    }
    return parsed
      .map((s) => {
        const o = s as Record<string, unknown>;
        return {
          id: typeof o['id'] === 'string' ? o['id'] : '',
          title: typeof o['title'] === 'string' ? o['title'] : null,
          alias: typeof o['alias'] === 'string' ? o['alias'] : null,
        };
      })
      .filter((s) => s.id !== '')
      .slice(0, MAX_OPEN);
  } catch {
    return [];
  }
}

function persist(list: OpenSession[]): void {
  try {
    localStorage.setItem(STORAGE_KEY, JSON.stringify(list));
  } catch {
    /* storage full/unavailable - tabs stay in memory only */
  }
}

@Injectable({ providedIn: 'root' })
export class OpenSessionsStore {
  readonly sessions = signal<OpenSession[]>(load());

  /** Open (or move to front) a tab; optionally set its title and alias. */
  open(id: string, title: string | null = null, alias: string | null = null): void {
    if (!id) {
      return;
    }
    this.sessions.update((list) =>
      persisting([{ id, title, alias }, ...list.filter((s) => s.id !== id)].slice(0, MAX_OPEN)),
    );
  }

  setTitle(id: string, title: string | null): void {
    this.sessions.update((list) =>
      persisting(list.map((s) => (s.id === id ? { ...s, title } : s))),
    );
  }

  setAlias(id: string, alias: string | null): void {
    this.sessions.update((list) =>
      persisting(list.map((s) => (s.id === id ? { ...s, alias } : s))),
    );
  }

  close(id: string): void {
    this.sessions.update((list) => persisting(list.filter((s) => s.id !== id)));
  }

  get(id: string): OpenSession | undefined {
    return this.sessions().find((s) => s.id === id);
  }
}

function persisting(list: OpenSession[]): OpenSession[] {
  persist(list);
  return list;
}
