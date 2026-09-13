/**
 * F9-2: agent-driven drawer reveals.
 *
 * Two things the *engine* does should surface a drawer panel without the user
 * hunting for it:
 *
 * - a sub-agent spawns (`task.started` on the open session) -> the Agents
 *   panel is turned on, expanded and scrolled into view;
 * - a `browser_*` tool call starts (a `message.part.updated` carrying a new
 *   running browser tool part) -> the Browser panel likewise.
 *
 * Both are deliberately gentle: they never re-open a drawer the user closed
 * (`openDrawer: false` - the panel state is armed for the next time the
 * drawer shows), they only react to the session currently open in chat, and
 * each task id / tool call id triggers at most once, so a long browser session
 * does not keep yanking the drawer back to the Browser panel on every call.
 *
 * Preview-from-a-link (F9-3) is a *user* action and goes through
 * `ExplorerSelectionStore.openInPreview` instead, which does open the drawer.
 */

import { DestroyRef, Injectable, inject } from '@angular/core';

import { EngineEvent, Message } from '../../core/engine.dtos';
import { EventsStore } from '../../core/events.store';
import { UiPrefsStore } from '../../core/ui-prefs.store';
import { ShellStore } from '../shell/shell.store';

/** Bounded memory of ids already revealed (oldest dropped first). */
const MAX_SEEN = 500;

@Injectable({ providedIn: 'root' })
export class DrawerAutoReveal {
  private readonly events = inject(EventsStore);
  private readonly shell = inject(ShellStore);
  private readonly prefs = inject(UiPrefsStore);
  private readonly destroyRef = inject(DestroyRef);

  private readonly seen = new Set<string>();
  private started = false;

  /** Idempotent: `AppShell` calls this once when it mounts. */
  start(): void {
    if (this.started) {
      return;
    }
    this.started = true;
    const unsubscribe = this.events.onEvent((ev) => this.handle(ev));
    this.destroyRef.onDestroy(unsubscribe);
  }

  /** Exposed for the spec; production traffic arrives via `EventsStore`. */
  handle(ev: EngineEvent): void {
    if (!this.shell.isChat() || ev.sessionID !== this.shell.currentSessionId()) {
      return;
    }
    if (ev.type === 'task.started') {
      const taskID = String(ev.properties?.['taskID'] ?? '');
      if (taskID && this.remember(`task:${taskID}`)) {
        this.prefs.revealRightDrawerPanel('agents', false);
      }
      return;
    }
    if (ev.type === 'message.part.updated') {
      const message = ev.properties?.['message'] as Message | undefined;
      if (!message || !Array.isArray(message.parts)) {
        return;
      }
      for (const part of message.parts) {
        if (part.type !== 'tool' || !part.name.startsWith('browser_')) {
          continue;
        }
        if (this.remember(`tool:${part.id}`)) {
          this.prefs.revealRightDrawerPanel('browser', false);
          return;
        }
      }
    }
  }

  private remember(key: string): boolean {
    if (this.seen.has(key)) {
      return false;
    }
    this.seen.add(key);
    if (this.seen.size > MAX_SEEN) {
      const oldest = this.seen.values().next().value;
      if (oldest !== undefined) {
        this.seen.delete(oldest);
      }
    }
    return true;
  }
}
