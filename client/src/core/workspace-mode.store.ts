/**
 * Workspace mode (1.8): **code** or **chat**, the two icons in the topbar.
 *
 * Code mode is the app as it always was - a project directory, its files,
 * local changes, terminal. Chat mode is talking to a model without picking a
 * directory: sessions live in the engine-owned scratch directory
 * (`GET /workspace/chat`, `<engine data dir>/chat`) where the model may
 * still write files, but nothing project-like (explorer, terminal, changes,
 * git) is shown. The choice persists per client; the scratch directory is
 * resolved once per engine target and cached, and is never written to
 * `bebok.lastDirectory`, so switching back to code mode reopens the real
 * project.
 */

import { Injectable, computed, inject, signal } from '@angular/core';
import { Router } from '@angular/router';

import { ENGINE_API } from './engine-api';
import { EngineTargetStore } from './engine-target.store';
import { ProjectSessionsStore } from '../ui/shell/project-sessions.store';

export type WorkspaceMode = 'code' | 'chat';

const MODE_KEY = 'bebok.workspaceMode';
const CHAT_DIR_PREFIX = 'bebok.chatDir.';

function readMode(): WorkspaceMode {
  try {
    return localStorage.getItem(MODE_KEY) === 'chat' ? 'chat' : 'code';
  } catch {
    return 'code';
  }
}

@Injectable({ providedIn: 'root' })
export class WorkspaceModeStore {
  private readonly engine = inject(ENGINE_API);
  private readonly targets = inject(EngineTargetStore);
  private readonly project = inject(ProjectSessionsStore);
  private readonly router = inject(Router);

  readonly mode = signal<WorkspaceMode>(readMode());
  readonly isChat = computed(() => this.mode() === 'chat');
  /** The scratch directory of the active engine, once resolved. */
  readonly chatDirectory = signal<string | null>(this.readCachedDir());
  readonly error = signal<string | null>(null);

  /**
   * The directory a screen should work on right now: the chat scratch
   * directory in chat mode, else the remembered project. Screens that took
   * `readLastDirectory()` directly use this so Settings/Stats follow the mode.
   */
  currentDirectory(): string | null {
    if (this.isChat()) {
      return this.chatDirectory() ?? this.project.directory() ?? this.engine.readLastDirectory();
    }
    return this.engine.readLastDirectory();
  }

  /** `currentDirectory()`, resolving the chat scratch directory first if needed. */
  async ensureDirectory(): Promise<string | null> {
    if (this.isChat() && !this.chatDirectory()) {
      await this.resolveChatDirectory();
    }
    return this.currentDirectory();
  }

  /** True when `directory` is the chat scratch directory (sidebar/topbar labels). */
  isChatDirectory(directory: string | null | undefined): boolean {
    const chat = this.chatDirectory();
    return !!directory && !!chat && directory === chat;
  }

  /**
   * Resolve (and cache) the scratch directory; `null` when the engine is
   * unreachable or predates the route.
   */
  async resolveChatDirectory(): Promise<string | null> {
    try {
      const { directory } = await this.engine.chatWorkspace();
      this.chatDirectory.set(directory);
      this.writeCachedDir(directory);
      return directory;
    } catch (err) {
      this.error.set(err instanceof Error ? err.message : String(err));
      return this.chatDirectory();
    }
  }

  /**
   * Switch modes: chat selects the scratch directory in the shell (without
   * persisting it), code restores the last real project. Both land on the
   * Start screen, which renders the matching card.
   */
  async setMode(mode: WorkspaceMode): Promise<void> {
    if (mode === this.mode() && this.selectionMatches()) {
      return;
    }
    this.mode.set(mode);
    try {
      localStorage.setItem(MODE_KEY, mode);
    } catch {
      /* in-memory only */
    }
    await this.applySelection();
    await this.router.navigateByUrl('/');
  }

  /**
   * Make the shell's selected directory agree with the mode. Called after
   * the engine connects (Start screen) and on every mode switch.
   */
  async applySelection(): Promise<void> {
    if (this.mode() === 'chat') {
      const dir = await this.resolveChatDirectory();
      if (dir) {
        await this.project.select(dir, true, { persist: false });
      }
      return;
    }
    const last = this.engine.readLastDirectory();
    if (
      this.isChatDirectory(this.project.directory()) ||
      (last && last !== this.project.directory())
    ) {
      await this.project.select(last, true);
    }
  }

  private selectionMatches(): boolean {
    const selected = this.project.directory();
    return this.mode() === 'chat'
      ? this.isChatDirectory(selected)
      : !this.isChatDirectory(selected);
  }

  private cacheKey(): string {
    return CHAT_DIR_PREFIX + (this.targets.activeId() ?? 'default');
  }

  private readCachedDir(): string | null {
    try {
      return localStorage.getItem(this.cacheKey());
    } catch {
      return null;
    }
  }

  private writeCachedDir(dir: string): void {
    try {
      localStorage.setItem(this.cacheKey(), dir);
    } catch {
      /* ignore */
    }
  }
}
