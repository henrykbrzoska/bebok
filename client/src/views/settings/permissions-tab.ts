/**
 * Permissions tab (WP-SETTINGS / F2-27).
 *
 * The rule table shows the `tool(args)` patterns from `config.permission.rules`
 * in monospace with a colored allow/ask/deny verdict; the rules themselves come
 * from the permission engine's own config section (nothing is invented here,
 * the old raw-JSON textarea is only presented properly). Below it sits the
 * visually separate, danger-tinted "YOLO mode" card.
 *
 * WP-CHAT4 (F7-7): the "Tool safety" card lists every tool the engine knows
 * with its explicit safety category (the colour of the dots in the chat
 * transcript). Uncategorized tools come first - this card is the one place
 * where a new/unknown tool gets its category (the chat never prompts for
 * it) - followed by every tool grouped by category, each row with a
 * category select, its source (built-in / mcp:<server> / plugin) and a
 * "Reset to default" for overridden rows. Categories are informational:
 * they never change the allow / ask / deny rules above.
 */

import { Component, OnInit, computed, effect, inject } from '@angular/core';
import { FormsModule } from '@angular/forms';

import { EngineClient } from '../../core/engine-client.service';
import {
  BROWSER_DISPLAYS,
  BrowserDisplay,
  SAFETY_CATEGORIES,
  SafetyCategory,
  ToolSafetyEntry,
  isBrowserDisplay,
  isSafetyCategory,
} from '../../core/engine.dtos';
import { ToolSafetyStore } from '../../core/tool-safety.store';
import { I18nService } from '../../i18n/i18n.service';
import { PermissionRule, SettingsStore } from './settings.store';

/** One category bucket of the "all tools" table (F7-7). */
export interface SafetyGroup {
  category: SafetyCategory;
  tools: ToolSafetyEntry[];
}

/** Group `entries` by their effective category, in `SAFETY_CATEGORIES` order. */
export function groupBySafety(entries: readonly ToolSafetyEntry[]): SafetyGroup[] {
  return SAFETY_CATEGORIES.map((category) => ({
    category,
    tools: entries.filter((e) => e.category === category),
  })).filter((g) => g.tools.length > 0);
}

/** Default for `browser.display` when the config does not say (engine default). */
export const DEFAULT_BROWSER_DISPLAY: BrowserDisplay = 'headed';

/**
 * Where the headed browser window should appear: directly right of the app
 * window, on the same top edge (screen pixels). `null` when the app window's
 * geometry is unknown (browser mode) - Chrome then picks a spot.
 */
export function windowPositionRightOf(
  app: { x: number; y: number; width: number } | null,
  gap = 8,
): [number, number] | null {
  if (!app) {
    return null;
  }
  return [Math.round(app.x + app.width + gap), Math.round(app.y)];
}

@Component({
  selector: 'app-settings-permissions',
  imports: [FormsModule],
  templateUrl: './permissions-tab.html',
  styleUrls: ['./settings-shared.css', './permissions-tab.css'],
})
export class PermissionsTab implements OnInit {
  private readonly i18n = inject(I18nService);
  private readonly engine = inject(EngineClient);

  readonly store = inject(SettingsStore);
  readonly toolSafety = inject(ToolSafetyStore);
  readonly t = this.i18n.t.bind(this.i18n);

  readonly actions: Array<PermissionRule['action']> = ['allow', 'ask', 'deny'];

  // --- WP-CHAT4 (F7-7): "Tool safety" ---------------------------------------

  readonly safetyCategories = SAFETY_CATEGORIES;
  /** Uncategorized tools, listed first (the "confirmation" place). */
  readonly uncategorizedTools = computed(() => this.toolSafety.uncategorized());
  /** Every tool grouped by its effective category. */
  readonly safetyGroups = computed(() => groupBySafety(this.toolSafety.entries()));

  constructor() {
    // The tool list is directory-scoped like the config itself.
    effect(() => {
      void this.toolSafety.ensure(this.store.directory());
    });
  }

  ngOnInit(): void {
    void this.toolSafety.ensure(this.store.directory());
  }

  safetyLabel(category: SafetyCategory): string {
    switch (category) {
      case 'safe':
        return this.t('settings.toolSafetyGroupSafe');
      case 'caution':
        return this.t('settings.toolSafetyGroupCaution');
      case 'dangerous':
        return this.t('settings.toolSafetyGroupDangerous');
      default:
        return this.t('settings.toolSafetyGroupUncategorized');
    }
  }

  /** Display label of a `source` column value. */
  sourceLabel(source: string): string {
    if (source === 'built-in') {
      return this.t('settings.toolSafetySourceBuiltin');
    }
    if (source === 'plugin') {
      return this.t('settings.toolSafetySourcePlugin');
    }
    return source;
  }

  /** Persist a category change for one tool (global layer). */
  async setToolCategory(name: string, raw: string): Promise<void> {
    if (!isSafetyCategory(raw)) {
      return;
    }
    this.store.error.set(null);
    this.store.saved.set(null);
    await this.toolSafety.setCategory(name, raw);
    this.afterSafetyWrite();
  }

  /** Drop the override(s) for one tool so its default category shows again. */
  async resetToolCategory(name: string): Promise<void> {
    this.store.error.set(null);
    this.store.saved.set(null);
    await this.toolSafety.resetCategory(name);
    this.afterSafetyWrite();
  }

  private afterSafetyWrite(): void {
    const err = this.toolSafety.error();
    if (err) {
      this.store.error.set(err);
    } else {
      this.store.saved.set(this.t('settings.toolSafetySaved'));
    }
  }

  // --- WP-BROWSER2 (F7-6): "Browser display" ---------------------------------

  readonly browserDisplays = BROWSER_DISPLAYS;

  /** Effective `browser.display` of the loaded config (resolved view). */
  readonly browserDisplay = computed<BrowserDisplay>(() => {
    const raw = this.store.config()?.config.browser?.display;
    return isBrowserDisplay(raw) ? raw : DEFAULT_BROWSER_DISPLAY;
  });

  browserDisplayLabel(mode: BrowserDisplay): string {
    switch (mode) {
      case 'viewer':
        return this.t('settings.browserDisplayViewer');
      case 'drawer':
        return this.t('settings.browserDisplayDrawer');
      default:
        return this.t('settings.browserDisplayHeaded');
    }
  }

  /**
   * Persist `browser.display` to the *global* config (a user preference,
   * shared by every project). For `headed` in the desktop shell the current
   * app-window geometry is stored as `windowPosition` so the browser window
   * opens right of the app.
   */
  async setBrowserDisplay(raw: string): Promise<void> {
    const mode = isBrowserDisplay(raw) ? raw : DEFAULT_BROWSER_DISPLAY;
    const dir = this.store.directory();
    if (!dir || this.store.saving()) {
      return;
    }
    this.store.saving.set(true);
    this.store.error.set(null);
    this.store.saved.set(null);
    try {
      const browser: Record<string, unknown> = { display: mode };
      const position = mode === 'headed' ? await this.appWindowPosition() : null;
      if (position) {
        browser['windowPosition'] = position;
      }
      const cfg = await this.engine.putConfig(dir, { browser }, { scope: 'global' });
      this.store.applyConfig(cfg);
      this.store.saved.set(this.t('settings.browserDisplaySaved'));
    } catch (err) {
      this.store.error.set(this.store.describe(err));
    } finally {
      this.store.saving.set(false);
    }
  }

  /** Right-of-app position (Tauri only; `null` elsewhere or on failure). */
  private async appWindowPosition(): Promise<[number, number] | null> {
    if (!this.engine.isTauri()) {
      return null;
    }
    try {
      const { getCurrentWindow } = await import('@tauri-apps/api/window');
      const win = getCurrentWindow();
      const [pos, size] = await Promise.all([win.outerPosition(), win.outerSize()]);
      return windowPositionRightOf({ x: pos.x, y: pos.y, width: size.width });
    } catch {
      return null;
    }
  }

  actionLabel(action: PermissionRule['action']): string {
    switch (action) {
      case 'allow':
        return this.t('settings.ruleAllow');
      case 'deny':
        return this.t('settings.ruleDeny');
      default:
        return this.t('settings.ruleAsk');
    }
  }
}
