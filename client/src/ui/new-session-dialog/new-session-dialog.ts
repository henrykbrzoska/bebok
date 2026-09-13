/**
 * "New session" dialog (WP-GIT / F6-16): the first creation UI in the app.
 *
 * Same overlay family as `command-palette` / `project-switcher`. Offers the
 * agent preset, the model (finally surfacing the `model` parameter
 * `POST /session` always accepted) and "Run in a git worktree", which reveals
 * a branch-name input (default `bebok/session-<6 random base36 chars>` - there
 * is no prompt text at creation time to derive a slug from) and a base-branch
 * input prefilled with the project's current branch from
 * `GET /projects/{id}/git`.
 *
 * The worktree option needs the directory to be a *registered* project (the
 * git endpoints are id-addressed) and a git repository; otherwise the
 * checkbox is disabled with an explanatory hint. Models come from
 * `GET /config` the same way the chat header's switcher builds its list
 * (providers with a resolvable key only).
 */

import { ChangeDetectionStrategy, Component, computed, effect, inject, signal, untracked } from '@angular/core';
import { FormsModule } from '@angular/forms';
import { Router } from '@angular/router';

import { AgentInfo, ProjectGitInfo, WorktreeSpec } from '../../core/engine.dtos';
import { EngineClient } from '../../core/engine-client.service';
import { ProjectsStore } from '../../core/projects.store';
import { I18nService } from '../../i18n/i18n.service';
import { ProjectSessionsStore } from '../shell/project-sessions.store';
import { NewSessionDialogStore } from './new-session-dialog.store';

/** Mirrors `bebok_core::git::validate_branch` closely enough to catch typos before the round trip. */
export const BRANCH_PATTERN = /^(?![-./])(?!.*(\.\.|\/\/|@\{))[A-Za-z0-9._/-]+(?<![./])(?<!\.lock)$/;

export function isValidBranch(branch: string): boolean {
  if (!BRANCH_PATTERN.test(branch)) {
    return false;
  }
  return branch.split('/').every((segment) => segment.length > 0 && !segment.startsWith('.'));
}

/** `bebok/session-<6 base36 chars>` - unique enough per project, readable in `git branch`. */
export function defaultBranchName(): string {
  const id = Math.random().toString(36).slice(2, 8).padEnd(6, '0');
  return `bebok/session-${id}`;
}

@Component({
  selector: 'app-new-session-dialog',
  changeDetection: ChangeDetectionStrategy.OnPush,
  imports: [FormsModule],
  templateUrl: './new-session-dialog.html',
  styleUrl: './new-session-dialog.css',
  host: { '(document:keydown)': 'onDocumentKeydown($event)' },
})
export class NewSessionDialog {
  private readonly store = inject(NewSessionDialogStore);
  private readonly engine = inject(EngineClient);
  private readonly projects = inject(ProjectsStore);
  private readonly project = inject(ProjectSessionsStore);
  private readonly router = inject(Router);
  private readonly i18n = inject(I18nService);

  readonly t = this.i18n.t.bind(this.i18n);
  readonly request = this.store.request;
  readonly open = computed(() => this.request() !== null);
  readonly directory = computed(() => this.request()?.directory ?? null);

  /** Agent presets of the selected project (shared store, already loaded by the sidebar). */
  readonly agents = this.project.agents;
  readonly selectedAgent = signal('code');
  /** `provider/model` strings; `''` means "agent default". */
  readonly models = signal<string[]>([]);
  readonly selectedModel = signal('');

  readonly useWorktree = signal(false);
  readonly branch = signal('');
  readonly base = signal('');

  /** Registry entry for the directory (null when the directory was typed, not registered). */
  readonly projectEntry = computed(() => this.projects.findByPath(this.directory()));
  readonly git = signal<ProjectGitInfo | null>(null);
  readonly gitLoading = signal(false);
  readonly worktreeAvailable = computed(() => this.projectEntry() !== null && this.git()?.is_repo === true);
  readonly worktreeHint = computed(() => {
    if (!this.projectEntry()) {
      return this.t('newSession.worktreeNoProject');
    }
    if (this.gitLoading()) {
      return this.t('newSession.gitLoading');
    }
    if (!this.git()?.is_repo) {
      return this.t('newSession.worktreeUnavailable');
    }
    return this.t('newSession.worktreeHint');
  });
  readonly branchValid = computed(() => !this.useWorktree() || isValidBranch(this.branch().trim()));
  readonly canCreate = computed(() => !!this.directory() && !this.creating() && this.branchValid());

  readonly creating = signal(false);
  readonly error = signal<string | null>(null);

  /** Bumped per open so a slow probe from a previous open cannot land in a newer one. */
  private loadSeq = 0;

  constructor() {
    effect(() => {
      const request = this.request();
      untracked(() => {
        if (request) {
          void this.prepare(request.directory, request.agent);
        }
      });
    });
  }

  onDocumentKeydown(event: KeyboardEvent): void {
    if (event.key === 'Escape' && this.open() && !this.creating()) {
      event.preventDefault();
      this.close();
    }
  }

  close(): void {
    if (this.creating()) {
      return;
    }
    this.store.close();
  }

  agentLabel(agent: AgentInfo): string {
    return agent.model ? `${agent.name} · ${agent.model}` : agent.name;
  }

  /** The worktree spec the Create button would send, or null when the option is off. */
  worktreeSpec(): WorktreeSpec | null {
    if (!this.useWorktree()) {
      return null;
    }
    const branch = this.branch().trim();
    const base = this.base().trim();
    return base ? { branch, base } : { branch };
  }

  async create(): Promise<void> {
    const directory = this.directory();
    if (!directory || !this.canCreate()) {
      return;
    }
    this.creating.set(true);
    this.error.set(null);
    try {
      const model = this.selectedModel().trim();
      const created = await this.engine.createSession(
        directory,
        this.selectedAgent(),
        model || undefined,
        this.worktreeSpec() ?? undefined,
      );
      this.store.close();
      await this.project.refresh();
      await this.router.navigate(['/chat', created.sessionID]);
    } catch (err) {
      this.error.set(err instanceof Error ? err.message : String(err));
    } finally {
      this.creating.set(false);
    }
  }

  /** Reset the form for a fresh open and load models + git state for the directory. */
  private async prepare(directory: string, agent?: string): Promise<void> {
    const seq = ++this.loadSeq;
    this.error.set(null);
    this.creating.set(false);
    this.selectedAgent.set(agent || this.agents()[0]?.name || 'code');
    this.selectedModel.set('');
    this.models.set([]);
    this.useWorktree.set(false);
    this.branch.set(defaultBranchName());
    this.base.set('');
    this.git.set(null);

    const entry = this.projectEntry();
    this.gitLoading.set(entry !== null);
    const [models, git] = await Promise.all([
      this.loadModels(directory),
      entry ? this.engine.projectGit(entry.id).catch(() => null) : Promise.resolve(null),
    ]);
    if (seq !== this.loadSeq) {
      return;
    }
    this.models.set(models);
    this.git.set(git);
    this.gitLoading.set(false);
    if (git?.branch && !this.base()) {
      this.base.set(git.branch);
    }
  }

  /** Same derivation as the chat header's model switcher: keyed providers only. */
  private async loadModels(directory: string): Promise<string[]> {
    try {
      const cfg = await this.engine.getConfig(directory);
      const models: string[] = [];
      for (const provider of cfg.providers ?? []) {
        if (!provider.has_key) {
          continue;
        }
        for (const model of provider.models ?? []) {
          models.push(`${provider.name}/${model}`);
        }
      }
      return models;
    } catch {
      return [];
    }
  }
}
