/**
 * Settings view (M4): model + API key, agents, permission rules, MCP toggles
 * and skill toggles. Every save maps to a config edit on the engine (PUT /config
 * or POST /mcp/{name}/toggle); the GUI holds no separate state.
 */

import { Component, OnInit, computed, inject, signal } from '@angular/core';
import { FormsModule } from '@angular/forms';
import { ActivatedRoute, RouterLink } from '@angular/router';

import { EngineClient } from '../../core/engine-client.service';
import {
  ConfigResponse,
  DockerStatus,
  McpStatus,
  ProviderSpec,
  ResolvedSkill,
} from '../../core/engine.dtos';
import { EventsStore } from '../../core/events.store';
import { I18nService } from '../../i18n/i18n.service';

@Component({
  selector: 'app-settings',
  imports: [FormsModule, RouterLink],
  templateUrl: './settings.html',
  styleUrl: './settings.css',
})
export class SettingsView implements OnInit {
  private readonly engine = inject(EngineClient);
  private readonly route = inject(ActivatedRoute);
  private readonly events = inject(EventsStore);
  private readonly i18n = inject(I18nService);

  readonly t = this.i18n.t.bind(this.i18n);

  /** Native file pickers are only available in the Tauri desktop shell. */
  readonly isTauri = this.engine.isTauri;

  readonly directory = signal<string | null>(null);
  readonly loading = signal(false);
  readonly saving = signal(false);
  readonly error = signal<string | null>(null);
  readonly saved = signal<string | null>(null);

  readonly config = signal<ConfigResponse | null>(null);

  readonly rulesText = signal('[]');


  /** M6: providers + per-agent-type models */
  readonly providers = signal<ProviderSpec[]>([]);
  readonly typeModels = signal<Record<string, string>>({});
  readonly checkedModels = signal<Record<string, string[]>>({});
  readonly checkingProvider = signal<string | null>(null);
  readonly providerModelsError = signal<string | null>(null);

  /** Draft form for adding a custom API (provider). */
  readonly customProvider = signal<{
    name: string;
    kind: 'openai' | 'anthropic';
    endpoint: string;
    api_key: string;
  }>({ name: '', kind: 'openai', endpoint: '', api_key: '' });

  /** agent types with their per-type model override */
  readonly agentTypes = ['code', 'ask', 'plan', 'debug', 'orchestrator'];

  /** YOLO mode: auto-allow every tool call (dangerous). */
  readonly yolo = signal(false);

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

  /** runtime executable paths */
  readonly pythonPath = signal('');
  readonly python3Path = signal('');
  readonly nodePath = signal('');
  readonly phpPath = signal('');
  readonly dockerPath = signal('');
  readonly gitPath = signal('');

  /** docker access probe */
  readonly dockerStatus = signal<DockerStatus | null>(null);
  readonly checkingDocker = signal(false);

  async ngOnInit(): Promise<void> {
    this.directory.set(
      this.route.snapshot.queryParamMap.get('directory') ?? this.engine.readLastDirectory(),
    );
    if (!this.engine.connected()) {
      try {
        await this.engine.connect();
      } catch (err) {
        this.error.set(this.describe(err));
        return;
      }
    }
    this.events.start();
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
      this.config.set(cfg);
      this.rulesText.set(this.rulesToText(cfg.config.permission));
      this.providers.set(cfg.providers ?? []);
      this.typeModels.set({ ...(cfg.config.models ?? {}) });
      this.yolo.set(!!cfg.config.yolo);
      this.pythonPath.set(cfg.runtimes.python);
      this.python3Path.set(cfg.runtimes.python3);
      this.nodePath.set(cfg.runtimes.node);
      this.phpPath.set(cfg.runtimes.php);
      this.dockerPath.set(cfg.runtimes.docker);
      this.gitPath.set(cfg.runtimes.git);
    } catch (err) {
      this.error.set(this.describe(err));
    } finally {
      this.loading.set(false);
    }
  }

  async saveRules(): Promise<void> {
    const dir = this.directory();
    if (!dir || this.saving()) {
      return;
    }
    let rules: unknown;
    try {
      rules = JSON.parse(this.rulesText());
    } catch (err) {
      this.error.set(this.i18n.t('settings.invalidRules', { msg: this.describe(err) }));
      return;
    }
    this.saving.set(true);
    this.error.set(null);
    this.saved.set(null);
    try {
      await this.engine.putConfig(dir, { permission: { rules } });
      this.saved.set(this.i18n.t('settings.savedRules'));
      await this.reload();
    } catch (err) {
      this.error.set(this.describe(err));
    } finally {
      this.saving.set(false);
    }
  }

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

  /** Browse for a runtime executable with the native file picker (Tauri only). */
  async browseRuntime(field: 'python' | 'python3' | 'node' | 'php' | 'docker' | 'git'): Promise<void> {
    const picked = await this.engine.pickFile(field);
    if (!picked) {
      return;
    }
    switch (field) {
      case 'python':
        this.pythonPath.set(picked);
        break;
      case 'python3':
        this.python3Path.set(picked);
        break;
      case 'node':
        this.nodePath.set(picked);
        break;
      case 'php':
        this.phpPath.set(picked);
        break;
      case 'docker':
        this.dockerPath.set(picked);
        break;
      case 'git':
        this.gitPath.set(picked);
        break;
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
      const status = await this.engine.checkDocker(dir);
      this.dockerStatus.set(status);
    } catch (err) {
      this.error.set(this.describe(err));
    } finally {
      this.checkingDocker.set(false);
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

  async checkModels(provider: ProviderSpec): Promise<void> {
    const dir = this.directory();
    if (!dir || this.checkingProvider()) {
      return;
    }
    this.checkingProvider.set(provider.name);
    this.error.set(null);
    this.providerModelsError.set(null);
    try {
      const res = await this.engine.listModels(dir, provider.name);
      this.checkedModels.update((m) => ({ ...m, [provider.name]: res.models }));
      // The engine persisted the models to the global config; reload so the
      // providers/models signals (and the per-agent model selects) refresh.
      await this.reload();
      this.saved.set(this.i18n.t('settings.modelsSaved', { name: provider.name }));
    } catch (err) {
      this.providerModelsError.set(this.describe(err));
    } finally {
      this.checkingProvider.set(null);
    }
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
      for (const t of this.agentTypes) {
        const v = this.typeModels()[t]?.trim();
        if (v) {
          models[t] = v;
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

  async saveProviders(): Promise<void> {
    const dir = this.directory();
    if (!dir || this.saving()) {
      return;
    }
    this.saving.set(true);
    this.error.set(null);
    this.saved.set(null);
    try {
      await this.engine.putConfig(dir, { providers: this.providers() });
      this.saved.set(this.i18n.t('settings.savedProviders'));
      await this.reload();
    } catch (err) {
      this.error.set(this.describe(err));
    } finally {
      this.saving.set(false);
    }
  }

  updateProviderField(index: number, field: 'endpoint' | 'api_key', value: string): void {
    this.providers.update((list) => {
      const next = list.map((p) => ({ ...p }));
      if (next[index]) {
        next[index] = { ...next[index], [field]: value };
      }
      return next;
    });
  }

  updateProviderKind(index: number, value: 'openai' | 'anthropic'): void {
    this.providers.update((list) => {
      const next = list.map((p) => ({ ...p }));
      if (next[index]) {
        next[index] = { ...next[index], kind: value };
      }
      return next;
    });
  }

  setCustomProvider(field: 'name' | 'kind' | 'endpoint' | 'api_key', value: string): void {
    this.customProvider.update((c) => ({ ...c, [field]: value }));
  }

  /** Add a custom API (provider) to the list; persist with "Save providers". */
  addCustomProvider(): void {
    const p = this.customProvider();
    const name = p.name.trim();
    if (!name) {
      return;
    }
    const exists = this.providers().some((x) => x.name === name);
    if (!exists) {
      this.providers.update((list) => [
        ...list,
        {
          name,
          kind: p.kind,
          endpoint: p.endpoint.trim() || null,
          api_key: p.api_key.trim() || null,
          models: [],
        },
      ]);
    }
    this.customProvider.set({ name: '', kind: 'openai', endpoint: '', api_key: '' });
  }

  setTypeModel(type: string, value: string): void {
    this.typeModels.update((m) => ({ ...m, [type]: value }));
  }

  /** Toggle YOLO mode (auto-allow every tool call). Persisted to config. */
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

  typeModelFor(type: string): string {
    return this.typeModels()[type] ?? '';
  }

  rulesToText(permission: unknown): string {
    if (permission && typeof permission === 'object') {
      const obj = permission as Record<string, unknown>;
      if (Array.isArray(obj['rules'])) {
        return JSON.stringify(obj['rules'], null, 2);
      }
    }
    return '[]';
  }

  describe(err: unknown): string {
    return err instanceof Error ? err.message : String(err);
  }
}
