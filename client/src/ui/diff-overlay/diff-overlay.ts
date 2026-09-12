/**
 * Diff overlay (WP-CHANGES / F6-9).
 *
 * Full-screen dim backdrop + large centered card showing the engine-tracked
 * diff of one file (`GET /session/{id}/changes/diff?path=`), rendered by the
 * shared `<app-diff-view>` (unified/split toggle lives there). Escape or a
 * backdrop click closes it - the same overlay pattern as the command palette.
 *
 * Two actions: "Open in Explorer" (select the file in `ExplorerSelectionStore`
 * and navigate to `/explorer`, exactly like the drawer's mini explorer) and
 * "Revert file" (confirm, `POST .../changes/revert`, then `reverted` fires so
 * the owning panel refreshes its list).
 */

import {
  ChangeDetectionStrategy,
  Component,
  effect,
  inject,
  input,
  output,
  signal,
} from '@angular/core';
import { Router } from '@angular/router';

import { EngineClient } from '../../core/engine-client.service';
import { ChangeBaseline } from '../../core/engine.dtos';
import { I18nService } from '../../i18n/i18n.service';
import { DiffViewComponent } from '../diff-view/diff-view';
import { ExplorerSelectionStore } from '../right-drawer/panels/explorer-selection.store';

@Component({
  selector: 'app-diff-overlay',
  changeDetection: ChangeDetectionStrategy.OnPush,
  imports: [DiffViewComponent],
  templateUrl: './diff-overlay.html',
  styleUrl: './diff-overlay.css',
  host: {
    '(document:keydown.escape)': 'close()',
  },
})
export class DiffOverlay {
  private readonly engine = inject(EngineClient);
  private readonly router = inject(Router);
  private readonly selection = inject(ExplorerSelectionStore);
  private readonly i18n = inject(I18nService);

  readonly t = this.i18n.t.bind(this.i18n);

  /** Session whose change is shown. */
  readonly sessionId = input.required<string>();
  /** Project-relative path of the tracked file. */
  readonly path = input.required<string>();
  /** Absolute project root (for "Open in Explorer"). */
  readonly directory = input<string | null>(null);

  readonly closed = output<void>();
  /** Fires after a successful revert (the panel re-lists). */
  readonly reverted = output<string>();

  readonly diff = signal<string | null>(null);
  readonly baseline = signal<ChangeBaseline | null>(null);
  readonly loading = signal(false);
  readonly reverting = signal(false);
  readonly error = signal<string | null>(null);

  constructor() {
    effect(() => {
      const id = this.sessionId();
      const path = this.path();
      void this.load(id, path);
    });
  }

  close(): void {
    this.closed.emit();
  }

  async openInExplorer(): Promise<void> {
    const directory = this.directory();
    if (!directory) {
      return;
    }
    this.selection.select(directory, this.path());
    this.close();
    await this.router.navigate(['/explorer'], { queryParams: { directory } });
  }

  async revert(): Promise<void> {
    if (this.reverting()) {
      return;
    }
    if (!confirm(this.t('changes.revertConfirm', { path: this.path() }))) {
      return;
    }
    this.reverting.set(true);
    this.error.set(null);
    try {
      const result = await this.engine.revertSessionChange(this.sessionId(), this.path());
      this.reverted.emit(result.path);
      this.close();
    } catch (err) {
      this.error.set(describe(err));
    } finally {
      this.reverting.set(false);
    }
  }

  private async load(id: string, path: string): Promise<void> {
    this.loading.set(true);
    this.error.set(null);
    this.diff.set(null);
    try {
      const res = await this.engine.sessionChangeDiff(id, path);
      if (this.sessionId() !== id || this.path() !== path) {
        return;
      }
      this.diff.set(res.diff);
      this.baseline.set(res.baseline);
    } catch (err) {
      this.error.set(describe(err));
    } finally {
      this.loading.set(false);
    }
  }
}

function describe(err: unknown): string {
  return err instanceof Error ? err.message : String(err);
}
