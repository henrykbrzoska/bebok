/**
 * Tools tab: a 230px toggle list (name + pill switch) and a detail card with
 * the selected tool's effective description (editable), its read-only marker,
 * safety category and source.
 *
 * The list comes from `GET /tools/list` (see `SettingsStore.loadTools`), not
 * from `GET /config`: the engine's live tool registry depends on the project's
 * MCP servers, plugins and conditional features.
 */

import { Component, OnInit, computed, inject, signal } from '@angular/core';

import { ResolvedTool } from '../../core/engine.dtos';
import { I18nService } from '../../i18n/i18n.service';
import { SettingsStore } from './settings.store';

@Component({
  selector: 'app-settings-tools',
  templateUrl: './tools-tab.html',
  styleUrls: ['./settings-shared.css', './tools-tab.css'],
})
export class ToolsTab implements OnInit {
  private readonly i18n = inject(I18nService);

  readonly store = inject(SettingsStore);
  readonly t = this.i18n.t.bind(this.i18n);

  readonly tools = computed<ResolvedTool[]>(() => this.store.toolConfigs());

  readonly selected = computed<ResolvedTool | null>(
    () => this.tools().find((tool) => tool.name === this.store.selectedTool()) ?? null,
  );

  /** Local edit draft, tied to the tool it was typed for. */
  private readonly draft = signal<{ name: string; text: string } | null>(null);

  /** Textarea value: the draft if it belongs to the selected tool, else its saved description. */
  readonly description = computed<string>(() => {
    const tool = this.selected();
    if (!tool) {
      return '';
    }
    const draft = this.draft();
    return draft && draft.name === tool.name ? draft.text : (tool.description ?? '');
  });

  ngOnInit(): void {
    if (this.tools().length === 0) {
      void this.store.loadTools();
    }
  }

  select(name: string): void {
    this.draft.set(null);
    this.store.selectedTool.set(name);
  }

  toggle(tool: ResolvedTool, event: Event): void {
    event.stopPropagation();
    void this.store.toggleTool(tool.name, !tool.enabled);
  }

  setDescription(value: string): void {
    const tool = this.selected();
    if (tool) {
      this.draft.set({ name: tool.name, text: value });
    }
  }

  saveDescription(): void {
    const tool = this.selected();
    if (!tool) {
      return;
    }
    this.draft.set(null);
    void this.store.saveToolDescription(tool.name, this.description());
  }

  /** Dot class for a safety category (shared `.dot` styles). */
  safetyDot(tool: ResolvedTool): string {
    switch (tool.safety) {
      case 'safe':
        return 'ok';
      case 'caution':
        return 'warn';
      case 'dangerous':
        return 'err';
      default:
        return 'idle';
    }
  }
}
