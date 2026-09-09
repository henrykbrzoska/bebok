/**
 * Config view: raw `config.json` editor (project + global layers).
 * Moved out of Settings into its own tab. The whole editor text replaces
 * the layer file (`PUT /config?scope=..&replace=true`).
 */

import { Component, OnInit, inject, signal } from '@angular/core';
import { FormsModule } from '@angular/forms';
import { ActivatedRoute, RouterLink } from '@angular/router';

import { EngineClient } from '../../core/engine-client.service';
import { EventsStore } from '../../core/events.store';
import { I18nService } from '../../i18n/i18n.service';

@Component({
  selector: 'app-config',
  imports: [FormsModule, RouterLink],
  templateUrl: './config.html',
  styleUrl: './config.css',
})
export class ConfigView implements OnInit {
  private readonly engine = inject(EngineClient);
  private readonly route = inject(ActivatedRoute);
  private readonly events = inject(EventsStore);
  private readonly i18n = inject(I18nService);

  readonly t = this.i18n.t.bind(this.i18n);

  readonly directory = signal<string | null>(null);
  readonly loading = signal(false);
  readonly saving = signal(false);
  readonly error = signal<string | null>(null);
  readonly saved = signal<string | null>(null);

  readonly rawTab = signal<'project' | 'global'>('project');
  readonly rawProjectText = signal('{}');
  readonly rawGlobalText = signal('{}');
  readonly rawProjectExists = signal(false);
  readonly rawGlobalExists = signal(false);
  readonly rawProjectPath = signal('');
  readonly rawGlobalPath = signal('');

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
      this.rawProjectText.set(cfg.files?.project?.content ?? '{}');
      this.rawGlobalText.set(cfg.files?.global?.content ?? '{}');
      this.rawProjectExists.set(!!cfg.files?.project?.exists);
      this.rawGlobalExists.set(!!cfg.files?.global?.exists);
      this.rawProjectPath.set(cfg.files?.project?.path ?? '');
      this.rawGlobalPath.set(cfg.files?.global?.path ?? '');
    } catch (err) {
      this.error.set(this.describe(err));
    } finally {
      this.loading.set(false);
    }
  }

  /** Active layer text. */
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

  /** Pretty-print the active layer (validates JSON lightly: comments not kept). */
  formatRaw(): void {
    const raw = this.rawText();
    try {
      const parsed: unknown = JSON.parse(raw);
      this.setRawText(JSON.stringify(parsed, null, 2) + '\n');
    } catch (err) {
      this.error.set(this.i18n.t('settings.invalidRawJson', { msg: this.describe(err) }));
    }
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
