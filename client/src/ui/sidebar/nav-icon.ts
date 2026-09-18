/**
 * F9-13: inline SVG icons for the sidebar's bottom nav rail.
 *
 * One consistent stroke style (24-unit grid, 1.75 stroke, round caps/joins -
 * the Feather look), rendered at 20px and coloured through `currentColor` so
 * the active/hover states are plain CSS on the button. Inline SVG rather than
 * an icon font: no extra request, no FOUT, and the desktop/Tauri build may
 * run offline.
 */

import { ChangeDetectionStrategy, Component, computed, inject, input } from '@angular/core';
import { DomSanitizer } from '@angular/platform-browser';

export type NavIconName = 'explorer' | 'terminal' | 'debug' | 'stats' | 'schedules' | 'settings' | 'subagent';

/** Path data per icon (Feather icons, MIT). */
const PATHS: Record<NavIconName, string> = {
  // folder
  explorer:
    '<path d="M22 19a2 2 0 0 1-2 2H4a2 2 0 0 1-2-2V5a2 2 0 0 1 2-2h5l2 3h9a2 2 0 0 1 2 2z"/>',
  // terminal
  terminal: '<polyline points="4 17 10 11 4 5"/><line x1="12" y1="19" x2="20" y2="19"/>',
  // file-text (a log)
  debug:
    '<path d="M14 2H6a2 2 0 0 0-2 2v16a2 2 0 0 0 2 2h12a2 2 0 0 0 2-2V8z"/><polyline points="14 2 14 8 20 8"/><line x1="16" y1="13" x2="8" y2="13"/><line x1="16" y1="17" x2="8" y2="17"/><polyline points="10 9 9 9 8 9"/>',
  // bar-chart-2
  stats:
    '<line x1="18" y1="20" x2="18" y2="10"/><line x1="12" y1="20" x2="12" y2="4"/><line x1="6" y1="20" x2="6" y2="14"/>',
  // clock
  schedules:
    '<circle cx="12" cy="12" r="10"/><polyline points="12 6 12 12 16 14"/>',
  // sliders
  settings:
    '<line x1="4" y1="21" x2="4" y2="14"/><line x1="4" y1="10" x2="4" y2="3"/><line x1="12" y1="21" x2="12" y2="12"/><line x1="12" y1="8" x2="12" y2="3"/><line x1="20" y1="21" x2="20" y2="16"/><line x1="20" y1="12" x2="20" y2="3"/><line x1="1" y1="14" x2="7" y2="14"/><line x1="9" y1="8" x2="15" y2="8"/><line x1="17" y1="16" x2="23" y2="16"/>',
  // corner-down-right (↳) - sub-agent marker in session lists (F9-12)
  subagent: '<polyline points="15 10 20 15 15 20"/><path d="M4 4v7a4 4 0 0 0 4 4h12"/>',
};

@Component({
  selector: 'app-nav-icon',
  changeDetection: ChangeDetectionStrategy.OnPush,
  template: `
    <svg
      [attr.width]="size()"
      [attr.height]="size()"
      viewBox="0 0 24 24"
      fill="none"
      stroke="currentColor"
      stroke-width="1.75"
      stroke-linecap="round"
      stroke-linejoin="round"
      aria-hidden="true"
      focusable="false"
      [innerHTML]="markup()"
    ></svg>
  `,
  styles: [
    `
      :host {
        display: inline-flex;
        flex: none;
        line-height: 0;
      }
    `,
  ],
})
export class NavIcon {
  readonly name = input.required<NavIconName>();
  readonly size = input(20);

  private readonly sanitizer = inject(DomSanitizer);

  /** The path data is the trusted constant table above, never user input -
   *  bypassing the HTML sanitizer keeps the SVG child elements intact. */
  readonly markup = computed(() => this.sanitizer.bypassSecurityTrustHtml(PATHS[this.name()]));
}
