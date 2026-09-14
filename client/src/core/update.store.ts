/**
 * Update store: knows which Bebok is running and whether a newer release
 * exists.
 *
 * Two sources, picked by platform:
 *
 * - Desktop shell (Tauri, release build): the Rust `update_check` /
 *   `update_install` commands wrap `tauri-plugin-updater` against the GitHub
 *   Releases `latest.json`. Only signed bundles install; the sidecar is
 *   killed by the shell before the installer runs.
 * - Everything else (browser mode, Capacitor, `tauri dev`): the GitHub
 *   Releases API is asked for the latest tag and compared with the engine's
 *   version - detection only, the banner links to the release page.
 *
 * Automatic checks run shortly after start and every `CHECK_INTERVAL_MS`;
 * `check()` also backs the "Check for updates" button on the About screen.
 * Dismissing a version hides the banner for that version only (persisted);
 * the topbar version chip keeps showing the newer version.
 */

import { Injectable, computed, inject, signal } from '@angular/core';

import { EngineClient } from './engine-client.service';
import { SessionActivityStore } from './session-activity.store';

export type UpdatePhase = 'idle' | 'checking' | 'downloading' | 'installing' | 'ready' | 'error';

export interface AvailableUpdate {
  version: string;
  currentVersion: string;
  notes: string | null;
  date: string | null;
  /** True when the desktop shell can download and install it itself. */
  installable: boolean;
  /** Release page for the manual path (always set). */
  releaseUrl: string;
}

export interface UpdateProgress {
  downloaded: number;
  total: number | null;
}

interface DesktopInfo {
  version: string;
  debug: boolean;
  os: string;
  arch: string;
}

interface DesktopUpdateInfo {
  version: string;
  currentVersion: string;
  notes: string | null;
  date: string | null;
}

interface GitHubRelease {
  tag_name: string;
  html_url: string;
  body: string | null;
  published_at: string | null;
  draft: boolean;
  prerelease: boolean;
}

/** One row of the Updates screen's release list (GitHub Releases API). */
export interface ReleaseSummary {
  version: string;
  url: string;
  notes: string | null;
  date: string | null;
  prerelease: boolean;
  /** Same version as the running app. */
  current: boolean;
}

export const RELEASES_REPO = 'henrykbrzoska/bebok';
export const RELEASES_PAGE = `https://github.com/${RELEASES_REPO}/releases`;
const LATEST_RELEASE_API = `https://api.github.com/repos/${RELEASES_REPO}/releases/latest`;
const RELEASES_API = `https://api.github.com/repos/${RELEASES_REPO}/releases?per_page=15`;

const DISMISSED_KEY = 'bebok.update.dismissed';
/** Delay before the first automatic check, so it never competes with startup. */
export const INITIAL_CHECK_DELAY_MS = 10_000;
export const CHECK_INTERVAL_MS = 6 * 60 * 60 * 1000;

/**
 * Semver-ish comparison (`1.6.1`, `v1.6.1`, `1.7.0-rc.1`): negative when `a`
 * is older than `b`, zero when equal. A pre-release sorts before its release.
 */
export function compareVersions(a: string, b: string): number {
  const parse = (raw: string) => {
    const [core, pre] = raw.trim().replace(/^v/i, '').split('-', 2);
    const nums = core.split('.').map((part) => Number.parseInt(part, 10) || 0);
    while (nums.length < 3) {
      nums.push(0);
    }
    return { nums, pre: pre ?? null };
  };
  const left = parse(a);
  const right = parse(b);
  for (let index = 0; index < 3; index += 1) {
    if (left.nums[index] !== right.nums[index]) {
      return left.nums[index] - right.nums[index];
    }
  }
  if (left.pre === right.pre) {
    return 0;
  }
  if (left.pre === null) {
    return 1;
  }
  if (right.pre === null) {
    return -1;
  }
  return left.pre.localeCompare(right.pre);
}

@Injectable({ providedIn: 'root' })
export class UpdateStore {
  private readonly engine = inject(EngineClient);
  private readonly activity = inject(SessionActivityStore);

  /** Version of the desktop shell; null in browser mode. */
  readonly shellVersion = signal<string | null>(null);
  /** Version the engine reports (`GET /version`); null until connected. */
  readonly engineVersion = signal<string | null>(null);
  /** `tauri dev` / debug build: detection only, never install. */
  readonly debugBuild = signal(false);

  readonly available = signal<AvailableUpdate | null>(null);
  readonly phase = signal<UpdatePhase>('idle');
  readonly progress = signal<UpdateProgress | null>(null);
  readonly error = signal<string | null>(null);
  readonly lastCheckedAt = signal<number | null>(null);
  readonly dismissedVersion = signal<string | null>(readDismissed());

  /** Recent releases for the Updates screen; loaded on demand. */
  readonly releases = signal<ReleaseSummary[] | null>(null);
  readonly releasesLoading = signal(false);
  readonly releasesError = signal<string | null>(null);

  /** The version the user is on, whichever side reports it. */
  readonly currentVersion = computed(() => this.shellVersion() ?? this.engineVersion());

  /** Desktop shell and engine disagree - a half-applied update. */
  readonly versionMismatch = computed(() => {
    const shell = this.shellVersion();
    const engine = this.engineVersion();
    return shell !== null && engine !== null && shell !== engine;
  });

  /** Installing restarts the engine, which would cut running agent turns. */
  readonly installBlocked = computed(() => this.activity.runningSessions().size > 0);

  readonly bannerVisible = computed(() => {
    if (this.phase() === 'ready') {
      return true;
    }
    const update = this.available();
    return update !== null && update.version !== this.dismissedVersion();
  });

  /**
   * The release feed answered but carried no updater manifest - the current
   * *Latest* release on GitHub was not produced by the release workflow.
   */
  readonly feedMissing = computed(() => {
    const message = this.error();
    return message !== null && /valid release JSON|latest\.json/i.test(message);
  });

  readonly progressPercent = computed(() => {
    const progress = this.progress();
    if (!progress || !progress.total) {
      return null;
    }
    return Math.min(100, Math.round((progress.downloaded / progress.total) * 100));
  });

  private timer: ReturnType<typeof setTimeout> | null = null;
  private started = false;

  /** Schedule the automatic checks (main window only; idempotent). */
  start(): void {
    if (this.started) {
      return;
    }
    this.started = true;
    void this.loadVersions();
    this.schedule(INITIAL_CHECK_DELAY_MS);
  }

  stop(): void {
    if (this.timer !== null) {
      clearTimeout(this.timer);
      this.timer = null;
    }
    this.started = false;
  }

  /** Query the release feed now; `available` and `phase` reflect the answer. */
  async check(): Promise<void> {
    if (
      this.phase() === 'checking' ||
      this.phase() === 'downloading' ||
      this.phase() === 'installing'
    ) {
      return;
    }
    this.phase.set('checking');
    this.error.set(null);
    try {
      await this.loadVersions();
      const update = this.installerAvailable()
        ? await this.checkDesktop()
        : await this.checkGitHub();
      this.available.set(update);
      this.phase.set('idle');
    } catch (err) {
      this.error.set(errorMessage(err));
      this.phase.set('error');
    } finally {
      this.lastCheckedAt.set(Date.now());
    }
  }

  /** Download and install the pending update (desktop shell only). */
  async install(): Promise<void> {
    const update = this.available();
    if (!update?.installable || this.installBlocked() || this.phase() === 'downloading') {
      return;
    }
    this.phase.set('downloading');
    this.progress.set({ downloaded: 0, total: null });
    this.error.set(null);
    let unlisten: (() => void) | null = null;
    try {
      const [{ invoke }, { listen }] = await Promise.all([
        import('@tauri-apps/api/core'),
        import('@tauri-apps/api/event'),
      ]);
      unlisten = await listen<UpdateProgress>('update://progress', (event) => {
        this.progress.set(event.payload);
        if (event.payload.total !== null && event.payload.downloaded >= event.payload.total) {
          this.phase.set('installing');
        }
      });
      await invoke('update_install');
      // Windows never gets here (the installer exits the process).
      this.phase.set('ready');
    } catch (err) {
      this.error.set(errorMessage(err));
      this.phase.set('error');
      // The shell consumed the pending update; a fresh check re-arms it.
      this.available.set(null);
    } finally {
      unlisten?.();
      this.progress.set(null);
    }
  }

  /** Restart into the installed version (desktop shell only). */
  async relaunch(): Promise<void> {
    const { invoke } = await import('@tauri-apps/api/core');
    await invoke('relaunch_after_update');
  }

  /** Open the release page in the system browser. */
  async openReleasePage(): Promise<void> {
    const url = this.available()?.releaseUrl ?? RELEASES_PAGE;
    if (this.engine.isTauri()) {
      const { open } = await import('@tauri-apps/plugin-shell');
      await open(url);
      return;
    }
    window.open(url, '_blank', 'noopener');
  }

  /** Fetch the recent release list (published, non-draft) for the Updates screen. */
  async loadReleases(): Promise<void> {
    if (this.releasesLoading()) {
      return;
    }
    this.releasesLoading.set(true);
    this.releasesError.set(null);
    try {
      const res = await fetch(RELEASES_API, { headers: { Accept: 'application/vnd.github+json' } });
      if (!res.ok) {
        throw new Error(`GitHub releases API answered ${res.status}`);
      }
      const list = (await res.json()) as GitHubRelease[];
      const current = this.currentVersion();
      this.releases.set(
        list
          .filter((release) => !release.draft)
          .map((release) => {
            const version = release.tag_name.replace(/^v/i, '');
            return {
              version,
              url: release.html_url,
              notes: release.body,
              date: release.published_at,
              prerelease: release.prerelease,
              current: current !== null && compareVersions(version, current) === 0,
            };
          }),
      );
    } catch (err) {
      this.releasesError.set(errorMessage(err));
    } finally {
      this.releasesLoading.set(false);
    }
  }

  /** Hide the banner for the offered version; the version chip keeps its mark. */
  dismiss(): void {
    const version = this.available()?.version ?? null;
    this.dismissedVersion.set(version);
    try {
      if (version) {
        localStorage.setItem(DISMISSED_KEY, version);
      } else {
        localStorage.removeItem(DISMISSED_KEY);
      }
    } catch {
      /* localStorage unavailable - the dismissal lasts for this page only */
    }
  }

  private installerAvailable(): boolean {
    return this.engine.isTauri() && !this.debugBuild();
  }

  private schedule(delayMs: number): void {
    if (this.timer !== null) {
      clearTimeout(this.timer);
    }
    this.timer = setTimeout(() => {
      this.timer = null;
      void this.check().finally(() => {
        if (this.started) {
          this.schedule(CHECK_INTERVAL_MS);
        }
      });
    }, delayMs);
  }

  private async loadVersions(): Promise<void> {
    if (this.engine.isTauri() && this.shellVersion() === null) {
      try {
        const { invoke } = await import('@tauri-apps/api/core');
        const info = await invoke<DesktopInfo>('desktop_info');
        this.shellVersion.set(info.version);
        this.debugBuild.set(info.debug);
      } catch {
        /* older shell without the command - browser-style detection below */
      }
    }
    try {
      await this.engine.connect();
      const { version } = await this.engine.getVersion();
      this.engineVersion.set(version);
    } catch {
      /* engine unreachable or too old for /version - nothing to compare */
    }
  }

  private async checkDesktop(): Promise<AvailableUpdate | null> {
    const { invoke } = await import('@tauri-apps/api/core');
    const info = await invoke<DesktopUpdateInfo | null>('update_check');
    if (!info) {
      return null;
    }
    return {
      version: info.version,
      currentVersion: info.currentVersion,
      notes: info.notes,
      date: info.date,
      installable: true,
      releaseUrl: `${RELEASES_PAGE}/tag/${encodeURIComponent(info.version)}`,
    };
  }

  private async checkGitHub(): Promise<AvailableUpdate | null> {
    const current = this.currentVersion();
    if (!current) {
      throw new Error('current version unknown - connect to an engine first');
    }
    const res = await fetch(LATEST_RELEASE_API, {
      headers: { Accept: 'application/vnd.github+json' },
    });
    if (!res.ok) {
      throw new Error(`GitHub releases API answered ${res.status}`);
    }
    const release = (await res.json()) as GitHubRelease;
    if (release.draft || release.prerelease) {
      return null;
    }
    const latest = release.tag_name.replace(/^v/i, '');
    if (compareVersions(latest, current) <= 0) {
      return null;
    }
    return {
      version: latest,
      currentVersion: current,
      notes: release.body,
      date: release.published_at,
      installable: false,
      releaseUrl: release.html_url,
    };
  }
}

function readDismissed(): string | null {
  try {
    return localStorage.getItem(DISMISSED_KEY);
  } catch {
    return null;
  }
}

function errorMessage(err: unknown): string {
  return err instanceof Error ? err.message : String(err);
}
