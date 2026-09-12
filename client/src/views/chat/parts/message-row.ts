import { Component, computed, inject, input, linkedSignal, output } from '@angular/core';

import { Message, Part, ToolStateKind } from '../../../core/engine.dtos';
import { UiPrefsStore } from '../../../core/ui-prefs.store';
import { I18nService } from '../../../i18n/i18n.service';
import { formatMs } from '../../../core/format';
import { PartRendererComponent } from './part-renderer';
import { RenderedPart, ToolGroupComponent, summarizeToolRun } from './tool-group';

export type { RenderedPart };

/**
 * A run of >= 2 consecutive tool calls folded into one collapsible summary
 * row (F6-1). `key` is the index of the run's first part in `message.parts`,
 * stable for the lifetime of the message and used to remember toggles.
 */
export interface ToolGroup {
  kind: 'group';
  key: number;
  rows: RenderedPart[];
  /** Worst state across the run: error > running/pending > completed. */
  state: ToolStateKind;
  /** "read ×3, edit ×2" - tool names by first appearance with their counts. */
  names: string;
}

export type RenderedItem = RenderedPart | ToolGroup;

/**
 * Grouping pass (F6-1): walks one message's parts, numbering tool calls with
 * their per-message ordinal (`toolIndex`, so the first call of the turn can
 * open by default) and folding any run of two or more consecutive tool parts
 * into a single `ToolGroup`. A lone tool call - one with no tool neighbour -
 * stays an ungrouped `RenderedPart`, exactly as before grouping existed.
 */
export function groupParts(parts: readonly Part[]): RenderedItem[] {
  const items: RenderedItem[] = [];
  let toolIndex = -1;
  let run: RenderedPart[] = [];
  let runStart = -1;

  const flush = (): void => {
    if (run.length >= 2) {
      items.push(makeGroup(runStart, run));
    } else if (run.length === 1) {
      items.push(run[0]);
    }
    run = [];
    runStart = -1;
  };

  parts.forEach((part, index) => {
    if (part.type === 'tool') {
      toolIndex += 1;
      if (run.length === 0) {
        runStart = index;
      }
      run.push({ kind: 'part', part, toolIndex });
      return;
    }
    flush();
    items.push({ kind: 'part', part, toolIndex: -1 });
  });
  flush();
  return items;
}

function makeGroup(key: number, rows: RenderedPart[]): ToolGroup {
  const { state, names } = summarizeToolRun(rows);
  return { kind: 'group', key, rows, state, names };
}

/**
 * One message in the transcript (F2-4).
 *
 * User turns are right-aligned, compact `--accent` bubbles; assistant turns have
 * no bubble at all - just an `ASSISTANT` micro-label above a readable body.
 */
@Component({
  selector: 'app-message-row',
  imports: [PartRendererComponent, ToolGroupComponent],
  host: { '[id]': 'rowId()', '[class.user]': 'isUser()' },
  template: `
    @if (isUser()) {
      <div class="user-row">
        @if (rollbackEnabled()) {
          <button
            type="button"
            class="rollback"
            [title]="t('chat.rollback')"
            [attr.aria-label]="t('chat.rollback')"
            (click)="rollback.emit(message().id)"
          >&#8617;</button>
        }
        <div class="bubble">
          @for (item of items(); track $index) {
            @if (item.kind === 'part') {
              <app-part-renderer
                [part]="item.part"
                [toolIndex]="item.toolIndex"
                [taskLinks]="taskLinks()"
              />
            }
          }
        </div>
      </div>
    } @else {
      <div class="assistant-row">
        <div class="label-row">
          <span class="who">{{ t('chat.assistantLabel') }}</span>
          @if (agentModel()) {
            <span class="model">{{ agentModel() }}</span>
          }
          @if (time()) {
            <span class="time">{{ time() }}</span>
          }
        </div>
        <div class="body">
          @for (item of items(); track $index) {
            @if (item.kind === 'group') {
              <app-tool-group
                [rows]="item.rows"
                [state]="item.state"
                [names]="item.names"
                [count]="item.rows.length"
                [open]="groupOpen(item.key)"
                [taskLinks]="taskLinks()"
                (toggle)="toggleGroup(item.key)"
              />
            } @else {
              <app-part-renderer
                [part]="item.part"
                [toolIndex]="item.toolIndex"
                [taskLinks]="taskLinks()"
              />
            }
          }
        </div>
      </div>
    }
  `,
  styles: `
    :host {
      display: block;
    }

    /* --- user turn: right-aligned accent bubble --- */
    .user-row {
      display: flex;
      align-items: flex-start;
      justify-content: flex-end;
      gap: var(--space-6);
    }
    .bubble {
      max-width: min(68%, 620px);
      padding: 5px 9px;
      background: color-mix(in srgb, var(--accent) 16%, var(--surface-3));
      color: var(--text);
      border-radius: var(--radius-bubble) var(--radius-bubble) 3px var(--radius-bubble);
      font-size: 13px;
      line-height: 1.4;
      overflow-wrap: anywhere;
    }
    .rollback {
      align-self: center;
      flex: none;
      border: 1px solid var(--border);
      background: var(--surface-2);
      color: var(--text-faint);
      border-radius: var(--radius-control-sm);
      font-size: var(--fs-12);
      line-height: 1;
      padding: 3px 7px;
      opacity: 0;
      transition: opacity 120ms ease;
    }
    .user-row:hover .rollback,
    .rollback:focus-visible {
      opacity: 1;
    }
    .rollback:hover {
      color: var(--accent);
      border-color: var(--accent);
    }

    /* --- assistant turn: no bubble --- */
    .assistant-row {
      display: flex;
      flex-direction: column;
      gap: 3px;
      max-width: 100%;
    }
    .label-row {
      display: flex;
      align-items: baseline;
      gap: var(--space-8);
    }
    .who {
      font-size: var(--fs-11);
      font-weight: 600;
      letter-spacing: var(--label-tracking);
      text-transform: uppercase;
      color: var(--text-faint);
    }
    .model,
    .time {
      font-family: var(--font-mono);
      font-size: var(--fs-11);
      color: var(--text-faint);
    }
    .body {
      font-size: 13px;
      line-height: 1.45;
      color: var(--text);
      display: flex;
      flex-direction: column;
      gap: var(--space-4);
      overflow-wrap: anywhere;
    }

    /* F6-1: the grouped-run summary row itself is app-tool-group now
       (./tool-group.ts), reused as-is by the cross-message merge (F6-1c,
       tool-run-row.ts); its styles live there. */

    @media (max-width: 700px) {
      .bubble {
        max-width: 86%;
      }
    }
  `,
})
export class MessageRowComponent {
  private readonly i18n = inject(I18nService);
  private readonly prefs = inject(UiPrefsStore);
  readonly t = this.i18n.t.bind(this.i18n);
  readonly message = input.required<Message>();
  readonly rollbackEnabled = input(false);
  readonly rollback = output<string>();
  readonly rowId = input('');
  /** Task name/ID → childSessionID map for clickable sub-agent links. */
  readonly taskLinks = input<Map<string, string>>(new Map());
  readonly isUser = computed(() => this.message().role === 'user');

  /**
   * Parts plus their tool ordinal (F2-6, now only used for numbering - every
   * tool call starts collapsed regardless, F6-1b), with runs of >= 2
   * consecutive tool calls folded into collapsible groups (F6-1).
   */
  readonly items = computed<RenderedItem[]>(() => groupParts(this.message().parts));

  /**
   * Per-group open/closed overrides (keyed by `ToolGroup.key`). Reset whenever
   * the message identity or the "expand by default" preference changes, so a
   * flipped preference takes effect immediately, like `ToolPartComponent`.
   */
  private readonly groupOverrides = linkedSignal<
    { id: string; expand: boolean },
    Record<number, boolean>
  >({
    source: () => ({ id: this.message().id, expand: this.prefs.expandToolCallsByDefault() }),
    computation: () => ({}),
  });

  groupOpen(key: number): boolean {
    return this.groupOverrides()[key] ?? this.prefs.expandToolCallsByDefault();
  }

  toggleGroup(key: number): void {
    const open = this.groupOpen(key);
    this.groupOverrides.update((current) => ({ ...current, [key]: !open }));
  }

  readonly time = computed(() => {
    const created = this.message().meta?.created_at;
    return created ? formatMs(created) : '';
  });
  readonly agentModel = computed(() => {
    const meta = this.message().meta;
    if (!meta?.agent && !meta?.model) {
      return '';
    }
    return [meta?.agent, meta?.model].filter(Boolean).join(' · ');
  });
}
