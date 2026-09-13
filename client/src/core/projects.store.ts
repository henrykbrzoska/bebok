/**
 * Projects registry store (F5-3).
 *
 * Mirrors the engine's `/projects` registry into signals. The engine remains
 * the source of truth - this only caches the last response and re-requests
 * after every mutation, so two clients never drift apart.
 *
 * Migration: before this package a user's only remembered project was the
 * single `bebok.lastDirectory` value in `localStorage`. On the first
 * `refresh()` that comes back with an *empty* registry we register that
 * directory once, so nothing is lost on upgrade. The empty-list check is the
 * idempotency guard: after the first successful add the list is non-empty, so
 * the migration can never run twice.
 */

import { Injectable, computed, inject, signal } from '@angular/core';

import { EngineClient } from './engine-client.service';
import { ProjectEntry, ProjectPatch } from './engine.dtos';

/** How many entries the Start screen's "recent" chips show. */
export const RECENT_LIMIT = 5;

/** One collapsible section of the project switcher (F6-7). */
export interface ProjectGroup {
  /** `null` is the always-last "Ungrouped" bucket. */
  name: string | null;
  projects: ProjectEntry[];
}

@Injectable({ providedIn: 'root' })
export class ProjectsStore {
  private readonly engine = inject(EngineClient);

  readonly projects = signal<ProjectEntry[]>([]);
  readonly loading = signal(false);
  readonly error = signal<string | null>(null);

  /** Most recently opened projects, newest first, capped at `RECENT_LIMIT`. */
  readonly recent = computed<ProjectEntry[]>(() =>
    this.projects()
      .filter((p) => p.last_opened_at !== null && p.last_opened_at !== undefined)
      .sort((a, b) => (b.last_opened_at ?? 0) - (a.last_opened_at ?? 0))
      .slice(0, RECENT_LIMIT),
  );

  /**
   * `projects()` grouped by `group`, preserving each project's position
   * within its bucket (the registry's own pinned -> recent -> name order).
   * Named groups sort alphabetically (case-insensitive); the `name: null`
   * "Ungrouped" bucket is always last, whatever its alphabetical position
   * would be, and is omitted entirely when every project is grouped.
   */
  readonly groups = computed<ProjectGroup[]>(() => {
    const buckets = new Map<string, ProjectEntry[]>();
    const ungrouped: ProjectEntry[] = [];
    for (const entry of this.projects()) {
      const name = entry.group?.trim();
      if (!name) {
        ungrouped.push(entry);
        continue;
      }
      const bucket = buckets.get(name);
      if (bucket) {
        bucket.push(entry);
      } else {
        buckets.set(name, [entry]);
      }
    }
    const named = [...buckets.keys()].sort((a, b) =>
      a.localeCompare(b, undefined, { sensitivity: 'base' }),
    );
    const result: ProjectGroup[] = named.map((name) => ({ name, projects: buckets.get(name)! }));
    if (ungrouped.length > 0) {
      result.push({ name: null, projects: ungrouped });
    }
    return result;
  });

  private migrated = false;

  /** Reload the registry (and run the one-time last-directory migration). */
  async refresh(): Promise<void> {
    if (!this.engine.connected()) {
      return;
    }
    this.loading.set(true);
    this.error.set(null);
    try {
      let projects = await this.engine.listProjects();
      if (projects.length === 0 && !this.migrated) {
        this.migrated = true;
        const last = this.engine.readLastDirectory();
        if (last) {
          try {
            await this.engine.addProject(last);
            projects = await this.engine.listProjects();
          } catch {
            /* the remembered directory may be gone - not fatal */
          }
        }
      }
      this.projects.set(projects);
    } catch (err) {
      this.error.set(describe(err));
    } finally {
      this.loading.set(false);
    }
  }

  /** Register a directory (idempotent on the engine side). */
  async add(path: string, name?: string): Promise<ProjectEntry | null> {
    try {
      const entry = await this.engine.addProject(path, name);
      await this.refresh();
      return entry;
    } catch (err) {
      this.error.set(describe(err));
      return null;
    }
  }

  async patch(id: string, patch: ProjectPatch): Promise<void> {
    try {
      await this.engine.updateProject(id, patch);
      await this.refresh();
    } catch (err) {
      this.error.set(describe(err));
    }
  }

  rename(id: string, name: string): Promise<void> {
    return this.patch(id, { name });
  }

  togglePinned(id: string): Promise<void> {
    const current = this.findById(id);
    return this.patch(id, { pinned: !current?.pinned });
  }

  /**
   * Move a project to `group` (or ungroup it when `group` is `null`/blank -
   * see the `ProjectPatch.group` wire contract in `engine.dtos.ts`). Typing a
   * brand-new name simply creates that group on the fly.
   */
  moveToGroup(id: string, group: string | null): Promise<void> {
    return this.patch(id, { group: group?.trim() ?? '' });
  }

  /** Forget a project. Never deletes anything on disk. */
  async remove(id: string): Promise<void> {
    try {
      await this.engine.removeProject(id);
      await this.refresh();
    } catch (err) {
      this.error.set(describe(err));
    }
  }

  /**
   * Mark a project opened and return the normalised path the caller should
   * switch the active directory to.
   */
  async open(id: string): Promise<string | null> {
    try {
      const entry = await this.engine.openProject(id);
      await this.refresh();
      return entry.path;
    } catch (err) {
      this.error.set(describe(err));
      return null;
    }
  }

  /** Distinct existing group names, alphabetical - offered as move-to-group suggestions. */
  distinctGroupNames(): string[] {
    return this.groups()
      .map((g) => g.name)
      .filter((name): name is string => name !== null);
  }

  findById(id: string): ProjectEntry | null {
    return this.projects().find((p) => p.id === id) ?? null;
  }

  /**
   * Match a directory against the registry. Paths from the engine are already
   * normalised; comparison is case-insensitive so a Windows path typed with a
   * different drive-letter case still matches.
   */
  findByPath(path: string | null): ProjectEntry | null {
    if (!path) {
      return null;
    }
    const needle = path.trim().toLowerCase();
    return this.projects().find((p) => p.path.toLowerCase() === needle) ?? null;
  }
}

function describe(err: unknown): string {
  return err instanceof Error ? err.message : String(err);
}
