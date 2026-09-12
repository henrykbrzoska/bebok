/**
 * MCP tab (WP-SETTINGS / F2-25).
 *
 * Stacked server rows (status dot + name + status text + inline Enabled
 * checkbox, dimmed when disabled) replacing the old flat toggle list, plus a
 * detail card for the selected server (Command, Args, "Test connection").
 *
 * Also covers part of known issue B11: an enabled server whose tools have not
 * been verified gets a trust note, worded like the Permissions tab's YOLO
 * warning - MCP tools run with the same reach as built-in tools.
 */

import { Component, computed, inject, signal } from '@angular/core';
import { FormsModule } from '@angular/forms';

import { McpStatus } from '../../core/engine.dtos';
import { I18nService } from '../../i18n/i18n.service';
import { McpServerConfig, SettingsStore } from './settings.store';

@Component({
  selector: 'app-settings-mcp',
  imports: [FormsModule],
  templateUrl: './mcp-tab.html',
  styleUrls: ['./settings-shared.css', './mcp-tab.css'],
})
export class McpTab {
  private readonly i18n = inject(I18nService);

  readonly store = inject(SettingsStore);
  readonly t = this.i18n.t.bind(this.i18n);

  /** Local edits of the selected server's command/args. */
  readonly commandDraft = signal<string | null>(null);
  readonly argsDraft = signal<string | null>(null);
  readonly testing = signal(false);

  readonly servers = computed<McpStatus[]>(() => this.store.config()?.mcp ?? []);

  readonly selected = computed<McpStatus | null>(
    () => this.servers().find((s) => s.name === this.store.selectedMcp()) ?? null,
  );

  readonly selectedConfig = computed<McpServerConfig>(() => {
    const name = this.store.selectedMcp();
    return (name ? this.store.mcpConfig()[name] : null) ?? {};
  });

  /** True when at least one enabled server is not connected/verified (B11). */
  readonly hasUnverified = computed(() =>
    this.servers().some((s) => s.enabled && !s.connected),
  );

  select(name: string): void {
    this.store.selectedMcp.set(name);
    this.commandDraft.set(null);
    this.argsDraft.set(null);
  }

  statusText(server: McpStatus): string {
    if (server.connected) {
      return this.t('settings.mcpConnected', { n: server.tool_count });
    }
    if (server.enabled && server.error) {
      return `${this.t('settings.errorPrefix')} ${server.error}`;
    }
    if (server.enabled) {
      return this.t('settings.connectingStatus');
    }
    return this.t('settings.disabled');
  }

  statusTone(server: McpStatus): string {
    if (server.connected) {
      return 'ok';
    }
    if (!server.enabled) {
      return 'idle';
    }
    return server.error ? 'err' : 'warn';
  }

  command(): string {
    return this.commandDraft() ?? (this.selectedConfig().command ?? '');
  }

  args(): string {
    const draft = this.argsDraft();
    if (draft !== null) {
      return draft;
    }
    const args = this.selectedConfig().args;
    return Array.isArray(args) ? args.join(' ') : '';
  }

  async save(): Promise<void> {
    const server = this.selected();
    if (!server) {
      return;
    }
    const args = this.args()
      .split(/\s+/)
      .filter((a) => a.length > 0);
    await this.store.saveMcpServer(server.name, { command: this.command().trim(), args });
    this.commandDraft.set(null);
    this.argsDraft.set(null);
  }

  /**
   * "Test connection": re-enable the server, which makes the engine drop and
   * rebuild the connection, then report the refreshed status.
   */
  async test(): Promise<void> {
    const server = this.selected();
    if (!server || this.testing()) {
      return;
    }
    this.testing.set(true);
    try {
      await this.store.toggleMcp(server, true);
    } finally {
      this.testing.set(false);
    }
  }
}
