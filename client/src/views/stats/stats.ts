/**
 * Stats screen (F7-5): usage across every persisted session.
 *
 * Header: title + project filter (all / current) + range filter (7d / 30d /
 * all) + Refresh. Body: summary tiles (tokens in/out/cache, cost, sessions,
 * tool calls), a 30-day stacked bar chart (pure CSS, no chart library), and
 * sortable tables for models, providers, agents, projects, tools and the top
 * sessions by tokens. The engine aggregates (`GET /stats`); this component
 * only maps the payload onto the design tokens.
 *
 * Refreshes itself when a turn finishes (`session.updated` with
 * `running: false`) so the tiles follow the chat without polling.
 */

import {
  ChangeDetectionStrategy,
  Component,
  OnDestroy,
  OnInit,
  computed,
  inject,
} from '@angular/core';

import { EngineEvent, StatsBucket, StatsSessionRow, StatsToolRow } from '../../core/engine.dtos';
import { EventsStore } from '../../core/events.store';
import { I18nService } from '../../i18n/i18n.service';
import { formatCost, formatTokens } from '../chat/chat-session.store';
import { StatsColumn, StatsTable, formatNumber } from './stats-table';
import { ProjectScope, RangeScope, StatsStore } from './stats.store';

/** Row of the top-sessions table: the engine row + a display title. */
export interface SessionTableRow extends StatsSessionRow {
  label: string;
  total_tokens: number;
}

const BUCKET_TAIL: StatsColumn<StatsBucket>[] = [
  { key: 'sessions', labelKey: 'stats.colSessions', kind: 'number' },
  { key: 'llm_calls', labelKey: 'stats.colCalls', kind: 'number' },
  { key: 'input_tokens', labelKey: 'stats.colIn', kind: 'number' },
  { key: 'output_tokens', labelKey: 'stats.colOut', kind: 'number' },
  { key: 'cache_read_tokens', labelKey: 'stats.colCache', kind: 'number' },
  { key: 'cost', labelKey: 'stats.colCost', kind: 'cost' },
];

@Component({
  selector: 'app-stats',
  changeDetection: ChangeDetectionStrategy.OnPush,
  imports: [StatsTable],
  templateUrl: './stats.html',
  styleUrl: './stats.css',
})
export class StatsView implements OnInit, OnDestroy {
  readonly store = inject(StatsStore);
  private readonly events = inject(EventsStore);
  private readonly i18n = inject(I18nService);

  readonly t = this.i18n.t.bind(this.i18n);

  readonly scopes: { id: ProjectScope; labelKey: 'stats.projectAll' | 'stats.projectCurrent' }[] = [
    { id: 'all', labelKey: 'stats.projectAll' },
    { id: 'current', labelKey: 'stats.projectCurrent' },
  ];
  readonly ranges: {
    id: RangeScope;
    labelKey: 'stats.range7d' | 'stats.range30d' | 'stats.rangeAll';
  }[] = [
    { id: '7d', labelKey: 'stats.range7d' },
    { id: '30d', labelKey: 'stats.range30d' },
    { id: 'all', labelKey: 'stats.rangeAll' },
  ];

  readonly modelColumns: StatsColumn<StatsBucket>[] = [
    { key: 'key', labelKey: 'stats.colModel', kind: 'mono' },
    ...BUCKET_TAIL,
  ];
  readonly providerColumns: StatsColumn<StatsBucket>[] = [
    { key: 'key', labelKey: 'stats.colProvider', kind: 'mono' },
    ...BUCKET_TAIL,
  ];
  readonly agentColumns: StatsColumn<StatsBucket>[] = [
    { key: 'key', labelKey: 'stats.colAgent', kind: 'text' },
    { key: 'turns', labelKey: 'stats.colTurns', kind: 'number' },
    ...BUCKET_TAIL,
  ];
  readonly projectColumns: StatsColumn<StatsBucket>[] = [
    { key: 'key', labelKey: 'stats.colProject', kind: 'mono' },
    { key: 'tool_calls', labelKey: 'stats.colToolCalls', kind: 'number' },
    ...BUCKET_TAIL,
  ];
  readonly toolColumns: StatsColumn<StatsToolRow>[] = [
    { key: 'name', labelKey: 'stats.colTool', kind: 'mono' },
    { key: 'calls', labelKey: 'stats.colCalls', kind: 'number' },
    { key: 'errors', labelKey: 'stats.colErrors', kind: 'number' },
  ];
  readonly sessionColumns: StatsColumn<SessionTableRow>[] = [
    { key: 'label', labelKey: 'stats.colSession', kind: 'text' },
    { key: 'agent', labelKey: 'stats.colAgent', kind: 'text' },
    { key: 'directory', labelKey: 'stats.colProject', kind: 'mono' },
    { key: 'total_tokens', labelKey: 'stats.colTokens', kind: 'number' },
    { key: 'input_tokens', labelKey: 'stats.colIn', kind: 'number' },
    { key: 'output_tokens', labelKey: 'stats.colOut', kind: 'number' },
    { key: 'tool_calls', labelKey: 'stats.colToolCalls', kind: 'number' },
    { key: 'cost', labelKey: 'stats.colCost', kind: 'cost' },
  ];

  readonly data = this.store.data;
  readonly totals = this.store.totals;
  readonly bars = this.store.bars;

  readonly sessionRows = computed<SessionTableRow[]>(() =>
    (this.data()?.top_sessions ?? []).map((s) => ({
      ...s,
      label: s.title?.trim() || this.t('stats.untitled'),
      total_tokens: s.input_tokens + s.output_tokens + s.cache_read_tokens + s.cache_write_tokens,
    })),
  );

  /** Summary tiles. `value` is the compact figure, `detail` the exact one. */
  readonly tiles = computed(() => {
    const t = this.totals();
    if (!t) {
      return [];
    }
    const cache = t.cache_read_tokens + t.cache_write_tokens;
    return [
      {
        id: 'in',
        labelKey: 'stats.tokensIn' as const,
        value: formatTokens(t.input_tokens),
        detail: formatNumber(t.input_tokens),
      },
      {
        id: 'out',
        labelKey: 'stats.tokensOut' as const,
        value: formatTokens(t.output_tokens),
        detail: formatNumber(t.output_tokens),
      },
      {
        id: 'cache',
        labelKey: 'stats.tokensCache' as const,
        value: formatTokens(cache),
        detail: this.t('stats.cacheDetail', {
          read: formatNumber(t.cache_read_tokens),
          write: formatNumber(t.cache_write_tokens),
        }),
      },
      {
        id: 'cost',
        labelKey: 'stats.cost' as const,
        value: formatCost(t.cost),
        detail:
          t.cost_unknown_calls > 0
            ? this.t('stats.costUnknown', { n: t.cost_unknown_calls })
            : this.t('stats.costKnown'),
      },
      {
        id: 'sessions',
        labelKey: 'stats.sessions' as const,
        value: formatNumber(t.sessions),
        detail: this.t('stats.sessionsDetail', {
          turns: formatNumber(t.turns),
          calls: formatNumber(t.llm_calls),
        }),
      },
      {
        id: 'tools',
        labelKey: 'stats.toolCalls' as const,
        value: formatNumber(t.tool_calls),
        detail: this.t('stats.toolCallsDetail', { n: this.data()?.tools.length ?? 0 }),
      },
    ];
  });

  readonly compactionLabel = computed(() => {
    const c = this.data()?.compaction;
    if (!c || c.count === 0 || c.avg_context_before === null) {
      return this.t('stats.compactionNone');
    }
    return this.t('stats.compactionAvg', {
      n: c.count,
      tokens: formatTokens(c.avg_context_before),
    });
  });

  readonly rangeNote = computed(() => {
    const d = this.data();
    if (!d) {
      return '';
    }
    const scope = this.store.scopeFallback()
      ? this.t('stats.scopeFallback')
      : (d.range.directory ?? this.t('stats.allProjects'));
    return this.t('stats.scanned', { n: d.range.scanned_sessions, scope });
  });

  private unsubscribe: (() => void) | null = null;

  ngOnInit(): void {
    void this.store.load();
    this.events.start();
    this.unsubscribe = this.events.onEvent((ev) => this.onEvent(ev));
  }

  ngOnDestroy(): void {
    this.unsubscribe?.();
    this.unsubscribe = null;
  }

  setScope(scope: ProjectScope): void {
    this.store.setScope(scope);
  }

  setRange(range: RangeScope): void {
    this.store.setRange(range);
  }

  refresh(): void {
    void this.store.load();
  }

  barTitle(bar: { day: string; total: number; calls: number; cost: number | null }): string {
    return this.t('stats.barTitle', {
      day: bar.day,
      tokens: formatNumber(bar.total),
      calls: bar.calls,
      cost: formatCost(bar.cost),
    });
  }

  /** Only label every 5th day (plus the last) so the axis stays readable. */
  showLabel(index: number, count: number): boolean {
    return index === count - 1 || (count - 1 - index) % 5 === 0;
  }

  /** A finished turn changes the numbers: refresh once, not per streamed part. */
  private onEvent(ev: EngineEvent): void {
    if (ev.type === 'session.updated' && ev.properties?.['running'] === false) {
      void this.store.load();
    } else if (ev.type === 'session.deleted' || ev.type === 'session.created') {
      void this.store.load();
    }
  }
}
