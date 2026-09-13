/**
 * Quick-session scratch directory (WP-M5 / F10-18).
 *
 * `<engine home>/.bebok/quick`, resolved once per engine target through
 * `GET /fs/browse` (the engine's `Home` root - on Android the app's private
 * files dir, which the launcher sets as `HOME`) and cached in localStorage.
 * The engine creates the directory on first use (`get_or_create_instance`).
 * Falls back to the last used directory / first registered project when the
 * engine's `/fs/browse` is unavailable (e.g. a paired desktop in Remote
 * scope). Shared by chat-home (quick sessions) and settings-lite (the
 * directory Settings loads the config for on a fresh install).
 */

import type { EngineApi } from '../../../core/engine-api';
import type { ProjectEntry } from '../../../core/engine.dtos';

const QUICK_DIR_PREFIX = 'bebok.mobile.quickDir.';
/** Scratch directory under the engine's home for quick sessions. */
export const QUICK_DIR_SUFFIX = '.bebok/quick';

export function readCachedQuickDir(targetId: string): string | null {
  try {
    return localStorage.getItem(QUICK_DIR_PREFIX + targetId);
  } catch {
    return null;
  }
}

export function writeCachedQuickDir(targetId: string, dir: string): void {
  try {
    localStorage.setItem(QUICK_DIR_PREFIX + targetId, dir);
  } catch {
    /* ignore */
  }
}

/** `<home>/.bebok/quick` from the engine's `Home` root (forward slashes). */
export function quickDirFromHome(home: string): string {
  const base = home.replace(/[\\/]+$/, '');
  return `${base}/${QUICK_DIR_SUFFIX}`;
}

export async function resolveQuickDirectory(
  engine: Pick<EngineApi, 'browseDirectory' | 'readLastDirectory'>,
  targetId: string | null,
  projects: readonly ProjectEntry[] = [],
): Promise<string | null> {
  const key = targetId ?? 'default';
  const cached = readCachedQuickDir(key);
  if (cached) {
    return cached;
  }
  try {
    const roots = await engine.browseDirectory(null);
    const home = roots.entries.find((e) => e.name === 'Home')?.path;
    if (home) {
      const dir = quickDirFromHome(home);
      writeCachedQuickDir(key, dir);
      return dir;
    }
  } catch {
    /* fall through */
  }
  return engine.readLastDirectory() || projects[0]?.path || null;
}
