import { Component, computed, inject, input, linkedSignal } from '@angular/core';

import { Message, ToolStateKind } from '../../../core/engine.dtos';
import { formatMs } from '../../../core/format';
import { I18nService } from '../../../i18n/i18n.service';
import { UiPrefsStore } from '../../../core/ui-prefs.store';
import { GroupRow, RenderedPart, ToolGroupComponent, summarizeToolRun } from './tool-group';

/** Everything `ToolRunRowComponent` needs, pre-computed from the merged turns. */
export interface ToolRunSummary {
  /** First message's id - stable across the run's lifetime (see `buildToolRun`). */
  key: string;
  rows: GroupRow[];
  state: ToolStateKind;
  names: string;
  count: number;
  tokensIn: number;
  tokensOut: number;
  cost: number | null;
}

/**
 * F6-1c: fold a run of >= 2 consecutive tool-only assistant turns (no text
 * part - only tool calls/results, optionally thinking, see
 * `chat.ts#isToolOnlyTurn`) into one `ToolRunSummary`: the combined tool-name
 * summary and worst state across every turn (same rules as a single
 * message's group, `summarizeToolRun`), the token/cost totals, and the
 * expanded body's rows - each turn's own non-usage parts, preceded by a
 * `TurnStrip` divider carrying that turn's own token counts (so they stay
 * visible once expanded even though the header only shows the sum).
 *
 * `toolIndex` is numbered continuously across the whole run purely for
 * consistency with `groupParts`; it no longer affects any default open state
 * (F6-1b - every call starts collapsed regardless).
 */
export function buildToolRun(messages: readonly Message[]): ToolRunSummary {
  const rows: GroupRow[] = [];
  const toolRows: RenderedPart[] = [];
  let tokensIn = 0;
  let tokensOut = 0;
  let cost: number | null = null;
  let toolIndex = -1;

  for (const message of messages) {
    let turnIn = 0;
    let turnOut = 0;
    const turnRows: RenderedPart[] = [];
    for (const part of message.parts) {
      if (part.type === 'usage') {
        turnIn += part.input_tokens;
        turnOut += part.output_tokens;
        if (typeof part.cost === 'number') {
          cost = (cost ?? 0) + part.cost;
        }
        continue;
      }
      const rp: RenderedPart = {
        kind: 'part',
        part,
        toolIndex: part.type === 'tool' ? ++toolIndex : -1,
      };
      turnRows.push(rp);
      if (part.type === 'tool') {
        toolRows.push(rp);
      }
    }
    rows.push({ kind: 'turn', tokensIn: turnIn, tokensOut: turnOut });
    rows.push(...turnRows);
    tokensIn += turnIn;
    tokensOut += turnOut;
  }

  const { state, names } = summarizeToolRun(toolRows);
  return {
    key: messages[0]?.id ?? '',
    rows,
    state,
    names,
    count: toolRows.length,
    tokensIn,
    tokensOut,
    cost,
  };
}

/**
 * One row in the transcript for a merged run of consecutive tool-only
 * assistant turns (F6-1c): a single "ASSISTANT agent · model · first
 * timestamp" header (from the run's first message) above one
 * `<app-tool-group>` covering every turn's tool calls, and one usage line
 * with the summed tokens/cost - the per-turn numbers stay available as small
 * labels inside the expanded group (see `TurnStrip`).
 *
 * The group's open/closed state is a single flag, not the per-call record
 * `MessageRowComponent` keeps: a run is one collapsible unit. It resets when
 * the run's identity (`messages()[0].id`) or the expand-by-default
 * preference changes; a streaming turn appended to the *same* run keeps that
 * identity, so an open/closed choice survives new tool calls arriving mid-run.
 */
@Component({
  selector: 'app-tool-run-row',
  imports: [ToolGroupComponent],
  host: { '[id]': 'rowId()' },
  template: `
    <div class="assistant-row">
      <div class="label-row" [title]="t('chat.mergedTurns', { n: messages().length })">
        <span class="who">{{ t('chat.assistantLabel') }}</span>
        @if (agentModel()) {
          <span class="model">{{ agentModel() }}</span>
        }
        @if (time()) {
          <span class="time">{{ time() }}</span>
        }
      </div>
      <div class="body">
        <app-tool-group
          [rows]="summary().rows"
          [state]="summary().state"
          [names]="summary().names"
          [count]="summary().count"
          [open]="open()"
          [taskLinks]="taskLinks()"
          (toggle)="toggle()"
        />
        <div class="usage">
          <span>{{ t('drawer.tokensIn') }} {{ summary().tokensIn }}</span>
          <span>{{ t('drawer.tokensOut') }} {{ summary().tokensOut }}</span>
          @if (summary().cost !== null) {
            <span>· {{ summary().cost }} USD</span>
          }
        </div>
      </div>
    </div>
  `,
  styles: `
    :host {
      display: block;
    }
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
    .usage {
      display: flex;
      gap: 10px;
      font-family: var(--font-mono);
      font-size: var(--fs-11);
      color: var(--text-faint);
      justify-content: flex-end;
      border-top: 1px dashed var(--border);
      margin-top: 6px;
      padding-top: 6px;
    }
  `,
})
export class ToolRunRowComponent {
  private readonly i18n = inject(I18nService);
  private readonly prefs = inject(UiPrefsStore);
  readonly t = this.i18n.t.bind(this.i18n);

  readonly rowId = input('');
  readonly messages = input.required<Message[]>();
  readonly taskLinks = input<Map<string, string>>(new Map());

  readonly summary = computed<ToolRunSummary>(() => buildToolRun(this.messages()));

  /**
   * Single open/closed override for the whole run. Reset when the run's
   * first-message id changes (a genuinely different run took this slot) or
   * the pref flips; unaffected by more turns appending to the same run.
   */
  private readonly openOverride = linkedSignal<
    { id: string; expand: boolean },
    boolean | null
  >({
    source: () => ({
      id: this.messages()[0]?.id ?? '',
      expand: this.prefs.expandToolCallsByDefault(),
    }),
    computation: () => null,
  });

  readonly open = computed(() => this.openOverride() ?? this.prefs.expandToolCallsByDefault());

  toggle(): void {
    this.openOverride.set(!this.open());
  }

  readonly time = computed(() => {
    const created = this.messages()[0]?.meta?.created_at;
    return created ? formatMs(created) : '';
  });
  readonly agentModel = computed(() => {
    const meta = this.messages()[0]?.meta;
    if (!meta?.agent && !meta?.model) {
      return '';
    }
    return [meta?.agent, meta?.model].filter(Boolean).join(' · ');
  });
}
