/**
 * Appearance tab (WP-SETTINGS / F2-28): runtime-path inputs in a 2-column
 * grid, a Comfortable/Compact density segmented control wired to WP-SHELL's
 * `ui-prefs.store.ts` density signal (read, never redefined), plus the custom
 * CSS editor and the Docker probe that already lived under "Others".
 */

import { Component, inject } from '@angular/core';
import { FormsModule } from '@angular/forms';

import { Density, UiPrefsStore } from '../../core/ui-prefs.store';
import { I18nService } from '../../i18n/i18n.service';
import { SettingsStore } from './settings.store';

type RuntimeField = 'python' | 'python3' | 'node' | 'php' | 'docker' | 'git';

@Component({
  selector: 'app-settings-appearance',
  imports: [FormsModule],
  templateUrl: './appearance-tab.html',
  styleUrls: ['./settings-shared.css', './appearance-tab.css'],
})
export class AppearanceTab {
  private readonly i18n = inject(I18nService);
  private readonly prefs = inject(UiPrefsStore);

  readonly store = inject(SettingsStore);
  readonly t = this.i18n.t.bind(this.i18n);

  readonly density = this.prefs.density;
  readonly densities: Density[] = ['comfortable', 'compact'];

  readonly runtimes: RuntimeField[] = ['python', 'python3', 'node', 'php', 'docker', 'git'];

  setDensity(density: Density): void {
    this.prefs.setDensity(density);
  }

  densityLabel(density: Density): string {
    return density === 'compact' ? this.t('settings.densityCompact') : this.t('settings.densityComfortable');
  }

  pathFor(field: RuntimeField): string {
    switch (field) {
      case 'python':
        return this.store.pythonPath();
      case 'python3':
        return this.store.python3Path();
      case 'node':
        return this.store.nodePath();
      case 'php':
        return this.store.phpPath();
      case 'docker':
        return this.store.dockerPath();
      case 'git':
        return this.store.gitPath();
    }
  }

  setPath(field: RuntimeField, value: string): void {
    switch (field) {
      case 'python':
        this.store.pythonPath.set(value);
        break;
      case 'python3':
        this.store.python3Path.set(value);
        break;
      case 'node':
        this.store.nodePath.set(value);
        break;
      case 'php':
        this.store.phpPath.set(value);
        break;
      case 'docker':
        this.store.dockerPath.set(value);
        break;
      case 'git':
        this.store.gitPath.set(value);
        break;
    }
  }
}
