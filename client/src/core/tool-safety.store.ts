/**
 * Tool safety categories (WP-CHAT4 / F7-7).
 *
 * One directory-scoped cache of `GET /tools/safety`: every tool the engine
 * knows right now (built-ins, MCP servers' tools, plugin tools) with its
 * explicit category (`safe` / `caution` / `dangerous` / `uncategorized`).
 *
 * Two consumers:
 *  - the chat transcript, which colours the safety dots of *historical*
 *    tool parts (persisted before the engine stamped `safety`) by looking
 *    the tool name up here (`categoryOf`);
 *  - Settings > Permissions > "Tool safety", which lists, re-categorizes
 *    and resets tools (`setCategory` / `resetCategory`) and shows the
 *    uncategorized badge count.
 *
 * Categories are informational only: nothing here touches the permission
 * rules, and the engine never lets a category change an allow/ask/deny
 * verdict.
 */

import { Injectable, computed, inject, signal } from '@angular/core';

import { EngineClient } from './engine-client.service';
import { SafetyCategory, ToolSafetyEntry, ToolSafetyResponse } from './engine.dtos';

@Injectable({ providedIn: 'root' })
export class ToolSafetyStore {
  private readonly engine = inject(EngineClient);

  /** Directory the cached list belongs to (`null` before the first load). */
  readonly directory = signal<string | null>(null);
  readonly entries = signal<ToolSafetyEntry[]>([]);
  readonly loading = signal(false);
  readonly saving = signal(false);
  readonly error = signal<string | null>(null);
  /** Raw per-layer `tool_safety` maps (to tell a project override apart). */
  readonly layerOverrides = signal<{ global: Record<string, string>; project: Record<string, string> }>({
    global: {},
    project: {},
  });

  /** Name -> category for the fast lookup used by every tool dot. */
  private readonly byName = computed(() => {
    const map = new Map<string, SafetyCategory>();
    for (const entry of this.entries()) {
      map.set(entry.name, entry.category);
    }
    return map;
  });

  readonly uncategorized = computed(() =>
    this.entries().filter((e) => e.category === 'uncategorized'),
  );
  readonly uncategorizedCount = computed(() => this.uncategorized().length);

  /**
   * Current category of a tool by name, or `null` when the engine does not
   * list it (a tool removed since the call was recorded, or the list not
   * loaded yet). Reads a signal, so a `computed` using it re-evaluates once
   * the list arrives.
   */
  categoryOf(toolName: string): SafetyCategory | null {
    return this.byName().get(toolName) ?? null;
  }

  /** Where an override for `name` lives, if any. */
  overrideScope(name: string): 'project' | 'global' | null {
    const layers = this.layerOverrides();
    if (name in layers.project) {
      return 'project';
    }
    if (name in layers.global) {
      return 'global';
    }
    return null;
  }

  /** Load (or reload) the list for `directory`. No-op for an empty directory. */
  async load(directory: string | null): Promise<void> {
    if (!directory) {
      return;
    }
    this.directory.set(directory);
    this.loading.set(true);
    this.error.set(null);
    try {
      const res = await this.engine.getToolSafety(directory);
      if (this.directory() !== directory) {
        return; // a faster switch already moved on
      }
      this.apply(res);
    } catch (err) {
      this.error.set(describe(err));
    } finally {
      this.loading.set(false);
    }
  }

  /** Load only when the cache is for a different directory (cheap to call often). */
  async ensure(directory: string | null): Promise<void> {
    if (!directory || (this.directory() === directory && this.entries().length > 0)) {
      return;
    }
    await this.load(directory);
  }

  /** Re-fetch for the cached directory (after `config.changed`). */
  async resync(): Promise<void> {
    const dir = this.directory();
    if (dir) {
      await this.load(dir);
    }
  }

  /**
   * Persist one override (global layer by default). The engine returns the
   * refreshed list, so the dots and the Settings table update at once.
   */
  async setCategory(
    name: string,
    category: SafetyCategory,
    scope: 'global' | 'project' = 'global',
  ): Promise<void> {
    await this.write({ [name]: category }, scope);
  }

  /**
   * Remove the override for `name` from every layer that has one, so the
   * built-in/annotation default shows again.
   */
  async resetCategory(name: string): Promise<void> {
    const layers = this.layerOverrides();
    const patch = { [name]: null };
    if (name in layers.project) {
      await this.write(patch, 'project');
    }
    if (name in layers.global || !(name in layers.project)) {
      await this.write(patch, 'global');
    }
  }

  private async write(
    overrides: Record<string, SafetyCategory | null>,
    scope: 'global' | 'project',
  ): Promise<void> {
    const dir = this.directory();
    if (!dir || this.saving()) {
      return;
    }
    this.saving.set(true);
    this.error.set(null);
    try {
      const res = await this.engine.putToolSafety(dir, overrides, { scope });
      this.apply(res);
    } catch (err) {
      this.error.set(describe(err));
    } finally {
      this.saving.set(false);
    }
  }

  private apply(res: ToolSafetyResponse): void {
    this.entries.set(res.tools ?? []);
    this.layerOverrides.set({
      global: res.overrides?.global ?? {},
      project: res.overrides?.project ?? {},
    });
  }
}

function describe(err: unknown): string {
  return err instanceof Error ? err.message : String(err);
}
