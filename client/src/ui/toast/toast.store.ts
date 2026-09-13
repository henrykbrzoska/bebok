/**
 * Toast notifications (F9-5).
 *
 * A root service holding a signal list of short, transient messages; the
 * `<app-toast-host>` (mounted once in the chat view) renders them as a
 * bottom-right stack. `show()` returns the toast id so callers can dismiss
 * early. Auto-dismiss defaults to 5 s; `ttlMs: 0` keeps a toast until it is
 * clicked or dismissed programmatically.
 */

import { Injectable, signal } from '@angular/core';

export type ToastKind = 'info' | 'success' | 'warning' | 'danger';

export interface Toast {
  id: number;
  text: string;
  kind: ToastKind;
  /** Unix ms. */
  at: number;
}

export interface ToastOptions {
  kind?: ToastKind;
  /** Auto-dismiss delay; `0` disables it. Default 5000. */
  ttlMs?: number;
}

export const DEFAULT_TOAST_TTL_MS = 5_000;
/** Older toasts are dropped once the stack grows beyond this. */
const MAX_TOASTS = 5;

@Injectable({ providedIn: 'root' })
export class ToastStore {
  private readonly list = signal<Toast[]>([]);
  private readonly timers = new Map<number, ReturnType<typeof setTimeout>>();
  private seq = 0;

  /** Visible toasts, oldest first. */
  readonly toasts = this.list.asReadonly();

  show(text: string, options: ToastOptions = {}): number {
    const id = ++this.seq;
    const toast: Toast = { id, text, kind: options.kind ?? 'info', at: Date.now() };
    this.list.update((current) => {
      const next = [...current, toast];
      while (next.length > MAX_TOASTS) {
        const dropped = next.shift();
        if (dropped) {
          this.clearTimer(dropped.id);
        }
      }
      return next;
    });
    const ttl = options.ttlMs ?? DEFAULT_TOAST_TTL_MS;
    if (ttl > 0) {
      this.timers.set(
        id,
        setTimeout(() => this.dismiss(id), ttl),
      );
    }
    return id;
  }

  dismiss(id: number): void {
    this.clearTimer(id);
    this.list.update((current) =>
      current.some((t) => t.id === id) ? current.filter((t) => t.id !== id) : current,
    );
  }

  clear(): void {
    for (const id of [...this.timers.keys()]) {
      this.clearTimer(id);
    }
    this.list.set([]);
  }

  private clearTimer(id: number): void {
    const timer = this.timers.get(id);
    if (timer !== undefined) {
      clearTimeout(timer);
      this.timers.delete(id);
    }
  }
}
