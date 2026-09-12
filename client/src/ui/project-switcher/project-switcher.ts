import { ChangeDetectionStrategy, Component, computed, effect, inject, signal } from '@angular/core';

import { DirectoryPicker } from '../../core/directory-picker.service';
import { ProjectEntry } from '../../core/engine.dtos';
import { ProjectsStore } from '../../core/projects.store';
import { I18nService } from '../../i18n/i18n.service';
import { ProjectSessionsStore } from '../shell/project-sessions.store';
import { ShellStore } from '../shell/shell.store';

@Component({
  selector: 'app-project-switcher',
  changeDetection: ChangeDetectionStrategy.OnPush,
  templateUrl: './project-switcher.html',
  styleUrl: './project-switcher.css',
  host: { '(document:keydown)': 'onDocumentKeydown($event)' },
})
export class ProjectSwitcher {
  private readonly shell = inject(ShellStore);
  private readonly projects = inject(ProjectsStore);
  private readonly project = inject(ProjectSessionsStore);
  private readonly picker = inject(DirectoryPicker);
  private readonly i18n = inject(I18nService);

  readonly t = this.i18n.t.bind(this.i18n);
  readonly open = this.shell.projectSwitcherOpen;
  readonly entries = this.projects.projects;
  readonly activePath = this.project.directory;
  readonly busy = signal(false);
  readonly activeProject = computed(() => this.projects.findByPath(this.activePath()));

  constructor() {
    effect(() => {
      if (this.open()) {
        void this.projects.refresh();
      }
    });
  }

  onDocumentKeydown(event: KeyboardEvent): void {
    if (event.key === 'Escape' && this.open()) {
      event.preventDefault();
      this.close();
    }
  }

  close(): void {
    this.shell.closeProjectSwitcher();
  }

  async select(entry: ProjectEntry): Promise<void> {
    if (this.busy()) return;
    this.busy.set(true);
    try {
      const path = await this.projects.open(entry.id);
      if (path) {
        await this.project.select(path, true);
        this.close();
      }
    } finally {
      this.busy.set(false);
    }
  }

  async add(): Promise<void> {
    if (this.busy()) return;
    const path = await this.picker.pick(this.t('dialog.pickDirectory'));
    if (!path) return;
    this.busy.set(true);
    try {
      const added = await this.projects.add(path);
      if (added) {
        const selected = await this.projects.open(added.id);
        if (selected) {
          await this.project.select(selected, true);
          this.close();
        }
      }
    } finally {
      this.busy.set(false);
    }
  }

  async togglePinned(entry: ProjectEntry): Promise<void> {
    await this.projects.togglePinned(entry.id);
  }

  async remove(entry: ProjectEntry): Promise<void> {
    if (confirm(this.t('projectSwitcher.removeConfirm', { name: entry.name }))) {
      await this.projects.remove(entry.id);
    }
  }

  relativeLastOpened(entry: ProjectEntry): string {
    if (!entry.last_opened_at) return this.t('projectSwitcher.never');
    const seconds = Math.round((entry.last_opened_at - Date.now()) / 1000);
    const unit: Intl.RelativeTimeFormatUnit = Math.abs(seconds) < 60 ? 'second' : Math.abs(seconds) < 3600 ? 'minute' : Math.abs(seconds) < 86400 ? 'hour' : 'day';
    const divisor = unit === 'second' ? 1 : unit === 'minute' ? 60 : unit === 'hour' ? 3600 : 86400;
    return new Intl.RelativeTimeFormat(this.i18n.lang(), { numeric: 'auto' }).format(Math.round(seconds / divisor), unit);
  }
}
