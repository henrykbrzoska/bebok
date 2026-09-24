/**
 * General tab: app-level sections that are not per-provider
 * configuration — currently the Plugins section.
 *
 * Lists installable plugins from the engine's central registry
 * (`GET /plugins/registry`, engine-wide, cached server-side) merged with
 * the per-project declared state (`GET /plugins?directory=`): each row
 * shows Install (not installed) or Enable/Disable plus Update (installed).
 * Install clones the plugin repo at its latest tag and validates its
 * `bebok-plugin.json` manifest; Update re-resolves the release and downloads
 * the platform binary when it is missing (Plan B). Unknown names are rejected
 * by the engine.
 */

import { Component, OnDestroy, effect, inject, signal, computed } from '@angular/core';
import { Router } from '@angular/router';

import { DeclaredPlugin, IndexStatusResponse, RegistryPlugin } from '../../core/engine.dtos';
import { EngineClient } from '../../core/engine-client.service';
import { MessageKey } from '../../i18n';
import { I18nService } from '../../i18n/i18n.service';
import { ToastStore } from '../../ui/toast/toast.store';
import { SettingsStore } from './settings.store';
import { CodeFeaturesCard } from './code-features-card';

/** One plugin row: registry catalogue entry + per-project declared state. */
export interface PluginRow {
  name: string;
  repo: string;
  description: string;
  installed: boolean;
  enabled: boolean;
  /** Available/installed version (`''` when the engine reports none). */
  version: string;
  /** Release binary for this platform, when the engine reports it. */
  binary?: 'present' | 'missing';
}

@Component({
  selector: 'app-settings-general',
  imports: [CodeFeaturesCard],
  templateUrl: './general-tab.html',
  styleUrls: ['./settings-shared.css', './general-tab.css'],
})
export class GeneralTab implements OnDestroy {
  private readonly engine = inject(EngineClient);
  private readonly i18n = inject(I18nService);
  private readonly toasts = inject(ToastStore);
  private readonly router = inject(Router);

  readonly store = inject(SettingsStore);
  readonly t = this.i18n.t.bind(this.i18n);

  /** Registry catalogue (`GET /plugins/registry`). */
  readonly registryPlugins = signal<RegistryPlugin[]>([]);
  /** Declared plugins from `GET /plugins?directory=` (`declared` array). */
  readonly declaredPlugins = signal<DeclaredPlugin[]>([]);
  readonly pluginsLoading = signal(false);
  /** Last registry fetch failure (null = no error); shown under the card. */
  readonly pluginsError = signal<string | null>(null);
  readonly busyName = signal<string | null>(null);

  /** Code index snapshot (`GET /plugins/bebok-index/status?directory=`). */
  readonly indexStatus = signal<IndexStatusResponse | null>(null);
  readonly indexLoading = signal(false);
  readonly indexRebuilding = signal(false);
  /** Last index fetch/rebuild failure (null = no error); shown under the card. */
  readonly indexError = signal<string | null>(null);
  /** When the last rebuild was triggered in this session (null = never). */
  readonly indexLastRebuild = signal<Date | null>(null);
  private indexPoll: ReturnType<typeof setInterval> | null = null;
  /**
   * Consecutive quiet-poll failures (`refreshIndexStatus(true)`); after 3 the
   * card falls back to `unknown` instead of showing a stale `ready`.
   */
  private consecutiveFailures = 0;

  /** Registry entries merged with declared state (registry drives the list). */
  readonly rows = computed<PluginRow[]>(() => {
    const declared = this.declaredPlugins();
    return this.registryPlugins().map((reg) => {
      const decl = declared.find((p) => p.name === reg.name) ?? null;
      // Installed version (declared) wins over the registry's catalogue entry.
      const version = (decl?.version ?? reg.version ?? '').trim();
      return {
        name: reg.name,
        repo: reg.repo,
        description: reg.description,
        installed: decl?.installed ?? false,
        enabled: decl?.enabled ?? false,
        version,
        ...(decl?.binary ? { binary: decl.binary } : {}),
      };
    });
  });

  constructor() {
    // A project switch while Settings is open reloads the plugin list and
    // the index card for the new project (otherwise both keep showing the
    // previous project's data).
    effect(
      () => {
        const directory = this.store.directory();
        if (directory) {
          this.indexStatus.set(null);
          this.indexError.set(null);
          this.consecutiveFailures = 0;
          void this.refreshPlugins();
          void this.refreshIndexStatus();
        }
      },
      { allowSignalWrites: true },
    );
  }

  ngOnInit(): void {
    void this.refreshPlugins();
    void this.refreshIndexStatus();
    // Poll while the tab is open so `indexing` flips to `ready` without
    // manual refresh (same on-init + background pattern as the plugin list).
    this.indexPoll = setInterval(() => {
      void this.refreshIndexStatus(true);
    }, 5000);
  }

  ngOnDestroy(): void {
    if (this.indexPoll !== null) {
      clearInterval(this.indexPoll);
      this.indexPoll = null;
    }
  }

  navigateStats(): void {
    void this.router.navigate(['/stats']);
  }

  async refreshPlugins(): Promise<void> {
    this.pluginsLoading.set(true);
    this.pluginsError.set(null);
    try {
      // The tab may initialise while SettingsView is still connecting (child
      // ngOnInit runs before the parent's `await connect()` resumes), and
      // `pluginRegistry()` throws while unconnected. `connect()` is cached,
      // so this only waits when no connection exists yet.
      await this.engine.connect();
      const res = await this.engine.pluginRegistry();
      this.registryPlugins.set(normalizeRegistryPlugins(res));
    } catch (err) {
      this.pluginsError.set(describeError(err));
      console.error('plugin registry fetch failed', err);
      this.registryPlugins.set([]);
    }
    const dir = this.store.directory();
    if (!dir) {
      this.declaredPlugins.set([]);
      this.pluginsLoading.set(false);
      return;
    }
    try {
      const res = await this.engine.listPlugins(dir);
      this.declaredPlugins.set(normalizeDeclaredPlugins(res));
    } catch (err) {
      console.error('plugin list fetch failed', err);
      this.declaredPlugins.set([]);
    } finally {
      this.pluginsLoading.set(false);
    }
  }

  /** Localised label for the raw index status string from the engine. */
  indexStatusText(): string {
    const status = this.indexStatus()?.status;
    switch (status) {
      case 'ready':
        return this.t('settings.indexReady');
      case 'indexing':
        return this.t('settings.indexIndexing');
      case 'disabled':
        return this.t('settings.indexDisabled');
      case 'error':
        return this.t('settings.indexErrorState');
      case 'unknown':
        return this.t('settings.indexUnknown');
      case undefined:
        return this.t('settings.indexUnknown');
      default:
        return status;
    }
  }

  /** Status-dot tone matching the settings-shared.css palette. */
  indexStatusTone(): string {
    switch (this.indexStatus()?.status) {
      case 'ready':
        return 'ok';
      case 'indexing':
        return 'warn';
      case 'error':
        return 'err';
      default:
        return 'idle';
    }
  }

  indexLastRebuildText(): string {
    const when = this.indexLastRebuild();
    if (!when) {
      return this.t('settings.indexNeverRebuilt');
    }
    return this.t('settings.indexLastRebuild', { when: when.toLocaleString() });
  }

  async refreshIndexStatus(quiet = false): Promise<void> {
    const dir = this.store.directory();
    if (!dir) {
      if (!quiet) {
        this.indexStatus.set(null);
      }
      return;
    }
    if (!quiet) {
      this.indexLoading.set(true);
      this.indexError.set(null);
    }
    try {
      await this.engine.connect();
      const res = await this.engine.getIndexStatus(dir);
      this.indexStatus.set(normalizeIndexStatus(res));
      this.indexError.set(null);
      this.consecutiveFailures = 0;
    } catch (err) {
      if (quiet) {
        // Background poll: never surface a toast/spinner, but after 3
        // consecutive failures stop showing a potentially stale status.
        this.consecutiveFailures += 1;
        if (this.consecutiveFailures >= 3) {
          this.indexStatus.set({ status: 'unknown', files: 0, symbols: 0 });
        }
      } else {
        this.consecutiveFailures = 0;
        this.indexError.set(describeError(err));
        console.error('code index status fetch failed', err);
      }
    } finally {
      if (!quiet) {
        this.indexLoading.set(false);
      }
    }
  }

  async rebuildIndex(): Promise<void> {
    const dir = this.store.directory();
    if (!dir || this.indexRebuilding()) {
      return;
    }
    this.indexRebuilding.set(true);
    try {
      await this.engine.connect();
      const res = await this.engine.rebuildIndex(dir);
      this.indexStatus.set(normalizeIndexStatus(res));
      // The engine returns the plugin JSON verbatim: `ok: false` means the
      // rebuild did not start — report it instead of a success toast.
      if (res.ok === false) {
        const msg = `rebuild rejected: ${JSON.stringify(res)}`;
        this.indexError.set(msg);
        this.toasts.show(this.t('settings.indexError', { msg }), { kind: 'danger' });
        return;
      }
      this.indexLastRebuild.set(new Date());
      this.indexError.set(null);
      this.toasts.show(this.t('settings.indexRebuilt'), { kind: 'success' });
    } catch (err) {
      this.indexError.set(describeError(err));
      this.toasts.show(this.t('settings.indexError', { msg: describeError(err) }), {
        kind: 'danger',
      });
    } finally {
      this.indexRebuilding.set(false);
    }
  }

  async install(name: string): Promise<void> {
    if (this.busyName()) {
      return;
    }
    const dir = this.store.directory();
    if (!dir) {
      return;
    }
    this.busyName.set(name);
    try {
      await this.engine.installPlugin(dir, name);
      this.toasts.show(this.t('settings.pluginSaved', { name }), {
        kind: 'success',
      });
      await this.refreshPlugins();
      // bebok-index backs the code-index card: re-read its status too.
      if (name === 'bebok-index') {
        await this.refreshIndexStatus();
      }
    } catch (err) {
      this.toasts.show(this.t('settings.pluginError', { msg: describeError(err) }), {
        kind: 'danger',
      });
    } finally {
      this.busyName.set(null);
    }
  }

  /**
   * Update button label: localized "Update" (+ `v{version}` with the
   * registry/latest version as the target), swapped for "Updating…" while
   * this row is in flight. The version suffix shows only when the registry
   * advertises a version that differs from the installed (declared) one;
   * when both agree — or neither reports one — the label is bare "Update".
   */
  updateLabel(row: PluginRow): string {
    const key: MessageKey =
      this.busyName() === row.name ? 'settings.pluginUpdating' : 'settings.pluginUpdate';
    const base = this.t(key);
    const target = (this.registryPlugins().find((p) => p.name === row.name)?.version ?? '').trim();
    const installed = (this.declaredPlugins().find((p) => p.name === row.name)?.version ?? '').trim();
    if (target) {
      return target !== installed ? `${base} v${target}` : base;
    }
    return row.version ? `${base} v${row.version}` : base;
  }

  /** True when the engine reports the release binary as missing (Plan B). */
  isBinaryMissing(row: PluginRow): boolean {
    return row.binary === 'missing';
  }

  /**
   * Plan B: `POST /plugins/{name}/update` — re-resolve the release and fetch
   * the platform binary when it is absent, then refresh the list so the row
   * shows the new version/binary state.
   */
  async update(row: PluginRow): Promise<void> {
    if (this.busyName()) {
      return;
    }
    const dir = this.store.directory();
    if (!dir) {
      return;
    }
    this.busyName.set(row.name);
    try {
      await this.engine.updatePlugin(dir, row.name);
      this.toasts.show(this.t('settings.pluginUpdated', { name: row.name }), {
        kind: 'success',
      });
      await this.refreshPlugins();
      // bebok-index backs the code-index card: re-read its status too.
      if (row.name === 'bebok-index') {
        await this.refreshIndexStatus();
      }
    } catch (err) {
      const raw = describeError(err);
      const known = pluginUpdateErrorKey(raw);
      if (known) {
        // `offline_fallback` is a degraded success (bundled asset used), the
        // rest are hard failures — same message key, different severity.
        this.toasts.show(this.t(known), {
          kind: known === 'settings.pluginOfflineFallback' ? 'warning' : 'danger',
        });
      } else {
        this.toasts.show(this.t('settings.pluginError', { msg: raw }), { kind: 'danger' });
      }
    } finally {
      this.busyName.set(null);
    }
  }

  async toggle(row: PluginRow): Promise<void> {
    if (this.busyName()) {
      return;
    }
    const dir = this.store.directory();
    if (!dir) {
      return;
    }
    this.busyName.set(row.name);
    try {
      await this.engine.togglePlugin(dir, row.name, !row.enabled);
      this.toasts.show(this.t('settings.pluginSaved', { name: row.name }), {
        kind: 'success',
      });
      await this.refreshPlugins();
      // bebok-index backs the code-index card: re-read its status too.
      if (row.name === 'bebok-index') {
        await this.refreshIndexStatus();
      }
    } catch (err) {
      this.toasts.show(this.t('settings.pluginError', { msg: describeError(err) }), {
        kind: 'danger',
      });
    } finally {
      this.busyName.set(null);
    }
  }
}

/**
 * Accept the `{plugins: [...]}` registry shape; defensively tolerate a
 * bare array or a missing field (empty catalogue, no error). Entry fields
 * are coerced to strings (`repo`, `url`, `description` default to `''`)
 * so rows render even when the registry omits them.
 */
export function normalizeRegistryPlugins(raw: unknown): RegistryPlugin[] {
  const list = (raw && typeof raw === 'object' && Array.isArray((raw as Record<string, unknown>)['plugins'])
    ? (raw as Record<string, unknown[]>)['plugins']
    : Array.isArray(raw)
      ? raw
      : []) as unknown[];
  return list
    .filter((p): p is Record<string, unknown> =>
      !!p && typeof p === 'object' && typeof (p as { name?: unknown }).name === 'string',
    )
    .map((p) => ({
      name: p['name'] as string,
      repo: typeof p['repo'] === 'string' ? (p['repo'] as string) : '',
      url: typeof p['url'] === 'string' ? (p['url'] as string) : '',
      description: typeof p['description'] === 'string' ? (p['description'] as string) : '',
      version: typeof p['version'] === 'string' ? (p['version'] as string) : undefined,
    }));
}

/**
 * Accept the `{declared: [...]}` shape; defensively tolerate the
 * legacy `{plugins: string[]}` host-names shape (mapped to
 * `{name, enabled: false, installed: true}` stubs). `version` / `binary`
 * (Plan B) are optional and dropped when the engine does not send them.
 */
export function normalizeDeclaredPlugins(raw: unknown): DeclaredPlugin[] {
  if (raw && typeof raw === 'object') {
    const declared = (raw as Record<string, unknown>)['declared'];
    if (Array.isArray(declared)) {
      return declared
        .filter((p): p is Record<string, unknown> =>
          !!p && typeof p === 'object' && typeof (p as DeclaredPlugin).name === 'string',
        )
        .map((p) => ({
          name: p['name'] as string,
          repo: typeof p['repo'] === 'string' ? (p['repo'] as string) : '',
          url: typeof p['url'] === 'string' ? (p['url'] as string) : '',
          enabled: !!p['enabled'],
          installed: !!p['installed'],
          version: typeof p['version'] === 'string' ? (p['version'] as string) : undefined,
          binary: p['binary'] === 'missing' || p['binary'] === 'present' ? p['binary'] : undefined,
        }));
    }
  }
  return normalizePluginNames(raw).map((name) => ({
    name,
    repo: '',
    url: '',
    enabled: false,
    installed: true,
  }));
}

/** Accept both `string[]` and `{plugins: string[]}` shapes (legacy host names). */
export function normalizePluginNames(raw: unknown): string[] {
  const list = Array.isArray(raw)
    ? raw
    : raw && typeof raw === 'object' && Array.isArray((raw as Record<string, unknown>)['plugins'])
      ? (raw as Record<string, unknown[]>)['plugins']
      : [];
  return list.filter((p): p is string => typeof p === 'string');
}

/**
 * Accept the `{status, files, symbols}` index-status shape; defensively
 * coerce missing/non-numeric fields so the card renders even when the
 * engine omits them. The full-rebuild flag (`rebuild`, echoed by
 * `POST …/rebuild`) is passed through when the engine sends a boolean.
 */
export function normalizeIndexStatus(raw: unknown): IndexStatusResponse {
  const obj = raw && typeof raw === 'object' ? (raw as Record<string, unknown>) : {};
  return {
    status: typeof obj['status'] === 'string' ? (obj['status'] as string) : 'unknown',
    files: typeof obj['files'] === 'number' ? (obj['files'] as number) : 0,
    symbols: typeof obj['symbols'] === 'number' ? (obj['symbols'] as number) : 0,
    ...(typeof obj['rebuild'] === 'boolean' ? { rebuild: obj['rebuild'] as boolean } : {}),
  };
}

function describeError(err: unknown): string {
  return err instanceof Error ? err.message : String(err);
}

/** Failure patterns the update endpoint reports -> localised message keys. */
const UPDATE_ERROR_PATTERNS: readonly (readonly [string, MessageKey])[] = [
  ['binary_missing', 'settings.pluginBinaryMissing'],
  ['binary is missing', 'settings.pluginBinaryMissing'],
  // TODO: the engine never sends these machine codes (only `binary_missing`
  // arrives as `503` text with JSON inside); the keys stay for the readable
  // fallbacks below, which match the engine's free-form wording instead.
  ['no_asset_for_platform', 'settings.pluginNoAssetForPlatform'],
  ['no asset for', 'settings.pluginNoAssetForPlatform'],
  ['unsupported archive', 'settings.pluginNoAssetForPlatform'],
  // TODO: same as above — no `offline_fallback` code from the engine.
  ['offline_fallback', 'settings.pluginOfflineFallback'],
  ['offline fallback', 'settings.pluginOfflineFallback'],
  ['clone failed', 'settings.pluginOfflineFallback'],
  // TODO: same as above — no `checksum_mismatch` code from the engine.
  ['checksum_mismatch', 'settings.pluginChecksumMismatch'],
  ['checksum', 'settings.pluginChecksumMismatch'],
  ['sha256', 'settings.pluginChecksumMismatch'],
];

/**
 * Map an update failure message onto a localised message key. The engine
 * reports the failure either as a machine code inside the error body
 * (`{"error":"binary_missing",…}`, surfaced by the client as
 * `engine POST … -> 503: {"error":…}`) or as a readable sentence
 * (`clone failed`, `unsupported archive`, `sha256 …`), so both spellings
 * are recognised (case-insensitive).
 */
export function pluginUpdateErrorKey(message: string): MessageKey | null {
  const lower = message.toLowerCase();
  for (const [pattern, key] of UPDATE_ERROR_PATTERNS) {
    if (lower.includes(pattern)) {
      return key;
    }
  }
  return null;
}
