/**
 * Settings state (WP-SETTINGS / F2-22).
 *
 * The redesigned Settings screen is a left rail + a detail pane whose content
 * is a separate component per tab. All of them need the same loaded config and
 * the same save helpers, so the state that used to live inside `SettingsView`
 * moved here. The store is provided by `SettingsView` (not `root`), so it is
 * created with the screen and thrown away with it.
 *
 * Nothing here is new data logic: every save is still a `PUT /config` delta on
 * the engine, exactly as before.
 */

import { Injectable, computed, inject, signal } from '@angular/core';

import { EngineClient } from '../../core/engine-client.service';
import { CustomCssService } from '../../core/custom-css.service';
import {
  ConfigResponse,
  DockerStatus,
  FleetMember,
  McpStatus,
  ResolvedSkill,
} from '../../core/engine.dtos';
import { I18nService } from '../../i18n/i18n.service';
import { ProviderDraft } from './provider-catalog';

/** The seven tabs of the redesigned Settings screen (design handoff §8). */
export type SettingsTab =
  | 'providers'
  | 'agents'
  | 'mcp'
  | 'skills'
  | 'permissions'
  | 'appearance'
  | 'rawJson';

export const SETTINGS_TABS: readonly SettingsTab[] = [
  'providers',
  'agents',
  'mcp',
  'skills',
  'permissions',
  'appearance',
  'rawJson',
];

/** One permission rule as stored in `config.permission.rules`. */
export interface PermissionRule {
  pattern: string;
  action: 'allow' | 'ask' | 'deny';
}

/** One MCP server entry as stored in `config.mcp.<name>`. */
export interface McpServerConfig {
  transport?: string;
  command?: string;
  args?: string[];
  url?: string;
  enabled?: boolean;
  [key: string]: unknown;
}

@Injectable()
export class SettingsStore {
  private readonly engine = inject(EngineClient);
  private readonly i18n = inject(I18nService);
  private readonly customCss = inject(CustomCssService);

  readonly directory = signal<string | null>(null);
  readonly loading = signal(false);
  readonly saving = signal(false);
  readonly error = signal<string | null>(null);
  readonly saved = signal<string | null>(null);

  readonly config = signal<ConfigResponse | null>(null);

  /** Active tab of the left rail. */
  readonly tab = signal<SettingsTab>('providers');

  /** Selected row inside a list+detail tab. */
  readonly selectedProvider = signal<string | null>(null);
  readonly selectedAgent = signal<string | null>(null);
  readonly selectedMcp = signal<string | null>(null);
  readonly selectedSkill = signal<string | null>(null);

  /** Native file pickers are only available in the Tauri desktop shell. */
  readonly isTauri = this.engine.isTauri;

  // --- providers -----------------------------------------------------------

  readonly providers = signal<ProviderDraft[]>([]);
  readonly checkedModels = signal<Record<string, string[]>>({});
  readonly checkingProvider = signal<string | null>(null);
  readonly providerModelsError = signal<string | null>(null);

  /**
   * Per-provider API-key drafts. The stored key is NEVER bound into the DOM:
   * the field starts empty and shows whether a key is configured, and only a
   * non-empty draft replaces the saved value on save.
   */
  readonly keyDrafts = signal<Record<string, string>>({});

  // --- agents --------------------------------------------------------------

  /** agent types with a per-type model override in `config.models`. */
  readonly agentTypes = ['code', 'ask', 'plan', 'debug', 'orchestrator'];
  readonly typeModels = signal<Record<string, string>>({});
  readonly systemPrompts = signal<Record<string, string>>({});

  /** Parallel-agents fleet: enabled flag + editable member list. */
  readonly fleetEnabled = signal(false);
  readonly fleetMembers = signal<FleetMember[]>([]);

  // --- permissions ---------------------------------------------------------

  readonly rules = signal<PermissionRule[]>([]);
  /** YOLO mode: auto-allow every tool call (dangerous). */
  readonly yolo = signal(false);

  // --- appearance ----------------------------------------------------------

  readonly pythonPath = signal('');
  readonly python3Path = signal('');
  readonly nodePath = signal('');
  readonly phpPath = signal('');
  readonly dockerPath = signal('');
  readonly gitPath = signal('');
  readonly customCssText = signal('');
  readonly customCssFilesText = signal('');
  readonly dockerStatus = signal<DockerStatus | null>(null);
  readonly checkingDocker = signal(false);

  // --- raw json ------------------------------------------------------------

  readonly rawTab = signal<'project' | 'global'>('project');
  readonly rawProjectText = signal('{}');
  readonly rawGlobalText = signal('{}');
  readonly rawProjectExists = signal(false);
  readonly rawGlobalExists = signal(false);
  readonly rawProjectPath = signal('');
  readonly rawGlobalPath = signal('');

  /** Flat list of selectable models (`provider/model`) from every provider. */
  readonly availableModels = computed<string[]>(() => {
    const out: string[] = [];
    for (const provider of this.providers()) {
      for (const model of provider.models ?? []) {
        out.push(`${provider.name}/${model}`);
      }
    }
    return out;
  });

  /** MCP server entries from the raw `config.mcp` object. */
  readonly mcpConfig = computed<Record<string, McpServerConfig>>(() => {
    const raw = this.config()?.config?.mcp;
    if (raw && typeof raw === 'object' && !Array.isArray(raw)) {
      return raw as Record<string, McpServerConfig>;
    }
    return {};
  });

  setTab(tab: SettingsTab): void {
    this.tab.set(tab);
    this.error.set(null);
    this.saved.set(null);
  }

  async load(directory: string | null): Promise<void> {
    this.directory.set(directory);
    await this.reload();
  }

  async reload(): Promise<void> {
    const dir = this.directory();
    if (!dir) {
      this.error.set(this.i18n.t('settings.noDirectory'));
      return;
    }
    this.loading.set(true);
    this.error.set(null);
    try {
      const cfg = await this.engine.getConfig(dir);
      this.applyConfig(cfg);
    } catch (err) {
      this.error.set(this.describe(err));
    } finally {
      this.loading.set(false);
    }
  }

  /** Fan a loaded `GET /config` response out into the per-tab signals. */
  applyConfig(cfg: ConfigResponse): void {
    this.config.set(cfg);
    // WP-SEC redacts `api_key` server-side (null + `has_key`); drop it here too
    // so an older engine's response can never put a key into the GUI state.
    // `PUT /config` restores an unchanged null placeholder by provider name.
    this.providers.set((cfg.providers ?? []).map((p) => ({ ...p, api_key: null })));
    this.keyDrafts.set({});
    this.rules.set(readRules(cfg.config.permission));
    this.typeModels.set({ ...(cfg.config.models ?? {}) });
    this.yolo.set(!!cfg.config.yolo);
    const fleet = cfg.config.fleet;
    this.fleetEnabled.set(!!fleet?.enabled);
    this.fleetMembers.set(
      Array.isArray(fleet?.members)
        ? fleet.members.map((m) => ({
            name: m.name ?? '',
            agent: m.agent ?? 'code',
            model: m.model ?? '',
          }))
        : [],
    );
    const ui = (cfg.config.ui ?? {}) as {
      customCss?: string;
      custom_css?: string;
      customCssFiles?: string[];
      custom_css_files?: string[];
    };
    this.customCssText.set(ui.customCss ?? ui.custom_css ?? '');
    const files = ui.customCssFiles ?? ui.custom_css_files ?? [];
    this.customCssFilesText.set(Array.isArray(files) ? files.join('\n') : '');
    this.pythonPath.set(cfg.runtimes.python);
    this.python3Path.set(cfg.runtimes.python3);
    this.nodePath.set(cfg.runtimes.node);
    this.phpPath.set(cfg.runtimes.php);
    this.dockerPath.set(cfg.runtimes.docker);
    this.gitPath.set(cfg.runtimes.git);
    this.rawProjectText.set(cfg.files?.project?.content ?? '{}');
    this.rawGlobalText.set(cfg.files?.global?.content ?? '{}');
    this.rawProjectExists.set(!!cfg.files?.project?.exists);
    this.rawGlobalExists.set(!!cfg.files?.global?.exists);
    this.rawProjectPath.set(cfg.files?.project?.path ?? '');
    this.rawGlobalPath.set(cfg.files?.global?.path ?? '');

    // Keep the list selections valid after a reload.
    if (!this.providers().some((p) => p.name === this.selectedProvider())) {
      this.selectedProvider.set(this.providers()[0]?.name ?? null);
    }
    const agents = cfg.agents ?? [];
    if (!agents.some((a) => a.name === this.selectedAgent())) {
      this.selectedAgent.set(agents[0]?.name ?? null);
    }
    const mcp = cfg.mcp ?? [];
    if (!mcp.some((s) => s.name === this.selectedMcp())) {
      this.selectedMcp.set(mcp[0]?.name ?? null);
    }
    const skills = cfg.skills ?? [];
    if (!skills.some((s) => s.name === this.selectedSkill())) {
      this.selectedSkill.set(skills[0]?.name ?? null);
    }
  }

  // --- provider actions ----------------------------------------------------

  /**
   * The providers list as it should be persisted: the saved key is kept unless
   * the user typed a new one into the (always empty) key field.
   */
  providersToPersist(): ProviderDraft[] {
    const drafts = this.keyDrafts();
    return this.providers().map((p) => {
      const draft = drafts[p.name];
      if (draft === undefined || draft.trim().length === 0) {
        return { ...p };
      }
      return { ...p, api_key: draft.trim() };
    });
  }

  async saveProviders(): Promise<void> {
    const dir = this.directory();
    if (!dir || this.saving()) {
      return;
    }
    this.saving.set(true);
    this.error.set(null);
    this.saved.set(null);
    try {
      await this.engine.putConfig(dir, { providers: this.providersToPersist() });
      this.saved.set(this.i18n.t('settings.savedProviders'));
      await this.reload();
    } catch (err) {
      this.error.set(this.describe(err));
    } finally {
      this.saving.set(false);
    }
  }

  updateProvider(name: string, patch: Partial<ProviderDraft>): void {
    this.providers.update((list) =>
      list.map((p) => (p.name === name ? { ...p, ...patch } : { ...p })),
    );
  }

  /** Set one `extra` field of a provider (catalog-declared dynamic field). */
  updateProviderExtra(name: string, key: string, value: string): void {
    this.providers.update((list) =>
      list.map((p) => {
        if (p.name !== name) {
          return { ...p };
        }
        const extra = { ...(p.extra ?? {}) };
        if (value.trim().length === 0) {
          delete extra[key];
        } else {
          extra[key] = value;
        }
        return { ...p, extra };
      }),
    );
  }

  setKeyDraft(name: string, value: string): void {
    this.keyDrafts.update((d) => ({ ...d, [name]: value }));
  }

  addProvider(spec: ProviderDraft): void {
    if (this.providers().some((p) => p.name === spec.name)) {
      return;
    }
    this.providers.update((list) => [...list, spec]);
    this.selectedProvider.set(spec.name);
  }

  removeProvider(name: string): void {
    this.providers.update((list) => list.filter((p) => p.name !== name));
    if (this.selectedProvider() === name) {
      this.selectedProvider.set(this.providers()[0]?.name ?? null);
    }
  }

  // --- agents actions ------------------------------------------------------

  setTypeModel(type: string, value: string): void {
    this.typeModels.update((m) => ({ ...m, [type]: value }));
  }

  typeModelFor(type: string): string {
    return this.typeModels()[type] ?? '';
  }

  async saveTypeModels(): Promise<void> {
    const dir = this.directory();
    if (!dir || this.saving()) {
      return;
    }
    this.saving.set(true);
    this.error.set(null);
    this.saved.set(null);
    try {
      const models: Record<string, string> = {};
      for (const type of this.agentTypes) {
        const v = this.typeModels()[type]?.trim();
        if (v) {
          models[type] = v;
        }
      }
      await this.engine.putConfig(dir, { models });
      this.saved.set(this.i18n.t('settings.savedModels'));
      await this.reload();
    } catch (err) {
      this.error.set(this.describe(err));
    } finally {
      this.saving.set(false);
    }
  }

  addFleetMember(): void {
    this.fleetMembers.update((list) => [...list, { name: '', agent: 'code', model: '' }]);
  }

  removeFleetMember(index: number): void {
    this.fleetMembers.update((list) => list.filter((_, i) => i !== index));
  }

  updateFleetMember(index: number, field: 'name' | 'agent' | 'model', value: string): void {
    this.fleetMembers.update((list) => {
      const next = list.map((m) => ({ ...m }));
      if (next[index]) {
        next[index] = { ...next[index], [field]: value };
      }
      return next;
    });
  }

  async toggleFleet(enabled: boolean): Promise<void> {
    this.fleetEnabled.set(enabled);
    await this.saveFleet();
  }

  async saveFleet(): Promise<void> {
    const dir = this.directory();
    if (!dir || this.saving()) {
      return;
    }
    this.saving.set(true);
    this.error.set(null);
    this.saved.set(null);
    try {
      const members = this.fleetMembers()
        .map((m) => ({
          name: m.name.trim(),
          agent: m.agent.trim() || 'code',
          model: m.model.trim(),
        }))
        .filter((m) => m.name.length > 0);
      await this.engine.putConfig(dir, {
        fleet: { enabled: this.fleetEnabled(), members },
      });
      this.saved.set(this.i18n.t('settings.fleetSaved'));
      await this.reload();
    } catch (err) {
      this.error.set(this.describe(err));
    } finally {
      this.saving.set(false);
    }
  }

  // --- mcp / skills --------------------------------------------------------

  async toggleMcp(server: McpStatus, enabled: boolean): Promise<void> {
    const dir = this.directory();
    if (!dir) {
      return;
    }
    this.error.set(null);
    this.saved.set(null);
    try {
      await this.engine.toggleMcp(dir, server.name, enabled);
      this.saved.set(
        this.i18n.t(enabled ? 'settings.mcpEnabled' : 'settings.mcpDisabled', {
          name: server.name,
        }),
      );
      await this.reload();
    } catch (err) {
      this.error.set(this.describe(err));
    }
  }

  /** Persist one MCP server's command/args back into `config.mcp.<name>`. */
  async saveMcpServer(name: string, patch: Partial<McpServerConfig>): Promise<void> {
    const dir = this.directory();
    if (!dir || this.saving()) {
      return;
    }
    this.saving.set(true);
    this.error.set(null);
    this.saved.set(null);
    try {
      const current = this.mcpConfig()[name] ?? {};
      await this.engine.putConfig(dir, { mcp: { [name]: { ...current, ...patch } } });
      this.saved.set(this.i18n.t('settings.mcpSaved', { name }));
      await this.reload();
    } catch (err) {
      this.error.set(this.describe(err));
    } finally {
      this.saving.set(false);
    }
  }

  async toggleSkill(skill: ResolvedSkill, enabled: boolean): Promise<void> {
    const dir = this.directory();
    if (!dir) {
      return;
    }
    this.error.set(null);
    this.saved.set(null);
    try {
      const skills: Record<string, boolean> = {};
      for (const s of this.config()?.skills ?? []) {
        skills[s.name] = s.enabled;
      }
      skills[skill.name] = enabled;
      await this.engine.putConfig(dir, { skills });
      this.saved.set(
        this.i18n.t(enabled ? 'settings.skillEnabled' : 'settings.skillDisabled', {
          name: skill.name,
        }),
      );
      await this.reload();
    } catch (err) {
      this.error.set(this.describe(err));
    }
  }

  // --- permissions ---------------------------------------------------------

  updateRule(index: number, patch: Partial<PermissionRule>): void {
    this.rules.update((list) => {
      const next = list.map((r) => ({ ...r }));
      if (next[index]) {
        next[index] = { ...next[index], ...patch };
      }
      return next;
    });
  }

  addRule(): void {
    this.rules.update((list) => [...list, { pattern: '', action: 'ask' }]);
  }

  removeRule(index: number): void {
    this.rules.update((list) => list.filter((_, i) => i !== index));
  }

  async saveRules(): Promise<void> {
    const dir = this.directory();
    if (!dir || this.saving()) {
      return;
    }
    this.saving.set(true);
    this.error.set(null);
    this.saved.set(null);
    try {
      const rules = this.rules()
        .map((r) => ({ pattern: r.pattern.trim(), action: r.action }))
        .filter((r) => r.pattern.length > 0);
      await this.engine.putConfig(dir, { permission: { rules } });
      this.saved.set(this.i18n.t('settings.savedRules'));
      await this.reload();
    } catch (err) {
      this.error.set(this.describe(err));
    } finally {
      this.saving.set(false);
    }
  }

  async toggleYolo(enabled: boolean): Promise<void> {
    const dir = this.directory();
    if (!dir || this.saving()) {
      return;
    }
    this.saving.set(true);
    this.error.set(null);
    this.saved.set(null);
    try {
      await this.engine.putConfig(dir, { yolo: enabled });
      this.yolo.set(enabled);
      this.saved.set(this.i18n.t('settings.yoloSaved'));
    } catch (err) {
      this.error.set(this.describe(err));
    } finally {
      this.saving.set(false);
    }
  }

  // --- appearance ----------------------------------------------------------

  async saveRuntimes(): Promise<void> {
    const dir = this.directory();
    if (!dir || this.saving()) {
      return;
    }
    this.saving.set(true);
    this.error.set(null);
    this.saved.set(null);
    try {
      await this.engine.putConfig(dir, {
        runtimes: {
          python: this.pythonPath().trim(),
          python3: this.python3Path().trim(),
          node: this.nodePath().trim(),
          php: this.phpPath().trim(),
          docker: this.dockerPath().trim(),
          git: this.gitPath().trim(),
        },
      });
      this.saved.set(this.i18n.t('settings.savedPaths'));
      this.dockerStatus.set(null);
      await this.reload();
    } catch (err) {
      this.error.set(this.describe(err));
    } finally {
      this.saving.set(false);
    }
  }

  async saveAppearance(): Promise<void> {
    const dir = this.directory();
    if (!dir || this.saving()) {
      return;
    }
    const files = this.customCssFilesText()
      .split('\n')
      .map((s) => s.trim())
      .filter((s) => s.length > 0);
    this.saving.set(true);
    this.error.set(null);
    this.saved.set(null);
    try {
      await this.engine.putConfig(dir, {
        ui: { customCss: this.customCssText(), customCssFiles: files },
      });
      this.saved.set(this.i18n.t('settings.savedAppearance'));
      await this.customCss.sync(dir);
      await this.reload();
    } catch (err) {
      this.error.set(this.describe(err));
    } finally {
      this.saving.set(false);
    }
  }

  async checkDocker(): Promise<void> {
    const dir = this.directory();
    if (!dir || this.checkingDocker()) {
      return;
    }
    this.checkingDocker.set(true);
    this.error.set(null);
    this.dockerStatus.set(null);
    try {
      this.dockerStatus.set(await this.engine.checkDocker(dir));
    } catch (err) {
      this.error.set(this.describe(err));
    } finally {
      this.checkingDocker.set(false);
    }
  }

  /** Browse for a runtime executable with the native file picker (Tauri only). */
  async browseRuntime(
    field: 'python' | 'python3' | 'node' | 'php' | 'docker' | 'git',
  ): Promise<void> {
    const picked = await this.engine.pickFile(field);
    if (!picked) {
      return;
    }
    const targets = {
      python: this.pythonPath,
      python3: this.python3Path,
      node: this.nodePath,
      php: this.phpPath,
      docker: this.dockerPath,
      git: this.gitPath,
    };
    targets[field].set(picked);
  }

  // --- raw json ------------------------------------------------------------

  rawText(): string {
    return this.rawTab() === 'project' ? this.rawProjectText() : this.rawGlobalText();
  }

  setRawText(value: string): void {
    if (this.rawTab() === 'project') {
      this.rawProjectText.set(value);
    } else {
      this.rawGlobalText.set(value);
    }
  }

  setRawTab(tab: 'project' | 'global'): void {
    this.rawTab.set(tab);
    this.error.set(null);
    this.saved.set(null);
  }

  /** Save the active layer: the whole editor text replaces the layer file. */
  async saveRaw(): Promise<void> {
    const dir = this.directory();
    if (!dir || this.saving()) {
      return;
    }
    let parsed: unknown;
    try {
      parsed = JSON.parse(this.rawText());
    } catch (err) {
      this.error.set(this.i18n.t('settings.invalidRawJson', { msg: this.describe(err) }));
      return;
    }
    if (!parsed || typeof parsed !== 'object' || Array.isArray(parsed)) {
      this.error.set(this.i18n.t('settings.rawMustBeObject'));
      return;
    }
    const scope = this.rawTab();
    this.saving.set(true);
    this.error.set(null);
    this.saved.set(null);
    try {
      await this.engine.putConfig(dir, parsed, { scope, replace: true });
      this.saved.set(this.i18n.t('settings.savedRaw', { scope }));
      await this.reload();
    } catch (err) {
      this.error.set(this.describe(err));
    } finally {
      this.saving.set(false);
    }
  }

  describe(err: unknown): string {
    return err instanceof Error ? err.message : String(err);
  }
}

/** Read `config.permission.rules` into a typed list (tolerant of junk). */
function readRules(permission: unknown): PermissionRule[] {
  if (!permission || typeof permission !== 'object') {
    return [];
  }
  const raw = (permission as Record<string, unknown>)['rules'];
  if (!Array.isArray(raw)) {
    return [];
  }
  const out: PermissionRule[] = [];
  for (const entry of raw) {
    if (!entry || typeof entry !== 'object') {
      continue;
    }
    const obj = entry as Record<string, unknown>;
    const pattern = typeof obj['pattern'] === 'string' ? obj['pattern'] : '';
    const action = obj['action'];
    out.push({
      pattern,
      action: action === 'allow' || action === 'deny' ? action : 'ask',
    });
  }
  return out;
}
