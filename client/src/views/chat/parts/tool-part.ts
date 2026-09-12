import { Component, computed, inject, input, linkedSignal } from '@angular/core';
import { RouterLink } from '@angular/router';

import { DiffViewComponent } from '../../../ui/diff-view/diff-view';
import {
  Part,
  SafetyCategory,
  TaskLink,
  ToolPart,
  ToolState,
  ToolStateCompleted,
  ToolStateError,
  safetyCategory,
} from '../../../core/engine.dtos';
import { formatBytes, prettyJson, toolCallPreview } from '../../../core/format';
import { LiveTaskEntry, TaskProgressStore } from '../../../core/task-progress.store';
import { TaskProgressLine } from '../../../ui/task-progress-line/task-progress-line';
import { ChatSessionStore } from '../chat-session.store';
import { ToolSafetyStore } from '../../../core/tool-safety.store';
import { UiPrefsStore } from '../../../core/ui-prefs.store';
import { I18nService } from '../../../i18n/i18n.service';
import { safetyLegend } from './safety-legend';

/**
 * Tool-call block (F2-6 / F2-9).
 *
 * Bordered card whose header is one clickable row: status dot (success /
 * accent / danger by outcome) + monospace tool name + truncated monospace
 * args + state label + chevron. Expanding reveals the arguments and the
 * result body on a `--bg` panel.
 *
 * F6-1b: every tool call - single or grouped, first in the turn or not -
 * starts collapsed. The only way a call starts open is the "Expand tool
 * calls by default" preference (`UiPrefsStore.expandToolCallsByDefault`),
 * which opens every call at once. A collapsed call is a single ~28px line:
 * status dot + mono tool name + truncated key-argument preview + state label
 * (RUNNING/COMPLETED/FAILED, uppercased by CSS) + chevron. No duration is
 * shown: the engine's `ToolState` carries `started_at` only while running and
 * no end timestamp once completed, so there is nothing truthful to display;
 * a completed call's output size is appended to the preview instead.
 */
@Component({
  selector: 'app-tool-part',
  imports: [DiffViewComponent, RouterLink, TaskProgressLine],
  template: `
    <div class="tool" [class.failed]="kind() === 'error'">
      <button
        type="button"
        class="tool-head"
        [class.collapsed]="!detailsOpen()"
        (click)="detailsOpen.set(!detailsOpen())"
        [attr.aria-expanded]="detailsOpen()"
        [title]="detailsOpen() ? t('tool.collapseCall') : t('tool.expandCall')"
      >
        <span
          class="dot safety-{{ safety() }}"
          [class.pulse]="kind() === 'running'"
          [class.ring-failed]="kind() === 'error'"
          [title]="safetyTitle()"
          [attr.data-safety]="safety()"
          aria-hidden="true"
        ></span>
        <span class="tool-name">{{ name() }}</span>
        @if (isTask()) {
          <span class="badge delegation">{{ t('tool.delegation') }}</span>
        }
        <span class="tool-args">{{ summary() }}{{ sizeSuffix() }}</span>
        <span class="state-label state-{{ kind() }}">{{ stateLabel() }}</span>
        <span class="chevron" aria-hidden="true">{{ detailsOpen() ? '▾' : '▸' }}</span>
      </button>

      @if (isTask()) {
        <div class="task-row">
          @if (taskLink()) {
            <a
              class="task-target"
              [routerLink]="['/chat', taskLink()!]"
              [title]="t('tool.openSubagent')"
            >{{ taskName() }}</a>
          } @else {
            <span class="task-target muted">{{ taskName() }}</span>
          }
        </div>
      }

      <!-- WP-DELEGATION: live one-line progress of the child(ren) while the
           delegation call is still running (fed by task.progress events). -->
      @for (live of liveChildren(); track live.taskID) {
        <div class="progress-row" data-testid="task-progress-row">
          @if (liveChildren().length > 1 || !isTask()) {
            <span class="progress-name">{{ live.name }}</span>
          }
          <app-task-progress-line
            class="progress-line"
            [status]="live.status"
            [progress]="live.progress"
            [tokens]="live.tokens.input + live.tokens.output"
          />
        </div>
      }

      @if (detailsOpen()) {
        <div class="tool-details">
          <div class="section-label">{{ t('tool.arguments') }}</div>
          <pre class="panel"><code>{{ inputText() }}</code></pre>

          @switch (kind()) {
            @case ('completed') {
              @if (outputText()) {
                <div class="section-label">{{ t('tool.output') }}</div>
                <app-diff-view [text]="outputText()" [fileLabel]="filePath()" />
              }
            }
            @case ('error') {
              @if (errorText()) {
                <div class="section-label danger">{{ t('tool.error') }}</div>
                <pre class="panel error"><code>{{ errorText() }}</code></pre>
              }
            }
            @case ('running') {
              <div class="running-note">{{ t('tool.running') }}</div>
            }
          }
        </div>
      }
    </div>
  `,
  styles: `
    .tool {
      border: 1px solid var(--border);
      border-radius: var(--radius-panel);
      background: var(--surface);
      overflow: hidden;
    }
    /* F2-9: a failed tool call is a danger-bordered, red-tinted card. */
    .tool.failed {
      border-color: var(--danger);
      background: rgba(226, 100, 95, 0.06);
    }
    .tool.failed .tool-name {
      color: var(--diff-remove-text);
    }
    .tool.failed .tool-args {
      color: var(--diff-remove-text);
      opacity: 0.85;
    }

    .tool-head {
      display: flex;
      align-items: center;
      gap: var(--space-8);
      width: 100%;
      background: none;
      border: none;
      border-radius: 0;
      padding: var(--space-8) var(--space-12);
      cursor: pointer;
      text-align: left;
      min-width: 0;
    }
    .tool-head:hover {
      background: var(--surface-2);
    }
    /* F6-1: a collapsed call is one dense ~28px line. */
    .tool-head.collapsed {
      padding-top: 5px;
      padding-bottom: 5px;
      min-height: 28px;
    }

    /* F7-7: the dot's fill is the tool's explicit safety category
       (safe / caution / dangerous / uncategorized), not the run state - the
       state stays visible as the uppercase text label next to it. */
    .dot {
      width: 7px;
      height: 7px;
      border-radius: 50%;
      flex: none;
      background: var(--text-faint);
    }
    .dot.safety-safe {
      background: var(--success);
    }
    .dot.safety-caution {
      background: var(--warning);
    }
    .dot.safety-dangerous {
      background: var(--accent);
    }
    .dot.safety-uncategorized {
      background: var(--text-faint);
    }
    .dot.pulse {
      animation: tool-dot-pulse 1.4s ease-in-out infinite;
    }
    /* F7-1: a failed call keeps a red ring regardless of its safety colour. */
    .dot.ring-failed {
      box-shadow: 0 0 0 2px var(--danger);
    }
    @keyframes tool-dot-pulse {
      0%, 100% { opacity: 1; }
      50% { opacity: 0.35; }
    }

    .tool-name {
      font-family: var(--font-mono);
      font-size: var(--fs-12-5);
      font-weight: 600;
      color: var(--text);
      flex: none;
    }

    .tool-args {
      flex: 1 1 auto;
      min-width: 0;
      font-family: var(--font-mono);
      font-size: var(--fs-11-5);
      color: var(--text-muted);
      overflow: hidden;
      text-overflow: ellipsis;
      white-space: nowrap;
    }

    .state-label {
      flex: none;
      font-size: var(--fs-11);
      letter-spacing: var(--label-tracking);
      text-transform: uppercase;
      color: var(--text-faint);
    }
    .state-label.state-completed {
      color: var(--success);
    }
    .state-label.state-running {
      color: var(--accent);
    }
    .state-label.state-error {
      color: var(--danger);
    }

    .chevron {
      flex: none;
      font-size: 10px;
      color: var(--text-faint);
    }

    .badge {
      flex: none;
      font-size: var(--fs-11);
      text-transform: uppercase;
      letter-spacing: var(--label-tracking);
      padding: 1px 7px;
      border-radius: 20px;
      border: 1px solid var(--accent);
      color: var(--accent);
    }

    .task-row {
      padding: 0 var(--space-12) var(--space-8);
      font-size: var(--fs-12);
    }
    .task-target {
      font-family: var(--font-mono);
      font-size: var(--fs-11-5);
      font-weight: 600;
      color: var(--accent);
      text-decoration: none;
    }
    a.task-target:hover {
      text-decoration: underline;
    }
    .muted {
      color: var(--text-muted);
    }

    .progress-row {
      display: flex;
      align-items: center;
      gap: var(--space-8);
      min-width: 0;
      padding: 0 var(--space-12) var(--space-6);
    }
    .progress-name {
      flex: none;
      font-family: var(--font-mono);
      font-size: var(--fs-11);
      color: var(--accent);
    }
    .progress-line {
      flex: 1 1 auto;
      min-width: 0;
    }

    .tool-details {
      padding: 0 var(--space-12) var(--space-12);
      display: flex;
      flex-direction: column;
      gap: var(--space-6);
    }

    .section-label {
      font-size: var(--fs-11);
      text-transform: uppercase;
      letter-spacing: var(--label-tracking);
      color: var(--text-faint);
    }
    .section-label.danger {
      color: var(--danger);
    }

    .panel {
      background: var(--bg);
      border: 1px solid var(--border);
      border-radius: var(--radius-control-sm);
      padding: 8px 10px;
      overflow-x: auto;
      white-space: pre-wrap;
      word-break: break-word;
      margin: 0;
      font-size: var(--fs-12);
      line-height: 1.5;
      color: var(--code-text-strong);
      max-height: 420px;
      overflow-y: auto;
    }
    .panel.error {
      color: var(--diff-remove-text);
      border-color: var(--danger);
    }

    .running-note {
      font-size: var(--fs-12);
      color: var(--text-muted);
    }
  `,
})
export class ToolPartComponent {
  private readonly i18n = inject(I18nService);
  private readonly prefs = inject(UiPrefsStore);
  private readonly toolSafety = inject(ToolSafetyStore);
  private readonly liveTasks = inject(TaskProgressStore);
  private readonly session = inject(ChatSessionStore);
  readonly t = this.i18n.t.bind(this.i18n);

  readonly part = input.required<Part>();
  /**
   * Ordinal of this tool call within its message. No longer affects the
   * default open state (F6-1b: every call starts collapsed) - kept for
   * numbering/debugging and to stay a stable input for callers.
   */
  readonly toolIndex = input(-1);
  /** Task name/ID → childSessionID map, passed down from the chat view. */
  readonly taskLinks = input<Map<string, string>>(new Map());

  /**
   * Default open state (F6-1b): every call starts collapsed unless the
   * "expand tool calls by default" preference is on, in which case every
   * call starts expanded. A user toggle overrides the default until the
   * preference changes.
   */
  readonly detailsOpen = linkedSignal(() => this.prefs.expandToolCallsByDefault());

  private readonly toolPart = computed(() => this.part() as ToolPart);
  private readonly state = computed<ToolState>(() => this.toolPart().state);

  readonly name = computed(() => this.toolPart().name);
  readonly kind = computed(() => this.state().state);
  /** True for the sub-agent delegation tool (`task`). */
  readonly isTask = computed(() => this.toolPart().name === 'task');
  /** True for the parallel fan-out tool (`fleet`). */
  readonly isFleet = computed(() => this.toolPart().name === 'fleet');

  /**
   * WP-DELEGATION: the child task(s) this call spawned, while it is still
   * running. A `task` call is matched by its `name`/`prompt` arguments; a
   * `fleet` call (one per turn, never parallel) shows every live child.
   */
  readonly liveChildren = computed<LiveTaskEntry[]>(() => {
    if (this.kind() !== 'running') {
      return [];
    }
    if (this.isTask()) {
      const match = this.liveTasks.matchTaskCall(
        this.state().input as Record<string, unknown> | undefined,
        this.session.meta()?.id,
      );
      return match ? [match] : [];
    }
    if (this.isFleet()) {
      const sessionID = this.session.meta()?.id;
      return sessionID ? this.liveTasks.liveForSession(sessionID) : [];
    }
    return [];
  });

  /** Display name for the delegated task (from structured metadata or input). */
  readonly taskName = computed(() => {
    const completed = this.completed();
    const structured = completed?.structured as TaskLink | undefined;
    if (structured?.name) {
      return structured.name;
    }
    const input = this.state().input as Record<string, unknown> | undefined;
    const name = input?.['name'];
    if (typeof name === 'string' && name.trim()) {
      return name.trim();
    }
    const agent = input?.['agent'];
    if (typeof agent === 'string' && agent.trim()) {
      return agent.trim();
    }
    return structured?.agent ?? 'code';
  });

  /** Child session ID for a task link (from structured metadata or the parent map). */
  readonly taskLink = computed<string | null>(() => {
    const completed = this.completed();
    const structured = completed?.structured as TaskLink | undefined;
    if (structured?.childSessionID) {
      return structured.childSessionID;
    }
    // Fall back to the parent-provided name→id map.
    const name = this.taskName();
    return this.taskLinks().get(name) ?? null;
  });

  /** `path` argument, shown as the diff block's title when present. */
  readonly filePath = computed(() => {
    const input = this.state().input as Record<string, unknown> | undefined;
    const path = input?.['path'] ?? input?.['file'] ?? input?.['file_path'];
    return typeof path === 'string' ? path : '';
  });

  /**
   * F7-7 safety category for the header dot: the engine-stamped category,
   * else (historical part) the category the engine reports for the tool
   * name now, else `uncategorized`.
   */
  readonly safety = computed<SafetyCategory>(() =>
    safetyCategory(this.toolPart(), (name) => this.toolSafety.categoryOf(name)),
  );
  /** "<category label> · <legend>" tooltip on the dot. */
  readonly safetyTitle = computed(() => safetyLegend(this.t, this.safety()));

  readonly inputText = computed(() => prettyJson(this.state().input));
  readonly summary = computed(() => toolCallPreview(this.name(), this.state().input));
  /** RUNNING/COMPLETED/FAILED (uppercased by CSS) - `error` reads as "failed". */
  readonly stateLabel = computed(() => (this.kind() === 'error' ? 'failed' : this.kind()));
  /** Appended to the collapsed preview once a call has completed, e.g. " · 1.2 kB". */
  readonly sizeSuffix = computed(() => {
    const bytes = this.outputText().length;
    return this.kind() === 'completed' && bytes > 0 ? ` · ${formatBytes(bytes)}` : '';
  });

  private readonly completed = computed<ToolStateCompleted | null>(() =>
    this.state().state === 'completed' ? (this.state() as ToolStateCompleted) : null,
  );
  private readonly failed = computed<ToolStateError | null>(() =>
    this.state().state === 'error' ? (this.state() as ToolStateError) : null,
  );

  readonly outputText = computed(() => this.completed()?.output ?? '');
  readonly errorText = computed(() => this.failed()?.error ?? '');
}
