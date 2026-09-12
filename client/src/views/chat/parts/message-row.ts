import { Component, computed, inject, input, linkedSignal, output } from '@angular/core';

import { Message, Part, ToolStateKind } from '../../../core/engine.dtos';
import { UiPrefsStore } from '../../../core/ui-prefs.store';
import { I18nService } from '../../../i18n/i18n.service';
import { formatMs } from '../../../core/format';
import { PartRendererComponent } from './part-renderer';

/** One part plus the info the renderer needs to pick its default state. */
export interface RenderedPart {
  kind: 'part';
  part: Part;
  /** 0-based index of this part among the tool parts of the same message. */
  toolIndex: number;
}

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

/** Names beyond this many are folded into a trailing ellipsis. */
const MAX_SUMMARY_NAMES = 4;

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
  const counts = new Map<string, number>();
  let state: ToolStateKind = 'completed';
  for (const row of rows) {
    if (row.part.type !== 'tool') {
      continue;
    }
    counts.set(row.part.name, (counts.get(row.part.name) ?? 0) + 1);
    const kind = row.part.state.state;
    if (kind === 'error') {
      state = 'error';
    } else if ((kind === 'running' || kind === 'pending') && state !== 'error') {
      state = 'running';
    }
  }
  const labels = [...counts.entries()].map(([name, n]) => (n > 1 ? `${name} ×${n}` : name));
  const names =
    labels.length > MAX_SUMMARY_NAMES
      ? `${labels.slice(0, MAX_SUMMARY_NAMES).join(', ')}, …`
      : labels.join(', ');
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
  imports: [PartRendererComponent],
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
              <div class="tool-group" [class.open]="groupOpen(item.key)">
                <button
                  type="button"
                  class="group-head"
                  (click)="toggleGroup(item.key)"
                  [attr.aria-expanded]="groupOpen(item.key)"
                  [title]="groupOpen(item.key) ? t('toolGroup.collapse') : t('toolGroup.expand')"
                >
                  <span class="dot state-{{ item.state }}" aria-hidden="true"></span>
                  <span class="group-count">{{ t('toolGroup.summary', { n: item.rows.length }) }}</span>
                  <span class="group-names">{{ item.names }}</span>
                  <span class="chevron" aria-hidden="true">{{ groupOpen(item.key) ? '▾' : '▸' }}</span>
                </button>
                @if (groupOpen(item.key)) {
                  <div class="group-body">
                    @for (row of item.rows; track $index) {
                      <app-part-renderer
                        [part]="row.part"
                        [toolIndex]="row.toolIndex"
                        [taskLinks]="taskLinks()"
                      />
                    }
                  </div>
                }
              </div>
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

    /* --- F6-1: grouped run of tool calls --- */
    .tool-group {
      border: 1px solid var(--border);
      border-radius: var(--radius-panel);
      background: var(--surface);
      overflow: hidden;
    }
    .group-head {
      display: flex;
      align-items: center;
      gap: var(--space-8);
      width: 100%;
      min-height: 28px;
      background: none;
      border: none;
      border-radius: 0;
      padding: 5px var(--space-12);
      cursor: pointer;
      text-align: left;
      min-width: 0;
    }
    .group-head:hover {
      background: var(--surface-2);
    }
    .group-head .dot {
      width: 7px;
      height: 7px;
      border-radius: 50%;
      flex: none;
      background: var(--text-faint);
    }
    .group-head .dot.state-completed {
      background: var(--success);
    }
    .group-head .dot.state-running {
      background: var(--accent);
    }
    .group-head .dot.state-error {
      background: var(--danger);
    }
    .group-count {
      flex: none;
      font-family: var(--font-mono);
      font-size: var(--fs-12-5);
      font-weight: 600;
      color: var(--text);
    }
    .group-names {
      flex: 1 1 auto;
      min-width: 0;
      font-family: var(--font-mono);
      font-size: var(--fs-11-5);
      color: var(--text-muted);
      overflow: hidden;
      text-overflow: ellipsis;
      white-space: nowrap;
    }
    .group-head .chevron {
      flex: none;
      font-size: 10px;
      color: var(--text-faint);
    }
    .group-body {
      display: flex;
      flex-direction: column;
      gap: var(--space-4);
      padding: var(--space-8);
      border-top: 1px solid var(--border);
    }

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
   * Parts plus their tool ordinal (the first tool call of a turn renders
   * expanded, every later one collapsed - F2-6), with runs of >= 2
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
