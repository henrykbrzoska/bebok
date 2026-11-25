/**
 * Right-drawer "Git" panel.
 *
 * Phase 1 (read-only) shows the repository state of the project the open
 * session lives in:
 * - `GET /projects/{id}/git/status`   -> branch, ahead/behind, per-file rows
 *   grouped into Staged / Unstaged / Untracked;
 * - `GET /projects/{id}/git/branches` -> local branches with tracking info;
 * - `GET /projects/{id}/git/log`      -> recent commits, newest first, with a
 *   hand-rolled lane graph (`computeLanes` + `laneSegments`, no deps);
 * - `GET /projects/{id}/git/show`     -> the unified diff of a clicked commit,
 *   rendered by the shared `<app-diff-view>`.
 *
 * Phase 4b adds the write side: stage/unstage (per file and all), commit
 * (message + amend), reset, stash, branch create/rename/delete/checkout,
 * cherry-pick (pick/abort/continue), conflicts, remotes + fetch/pull/push,
 * `git config` read/write, clone, and the forge (`gh` / `glab`) form. Every
 * destructive operation needs an explicit confirm checkbox before the request
 * sends `confirm: true` - the engine rejects it otherwise.
 *
 * Refreshing: every mutating engine route publishes `git.changed`, which this
 * panel listens to; tool writes during a turn are picked up from the session
 * transcript (debounced).
 *
 * The project is resolved from the session directory through
 * `ProjectsStore.findByPath`; a session running in a Bebok worktree
 * (`<project>/.bebok/worktrees/<branch>`) is mapped back to its owning
 * project, so the panel shows the same repository from either side.
 */

import {
  ChangeDetectionStrategy,
  Component,
  DestroyRef,
  computed,
  effect,
  inject,
  signal,
  untracked,
} from '@angular/core';

import { EngineClient } from '../../../core/engine-client.service';
import {
  EngineEvent,
  GitBranch,
  GitConfigScope,
  GitConflict,
  GitFileStatus,
  GitLogEntry,
  GitRemote,
  GitResetMode,
  GitShow,
  GitStashEntry,
  GitStatus,
  GitTools,
} from '../../../core/engine.dtos';
import { EventsStore } from '../../../core/events.store';
import { ProjectsStore } from '../../../core/projects.store';
import { I18nService } from '../../../i18n/i18n.service';
import { MessageKey } from '../../../i18n/index';
import { ChatSessionStore } from '../../../views/chat/chat-session.store';
import { DiffViewComponent } from '../../diff-view/diff-view';

/** How many commits one page of the log requests. */
export const GIT_LOG_PAGE = 50;

/** Files smaller than this are refreshed after a turn settles (no fs watch). */
const REFRESH_DEBOUNCE_MS = 400;

/** Session worktree marker: segments owned by the engine, never by the user. */
const WORKTREES_MARK = '/.bebok/worktrees/';

/** Horizontal distance between two graph lanes (px). */
export const GIT_LANE_W = 10;

/** Height of one graph row (px) - matches the log row's line box. */
export const GIT_ROW_H = 20;

/** One group of the Status section. */
export interface GitFileGroup {
  key: 'staged' | 'unstaged' | 'untracked';
  labelKey: MessageKey;
  rows: GitStatus['files'];
}

/**
 * Split the engine's per-file rows into the three display groups, keeping the
 * engine order. A file can appear in more than one group (partially staged):
 * `staged`/`unstaged` are independent porcelain flags, `untracked` is `??`.
 */
export function groupGitFiles(files: readonly GitFileStatus[]): GitFileGroup[] {
  return (
    [
      { key: 'staged', labelKey: 'git.staged', rows: files.filter((f) => f.staged) },
      { key: 'unstaged', labelKey: 'git.unstaged', rows: files.filter((f) => f.unstaged) },
      { key: 'untracked', labelKey: 'git.untracked', rows: files.filter((f) => f.untracked) },
    ] as GitFileGroup[]
  ).filter((group) => group.rows.length > 0);
}

/**
 * One row of the branch graph.
 *
 * `before` is the lane occupancy *above* the row (lane -> hash expected there),
 * `after` the occupancy below it, so a rail can be drawn from the two halves
 * without looking at neighbouring rows.
 */
export interface GitLaneRow {
  /** Lane the commit's node sits in. */
  lane: number;
  /** Lanes carried into this row from above. */
  before: (string | null)[];
  /** Lanes carried out of this row. */
  after: (string | null)[];
  /** Lanes this commit's parents continue in. */
  parentLanes: number[];
  /** Number of lane columns this row needs. */
  width: number;
}

/**
 * Assign swim lanes to a `git log` listing (newest commit first).
 *
 * `parents[i]` is commit *i*'s parent-hash list and `hashes[i]` its own hash.
 * The walk keeps one slot per open lane: a commit takes the lane already
 * waiting for it (or the first free one when it is a branch tip), its first
 * parent inherits that lane, and every additional merge parent opens or reuses
 * another lane. Freed lanes are trimmed from the right so the graph never
 * grows without bound, while `width` keeps the column count of a row stable.
 *
 * Pure and dependency-free: the panel renders the result as inline SVG.
 */
export function computeLanes(
  parents: readonly (readonly string[])[],
  hashes: readonly string[] = [],
): GitLaneRow[] {
  const rows: GitLaneRow[] = [];
  const lanes: (string | null)[] = [];

  for (let i = 0; i < parents.length; i++) {
    const hash = hashes[i] ?? '';
    const ps = parents[i] ?? [];
    const before = lanes.slice();

    // Where does this commit sit? Either a lane is already waiting for it, or
    // it is a new tip and takes the first free column.
    let lane = hash ? lanes.indexOf(hash) : -1;
    if (lane < 0) {
      lane = lanes.indexOf(null);
      if (lane < 0) {
        lane = lanes.length;
        lanes.push(null);
      }
    }

    // The first parent continues in this lane unless another lane already
    // waits for it (then this row's column is freed and the rail it joins is
    // used instead).
    const first = ps.length > 0 ? ps[0] : null;
    const parentLanes: number[] = [];
    if (first === null) {
      lanes[lane] = null;
    } else {
      const existing = lanes.findIndex((h, idx) => idx !== lane && h === first);
      if (existing >= 0) {
        lanes[lane] = null;
        parentLanes.push(existing);
      } else {
        lanes[lane] = first;
        parentLanes.push(lane);
      }
    }

    // Additional merge parents open (or reuse) their own lane.
    for (const parent of ps.slice(1)) {
      let pl = lanes.indexOf(parent);
      if (pl < 0) {
        pl = lanes.indexOf(null);
        if (pl < 0) {
          pl = lanes.length;
          lanes.push(null);
        }
        lanes[pl] = parent;
      }
      if (!parentLanes.includes(pl)) {
        parentLanes.push(pl);
      }
    }

    while (lanes.length > 0 && lanes[lanes.length - 1] === null) {
      lanes.pop();
    }

    const after = lanes.slice();
    rows.push({
      lane,
      before,
      after,
      parentLanes,
      width: Math.max(before.length, after.length, lane + 1),
    });
  }

  return rows;
}

/** One drawn piece of the branch graph: SVG path data plus the lane it colours. */
export interface GitLaneSegment {
  d: string;
  lane: number;
}

/**
 * Turn one [`GitLaneRow`] into SVG path data. Vertical rails run the full row
 * height when they continue below and half height when they start or end here;
 * merge parents get a cubic curve from the node down to their lane.
 */
export function laneSegments(
  row: GitLaneRow,
  laneW: number = GIT_LANE_W,
  rowH: number = GIT_ROW_H,
): GitLaneSegment[] {
  const segments: GitLaneSegment[] = [];
  const cx = (lane: number): number => lane * laneW + laneW / 2;
  const mid = rowH / 2;
  const width = Math.max(row.width, row.lane + 1, 1);

  for (let lane = 0; lane < width; lane++) {
    if (lane === row.lane) {
      // Incoming rail above the node; the half below is drawn with the
      // parents so a merge can fan out.
      if ((row.before[lane] ?? null) !== null) {
        segments.push({ lane, d: `M${cx(lane)} 0 V${mid}` });
      }
      continue;
    }
    const above = row.before[lane] ?? null;
    const below = row.after[lane] ?? null;
    if (above !== null && below !== null) {
      segments.push({ lane, d: `M${cx(lane)} 0 V${rowH}` });
    } else if (above !== null) {
      segments.push({ lane, d: `M${cx(lane)} 0 V${mid}` });
    } else if (below !== null) {
      segments.push({ lane, d: `M${cx(lane)} ${mid} V${rowH}` });
    }
  }

  for (const pl of row.parentLanes) {
    if (pl === row.lane) {
      segments.push({ lane: pl, d: `M${cx(pl)} ${mid} V${rowH}` });
      continue;
    }
    const x1 = cx(row.lane);
    const x2 = cx(pl);
    segments.push({
      lane: pl,
      d: `M${x1} ${mid} C${x1} ${mid + 5}, ${x2} ${rowH - 5}, ${x2} ${rowH}`,
    });
  }

  return segments;
}

/** One rendered log row: the commit, its lane data and the SVG to draw. */
interface GitGraphRow {
  entry: GitLogEntry;
  row: GitLaneRow;
  segments: GitLaneSegment[];
  width: number;
  height: number;
  nodeX: number;
  nodeY: number;
}

function describe(err: unknown): string {
  return err instanceof Error ? err.message : String(err);
}

@Component({
  selector: 'app-git-panel',
  changeDetection: ChangeDetectionStrategy.OnPush,
  imports: [DiffViewComponent],
  template: `
    @if (!projectId()) {
      <div class="empty">{{ t('git.noProject') }}</div>
    } @else {
      <div class="panel-body">
        @if (error()) {
          <div class="error" data-testid="git-error">{{ t('git.error') }}: {{ error() }}</div>
        }
        @if (note()) {
          <div class="note" data-testid="git-note">{{ note() }}</div>
        }

        <!-- Conflicts ------------------------------------------------ -->
        @if (conflicts().length > 0) {
          <div class="banner" data-testid="git-conflicts">
            <div class="banner-title">{{ t('git.conflictsBanner') }}</div>
            <div class="banner-hint">{{ t('git.conflictsHint') }}</div>
            <ul class="rows">
              @for (conflict of conflicts(); track conflict.path) {
                <li class="row" data-testid="git-conflict-row" [title]="conflict.path">
                  <span class="code">UU</span>
                  <span class="path">
                    <span class="path-dir">{{ dirOf(conflict.path) }}</span
                    ><span class="path-name">{{ nameOf(conflict.path) }}</span>
                  </span>
                  <span class="conflict-stages">{{ conflict.stages }}</span>
                </li>
              }
            </ul>
          </div>
        }

        <!-- Status ---------------------------------------------------- -->
        <div class="head">
          <span class="section-label">{{ t('git.status') }}</span>
          @if (status(); as s) {
            <span class="branch" [title]="s.branch ?? ''">{{ s.branch || '—' }}</span>
            @if (s.ahead || s.behind) {
              <span class="track" [title]="t('git.status')">
                @if (s.ahead) {
                  <span class="ahead">↑{{ s.ahead }}</span>
                }
                @if (s.behind) {
                  <span class="behind">↓{{ s.behind }}</span>
                }
              </span>
            }
          }
          <button
            type="button"
            class="icon"
            data-testid="git-stage-all"
            (click)="stageAll()"
            [disabled]="!canStageAll()"
            [title]="t('git.stageAll')"
            [attr.aria-label]="t('git.stageAll')"
          >
            ＋
          </button>
          <button
            type="button"
            class="icon"
            data-testid="git-unstage-all"
            (click)="unstageAll()"
            [disabled]="!canUnstageAll()"
            [title]="t('git.unstageAll')"
            [attr.aria-label]="t('git.unstageAll')"
          >
            －
          </button>
          <button
            type="button"
            class="refresh"
            data-testid="git-refresh"
            (click)="refresh()"
            [disabled]="loading()"
            [title]="t('git.refresh')"
            [attr.aria-label]="t('git.refresh')"
          >
            ↻
          </button>
        </div>

        @if (loading() && !status()) {
          <div class="none">{{ t('git.loading') }}</div>
        } @else if (groups().length === 0) {
          <div class="none">{{ t('git.empty') }}</div>
        } @else {
          @for (group of groups(); track group.key) {
            <div class="group-head" [attr.data-group]="group.key">
              <span class="group-label">{{ t(group.labelKey) }}</span>
              <span class="group-count">{{ group.rows.length }}</span>
            </div>
            <ul class="rows">
              @for (file of group.rows; track file.path) {
                <li class="row" data-testid="git-file-row" [title]="fileTitle(file)">
                  <span class="code" [attr.data-code]="file.x + file.y">{{ file.x }}{{ file.y }}</span>
                  <span class="path">
                    <span class="path-dir">{{ dirOf(file.path) }}</span
                    ><span class="path-name">{{ nameOf(file.path) }}</span>
                  </span>
                  @if (file.staged) {
                    <button
                      type="button"
                      class="icon"
                      data-testid="git-unstage-file"
                      (click)="unstageFile(file)"
                      [disabled]="busy() !== null"
                      [title]="t('git.unstage')"
                      [attr.aria-label]="t('git.unstage')"
                    >
                      －
                    </button>
                  }
                  @if (file.unstaged || file.untracked) {
                    <button
                      type="button"
                      class="icon"
                      data-testid="git-stage-file"
                      (click)="stageFile(file)"
                      [disabled]="busy() !== null"
                      [title]="t('git.stage')"
                      [attr.aria-label]="t('git.stage')"
                    >
                      ＋
                    </button>
                  }
                </li>
              }
            </ul>
          }
        }

        <!-- Commit ---------------------------------------------------- -->
        <div class="section" data-testid="git-commit">
          <div class="head">
            <span class="section-label">{{ t('git.commit') }}</span>
            @if (lastCommit(); as hash) {
              <span class="note-inline" data-testid="git-commit-hash">
                {{ t('git.commitDone') }} {{ shortHash(hash) }}
              </span>
            }
          </div>
          <div class="form">
            <textarea
              class="input area"
              rows="2"
              data-testid="git-commit-message"
              [value]="commitMessage()"
              (input)="commitMessage.set(valueOf($event))"
              [placeholder]="t('git.commitMessage')"
              [attr.aria-label]="t('git.commitMessage')"
            ></textarea>
            <label class="check">
              <input
                type="checkbox"
                data-testid="git-commit-amend"
                [checked]="commitAmend()"
                (change)="commitAmend.set(checkedOf($event))"
              />
              {{ t('git.commitAmend') }}
            </label>
            <button
              type="button"
              class="btn primary"
              data-testid="git-commit-button"
              (click)="commit()"
              [disabled]="!canCommit()"
            >
              {{ t('git.commit') }}
            </button>
          </div>
        </div>

        <!-- Reset ----------------------------------------------------- -->
        <div class="section" data-testid="git-reset">
          <div class="head">
            <span class="section-label">{{ t('git.reset') }}</span>
          </div>
          <div class="form">
            <div class="line">
              <select
                class="input select"
                data-testid="git-reset-mode"
                [value]="resetMode()"
                (change)="resetMode.set(resetModeOf($event))"
                [attr.aria-label]="t('git.resetMode')"
              >
                <option value="soft">soft</option>
                <option value="mixed">mixed</option>
                <option value="hard">hard</option>
              </select>
              <input
                class="input"
                type="text"
                data-testid="git-reset-rev"
                [value]="resetRev()"
                (input)="resetRev.set(valueOf($event))"
                [placeholder]="t('git.resetRev')"
                [attr.aria-label]="t('git.resetRev')"
              />
            </div>
            @if (resetMode() === 'hard') {
              <label class="check">
                <input
                  type="checkbox"
                  data-testid="git-reset-confirm"
                  [checked]="resetConfirm()"
                  (change)="resetConfirm.set(checkedOf($event))"
                />
                {{ t('git.resetHardConfirm') }}
              </label>
            }
            <button
              type="button"
              class="btn"
              data-testid="git-reset-run"
              (click)="reset()"
              [disabled]="!canReset()"
            >
              {{ t('git.reset') }}
            </button>
          </div>
        </div>

        <!-- Branches -------------------------------------------------- -->
        <div class="head">
          <span class="section-label">{{ t('git.branches') }}</span>
          <button
            type="button"
            class="refresh"
            data-testid="git-branch-form-toggle"
            (click)="toggleBranchForm()"
            [title]="t('git.branchNew')"
            [attr.aria-label]="t('git.branchNew')"
          >
            ＋
          </button>
        </div>

        @if (showBranchForm()) {
          <div class="form" data-testid="git-branch-form">
            <div class="line">
              <input
                class="input"
                type="text"
                data-testid="git-branch-name"
                [value]="branchName()"
                (input)="branchName.set(valueOf($event))"
                [placeholder]="t('git.branchNew')"
                [attr.aria-label]="t('git.branchNew')"
              />
              <input
                class="input"
                type="text"
                data-testid="git-branch-base"
                [value]="branchBase()"
                (input)="branchBase.set(valueOf($event))"
                [placeholder]="t('git.branchBase')"
                [attr.aria-label]="t('git.branchBase')"
              />
            </div>
            <button
              type="button"
              class="btn"
              data-testid="git-branch-create"
              (click)="createBranch()"
              [disabled]="!canCreateBranch()"
            >
              {{ t('git.branchCreate') }}
            </button>
          </div>
        }

        @if (branches().length === 0) {
          <div class="none">{{ loading() ? t('git.loading') : t('git.empty') }}</div>
        } @else {
          <ul class="rows">
            @for (branch of branches(); track branch.name) {
              <li
                class="row branch-row"
                data-testid="git-branch-row"
                [class.current]="branch.current"
                [title]="branch.upstream ?? branch.name"
                (click)="checkoutBranch(branch)"
              >
                <span class="branch-name">{{ branch.name }}</span>
                @if (branch.current) {
                  <span class="badge">{{ t('git.current') }}</span>
                }
                @if (branch.upstream) {
                  <span class="upstream">{{ branch.upstream }}</span>
                }
                @if (branch.ahead || branch.behind) {
                  <span class="track">
                    @if (branch.ahead) {
                      <span class="ahead">↑{{ branch.ahead }}</span>
                    }
                    @if (branch.behind) {
                      <span class="behind">↓{{ branch.behind }}</span>
                    }
                  </span>
                }
                <button
                  type="button"
                  class="icon"
                  data-testid="git-branch-rename"
                  (click)="startRename(branch.name, $event)"
                  [disabled]="busy() !== null"
                  [title]="t('git.branchRename')"
                  [attr.aria-label]="t('git.branchRename')"
                >
                  ✎
                </button>
                <button
                  type="button"
                  class="icon"
                  data-testid="git-branch-delete"
                  (click)="deleteBranch(branch.name, $event)"
                  [disabled]="busy() !== null || !canDeleteBranch(branch.name)"
                  [title]="t('git.branchDelete')"
                  [attr.aria-label]="t('git.branchDelete')"
                >
                  ✕
                </button>
              </li>
            }
          </ul>
        }

        @if (renameFrom()) {
          <div class="form" data-testid="git-branch-rename-form">
            <div class="line">
              <span class="label">{{ renameFrom() }} →</span>
              <input
                class="input"
                type="text"
                data-testid="git-branch-rename-to"
                [value]="renameTo()"
                (input)="renameTo.set(valueOf($event))"
                [placeholder]="t('git.branchRenameTo')"
                [attr.aria-label]="t('git.branchRenameTo')"
              />
            </div>
            <div class="line">
              <label class="check">
                <input
                  type="checkbox"
                  data-testid="git-branch-force-confirm"
                  [checked]="deleteConfirm()"
                  (change)="deleteConfirm.set(checkedOf($event))"
                />
                {{ t('git.branchForce') }}
              </label>
              <button
                type="button"
                class="btn"
                data-testid="git-branch-rename-run"
                (click)="renameBranch()"
                [disabled]="!canRenameBranch()"
              >
                {{ t('git.branchRename') }}
              </button>
            </div>
          </div>
        }

        <!-- Log ------------------------------------------------------- -->
        <div class="head">
          <span class="section-label">{{ t('git.log') }}</span>
        </div>
        @if (log().length === 0) {
          <div class="none">{{ loading() ? t('git.loading') : t('git.empty') }}</div>
        } @else {
          <ul class="rows">
            @for (g of graphRows(); track g.entry.hash) {
              <li class="log-item" [style.min-height.px]="g.height">
                <svg
                  class="graph"
                  [attr.width]="g.width * laneWidth"
                  [attr.height]="g.height"
                  aria-hidden="true"
                >
                  @for (seg of g.segments; track $index) {
                    <path [attr.d]="seg.d" [attr.class]="'rail lane-' + (seg.lane % 6)" />
                  }
                  <circle
                    [attr.cx]="g.nodeX"
                    [attr.cy]="g.nodeY"
                    r="2.6"
                    [attr.class]="'node lane-' + (g.row.lane % 6)"
                  />
                </svg>
                <button
                  type="button"
                  class="row log-row"
                  data-testid="git-log-row"
                  [title]="g.entry.subject"
                  (click)="openCommit(g.entry)"
                >
                  <span class="hash">{{ shortHash(g.entry.hash) }}</span>
                  <span class="subject">{{ g.entry.subject }}</span>
                  @for (ref of g.entry.refs; track ref) {
                    <span class="badge ref">{{ ref }}</span>
                  }
                  <span class="author">{{ g.entry.author }}</span>
                </button>
              </li>
            }
          </ul>
        }

        <!-- Stash ----------------------------------------------------- -->
        <div class="head">
          <span class="section-label">{{ t('git.stash') }}</span>
        </div>
        <div class="form" data-testid="git-stash-form">
          <div class="line">
            <input
              class="input"
              type="text"
              data-testid="git-stash-message"
              [value]="stashMessage()"
              (input)="stashMessage.set(valueOf($event))"
              [placeholder]="t('git.stashMessage')"
              [attr.aria-label]="t('git.stashMessage')"
            />
            <button
              type="button"
              class="btn"
              data-testid="git-stash-push"
              (click)="stashPush()"
              [disabled]="busy() !== null"
            >
              {{ t('git.stashPush') }}
            </button>
          </div>
        </div>
        @if (stashes().length === 0) {
          <div class="none">{{ t('git.stashEmpty') }}</div>
        } @else {
          <ul class="rows">
            @for (stash of stashes(); track stash.index) {
              <li class="row" data-testid="git-stash-row" [title]="stash.message">
                <span class="hash">{{ stash.name }}</span>
                <span class="subject">{{ stash.message }}</span>
                <button
                  type="button"
                  class="icon"
                  data-testid="git-stash-pop"
                  (click)="stashAction('pop', stash.index)"
                  [disabled]="busy() !== null"
                  [title]="t('git.stashPop')"
                  [attr.aria-label]="t('git.stashPop')"
                >
                  ↑
                </button>
                <button
                  type="button"
                  class="icon"
                  data-testid="git-stash-apply"
                  (click)="stashAction('apply', stash.index)"
                  [disabled]="busy() !== null"
                  [title]="t('git.stashApply')"
                  [attr.aria-label]="t('git.stashApply')"
                >
                  ⤓
                </button>
                <button
                  type="button"
                  class="icon"
                  data-testid="git-stash-drop"
                  (click)="stashAction('drop', stash.index)"
                  [disabled]="busy() !== null || !stashConfirm()"
                  [title]="t('git.stashDrop')"
                  [attr.aria-label]="t('git.stashDrop')"
                >
                  ✕
                </button>
              </li>
            }
          </ul>
          <div class="form">
            <label class="check">
              <input
                type="checkbox"
                data-testid="git-stash-confirm"
                [checked]="stashConfirm()"
                (change)="stashConfirm.set(checkedOf($event))"
              />
              {{ t('git.confirmDanger') }}
            </label>
          </div>
        }

        <!-- Cherry-pick ----------------------------------------------- -->
        <div class="section" data-testid="git-cherry-pick">
          <div class="head">
            <span class="section-label">{{ t('git.cherryPick') }}</span>
          </div>
          <div class="form">
            <div class="line">
              <input
                class="input"
                type="text"
                data-testid="git-cherry-pick-rev"
                [value]="cherryRev()"
                (input)="cherryRev.set(valueOf($event))"
                [placeholder]="t('git.cherryPickRev')"
                [attr.aria-label]="t('git.cherryPickRev')"
              />
              <button
                type="button"
                class="btn"
                data-testid="git-cherry-pick-run"
                (click)="cherryPick()"
                [disabled]="!canCherryPick()"
              >
                {{ t('git.cherryPick') }}
              </button>
            </div>
            <div class="line">
              <button
                type="button"
                class="btn"
                data-testid="git-cherry-pick-abort"
                (click)="cherryPickAbort()"
                [disabled]="busy() !== null"
              >
                {{ t('git.cherryPickAbort') }}
              </button>
              <button
                type="button"
                class="btn"
                data-testid="git-cherry-pick-continue"
                (click)="cherryPickContinue()"
                [disabled]="busy() !== null"
              >
                {{ t('git.cherryPickContinue') }}
              </button>
            </div>
          </div>
        </div>

        <!-- Remotes + sync -------------------------------------------- -->
        <div class="head">
          <span class="section-label">{{ t('git.remotes') }}</span>
        </div>
        @if (remotes().length === 0) {
          <div class="none">{{ t('git.remoteEmpty') }}</div>
        } @else {
          <ul class="rows">
            @for (remote of remotes(); track remote.name) {
              <li class="row" data-testid="git-remote-row" [title]="remote.url">
                <span class="branch-name">{{ remote.name }}</span>
                <span class="upstream">{{ remote.url }}</span>
              </li>
            }
          </ul>
        }
        <div class="form" data-testid="git-remote-form">
          <div class="line">
            <input
              class="input narrow"
              type="text"
              data-testid="git-remote-name"
              [value]="remoteName()"
              (input)="remoteName.set(valueOf($event))"
              [placeholder]="t('git.syncRemote')"
              [attr.aria-label]="t('git.syncRemote')"
            />
            <input
              class="input"
              type="text"
              data-testid="git-remote-url"
              [value]="remoteUrl()"
              (input)="remoteUrl.set(valueOf($event))"
              [placeholder]="t('git.remoteUrl')"
              [attr.aria-label]="t('git.remoteUrl')"
            />
            <button
              type="button"
              class="btn"
              data-testid="git-remote-save"
              (click)="setRemoteUrl()"
              [disabled]="!canSetRemoteUrl()"
            >
              {{ t('git.remoteSave') }}
            </button>
          </div>
        </div>

        <div class="head">
          <span class="section-label">{{ t('git.sync') }}</span>
        </div>
        <div class="form" data-testid="git-sync-form">
          <div class="line">
            <input
              class="input narrow"
              type="text"
              data-testid="git-sync-remote"
              [value]="remoteName()"
              (input)="remoteName.set(valueOf($event))"
              [placeholder]="t('git.syncRemote')"
              [attr.aria-label]="t('git.syncRemote')"
            />
            <input
              class="input"
              type="text"
              data-testid="git-sync-refspec"
              [value]="syncRefspec()"
              (input)="syncRefspec.set(valueOf($event))"
              [placeholder]="t('git.syncRefspec')"
              [attr.aria-label]="t('git.syncRefspec')"
            />
          </div>
          <div class="line">
            <button
              type="button"
              class="btn"
              data-testid="git-sync-fetch"
              (click)="syncFetch()"
              [disabled]="!canSyncFetch()"
            >
              {{ t('git.syncFetch') }}
            </button>
            <button
              type="button"
              class="btn"
              data-testid="git-sync-pull"
              (click)="syncPull()"
              [disabled]="!canSyncFetch()"
            >
              {{ t('git.syncPull') }}
            </button>
            <button
              type="button"
              class="btn"
              data-testid="git-sync-push"
              (click)="syncPush()"
              [disabled]="!canSyncPush()"
            >
              {{ t('git.syncPush') }}
            </button>
          </div>
        </div>

        <!-- Config ---------------------------------------------------- -->
        <div class="section" data-testid="git-config">
          <div class="head">
            <span class="section-label">{{ t('git.config') }}</span>
          </div>
          <div class="form">
            <div class="line">
              <input
                class="input"
                type="text"
                data-testid="git-config-key"
                [value]="configKey()"
                (input)="configKey.set(valueOf($event))"
                [placeholder]="t('git.configKey')"
                [attr.aria-label]="t('git.configKey')"
              />
              <select
                class="input select"
                data-testid="git-config-scope"
                [value]="configScope()"
                (change)="configScope.set(configScopeOf($event))"
                [attr.aria-label]="t('git.configScope')"
              >
                <option value="local">{{ t('git.configLocal') }}</option>
                <option value="global">{{ t('git.configGlobal') }}</option>
              </select>
              <button
                type="button"
                class="btn"
                data-testid="git-config-get"
                (click)="configGet()"
                [disabled]="!canConfigGet()"
              >
                {{ t('git.configGet') }}
              </button>
            </div>
            <div class="line">
              <input
                class="input"
                type="text"
                data-testid="git-config-value"
                [value]="configValue()"
                (input)="configValue.set(valueOf($event))"
                [placeholder]="t('git.configValue')"
                [attr.aria-label]="t('git.configValue')"
              />
              <button
                type="button"
                class="btn"
                data-testid="git-config-set"
                (click)="configSet()"
                [disabled]="!canConfigSet()"
              >
                {{ t('git.configSet') }}
              </button>
            </div>
            @if (configRead() === null) {
              <div class="none" data-testid="git-config-unset">{{ t('git.configUnset') }}</div>
            }
            @if (configScope() === 'global') {
              <label class="check">
                <input
                  type="checkbox"
                  data-testid="git-config-confirm"
                  [checked]="configConfirm()"
                  (change)="configConfirm.set(checkedOf($event))"
                />
                {{ t('git.confirmDanger') }}
              </label>
            }
          </div>
        </div>

        <!-- Clone ----------------------------------------------------- -->
        <div class="section" data-testid="git-clone">
          <div class="head">
            <span class="section-label">{{ t('git.clone') }}</span>
          </div>
          <div class="form">
            <input
              class="input"
              type="text"
              data-testid="git-clone-source"
              [value]="cloneSource()"
              (input)="cloneSource.set(valueOf($event))"
              [placeholder]="t('git.cloneSource')"
              [attr.aria-label]="t('git.cloneSource')"
            />
            <div class="line">
              <input
                class="input"
                type="text"
                data-testid="git-clone-dest"
                [value]="cloneDest()"
                (input)="cloneDest.set(valueOf($event))"
                [placeholder]="t('git.cloneDest')"
                [attr.aria-label]="t('git.cloneDest')"
              />
              <input
                class="input narrow"
                type="text"
                data-testid="git-clone-branch"
                [value]="cloneBranch()"
                (input)="cloneBranch.set(valueOf($event))"
                [placeholder]="t('git.cloneBranch')"
                [attr.aria-label]="t('git.cloneBranch')"
              />
            </div>
            <button
              type="button"
              class="btn"
              data-testid="git-clone-run"
              (click)="cloneRepo()"
              [disabled]="!canClone()"
            >
              {{ t('git.cloneRun') }}
            </button>
          </div>
        </div>

        <!-- Tools + forge --------------------------------------------- -->
        <div class="head">
          <span class="section-label">{{ t('git.tools') }}</span>
          @if (tools(); as tl) {
            <span class="badge" [class.on]="tl.git" data-testid="git-tool-git">git</span>
            <span class="badge" [class.on]="tl.gh" data-testid="git-tool-gh">gh</span>
            <span class="badge" [class.on]="tl.glab" data-testid="git-tool-glab">glab</span>
          }
        </div>
        @if (forgeAvailable()) {
          <div class="form" data-testid="git-forge">
            <div class="line">
              <select
                class="input select"
                data-testid="git-forge-tool"
                [value]="forgeTool()"
                (change)="forgeTool.set(forgeToolOf($event))"
                [attr.aria-label]="t('git.forgeTool')"
              >
                @if (tools()?.gh) {
                  <option value="gh">gh</option>
                }
                @if (tools()?.glab) {
                  <option value="glab">glab</option>
                }
              </select>
              <input
                class="input"
                type="text"
                data-testid="git-forge-args"
                [value]="forgeArgs()"
                (input)="forgeArgs.set(valueOf($event))"
                [placeholder]="t('git.forgeArgs')"
                [attr.aria-label]="t('git.forgeArgs')"
              />
              <button
                type="button"
                class="btn"
                data-testid="git-forge-run"
                (click)="runForge()"
                [disabled]="!canRunForge()"
              >
                {{ t('git.forgeRun') }}
              </button>
            </div>
            @if (forgeOutput(); as out) {
              <pre class="output" data-testid="git-forge-output">{{ out }}</pre>
            }
          </div>
        } @else if (tools()) {
          <div class="none" data-testid="git-forge-unavailable">{{ t('git.forgeUnavailable') }}</div>
        }
      </div>

      @if (shown(); as commit) {
        <div class="show-overlay" data-testid="git-show" (click)="closeShow()">
          <div class="show-card" (click)="$event.stopPropagation()">
            <header class="show-head">
              <span class="show-hash">{{ shortHash(commit.hash) }}</span>
              <span class="show-subject" [title]="commit.subject">{{ commit.subject }}</span>
              <button
                type="button"
                class="show-close"
                data-testid="git-show-close"
                (click)="closeShow()"
                [title]="t('git.close')"
                [attr.aria-label]="t('git.close')"
              >
                ✕
              </button>
            </header>
            @if (commit.files.length > 0) {
              <ul class="show-files">
                @for (file of commit.files; track file.path) {
                  <li class="show-file">
                    <span class="code">{{ file.status }}</span>
                    <span class="path-name">{{ file.path }}</span>
                  </li>
                }
              </ul>
            }
            <div class="show-diff">
              <app-diff-view [text]="commit.diff" [fileLabel]="commit.subject" />
            </div>
          </div>
        </div>
      }
    }
  `,
  styles: [
    `
      .empty,
      .none,
      .error,
      .note {
        padding: var(--space-12) var(--space-14);
        font-size: var(--fs-11-5);
        color: var(--text-faint);
      }

      .error {
        color: var(--danger);
        overflow-wrap: anywhere;
      }

      .note {
        color: var(--accent);
        overflow-wrap: anywhere;
      }

      .note-inline {
        flex: 0 1 auto;
        min-width: 0;
        overflow: hidden;
        text-overflow: ellipsis;
        white-space: nowrap;
        font-size: var(--fs-10);
        color: var(--accent);
      }

      .panel-body {
        display: flex;
        flex-direction: column;
        padding-bottom: var(--space-8);
      }

      .head {
        display: flex;
        align-items: center;
        gap: var(--space-6);
        padding: var(--space-6) var(--space-10);
        border-bottom: 1px solid var(--border);
      }

      .head:not(:first-child) {
        border-top: 1px solid var(--border);
      }

      .section-label {
        flex: none;
        font-size: var(--fs-label);
        text-transform: uppercase;
        letter-spacing: var(--label-tracking);
        color: var(--text-faint);
      }

      .branch {
        flex: 0 1 auto;
        min-width: 0;
        overflow: hidden;
        text-overflow: ellipsis;
        white-space: nowrap;
        font-family: var(--font-mono);
        font-size: var(--fs-11);
        color: var(--code-text-strong);
      }

      .track {
        flex: none;
        display: inline-flex;
        gap: 4px;
        font-family: var(--font-mono);
        font-size: var(--fs-10);
      }

      .ahead {
        color: var(--diff-add-text);
      }

      .behind {
        color: var(--diff-remove-text);
      }

      .refresh,
      .icon {
        flex: none;
        border: none;
        background: transparent;
        color: var(--text-muted);
        padding: 0 2px;
        cursor: pointer;
      }

      .refresh {
        margin-left: auto;
        font-size: var(--fs-13);
      }

      .icon {
        font-size: var(--fs-11);
        line-height: 1.4;
      }

      .head .icon + .refresh {
        margin-left: 0;
      }

      .icon:hover:not(:disabled),
      .refresh:hover:not(:disabled) {
        color: var(--accent);
      }

      .icon:disabled,
      .refresh:disabled,
      .btn:disabled {
        opacity: 0.45;
        cursor: default;
      }

      .banner {
        margin: var(--space-8) var(--space-10);
        padding: var(--space-8) var(--space-10);
        border: 1px solid color-mix(in srgb, var(--danger) 55%, var(--border));
        border-radius: var(--radius-panel);
        background: color-mix(in srgb, var(--danger) 10%, transparent);
      }

      .banner-title {
        font-size: var(--fs-11-5);
        font-weight: 600;
        color: var(--danger);
      }

      .banner-hint {
        margin-top: 2px;
        font-size: var(--fs-10);
        color: var(--text-muted);
      }

      .group-head {
        display: flex;
        align-items: baseline;
        gap: var(--space-6);
        padding: var(--space-6) var(--space-10) 2px;
        font-size: var(--fs-label);
        text-transform: uppercase;
        letter-spacing: var(--label-tracking);
        color: var(--text-faint);
      }

      .group-label {
        flex: 1 1 auto;
        min-width: 0;
      }

      .group-count {
        flex: none;
        font-family: var(--font-mono);
      }

      .rows {
        list-style: none;
        margin: 0;
        padding: var(--space-6) 0 0;
        display: flex;
        flex-direction: column;
        gap: 1px;
      }

      .row {
        display: flex;
        align-items: baseline;
        gap: var(--space-6);
        width: 100%;
        padding: 3px var(--space-10);
        border: 1px solid transparent;
        border-radius: 0;
        background: transparent;
        text-align: left;
        font-family: var(--font-mono);
        font-size: var(--fs-11);
        color: var(--text-muted);
        min-width: 0;
      }

      .row.current {
        color: var(--text);
      }

      .branch-row {
        cursor: pointer;
      }

      .branch-row:hover {
        background: var(--surface-2);
      }

      .code {
        flex: none;
        width: 2ch;
        color: var(--text-faint);
        text-transform: uppercase;
      }

      .conflict-stages {
        flex: none;
        font-size: var(--fs-10);
        color: var(--danger);
      }

      .path {
        flex: 1 1 auto;
        min-width: 0;
        display: flex;
        color: var(--code-text-strong);
        white-space: nowrap;
      }

      .path-dir {
        flex: 0 1 auto;
        min-width: 0;
        overflow: hidden;
        text-overflow: ellipsis;
        color: var(--text-muted);
      }

      .path-name {
        flex: 0 0 auto;
      }

      .branch-name {
        flex: 1 1 auto;
        min-width: 0;
        overflow: hidden;
        text-overflow: ellipsis;
        white-space: nowrap;
        color: var(--code-text-strong);
      }

      .upstream {
        flex: 0 1 auto;
        min-width: 0;
        overflow: hidden;
        text-overflow: ellipsis;
        white-space: nowrap;
        color: var(--text-faint);
        font-size: var(--fs-10);
      }

      .badge {
        flex: none;
        font-family: var(--font-sans);
        font-size: 9.5px;
        text-transform: uppercase;
        letter-spacing: var(--label-tracking);
        padding: 0 5px;
        border-radius: var(--radius-bubble);
        border: 1px solid var(--border-strong);
        color: var(--text-faint);
        line-height: 1.5;
      }

      .badge.on {
        color: var(--diff-add-text);
        border-color: color-mix(in srgb, var(--diff-add-text) 60%, var(--border));
      }

      .badge.ref {
        color: var(--accent);
        border-color: color-mix(in srgb, var(--accent) 60%, var(--border));
        text-transform: none;
        letter-spacing: 0;
      }

      /* --- log rows with the hand-rolled lane graph ---------------------- */

      .log-item {
        display: flex;
        align-items: center;
        gap: 0;
        padding-left: var(--space-8);
      }

      .graph {
        flex: none;
        display: block;
        overflow: visible;
      }

      .rail {
        fill: none;
        stroke-width: 1.4;
        stroke-linecap: round;
      }

      .node {
        stroke: none;
      }

      .lane-0 {
        stroke: var(--accent);
        fill: var(--accent);
      }

      .lane-1 {
        stroke: var(--diff-add-text);
        fill: var(--diff-add-text);
      }

      .lane-2 {
        stroke: var(--diff-remove-text);
        fill: var(--diff-remove-text);
      }

      .lane-3 {
        stroke: #c08bd8;
        fill: #c08bd8;
      }

      .lane-4 {
        stroke: #d8a15a;
        fill: #d8a15a;
      }

      .lane-5 {
        stroke: #5aa6d8;
        fill: #5aa6d8;
      }

      .log-row {
        flex: 1 1 auto;
        cursor: pointer;
      }

      .log-row:hover {
        background: var(--surface-2);
      }

      .hash {
        flex: none;
        color: var(--text-faint);
      }

      .subject {
        flex: 1 1 auto;
        min-width: 0;
        overflow: hidden;
        text-overflow: ellipsis;
        white-space: nowrap;
        font-family: var(--font-sans);
        color: var(--code-text-strong);
      }

      .author {
        flex: none;
        max-width: 90px;
        overflow: hidden;
        text-overflow: ellipsis;
        white-space: nowrap;
        font-size: var(--fs-10);
        color: var(--text-faint);
      }

      /* --- forms -------------------------------------------------------- */

      .section {
        display: flex;
        flex-direction: column;
      }

      .form {
        display: flex;
        flex-direction: column;
        gap: var(--space-6);
        padding: var(--space-8) var(--space-10);
      }

      .line {
        display: flex;
        align-items: center;
        gap: var(--space-6);
        min-width: 0;
      }

      .label {
        flex: none;
        font-family: var(--font-mono);
        font-size: var(--fs-10);
        color: var(--text-muted);
      }

      .input {
        flex: 1 1 auto;
        min-width: 0;
        padding: 3px var(--space-6);
        border: 1px solid var(--border);
        border-radius: var(--radius-bubble);
        background: var(--surface-2);
        color: var(--text);
        font-family: var(--font-mono);
        font-size: var(--fs-11);
      }

      .input:focus {
        outline: none;
        border-color: var(--accent);
      }

      .input.area {
        resize: vertical;
        font-family: var(--font-sans);
        line-height: 1.4;
      }

      .input.narrow,
      .input.select {
        flex: 0 0 auto;
        width: 92px;
      }

      .input.select {
        width: auto;
        min-width: 72px;
      }

      .btn {
        flex: none;
        align-self: flex-start;
        padding: 3px var(--space-10);
        border: 1px solid var(--border-strong);
        border-radius: var(--radius-bubble);
        background: var(--surface-2);
        color: var(--text);
        font-size: var(--fs-11);
        cursor: pointer;
      }

      .btn:hover:not(:disabled) {
        border-color: var(--accent);
        color: var(--accent);
      }

      .btn.primary {
        background: var(--accent);
        border-color: var(--accent);
        color: var(--accent-contrast, #fff);
      }

      .check {
        display: inline-flex;
        align-items: center;
        gap: var(--space-6);
        font-size: var(--fs-10);
        color: var(--text-muted);
        cursor: pointer;
      }

      .output {
        margin: 0;
        max-height: 180px;
        overflow: auto;
        padding: var(--space-6);
        border: 1px solid var(--border);
        border-radius: var(--radius-bubble);
        background: var(--code-bg, var(--surface-2));
        color: var(--code-text-strong);
        font-family: var(--font-mono);
        font-size: var(--fs-10);
        white-space: pre-wrap;
        overflow-wrap: anywhere;
      }

      /* --- commit diff overlay (same pattern as the changes overlay) ----- */

      .show-overlay {
        position: fixed;
        inset: 0;
        z-index: 60;
        display: flex;
        align-items: center;
        justify-content: center;
        padding: var(--space-16);
        background: rgb(0 0 0 / 55%);
      }

      .show-card {
        display: flex;
        flex-direction: column;
        width: min(960px, 92vw);
        max-height: 86vh;
        background: var(--surface);
        border: 1px solid var(--border-strong);
        border-radius: var(--radius-panel);
        overflow: hidden;
      }

      .show-head {
        display: flex;
        align-items: center;
        gap: var(--space-8);
        padding: var(--space-10) var(--space-12);
        border-bottom: 1px solid var(--border);
      }

      .show-hash {
        flex: none;
        font-family: var(--font-mono);
        font-size: var(--fs-11);
        color: var(--text-faint);
      }

      .show-subject {
        flex: 1 1 auto;
        min-width: 0;
        overflow: hidden;
        text-overflow: ellipsis;
        white-space: nowrap;
        font-size: var(--fs-12);
        color: var(--text);
      }

      .show-close {
        flex: none;
        border: none;
        background: transparent;
        color: var(--text-muted);
        font-size: var(--fs-13);
        padding: 0 4px;
        cursor: pointer;
      }

      .show-close:hover {
        color: var(--danger);
      }

      .show-files {
        list-style: none;
        margin: 0;
        padding: var(--space-6) var(--space-12);
        display: flex;
        flex-wrap: wrap;
        gap: 2px var(--space-10);
        max-height: 96px;
        overflow-y: auto;
        border-bottom: 1px solid var(--border);
        font-family: var(--font-mono);
        font-size: var(--fs-10);
        color: var(--text-muted);
      }

      .show-file {
        display: inline-flex;
        gap: var(--space-6);
      }

      .show-diff {
        flex: 1 1 auto;
        min-height: 0;
        overflow: auto;
        padding: var(--space-8) var(--space-12);
      }
    `,
  ],
  host: {
    '(document:keydown.escape)': 'closeShow()',
  },
})
export class GitPanel {
  private readonly i18n = inject(I18nService);
  private readonly engine = inject(EngineClient);
  private readonly projects = inject(ProjectsStore);
  private readonly events = inject(EventsStore);
  private readonly destroyRef = inject(DestroyRef);
  private readonly session = inject(ChatSessionStore);

  readonly t = this.i18n.t.bind(this.i18n);

  /** Horizontal distance between graph lanes (exposed for the template). */
  readonly laneWidth = GIT_LANE_W;

  /** Project the open session belongs to (worktree sessions map back to their root). */
  readonly projectId = computed(() => this.matchProjectId(this.session.directory()));

  readonly status = signal<GitStatus | null>(null);
  readonly branches = signal<GitBranch[]>([]);
  readonly log = signal<GitLogEntry[]>([]);
  readonly loading = signal(false);
  readonly error = signal<string | null>(null);
  /** Transient success message (last commit hash, clone result, …). */
  readonly note = signal<string | null>(null);
  /** Which mutation is in flight (`null` = idle); disables the write buttons. */
  readonly busy = signal<string | null>(null);

  /** Commit currently shown in the diff overlay (null = closed). */
  readonly shown = signal<GitShow | null>(null);

  /** Files grouped for display: staged, unstaged, untracked (engine order kept). */
  readonly groups = computed<GitFileGroup[]>(() => groupGitFiles(this.status()?.files ?? []));

  /** Log rows with lane data and SVG segments, newest first. */
  readonly graphRows = computed<GitGraphRow[]>(() => {
    const entries = this.log();
    const rows = computeLanes(
      entries.map((e) => e.parents),
      entries.map((e) => e.hash),
    );
    return entries.map((entry, i) => {
      const row = rows[i];
      return {
        entry,
        row,
        segments: laneSegments(row),
        width: Math.max(row.width, 1) * GIT_LANE_W,
        height: GIT_ROW_H,
        nodeX: row.lane * GIT_LANE_W + GIT_LANE_W / 2,
        nodeY: GIT_ROW_H / 2,
      };
    });
  });

  // -- write state ---------------------------------------------------------

  readonly commitMessage = signal('');
  readonly commitAmend = signal(false);
  readonly lastCommit = signal<string | null>(null);

  readonly resetMode = signal<GitResetMode>('mixed');
  readonly resetRev = signal('HEAD');
  readonly resetConfirm = signal(false);

  readonly stashes = signal<GitStashEntry[]>([]);
  readonly stashMessage = signal('');
  readonly stashConfirm = signal(false);

  readonly conflicts = signal<GitConflict[]>([]);

  readonly cherryRev = signal('');

  readonly remotes = signal<GitRemote[]>([]);
  readonly remoteName = signal('origin');
  readonly remoteUrl = signal('');
  readonly syncRefspec = signal('');

  readonly configKey = signal('user.name');
  readonly configValue = signal('');
  readonly configScope = signal<GitConfigScope>('local');
  readonly configConfirm = signal(false);
  /** Last value read with `git config <key>`; `null` = not set. */
  readonly configRead = signal<string | null>(null);

  readonly cloneSource = signal('');
  readonly cloneDest = signal('');
  readonly cloneBranch = signal('');

  readonly tools = signal<GitTools | null>(null);
  readonly forgeTool = signal<'gh' | 'glab'>('gh');
  readonly forgeArgs = signal('');
  readonly forgeOutput = signal<string | null>(null);

  readonly showBranchForm = signal(false);
  readonly branchName = signal('');
  readonly branchBase = signal('');
  readonly renameFrom = signal<string | null>(null);
  readonly renameTo = signal('');
  readonly deleteConfirm = signal(false);

  /** True when at least one forge CLI exists on the engine host. */
  readonly forgeAvailable = computed(() => {
    const tools = this.tools();
    return !!tools && (tools.gh || tools.glab);
  });

  // -- guards --------------------------------------------------------------

  readonly canStageAll = computed(
    () => this.busy() === null && this.groups().some((g) => g.key !== 'staged'),
  );

  readonly canUnstageAll = computed(() => {
    const status = this.status();
    return this.busy() === null && !!status && status.files.some((f) => f.staged);
  });

  readonly canCommit = computed(() => {
    if (this.busy() !== null) {
      return false;
    }
    const message = this.commitMessage().trim();
    if (message.length === 0) {
      return false;
    }
    // An amend reuses the previous commit's tree, so it needs no staged rows.
    return this.commitAmend() || (this.status()?.files ?? []).some((f) => f.staged);
  });

  readonly canReset = computed(() => {
    if (this.busy() !== null || this.resetRev().trim().length === 0) {
      return false;
    }
    return this.resetMode() !== 'hard' || this.resetConfirm();
  });

  readonly canCherryPick = computed(
    () => this.busy() === null && this.cherryRev().trim().length > 0,
  );

  readonly canSetRemoteUrl = computed(() => {
    const name = this.remoteName().trim();
    const url = this.remoteUrl().trim();
    return this.busy() === null && name.length > 0 && url.length > 0;
  });

  readonly canSyncFetch = computed(
    () => this.busy() === null && this.remoteName().trim().length > 0,
  );

  readonly canSyncPush = computed(() => {
    const refspec = this.syncRefspec().trim();
    return this.busy() === null && this.remoteName().trim().length > 0 && refspec.length > 0;
  });

  readonly canConfigGet = computed(
    () => this.busy() === null && this.configKey().trim().length > 0,
  );

  readonly canConfigSet = computed(() => {
    if (!this.canConfigGet()) {
      return false;
    }
    // A global write changes the user's whole machine: always confirmed here.
    return this.configScope() !== 'global' || this.configConfirm();
  });

  readonly canClone = computed(
    () =>
      this.busy() === null &&
      this.cloneSource().trim().length > 0 &&
      this.cloneDest().trim().length > 0,
  );

  readonly canRunForge = computed(() => this.busy() === null && this.forgeArgs().trim().length > 0);

  readonly canCreateBranch = computed(
    () => this.busy() === null && this.branchName().trim().length > 0,
  );

  readonly canRenameBranch = computed(
    () => this.busy() === null && !!this.renameFrom() && this.renameTo().trim().length > 0,
  );

  private loadedFor: string | null = null;
  private debounce: ReturnType<typeof setTimeout> | null = null;

  constructor() {
    effect(() => {
      const id = this.projectId();
      // Re-read when the project changes and whenever a turn starts/settles.
      this.session.running();
      if (!id) {
        this.loadedFor = null;
        this.status.set(null);
        this.branches.set([]);
        this.log.set([]);
        this.error.set(null);
        this.shown.set(null);
        return;
      }
      if (this.loadedFor !== id) {
        this.loadedFor = id;
        this.status.set(null);
        this.branches.set([]);
        this.log.set([]);
        this.shown.set(null);
        this.note.set(null);
        this.lastCommit.set(null);
        this.forgeOutput.set(null);
      }
      untracked(() => void this.refresh());
    });

    const unsubscribe = this.events.onEvent((ev) => this.handleEvent(ev));
    this.destroyRef.onDestroy(() => {
      unsubscribe();
      if (this.debounce) {
        clearTimeout(this.debounce);
      }
    });
  }

  refresh(): void {
    const id = this.projectId();
    if (id) {
      void this.load(id);
    }
  }

  // -- small template helpers ---------------------------------------------

  /** Directory part of a path including the trailing `/` ('' for a root file). */
  dirOf(path: string): string {
    const i = path.lastIndexOf('/');
    return i < 0 ? '' : path.slice(0, i + 1);
  }

  /** Basename of a path. */
  nameOf(path: string): string {
    return path.slice(path.lastIndexOf('/') + 1);
  }

  /** Row tooltip: the path, plus the rename source when the engine reported one. */
  fileTitle(file: GitFileStatus): string {
    return file.renamed_from ? `${file.renamed_from} → ${file.path}` : file.path;
  }

  /** First 7 characters of a commit hash (full hash stays in the row). */
  shortHash(hash: string): string {
    return hash.slice(0, 7);
  }

  /** `event.target.value` for an input/textarea (templates stay cast-free). */
  valueOf(event: Event): string {
    return (event.target as HTMLInputElement | HTMLTextAreaElement | null)?.value ?? '';
  }

  /** `event.target.checked` for a checkbox. */
  checkedOf(event: Event): boolean {
    return (event.target as HTMLInputElement | null)?.checked ?? false;
  }

  /** Narrow a `<select>` value to a reset mode. */
  resetModeOf(event: Event): GitResetMode {
    const value = this.valueOf(event);
    return value === 'soft' || value === 'hard' ? value : 'mixed';
  }

  /** Narrow a `<select>` value to a config scope. */
  configScopeOf(event: Event): GitConfigScope {
    return this.valueOf(event) === 'global' ? 'global' : 'local';
  }

  /** Narrow a `<select>` value to a forge tool. */
  forgeToolOf(event: Event): 'gh' | 'glab' {
    return this.valueOf(event) === 'glab' ? 'glab' : 'gh';
  }

  /** Delete needs an explicit confirm only for the forced variant. */
  canDeleteBranch(name: string): boolean {
    return this.busy() === null && (this.deleteConfirm() || this.branchIsMerged(name));
  }

  toggleBranchForm(): void {
    this.showBranchForm.update((open) => !open);
  }

  startRename(name: string, event: Event): void {
    event.stopPropagation();
    this.renameFrom.set(name);
    this.renameTo.set(name);
  }

  // -- mutations -----------------------------------------------------------

  async stageFile(file: GitFileStatus): Promise<void> {
    await this.mutate('stage', (id) => this.engine.gitStage(id, [file.path]));
  }

  async unstageFile(file: GitFileStatus): Promise<void> {
    await this.mutate('unstage', (id) => this.engine.gitUnstage(id, [file.path]));
  }

  async stageAll(): Promise<void> {
    const paths = (this.status()?.files ?? []).filter((f) => !f.staged && (f.unstaged || f.untracked));
    if (paths.length === 0) {
      return;
    }
    await this.mutate('stage', (id) =>
      this.engine.gitStage(
        id,
        paths.map((f) => f.path),
      ),
    );
  }

  async unstageAll(): Promise<void> {
    const paths = (this.status()?.files ?? []).filter((f) => f.staged);
    if (paths.length === 0) {
      return;
    }
    await this.mutate('unstage', (id) =>
      this.engine.gitUnstage(
        id,
        paths.map((f) => f.path),
      ),
    );
  }

  async commit(): Promise<void> {
    if (!this.canCommit()) {
      this.error.set(this.t('git.commitEmpty'));
      return;
    }
    const message = this.commitMessage().trim();
    const amend = this.commitAmend();
    const hash = await this.mutate('commit', (id) =>
      this.engine.gitCommit(id, message, amend).then((res) => res.hash),
    );
    if (hash) {
      this.lastCommit.set(hash);
      this.note.set(`${this.t('git.commitDone')} ${this.shortHash(hash)}`);
      this.commitMessage.set('');
      this.commitAmend.set(false);
    }
  }

  async reset(): Promise<void> {
    if (!this.canReset()) {
      return;
    }
    const mode = this.resetMode();
    const rev = this.resetRev().trim();
    const confirm = mode === 'hard' && this.resetConfirm();
    await this.mutate('reset', (id) => this.engine.gitReset(id, mode, rev, confirm));
  }

  async stashPush(): Promise<void> {
    const message = this.stashMessage().trim();
    await this.mutate('stash', (id) => this.engine.gitStashPush(id, message || undefined));
    this.stashMessage.set('');
  }

  async stashAction(action: 'pop' | 'apply' | 'drop', index: number): Promise<void> {
    if (action === 'drop' && !this.stashConfirm()) {
      return;
    }
    await this.mutate('stash', (id) => this.engine.gitStashAction(id, action, index));
  }

  async checkoutBranch(branch: GitBranch): Promise<void> {
    if (branch.current) {
      return;
    }
    await this.mutate('checkout', (id) => this.engine.gitCheckout(id, branch.name));
  }

  async createBranch(): Promise<void> {
    if (!this.canCreateBranch()) {
      return;
    }
    const name = this.branchName().trim();
    const base = this.branchBase().trim();
    await this.mutate('branch', (id) => this.engine.gitBranchCreate(id, name, base || null));
    this.branchName.set('');
    this.branchBase.set('');
    this.showBranchForm.set(false);
  }

  async renameBranch(): Promise<void> {
    const from = this.renameFrom();
    if (!from || !this.canRenameBranch()) {
      return;
    }
    const to = this.renameTo().trim();
    await this.mutate('branch', (id) => this.engine.gitBranchRename(id, from, to));
    this.renameFrom.set(null);
    this.renameTo.set('');
  }

  async deleteBranch(name: string, event?: Event): Promise<void> {
    event?.stopPropagation();
    const force = !this.branchIsMerged(name);
    if (force && !this.deleteConfirm()) {
      return;
    }
    await this.mutate('branch', (id) => this.engine.gitBranchDelete(id, name, force, force));
  }

  async cherryPick(): Promise<void> {
    if (!this.canCherryPick()) {
      return;
    }
    const rev = this.cherryRev().trim();
    await this.mutate('cherry-pick', (id) => this.engine.gitCherryPick(id, rev));
    this.cherryRev.set('');
  }

  async cherryPickAbort(): Promise<void> {
    await this.mutate('cherry-pick', (id) => this.engine.gitCherryPickAbort(id));
  }

  async cherryPickContinue(): Promise<void> {
    await this.mutate('cherry-pick', (id) => this.engine.gitCherryPickContinue(id));
  }

  async setRemoteUrl(): Promise<void> {
    if (!this.canSetRemoteUrl()) {
      return;
    }
    const remote = this.remoteName().trim();
    const url = this.remoteUrl().trim();
    await this.mutate('remote', (id) => this.engine.gitSetRemoteUrl(id, remote, url));
    this.remoteUrl.set('');
  }

  async syncFetch(): Promise<void> {
    if (!this.canSyncFetch()) {
      return;
    }
    const remote = this.remoteName().trim();
    await this.mutate('fetch', (id) => this.engine.gitSyncFetch(id, remote));
  }

  async syncPull(): Promise<void> {
    if (!this.canSyncFetch()) {
      return;
    }
    const remote = this.remoteName().trim();
    const refspec = this.syncRefspec().trim();
    await this.mutate('pull', (id) => this.engine.gitSyncPull(id, remote, refspec || null));
  }

  async syncPush(): Promise<void> {
    if (!this.canSyncPush()) {
      return;
    }
    const remote = this.remoteName().trim();
    const refspec = this.syncRefspec().trim();
    await this.mutate('push', (id) => this.engine.gitSyncPush(id, remote, refspec));
  }

  async configGet(): Promise<void> {
    const id = this.projectId();
    if (!id || !this.canConfigGet()) {
      return;
    }
    const key = this.configKey().trim();
    this.busy.set('config');
    try {
      const res = await this.engine.gitConfigGet(id, key);
      this.configRead.set(res.value);
      if (res.value !== null) {
        this.configValue.set(res.value);
      }
      this.error.set(null);
    } catch (err) {
      this.error.set(describe(err));
    } finally {
      this.busy.set(null);
    }
  }

  async configSet(): Promise<void> {
    if (!this.canConfigSet()) {
      return;
    }
    const key = this.configKey().trim();
    const value = this.configValue();
    const scope = this.configScope();
    const confirm = scope === 'global' && this.configConfirm();
    await this.mutate('config', (id) => this.engine.gitConfigSet(id, key, value, scope, confirm));
  }

  async cloneRepo(): Promise<void> {
    if (!this.canClone()) {
      return;
    }
    const source = this.cloneSource().trim();
    const dest = this.cloneDest().trim();
    const branch = this.cloneBranch().trim();
    const result = await this.mutate('clone', () =>
      this.engine.gitClone(source, dest, branch || null).then((res) => res.path),
    );
    if (result) {
      this.note.set(`${this.t('git.cloneDone')} ${result}`);
      this.cloneSource.set('');
      this.cloneDest.set('');
      this.cloneBranch.set('');
      void this.projects.refresh();
    }
  }

  async runForge(): Promise<void> {
    if (!this.canRunForge()) {
      return;
    }
    const args = this.forgeArgs()
      .split(/\s+/)
      .filter((a) => a.length > 0);
    const tool = this.forgeTool();
    const output = await this.mutate('forge', (id) =>
      this.engine.gitForge(id, tool, args).then((res) => res.output),
    );
    if (output !== null) {
      this.forgeOutput.set(output);
    }
  }

  // -- commit diff overlay -------------------------------------------------

  /** Fetch the commit and open its diff overlay. */
  async openCommit(entry: GitLogEntry): Promise<void> {
    const id = this.projectId();
    if (!id) {
      return;
    }
    try {
      this.shown.set(await this.engine.gitShow(id, entry.hash));
    } catch (err) {
      this.error.set(describe(err));
    }
  }

  closeShow(): void {
    this.shown.set(null);
  }

  // -- internals -----------------------------------------------------------

  /**
   * Run one mutation with the shared busy/error/refresh cycle. The action's
   * resolved value is returned so callers can surface a hash or a path; a
   * failure yields `null` and shows the engine message inline.
   */
  private async mutate<T>(
    label: string,
    action: (id: string) => Promise<T>,
  ): Promise<T | null> {
    const id = this.projectId();
    if (!id || this.busy() !== null) {
      return null;
    }
    this.busy.set(label);
    try {
      const result = await action(id);
      this.error.set(null);
      await this.load(id);
      return result;
    } catch (err) {
      this.error.set(describe(err));
      return null;
    } finally {
      this.busy.set(null);
    }
  }

  /** A branch the log already shows merged into HEAD can be deleted unforced. */
  private branchIsMerged(name: string): boolean {
    return (this.branches().find((b) => b.name === name)?.behind ?? 0) === 0;
  }

  /**
   * Refresh triggers, in order of preference:
   * - `git.changed`: published by every mutating engine route (including the
   *   ones an agent's tool calls trigger), so a refresh is immediate;
   * - `message.part.updated` / `message.updated` while a turn runs: tool
   *   writes land mid-turn, so re-read after transcript updates (debounced).
   */
  private handleEvent(ev: EngineEvent): void {
    const id = this.projectId();
    if (!id) {
      return;
    }
    if (ev.type === 'git.changed') {
      if (this.debounce) {
        clearTimeout(this.debounce);
      }
      this.debounce = setTimeout(() => {
        this.debounce = null;
        if (this.projectId() === id) {
          void this.load(id);
        }
      }, REFRESH_DEBOUNCE_MS);
      return;
    }
    if (ev.sessionID !== this.session.meta()?.id || !this.session.running()) {
      return;
    }
    if (ev.type !== 'message.part.updated' && ev.type !== 'message.updated') {
      return;
    }
    if (this.debounce) {
      clearTimeout(this.debounce);
    }
    this.debounce = setTimeout(() => {
      this.debounce = null;
      if (this.projectId() === id) {
        void this.load(id);
      }
    }, REFRESH_DEBOUNCE_MS);
  }

  private async load(id: string): Promise<void> {
    if (!this.engine.connected()) {
      return;
    }
    this.loading.set(true);
    try {
      const [status, branches, log, stashes, conflicts, remotes, tools] = await Promise.all([
        this.engine.gitStatus(id),
        this.engine.gitBranches(id),
        this.engine.gitLog(id, GIT_LOG_PAGE),
        this.engine.gitStashList(id),
        this.engine.gitConflicts(id),
        this.engine.gitRemotes(id),
        this.engine.gitTools(id),
      ]);
      if (this.projectId() !== id) {
        return;
      }
      this.status.set(status);
      this.branches.set(branches.branches ?? []);
      this.log.set(log.entries ?? []);
      this.stashes.set(stashes.stashes ?? []);
      this.conflicts.set(conflicts.conflicts ?? []);
      this.remotes.set(remotes.remotes ?? []);
      this.tools.set(tools.tools ?? null);
      this.error.set(null);
    } catch (err) {
      if (this.projectId() === id) {
        this.error.set(describe(err));
      }
    } finally {
      this.loading.set(false);
    }
  }

  /** Registered project for a session directory (worktree dirs map to their root). */
  private matchProjectId(directory: string | null): string | null {
    if (!directory) {
      return null;
    }
    const direct = this.projects.findByPath(directory);
    if (direct) {
      return direct.id;
    }
    const normalized = directory.replace(/\\/g, '/');
    const marker = normalized.indexOf(WORKTREES_MARK);
    if (marker > 0) {
      return this.projects.findByPath(normalized.slice(0, marker))?.id ?? null;
    }
    return null;
  }
}
