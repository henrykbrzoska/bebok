/**
 * Command palette (WP-SHELL / F1-9).
 *
 * The component is always mounted - it owns the global `Ctrl/Cmd+K` listener -
 * but only paints the dim overlay + 520px card while `commandPaletteOpen` is
 * set. Escape, a backdrop click or picking an action closes it; action rows
 * navigate through the router, so deep links stay intact.
 */

import {
  ChangeDetectionStrategy,
  Component,
  ElementRef,
  computed,
  effect,
  inject,
  signal,
  viewChild,
} from '@angular/core';
import { FormsModule } from '@angular/forms';
import { Router } from '@angular/router';

import { EngineClient } from '../../core/engine-client.service';
import { I18nService } from '../../i18n/i18n.service';
import { ChatSessionStore } from '../../views/chat/chat-session.store';
import { ProjectSessionsStore } from '../shell/project-sessions.store';
import { ShellStore } from '../shell/shell.store';

type ActionId =
  | 'newSession'
  | 'compactSession'
  | 'switchProject'
  | 'openExplorer'
  | 'openTerminal'
  | 'settingsMcp'
  | 'settingsSkills'
  | 'settingsAgents'
  | 'settingsProviders';

interface PaletteAction {
  id: ActionId;
  labelKey:
    | 'palette.newSession'
    | 'palette.compactSession'
    | 'palette.switchProject'
    | 'palette.openExplorer'
    | 'palette.openTerminal'
    | 'palette.settingsMcp'
    | 'palette.settingsSkills'
    | 'palette.settingsAgents'
    | 'palette.settingsProviders';
  hint?: string;
}

const ACTIONS: PaletteAction[] = [
  { id: 'newSession', labelKey: 'palette.newSession', hint: '↵' },
  { id: 'compactSession', labelKey: 'palette.compactSession' },
  { id: 'switchProject', labelKey: 'palette.switchProject' },
  { id: 'openExplorer', labelKey: 'palette.openExplorer' },
  { id: 'openTerminal', labelKey: 'palette.openTerminal' },
  { id: 'settingsMcp', labelKey: 'palette.settingsMcp' },
  { id: 'settingsSkills', labelKey: 'palette.settingsSkills' },
  { id: 'settingsAgents', labelKey: 'palette.settingsAgents' },
  { id: 'settingsProviders', labelKey: 'palette.settingsProviders' },
];

@Component({
  selector: 'app-command-palette',
  changeDetection: ChangeDetectionStrategy.OnPush,
  imports: [FormsModule],
  templateUrl: './command-palette.html',
  styleUrl: './command-palette.css',
  host: {
    '(document:keydown)': 'onDocumentKeydown($event)',
  },
})
export class CommandPalette {
  private readonly shell = inject(ShellStore);
  private readonly router = inject(Router);
  private readonly engine = inject(EngineClient);
  private readonly project = inject(ProjectSessionsStore);
  private readonly i18n = inject(I18nService);
  /** F6-4: "Compact now" needs the session currently open in the chat view. */
  private readonly session = inject(ChatSessionStore);

  readonly t = this.i18n.t.bind(this.i18n);
  readonly open = this.shell.commandPaletteOpen;
  readonly query = signal('');

  private readonly searchInput = viewChild<ElementRef<HTMLInputElement>>('searchInput');

  /** Actions that apply right now: session-scoped ones need an open chat. */
  private readonly available = computed<PaletteAction[]>(() => {
    const hasSession = this.session.meta() !== null && this.session.canCompact();
    return ACTIONS.filter((action) => action.id !== 'compactSession' || hasSession);
  });

  readonly actions = computed<PaletteAction[]>(() => {
    const q = this.query().trim().toLowerCase();
    const available = this.available();
    if (!q) {
      return available;
    }
    return available.filter((action) => this.t(action.labelKey).toLowerCase().includes(q));
  });

  constructor() {
    effect(() => {
      if (this.open()) {
        this.query.set('');
        queueMicrotask(() => this.searchInput()?.nativeElement.focus());
      }
    });
  }

  /** Global shortcut: Ctrl/Cmd+K opens, Escape closes. */
  onDocumentKeydown(event: KeyboardEvent): void {
    if ((event.ctrlKey || event.metaKey) && event.key.toLowerCase() === 'k') {
      event.preventDefault();
      this.shell.toggleCommandPalette();
      return;
    }
    if (event.key === 'Escape' && this.open()) {
      event.preventDefault();
      this.close();
    }
  }

  close(): void {
    this.shell.closeCommandPalette();
  }

  /** Enter on the search field runs the first (filtered) action. */
  runFirst(): void {
    const first = this.actions()[0];
    if (first) {
      void this.run(first);
    }
  }

  async run(action: PaletteAction): Promise<void> {
    this.close();
    const directory = this.project.directory();
    const queryParams = directory ? { directory } : {};
    switch (action.id) {
      case 'newSession':
        await this.newSession(directory);
        return;
      case 'compactSession':
        await this.compactSession();
        return;
      case 'switchProject':
        this.shell.openProjectSwitcher();
        return;
      case 'openExplorer':
        await this.router.navigate(['/explorer'], { queryParams });
        return;
      case 'openTerminal':
        await this.router.navigate(['/terminal'], { queryParams });
        return;
      case 'settingsMcp':
        await this.goSettings('mcp', directory);
        return;
      case 'settingsSkills':
        await this.goSettings('skills', directory);
        return;
      case 'settingsAgents':
        await this.goSettings('agents', directory);
        return;
      case 'settingsProviders':
        await this.goSettings('providers', directory);
        return;
    }
  }

  private async goSettings(tab: string, directory: string | null): Promise<void> {
    await this.router.navigate(['/settings'], {
      queryParams: directory ? { directory, tab } : { tab },
    });
  }

  /**
   * Compact the open session (F6-4). Compaction forks: the engine answers
   * with a *new* session id, so the palette navigates there. A running turn
   * is left alone (the engine would race the transcript).
   */
  private async compactSession(): Promise<void> {
    const meta = this.session.meta();
    if (!meta || this.session.running() || !this.session.canCompact()) {
      return;
    }
    try {
      const forked = await this.engine.compactSession(meta.id, this.session.compactBudget());
      await this.project.refresh();
      await this.router.navigate(['/chat', forked.sessionID]);
    } catch {
      /* the chat view surfaces compaction errors; the palette stays quiet */
    }
  }

  private async newSession(directory: string | null): Promise<void> {
    if (!directory) {
      await this.router.navigate(['/']);
      return;
    }
    try {
      const created = await this.engine.createSession(directory);
      await this.project.refresh();
      await this.router.navigate(['/chat', created.sessionID]);
    } catch {
      await this.router.navigate(['/']);
    }
  }
}
