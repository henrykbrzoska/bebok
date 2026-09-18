/**
 * Schedules screen: list and manage scheduled tasks.
 *
 * Features:
 * - List tasks (GET /schedules?directory=<current project>)
 * - Add new task (name, prompt textarea, agent select, schedule mode, enabled toggle)
 * - Actions: run-now, delete, toggle enabled
 * - Signal-based state with engine-client REST calls
 */

import { ChangeDetectionStrategy, Component, computed, effect, inject, signal } from '@angular/core';
import { CommonModule } from '@angular/common';
import { FormsModule } from '@angular/forms';

import { EngineClient } from '../../core/engine-client.service';
import { I18nService } from '../../i18n/i18n.service';
import { ProjectSessionsStore } from '../../ui/shell/project-sessions.store';
import { ScheduleTask } from '../../core/engine.dtos';

@Component({
  selector: 'app-schedules',
  changeDetection: ChangeDetectionStrategy.OnPush,
  imports: [CommonModule, FormsModule],
  templateUrl: './schedules.html',
  styleUrl: './schedules.css',
})
export class SchedulesView {
  private readonly engine = inject(EngineClient);
  private readonly i18n = inject(I18nService);
  private readonly project = inject(ProjectSessionsStore);

  readonly t = this.i18n.t.bind(this.i18n);

  readonly tasks = signal<ScheduleTask[]>([]);
  readonly tasksLoading = signal(false);
  readonly tasksError = signal<string | null>(null);

  readonly directory = this.project.directory;

  readonly agents = signal<string[]>(['code', 'ask', 'plan', 'debug']);

  readonly showAddForm = signal(false);

  // New task form
  readonly newName = signal('');
  readonly newPrompt = signal('');
  readonly newAgent = signal('code');
  readonly newMode = signal<'interval' | 'daily' | 'weekly'>('interval');
  readonly newIntervalMinutes = signal(60);
  readonly newDailyTime = signal('09:00');
  readonly newWeeklyDay = signal('mon');
  readonly newWeeklyTime = signal('09:00');
  readonly newEnabled = signal(true);

  readonly newTaskLoading = signal(false);

  readonly agentOptions = computed(() => this.agents());

  constructor() {
    // Reload tasks whenever directory changes
    effect(() => {
      const dir = this.directory();
      if (dir) {
        void this.loadTasks();
      }
    });
  }

  async loadTasks(): Promise<void> {
    const dir = this.directory();
    if (!dir) {
      this.tasks.set([]);
      this.tasksLoading.set(false);
      return;
    }

    this.tasksLoading.set(true);
    this.tasksError.set(null);

    try {
      await this.engine.connect();
      const res = await this.engine.listSchedules(dir);
      this.tasks.set(res);
    } catch (err) {
      const msg = err instanceof Error ? err.message : String(err);
      this.tasksError.set(msg);
      this.tasks.set([]);
    } finally {
      this.tasksLoading.set(false);
    }
  }

  toggleForm(): void {
    this.showAddForm.update(v => !v);
    if (!this.showAddForm()) {
      this.resetForm();
    }
  }

  resetForm(): void {
    this.newName.set('');
    this.newPrompt.set('');
    this.newAgent.set('code');
    this.newMode.set('interval');
    this.newIntervalMinutes.set(60);
    this.newDailyTime.set('09:00');
    this.newWeeklyDay.set('mon');
    this.newWeeklyTime.set('09:00');
    this.newEnabled.set(true);
  }

  async addTask(): Promise<void> {
    this.newTaskLoading.set(true);
    try {
      await this.engine.connect();
      const dir = this.project.directory();
      if (!dir) {
        this.tasksError.set('No project directory selected. Please select a project first.');
        return;
      }
      const payload: Partial<ScheduleTask> = {
        name: this.newName(),
        prompt: this.newPrompt(),
        agent: this.newAgent(),
        enabled: this.newEnabled(),
        directory: dir,
      };

      const mode = this.newMode();
      if (mode === 'interval') {
        payload.kind = 'interval_mins';
        payload.interval_mins = Number(this.newIntervalMinutes());
        payload.hour = null;
        payload.minute = null;
        payload.weekday = null;
      } else if (mode === 'daily') {
        payload.kind = 'daily';
        const [h, m] = this.newDailyTime().split(':').map(Number);
        payload.hour = h;
        payload.minute = m;
        payload.interval_mins = null;
        payload.weekday = null;
      } else {
        payload.kind = 'weekly';
        const dayMap: Record<string, number> = {
          mon: 1, tue: 2, wed: 3, thu: 4, fri: 5, sat: 6, sun: 7,
        };
        payload.weekday = dayMap[this.newWeeklyDay()];
        const [h, m] = this.newWeeklyTime().split(':').map(Number);
        payload.hour = h;
        payload.minute = m;
        payload.interval_mins = null;
      }

      await this.engine.createSchedule(payload);
      await this.loadTasks();
      this.toggleForm();
    } catch (err) {
      console.error('Failed to create schedule:', err);
      this.tasksError.set(err instanceof Error ? err.message : String(err));
    } finally {
      this.newTaskLoading.set(false);
    }
  }

  async toggleEnabled(task: ScheduleTask): Promise<void> {
    try {
      await this.engine.connect();
      await this.engine.patchSchedule(task.id, { enabled: !task.enabled });
      await this.loadTasks();
    } catch (err) {
      console.error('Failed to toggle schedule:', err);
    }
  }

  async deleteTask(task: ScheduleTask): Promise<void> {
    if (!confirm(`Delete schedule "${task.name}"?`)) {
      return;
    }

    try {
      await this.engine.connect();
      await this.engine.deleteSchedule(task.id);
      await this.loadTasks();
      this.resetForm();
      this.showAddForm.set(false);
    } catch (err) {
      console.error('Failed to delete schedule:', err);
    }
  }

  async runNow(task: ScheduleTask): Promise<void> {
    try {
      await this.engine.connect();
      await this.engine.runSchedule(task.id);
      await this.loadTasks();
    } catch (err) {
      console.error('Failed to run schedule:', err);
    }
  }
}
