/**
 * Updates screen (`/updates`, opened from the topbar version chip): what is
 * running (desktop shell + engine), "Check for updates" with the install /
 * download action, the offered release's notes, and the recent releases
 * from GitHub with the running one marked. All state lives in `UpdateStore`;
 * the release list is fetched when the screen opens.
 */

import { ChangeDetectionStrategy, Component, inject } from '@angular/core';
import { RouterLink } from '@angular/router';

import { RELEASES_PAGE, UpdateStore } from '../../core/update.store';
import { I18nService } from '../../i18n/i18n.service';
import { MarkdownViewComponent } from '../../ui/markdown-view/markdown-view';

@Component({
  selector: 'app-updates',
  changeDetection: ChangeDetectionStrategy.OnPush,
  imports: [RouterLink, MarkdownViewComponent],
  templateUrl: './updates.html',
  styleUrl: './updates.css',
})
export class UpdatesView {
  private readonly i18n = inject(I18nService);
  readonly update = inject(UpdateStore);

  readonly t = this.i18n.t.bind(this.i18n);
  readonly releasesPage = RELEASES_PAGE;

  constructor() {
    void this.update.loadReleases();
  }

  formatDate(iso: string | null): string {
    if (!iso) {
      return '';
    }
    const date = new Date(iso);
    return Number.isNaN(date.getTime()) ? iso : date.toLocaleDateString(this.i18n.lang());
  }
}
