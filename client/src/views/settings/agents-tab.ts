/**
 * Agents tab (WP-SETTINGS / F2-24).
 *
 * 220px list of presets (code/ask/plan/debug/orchestrator + file agents), each
 * with a status dot, plus "+ Add from file"; the detail card shows the agent's
 * description, a model-override select, an optional system-prompt textarea and
 * "Save agent". Selecting the orchestrator additionally reveals the "Parallel
 * fleet" section (enable checkbox, "+ Add member", "Save fleet").
 *
 * The data logic is the one that was already in `settings.ts`: the model
 * override is `config.models.<type>` and the fleet is `config.fleet`. The
 * system prompt is the project agent file `.bebok/agent/<name>.md`, which the
 * engine hot-reloads - written through the existing `/fs/file` endpoint.
 */

import { Component, computed, effect, inject, signal } from '@angular/core';
import { FormsModule } from '@angular/forms';

import { AgentInfo } from '../../core/engine.dtos';
import { EngineClient } from '../../core/engine-client.service';
import { I18nService } from '../../i18n/i18n.service';
import { SettingsStore } from './settings.store';

@Component({
  selector: 'app-settings-agents',
  imports: [FormsModule],
  templateUrl: './agents-tab.html',
  styleUrls: ['./settings-shared.css', './agents-tab.css'],
})
export class AgentsTab {
  private readonly i18n = inject(I18nService);
  private readonly engine = inject(EngineClient);

  readonly store = inject(SettingsStore);
  readonly t = this.i18n.t.bind(this.i18n);

  /** Raw text of `.bebok/agent/<name>.md` for the selected agent. */
  readonly promptText = signal('');
  readonly promptLoading = signal(false);

  readonly agents = computed<AgentInfo[]>(() => this.store.config()?.agents ?? []);

  readonly selected = computed<AgentInfo | null>(
    () => this.agents().find((a) => a.name === this.store.selectedAgent()) ?? null,
  );

  /** True when the selected preset is one of the built-in agent types. */
  readonly isType = computed(() => {
    const agent = this.selected();
    return !!agent && this.store.agentTypes.includes(agent.name);
  });

  constructor() {
    effect(() => {
      const agent = this.store.selectedAgent();
      if (agent) {
        void this.loadPrompt(agent);
      } else {
        this.promptText.set('');
      }
    });
  }

  select(name: string): void {
    this.store.selectedAgent.set(name);
  }

  /** Model override for the selected agent (`config.models.<name>`). */
  modelOverride(): string {
    const agent = this.selected();
    return agent ? this.store.typeModelFor(agent.name) : '';
  }

  setModelOverride(value: string): void {
    const agent = this.selected();
    if (agent) {
      this.store.setTypeModel(agent.name, value);
    }
  }

  agentFilePath(name: string): string {
    return `.bebok/agent/${name}.md`;
  }

  /** Save the model override and, when filled in, the project agent file. */
  async saveAgent(): Promise<void> {
    const agent = this.selected();
    const dir = this.store.directory();
    if (!agent || !dir) {
      return;
    }
    const text = this.promptText().trim();
    if (text.length > 0) {
      try {
        await this.engine.fsFileWrite(dir, this.agentFilePath(agent.name), `${text}\n`);
      } catch (err) {
        this.store.error.set(this.store.describe(err));
        return;
      }
    }
    await this.store.saveTypeModels();
  }

  /** "+ Add from file": seed a new project agent file in the editor. */
  addFromFile(): void {
    this.promptText.set(
      ['---', 'name: my-agent', 'description: ', 'model: ', '---', '', ''].join('\n'),
    );
  }

  private async loadPrompt(name: string): Promise<void> {
    const dir = this.store.directory();
    if (!dir) {
      return;
    }
    this.promptLoading.set(true);
    try {
      const res = await this.engine.fsFile(dir, this.agentFilePath(name));
      this.promptText.set(res.content);
    } catch {
      // No project override for this agent - the textarea stays empty and
      // saving it creates the file.
      this.promptText.set('');
    } finally {
      this.promptLoading.set(false);
    }
  }
}
