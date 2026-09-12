/**
 * Skills tab (WP-SETTINGS / F2-26): a 230px toggle list (name + pill switch)
 * and a detail card with the selected skill's description and source path.
 */

import { Component, computed, inject } from '@angular/core';

import { ResolvedSkill } from '../../core/engine.dtos';
import { I18nService } from '../../i18n/i18n.service';
import { SettingsStore } from './settings.store';

@Component({
  selector: 'app-settings-skills',
  templateUrl: './skills-tab.html',
  styleUrls: ['./settings-shared.css', './skills-tab.css'],
})
export class SkillsTab {
  private readonly i18n = inject(I18nService);

  readonly store = inject(SettingsStore);
  readonly t = this.i18n.t.bind(this.i18n);

  readonly skills = computed<ResolvedSkill[]>(() => this.store.config()?.skills ?? []);

  readonly selected = computed<ResolvedSkill | null>(
    () => this.skills().find((s) => s.name === this.store.selectedSkill()) ?? null,
  );

  select(name: string): void {
    this.store.selectedSkill.set(name);
  }

  toggle(skill: ResolvedSkill, event: Event): void {
    event.stopPropagation();
    void this.store.toggleSkill(skill, !skill.enabled);
  }
}
