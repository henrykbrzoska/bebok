/**
 * Tiny `⎇ branch` badge for sessions that run in a Bebok git worktree
 * (WP-GIT / F6-16). Rendered next to the meta line in the sidebar and on the
 * Start screen's session cards; the branch name comes from the engine
 * (`SessionMeta.worktree_branch`), never from splitting the path client-side.
 */

import { ChangeDetectionStrategy, Component, inject, input } from '@angular/core';

import { I18nService } from '../../i18n/i18n.service';

@Component({
  selector: 'app-branch-badge',
  changeDetection: ChangeDetectionStrategy.OnPush,
  template: `<span class="badge" [title]="title()"><span class="glyph" aria-hidden="true">⎇</span>{{ branch() }}</span>`,
  styles: `
    :host { display: inline-flex; min-width: 0; max-width: 100%; vertical-align: middle; }
    .badge {
      display: inline-flex; align-items: center; gap: 3px; min-width: 0; max-width: 100%;
      padding: 0 6px; border: 1px solid var(--border); border-radius: 999px;
      background: var(--surface-2); color: var(--text-muted);
      font-family: var(--font-mono); font-size: var(--fs-10-5, 10.5px); line-height: 16px;
      overflow: hidden; text-overflow: ellipsis; white-space: nowrap;
    }
    .glyph { color: var(--accent); font-family: var(--font-sans, inherit); }
  `,
})
export class BranchBadge {
  private readonly i18n = inject(I18nService);
  readonly branch = input.required<string>();
  readonly title = () => this.i18n.t('session.worktreeBadge', { branch: this.branch() });
}
