/**
 * Offline cache + command queue (WP-M6 / F10-27).
 *
 * Remote mode follows a desktop that is often unreachable from a phone (no
 * tailnet, Doze, a tunnel in the lift). Two things survive that:
 *
 * 1. **Cache** - the session list of the paired desktop and the last
 *    `MAX_CACHED_MESSAGES` messages of every transcript opened, in IndexedDB.
 *    Records are sealed with a key derived from the target's device token
 *    (`TargetSecrets` - once WP-M5 moves tokens into the Android Keystore the
 *    key material is no longer readable from the app sandbox, which is the
 *    "encryption strengthens later" the plan describes). Without a token or
 *    `crypto.subtle` the record is stored as marked plaintext, never dropped.
 *
 * 2. **Queue** - a prompt / permission decision / abort issued while the
 *    desktop is unreachable is queued for `QUEUE_TTL_MS` (10 min), sent in
 *    order the moment the stream is live again, and otherwise marked
 *    `expired` - visibly, in the queue the UI renders; nothing is dropped
 *    silently and nothing is retried forever.
 *
 * Storage and crypto sit behind small interfaces so the specs run against
 * in-memory fakes with fake timers.
 */

import { Injectable, InjectionToken, computed, effect, inject, signal, untracked } from '@angular/core';

import { ENGINE_API } from '../engine-api';
import { EngineTargetStore } from '../engine-target.store';
import type { Message, SessionMeta } from '../engine.dtos';
import { EventsStore } from '../events.store';

// ---------------------------------------------------------------------------
// storage
// ---------------------------------------------------------------------------

/** Minimal async key/value store (IndexedDB in the app, a Map in specs). */
export interface CacheStore {
  get(key: string): Promise<string | null>;
  set(key: string, value: string): Promise<void>;
  delete(key: string): Promise<void>;
  keys(prefix: string): Promise<string[]>;
}

export class MemoryCacheStore implements CacheStore {
  readonly map = new Map<string, string>();

  async get(key: string): Promise<string | null> {
    return this.map.get(key) ?? null;
  }

  async set(key: string, value: string): Promise<void> {
    this.map.set(key, value);
  }

  async delete(key: string): Promise<void> {
    this.map.delete(key);
  }

  async keys(prefix: string): Promise<string[]> {
    return [...this.map.keys()].filter((k) => k.startsWith(prefix));
  }
}

const DB_NAME = 'bebok-mobile';
const DB_VERSION = 1;
const STORE = 'kv';

/** IndexedDB-backed store; every failure degrades to "no cache" (null). */
export class IndexedDbCacheStore implements CacheStore {
  private db: Promise<IDBDatabase | null> | null = null;

  private open(): Promise<IDBDatabase | null> {
    if (!this.db) {
      this.db = new Promise((resolve) => {
        try {
          const req = indexedDB.open(DB_NAME, DB_VERSION);
          req.onupgradeneeded = () => {
            const db = req.result;
            if (!db.objectStoreNames.contains(STORE)) {
              db.createObjectStore(STORE);
            }
          };
          req.onsuccess = () => resolve(req.result);
          req.onerror = () => resolve(null);
          req.onblocked = () => resolve(null);
        } catch {
          resolve(null);
        }
      });
    }
    return this.db;
  }

  private async run<T>(
    mode: IDBTransactionMode,
    op: (store: IDBObjectStore) => IDBRequest<T>,
  ): Promise<T | null> {
    const db = await this.open();
    if (!db) {
      return null;
    }
    return new Promise((resolve) => {
      try {
        const tx = db.transaction(STORE, mode);
        const req = op(tx.objectStore(STORE));
        req.onsuccess = () => resolve(req.result);
        req.onerror = () => resolve(null);
        tx.onabort = () => resolve(null);
      } catch {
        resolve(null);
      }
    });
  }

  async get(key: string): Promise<string | null> {
    const value = await this.run<unknown>('readonly', (s) => s.get(key));
    return typeof value === 'string' ? value : null;
  }

  async set(key: string, value: string): Promise<void> {
    await this.run('readwrite', (s) => s.put(value, key));
  }

  async delete(key: string): Promise<void> {
    await this.run('readwrite', (s) => s.delete(key));
  }

  async keys(prefix: string): Promise<string[]> {
    const all = await this.run<IDBValidKey[]>('readonly', (s) => s.getAllKeys());
    return (all ?? []).filter((k): k is string => typeof k === 'string' && k.startsWith(prefix));
  }
}

export const OFFLINE_CACHE_STORE = new InjectionToken<CacheStore>('OFFLINE_CACHE_STORE', {
  providedIn: 'root',
  factory: () =>
    typeof indexedDB !== 'undefined' ? new IndexedDbCacheStore() : new MemoryCacheStore(),
});

// ---------------------------------------------------------------------------
// sealing
// ---------------------------------------------------------------------------

/** Seals cache records with a secret (the target's device token). */
export interface CacheCipher {
  seal(secret: string | null, plain: string): Promise<string>;
  /** Returns null when the record cannot be opened (wrong key, corrupt). */
  open(secret: string | null, sealed: string): Promise<string | null>;
}

const PLAIN_PREFIX = 'p:';
const SEALED_PREFIX = 'v1:';

function toBase64(bytes: Uint8Array): string {
  let bin = '';
  for (const b of bytes) {
    bin += String.fromCharCode(b);
  }
  return btoa(bin);
}

function fromBase64(text: string): Uint8Array<ArrayBuffer> {
  const bin = atob(text);
  const out = new Uint8Array(new ArrayBuffer(bin.length));
  for (let i = 0; i < bin.length; i++) {
    out[i] = bin.charCodeAt(i);
  }
  return out;
}

/**
 * AES-GCM-256 with the key `SHA-256(secret)` through `crypto.subtle`; falls
 * back to marked plaintext when there is no secret or no WebCrypto (the
 * record is then readable from the sandbox, exactly like the token that would
 * have unlocked it - no worse than the storage of the token itself).
 */
export class SubtleCacheCipher implements CacheCipher {
  private readonly subtle: SubtleCrypto | null =
    typeof crypto !== 'undefined' && crypto.subtle ? crypto.subtle : null;

  private async key(secret: string): Promise<CryptoKey> {
    const digest = await this.subtle!.digest('SHA-256', new TextEncoder().encode(secret));
    return this.subtle!.importKey('raw', digest, { name: 'AES-GCM' }, false, ['encrypt', 'decrypt']);
  }

  async seal(secret: string | null, plain: string): Promise<string> {
    if (!secret || !this.subtle) {
      return PLAIN_PREFIX + plain;
    }
    try {
      const key = await this.key(secret);
      const iv = crypto.getRandomValues(new Uint8Array(12));
      const ct = await this.subtle.encrypt({ name: 'AES-GCM', iv }, key, new TextEncoder().encode(plain));
      return `${SEALED_PREFIX}${toBase64(iv)}:${toBase64(new Uint8Array(ct))}`;
    } catch {
      return PLAIN_PREFIX + plain;
    }
  }

  async open(secret: string | null, sealed: string): Promise<string | null> {
    if (sealed.startsWith(PLAIN_PREFIX)) {
      return sealed.slice(PLAIN_PREFIX.length);
    }
    if (!sealed.startsWith(SEALED_PREFIX) || !secret || !this.subtle) {
      return null;
    }
    try {
      const [ivB64, ctB64] = sealed.slice(SEALED_PREFIX.length).split(':');
      const key = await this.key(secret);
      const plain = await this.subtle.decrypt(
        { name: 'AES-GCM', iv: fromBase64(ivB64) },
        key,
        fromBase64(ctB64),
      );
      return new TextDecoder().decode(plain);
    } catch {
      return null;
    }
  }
}

export const OFFLINE_CACHE_CIPHER = new InjectionToken<CacheCipher>('OFFLINE_CACHE_CIPHER', {
  providedIn: 'root',
  factory: () => new SubtleCacheCipher(),
});

// ---------------------------------------------------------------------------
// cache
// ---------------------------------------------------------------------------

/** Transcript tail kept per session. */
export const MAX_CACHED_MESSAGES = 200;
/** Transcripts kept per target (least recently saved evicted first). */
export const MAX_CACHED_TRANSCRIPTS = 20;

export interface CachedSessions {
  sessions: SessionMeta[];
  savedAt: number;
}

export interface CachedTranscript {
  sessionID: string;
  messages: Message[];
  savedAt: number;
  /** True when the tail was cut to `MAX_CACHED_MESSAGES`. */
  truncated: boolean;
}

interface TranscriptIndex {
  entries: { sessionID: string; savedAt: number }[];
}

const sessionsKey = (target: string): string => `sessions:${target}`;
const transcriptKey = (target: string, sid: string): string => `transcript:${target}:${sid}`;
const indexKey = (target: string): string => `index:${target}`;

@Injectable({ providedIn: 'root' })
export class OfflineCache {
  private readonly store = inject(OFFLINE_CACHE_STORE);
  private readonly cipher = inject(OFFLINE_CACHE_CIPHER);
  private readonly targets = inject(EngineTargetStore);

  /** Injectable clock (specs). */
  now: () => number = () => Date.now();

  private secretFor(targetId: string): Promise<string | null> {
    return this.targets.tokenFor(targetId).catch(() => null);
  }

  private async write(targetId: string, key: string, value: unknown): Promise<void> {
    const sealed = await this.cipher.seal(await this.secretFor(targetId), JSON.stringify(value));
    await this.store.set(key, sealed);
  }

  private async read<T>(targetId: string, key: string): Promise<T | null> {
    const sealed = await this.store.get(key);
    if (sealed === null) {
      return null;
    }
    const plain = await this.cipher.open(await this.secretFor(targetId), sealed);
    if (plain === null) {
      return null;
    }
    try {
      return JSON.parse(plain) as T;
    } catch {
      return null;
    }
  }

  async putSessions(targetId: string, sessions: SessionMeta[]): Promise<void> {
    await this.write(targetId, sessionsKey(targetId), {
      sessions,
      savedAt: this.now(),
    } satisfies CachedSessions);
  }

  getSessions(targetId: string): Promise<CachedSessions | null> {
    return this.read<CachedSessions>(targetId, sessionsKey(targetId));
  }

  /** Store the tail of a transcript; evicts the oldest beyond the per-target cap. */
  async putTranscript(targetId: string, sessionID: string, messages: Message[]): Promise<void> {
    const tail = messages.slice(-MAX_CACHED_MESSAGES);
    const savedAt = this.now();
    await this.write(targetId, transcriptKey(targetId, sessionID), {
      sessionID,
      messages: tail,
      savedAt,
      truncated: messages.length > tail.length,
    } satisfies CachedTranscript);
    const index = (await this.read<TranscriptIndex>(targetId, indexKey(targetId))) ?? { entries: [] };
    const entries = index.entries.filter((e) => e.sessionID !== sessionID);
    entries.push({ sessionID, savedAt });
    entries.sort((a, b) => b.savedAt - a.savedAt);
    const evicted = entries.splice(MAX_CACHED_TRANSCRIPTS);
    for (const e of evicted) {
      await this.store.delete(transcriptKey(targetId, e.sessionID));
    }
    await this.write(targetId, indexKey(targetId), { entries } satisfies TranscriptIndex);
  }

  getTranscript(targetId: string, sessionID: string): Promise<CachedTranscript | null> {
    return this.read<CachedTranscript>(targetId, transcriptKey(targetId, sessionID));
  }

  /** Drop everything cached for a target (unpair / revoke). */
  async evictTarget(targetId: string): Promise<void> {
    for (const key of [
      ...(await this.store.keys(`sessions:${targetId}`)),
      ...(await this.store.keys(`transcript:${targetId}:`)),
      ...(await this.store.keys(`index:${targetId}`)),
      ...(await this.store.keys(`queue:${targetId}`)),
    ]) {
      await this.store.delete(key);
    }
  }
}

// ---------------------------------------------------------------------------
// command queue
// ---------------------------------------------------------------------------

export const QUEUE_TTL_MS = 10 * 60 * 1000;
/** How often pending commands are checked for expiry. */
export const QUEUE_TICK_MS = 15_000;

export type QueuedCommandKind = 'prompt' | 'permission' | 'abort';
export type QueuedCommandState = 'pending' | 'sending' | 'sent' | 'expired' | 'failed';

export interface QueuedCommand {
  id: string;
  targetId: string;
  sessionID: string;
  kind: QueuedCommandKind;
  /** `prompt`: `{ message }`; `permission`: `{ requestID, decision, always }`; `abort`: `{}`. */
  payload: Record<string, unknown>;
  queuedAt: number;
  expiresAt: number;
  state: QueuedCommandState;
  error?: string;
}

let queueSeq = 0;

@Injectable({ providedIn: 'root' })
export class OfflineQueue {
  private readonly engine = inject(ENGINE_API);
  private readonly events = inject(EventsStore);
  private readonly targets = inject(EngineTargetStore);

  readonly commands = signal<QueuedCommand[]>([]);
  readonly pending = computed(() => this.commands().filter((c) => c.state === 'pending'));
  readonly expired = computed(() => this.commands().filter((c) => c.state === 'expired'));

  /** Injectable clock (specs). */
  now: () => number = () => Date.now();

  private flushing = false;
  private timer: ReturnType<typeof setInterval> | null = null;

  constructor() {
    // The moment the stream is live again, send what waited.
    effect(() => {
      const state = this.events.state();
      untracked(() => {
        if (state === 'live') {
          void this.flush();
        }
      });
    });
  }

  /** True when a command should be queued rather than sent right now. */
  get offline(): boolean {
    const state = this.events.state();
    return state === 'error' || state === 'reconnecting' || state === 'idle';
  }

  enqueue(
    kind: QueuedCommandKind,
    sessionID: string,
    payload: Record<string, unknown>,
    targetId: string | null = this.targets.activeId(),
  ): QueuedCommand {
    const queuedAt = this.now();
    const command: QueuedCommand = {
      id: `q${++queueSeq}-${queuedAt}`,
      targetId: targetId ?? '',
      sessionID,
      kind,
      payload,
      queuedAt,
      expiresAt: queuedAt + QUEUE_TTL_MS,
      state: 'pending',
    };
    this.commands.update((list) => [...list, command]);
    this.ensureTimer();
    return command;
  }

  /** Mark every pending command past its deadline as `expired`. */
  tick(now: number = this.now()): void {
    let changed = false;
    this.commands.update((list) =>
      list.map((c) => {
        if (c.state === 'pending' && c.expiresAt <= now) {
          changed = true;
          return { ...c, state: 'expired' as const };
        }
        return c;
      }),
    );
    if (changed || this.pending().length === 0) {
      this.stopTimerIfIdle();
    }
  }

  /** Remove sent/expired/failed entries the user acknowledged. */
  dismiss(id: string): void {
    this.commands.update((list) => list.filter((c) => c.id !== id));
  }

  clearFinished(): void {
    this.commands.update((list) => list.filter((c) => c.state === 'pending' || c.state === 'sending'));
  }

  /** Send pending commands in order against the active target. */
  async flush(): Promise<void> {
    if (this.flushing) {
      return;
    }
    this.flushing = true;
    try {
      this.tick();
      const active = this.targets.activeId();
      for (const command of this.pending()) {
        if (command.targetId && active && command.targetId !== active) {
          continue; // queued for another desktop - stays pending until it is active again
        }
        this.setState(command.id, 'sending');
        try {
          await this.send(command);
          this.setState(command.id, 'sent');
        } catch (err) {
          const message = err instanceof Error ? err.message : String(err);
          if (isNetworkError(err)) {
            // Still offline: keep it pending for the next live edge.
            this.setState(command.id, 'pending');
            break;
          }
          this.setState(command.id, 'failed', message);
        }
      }
    } finally {
      this.flushing = false;
      this.stopTimerIfIdle();
    }
  }

  private async send(command: QueuedCommand): Promise<void> {
    switch (command.kind) {
      case 'prompt':
        await this.engine.prompt(command.sessionID, String(command.payload['message'] ?? ''));
        return;
      case 'permission':
        await this.engine.resolvePermission(
          command.sessionID,
          String(command.payload['requestID'] ?? ''),
          {
            decision: command.payload['decision'] === 'deny' ? 'deny' : 'allow',
            always: command.payload['always'] === true,
          },
        );
        return;
      case 'abort':
        await this.engine.abort(command.sessionID);
        return;
    }
  }

  private setState(id: string, state: QueuedCommandState, error?: string): void {
    this.commands.update((list) =>
      list.map((c) => (c.id === id ? { ...c, state, ...(error ? { error } : {}) } : c)),
    );
  }

  private ensureTimer(): void {
    if (this.timer === null) {
      this.timer = setInterval(() => this.tick(), QUEUE_TICK_MS);
    }
  }

  private stopTimerIfIdle(): void {
    if (this.timer !== null && this.pending().length === 0) {
      clearInterval(this.timer);
      this.timer = null;
    }
  }
}

/** `fetch` rejects with a TypeError when the host is unreachable. */
export function isNetworkError(err: unknown): boolean {
  if (err instanceof TypeError) {
    return true;
  }
  const message = err instanceof Error ? err.message : String(err);
  return /Failed to fetch|NetworkError|Load failed|network/i.test(message) && !/-> \d{3}/.test(message);
}
