/**
 * Session sidebar (M6): a compact summary of the session - models used (with a
 * filter), tokens (in / out / prompt-cache hit & write / hit rate), cost, files
 * changed - plus live toggles for the environment (MCP servers, skills, YOLO
 * mode). Token/cache semantics:
 *   - In  = input tokens (uncached prompt)
 *   - Out = output tokens
 *   - cache = cache_read_input_tokens (prompt-cache hit)
 *   - cache W = cache_creation_input_tokens (prompt-cache write)
 *   - hit rate = cacheRead / (input + cacheRead + cacheWrite)
 */

import { Component, computed, effect, inject, input, output, signal } from '@angular/core';

import { EngineClient } from '../../core/engine-client.service';
import {
  Message,
  McpStatus,
  ResolvedSkill,
  SessionMeta,
  UsagePart,
} from '../../core/engine.dtos';

interface Totals {
  input: number;
  output: number;
  cacheRead: number;
  cacheWrite: number;
  cost: number;
}

@Component({
  selector: 'app-session-sidebar',
  template: `
    <aside class="sidebar">
      <div class="block">
        <div class="block-title">Models</div>
        <ul class="model-list">
          <li class="model-item" [class.active]="!filterModel()" (click)="filterModelChange.emit(null)">
            <span class="muted small">all</span>
          </li>
          @for (m of models(); track m) {
            <li
              class="model-item"
              [class.active]="filterModel() === m"
              (click)="filterModelChange.emit(m)"
              [title]="m"
            >
              <span class="mono small">{{ m }}</span>
            </li>
          }
        </ul>
      </div>

      <div class="block">
        <div class="block-title">Tokens</div>
        <div class="row"><span class="muted">In</span><span class="value">{{ totals().input }}</span></div>
        <div class="row"><span class="muted">cache</span><span class="value">{{ totals().cacheRead }}</span></div>
        <div class="row"><span class="muted">Out</span><span class="value">{{ totals().output }}</span></div>
        <div class="row"><span class="muted">cache R</span><span class="value">{{ totals().cacheRead }}</span></div>
        <div class="row"><span class="muted">cache W</span><span class="value">{{ totals().cacheWrite }}</span></div>
        <div class="row"><span class="muted">hit rate</span><span class="value">{{ cacheRate() }}</span></div>
      </div>

      <div class="block">
        <div class="block-title">Cost</div>
        <div class="value">{{ costLabel() }}</div>
      </div>

      <div class="block">
        <div class="block-title">Files changed</div>
        @if (filesChanged().length === 0) {
          <div class="muted small">-</div>
        }
        <ul class="files">
          @for (file of filesChanged(); track file) {
            <li class="mono small">{{ file }}</li>
          }
        </ul>
      </div>

      <div class="block">
        <div class="block-title">YOLO mode</div>
        <label class="toggle">
          <input type="checkbox" [checked]="yolo()" (change)="toggleYolo(!yolo())" />
          <span class="muted small">auto-allow all</span>
        </label>
      </div>

      <div class="block">
        <div class="block-title">MCP</div>
        @if (mcp().length === 0) {
          <div class="muted small">-</div>
        }
        <ul class="toggles">
          @for (server of mcp(); track server.name) {
            <li>
              <label class="toggle">
                <input
                  type="checkbox"
                  [checked]="server.enabled"
                  (change)="toggleMcp(server, !server.enabled)"
                />
                <span class="mono small">{{ server.name }}</span>
              </label>
            </li>
          }
        </ul>
      </div>

      <div class="block">
        <div class="block-title">Skills</div>
        @if (skills().length === 0) {
          <div class="muted small">-</div>
        }
        <ul class="toggles">
          @for (skill of skills(); track skill.name) {
            <li>
              <label class="toggle">
                <input
                  type="checkbox"
                  [checked]="skill.enabled"
                  (change)="toggleSkill(skill, !skill.enabled)"
                />
                <span class="mono small">{{ skill.name }}</span>
              </label>
            </li>
          }
        </ul>
      </div>

      @if (running()) {
        <div class="working" aria-label="agent working">
          <span class="pulse"></span>
          <span class="muted small">agent working…</span>
        </div>
      }
    </aside>
  `,
  styles: `
    .sidebar {
      width: 230px;
      flex-shrink: 0;
      background: var(--bg-surface);
      border: 1px solid var(--border);
      border-radius: var(--radius);
      padding: 12px;
      display: flex;
      flex-direction: column;
      gap: 16px;
      overflow-y: auto;
      font-size: 12.5px;
    }
    .block-title {
      font-size: 10.5px;
      text-transform: uppercase;
      letter-spacing: 0.4px;
      color: var(--fg-muted);
      margin-bottom: 4px;
    }
    .value {
      font-weight: 600;
    }
    .row {
      display: flex;
      justify-content: space-between;
      gap: 8px;
    }
    .small {
      font-size: 11.5px;
    }
    .muted {
      color: var(--fg-muted);
    }
    .files, .toggles, .model-list {
      list-style: none;
      margin: 4px 0 0;
      padding: 0;
      display: flex;
      flex-direction: column;
      gap: 2px;
      overflow-wrap: anywhere;
    }
    .model-item {
      display: flex;
      align-items: center;
      gap: 6px;
      padding: 3px 6px;
      border-radius: var(--radius-sm);
      cursor: pointer;
      border: 1px solid transparent;
    }
    .model-item:hover {
      background: var(--bg-raised);
    }
    .model-item.active {
      border-color: var(--accent);
      background: rgba(63, 111, 224, 0.12);
    }
    .toggle {
      display: inline-flex;
      align-items: center;
      gap: 6px;
      cursor: pointer;
    }
    .working {
      margin-top: auto;
      display: flex;
      align-items: center;
      gap: 8px;
      padding: 8px 10px;
      border: 1px solid var(--accent);
      border-radius: var(--radius-sm);
      background: var(--bg-raised);
    }
    .pulse {
      width: 10px;
      height: 10px;
      border-radius: 50%;
      background: var(--accent);
      animation: pulse 1s ease-in-out infinite;
    }
    @keyframes pulse {
      0%, 100% { opacity: 0.25; transform: scale(0.85); }
      50% { opacity: 1; transform: scale(1.15); }
    }
  `,
})
export class SessionSidebarComponent {
  private readonly engine = inject(EngineClient);

  readonly messages = input.required<Message[]>();
  readonly meta = input<SessionMeta | null>(null);
  readonly running = input(false);
  /** Models used across the session (from message meta). */
  readonly models = input<string[]>([]);
  /** Currently selected model filter (null = all). */
  readonly filterModel = input<string | null>(null);
  readonly filterModelChange = output<string | null>();

  readonly mcp = signal<McpStatus[]>([]);
  readonly skills = signal<ResolvedSkill[]>([]);
  readonly yolo = signal(false);

  constructor() {
    effect(() => {
      const dir = this.meta()?.directory;
      if (dir) {
        void this.load(dir);
      }
    });
  }

  readonly totals = computed<Totals>(() => {
    const t: Totals = { input: 0, output: 0, cacheRead: 0, cacheWrite: 0, cost: 0 };
    for (const message of this.messages()) {
      for (const part of message.parts) {
        if (part.type === 'usage') {
          const u = part as UsagePart;
          t.input += u.input_tokens ?? 0;
          t.output += u.output_tokens ?? 0;
          t.cacheRead += u.cache_read_input_tokens ?? 0;
          t.cacheWrite += u.cache_creation_input_tokens ?? 0;
          t.cost += u.cost ?? 0;
        }
      }
    }
    return t;
  });

  readonly cacheRate = computed<string>(() => {
    const t = this.totals();
    const total = t.input + t.cacheRead + t.cacheWrite;
    if (total === 0) {
      return '-';
    }
    return `${((t.cacheRead / total) * 100).toFixed(1)}%`;
  });

  readonly costLabel = computed(() =>
    this.totals().cost > 0 ? `$${this.totals().cost.toFixed(4)}` : '-',
  );

  readonly filesChanged = computed<string[]>(() => {
    const files = new Set<string>();
    for (const message of this.messages()) {
      for (const part of message.parts) {
        if (part.type !== 'tool') {
          continue;
        }
        if (part.name === 'write_file' || part.name === 'edit_file') {
          const input = part.state.input as Record<string, unknown> | undefined;
          const path = input?.['path'];
          if (typeof path === 'string' && path) {
            files.add(path);
          }
        }
      }
    }
    return [...files].slice(0, 40);
  });

  private async load(dir: string): Promise<void> {
    try {
      const cfg = await this.engine.getConfig(dir);
      this.mcp.set(cfg.mcp);
      this.skills.set(cfg.skills);
      this.yolo.set(!!cfg.config.yolo);
    } catch {
      /* environment list is non-critical */
    }
  }

  async toggleMcp(server: McpStatus, enabled: boolean): Promise<void> {
    const dir = this.meta()?.directory;
    if (!dir) {
      return;
    }
    try {
      const servers = await this.engine.toggleMcp(dir, server.name, enabled);
      this.mcp.set(servers);
    } catch {
      /* keep last state */
    }
  }

  async toggleSkill(skill: ResolvedSkill, enabled: boolean): Promise<void> {
    const dir = this.meta()?.directory;
    if (!dir) {
      return;
    }
    try {
      await this.engine.putConfig(dir, { skills: { [skill.name]: enabled } });
      await this.load(dir);
    } catch {
      /* keep last state */
    }
  }

  async toggleYolo(enabled: boolean): Promise<void> {
    const dir = this.meta()?.directory;
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
