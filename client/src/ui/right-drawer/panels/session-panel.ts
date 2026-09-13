/**
 * Right-drawer "Session" panel (F2-12).
 *
 * The stacked replacement for the old left-hand `ui/session-sidebar`: TOKENS
 * grid, COST, SUB-AGENTS and an "Active in this session" chip list of the
 * enabled MCP servers and skills
 * (chips toggle them, as the old sidebar's checkboxes did), plus the YOLO
 * switch. The numbers come from `ChatSessionStore`, which the chat view
 * publishes into - the panel never refetches the transcript.
 */

import {
  ChangeDetectionStrategy,
  Component,
  DestroyRef,
  computed,
  effect,
  inject,
  signal,
} from '@angular/core';
import { RouterLink } from '@angular/router';

import { EngineClient } from '../../../core/engine-client.service';
import { EngineEvent, McpStatus, ResolvedSkill, SessionMeta } from '../../../core/engine.dtos';
import { EventsStore } from '../../../core/events.store';
import { I18nService } from '../../../i18n/i18n.service';
import { ChatSessionStore } from '../../../views/chat/chat-session.store';

@Component({
  selector: 'app-session-panel',
  changeDetection: ChangeDetectionStrategy.OnPush,
  imports: [RouterLink],
  template: `
    @if (!session.meta()) {
      <div class="empty">{{ t('drawer.noSession') }}</div>
    } @else {
      <div class="panel-body">
        <section class="group">
          <h3 class="group-title">{{ t('drawer.tokens') }}</h3>
          <div class="token-grid">
            <div class="cell">
              <span class="cell-label">{{ t('drawer.tokensIn') }}</span>
              <span class="cell-value">{{ format(totals().input) }}</span>
            </div>
            <div class="cell">
              <span class="cell-label">{{ t('drawer.tokensCacheRead') }}</span>
              <span class="cell-value">{{ format(totals().cacheRead) }}</span>
            </div>
            <div class="cell">
              <span class="cell-label">{{ t('drawer.tokensOut') }}</span>
              <span class="cell-value">{{ format(totals().output) }}</span>
            </div>
            <div class="cell">
              <span class="cell-label">{{ t('drawer.tokensHitRate') }}</span>
              <span class="cell-value">{{ session.cacheRate() }}</span>
            </div>
          </div>
        </section>

        <section class="group">
          <h3 class="group-title">{{ t('drawer.context') }}</h3>
          @if (session.contextLabel()) {
            <div
              class="token-grid context"
              [class.warning]="session.contextLevel() === 'warning'"
              [class.danger]="session.contextLevel() === 'danger'"
              data-testid="context-meter"
            >
              <div class="cell">
                <span class="cell-label">{{ t('drawer.contextUsed') }}</span>
                <span class="cell-value context-value">{{ format(session.contextUsed() ?? 0) }}</span>
              </div>
              <div class="cell">
                <span class="cell-label">{{ t('drawer.contextWindow') }}</span>
                <span class="cell-value">{{ format(session.contextWindow()) }}</span>
              </div>
              <div class="cell wide">
                <span class="cell-label">{{ session.contextLabel() }}</span>
                <span class="context-bar" aria-hidden="true">
                  <span class="context-fill" [style.width.%]="contextFill()"></span>
                </span>
              </div>
            </div>
          } @else {
            <div class="none">{{ t('drawer.contextNone') }}</div>
          }
        </section>

        <section class="group">
          <h3 class="group-title">{{ t('drawer.cost') }}</h3>
          <div
            class="cost"
            [class.unknown]="session.totals().cost === null"
            [title]="session.totals().cost === null ? t('drawer.costUnknown') : ''"
          >{{ session.costLabel() }}</div>
        </section>

        <section class="group">
          <h3 class="group-title">{{ t('drawer.subagents') }}</h3>
          @if (subagents().length === 0) {
            <div class="none">{{ t('chrome.noSubagents') }}</div>
          } @else {
            <ul class="subagents">
              @for (child of subagents(); track child.id) {
                <li>
                  <a
                    class="subagent"
                    [routerLink]="['/chat', child.id]"
                    [title]="child.alias || child.title || child.id"
                  >
                    <span class="subagent-agent">{{ child.agent }}</span>
                    <span class="subagent-title">{{
                      child.alias || child.title || shortId(child.id)
                    }}</span>
                  </a>
                </li>
              }
            </ul>
          }
        </section>

        @if (models().length > 1) {
          <section class="group">
            <h3 class="group-title">{{ t('chrome.models') }}</h3>
            <div class="chips">
              <button
                type="button"
                class="chip"
                [class.on]="!session.filterModel()"
                (click)="session.filterModel.set(null)"
              >{{ t('chrome.all') }}</button>
              @for (model of models(); track model) {
                <button
                  type="button"
                  class="chip"
                  [class.on]="session.filterModel() === model"
                  (click)="session.filterModel.set(model)"
                  [title]="model"
                >{{ model }}</button>
              }
            </div>
          </section>
        }

        <section class="group">
          <h3 class="group-title">{{ t('drawer.activeHere') }}</h3>
          @if (mcp().length === 0 && skills().length === 0) {
            <div class="none">{{ t('drawer.nothingActive') }}</div>
          } @else {
            <div class="chips">
              @for (server of mcp(); track server.name) {
                <button
                  type="button"
                  class="chip tag"
                  [class.on]="server.enabled"
                  (click)="toggleMcp(server)"
                  [title]="server.name + ' · ' + (server.enabled ? t('drawer.enabled') : t('drawer.disabled'))"
                >
                  <span class="chip-kind">mcp</span>{{ server.name }}
                </button>
              }
              @for (skill of skills(); track skill.name) {
                <button
                  type="button"
                  class="chip tag"
                  [class.on]="skill.enabled"
                  (click)="toggleSkill(skill)"
                  [title]="skill.name + ' · ' + (skill.enabled ? t('drawer.enabled') : t('drawer.disabled'))"
                >
                  <span class="chip-kind">skill</span>{{ skill.name }}
                </button>
              }
            </div>
          }
        </section>

        <section class="group">
          <label class="yolo">
            <input type="checkbox" [checked]="yolo()" (change)="toggleYolo(!yolo())" />
            <span class="yolo-label">{{ t('drawer.yolo') }}</span>
          </label>
          <p class="yolo-hint">{{ t('drawer.yoloHint') }}</p>
        </section>
      </div>
    }
  `,
  styles: [
    `
      .empty,
      .none {
        padding: var(--space-6) 0;
        font-size: var(--fs-11-5);
        color: var(--text-faint);
      }

      .empty {
        padding: var(--space-16);
      }

      .panel-body {
        display: flex;
        flex-direction: column;
        gap: var(--space-14);
        padding: var(--space-12) var(--space-14) var(--space-16);
      }

      .group-title {
        margin: 0 0 var(--space-6);
        font-size: var(--fs-label);
        font-weight: 600;
        text-transform: uppercase;
        letter-spacing: var(--label-tracking);
        color: var(--text-faint);
      }

      .token-grid {
        display: grid;
        grid-template-columns: 1fr 1fr;
        gap: var(--space-6);
      }

      .cell {
        display: flex;
        flex-direction: column;
        gap: 1px;
        padding: var(--space-6) var(--space-8);
        background: var(--surface-2);
        border: 1px solid var(--border);
        border-radius: var(--radius-control-sm);
        min-width: 0;
      }

      .cell-label {
        font-size: 10px;
        text-transform: uppercase;
        letter-spacing: var(--label-tracking);
        color: var(--text-faint);
      }

      .cell-value {
        font-family: var(--font-mono);
        font-size: var(--fs-12-5);
        color: var(--text);
      }

      .cell.wide {
        grid-column: 1 / -1;
      }

      .context-bar {
        display: block;
        height: 6px;
        margin-top: 3px;
        border-radius: 3px;
        background: var(--surface-3);
        border: 1px solid var(--border);
        overflow: hidden;
      }

      .context-fill {
        display: block;
        height: 100%;
        background: var(--accent);
        transition: width 0.3s ease;
      }

      .context.warning .context-fill {
        background: var(--warning);
      }

      .context.warning .context-value {
        color: var(--warning);
      }

      .context.danger .context-fill {
        background: var(--danger);
      }

      .context.danger .context-value {
        color: var(--danger);
      }

      .cost {
        font-family: var(--font-mono);
        font-size: var(--fs-20);
        font-weight: 600;
        color: var(--text);
      }

      /* F6-5: unknown pricing renders "—", muted rather than as a figure. */
      .cost.unknown {
        color: var(--text-faint);
        font-weight: 400;
      }

      .subagents {
        list-style: none;
        margin: 0;
        padding: 0;
        display: flex;
        flex-direction: column;
        gap: 3px;
      }

      .subagent {
        display: flex;
        align-items: baseline;
        gap: var(--space-6);
        padding: 3px var(--space-6);
        border-radius: var(--radius-control-sm);
        border: 1px solid transparent;
        color: inherit;
        text-decoration: none;
      }

      .subagent:hover {
        background: var(--surface-2);
        border-color: var(--border);
      }

      .subagent-agent {
        font-family: var(--font-mono);
        font-size: var(--fs-11);
        color: var(--accent);
        flex: none;
      }

      .subagent-title {
        font-size: var(--fs-11-5);
        color: var(--text-muted);
        overflow: hidden;
        text-overflow: ellipsis;
        white-space: nowrap;
      }

      .chips {
        display: flex;
        flex-wrap: wrap;
        gap: 4px;
      }

      .chip {
        display: inline-flex;
        align-items: center;
        gap: 4px;
        max-width: 100%;
        padding: 2px var(--space-8);
        border-radius: var(--radius-bubble);
        border: 1px solid var(--border);
        background: transparent;
        color: var(--text-faint);
        font-family: var(--font-mono);
        font-size: var(--fs-11);
        overflow: hidden;
        text-overflow: ellipsis;
        white-space: nowrap;
      }

      .chip.on {
        background: var(--surface-3);
        border-color: var(--border-strong);
        color: var(--text);
      }

      .chip-kind {
        font-size: 9.5px;
        text-transform: uppercase;
        letter-spacing: var(--label-tracking);
        color: var(--accent);
      }

      .yolo {
        display: flex;
        align-items: center;
        gap: var(--space-6);
        cursor: pointer;
      }

      .yolo-label {
        font-size: var(--fs-11-5);
        font-weight: 600;
        color: var(--warning);
        text-transform: uppercase;
        letter-spacing: var(--label-tracking);
      }

      .yolo-hint {
        margin: 3px 0 0;
        font-size: var(--fs-11);
        color: var(--text-faint);
        line-height: 1.45;
      }
    `,
  ],
})
export class SessionPanel {
  private readonly i18n = inject(I18nService);
  private readonly engine = inject(EngineClient);
  private readonly events = inject(EventsStore);
  private readonly destroyRef = inject(DestroyRef);
  readonly session = inject(ChatSessionStore);

  readonly t = this.i18n.t.bind(this.i18n);

  readonly totals = this.session.totals;
  readonly models = computed(() => this.session.modelsUsed());
  /** F6-3: bar width for the context meter (clamped to 0..100). */
  readonly contextFill = computed(() =>
    Math.min(100, Math.max(0, this.session.contextPercent() ?? 0)),
  );

  /** Sub-agent sessions delegated from this session (via the `task` tool). */
  readonly subagents = signal<SessionMeta[]>([]);
  readonly mcp = signal<McpStatus[]>([]);
  readonly skills = signal<ResolvedSkill[]>([]);
  readonly yolo = signal(false);

  constructor() {
    effect(() => {
      const dir = this.session.directory();
      if (dir) {
        void this.loadEnvironment(dir);
      }
    });
    // Sub-agents are created during a turn: refresh when the turn settles.
    effect(() => {
      const meta = this.session.meta();
      this.session.running();
      if (meta?.directory) {
        void this.loadSubagents(meta.id, meta.directory);
      }
    });
    const unsubscribe = this.events.onEvent((ev) => this.handleEvent(ev));
    this.destroyRef.onDestroy(unsubscribe);
  }

  /** Thousands separator for the token cells. */
  format(value: number): string {
    return value.toLocaleString();
  }

  /** First 8 chars of a UUID (title fallback for sub-agent links). */
  shortId(id: string): string {
    return id.slice(0, 8);
  }

  private handleEvent(ev: EngineEvent): void {
    const meta = this.session.meta();
    if (!meta?.id || !meta.directory || ev.directory !== meta.directory) {
      return;
    }
    if (ev.type === 'session.created') {
      const created = ev.properties?.['session'] as SessionMeta | undefined;
      const parentId = Array.isArray(created?.parent) ? created.parent[0] : null;
      if (created && parentId === meta.id && this.session.meta()?.id === meta.id) {
        this.subagents.update((list) =>
          list.some((s) => s.id === created.id) ? list : [...list, created],
        );
      }
    } else if (ev.type === 'task.started' && ev.sessionID === meta.id) {
      const childId =
        ev.properties?.['childSessionID'] ?? ev.properties?.['child_session_id'];
      if (typeof childId === 'string' && childId && !this.subagents().some((s) => s.id === childId)) {
        void this.loadSubagents(meta.id, meta.directory);
      }
    }
  }

  private async loadSubagents(parentId: string, dir: string): Promise<void> {
    try {
      const sessions = await this.engine.listSessions(dir);
      if (this.session.meta()?.id !== parentId) {
        return;
      }
      this.subagents.set(sessions.filter((s) => s.parent?.[0] === parentId));
    } catch {
      /* sub-agent list is non-critical */
    }
  }

  private async loadEnvironment(dir: string): Promise<void> {
    try {
      const cfg = await this.engine.getConfig(dir);
      this.mcp.set(cfg.mcp);
      this.skills.set(cfg.skills);
      this.yolo.set(!!cfg.config.yolo);
    } catch {
      /* environment list is non-critical */
    }
  }

  async toggleMcp(server: McpStatus): Promise<void> {
    const dir = this.session.directory();
    if (!dir) {
      return;
    }
    try {
      this.mcp.set(await this.engine.toggleMcp(dir, server.name, !server.enabled));
    } catch {
      /* keep last state */
    }
  }

  async toggleSkill(skill: ResolvedSkill): Promise<void> {
    const dir = this.session.directory();
    if (!dir) {
      return;
    }
    try {
      await this.engine.putConfig(dir, { skills: { [skill.name]: !skill.enabled } });
      await this.loadEnvironment(dir);
    } catch {
      /* keep last state */
    }
  }

  async toggleYolo(enabled: boolean): Promise<void> {
    const dir = this.session.directory();
    if (!dir) {
      return;
    }
    try {
      await this.engine.putConfig(dir, { yolo: enabled });
      this.yolo.set(enabled);
    } catch {
      /* keep last state */
    }
  }
}
