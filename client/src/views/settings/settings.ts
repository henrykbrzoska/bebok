/**
 * Settings screen (WP-SETTINGS / F2-22): one full-screen list+detail view.
 *
 * A 210px left rail lists the seven tabs of the design handoff (§8) -
 * Providers, Agents, MCP, Skills, Permissions, Appearance, Raw JSON - and the
 * detail pane swaps one component per tab. The old standalone Config page is
 * folded in as the Raw JSON tab; `/config` redirects here (WP-SHELL / F1-11).
 *
 * This component owns nothing but the shell: loading, the banners and the tab
 * selection. Everything else lives in `SettingsStore`, which it provides.
 */

import { Component, OnInit, effect, inject } from '@angular/core';
import { ActivatedRoute } from '@angular/router';
import { toSignal } from '@angular/core/rxjs-interop';
import { map } from 'rxjs';

import { EngineClient } from '../../core/engine-client.service';
import { EventsStore } from '../../core/events.store';
import { I18nService } from '../../i18n/i18n.service';
import { MessageKey } from '../../i18n';
import { ProviderCatalog } from './provider-catalog';
import { SETTINGS_TABS, SettingsStore, SettingsTab } from './settings.store';
import { ProvidersTab } from './providers-tab';
import { AgentsTab } from './agents-tab';
import { McpTab } from './mcp-tab';
import { SkillsTab } from './skills-tab';
import { PermissionsTab } from './permissions-tab';
import { AppearanceTab } from './appearance-tab';
import { RawJsonTab } from './raw-json-tab';

/** Query-param aliases accepted for `?tab=` (the command palette uses these). */
const TAB_ALIASES: Record<string, SettingsTab> = {
  providers: 'providers',
  agents: 'agents',
  mcp: 'mcp',
  skills: 'skills',
  permissions: 'permissions',
  appearance: 'appearance',
  raw: 'rawJson',
  rawjson: 'rawJson',
  json: 'rawJson',
  config: 'rawJson',
};

@Component({
  selector: 'app-settings',
  imports: [
    ProvidersTab,
    AgentsTab,
    McpTab,
    SkillsTab,
    PermissionsTab,
    AppearanceTab,
    RawJsonTab,
  ],
  providers: [SettingsStore],
  templateUrl: './settings.html',
  styleUrl: './settings.css',
})
export class SettingsView implements OnInit {
  private readonly engine = inject(EngineClient);
  private readonly route = inject(ActivatedRoute);
  private readonly events = inject(EventsStore);
  private readonly i18n = inject(I18nService);
  private readonly catalog = inject(ProviderCatalog);

  readonly store = inject(SettingsStore);
  readonly t = this.i18n.t.bind(this.i18n);
  readonly tabs = SETTINGS_TABS;

  /** Tab requested through `?tab=` (command palette deep links). */
  private readonly requestedTab = toSignal(
    this.route.queryParamMap.pipe(map((p) => p.get('tab'))),
    { initialValue: this.route.snapshot.queryParamMap.get('tab') },
  );

  constructor() {
    // A `?tab=` deep link (command palette) selects the rail entry, also when
    // the user is already on this screen.
    effect(() => this.applyRequestedTab());
  }

  async ngOnInit(): Promise<void> {
    const directory =
      this.route.snapshot.queryParamMap.get('directory') ?? this.engine.readLastDirectory();
    if (!this.engine.connected()) {
      try {
        await this.engine.connect();
      } catch (err) {
        this.store.error.set(this.store.describe(err));
        return;
      }
    }
    this.events.start();
    void this.catalog.load();
    await this.store.load(directory);
  }

  /** Label key of a tab in the left rail. */
  tabLabel(tab: SettingsTab): MessageKey {
    return `settings.tab.${tab}` as MessageKey;
  }

  select(tab: SettingsTab): void {
    this.store.setTab(tab);
  }

  private applyRequestedTab(): void {
    const raw = (this.requestedTab() ?? '').toLowerCase();
    const tab = TAB_ALIASES[raw];
    if (tab) {
      this.store.tab.set(tab);
    }
  }
}
