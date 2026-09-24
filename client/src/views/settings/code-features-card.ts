/**
 * "Code intelligence" card, shown in Settings -> General.
 *
 * On/off toggles for the three code-analysis features the engine reads from
 * config: `code_map` (directory map injected into the system prompt),
 * `code_graph` (module dependency graph behind the code_graph_* tools) and
 * `ast_search` (structural search behind the code_ast tool). Each toggle is
 * a `PUT /config` delta `{ <section>: { enabled } }` — the engine merges
 * per-key, reloads the instance and registers/unregisters the tools, so the
 * change takes effect on the next turn.
 *
 * A separate component (not inlined into `general-tab.*`) so concurrent work
 * on the General tab merges without conflicts — same shape as
 * `frontend-verify.ts`.
 */

import { Component, effect, inject, signal } from '@angular/core';

import { EngineClient } from '../../core/engine-client.service';
import { I18nService } from '../../i18n/i18n.service';
import { ToastStore } from '../../ui/toast/toast.store';
import { SettingsStore } from './settings.store';

@Component({
  selector: 'app-code-features-card',
  imports: [],
  templateUrl: './code-features-card.html',
  styleUrls: ['./settings-shared.css', './code-features-card.css'],
})
export class CodeFeaturesCard {
  private readonly i18n = inject(I18nService);
  private readonly engine = inject(EngineClient);
  private readonly toasts = inject(ToastStore);

  readonly store = inject(SettingsStore);
  readonly t = this.i18n.t.bind(this.i18n);

  // --- code map -------------------------------------------------------------
  readonly codeMapEnabled = signal(false);
  readonly codeMapRegenerating = signal(false);
  readonly codeMapEntries = signal<number | null>(null);

  // --- code graph -----------------------------------------------------------
  readonly codeGraphEnabled = signal(false);
  readonly codeGraphRegenerating = signal(false);
  readonly codeGraphModules = signal<number | null>(null);
  readonly codeGraphEdges = signal<number | null>(null);

  // --- ast search -----------------------------------------------------------
  readonly astSearchEnabled = signal(false);

  /** True while any config save is in flight (disables the toggles). */
  readonly saving = signal(false);

  constructor() {
    // Mirror the resolved config into the toggle signals (after a load or a
    // save by this card or any sibling card on the tab).
    effect(() => {
      const cfg = this.store.config()?.config;
      if (!cfg) {
        return;
      }
      this.codeMapEnabled.set(!!cfg.code_map?.enabled);
      this.codeGraphEnabled.set(!!cfg.code_graph?.enabled);
      this.astSearchEnabled.set(!!cfg.ast_search?.enabled);
    });
  }

  // --- toggles ---------------------------------------------------------------

  async toggleCodeMap(enabled: boolean): Promise<void> {
    await this.saveEnabled('code_map', enabled);
    if (enabled) {
      await this.refreshCodeMapInfo();
    }
  }

  async toggleCodeGraph(enabled: boolean): Promise<void> {
    await this.saveEnabled('code_graph', enabled);
    if (enabled) {
      await this.refreshCodeGraphInfo();
    }
  }

  async toggleAstSearch(enabled: boolean): Promise<void> {
    await this.saveEnabled('ast_search', enabled);
  }

  // --- regenerate -------------------------------------------------------------

  async regenerateCodeMap(): Promise<void> {
    const dir = this.store.directory();
    if (!dir || this.codeMapRegenerating()) {
      return;
    }
    this.codeMapRegenerating.set(true);
    try {
      await this.engine.regenerateCodeMap(dir);
      await this.refreshCodeMapInfo();
      this.toasts.show(this.t('settings.codeMapRegenerated'), { kind: 'success' });
    } catch (err) {
      this.store.error.set(this.t('settings.codeFeaturesError', { msg: describe(err) }));
    } finally {
      this.codeMapRegenerating.set(false);
    }
  }

  async regenerateCodeGraph(): Promise<void> {
    const dir = this.store.directory();
    if (!dir || this.codeGraphRegenerating()) {
      return;
    }
    this.codeGraphRegenerating.set(true);
    try {
      await this.engine.regenerateCodeGraph(dir);
      await this.refreshCodeGraphInfo();
      this.toasts.show(this.t('settings.codeGraphRegenerated'), { kind: 'success' });
    } catch (err) {
      this.store.error.set(this.t('settings.codeFeaturesError', { msg: describe(err) }));
    } finally {
      this.codeGraphRegenerating.set(false);
    }
  }

  // --- internals --------------------------------------------------------------

  /** Persist one `{ <section>: { enabled } }` delta and adopt the response. */
  private async saveEnabled(
    section: 'code_map' | 'code_graph' | 'ast_search',
    enabled: boolean,
  ): Promise<void> {
    const dir = this.store.directory();
    if (!dir || this.saving()) {
      return;
    }
    this.saving.set(true);
    this.store.error.set(null);
    this.store.saved.set(null);
    try {
      const cfg = await this.engine.putConfig(dir, { [section]: { enabled } });
      this.store.applyConfig(cfg);
    } catch (err) {
      this.store.error.set(this.t('settings.codeFeaturesError', { msg: describe(err) }));
    } finally {
      this.saving.set(false);
    }
  }

  /** Best-effort cache info for the code map (entry count); 404 tolerated. */
  private async refreshCodeMapInfo(): Promise<void> {
    const dir = this.store.directory();
    if (!dir) {
      return;
    }
    try {
      const res = await this.engine.getCodeMap(dir);
      const map = res.map as { entries?: unknown } | null | undefined;
      const entries = Array.isArray(map?.entries) ? (map?.entries as unknown[]).length : null;
      this.codeMapEntries.set(entries);
    } catch {
      // No cache yet (or the feature was just enabled) — hide the count.
      this.codeMapEntries.set(null);
    }
  }

  /** Best-effort cache info for the code graph (module/edge counts). */
  private async refreshCodeGraphInfo(): Promise<void> {
    const dir = this.store.directory();
    if (!dir) {
      return;
    }
    try {
      const res = await this.engine.getCodeGraph(dir);
      const graph = res.graph as { modules?: unknown[]; edges?: unknown[] } | null | undefined;
      this.codeGraphModules.set(Array.isArray(graph?.modules) ? graph.modules.length : null);
      this.codeGraphEdges.set(Array.isArray(graph?.edges) ? graph.edges.length : null);
    } catch {
      this.codeGraphModules.set(null);
      this.codeGraphEdges.set(null);
    }
  }
}

function describe(err: unknown): string {
  return err instanceof Error ? err.message : String(err);
}
