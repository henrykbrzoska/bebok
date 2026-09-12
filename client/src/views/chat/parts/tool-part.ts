import { Component, computed, inject, input, linkedSignal } from '@angular/core';
import { RouterLink } from '@angular/router';

import { DiffViewComponent } from '../../../ui/diff-view/diff-view';
import {
  Part,
  TaskLink,
  ToolPart,
  ToolState,
  ToolStateCompleted,
  ToolStateError,
} from '../../../core/engine.dtos';
import { prettyJson, summarizeInput } from '../../../core/format';
import { I18nService } from '../../../i18n/i18n.service';

/**
 * Tool-call block (F2-6 / F2-9).
 *
 * Bordered card whose header is one clickable row: status dot (success /
 * accent / danger by outcome) + monospace tool name + truncated monospace
 * args + chevron. Expanding reveals the arguments and the result body on a
 * `--bg` panel. The first tool call of a turn starts expanded, every later one
 * collapsed - see `toolIndex`.
 */
@Component({
  selector: 'app-tool-part',
  imports: [DiffViewComponent, RouterLink],
  template: `
    <div class="tool" [class.failed]="kind() === 'error'">
      <button
        type="button"
        class="tool-head"
        (click)="detailsOpen.set(!detailsOpen())"
        [attr.aria-expanded]="detailsOpen()"
      >
        <span class="dot state-{{ kind() }}" aria-hidden="true"></span>
        <span class="tool-name">{{ name() }}</span>
        @if (isTask()) {
          <span class="badge delegation">{{ t('tool.delegation') }}</span>
        }
        <span class="tool-args">{{ summary() }}</span>
        <span class="state-label state-{{ kind() }}">{{ kind() }}</span>
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

      @if (detailsOpen()) {
        <div class="tool-details">
          <div class="section-label">{{ t('tool.arguments') }}</div>
          <pre class="panel"><code>{{ inputText() }}</code></pre>

          @switch (kind()) {
            @case ('completed') {
              @if (outputText()) {
                <div class="section-label">{{ t('tool.output') }}</div>
                <app-diff-view [text]="outputText()" />
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
    .tool.failed {
      border-color: var(--danger);
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

    .dot {
      width: 7px;
      height: 7px;
      border-radius: 50%;
      flex: none;
      background: var(--text-faint);
    }
    .dot.state-completed {
      background: var(--success);
    }
    .dot.state-running {
      background: var(--accent);
    }
    .dot.state-error {
      background: var(--danger);
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
  readonly t = this.i18n.t.bind(this.i18n);

  readonly part = input.required<Part>();
  /**
   * Ordinal of this tool call within its message. `0` (the first call of the
   * turn) opens expanded, every later one collapsed - the handoff's default.
   */
  readonly toolIndex = input(-1);
  /** Task name/ID → childSessionID map, passed down from the chat view. */
  readonly taskLinks = input<Map<string, string>>(new Map());

  readonly detailsOpen = linkedSignal(() => this.toolIndex() <= 0);

  private readonly toolPart = computed(() => this.part() as ToolPart);
  private readonly state = computed<ToolState>(() => this.toolPart().state);

  readonly name = computed(() => this.toolPart().name);
  readonly kind = computed(() => this.state().state);
  /** True for the sub-agent delegation tool (`task`). */
  readonly isTask = computed(() => this.toolPart().name === 'task');

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

  readonly inputText = computed(() => prettyJson(this.state().input));
  readonly summary = computed(() => summarizeInput(this.state().input));

  private readonly completed = computed<ToolStateCompleted | null>(() =>
    this.state().state === 'completed' ? (this.state() as ToolStateCompleted) : null,
  );
  private readonly failed = computed<ToolStateError | null>(() =>
    this.state().state === 'error' ? (this.state() as ToolStateError) : null,
  );

  readonly outputText = computed(() => this.completed()?.output ?? '');
  readonly errorText = computed(() => this.failed()?.error ?? '');
}
