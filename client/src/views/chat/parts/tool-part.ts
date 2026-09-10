import { Component, computed, inject, input, signal } from '@angular/core';
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

@Component({
  selector: 'app-tool-part',
  imports: [DiffViewComponent, RouterLink],
  template: `
    <div class="tool" [class.closed]="isClosed()">
      <div class="tool-head">
        <span class="tool-name">{{ name() }}</span>
        @if (isTask()) {
          <span class="badge delegation">{{ t('tool.delegation') }}</span>
          @if (taskLink()) {
            <a
              class="task-target muted"
              [routerLink]="['/chat', taskLink()!]"
              [title]="t('tool.openSubagent')"
            >{{ taskName() }}</a>
          } @else {
            <span class="task-target muted">{{ taskName() }}</span>
          }
        }
        <span class="badge state-{{ kind() }}">{{ kind() }}</span>
        <button class="expand" (click)="detailsOpen.set(!detailsOpen())">
          {{ detailsOpen() ? t('tool.collapse') : t('tool.expand') }}
        </button>
      </div>

      @if (summary()) {
        <div class="tool-summary">{{ summary() }}</div>
      }

      @if (detailsOpen()) {
        <div class="tool-details">
          <details>
            <summary>{{ t('tool.arguments') }}</summary>
            <pre><code>{{ inputText() }}</code></pre>
          </details>

          @switch (kind()) {
            @case ('completed') {
              @if (outputText()) {
                <details open>
                  <summary>{{ t('tool.output') }}</summary>
                  <app-diff-view [text]="outputText()" />
                </details>
              }
            }
            @case ('error') {
              @if (errorText()) {
                <details open>
                  <summary>{{ t('tool.error') }}</summary>
                  <pre class="error"><code>{{ errorText() }}</code></pre>
                </details>
              }
            }
            @case ('running') {
              <div class="running-note muted">{{ t('tool.running') }}</div>
            }
          }
        </div>
      }
    </div>
  `,
  styles: `
    .tool {
      border: 1px solid var(--border);
      border-left: 3px solid var(--fg-muted);
      border-radius: var(--radius-sm);
      background: var(--bg-surface);
      padding: 6px 10px;
      margin: 6px 0;
      font-size: 13px;
    }
    .tool.closed {
      border-left-color: var(--fg-muted);
    }
    .tool-head {
      display: flex;
      align-items: center;
      gap: 8px;
    }
    .tool-name {
      font-family: var(--mono);
      font-weight: 600;
    }
    .badge {
      font-size: 10.5px;
      text-transform: uppercase;
      letter-spacing: 0.4px;
      padding: 1px 8px;
      border-radius: 20px;
      background: var(--bg-raised);
      border: 1px solid var(--border);
      color: var(--fg-muted);
    }
    .badge.state-running {
      color: var(--warn);
      border-color: var(--warn);
    }
    .badge.state-completed {
      color: var(--ok);
      border-color: var(--ok);
    }
    .badge.state-error {
      color: var(--err);
      border-color: var(--err);
    }
    .badge.state-pending {
      color: var(--fg-muted);
    }
    .badge.delegation {
      color: var(--accent);
      border-color: var(--accent);
      background: rgba(63, 111, 224, 0.12);
    }
    .task-target {
      font-size: 12px;
      font-weight: 600;
    }
    a.task-target {
      color: var(--accent);
      text-decoration: none;
    }
    a.task-target:hover {
      text-decoration: underline;
    }
    .expand {
      margin-left: auto;
      background: none;
      border: none;
      color: var(--fg-muted);
      font-size: 11.5px;
      padding: 2px 6px;
    }
    .tool-summary {
      color: var(--fg-muted);
      font-size: 12.5px;
      margin: 4px 2px 0;
      overflow-wrap: anywhere;
    }
    .tool-details {
      margin-top: 6px;
    }
    details {
      margin-top: 4px;
      font-size: 12.5px;
    }
    summary {
      cursor: pointer;
      color: var(--fg-muted);
      user-select: none;
    }
    pre {
      background: var(--bg-raised);
      border: 1px solid var(--border);
      border-radius: var(--radius-sm);
      padding: 8px 10px;
      overflow-x: auto;
      white-space: pre-wrap;
      word-break: break-word;
      margin: 4px 0 0;
      font-size: 12px;
      max-height: 420px;
      overflow-y: auto;
    }
    pre.error {
      color: var(--err);
    }
    .running-note {
      font-size: 12px;
      margin-top: 4px;
    }
  `,
})
export class ToolPartComponent {
  private readonly i18n = inject(I18nService);
  readonly t = this.i18n.t.bind(this.i18n);

  readonly part = input.required<Part>();
  /** Task name/ID → childSessionID map, passed down from the chat view. */
  readonly taskLinks = input<Map<string, string>>(new Map());
  readonly detailsOpen = signal(true);

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
  readonly isClosed = computed(
    () => this.state().state === 'completed' || this.state().state === 'error',
  );

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
